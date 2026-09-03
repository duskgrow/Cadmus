//! The deterministic replayer (ADR-0005 §4): folds one trace's events back
//! into the run state. Pure and total — no clock, no IO, no panics on
//! truncated logs: a trace may end mid-anything (the crash window), and the
//! tolerance rules below keep the fold honest instead of failing. The same
//! log always folds to the same [`RunState`]; CI snapshot-locks that.
//!
//! Fold rules:
//!
//! - events are deduped by `id`, first occurrence wins — a retried append of
//!   the same command/event (ADR-0002's idempotent-retry seam) folds once;
//! - the first `start_run` seeds the message history and run metadata (a
//!   later one is a writer anomaly and is ignored) — a trace is
//!   self-sufficient;
//! - `llm_request` is asset-only (full text for audit/training), no state;
//! - `llm_response` appends the assistant message; only an `ok` response
//!   counts as a completed turn. An errored response still appends its
//!   partial message, but its structured error stays asset-only for now —
//!   the deferred resume work is its consumer;
//! - `tool_call` opens a span, `tool_result` closes it and appends the tool
//!   message; a result without a matching open call still appends (tolerant
//!   reader), and calls never closed surface as
//!   [`RunState::dangling_tool_calls`];
//! - `eval_score` accumulates; `run_finished` is the trace's terminal record.

use std::collections::HashSet;

use cadmus_contract::{Command, Event, EventError, EventKind, Message, ScoreEvent, Status, attrs};

/// The folded state of one trace.
#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    pub trace_id: String,
    /// `selfevol.provider` / `selfevol.model` from the start-run command,
    /// when recorded.
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The reconstructed history: the start-run seed plus every assistant
    /// and tool message, in log order.
    pub messages: Vec<Message>,
    /// Completed assistant turns (ok `llm_response` events).
    pub turns: u32,
    /// Every turn warning, in log order.
    pub warnings: Vec<String>,
    pub scores: Vec<ScoreEvent>,
    /// Tool calls never closed by a result — the crash window's dangling
    /// spans, in the order they opened.
    pub dangling_tool_calls: Vec<String>,
    /// The terminal record; `None` means the trace ended mid-run.
    pub finished: Option<FinishRecord>,
}

/// How the run ended, from its `run_finished` event's envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct FinishRecord {
    pub turns: u32,
    pub status: Status,
    pub error: Option<EventError>,
}

/// Folds one trace's events into its [`RunState`]. Input order is log order
/// (the append-only writer guarantees it); mixed traces are the caller's
/// problem — filter by `trace_id` before folding.
#[must_use]
pub fn replay_trace(events: &[Event]) -> RunState {
    let mut state = RunState {
        trace_id: events
            .first()
            .map_or_else(String::new, |event| event.trace_id.clone()),
        provider: None,
        model: None,
        messages: Vec::new(),
        turns: 0,
        warnings: Vec::new(),
        scores: Vec::new(),
        dangling_tool_calls: Vec::new(),
        finished: None,
    };
    let mut open_calls: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut started = false;
    for event in events {
        if !seen_ids.insert(event.id.as_str()) {
            continue;
        }
        match &event.kind {
            EventKind::Command(Command::StartRun { base }) => {
                if started {
                    continue;
                }
                started = true;
                state.messages.clone_from(&base.messages);
                state.provider = string_attr(event, attrs::PROVIDER);
                state.model = string_attr(event, attrs::MODEL);
            }
            EventKind::LlmRequest => {}
            EventKind::LlmResponse {
                message, warnings, ..
            } => {
                state.messages.push(message.clone());
                state.warnings.extend(warnings.iter().cloned());
                if event.status == Status::Ok {
                    state.turns += 1;
                }
            }
            EventKind::ToolCall { call } => open_calls.push(call.id.clone()),
            EventKind::ToolResult { call_id, result } => {
                if let Some(position) = open_calls.iter().position(|open| open == call_id) {
                    open_calls.remove(position);
                }
                state
                    .messages
                    .push(Message::tool_result(call_id.clone(), result.clone()));
            }
            EventKind::EvalScore(score) => state.scores.push(score.clone()),
            EventKind::RunFinished { turns } => {
                state.finished = Some(FinishRecord {
                    turns: *turns,
                    status: event.status,
                    error: event.error.clone(),
                });
            }
        }
    }
    state.dangling_tool_calls = open_calls;
    state
}

fn string_attr(event: &Event, key: &str) -> Option<String> {
    event.attributes.get(key)?.as_str().map(str::to_owned)
}
