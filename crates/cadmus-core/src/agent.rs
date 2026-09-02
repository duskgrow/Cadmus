use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cadmus_contract::{ChatRequest, FinishReason, Message, ModelError, Provider, ToolSpec};
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::{AssembledTurn, MessageAssembler, TurnOutcome};

/// A tool the agent may call. rmcp servers are wrapped into this trait at the
/// wiring layer; tests use hand-rolled fakes.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError>;
}

#[derive(Debug, thiserror::Error)]
#[error("tool `{tool}` failed: {message}")]
pub struct ToolError {
    pub tool: String,
    pub message: String,
}

/// The outcome of a completed run.
#[derive(Debug)]
pub struct RunOutcome {
    /// The full history, including every appended assistant/tool message.
    pub messages: Vec<Message>,
    /// The last assistant turn (the one that ended the loop without calls).
    pub final_turn: AssembledTurn,
    /// How many assistant turns ran.
    pub turns: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ModelError),
    #[error("assistant turn limit ({0}) exceeded")]
    TurnLimit(usize),
    /// No content at all; the finish reason distinguishes "spent everything
    /// on hidden thinking" (Length — retry/escalate) from a protocol anomaly
    /// (pitfall #5). Cascade routing is phase 3; for now it surfaces.
    #[error("empty assistant turn (finish: {finish:?})")]
    EmptyTurn { finish: FinishReason },
}

/// The minimal agent loop: stream → assemble → dispatch tool calls → repeat.
/// Everything external is injected (provider, tools, limits) — no hidden
/// time, randomness or IO.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn AgentTool>>,
    specs: Vec<ToolSpec>,
    max_turns: usize,
}

impl AgentLoop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn AgentTool>>,
        max_turns: usize,
    ) -> Self {
        let specs = tools.iter().map(|tool| tool.spec()).collect();
        let tools = tools
            .into_iter()
            .map(|tool| (tool.spec().name, tool))
            .collect();
        Self {
            provider,
            tools,
            specs,
            max_turns,
        }
    }

    /// Runs the loop from `base.messages` until an assistant turn produces no
    /// tool calls. Every other request parameter is reused byte-identically
    /// each turn — a stable prefix is what makes prompt caching possible.
    ///
    /// # Errors
    /// Propagates provider errors; turns tool failures into tool-result
    /// messages instead (the model reads its own failure and recovers).
    pub async fn run(&self, base: &ChatRequest) -> Result<RunOutcome, AgentError> {
        let mut messages = base.messages.clone();
        let mut base = base.clone();
        if !self.specs.is_empty() {
            base.tools = self.specs.clone();
        }

        for turn in 1..=self.max_turns {
            let request = base.clone().with_messages(messages.clone());
            let mut stream = self.provider.chat_stream(&request).await?;
            let mut assembler = MessageAssembler::new();
            // User-facing streaming is deliberately deferred: the seam is an
            // observer sink right before `push` (TextDelta / ToolCallStarted /
            // TurnCompleted events), leaving the assembler the single owner of
            // aggregation semantics.
            while let Some(item) = stream.next().await {
                assembler.push(item?);
            }
            let turn_result = assembler.complete();

            match turn_result.outcome {
                TurnOutcome::Empty => {
                    return Err(AgentError::EmptyTurn {
                        finish: turn_result.finish,
                    });
                }
                // Truncated turns still carry whatever content survived
                // (pitfall #3); the trajectory records the warnings.
                TurnOutcome::Content | TurnOutcome::Truncated => {}
            }

            messages.push(turn_result.message.clone());
            let calls: Vec<_> = turn_result.message.tool_calls().cloned().collect();
            if calls.is_empty() {
                return Ok(RunOutcome {
                    messages,
                    final_turn: turn_result,
                    turns: turn,
                });
            }
            for call in calls {
                messages.push(self.execute(call).await);
            }
        }
        Err(AgentError::TurnLimit(self.max_turns))
    }

    async fn execute(&self, call: cadmus_contract::ToolCall) -> Message {
        let result = match self.tools.get(&call.name) {
            Some(tool) => match tool.invoke(call.arguments.clone()).await {
                Ok(content) => content,
                Err(err) => Value::String(err.to_string()),
            },
            // A hallucinated tool name is feedback, not a fatal error.
            None => Value::String(format!("unknown tool: {}", call.name)),
        };
        Message::tool_result(call.id, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReplayProvider;
    use cadmus_contract::{CacheSupport, Capabilities, SoSupport, StreamChunk, Support};
    use serde_json::json;

    fn test_capabilities() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tools: Support::Yes,
            structured_output: SoSupport::NativeStrict,
            reasoning: None,
            prompt_cache: CacheSupport::Automatic,
            logprobs: false,
            max_context: 128_000,
            max_output: 8_000,
            opaque_echo: vec![],
        }
    }

    struct EchoTool;

    #[async_trait]
    impl AgentTool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".into(),
                description: "echoes back the input".into(),
                parameters: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            }
        }

        async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
            Ok(arguments)
        }
    }

    fn tool_call_script(id: &str, args: &str) -> Vec<Result<StreamChunk, ModelError>> {
        ReplayProvider::script(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: id.into(),
                name: "echo".into(),
            },
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: args.into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ])
    }

    fn text_script(text: &str) -> Vec<Result<StreamChunk, ModelError>> {
        ReplayProvider::script(vec![
            StreamChunk::TextDelta(text.into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ])
    }

    #[tokio::test]
    async fn runs_tool_call_round_trip() {
        let provider = Arc::new(
            ReplayProvider::new([
                tool_call_script("c1", "{\"text\":\"ping\"}"),
                text_script("pong received"),
            ])
            .with_capabilities(test_capabilities()),
        );
        let agent = AgentLoop::new(provider, vec![Arc::new(EchoTool)], 8);
        let outcome = agent
            .run(&ChatRequest::user_text("say ping", 1_024))
            .await
            .expect("run");

        assert_eq!(outcome.turns, 2);
        // user → assistant(call) → tool(result) → assistant(text)
        assert_eq!(outcome.messages.len(), 4);
        assert_eq!(
            outcome.messages[2].content[0],
            cadmus_contract::ContentPart::Text {
                text: "{\"text\":\"ping\"}".into()
            }
        );
        assert_eq!(outcome.messages[2].tool_call_id.as_deref(), Some("c1"));
    }

    #[tokio::test]
    async fn unknown_tool_is_feedback_not_failure() {
        let provider = Arc::new(ReplayProvider::new([
            ReplayProvider::script(vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: "does_not_exist".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::ToolCalls,
                },
            ]),
            text_script("sorry"),
        ]));
        let agent = AgentLoop::new(provider, vec![], 8);
        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");
        assert!(matches!(
            &outcome.messages[2].content[0],
            cadmus_contract::ContentPart::Text { text } if text.contains("unknown tool")
        ));
    }

    #[tokio::test]
    async fn empty_turn_is_an_error_carrying_the_finish_reason() {
        let provider = Arc::new(ReplayProvider::new([ReplayProvider::script(vec![
            StreamChunk::Done {
                finish: FinishReason::Length,
            },
        ])]));
        let agent = AgentLoop::new(provider, vec![], 8);
        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("empty turn must fail");
        assert!(matches!(
            err,
            AgentError::EmptyTurn {
                finish: FinishReason::Length
            }
        ));
    }

    #[tokio::test]
    async fn turn_limit_is_enforced() {
        let scripts: Vec<_> = (0..3)
            .map(|i| tool_call_script(&format!("c{i}"), "{\"text\":\"x\"}"))
            .collect();
        let provider = Arc::new(ReplayProvider::new(scripts));
        let agent = AgentLoop::new(provider, vec![Arc::new(EchoTool)], 3);
        let err = agent
            .run(&ChatRequest::user_text("loop", 1_024))
            .await
            .expect_err("must hit the turn limit");
        assert!(matches!(err, AgentError::TurnLimit(3)));
    }
}
