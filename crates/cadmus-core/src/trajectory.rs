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
//!   self-sufficient. Its prefix record is *request-render* material, not
//!   conversational history, so it never enters `messages` (the exact
//!   rendered request is reconstructible from record + fold + per-request
//!   trailer — phase 2's reflector is that consumer);
//! - `llm_request` is asset-only (full text for audit/training), no state;
//! - `llm_response` appends the assistant message; only an `ok` response
//!   counts as a completed turn. An errored response still appends its
//!   partial message, but its structured error stays asset-only for now —
//!   the deferred resume work is its consumer;
//! - `tool_call` opens a span, `tool_result` closes it and appends the tool
//!   message; a result without a matching open call still appends (tolerant
//!   reader), and calls never closed surface as
//!   [`RunState::dangling_tool_calls`];
//! - commands fold their state effects only: a `steer` appends its user
//!   message exactly where the loop applied it (commands are recorded at
//!   application, so the fold matches the live history); `resolve_approval`
//!   and `interrupt` are asset-only — their effects already arrive as tool
//!   results, the truncated turn and the terminal record;
//! - `instruction_injected` appends its user message through the same pure
//!   formatter the loop used, so folded and live bytes match (ADR-0005's
//!   fold invariant);
//! - `eval_score` accumulates; `run_finished` is the trace's terminal record.

use std::collections::HashSet;

use cadmus_contract::{Command, Event, EventKind, FinishRecord, Message, RunState, Status, attrs};

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
            EventKind::Command(Command::StartRun { base, .. }) => {
                if started {
                    continue;
                }
                started = true;
                state.messages.clone_from(&base.messages);
                state.provider = string_attr(event, attrs::PROVIDER);
                state.model = string_attr(event, attrs::MODEL);
            }
            EventKind::Command(Command::Steer { text, .. }) => {
                // Applied commands only (the loop records at application):
                // the user message lands exactly where the live history has
                // it.
                state.messages.push(Message::user(text.clone()));
            }
            // Asset-only arms: a request's history is rebuilt, never
            // snapshotted (its trailer rides it for audit, but the trailer
            // is render output, not history); a resolve's effects arrive as
            // tool results, an interrupt's as the truncated turn and
            // terminal record.
            EventKind::Command(Command::ResolveApproval { .. } | Command::Interrupt { .. })
            | EventKind::LlmRequest { .. } => {}
            EventKind::InstructionInjected { path, content } => {
                let file = cadmus_contract::InstructionFile {
                    path: path.clone(),
                    content: content.clone(),
                };
                state
                    .messages
                    .push(Message::user(crate::context::format_injected(&file)));
            }
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
                // The loop's own predicate: an errored event folds to an
                // is_error-marked message, so replayed state matches live
                // state (ADR-0005's fold invariant). Pre-flag logs fold
                // correctly too — their errored results carried the error
                // only on the envelope.
                let message = if event.error.is_some() {
                    Message::tool_error(call_id.clone(), result.clone())
                } else {
                    Message::tool_result(call_id.clone(), result.clone())
                };
                state.messages.push(message);
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
