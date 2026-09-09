use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use cadmus_contract::{
    Approval, ChatRequest, Clock, Command, Event, EventError, EventKind, EventSink, FinishReason,
    IdSequence, Message, ModelError, Provider, ToolCall, ToolSpec, TurnOutcome, attrs, error_kinds,
};
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::approval::Approver;
use crate::{AssembledTurn, MessageAssembler};

/// A tool the agent may call. rmcp servers are wrapped into this trait at the
/// wiring layer; tests use hand-rolled fakes.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Concurrency declaration (ADR-0008 item 2): whether `invoke` may run
    /// concurrently with other calls in the same turn. The default is
    /// fail-safe — undeclared tools serialize. That includes third-party
    /// MCP wrappers, whose semantics we do not control.
    fn concurrency(&self) -> Concurrency {
        Concurrency::Serial
    }

    /// Effect declaration (ADR-0008 item 4): whether a call can mutate the
    /// workspace. Mutation calls pass the client policy's approval gate —
    /// presented as one batch per turn; perception calls are never gated.
    /// The default is fail-safe: an undeclared read is merely gated, an
    /// undeclared mutation would execute ungated.
    fn effect(&self) -> Effect {
        Effect::Mutation
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError>;
}

/// Whether a tool's `invoke` may overlap with other calls in the same turn
/// (ADR-0008 item 2: declared concurrency safety, defaulting to non-parallel
/// (fail-safe)). Built-in tools are designed for parallel safety on purpose
/// — the declaration records the analysis; the serial default protects what
/// we cannot vouch for (third-party wrappers, unanalyzed additions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concurrency {
    Serial,
    ParallelSafe,
}

/// The gate-relevant effect of a call (ADR-0008 item 4). Mutation calls are
/// presented to the client policy before executing; perception is never
/// gated. The fail-safe default is `Mutation` — same protective direction
/// as [`Concurrency`]'s serial default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Perception,
    Mutation,
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
    approver: Arc<dyn Approver>,
    specs: Vec<ToolSpec>,
    max_turns: usize,
    telemetry: Telemetry,
}

impl AgentLoop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn AgentTool>>,
        approver: Arc<dyn Approver>,
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
            approver,
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
    /// pairs, and pushing the tool messages onto the history. A batch runs
    /// concurrently only when EVERY call in it is parallel-safe; a single
    /// undeclared call serializes the whole batch in call order (ADR-0008
    /// item 2). Messages and events always land in call order, so the
    /// trajectory reads the same either way.
    ///
    /// Batch approval precedes any execution (ADR-0008 item 4 amendment):
    /// the turn's gated (mutation) calls are presented to the client policy
    /// together, each approved or rejected independently, so a decision can
    /// depend on the batch's contents but never on another gated call's
    /// result. A rejected call never invokes; its rejection lands as an
    /// `is_error` tool result in call order.
    ///
    /// All-or-nothing, not segment mixing: the model emits a turn's calls as
    /// one unordered batch — it cannot know which tools are parallel-safe,
    /// and scheduling is not its job — so a finer-grained schedule buys no
    /// real ordering information, while a sloppy batch (a write followed by
    /// its own run command) is still rescued by in-order serial execution.
    /// Safety is a property of the tool's side effects (a same-file
    /// read-modify-write race, an external session's state), so the
    /// declaration lives on the tool with a fail-safe serial default. What
    /// the scheduler protects against is silent corruption, not errors —
    /// errors are already recoverable model feedback.
    async fn dispatch_tools(
        &self,
        calls: Vec<ToolCall>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let denied = self.gate(&calls).await;
        let all_safe = calls.iter().all(|call| self.is_parallel_safe(call));
        if all_safe {
            self.dispatch_parallel(&calls, &denied, messages, root_span, turn)
                .await?;
        } else {
            for (position, call) in calls.iter().enumerate() {
                self.dispatch_one(call, denied.get(&position), messages, root_span, turn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Presents the batch's gated calls to the client policy as one batch
    /// and returns the rejections keyed by batch position. Position, not
    /// call id: ids are provider-supplied wire data with no uniqueness
    /// check, and a duplicated id must never let one call's rejection deny
    /// its same-id sibling. A short decision reply denies the remainder:
    /// unanswered is deny (ADR-0008 item 4); extra decisions are ignored.
    async fn gate(&self, calls: &[ToolCall]) -> HashMap<usize, Option<String>> {
        let mut positions = Vec::new();
        let mut batch = Vec::new();
        for (position, call) in calls.iter().enumerate() {
            if self.is_gated(call) {
                positions.push(position);
                batch.push(call.clone());
            }
        }
        let mut denied = HashMap::new();
        if batch.is_empty() {
            return denied;
        }
        let decisions = self.approver.approve(&batch).await;
        for (index, position) in positions.iter().enumerate() {
            match decisions.get(index) {
                Some(Approval::Approved) => {}
                Some(Approval::Rejected { comment }) => {
                    denied.insert(*position, comment.clone());
                }
                None => {
                    denied.insert(*position, None);
                }
            }
        }
        denied
    }

    /// A call is gated when its tool declares a mutation effect (ADR-0008
    /// item 4). An unknown tool is never gated — it cannot mutate; dispatch
    /// turns it into `unknown_tool` feedback.
    fn is_gated(&self, call: &ToolCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.effect() == Effect::Mutation)
    }

    fn is_parallel_safe(&self, call: &ToolCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.concurrency() == Concurrency::ParallelSafe)
    }

    async fn dispatch_one(
        &self,
        call: &ToolCall,
        denied: Option<&Option<String>>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let tool_span = self.next_span();
        // The call event opens the span whether or not the gate lets the
        // invocation through — a rejected call is a paired, closed span,
        // never a dangling one.
        self.emit(&self.turn_event(
            &tool_span,
            root_span,
            turn,
            EventKind::ToolCall { call: call.clone() },
        ))?;
        let outcome = match denied {
            Some(comment) => rejection(call, comment.as_deref()),
            None => self.execute(call).await,
        };
        self.push_result(call, &tool_span, outcome, messages, root_span, turn)
    }

    /// One batch of parallel-safe calls (ADR-0008 item 2): call events land
    /// in order, results land in call order, and the invocations are driven
    /// cooperatively (one pinned single-item branch per call; `StreamMap`
    /// polls every branch — core stays runtime-free per ADR-0002).
    /// Cooperative means wall-time overlap exists only for futures that
    /// yield (MCP wrappers, approval waits): the built-in fs tools are
    /// blocking and never yield, so their batches currently serialize in
    /// wall time while keeping the same contract. Tool errors stay
    /// per-call; a panicking tool aborts the run, exactly as in serial
    /// execution.
    async fn dispatch_parallel(
        &self,
        calls: &[ToolCall],
        denied: &HashMap<usize, Option<String>>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let mut spans = Vec::with_capacity(calls.len());
        for call in calls {
            let tool_span = self.next_span();
            self.emit(&self.turn_event(
                &tool_span,
                root_span,
                turn,
                EventKind::ToolCall { call: call.clone() },
            ))?;
            spans.push(tool_span);
        }

        // One single-item stream per call; StreamMap polls every branch,
        // so the invocations are driven concurrently on this task. The
        // iter+then pair is load-bearing: `tokio_stream::once` would yield
        // the future OBJECT un-driven (its Item is the value itself).
        let mut branches = tokio_stream::StreamMap::new();
        let mut outcomes: Vec<Option<(Value, Option<EventError>)>> =
            (0..calls.len()).map(|_| None).collect();
        for (offset, call) in calls.iter().enumerate() {
            // A rejected call never becomes a branch: its denial is the
            // settled outcome, pre-filled in call order.
            if let Some(comment) = denied.get(&offset) {
                outcomes[offset] = Some(rejection(call, comment.as_deref()));
                continue;
            }
            let tool = self
                .tools
                .get(&call.name)
                .cloned()
                .expect("parallel-safe calls resolve to a registered tool");
            let arguments = call.arguments.clone();
            // Pinned to the heap for `Unpin`: an in-flight async block is
            // not `Unpin`, which `StreamMap` requires of its branches.
            let branch = Box::pin(
                tokio_stream::iter(std::iter::once(async move { tool.invoke(arguments).await }))
                    .then(std::convert::identity),
            );
            branches.insert(offset, branch);
        }
        while let Some((offset, result)) = branches.next().await {
            outcomes[offset] = Some(match result {
                Ok(content) => (content, None),
                Err(err) => tool_failure(&err),
            });
        }

        for (call, (tool_span, outcome)) in calls.iter().zip(spans.iter().zip(outcomes)) {
            // Invariant, not a runtime condition: every branch yields exactly
            // one item, so every slot is filled. No AgentError variant would
            // help a caller — a miss means this code changed shape — so the
            // assertion panics instead of propagating.
            let outcome = outcome.expect("every call in the batch settled");
            self.push_result(call, tool_span, outcome, messages, root_span, turn)?;
        }
        Ok(())
    }

    /// Appends the result event (errored on failure) and the tool message
    /// (`is_error`-marked on failure) for one settled call.
    fn push_result(
        &self,
        call: &ToolCall,
        tool_span: &str,
        outcome: (Value, Option<EventError>),
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let (result, error) = outcome;
        let is_error = error.is_some();
        let mut result_event = self.turn_event(
            tool_span,
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
        messages.push(if is_error {
            Message::tool_error(call.id.clone(), result)
        } else {
            Message::tool_result(call.id.clone(), result)
        });
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
            Err(err) => tool_failure(&err),
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

fn tool_failure(err: &ToolError) -> (Value, Option<EventError>) {
    (
        Value::String(err.to_string()),
        Some(EventError {
            kind: error_kinds::TOOL.into(),
            message: err.to_string(),
        }),
    )
}

/// A rejected call's outcome: the denial is model feedback (ADR-0008 item 4
/// — a rejection is returned as a tool result so it enters the trajectory),
/// phrased so the model cannot read the effect as having happened; the
/// event's error kind lets reflection tell a reviewer's no from a tool
/// failure.
fn rejection(call: &ToolCall, comment: Option<&str>) -> (Value, Option<EventError>) {
    let reason = comment.unwrap_or("no reason given");
    let message = format!(
        "{} call rejected ({reason}); it was not executed — do not assume its effect \
         happened. Adjust the approach or continue without it.",
        call.name
    );
    (
        Value::String(message.clone()),
        Some(EventError {
            kind: error_kinds::APPROVAL_REJECTED.into(),
            message,
        }),
    )
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

    /// A parallel-safe fake logging start/end with a genuine yield between,
    /// so a concurrent batch verifiably interleaves — a non-yielding fake
    /// completes on its first poll and would mask scheduling regressions.
    struct YieldTool {
        name: &'static str,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
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
    struct StepTool {
        name: &'static str,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
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

    struct FailTool;

    #[async_trait]
    impl AgentTool for FailTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fail".into(),
                description: "always fails".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Err(ToolError {
                tool: "fail".into(),
                message: "boom".into(),
            })
        }
    }

    /// A parallel-safe fake that waits for a notification before ending.
    struct WaitTool {
        name: &'static str,
        notify: Arc<tokio::sync::Notify>,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentTool for WaitTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "wait".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            self.log.lock().await.push(format!("start {}", self.name));
            self.notify.notified().await;
            self.log.lock().await.push(format!("end {}", self.name));
            Ok(Value::String(self.name.into()))
        }
    }

    /// A parallel-safe fake that ends and then releases the waiter — its
    /// completion strictly precedes the waiter's, under any poll order
    /// (`notify_one` stores the permit when nobody waits yet).
    struct ReleaseTool {
        name: &'static str,
        notify: Arc<tokio::sync::Notify>,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentTool for ReleaseTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "release".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            self.log.lock().await.push(format!("start {}", self.name));
            self.log.lock().await.push(format!("end {}", self.name));
            self.notify.notify_one();
            Ok(Value::String(self.name.into()))
        }
    }

    struct ParallelFailTool;

    #[async_trait]
    impl AgentTool for ParallelFailTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "pfail".into(),
                description: "parallel-safe, always fails".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Err(ToolError {
                tool: "pfail".into(),
                message: "boom".into(),
            })
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}),
        }
    }

    fn dispatch_harness(tools: Vec<Arc<dyn AgentTool>>) -> AgentLoop {
        let provider = ReplayProvider::new([]).with_capabilities(test_capabilities());
        let (agent, _sink) = test_loop(provider, tools, 8);
        agent
    }

    #[tokio::test]
    async fn parallel_safe_calls_overlap_and_land_in_call_order() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = dispatch_harness(vec![
            Arc::new(WaitTool {
                name: "p1",
                notify: notify.clone(),
                log: log.clone(),
            }),
            Arc::new(ReleaseTool {
                name: "p2",
                notify,
                log: log.clone(),
            }),
        ]);
        let mut messages = Vec::new();

        // A serialized batch deadlocks on the notification; the timeout turns
        // a concurrency regression into a fast, legible failure.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            agent.dispatch_tools(
                vec![call("c1", "p1"), call("c2", "p2")],
                &mut messages,
                "root",
                1,
            ),
        )
        .await
        .expect("parallel batch must not serialize into a deadlock")
        .expect("dispatch");

        // Overlap proven and completion deterministically staggered: p2 ran
        // to completion and released p1, so "end p2" precedes "end p1" under
        // any branch poll order.
        let log = log.lock().await;
        let end_p2 = log.iter().position(|entry| entry == "end p2");
        let end_p1 = log.iter().position(|entry| entry == "end p1");
        assert!(end_p2 < end_p1, "got: {log:?}");
        drop(log);

        // Results land in call order despite the staggered completion.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(!messages[0].is_error && !messages[1].is_error);
    }

    #[tokio::test]
    async fn a_batch_with_an_undeclared_call_serializes_entirely() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = dispatch_harness(vec![
            Arc::new(YieldTool {
                name: "p1",
                log: log.clone(),
            }),
            Arc::new(YieldTool {
                name: "p2",
                log: log.clone(),
            }),
            Arc::new(StepTool {
                name: "s1",
                log: log.clone(),
            }),
        ]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "p1"), call("c2", "p2"), call("c3", "s1")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // One undeclared call serializes the whole batch, in call order.
        let log = log.lock().await;
        assert_eq!(
            log.as_slice(),
            [
                "start p1", "end p1", "start p2", "end p2", "start s1", "end s1"
            ],
            "got: {log:?}"
        );
        drop(log);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("c3"));
    }

    #[tokio::test]
    async fn tool_errors_are_marked_is_error_in_the_history() {
        let agent = dispatch_harness(vec![Arc::new(FailTool), Arc::new(EchoTool)]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "fail"), call("c2", "echo")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        assert!(messages[0].is_error);
        assert!(
            matches!(&messages[0].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("boom")),
            "got: {:?}",
            messages[0].content
        );
        assert!(!messages[1].is_error);
    }

    #[tokio::test]
    async fn a_failure_inside_a_parallel_batch_stays_per_call() {
        let agent = dispatch_harness(vec![
            Arc::new(ParallelFailTool),
            Arc::new(YieldTool {
                name: "ok",
                log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            }),
        ]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "pfail"), call("c2", "ok")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // The failure is marked and its sibling is unaffected (ADR-0008:
        // a failure cascades only within its own batch).
        assert!(messages[0].is_error);
        assert!(
            matches!(&messages[0].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("boom")),
            "got: {:?}",
            messages[0].content
        );
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(!messages[1].is_error);
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
            // The dispatch-path tests exercise the loop, not the gate: their
            // fakes stay on the fail-safe `Effect::Mutation` default and
            // pass the gate unconditionally.
            AgentLoop::new(
                Arc::new(provider),
                tools,
                Arc::new(crate::testing::ApproveAll),
                max_turns,
                telemetry,
            ),
            sink,
        )
    }

    /// Records every batch it is shown onto a log shared with the tools (so
    /// approve-vs-execute ordering is observable) and answers with exactly
    /// `decisions` — shorter or longer than the batch on purpose, to pin the
    /// conservative-mismatch behavior.
    struct ScriptedApprover {
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
        decisions: Vec<Approval>,
    }

    #[async_trait]
    impl Approver for ScriptedApprover {
        async fn approve(&self, calls: &[ToolCall]) -> Vec<Approval> {
            let mut log = self.log.lock().await;
            for call in calls {
                log.push(format!("approve {}", call.name));
            }
            drop(log);
            self.decisions.clone()
        }
    }

    fn gate_harness(
        tools: Vec<Arc<dyn AgentTool>>,
        approver: ScriptedApprover,
    ) -> (AgentLoop, Arc<crate::testing::RecordingSink>) {
        let provider = ReplayProvider::new([]).with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        (
            AgentLoop::new(Arc::new(provider), tools, Arc::new(approver), 8, telemetry),
            sink,
        )
    }

    #[tokio::test]
    async fn a_rejected_mutation_never_executes_and_enters_the_trajectory() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let provider = ReplayProvider::new([
            ReplayProvider::script(vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: "step".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::ToolCalls,
                },
            ]),
            text_script("understood"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(StepTool {
                name: "step",
                log: log.clone(),
            })],
            Arc::new(ScriptedApprover {
                log: log.clone(),
                decisions: vec![Approval::Rejected {
                    comment: Some("not today".into()),
                }],
            }),
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");

        // The gate answered, the tool never invoked.
        assert_eq!(log.lock().await.as_slice(), ["approve step"]);
        // The rejection is model feedback: an is_error tool result naming
        // the reason, never a silent skip.
        let message = &outcome.messages[2];
        assert!(message.is_error);
        assert!(
            matches!(&message.content[0], cadmus_contract::ContentPart::Text { text } if text.contains("rejected") && text.contains("not today")),
            "got: {:?}",
            message.content
        );
        // The trajectory carries the routing kind, so reflection can tell a
        // reviewer's no from a tool failure.
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
            Some(error_kinds::APPROVAL_REJECTED)
        );
        // The fold mirrors the loop: replayed state matches live state, and
        // the rejected call's span stays paired (its tool_call event exists).
        let events = sink.events();
        assert!(
            events.iter().any(
                |event| matches!(&event.kind, EventKind::ToolCall { call } if call.id == "c1")
            )
        );
        let state = crate::replay_trace(&events);
        assert!(state.dangling_tool_calls.is_empty());
        assert_eq!(state.messages, outcome.messages);
    }

    #[tokio::test]
    async fn batch_approval_decides_each_call_independently_before_any_execution() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink) = gate_harness(
            vec![
                Arc::new(StepTool {
                    name: "w1",
                    log: log.clone(),
                }),
                Arc::new(StepTool {
                    name: "w2",
                    log: log.clone(),
                }),
            ],
            ScriptedApprover {
                log: log.clone(),
                decisions: vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                ],
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "w1"), call("c2", "w2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // Both calls were presented as one batch before either executed.
        assert_eq!(
            log.lock().await.as_slice(),
            ["approve w1", "approve w2", "start w1", "end w1"]
        );
        // Approved executed, rejected denied — and the results keep call
        // order.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert!(!messages[0].is_error);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(messages[1].is_error);
    }

    #[tokio::test]
    async fn a_rejected_call_inside_a_parallel_batch_still_lands_in_call_order() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // Both fakes are parallel-safe and (by the fail-safe default)
        // mutations, so the batch takes the parallel schedule with c1
        // pre-settled by the gate.
        let (agent, sink) = gate_harness(
            vec![
                Arc::new(YieldTool {
                    name: "m1",
                    log: log.clone(),
                }),
                Arc::new(YieldTool {
                    name: "m2",
                    log: log.clone(),
                }),
            ],
            ScriptedApprover {
                log: log.clone(),
                decisions: vec![
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                    Approval::Approved,
                ],
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "m1"), call("c2", "m2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        assert_eq!(
            log.lock().await.as_slice(),
            ["approve m1", "approve m2", "start m2", "end m2"]
        );
        assert_eq!(messages.len(), 2);
        assert!(messages[0].is_error);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert!(!messages[1].is_error);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        // The pre-settled rejection keeps the event pairing and the fold
        // identical to the live history.
        let events = sink.events();
        for id in ["c1", "c2"] {
            assert!(
                events.iter().any(
                    |event| matches!(&event.kind, EventKind::ToolCall { call } if call.id == id)
                )
            );
        }
        let state = crate::replay_trace(&events);
        assert!(state.dangling_tool_calls.is_empty());
        assert_eq!(state.messages, messages);
    }

    #[tokio::test]
    async fn a_short_decision_reply_denies_the_remainder() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink) = gate_harness(
            vec![
                Arc::new(StepTool {
                    name: "w1",
                    log: log.clone(),
                }),
                Arc::new(StepTool {
                    name: "w2",
                    log: log.clone(),
                }),
            ],
            ScriptedApprover {
                log: log.clone(),
                // One decision for two gated calls: the unanswered second
                // call must deny, not execute (ADR-0008 item 4).
                decisions: vec![Approval::Approved],
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "w1"), call("c2", "w2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        assert!(!messages[0].is_error);
        assert!(messages[1].is_error);
        assert!(
            matches!(&messages[1].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("no reason given")),
            "got: {:?}",
            messages[1].content
        );
    }

    /// A perception fake: `Effect::Perception` declared, so the gate never
    /// sees it (ADR-0008 item 4).
    struct PeekTool;

    #[async_trait]
    impl AgentTool for PeekTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "peek".into(),
                description: "peek".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn effect(&self) -> Effect {
            Effect::Perception
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("seen".into()))
        }
    }

    #[tokio::test]
    async fn perception_calls_never_reach_the_gate() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink) = gate_harness(
            vec![Arc::new(PeekTool)],
            ScriptedApprover {
                log: log.clone(),
                // Any answer would do — the gate must not ask at all.
                decisions: vec![],
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "peek")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert!(
            log.lock().await.is_empty(),
            "the gate was shown a perception call"
        );
        assert!(!messages[0].is_error);
    }

    #[tokio::test]
    async fn extra_decisions_are_ignored() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink) = gate_harness(
            vec![Arc::new(StepTool {
                name: "w1",
                log: log.clone(),
            })],
            ScriptedApprover {
                log: log.clone(),
                // More decisions than gated calls: the stray rejection must
                // not leak into the batch.
                decisions: vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("stray".into()),
                    },
                ],
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "w1")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert_eq!(
            log.lock().await.as_slice(),
            ["approve w1", "start w1", "end w1"]
        );
        assert!(!messages[0].is_error);
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
        assert!(outcome.messages[2].is_error);
        // Both channels carry the failure: the message is flagged, and the
        // tool_result event is errored with kind unknown_tool.
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
