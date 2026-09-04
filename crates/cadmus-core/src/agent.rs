use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use cadmus_contract::{
    ChatRequest, Clock, Command, Event, EventError, EventKind, EventSink, FinishReason, IdSequence,
    Message, ModelError, Provider, ToolCall, ToolSpec, TurnOutcome, attrs, error_kinds,
};
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::{AssembledTurn, MessageAssembler};

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

/// The trajectory-writing bundle injected into the loop (ADR-0002/0005):
/// every step of a run appends to the trace's append-only log, so a crash
/// loses at most the in-flight step. Time and ids come from here — the loop
/// never reads a clock or mints an id itself.
pub struct Telemetry {
    pub sink: Arc<dyn EventSink>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdSequence>,
    /// One run = one trace; minted by the wiring layer.
    pub trace_id: String,
    /// Run-level attributes merged onto the start-run event
    /// (`selfevol.provider` / `selfevol.model` / `selfevol.cadmus.version`).
    pub run_attributes: BTreeMap<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ModelError),
    /// The trajectory log failed mid-run; the run stops rather than
    /// continuing unrecorded (the trajectory is the evolution asset).
    #[error("trajectory log failed: {0}")]
    Log(#[from] cadmus_contract::LogError),
    #[error("assistant turn limit ({0}) exceeded")]
    TurnLimit(usize),
    /// No content at all; the finish reason distinguishes "spent everything
    /// on hidden thinking" (Length — retry/escalate) from a protocol anomaly
    /// (pitfall #5). Cascade routing is phase 3; for now it surfaces.
    #[error("empty assistant turn (finish: {finish:?})")]
    EmptyTurn { finish: FinishReason },
}

/// The minimal agent loop: stream → assemble → dispatch tool calls → repeat,
/// appending each step to the trace log. Everything external is injected
/// (provider, tools, telemetry, limits) — no hidden time, randomness or IO.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn AgentTool>>,
    specs: Vec<ToolSpec>,
    max_turns: usize,
    telemetry: Telemetry,
}

impl AgentLoop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn AgentTool>>,
        max_turns: usize,
        telemetry: Telemetry,
    ) -> Self {
        // One pass: the Vec keeps wire order (prompt-cache byte stability),
        // the map gives name dispatch.
        let mut specs = Vec::with_capacity(tools.len());
        let tools = tools
            .into_iter()
            .map(|tool| {
                let spec = tool.spec();
                specs.push(spec.clone());
                (spec.name, tool)
            })
            .collect();
        Self {
            provider,
            tools,
            specs,
            max_turns,
            telemetry,
        }
    }

    /// Runs the loop from `base.messages` until an assistant turn produces no
    /// tool calls. Every other request parameter is reused byte-identically
    /// each turn — a stable prefix is what makes prompt caching possible.
    ///
    /// # Errors
    /// Propagates provider and trajectory-log errors; turns tool failures
    /// into tool-result messages instead (the model reads its own failure and
    /// recovers).
    pub async fn run(&self, base: &ChatRequest) -> Result<RunOutcome, AgentError> {
        let mut messages = base.messages.clone();
        let mut base = base.clone();
        if !self.specs.is_empty() {
            base.tools = self.specs.clone();
        }

        let root_span = self.next_span();
        let mut start_run = self.envelope(
            &root_span,
            None,
            EventKind::Command(Command::StartRun {
                base: Box::new(base.clone()),
            }),
        );
        start_run.attributes = self.telemetry.run_attributes.clone();
        self.emit(&start_run)?;

        for turn in 1..=self.max_turns {
            let turn_span = self.next_span();
            // Per-turn clone is deliberate: the provider borrows an immutable
            // request while the loop owns the growing history. The cost is a
            // turn-boundary memcpy — negligible against the network call that
            // follows (the hot path, stream aggregation, stays clone-light).
            let request = base.clone().with_messages(messages.clone());
            // The span-open marker only; the full per-turn history is
            // reconstructible from the fold, never snapshotted here.
            self.emit(&self.turn_event(&turn_span, &root_span, turn, EventKind::LlmRequest))?;

            let turn_result = self
                .assistant_turn(&request, &turn_span, &root_span, turn)
                .await?;
            messages.push(turn_result.message.clone());
            let calls: Vec<_> = turn_result.message.tool_calls().cloned().collect();
            if calls.is_empty() {
                self.finish_with(&root_span, turn, None)?;
                return Ok(RunOutcome {
                    messages,
                    final_turn: turn_result,
                    turns: turn,
                });
            }
            self.dispatch_tools(calls, &mut messages, &root_span, turn)
                .await?;
        }

        let error = AgentError::TurnLimit(self.max_turns);
        self.finish_with(
            &root_span,
            self.max_turns,
            Some(EventError {
                kind: error_kinds::TURN_LIMIT.into(),
                message: error.to_string(),
            }),
        )?;
        Err(error)
    }

    /// Streams one assistant turn, assembles it and appends the
    /// `llm_response` event. Failure paths record what there is to record
    /// before returning:
    /// a call-level error leaves the turn span unclosed (no response ever
    /// arrived), a mid-stream error records the partial turn errored, an
    /// empty turn records its classification.
    async fn assistant_turn(
        &self,
        request: &ChatRequest,
        turn_span: &str,
        root_span: &str,
        turn: usize,
    ) -> Result<AssembledTurn, AgentError> {
        let mut stream = match self.provider.chat_stream(request).await {
            Ok(stream) => stream,
            Err(error) => {
                self.finish_with(root_span, turn - 1, Some(model_event_error(&error)))?;
                return Err(AgentError::Provider(error));
            }
        };
        let mut assembler = MessageAssembler::new();
        // User-facing streaming is deliberately deferred: the seam is an
        // observer sink right before `push` (TextDelta / ToolCallStarted /
        // TurnCompleted events), leaving the assembler the single owner of
        // aggregation semantics.
        let mut stream_error = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => assembler.push(chunk),
                Err(error) => {
                    stream_error = Some(error);
                    break;
                }
            }
        }
        let turn_result = assembler.complete();

        if let Some(error) = stream_error {
            let response = self
                .turn_event(turn_span, root_span, turn, response_kind(&turn_result))
                .errored(model_event_error(&error));
            self.emit(&response)?;
            self.finish_with(root_span, turn - 1, Some(model_event_error(&error)))?;
            return Err(AgentError::Provider(error));
        }

        if turn_result.outcome == TurnOutcome::Empty {
            let error = AgentError::EmptyTurn {
                finish: turn_result.finish.clone(),
            };
            let detail = EventError {
                kind: error_kinds::EMPTY_TURN.into(),
                message: error.to_string(),
            };
            let response = self
                .turn_event(turn_span, root_span, turn, response_kind(&turn_result))
                .errored(detail.clone());
            self.emit(&response)?;
            self.finish_with(root_span, turn - 1, Some(detail))?;
            return Err(error);
        }

        // Truncated turns still carry whatever content survived (pitfall #3);
        // the trajectory records the warnings.
        self.emit(&self.turn_event(turn_span, root_span, turn, response_kind(&turn_result)))?;
        Ok(turn_result)
    }

    /// Executes one assistant turn's tool calls, appending call/result event
    /// pairs, and pushes the tool messages onto the history.
    async fn dispatch_tools(
        &self,
        calls: Vec<ToolCall>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        for call in calls {
            let tool_span = self.next_span();
            self.emit(&self.turn_event(
                &tool_span,
                root_span,
                turn,
                EventKind::ToolCall { call: call.clone() },
            ))?;
            let (result, error) = self.execute(&call).await;
            let mut result_event = self.turn_event(
                &tool_span,
                root_span,
                turn,
                EventKind::ToolResult {
                    call_id: call.id.clone(),
                    result: result.clone(),
                },
            );
            if let Some(error) = error {
                result_event = result_event.errored(error);
            }
            self.emit(&result_event)?;
            messages.push(Message::tool_result(call.id, result));
        }
        Ok(())
    }

    /// Returns the result value and, for failures, the structured error: the
    /// message stream collapses both into text for the model, the trajectory
    /// keeps them apart. A hallucinated tool name is feedback, not a fatal
    /// error.
    async fn execute(&self, call: &ToolCall) -> (Value, Option<EventError>) {
        let Some(tool) = self.tools.get(&call.name) else {
            let message = format!("unknown tool: {}", call.name);
            return (
                Value::String(message.clone()),
                Some(EventError {
                    kind: error_kinds::UNKNOWN_TOOL.into(),
                    message,
                }),
            );
        };
        match tool.invoke(call.arguments.clone()).await {
            Ok(content) => (content, None),
            Err(err) => (
                Value::String(err.to_string()),
                Some(EventError {
                    kind: error_kinds::TOOL.into(),
                    message: err.to_string(),
                }),
            ),
        }
    }

    fn emit(&self, event: &Event) -> Result<(), AgentError> {
        self.telemetry.sink.append(event)?;
        Ok(())
    }

    fn envelope(&self, span: &str, parent: Option<&str>, kind: EventKind) -> Event {
        Event::new(
            format!("e{}", self.telemetry.ids.next()),
            self.telemetry.trace_id.clone(),
            span.to_string(),
            parent.map(str::to_string),
            self.telemetry.clock.now_unix_ms(),
            kind,
        )
    }

    /// One span per turn / tool execution, hanging off the run root, with
    /// the 1-based turn index as an attribute.
    fn turn_event(&self, span: &str, root: &str, turn: usize, kind: EventKind) -> Event {
        self.envelope(span, Some(root), kind)
            .with_attribute(attrs::TURN, u64::try_from(turn).unwrap_or(u64::MAX))
    }

    fn next_span(&self) -> String {
        format!("s{}", self.telemetry.ids.next())
    }

    /// The terminal record: `turns` counts *completed* assistant turns.
    fn finish_with(
        &self,
        root: &str,
        completed_turns: usize,
        error: Option<EventError>,
    ) -> Result<(), AgentError> {
        let event = self.envelope(
            root,
            None,
            EventKind::RunFinished {
                turns: u32::try_from(completed_turns).unwrap_or(u32::MAX),
            },
        );
        let event = match error {
            Some(error) => event.errored(error),
            None => event,
        };
        self.emit(&event)
    }
}

fn response_kind(turn: &AssembledTurn) -> EventKind {
    EventKind::LlmResponse {
        message: turn.message.clone(),
        usage: turn.usage.clone(),
        finish: turn.finish.clone(),
        outcome: turn.outcome,
        warnings: turn.warnings.clone(),
    }
}

fn model_event_error(error: &ModelError) -> EventError {
    let kind = match error {
        ModelError::RateLimited { .. } => error_kinds::RATE_LIMITED,
        ModelError::Server { .. } => error_kinds::SERVER,
        ModelError::Network(_) => error_kinds::NETWORK,
        ModelError::Protocol(_) => error_kinds::PROTOCOL,
        ModelError::InvalidRequest(_) => error_kinds::INVALID_REQUEST,
        ModelError::CapabilityMismatch(_) => error_kinds::CAPABILITY_MISMATCH,
        ModelError::Auth(_) => error_kinds::AUTH,
        ModelError::ContextLength => error_kinds::CONTEXT_LENGTH,
    };
    EventError {
        kind: kind.into(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReplayProvider;
    use crate::testing::test_telemetry;
    use cadmus_contract::testing::{ContractSubject, QueuedResponse};
    use cadmus_contract::{
        CacheSupport, Capabilities, SoSupport, Status, StreamChunk, Support, error_kinds,
    };
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

    fn test_loop(
        provider: ReplayProvider,
        tools: Vec<Arc<dyn AgentTool>>,
        max_turns: usize,
    ) -> (AgentLoop, Arc<crate::testing::RecordingSink>) {
        let (telemetry, sink) = test_telemetry("tr-test");
        (
            AgentLoop::new(Arc::new(provider), tools, max_turns, telemetry),
            sink,
        )
    }

    #[tokio::test]
    async fn runs_tool_call_round_trip() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"ping\"}"),
            text_script("pong received"),
        ])
        .with_capabilities(test_capabilities());
        let (agent, _sink) = test_loop(provider, vec![Arc::new(EchoTool)], 8);
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
        let provider = ReplayProvider::new([
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
        ]);
        let (agent, sink) = test_loop(provider, vec![], 8);
        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");
        assert!(matches!(
            &outcome.messages[2].content[0],
            cadmus_contract::ContentPart::Text { text } if text.contains("unknown tool")
        ));
        // The trajectory keeps the failure distinction the message stream
        // collapses: the tool_result event is errored with kind unknown_tool.
        let tool_result = sink
            .events()
            .into_iter()
            .find_map(|event| match event.kind {
                EventKind::ToolResult { .. } => Some(event),
                _ => None,
            })
            .expect("a tool_result event");
        assert_eq!(tool_result.status, Status::Error);
        assert_eq!(
            tool_result.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::UNKNOWN_TOOL)
        );
    }

    #[tokio::test]
    async fn empty_turn_is_an_error_carrying_the_finish_reason() {
        let provider = ReplayProvider::new([ReplayProvider::script(vec![StreamChunk::Done {
            finish: FinishReason::Length,
        }])]);
        let (agent, sink) = test_loop(provider, vec![], 8);
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
        // The empty turn is recorded: an errored llm_response (outcome empty)
        // and an errored terminal record.
        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            ["start_run", "llm_request", "llm_response", "run_finished"]
        );
        let EventKind::LlmResponse { outcome, .. } = &events[2].kind else {
            panic!("expected llm_response");
        };
        assert_eq!(*outcome, TurnOutcome::Empty);
        assert_eq!(events[2].status, Status::Error);
        assert_eq!(
            events[2].error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::EMPTY_TURN)
        );
        assert_eq!(events[3].status, Status::Error);
        assert!(matches!(
            events[3].kind,
            EventKind::RunFinished { turns: 0 }
        ));
    }

    #[tokio::test]
    async fn turn_limit_is_enforced() {
        let scripts: Vec<_> = (0..3)
            .map(|i| tool_call_script(&format!("c{i}"), "{\"text\":\"x\"}"))
            .collect();
        let provider = ReplayProvider::new(scripts);
        let (agent, sink) = test_loop(provider, vec![Arc::new(EchoTool)], 3);
        let err = agent
            .run(&ChatRequest::user_text("loop", 1_024))
            .await
            .expect_err("must hit the turn limit");
        assert!(matches!(err, AgentError::TurnLimit(3)));
        // The terminal record carries the failure.
        let last = sink.events().pop().expect("a terminal event");
        assert!(matches!(last.kind, EventKind::RunFinished { turns: 3 }));
        assert_eq!(last.status, Status::Error);
        assert_eq!(
            last.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::TURN_LIMIT)
        );
    }

    /// Crash honesty, call-level: the provider call itself fails — the turn
    /// span stays unclosed (no `llm_response` at all) and the terminal record
    /// carries the classified error.
    #[tokio::test]
    async fn call_level_failure_leaves_the_turn_span_unclosed() {
        let provider = ReplayProvider::new([]);
        provider.queue(QueuedResponse::CallError(ModelError::RateLimited {
            retry_after: None,
        }));
        let (agent, sink) = test_loop(provider, vec![], 8);
        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("the call fails");
        assert!(matches!(
            err,
            AgentError::Provider(ModelError::RateLimited { .. })
        ));

        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(kinds, ["start_run", "llm_request", "run_finished"]);
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 0 }));
        assert_eq!(
            finished.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::RATE_LIMITED)
        );
    }

    /// Crash honesty, mid-stream: the partial turn is recorded (errored)
    /// before the run dies — the trajectory shows how far the stream got.
    #[tokio::test]
    async fn mid_stream_failure_records_the_partial_turn() {
        let provider = ReplayProvider::new([]);
        provider.queue(QueuedResponse::StreamError {
            chunks: vec![StreamChunk::TextDelta("partial".into())],
            error: ModelError::Network("connection reset".into()),
        });
        let (agent, sink) = test_loop(provider, vec![], 8);
        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("the stream fails");
        assert!(matches!(err, AgentError::Provider(ModelError::Network(_))));

        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            ["start_run", "llm_request", "llm_response", "run_finished"]
        );
        let response = &events[2];
        assert_eq!(response.status, Status::Error);
        assert_eq!(
            response.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::NETWORK)
        );
        let EventKind::LlmResponse {
            message, outcome, ..
        } = &response.kind
        else {
            panic!("expected llm_response");
        };
        assert_eq!(*outcome, TurnOutcome::Truncated);
        assert!(matches!(
            message.content.first(),
            Some(cadmus_contract::ContentPart::Text { text }) if text == "partial"
        ));
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
    }

    fn kind_name(event: &Event) -> &'static str {
        match &event.kind {
            EventKind::Command(Command::StartRun { .. }) => "start_run",
            EventKind::LlmRequest => "llm_request",
            EventKind::LlmResponse { .. } => "llm_response",
            EventKind::ToolCall { .. } => "tool_call",
            EventKind::ToolResult { .. } => "tool_result",
            EventKind::EvalScore(_) => "eval_score",
            EventKind::RunFinished { .. } => "run_finished",
        }
    }
}
