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
//!   / `resolve_approval_call` and `interrupt` are asset-only: effects arrive as tool
//!   results, the truncated turn and the terminal record;
//! - `instruction_injected` appends its user message through the same pure
//!   formatter the loop used, so folded and live bytes match (ADR-0005's
//!   fold invariant);
//! - `fold` is asset-only: the log keeps the true history (full result
//!   text) forever — directives steer the *request render*, never this
//!   fold. The exact-context reconstruction the directive enables is the
//!   phase-2 reflector's render, a different consumer;
//! - `eval_score` accumulates; `run_finished` is the trace's terminal record.

use std::collections::HashSet;

use cadmus_contract::{
    Command, Event, EventKind, FinishRecord, Message, RunState, Status, ToolResultProjection, attrs,
};

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
        tool_results: Vec::new(),
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
                state.tool_results.clear();
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
            // terminal record; a fold directive steers the request render —
            // the true history here keeps the full text.
            EventKind::Command(
                Command::ResolveApproval { .. }
                | Command::ResolveApprovalCall { .. }
                | Command::Interrupt { .. },
            )
            | EventKind::LlmRequest { .. }
            | EventKind::Fold { .. } => {}
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
                if let Some(request_id) = string_attr(event, attrs::APPROVAL_REQUEST_ID)
                    && let Some(call_index) = event
                        .attributes
                        .get(attrs::APPROVAL_CALL_INDEX)
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|index| usize::try_from(index).ok())
                {
                    state.tool_results.push(ToolResultProjection {
                        message_index: state.messages.len(),
                        request_id,
                        call_index,
                    });
                }
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

/// The index in [`RunState::messages`] of the message the latest
/// `llm_response` appended, with that response's turn — the anchor an
/// approval request is placed by (`PendingApproval::message_index`).
///
/// A walk over the log rather than a counter kept beside the fold: the fold
/// above is the one statement of which events append messages, and a
/// consumer mirroring it incrementally drifts silently the first time an arm
/// lands on only one side. The walk repeats the fold's dedup and
/// first-`start_run` rules, so the two are read together —
/// `the_response_anchor_matches_the_fold_for_a_rich_stream` pins the
/// agreement.
#[must_use]
pub fn latest_response_anchor(events: &[Event]) -> Option<(u32, usize)> {
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut started = false;
    let mut count = 0usize;
    let mut latest = None;
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
                count = base.messages.len();
                latest = None;
            }
            EventKind::LlmResponse { .. } => {
                latest = event.turn().map(|turn| (turn, count));
                count += 1;
            }
            EventKind::Command(Command::Steer { .. })
            | EventKind::InstructionInjected { .. }
            | EventKind::ToolResult { .. } => count += 1,
            _ => {}
        }
    }
    latest
}

fn string_attr(event: &Event, key: &str) -> Option<String> {
    event.attributes.get(key)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result_event() -> Event {
        Event::new(
            1,
            "result".into(),
            "trace".into(),
            "span".into(),
            None,
            0,
            EventKind::ToolResult {
                call_id: "duplicate".into(),
                result: json!("B completed"),
            },
        )
    }

    #[test]
    fn approval_projection_requires_both_typed_attributes_and_preserves_legacy_messages() {
        let legacy = result_event();
        let expected = replay_trace(std::slice::from_ref(&legacy));
        assert!(expected.tool_results.is_empty());
        let attributed = legacy
            .clone()
            .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1")
            .with_attribute(attrs::APPROVAL_CALL_INDEX, 1);
        let replay = replay_trace(&[attributed.clone(), attributed]);
        assert_eq!(replay.messages, expected.messages);
        assert_eq!(
            replay.tool_results,
            [ToolResultProjection {
                message_index: 0,
                request_id: "ap1".into(),
                call_index: 1,
            }]
        );
        for event in [
            legacy
                .clone()
                .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1"),
            legacy.clone().with_attribute(attrs::APPROVAL_CALL_INDEX, 1),
            legacy
                .clone()
                .with_attribute(attrs::APPROVAL_REQUEST_ID, 42)
                .with_attribute(attrs::APPROVAL_CALL_INDEX, 1),
            legacy
                .clone()
                .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1")
                .with_attribute(attrs::APPROVAL_CALL_INDEX, "1"),
            legacy
                .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1")
                .with_attribute(attrs::APPROVAL_CALL_INDEX, -1),
        ] {
            assert_eq!(replay_trace(&[event]), expected);
        }
    }

    #[test]
    fn approval_projection_indices_include_seeded_history_and_reset_with_the_first_start() {
        let result = result_event()
            .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1")
            .with_attribute(attrs::APPROVAL_CALL_INDEX, 1);
        let start = Event::new(
            2,
            "start".into(),
            "trace".into(),
            "root".into(),
            None,
            0,
            EventKind::Command(Command::StartRun {
                base: Box::new(cadmus_contract::ChatRequest::user_text("seed", 1_024)),
                prefix: None,
            }),
        );
        let replay = replay_trace(&[start.clone(), result.clone()]);
        assert_eq!(replay.tool_results[0].message_index, 1);
        assert_eq!(replay.messages[0], Message::user("seed"));
        // A tolerated writer anomaly replaces pre-start messages, so their
        // result addresses must not survive pointing into the new seed.
        let reset = replay_trace(&[result, start]);
        assert_eq!(reset.messages, [Message::user("seed")]);
        assert!(reset.tool_results.is_empty());
    }

    /// The transport's approval anchor must be the fold's own answer, not a
    /// parallel count: a seeded assistant message and a later steer must not
    /// claim it, and a stream without a response has no anchor at all.
    #[test]
    fn the_response_anchor_matches_the_fold_for_a_rich_stream() {
        use cadmus_contract::{ChatRequest, FinishReason, Role, SteerMode, TurnOutcome};

        let mut base = ChatRequest::user_text("seed", 1_024);
        base.messages
            .push(Message::text(Role::Assistant, "seeded proposal"));
        let start = Event::new(
            1,
            "start".into(),
            "trace".into(),
            "root".into(),
            None,
            0,
            EventKind::Command(Command::StartRun {
                base: Box::new(base),
                prefix: None,
            }),
        );
        let response = |seq, id: &str, turn, text: &str| {
            Event::new(
                seq,
                id.into(),
                "trace".into(),
                format!("s{seq}"),
                None,
                0,
                EventKind::LlmResponse {
                    message: Message::text(Role::Assistant, text),
                    usage: None,
                    finish: FinishReason::Stop,
                    outcome: TurnOutcome::Content,
                    warnings: Vec::new(),
                },
            )
            .with_attribute(attrs::TURN, turn)
        };
        let events = vec![
            start.clone(),
            Event::new(
                2,
                "steer".into(),
                "trace".into(),
                "s2".into(),
                None,
                0,
                EventKind::Command(Command::Steer {
                    command_id: "c1".into(),
                    text: "again".into(),
                    mode: SteerMode::Inject,
                }),
            ),
            response(3, "r1", 1, "first"),
            Event::new(
                4,
                "result".into(),
                "trace".into(),
                "s4".into(),
                None,
                0,
                EventKind::ToolResult {
                    call_id: "call".into(),
                    result: json!("ok"),
                },
            ),
            // A duplicate id appends nothing, so the count must not advance.
            response(3, "r1", 1, "first"),
            response(5, "r2", 2, "second"),
        ];
        let folded = replay_trace(&events);
        let (turn, index) = latest_response_anchor(&events).expect("the last response anchors");
        assert_eq!(turn, 2);
        assert_eq!(
            folded.messages[index],
            Message::text(Role::Assistant, "second")
        );
        // Seeded history is not a response, so it never anchors a request.
        assert_eq!(latest_response_anchor(&[start]), None);
    }
}
