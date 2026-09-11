//! Shared fixtures for the agent module's test suites: capability fakes,
//! scripted providers, harness builders and assertion helpers. Each suite
//! lives next to the code it pins; what crosses a suite boundary lives here.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cadmus_contract::{
    CacheSupport, Capabilities, Command, Event, EventKind, FinishReason, LiveItem, LiveSink,
    Message, ModelError, SoSupport, StreamChunk, Support, ToolCall, ToolSpec,
};
use serde_json::{Value, json};

use super::{AgentLoop, AgentTool, Concurrency, ToolError};
use crate::ReplayProvider;
use crate::testing::test_telemetry;
pub fn test_capabilities() -> Capabilities {
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

pub struct EchoTool;

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

/// A parallel-safe fake logging start/end with a genuine yield between,
/// so a concurrent batch verifiably interleaves — a non-yielding fake
/// completes on its first poll and would mask scheduling regressions.
pub struct YieldTool {
    pub name: &'static str,
    pub log: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentTool for YieldTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "yield".into(),
            parameters: json!({"type": "object"}),
        }
    }

    fn concurrency(&self) -> Concurrency {
        Concurrency::ParallelSafe
    }

    async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
        self.log.lock().await.push(format!("start {}", self.name));
        tokio::task::yield_now().await;
        self.log.lock().await.push(format!("end {}", self.name));
        Ok(Value::String(self.name.into()))
    }
}

/// A serial fake: no `concurrency` override, so the fail-safe default
/// applies.
pub struct StepTool {
    pub name: &'static str,
    pub log: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl AgentTool for StepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "step".into(),
            parameters: json!({"type": "object"}),
        }
    }

    async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
        self.log.lock().await.push(format!("start {}", self.name));
        self.log.lock().await.push(format!("end {}", self.name));
        Ok(Value::String(self.name.into()))
    }
}

pub fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({}),
    }
}

pub fn tool_call_script(id: &str, args: &str) -> Vec<Result<StreamChunk, ModelError>> {
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

pub fn text_script(text: &str) -> Vec<Result<StreamChunk, ModelError>> {
    ReplayProvider::script(vec![
        StreamChunk::TextDelta(text.into()),
        StreamChunk::Done {
            finish: FinishReason::Stop,
        },
    ])
}

pub fn test_loop(
    provider: ReplayProvider,
    tools: Vec<Arc<dyn AgentTool>>,
    max_turns: usize,
) -> (AgentLoop, Arc<crate::testing::RecordingSink>) {
    let (telemetry, sink) = test_telemetry("tr-test");
    (
        // The dispatch-path tests exercise the loop, not the gate: their
        // fakes stay on the fail-safe `Effect::Mutation` default and the
        // test client auto-approves through the command path.
        AgentLoop::new(
            Arc::new(provider),
            tools,
            crate::testing::test_context(),
            crate::testing::auto_approving().0,
            max_turns,
            telemetry,
        ),
        sink,
    )
}

/// Sends one scripted command the first time a live item matches — how
/// a test plants a command at a precise protocol point, the way a real
/// client reacts to the stream (ADR-0013 item 6).
pub struct SendOnMatch<P> {
    inner: Arc<crate::testing::RecordingLive>,
    commands: std::sync::mpsc::Sender<Command>,
    predicate: P,
    command: Mutex<Option<Command>>,
}

impl<P> SendOnMatch<P> {
    pub fn new(
        inner: Arc<crate::testing::RecordingLive>,
        commands: std::sync::mpsc::Sender<Command>,
        predicate: P,
        command: Command,
    ) -> Self {
        Self {
            inner,
            commands,
            predicate,
            command: Mutex::new(Some(command)),
        }
    }
}

impl<P> LiveSink for SendOnMatch<P>
where
    P: Fn(&LiveItem) -> bool + Send + Sync,
{
    fn publish(&self, item: &LiveItem) {
        self.inner.publish(item);
        if (self.predicate)(item)
            && let Some(command) = self.command.lock().expect("send-on poisoned").take()
        {
            let _ = self.commands.send(command);
        }
    }
}

/// The text parts of one message, concatenated (test assertion helper).
pub fn text_of(message: &Message) -> String {
    use cadmus_contract::ContentPart;
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

pub fn kind_name(event: &Event) -> &'static str {
    match &event.kind {
        EventKind::Command(Command::StartRun { .. }) => "start_run",
        EventKind::Command(Command::ResolveApproval { .. }) => "resolve_approval",
        EventKind::Command(Command::Steer { .. }) => "steer",
        EventKind::Command(Command::Interrupt { .. }) => "interrupt",
        EventKind::LlmRequest { .. } => "llm_request",
        EventKind::Fold { .. } => "fold",
        EventKind::InstructionInjected { .. } => "instruction_injected",
        EventKind::LlmResponse { .. } => "llm_response",
        EventKind::ToolCall { .. } => "tool_call",
        EventKind::ToolResult { .. } => "tool_result",
        EventKind::EvalScore(_) => "eval_score",
        EventKind::RunFinished { .. } => "run_finished",
    }
}
