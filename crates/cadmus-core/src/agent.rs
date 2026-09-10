use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cadmus_contract::{
    Approval, ArtifactSink, ChatRequest, Clock, Command, CommandSource, EstimateSource, Event,
    EventError, EventKind, EventSink, FinishReason, FoldedRef, IdSequence, InstructionFile,
    LiveItem, LiveKind, LiveSink, Message, ModelError, Provider, SteerMode, TodoItem, ToolCall,
    ToolSpec, TurnOutcome, attrs, error_kinds,
};
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::context::{
    FoldPolicy, FrozenPrefix, InstructionTracker, StatusProbe, TODO_WRITE, TrailerView,
    fold_placeholder_message, format_injected, message_text, render_trailer,
};
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

/// The context pipeline's injected bundle (ADR-0007): the run-frozen prefix,
/// the per-turn freshness probe, the nested-instruction tracker, the
/// run-static cwd, the spill-artifact sink and the fold policy. Assembly
/// and rendering stay pure (`crate::context`); the wiring layer fills these
/// from the filesystem.
pub struct ContextBundle {
    pub prefix: FrozenPrefix,
    pub probe: Arc<dyn StatusProbe>,
    pub tracker: Arc<dyn InstructionTracker>,
    pub cwd: String,
    pub artifacts: Arc<dyn ArtifactSink>,
    pub fold_policy: FoldPolicy,
}

/// The client-protocol bundle injected into the loop (ADR-0013): `live` is
/// the ephemeral downstream (deltas, span boundaries via recorded-event
/// republication, approval requests), `commands` the only upstream (resolve,
/// steer, interrupt). A client is an event subscriber plus a command producer —
/// the loop never talks to a client any other way, so the TUI, headless
/// clients and phase 5's remote attach all share this one path.
pub struct ClientProtocol {
    pub live: Arc<dyn LiveSink>,
    pub commands: Arc<dyn CommandSource>,
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
    /// The client interrupted the run (ADR-0011 item 3's Esc): completed
    /// work is preserved — the partial turn and the terminal record carry
    /// [`error_kinds::INTERRUPTED`].
    #[error("run interrupted by the client")]
    Interrupted,
}

/// The minimal agent loop: stream → assemble → dispatch tool calls → repeat,
/// appending each step to the trace log and publishing it to the live
/// stream. Everything external is injected (provider, tools, protocol,
/// telemetry, limits) — no hidden time, randomness or IO.
pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn AgentTool>>,
    context: ContextBundle,
    protocol: ClientProtocol,
    specs: Vec<ToolSpec>,
    max_turns: usize,
    telemetry: Telemetry,
    inbox: Mutex<Inbox>,
    /// Trailer state (ADR-0007 item 1(c)): per-tool execution counters and
    /// the `todo_write` list — code-folded from settled calls, never
    /// model-recomputed.
    tool_counts: Mutex<BTreeMap<String, usize>>,
    todos: Mutex<Vec<TodoItem>>,
    /// Fold state (ADR-0007 item 2): every tool result's coordinates, the
    /// substitution map (prebuilt placeholder messages, so the per-turn
    /// render and estimate never re-derive them), the growth baseline and
    /// the provider's last reported input tokens.
    tool_result_tracks: Mutex<Vec<ResultTrack>>,
    folded: Mutex<BTreeMap<usize, Message>>,
    fold_baseline: Mutex<u64>,
    last_reported_input: Mutex<Option<u64>>,
}

/// One tool result's fold-relevant coordinates: its history position (the
/// render-substitution key), its turn (the recency scope) and its event id
/// (the directive's reference — the log references by id, never by path).
#[derive(Clone)]
struct ResultTrack {
    msg_index: usize,
    turn: usize,
    event_id: String,
    call_id: String,
}

/// Commands received ahead of their application point, classified on
/// receipt (ADR-0013 item 6: the loop applies each command in receipt
/// order at its defined point). The kinds split because their application
/// points and consumption rules differ — a resolve is matched to its gate
/// by request id, steers partition by mode and apply in receipt order, an
/// interrupt is first-wins state — one shared queue would make every
/// consumer re-scan and re-order. Client command ids are deduped at the
/// gate of every path — a retried submission applies exactly once.
#[derive(Default)]
struct Inbox {
    /// Resolves awaiting their gate (a resolve can only name an already
    /// published request, so a stale one sits harmlessly until the run
    /// ends — late answers and retries are safe by construction).
    resolves: VecDeque<Command>,
    /// Steers not yet applied; they land as user messages at the next
    /// application point, in receipt order.
    steers: Vec<Command>,
    /// The first pending interrupt (a second one changes nothing).
    interrupt: Option<Command>,
    /// Client command ids already seen — the idempotent-retry seam.
    seen: HashSet<String>,
}

impl AgentLoop {
    #[must_use]
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn AgentTool>>,
        context: ContextBundle,
        protocol: ClientProtocol,
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
            context,
            protocol,
            specs,
            max_turns,
            telemetry,
            inbox: Mutex::new(Inbox::default()),
            tool_counts: Mutex::new(BTreeMap::new()),
            todos: Mutex::new(Vec::new()),
            tool_result_tracks: Mutex::new(Vec::new()),
            folded: Mutex::new(BTreeMap::new()),
            fold_baseline: Mutex::new(0),
            last_reported_input: Mutex::new(None),
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
                prefix: Some(self.context.prefix.record()),
            }),
        );
        start_run.attributes = self.telemetry.run_attributes.clone();
        start_run
            .attributes
            .insert(attrs::PREFIX_HASH.into(), self.context.prefix.hash().into());
        self.emit(&start_run)?;

        for turn in 1..=self.max_turns {
            // Turn-boundary command application: a pending interrupt ends
            // the run here; buffered steers land as user messages before
            // this turn's request. Commands are recorded at application —
            // never at receipt — so the replayed history equals the live
            // one (ADR-0005's fold invariant).
            self.drain_boundary(&mut messages, &root_span, turn)?;
            let turn_span = self.next_span();
            // The fold decision point: periodic hygiene and the ceiling rule
            // (ADR-0007's 2026-09-10 amendment). After it, the render
            // substitutes every folded result with its placeholder.
            self.maybe_fold(&messages, &root_span, turn)?;
            // Three-segment render (ADR-0007): frozen prefix + history +
            // fresh trailer. The trailer never enters `messages` — the
            // history stays the true conversation; the rendered bytes ride
            // the request event so replay audits exactly what the model saw.
            let trailer = self.render_trailer();
            let mut request_messages = Vec::with_capacity(messages.len() + 2);
            request_messages.push(self.context.prefix.message());
            {
                let folded = self.folded.lock().expect("folded poisoned");
                request_messages.extend(
                    messages
                        .iter()
                        .enumerate()
                        .map(|(index, message)| folded.get(&index).unwrap_or(message).clone()),
                );
            }
            request_messages.push(Message::user(trailer.clone()));
            // Per-turn rebuild is deliberate: the provider borrows an immutable
            // request while the loop owns the growing history. The cost is a
            // turn-boundary memcpy — negligible against the network call that
            // follows (the hot path, stream aggregation, stays clone-light).
            let request = base.clone().with_messages(request_messages);
            self.emit(&self.turn_event(
                &turn_span,
                &root_span,
                turn,
                EventKind::LlmRequest {
                    trailer: Some(trailer),
                },
            ))?;

            let turn_result = self
                .assistant_turn(&request, &turn_span, &root_span, turn)
                .await?;
            // The freshest exact usage measurement feeds the fold estimator
            // (amendment item 4); a usage-less turn keeps the last known
            // value rather than dropping to the heuristic mid-run.
            if let Some(usage) = &turn_result.usage {
                *self.last_reported_input.lock().expect("usage poisoned") = Some(usage.input);
            }
            messages.push(turn_result.message.clone());
            let calls: Vec<_> = turn_result.message.tool_calls().cloned().collect();
            // Newly-entered subtrees surface before dispatch: the tracker
            // reads the calls' intent, so a failed or denied call still
            // injects — the model is already operating there.
            let nested = self.context.tracker.on_calls(&calls);
            if calls.is_empty() {
                self.poll_incoming();
                // An interrupt at the exact finish line is moot — the run
                // completed first; it is dropped unrecorded (it had no
                // effect). A queued steer instead continues the run: the
                // user's "one more thing" (ADR-0011 item 3).
                if self.take_interrupt().is_none()
                    && self.apply_steers(&mut messages, &root_span, turn + 1, true)? > 0
                {
                    continue;
                }
                self.finish_with(&root_span, turn, None)?;
                return Ok(RunOutcome {
                    messages,
                    final_turn: turn_result,
                    turns: turn,
                });
            }
            self.dispatch_tools(calls, &mut messages, &root_span, turn)
                .await?;
            self.inject_nested(nested, &mut messages, &root_span, turn)?;
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
    /// `llm_response` event. Every chunk is also published to the live
    /// stream before the assembler folds it (ADR-0013 item 2), so a
    /// subscriber's own fold reconciles exactly with the recorded turn.
    /// Failure paths record what there is to record before returning:
    /// a call-level error leaves the turn span unclosed (no response ever
    /// arrived), a mid-stream error records the partial turn errored, an
    /// empty turn records its classification, an interrupt records the
    /// command and then the truncated turn.
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
        let mut stream_error = None;
        let mut interrupt = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    self.publish(LiveKind::AssistantDelta {
                        turn: u32::try_from(turn).unwrap_or(u32::MAX),
                        chunk: chunk.clone(),
                    });
                    assembler.push(chunk);
                    // The mid-stream steering/interrupt seam, chunk
                    // granular: commands apply between chunks. A stalled
                    // stream still ends on the provider's own error path —
                    // core stays runtime-free, so there is no `select!`
                    // against the command channel here.
                    self.poll_incoming();
                    if let Some(command) = self.take_interrupt() {
                        interrupt = Some(command);
                        break;
                    }
                }
                Err(error) => {
                    stream_error = Some(error);
                    break;
                }
            }
        }
        // Dropping the stream mid-flight is the cancellation semantic (the
        // provider port's contract); a broken-off stream never poisons the
        // provider.
        drop(stream);
        let turn_result = assembler.complete();

        if let Some(error) = stream_error {
            let response = self
                .turn_event(turn_span, root_span, turn, response_kind(&turn_result))
                .errored(model_event_error(&error));
            self.emit(&response)?;
            self.finish_with(root_span, turn - 1, Some(model_event_error(&error)))?;
            return Err(AgentError::Provider(error));
        }

        if let Some(command) = interrupt {
            // The command lands before its consequences: interrupt, then
            // the truncated turn it caused, then the terminal record.
            self.record_command(command, root_span, turn)?;
            let detail = interrupted_detail();
            let response = self
                .turn_event(turn_span, root_span, turn, response_kind(&turn_result))
                .errored(detail.clone());
            self.emit(&response)?;
            self.finish_with(root_span, turn - 1, Some(detail))?;
            return Err(AgentError::Interrupted);
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
    /// the turn's gated (mutation) calls are presented together, each
    /// approved or rejected independently, so a decision can depend on the
    /// batch's contents but never on another gated call's result. A
    /// rejected call never invokes; its rejection lands as an `is_error`
    /// tool result in call order.
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
        let denied = self.gate(&calls, root_span, turn).await?;
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

    /// Presents the batch's gated calls as one approval request on the live
    /// stream and awaits the matching `resolve_approval` command (ADR-0008
    /// item 4 / ADR-0013 item 6): one path for local and future remote
    /// clients alike, and the resolution itself enters the trajectory as a
    /// command event — the approval is recorded, not just its effects.
    ///
    /// Returns the rejections keyed by batch position. Position, not call
    /// id: ids are provider-supplied wire data with no uniqueness check,
    /// and a duplicated id must never let one call's rejection deny its
    /// same-id sibling. A short decision reply denies the remainder:
    /// unanswered is deny (ADR-0008 item 4); extra decisions are ignored;
    /// a resolve naming another request is dropped. A command channel that
    /// closes mid-wait denies the whole batch.
    ///
    /// An interrupt during the wait ends the run before any gated call
    /// executes: no call or result events land — the trajectory shows the
    /// model's intent (the recorded response), then the interrupt. Steers
    /// received during the wait classify into the inbox and apply at the
    /// next boundary.
    ///
    /// Timeout: a client that waits on a human pairs its wait with its own
    /// deny timeout (ADR-0008 item 4: human-in-the-loop always pairs a
    /// timeout with the conservative default). The in-process policies
    /// answer synchronously, so no core-side timeout mechanism exists yet
    /// — it lands with the first waiting client (the TUI).
    async fn gate(
        &self,
        calls: &[ToolCall],
        root_span: &str,
        turn: usize,
    ) -> Result<HashMap<usize, Option<String>>, AgentError> {
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
            return Ok(denied);
        }

        let request_id = format!("ap{}", self.telemetry.ids.next());
        self.publish(LiveKind::ApprovalRequested {
            request_id: request_id.clone(),
            turn: u32::try_from(turn).unwrap_or(u32::MAX),
            calls: batch,
        });

        let command = loop {
            if let Some(command) = self.take_resolve(&request_id) {
                break command;
            }
            if let Some(command) = self.take_interrupt() {
                self.record_command(command, root_span, turn)?;
                self.finish_with(root_span, turn, Some(interrupted_detail()))?;
                return Err(AgentError::Interrupted);
            }
            match self.protocol.commands.recv().await {
                Some(command) => self.note(command),
                None => return Ok(deny_all(&positions)),
            }
        };
        let Command::ResolveApproval { decisions, .. } = command.clone() else {
            unreachable!("take_resolve returns only matching resolves");
        };
        self.record_command(command, root_span, turn)?;
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
        Ok(denied)
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
        self.note_outcome(call, error.as_ref());
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
        // Track the result's coordinates for the fold machinery: the history
        // position it is about to occupy, its turn and its event id.
        self.tool_result_tracks
            .lock()
            .expect("tracks poisoned")
            .push(ResultTrack {
                msg_index: messages.len(),
                turn,
                event_id: result_event.id.clone(),
                call_id: call.id.clone(),
            });
        messages.push(if is_error {
            Message::tool_error(call.id.clone(), result)
        } else {
            Message::tool_result(call.id.clone(), result)
        });
        Ok(())
    }

    /// The fold decision point (ADR-0007 item 2 + the 2026-09-10
    /// amendment): periodic hygiene every Δ estimated tokens of growth, and
    /// the ceiling rule — fold first there; a ceiling fold with nothing
    /// foldable falls through to the pre-existing `ContextLength` path (the
    /// phase-2 compactor is that case's designed answer, not a silent carry).
    fn maybe_fold(
        &self,
        messages: &[Message],
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let max_context = u64::from(self.provider.capabilities().max_context);
        let (estimate, estimator) = self.estimate_tokens(messages);
        let policy = &self.context.fold_policy;
        let baseline = *self.fold_baseline.lock().expect("baseline poisoned");
        let at_ceiling = estimate >= max_context * policy.ceiling_percent / 100;
        let delta = policy.growth_max_tokens.min(max_context / 10);
        if estimate.saturating_sub(baseline) < delta && !at_ceiling {
            return Ok(());
        }

        // Candidates: past the recency scope and the size floor, not already
        // folded. Locks come off before any spill IO.
        let candidates: Vec<ResultTrack> = {
            let folded = self.folded.lock().expect("folded poisoned");
            let tracks = self.tool_result_tracks.lock().expect("tracks poisoned");
            tracks
                .iter()
                .filter(|track| track.turn + policy.recent_turns < turn)
                .filter(|track| !folded.contains_key(&track.msg_index))
                .filter(|track| message_text(&messages[track.msg_index]).len() >= policy.min_bytes)
                .cloned()
                .collect()
        };
        if candidates.is_empty() {
            return Ok(());
        }

        // Spill first (the directive references the artifacts), then record
        // the directive, then update the substitution state — a crash before
        // the directive leaves harmless orphan artifacts, never a dangling
        // reference (the inverse order would be the corrupt one).
        let mut folded_refs: Vec<(usize, FoldedRef, Message)> = Vec::new();
        for track in candidates {
            let original = message_text(&messages[track.msg_index]);
            let spill = self
                .context
                .artifacts
                .spill(&format!("m{}.txt", track.msg_index), &original)?;
            let fold_ref = FoldedRef {
                event_id: track.event_id,
                call_id: track.call_id,
                spill,
                original_bytes: u64::try_from(original.len()).unwrap_or(u64::MAX),
            };
            let placeholder = fold_placeholder_message(&messages[track.msg_index], &fold_ref);
            folded_refs.push((track.msg_index, fold_ref, placeholder));
        }
        let directive = self.turn_event(
            &self.next_span(),
            root_span,
            turn,
            EventKind::Fold {
                folded: folded_refs
                    .iter()
                    .map(|(_, fold_ref, _)| fold_ref.clone())
                    .collect(),
                estimate,
                estimator,
            },
        );
        self.emit(&directive)?;

        let mut folded = self.folded.lock().expect("folded poisoned");
        let mut saved_bytes = 0usize;
        for (msg_index, _, placeholder) in folded_refs {
            // Savings in the estimate's own unit (serialized render bytes),
            // so the post-fold baseline stays on the cadence's scale.
            let before = serde_json::to_vec(&messages[msg_index]).map_or(0, |v| v.len());
            let after = serde_json::to_vec(&placeholder).map_or(0, |v| v.len());
            saved_bytes += before.saturating_sub(after);
            folded.insert(msg_index, placeholder);
        }
        // The post-fold baseline: the cadence measures growth from here.
        let saved_tokens = u64::try_from(saved_bytes / 4).unwrap_or(u64::MAX);
        *self.fold_baseline.lock().expect("baseline poisoned") =
            estimate.saturating_sub(saved_tokens);
        Ok(())
    }

    /// The boundary usage estimate (amendment item 4): the provider's last
    /// reported input tokens when present — exact but for the not-yet-sent
    /// growth — else the chars/4 heuristic over the upcoming render (prefix
    /// plus history with folded substitutions applied, so the estimate
    /// measures what the model actually sees and the cadence cannot
    /// degenerate into per-turn folding; the trailer is noise here).
    fn estimate_tokens(&self, messages: &[Message]) -> (u64, EstimateSource) {
        if let Some(tokens) = *self.last_reported_input.lock().expect("usage poisoned") {
            return (tokens, EstimateSource::Provider);
        }
        let folded = self.folded.lock().expect("folded poisoned");
        let bytes = self.context.prefix.byte_len()
            + messages
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    let rendered = folded.get(&index).unwrap_or(message);
                    serde_json::to_vec(rendered).map_or(0, |bytes| bytes.len())
                })
                .sum::<usize>();
        (
            u64::try_from(bytes / 4).unwrap_or(u64::MAX),
            EstimateSource::Chars4,
        )
    }

    /// Folds one settled call into the trailer state (ADR-0007 item 1(c)):
    /// the per-tool counter counts executions — gate rejections and
    /// unknown-tool answers never executed, so neither is counted — and a
    /// successful `todo_write` replaces the todo list verbatim (the one
    /// model-authored value code may store, per the item's own exception).
    fn note_outcome(&self, call: &ToolCall, error: Option<&EventError>) {
        let never_executed = matches!(
            error,
            Some(e) if e.kind == error_kinds::APPROVAL_REJECTED || e.kind == error_kinds::UNKNOWN_TOOL
        );
        if !never_executed {
            *self
                .tool_counts
                .lock()
                .expect("tool counts poisoned")
                .entry(call.name.clone())
                .or_insert(0) += 1;
        }
        if error.is_none()
            && call.name == TODO_WRITE
            && let Some(items) = call.arguments.get("items")
            && let Ok(items) = serde_json::from_value::<Vec<TodoItem>>(items.clone())
        {
            *self.todos.lock().expect("todos poisoned") = items;
        }
    }

    /// Renders this turn's trailer from the probe's fresh snapshot and the
    /// folded state. The probe runs before any lock is taken — it may block
    /// on a subprocess, and the folded state does not depend on it.
    fn render_trailer(&self) -> String {
        let git = self.context.probe.snapshot();
        let counts = self.tool_counts.lock().expect("tool counts poisoned");
        let todos = self.todos.lock().expect("todos poisoned");
        render_trailer(&TrailerView {
            cwd: &self.context.cwd,
            git,
            tool_counts: &counts,
            todos: &todos,
        })
    }

    /// Appends newly-entered subtrees' instruction files as standalone user
    /// messages (ADR-0007 item 1(a)): never folded into a tool result (that
    /// would pollute the errors-are-corrections channel), each recorded as
    /// an `instruction_injected` event so the fold rebuilds identical bytes.
    fn inject_nested(
        &self,
        files: Vec<InstructionFile>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        for file in files {
            let text = format_injected(&file);
            let span = self.next_span();
            self.emit(&self.turn_event(
                &span,
                root_span,
                turn,
                EventKind::InstructionInjected {
                    path: file.path,
                    content: file.content,
                },
            ))?;
            messages.push(Message::user(text));
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
            Err(err) => tool_failure(&err),
        }
    }

    fn emit(&self, event: &Event) -> Result<(), AgentError> {
        // Append first, publish second: a log failure aborts the run before
        // any subscriber sees the event — clients never observe state the
        // trajectory does not have. The republished copy is what makes the
        // log's contents observable on the live stream (ADR-0013 item 2).
        self.telemetry.sink.append(event)?;
        self.protocol.live.publish(&LiveItem {
            seq: event.seq,
            trace_id: event.trace_id.clone(),
            kind: LiveKind::Recorded {
                event: Box::new(event.clone()),
            },
        });
        Ok(())
    }

    /// Publishes one ephemeral live item, stamped from the same sequence
    /// the durable events draw from (one total order across both channels,
    /// ADR-0013 item 4). Best-effort: a lagging subscriber is told to
    /// re-sync, never awaited.
    fn publish(&self, kind: LiveKind) {
        self.protocol.live.publish(&LiveItem {
            seq: self.telemetry.ids.next(),
            trace_id: self.telemetry.trace_id.clone(),
            kind,
        });
    }

    /// Appends a client command to the trace — off the run root, stamped
    /// with the turn it takes effect in — and publishes it live through
    /// [`Self::emit`].
    fn record_command(
        &self,
        command: Command,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let span = self.next_span();
        self.emit(&self.turn_event(&span, root_span, turn, EventKind::Command(command)))
    }

    /// Classifies one received command into the inbox, deduped by the
    /// client-minted command id (ADR-0002's idempotent-retry seam). A
    /// `start_run` mid-run is a protocol anomaly: ignored.
    fn note(&self, command: Command) {
        let mut inbox = self.inbox.lock().expect("inbox poisoned");
        if let Some(id) = command.command_id()
            && !inbox.seen.insert(id.to_string())
        {
            return;
        }
        match command {
            Command::ResolveApproval { .. } => inbox.resolves.push_back(command),
            Command::Steer { .. } => inbox.steers.push(command),
            Command::Interrupt { .. } => {
                if inbox.interrupt.is_none() {
                    inbox.interrupt = Some(command);
                }
            }
            Command::StartRun { .. } => {}
        }
    }

    /// Non-blocking drain of the command source into the inbox — the
    /// mid-stream and boundary seam.
    fn poll_incoming(&self) {
        while let Some(command) = self.protocol.commands.poll() {
            self.note(command);
        }
    }

    /// The pending interrupt, taken once (the caller applies it).
    fn take_interrupt(&self) -> Option<Command> {
        self.inbox.lock().expect("inbox poisoned").interrupt.take()
    }

    /// The resolve naming `request_id`, taken out of the inbox once.
    fn take_resolve(&self, request_id: &str) -> Option<Command> {
        let mut inbox = self.inbox.lock().expect("inbox poisoned");
        let position = inbox.resolves.iter().position(|command| {
            matches!(
                command,
                Command::ResolveApproval { request_id: rid, .. } if rid == request_id
            )
        })?;
        inbox.resolves.remove(position)
    }

    /// Turn-boundary application point: a pending interrupt ends the run
    /// (command recorded, then the terminal record); otherwise every
    /// buffered inject-mode steer lands as a user message ahead of this
    /// turn's request. Queue-mode steers stay buffered — they apply only
    /// when the run would otherwise finish.
    fn drain_boundary(
        &self,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        self.poll_incoming();
        if let Some(command) = self.take_interrupt() {
            self.record_command(command, root_span, turn)?;
            self.finish_with(root_span, turn - 1, Some(interrupted_detail()))?;
            return Err(AgentError::Interrupted);
        }
        self.apply_steers(messages, root_span, turn, false)?;
        Ok(())
    }

    /// Appends buffered steers as user messages, recording each command at
    /// its application point (the fold-invariant recording rule). Inject
    /// steers apply at any boundary; queue steers only when
    /// `include_queued` (the run's would-be finish line). Returns how many
    /// steers were applied.
    fn apply_steers(
        &self,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
        include_queued: bool,
    ) -> Result<usize, AgentError> {
        let steers: Vec<Command> = {
            let mut inbox = self.inbox.lock().expect("inbox poisoned");
            let (apply, keep) = std::mem::take(&mut inbox.steers).into_iter().partition(
                |command| {
                    include_queued
                        || matches!(command, Command::Steer { mode, .. } if *mode == SteerMode::Inject)
                },
            );
            inbox.steers = keep;
            apply
        };
        let count = steers.len();
        for command in steers {
            let Command::Steer { text, .. } = &command else {
                continue;
            };
            messages.push(Message::user(text.clone()));
            self.record_command(command, root_span, turn)?;
        }
        Ok(count)
    }

    fn envelope(&self, span: &str, parent: Option<&str>, kind: EventKind) -> Event {
        // The id and the seq mint from one counter (`e7` ↔ seq 7), so the
        // log identity and the client-protocol position never disagree
        // (ADR-0013 item 4's total order, one sequence for both channels).
        let seq = self.telemetry.ids.next();
        Event::new(
            seq,
            format!("e{seq}"),
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

/// Every gated position denied without a reason — the gate's conservative
/// default when the command channel closes mid-wait (unanswered is deny,
/// ADR-0008 item 4).
fn deny_all(positions: &[usize]) -> HashMap<usize, Option<String>> {
    positions.iter().map(|position| (*position, None)).collect()
}

/// The structured detail of an interrupted run, carried by the truncated
/// turn and the terminal record alike.
fn interrupted_detail() -> EventError {
    EventError {
        kind: error_kinds::INTERRUPTED.into(),
        message: AgentError::Interrupted.to_string(),
    }
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

    /// A gate harness whose client resolves every request through the
    /// command channel with `policy`'s fixed decisions — shorter or longer
    /// than the batch on purpose, to pin the conservative-mismatch
    /// behavior. The recording live sink comes back: the protocol's items
    /// are the ordering evidence (request before any execution).
    fn gate_harness(
        tools: Vec<Arc<dyn AgentTool>>,
        policy: impl Fn(&[ToolCall]) -> Vec<Approval> + Send + Sync + 'static,
    ) -> (
        AgentLoop,
        Arc<crate::testing::RecordingSink>,
        Arc<crate::testing::RecordingLive>,
    ) {
        let provider = ReplayProvider::new([]).with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, live, _sender) = crate::testing::protocol_with(policy);
        (
            AgentLoop::new(
                Arc::new(provider),
                tools,
                crate::testing::test_context(),
                protocol,
                8,
                telemetry,
            ),
            sink,
            live,
        )
    }

    /// Sends one scripted command the first time a live item matches — how
    /// a test plants a command at a precise protocol point, the way a real
    /// client reacts to the stream (ADR-0013 item 6).
    struct SendOnMatch<P> {
        inner: Arc<crate::testing::RecordingLive>,
        commands: std::sync::mpsc::Sender<Command>,
        predicate: P,
        command: Mutex<Option<Command>>,
    }

    impl<P> SendOnMatch<P> {
        fn new(
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

    /// The position of the first approval request and of the first recorded
    /// tool call in the live stream — the approve-before-execute evidence.
    fn request_and_call_positions(live: &crate::testing::RecordingLive) -> (usize, usize) {
        let items = live.items();
        let request = items
            .iter()
            .position(|item| matches!(item.kind, LiveKind::ApprovalRequested { .. }))
            .expect("an approval request was published");
        let call = items
            .iter()
            .position(|item| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event } if matches!(event.kind, EventKind::ToolCall { .. })
                )
            })
            .expect("a tool call was recorded");
        (request, call)
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
        let (protocol, live, _sender) = crate::testing::protocol_with(|_| {
            vec![Approval::Rejected {
                comment: Some("not today".into()),
            }]
        });
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(StepTool {
                name: "step",
                log: log.clone(),
            })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");

        // The tool never invoked.
        assert!(
            log.lock().await.is_empty(),
            "a rejected call must not execute"
        );
        // The request preceded any execution, and the resolution entered the
        // trajectory as a command event — the approval itself is recorded,
        // not just its effect (ADR-0008 item 4 / ADR-0013 item 6).
        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        let events = sink.events();
        let resolve = events.iter().find_map(|event| match &event.kind {
            EventKind::Command(Command::ResolveApproval {
                request_id,
                decisions,
                ..
            }) => Some((request_id.clone(), decisions.clone())),
            _ => None,
        });
        let Some((request_id, decisions)) = resolve else {
            panic!("the resolve command must be recorded");
        };
        assert!(
            live.items().iter().any(|item| matches!(
                &item.kind,
                LiveKind::ApprovalRequested { request_id: rid, .. } if *rid == request_id
            )),
            "the recorded resolve answers the published request"
        );
        assert_eq!(
            decisions,
            vec![Approval::Rejected {
                comment: Some("not today".into())
            }]
        );
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
        let tool_result = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::ToolResult { .. }))
            .expect("a tool_result event");
        assert_eq!(tool_result.status, Status::Error);
        assert_eq!(
            tool_result.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::APPROVAL_REJECTED)
        );
        // The fold mirrors the loop: replayed state matches live state, and
        // the rejected call's span stays paired (its tool_call event exists).
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
        let (agent, _sink, live) = gate_harness(
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
            |_| {
                vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                ]
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

        // The batch was presented before either call executed.
        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        // Approved executed, rejected denied — and the results keep call
        // order.
        assert_eq!(
            log.lock().await.as_slice(),
            ["start w1", "end w1"],
            "the rejected call never runs"
        );
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
        let (agent, sink, live) = gate_harness(
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
            |_| {
                vec![
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                    Approval::Approved,
                ]
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

        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        assert_eq!(log.lock().await.as_slice(), ["start m2", "end m2"]);
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
        let (agent, _sink, _live) = gate_harness(
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
            // One decision for two gated calls: the unanswered second call
            // must deny, not execute (ADR-0008 item 4).
            |_| vec![Approval::Approved],
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
        let (agent, _sink, live) = gate_harness(
            vec![Arc::new(PeekTool)],
            // Any answer would do — the gate must not ask at all.
            |_| panic!("the gate was shown a perception call"),
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "peek")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert!(
            !live
                .items()
                .iter()
                .any(|item| matches!(item.kind, LiveKind::ApprovalRequested { .. })),
            "a perception call must not open an approval request"
        );
        assert!(!messages[0].is_error);
    }

    #[tokio::test]
    async fn extra_decisions_are_ignored() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink, _live) = gate_harness(
            vec![Arc::new(StepTool {
                name: "w1",
                log: log.clone(),
            })],
            // More decisions than gated calls: the stray rejection must not
            // leak into the batch.
            |_| {
                vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("stray".into()),
                    },
                ]
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "w1")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert_eq!(
            log.lock().await.as_slice(),
            ["start w1", "end w1"],
            "the approved call runs"
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

    // ---- ADR-0013 item 6: commands over the protocol ----

    /// A resolve naming an unknown request is dropped — late answers and
    /// retries can never resurrect a settled (or never-open) gate.
    #[tokio::test]
    async fn a_resolve_for_an_unknown_request_is_dropped() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"ping\"}"),
            text_script("done"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        // A bogus resolve sits in the channel before the real request exists.
        sender
            .send(Command::ResolveApproval {
                command_id: "cmd-bogus".into(),
                request_id: "ap-bogus".into(),
                decisions: vec![Approval::Rejected {
                    comment: Some("stray".into()),
                }],
            })
            .expect("channel open");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(EchoTool)],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the stray resolve must not poison the run");

        assert_eq!(outcome.turns, 2);
        let resolves: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    EventKind::Command(Command::ResolveApproval { .. })
                )
            })
            .collect();
        assert_eq!(
            resolves.len(),
            1,
            "only the real request's resolve is recorded"
        );
        assert!(!outcome.messages[2].is_error, "the call was approved");
    }

    /// ADR-0002's idempotent-retry seam: a retried submission (same client
    /// command id) applies exactly once — one message, one command event.
    #[tokio::test]
    async fn a_retried_command_applies_exactly_once() {
        let provider = ReplayProvider::new([text_script("one"), text_script("two")])
            .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        let steer = || Command::Steer {
            command_id: "cmd-dup".into(),
            text: "one more thing".into(),
            mode: SteerMode::Queue,
        };
        sender.send(steer()).expect("first send");
        sender.send(steer()).expect("the retry");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the queued steer extends the run");

        let steered: Vec<_> = outcome
            .messages
            .iter()
            .filter(|message| {
                matches!(&message.content[0], cadmus_contract::ContentPart::Text { text } if text == "one more thing")
            })
            .collect();
        assert_eq!(steered.len(), 1, "the retry deduped on the command id");
        let steers = sink
            .events()
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::Command(Command::Steer { .. })))
            .count();
        assert_eq!(steers, 1, "one command event, not two");
    }

    /// An inject steer sent while turn 1 streams lands at the next request
    /// boundary — never in the in-flight request — recorded at application
    /// so the fold matches the live history.
    #[tokio::test]
    async fn an_injected_steer_lands_at_the_next_request_boundary() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"ping\"}"),
            text_script("pong received"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let recording = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        // React to turn 1's recorded response exactly like a watching
        // client: the steer enters the channel while the tools dispatch.
        // The auto-resolver sits behind it so the gate still resolves.
        let reactive = SendOnMatch::new(
            recording,
            sender.clone(),
            |item: &LiveItem| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event }
                        if matches!(event.kind, EventKind::LlmResponse { .. })
                )
            },
            Command::Steer {
                command_id: "cmd-steer".into(),
                text: "also say thanks".into(),
                mode: SteerMode::Inject,
            },
        );
        let resolver = crate::testing::AutoResolver::new(
            std::sync::Arc::new(reactive),
            sender,
            |calls: &[ToolCall]| calls.iter().map(|_| Approval::Approved).collect(),
        );
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(resolver),
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(EchoTool)],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("say ping", 1_024))
            .await
            .expect("run");

        // user → assistant(call) → tool(result) → user(steer) → assistant(text)
        let roles: Vec<_> = outcome
            .messages
            .iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(
            roles,
            [
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
                cadmus_contract::Role::Tool,
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
            ]
        );
        assert!(
            matches!(&outcome.messages[3].content[0], cadmus_contract::ContentPart::Text { text } if text == "also say thanks")
        );
        // The command is recorded at application — between the tool result
        // and turn 2's request — so the replayed history equals the live one.
        let events = sink.events();
        let state = crate::replay_trace(&events);
        assert_eq!(state.messages, outcome.messages);
    }

    /// A queue steer holds past mid-task boundaries and fires only at the
    /// would-be finish line: the run continues instead of ending.
    #[tokio::test]
    async fn a_queued_steer_continues_a_run_that_would_finish() {
        let provider = ReplayProvider::new([text_script("first"), text_script("second")])
            .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        sender
            .send(Command::Steer {
                command_id: "cmd-queue".into(),
                text: "one more thing".into(),
                mode: SteerMode::Queue,
            })
            .expect("send");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the queued steer extends the run");

        assert_eq!(outcome.turns, 2);
        // user → assistant("first") → user(steer) → assistant("second")
        assert!(
            matches!(&outcome.messages[1].content[0], cadmus_contract::ContentPart::Text { text } if text == "first")
        );
        assert!(
            matches!(&outcome.messages[2].content[0], cadmus_contract::ContentPart::Text { text } if text == "one more thing")
        );
        let state = crate::replay_trace(&sink.events());
        assert_eq!(state.messages, outcome.messages);
    }

    /// Mid-stream interrupt (chunk granularity): the stream is dropped, the
    /// partial turn is recorded errored, and the terminal record carries the
    /// interruption — completed work is preserved.
    #[tokio::test]
    async fn an_interrupt_mid_stream_truncates_the_turn() {
        let provider = ReplayProvider::new([ReplayProvider::script(vec![
            StreamChunk::TextDelta("par".into()),
            StreamChunk::TextDelta("tial".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ])])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let live = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        let live = std::sync::Arc::new(SendOnMatch::new(
            live,
            sender,
            |item: &LiveItem| matches!(item.kind, LiveKind::AssistantDelta { .. }),
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        ));
        let protocol = ClientProtocol {
            live,
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("the run is interrupted");
        assert!(matches!(err, AgentError::Interrupted));

        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            [
                "start_run",
                "llm_request",
                "interrupt",
                "llm_response",
                "run_finished"
            ],
            "the command lands before its consequences"
        );
        let response = &events[3];
        assert_eq!(response.status, Status::Error);
        assert_eq!(
            response.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::INTERRUPTED)
        );
        let EventKind::LlmResponse { message, .. } = &response.kind else {
            panic!("expected llm_response");
        };
        assert!(matches!(
            message.content.first(),
            Some(cadmus_contract::ContentPart::Text { text }) if text == "par"
        ));
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 0 }));
    }

    /// Interrupt during the approval wait: the gated calls never execute —
    /// no call or result events land — and the terminal record follows the
    /// interrupt command.
    #[tokio::test]
    async fn an_interrupt_during_the_approval_wait_ends_the_run() {
        let provider = ReplayProvider::new([ReplayProvider::script(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "step".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ])])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let live = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        // The interrupt arrives while the gate awaits its resolve: sent when
        // the approval request publishes.
        let live = std::sync::Arc::new(SendOnMatch::new(
            live,
            sender,
            |item: &LiveItem| matches!(item.kind, LiveKind::ApprovalRequested { .. }),
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        ));
        let protocol = ClientProtocol {
            live,
            commands: std::sync::Arc::new(commands),
        };
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(StepTool {
                name: "step",
                log: log.clone(),
            })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("interrupted at the gate");
        assert!(matches!(err, AgentError::Interrupted));
        assert!(log.lock().await.is_empty(), "the gated call never executed");
        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            [
                "start_run",
                "llm_request",
                "llm_response",
                "interrupt",
                "run_finished"
            ]
        );
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 1 }));
    }

    /// An interrupt planted during turn 1's tool dispatch lands at the next
    /// boundary: dispatches in flight settle first (their results are
    /// recorded), then the run ends.
    #[tokio::test]
    async fn an_interrupt_at_the_boundary_lets_dispatch_settle() {
        /// Sends the interrupt when invoked — a mid-run client action at a
        /// deterministic point.
        struct InterruptTool(std::sync::mpsc::Sender<Command>);

        #[async_trait]
        impl AgentTool for InterruptTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "echo".into(),
                    description: "echoes and interrupts".into(),
                    parameters: json!({"type": "object"}),
                }
            }

            async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
                let _ = self.0.send(Command::Interrupt {
                    command_id: "cmd-esc".into(),
                });
                Ok(arguments)
            }
        }

        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"x\"}"),
            text_script("never reached"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(InterruptTool(sender))],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("interrupted at the boundary");
        assert!(matches!(err, AgentError::Interrupted));
        let events = sink.events();
        // The tool result landed before the run ended.
        assert!(events.iter().any(
            |event| matches!(&event.kind, EventKind::ToolResult { call_id, .. } if call_id == "c1")
        ));
        let finished = events.last().expect("terminal record");
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 1 }));
        assert_eq!(finished.status, Status::Error);
    }

    /// A command channel with no clients left (the sender is gone) mid-gate
    /// denies the batch — unanswered is deny (ADR-0008 item 4).
    #[tokio::test]
    async fn a_closed_command_channel_denies_the_gate() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"x\"}"),
            text_script("skipped"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (commands, sender) = crate::testing::ChannelCommands::new();
        drop(sender);
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(crate::testing::RecordingLive::default()),
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(EchoTool)],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the run adapts to the denial");
        assert!(outcome.messages[2].is_error);
        assert!(matches!(
            &outcome.messages[2].content[0],
            cadmus_contract::ContentPart::Text { text } if text.contains("no reason given")
        ));
        let result = sink
            .events()
            .into_iter()
            .find(|event| matches!(event.kind, EventKind::ToolResult { .. }))
            .expect("a tool_result event");
        assert_eq!(
            result.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::APPROVAL_REJECTED)
        );
    }

    /// The finish-line race: a run that completed before the interrupt was
    /// applied finishes clean — the moot interrupt is dropped unrecorded,
    /// and a queued steer it arrived with is canceled with it (the user
    /// stopped the run; "one more thing" dies with it).
    #[tokio::test]
    async fn an_interrupt_at_the_finish_line_is_moot() {
        let provider =
            ReplayProvider::new([text_script("one")]).with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let recording = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        sender
            .send(Command::Steer {
                command_id: "cmd-queue".into(),
                text: "one more thing".into(),
                mode: SteerMode::Queue,
            })
            .expect("the queued steer");
        // The interrupt lands when turn 1's response is recorded — after
        // the stream, before the finish check: the exact finish-line window.
        let reactive = SendOnMatch::new(
            recording,
            sender,
            |item: &LiveItem| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event }
                        if matches!(event.kind, EventKind::LlmResponse { .. })
                )
            },
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        );
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(reactive),
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the run finished before the interrupt applied");

        assert_eq!(outcome.turns, 1);
        assert_eq!(
            outcome.messages.len(),
            2,
            "the queued steer was canceled with the interrupt"
        );
        let events = sink.events();
        assert!(
            events.iter().all(|event| !matches!(
                event.kind,
                EventKind::Command(Command::Steer { .. } | Command::Interrupt { .. })
            )),
            "moot commands are never recorded"
        );
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Ok);
    }

    struct TodoTool;

    #[async_trait]
    impl AgentTool for TodoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: TODO_WRITE.into(),
                description: "test todo tool".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("recorded".into()))
        }
    }

    /// A tracker that yields its files on the first call batch, then nothing.
    struct OneShotTracker(std::sync::Mutex<Option<Vec<InstructionFile>>>);

    impl InstructionTracker for OneShotTracker {
        fn on_calls(&self, calls: &[ToolCall]) -> Vec<InstructionFile> {
            if calls.is_empty() {
                return Vec::new();
            }
            self.0
                .lock()
                .expect("tracker poisoned")
                .take()
                .unwrap_or_default()
        }
    }

    /// The text parts of one message, concatenated (test assertion helper).
    fn text_of(message: &Message) -> String {
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

    /// A context bundle with a real instruction chain and a scripted git
    /// probe, for the pipeline tests.
    fn pipeline_context(
        git: Option<crate::context::GitStatus>,
        tracker: Arc<dyn InstructionTracker>,
    ) -> ContextBundle {
        ContextBundle {
            prefix: FrozenPrefix::assemble(
                "test prompt",
                &[InstructionFile {
                    path: "/repo/AGENTS.md".into(),
                    content: "project rules".into(),
                }],
                &[],
            ),
            probe: Arc::new(crate::testing::FixedProbe(git)),
            tracker,
            cwd: "/repo".into(),
            artifacts: Arc::new(crate::testing::RecordingArtifacts::default()),
            fold_policy: crate::context::FoldPolicy::default(),
        }
    }

    #[tokio::test]
    async fn requests_render_three_segments_and_start_run_records_the_prefix() {
        let provider = Arc::new(
            ReplayProvider::new([text_script("done")]).with_capabilities(test_capabilities()),
        );
        let (telemetry, sink) = test_telemetry("tr-context");
        let context = pipeline_context(
            Some(crate::context::GitStatus {
                branch: "main".into(),
                dirty_count: 3,
            }),
            Arc::new(crate::context::NoInstructions),
        );
        let agent = AgentLoop::new(
            provider.clone(),
            vec![],
            context,
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        let messages = &requests[0].messages;
        assert_eq!(messages.len(), 3, "prefix + prompt + trailer");
        assert_eq!(messages[0].role, cadmus_contract::Role::System);
        let system = text_of(&messages[0]);
        assert!(system.contains("test prompt"));
        assert!(system.contains("## /repo/AGENTS.md"));
        assert!(system.contains("project rules"));
        assert_eq!(messages[1], Message::user("hi"));
        let trailer = text_of(&messages[2]);
        assert!(trailer.contains("[cadmus status]"));
        assert!(trailer.contains("cwd: /repo"));
        assert!(trailer.contains("git: main, dirty(3)"));

        let events = sink.events();
        let EventKind::Command(Command::StartRun { prefix, .. }) = &events[0].kind else {
            panic!("first event is start_run");
        };
        let record = prefix.as_ref().expect("prefix recorded on start_run");
        assert!(record.system.contains("test prompt"));
        assert_eq!(record.instructions.len(), 1);
        assert_eq!(
            Some(record.hash.as_str()),
            events[0].attributes[attrs::PREFIX_HASH].as_str(),
            "the hash attribute matches the record"
        );
        let request_event = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::LlmRequest { .. }))
            .expect("request event");
        let EventKind::LlmRequest { trailer: recorded } = &request_event.kind else {
            unreachable!()
        };
        assert_eq!(
            recorded.as_deref(),
            Some(trailer.as_str()),
            "the request event carries the exact rendered trailer"
        );
    }

    #[tokio::test]
    async fn todo_write_folds_into_the_next_trailer() {
        let provider = Arc::new(ReplayProvider::new([
            ReplayProvider::script(vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: TODO_WRITE.into(),
                },
                StreamChunk::ToolArgsDelta {
                    index: 0,
                    fragment: "{\"items\":[{\"content\":\"write tests\",\"status\":\"in_progress\"},{\"content\":\"ship it\",\"status\":\"pending\"}]}".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::ToolCalls,
                },
            ]),
            text_script("done"),
        ]));
        let (telemetry, _sink) = test_telemetry("tr-todo");
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(TodoTool)],
            crate::testing::test_context(),
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("plan it", 1_024))
            .await
            .expect("run");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let trailer = text_of(requests[1].messages.last().expect("trailer message"));
        assert!(
            trailer.contains("tools: todo_write: 1"),
            "counter: {trailer}"
        );
        assert!(
            trailer.contains("[>] write tests"),
            "in-progress: {trailer}"
        );
        assert!(trailer.contains("[ ] ship it"), "pending: {trailer}");
    }

    #[tokio::test]
    async fn rejected_calls_stay_out_of_the_tool_counters() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(ReplayProvider::new([
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
            text_script("done"),
        ]));
        let (telemetry, _sink) = test_telemetry("tr-rejected");
        let (protocol, _live, _sender) = crate::testing::protocol_with(|calls| {
            calls
                .iter()
                .map(|_| Approval::Rejected { comment: None })
                .collect()
        });
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(StepTool { name: "step", log })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("try", 1_024))
            .await
            .expect("run");

        let requests = provider.requests();
        let trailer = text_of(requests[1].messages.last().expect("trailer message"));
        assert!(
            !trailer.contains("tools:"),
            "a rejected call never executed, so no counter line: {trailer}"
        );
    }

    #[tokio::test]
    async fn nested_instructions_land_in_history_event_and_fold() {
        let file = InstructionFile {
            path: "/repo/crates/x/AGENTS.md".into(),
            content: "crate rules\n".into(),
        };
        let provider = Arc::new(
            ReplayProvider::new([
                tool_call_script("c1", "{\"text\":\"ping\"}"),
                text_script("done"),
            ])
            .with_capabilities(test_capabilities()),
        );
        let (telemetry, sink) = test_telemetry("tr-nested");
        let context = ContextBundle {
            tracker: Arc::new(OneShotTracker(std::sync::Mutex::new(Some(vec![
                file.clone(),
            ])))),
            ..crate::testing::test_context()
        };
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(EchoTool)],
            context,
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let outcome = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let injected_text = format_injected(&file);
        // The position is load-bearing, not just the presence: the injected
        // user message must land AFTER the tool results — between the
        // assistant's tool-call turn and its results it would be an invalid
        // sequence for strict providers. Pin the whole role sequence.
        let roles: Vec<cadmus_contract::Role> = outcome
            .messages
            .iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(
            roles,
            vec![
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
                cadmus_contract::Role::Tool,
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant
            ]
        );
        assert_eq!(text_of(&outcome.messages[3]), injected_text);
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::InstructionInjected { path, content }
                if path == &file.path && content == &file.content
        )));
        // The fold invariant end to end: replaying the log reproduces the
        // live history byte for byte, injected messages included.
        let folded = crate::replay_trace(&events);
        assert_eq!(folded.messages, outcome.messages);
    }

    /// A tool returning a fixed-size string — the fold tests' bulky result.
    struct BigTool(usize);

    #[async_trait]
    impl AgentTool for BigTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "big".into(),
                description: "returns a big string".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("x".repeat(self.0)))
        }
    }

    fn big_result_script(
        id: &str,
        input_tokens: Option<u64>,
    ) -> Vec<Result<StreamChunk, ModelError>> {
        let mut chunks = vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: id.into(),
                name: "big".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
        ];
        if let Some(input) = input_tokens {
            chunks.push(StreamChunk::Usage(cadmus_contract::Usage {
                input,
                ..cadmus_contract::Usage::default()
            }));
        }
        chunks.push(StreamChunk::Done {
            finish: FinishReason::ToolCalls,
        });
        ReplayProvider::script(chunks)
    }

    #[tokio::test]
    async fn fold_directive_records_refs_and_the_render_substitutes() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000; // Δ = 1000 tokens; usage 1500 fires it
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", Some(1_500)), text_script("done")])
                .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(0),
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let outcome = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let events = sink.events();
        let (folded_refs, estimate, estimator) = events
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::Fold {
                    folded,
                    estimate,
                    estimator,
                } => Some((folded, estimate, estimator)),
                _ => None,
            })
            .expect("a fold directive at the turn-2 boundary");
        assert_eq!(*estimate, 1_500);
        assert_eq!(*estimator, EstimateSource::Provider);
        assert_eq!(folded_refs.len(), 1);
        let fold_ref = &folded_refs[0];
        assert_eq!(fold_ref.call_id, "c1");
        assert_eq!(fold_ref.original_bytes, 3_000);
        // The directive references the result's event id, and that event
        // exists in the same log (the id-reference discipline).
        assert!(events.iter().any(|event| event.id == fold_ref.event_id
            && matches!(event.kind, EventKind::ToolResult { .. })));
        // The spill keeps the full text outside the log.
        assert_eq!(
            artifacts.spills().values().next().map(String::len),
            Some(3_000)
        );

        // The render substituted the placeholder; the live history and the
        // replayed fold keep the full text (fold invariant).
        let requests = provider.requests();
        let rendered = &requests[1].messages[3];
        assert_eq!(rendered.tool_call_id.as_deref(), Some("c1"));
        let placeholder = text_of(rendered);
        assert!(placeholder.contains("[COMPRESSED"), "{placeholder}");
        assert!(placeholder.len() < 3_000);
        assert_eq!(text_of(&outcome.messages[2]).len(), 3_000);
        let folded = crate::replay_trace(&events);
        assert_eq!(folded.messages, outcome.messages);
    }

    #[tokio::test]
    async fn fold_estimator_falls_back_to_chars4_without_reported_usage() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 4_000; // Δ = 400; the ~800-token chars/4 estimate fires it
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", None), text_script("done")])
                .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold-chars4");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(0),
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let estimator = sink.events().iter().find_map(|event| match &event.kind {
            EventKind::Fold { estimator, .. } => Some(*estimator),
            _ => None,
        });
        assert_eq!(estimator, Some(EstimateSource::Chars4));
    }

    #[tokio::test]
    async fn recent_results_stay_verbatim_within_the_scope() {
        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000;
        let provider = Arc::new(
            ReplayProvider::new([
                big_result_script("c1", Some(1_500)),
                big_result_script("c2", Some(1_500)),
                text_script("done"),
            ])
            .with_capabilities(capabilities),
        );
        let (telemetry, sink) = test_telemetry("tr-fold-recent");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: test_fold_policy(1),
                ..crate::testing::test_context()
            }, // only results strictly older than one turn fold
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let events = sink.events();
        let fold_events: Vec<_> = events
            .iter()
            .filter(|event| matches!(event.kind, EventKind::Fold { .. }))
            .collect();
        assert_eq!(fold_events.len(), 1, "only the turn-3 boundary can fold");
        let EventKind::Fold { folded, .. } = &fold_events[0].kind else {
            unreachable!()
        };
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].call_id, "c1", "turn 2's result stays verbatim");

        let requests = provider.requests();
        let last = &requests[2].messages;
        assert!(text_of(&last[3]).contains("[COMPRESSED"), "c1 folded");
        assert_eq!(text_of(&last[5]).len(), 3_000, "c2 verbatim");
    }

    #[tokio::test]
    async fn a_spill_failure_is_fatal_not_silent() {
        struct FailingArtifacts;
        impl cadmus_contract::ArtifactSink for FailingArtifacts {
            fn spill(
                &self,
                name: &str,
                _content: &str,
            ) -> Result<String, cadmus_contract::LogError> {
                Err(cadmus_contract::LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("cannot write {name}"),
                )))
            }
        }

        let mut capabilities = test_capabilities();
        capabilities.max_context = 10_000;
        let provider = Arc::new(
            ReplayProvider::new([big_result_script("c1", Some(1_500))])
                .with_capabilities(capabilities),
        );
        let (telemetry, _sink) = test_telemetry("tr-fold-fail");
        let context = ContextBundle {
            artifacts: Arc::new(FailingArtifacts),
            fold_policy: test_fold_policy(0),
            ..crate::testing::test_context()
        };
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(3_000))],
            context,
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let err = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect_err("a spill failure aborts the run");
        assert!(matches!(err, AgentError::Log(_)));
    }

    /// Runs a scripted fold scenario; returns the sink's events and the
    /// provider's recorded requests.
    async fn fold_scenario(
        scripts: Vec<Vec<Result<StreamChunk, ModelError>>>,
        max_context: u32,
        policy: crate::context::FoldPolicy,
        tool_size: usize,
    ) -> (
        Vec<Event>,
        Vec<ChatRequest>,
        Arc<crate::testing::RecordingArtifacts>,
    ) {
        let mut capabilities = test_capabilities();
        capabilities.max_context = max_context;
        let provider = Arc::new(ReplayProvider::new(scripts).with_capabilities(capabilities));
        let (telemetry, sink) = test_telemetry("tr-fold-scenario");
        let artifacts = Arc::new(crate::testing::RecordingArtifacts::default());
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(BigTool(tool_size))],
            ContextBundle {
                artifacts: artifacts.clone(),
                fold_policy: policy,
                ..crate::testing::test_context()
            },
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");
        (sink.events(), provider.requests(), artifacts)
    }

    /// The early-firing test policy (small numbers so short runs fold).
    fn test_fold_policy(recent_turns: usize) -> crate::context::FoldPolicy {
        crate::context::FoldPolicy {
            recent_turns,
            min_bytes: 64,
            growth_max_tokens: 100_000,
            ceiling_percent: 80,
        }
    }

    fn fold_events(events: &[Event]) -> Vec<&[FoldedRef]> {
        events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Fold { folded, .. } => Some(folded.as_slice()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn ceiling_fires_without_new_growth_after_a_fold() {
        // Turn 1 folds on the cadence; turn 2's growth is under Δ but still
        // over the 80% ceiling — the ceiling rule, not the cadence, folds it.
        let (events, _, _) = fold_scenario(
            vec![
                big_result_script("c1", Some(8_500)),
                big_result_script("c2", Some(8_600)),
                text_script("done"),
            ],
            10_000, // Δ = 1000, ceiling = 8000
            test_fold_policy(0),
            3_000,
        )
        .await;
        let folds = fold_events(&events);
        assert_eq!(folds.len(), 2, "cadence folds t1, the ceiling folds t2");
        assert_eq!(folds[1][0].call_id, "c2");
    }

    #[tokio::test]
    async fn ceiling_with_nothing_foldable_just_continues() {
        // Over the ceiling with every result still inside the recency scope:
        // no directive, no error — the pre-existing ContextLength path owns
        // this case until the phase-2 compactor lands.
        let (events, _, _) = fold_scenario(
            vec![big_result_script("c1", Some(8_500)), text_script("done")],
            10_000,
            test_fold_policy(5),
            3_000,
        )
        .await;
        assert!(fold_events(&events).is_empty());
    }

    #[tokio::test]
    async fn the_second_fold_waits_for_delta_of_new_growth() {
        // t1 folds at 1500 (baseline ≈ 1050 post-fold); t2's 1800 is under
        // baseline+Δ, so no fold; t3's 2200 crosses it. The second directive
        // skips the already-folded c1.
        let (events, _, _) = fold_scenario(
            vec![
                big_result_script("c1", Some(1_500)),
                big_result_script("c2", Some(1_800)),
                big_result_script("c3", Some(2_200)),
                text_script("done"),
            ],
            10_000,
            test_fold_policy(0),
            3_000,
        )
        .await;
        let folds = fold_events(&events);
        assert_eq!(folds.len(), 2, "boundaries 2 and 4 fold, 3 waits");
        assert_eq!(folds[0].len(), 1);
        assert_eq!(folds[0][0].call_id, "c1");
        let second: Vec<&str> = folds[1].iter().map(|r| r.call_id.as_str()).collect();
        assert_eq!(second, vec!["c2", "c3"], "already-folded results skip");
    }

    #[tokio::test]
    async fn results_under_the_size_floor_are_never_folded() {
        // The trigger fires (over the ceiling), but a 1 KB result under the
        // 2 KB floor is not a candidate: a placeholder would not shrink it.
        let (events, _, _) = fold_scenario(
            vec![big_result_script("c1", Some(8_500)), text_script("done")],
            10_000,
            crate::context::FoldPolicy {
                min_bytes: 2_048,
                ..test_fold_policy(0)
            },
            1_000,
        )
        .await;
        assert!(fold_events(&events).is_empty());
    }

    fn kind_name(event: &Event) -> &'static str {
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
}
