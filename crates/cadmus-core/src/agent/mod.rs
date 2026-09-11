//! The agent loop, split along its invariant seams: `tool` holds the tool
//! contract, `events` the event plumbing, `inbox` client-command intake,
//! `gate` the approval batch, `dispatch` tool execution, `fold` the context
//! fold machinery, `trailer` the ADR-0007 item-1 state. This module keeps
//! the public types, the loop's state definition and the run spine.

mod dispatch;
mod events;
mod fold;
mod gate;
mod inbox;
mod tool;
mod trailer;

#[cfg(test)]
mod fixtures;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use cadmus_contract::{
    ArtifactSink, ChatRequest, Clock, Command, CommandSource, EventError, EventKind, EventSink,
    FinishReason, IdSequence, LiveKind, LiveSink, Message, ModelError, Provider, TodoItem,
    ToolSpec, TurnOutcome, attrs, error_kinds,
};
use serde_json::Value;
use tokio_stream::StreamExt;

pub use tool::{AgentTool, Concurrency, Effect, ToolError};

use events::{interrupted_detail, model_event_error, response_kind};
use fold::ResultTrack;
use inbox::Inbox;

use crate::context::{FoldPolicy, FrozenPrefix, InstructionTracker, StatusProbe};
use crate::{AssembledTurn, MessageAssembler};

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
        // The trailer's elapsed anchors to the recorded run start — one
        // fact, one home (the StartRun event's own timestamp).
        let run_start_ms = start_run.time_unix_ms;

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
            let trailer = self.render_trailer(run_start_ms);
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cadmus_contract::testing::{ContractSubject, QueuedResponse};
    use cadmus_contract::{InstructionFile, Status, StreamChunk};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{
        EchoTool, kind_name, test_capabilities, test_loop, text_of, text_script, tool_call_script,
    };
    use crate::testing::test_telemetry;

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
}
