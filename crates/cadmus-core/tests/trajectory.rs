//! Replayer acceptance (ADR-0005 §4): the same log folds to an identical run
//! state — asserted by folding twice — snapshot-locked, and tolerant of
//! crash-truncated logs (dangling spans, missing terminal record).

use cadmus_contract::{
    ChatRequest, Command, ContentPart, Event, EventError, EventKind, FinishReason, Message, Role,
    ScoreEvent, Status, ToolCall, TurnOutcome, Usage, attrs,
};
use cadmus_core::replay_trace;
use serde_json::json;

/// Fixed envelope: deterministic ids, spans and timeline — replay tests are
/// data tests, nothing here may read a clock.
fn envelope(id: u32, span: u32, kind: EventKind) -> Event {
    Event::new(
        format!("e{id}"),
        "tr_test".into(),
        format!("s{span}"),
        (span > 1).then(|| "s1".to_string()),
        1_757_200_000_000 + u64::from(id),
        kind,
    )
}

fn assistant_call(id: &str, name: &str, args: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentPart::ToolCall {
            call: ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: serde_json::from_str(args).expect("valid args"),
            },
        }],
        tool_call_id: None,
        opaque: None,
    }
}

/// A complete two-turn run: question → tool call → result → answer → scored
/// → finished. Span `s1` is the run root; turns and tool spans hang below it.
fn full_log() -> Vec<Event> {
    let base = ChatRequest::user_text("look up TODOs in main.rs", 4_096);
    vec![
        envelope(
            1,
            1,
            EventKind::Command(Command::StartRun {
                base: Box::new(base.clone()),
            }),
        )
        .with_attribute(attrs::PROVIDER, "kimi")
        .with_attribute(attrs::MODEL, "kimi-k3"),
        envelope(
            2,
            2,
            EventKind::LlmRequest {
                request: Box::new(base),
            },
        )
        .with_attribute(attrs::TURN, 1),
        envelope(
            3,
            2,
            EventKind::LlmResponse {
                message: assistant_call("call_1", "read_file", "{\"path\":\"src/main.rs\"}"),
                usage: Some(Usage {
                    input: 900,
                    output: 30,
                    ..Usage::default()
                }),
                finish: FinishReason::ToolCalls,
                outcome: TurnOutcome::Content,
                warnings: vec![],
            },
        )
        .with_attribute(attrs::TURN, 1),
        envelope(
            4,
            3,
            EventKind::ToolCall {
                call: ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "src/main.rs"}),
                },
            },
        ),
        envelope(
            5,
            3,
            EventKind::ToolResult {
                call_id: "call_1".into(),
                result: json!("fn main() {\n    // TODO\n}"),
            },
        ),
        envelope(
            6,
            4,
            EventKind::LlmRequest {
                request: Box::new(ChatRequest::user_text("…second turn…", 4_096)),
            },
        )
        .with_attribute(attrs::TURN, 2),
        envelope(
            7,
            4,
            EventKind::LlmResponse {
                message: Message::text(Role::Assistant, "There is one TODO at line 2."),
                usage: Some(Usage {
                    input: 1_100,
                    output: 12,
                    ..Usage::default()
                }),
                finish: FinishReason::Stop,
                outcome: TurnOutcome::Content,
                warnings: vec![],
            },
        )
        .with_attribute(attrs::TURN, 2),
        envelope(
            8,
            1,
            EventKind::EvalScore(ScoreEvent {
                case_id: "find-todo".into(),
                metric: "answer_contains_line".into(),
                score: 1.0,
                passed: Some(true),
            }),
        ),
        envelope(9, 1, EventKind::RunFinished { turns: 2 }),
    ]
}

#[test]
fn same_log_folds_to_identical_state_twice() {
    let log = full_log();
    let first = replay_trace(&log);
    let second = replay_trace(&log);
    assert_eq!(first, second, "replay must be deterministic");

    assert_eq!(first.trace_id, "tr_test");
    assert_eq!(first.provider.as_deref(), Some("kimi"));
    assert_eq!(first.model.as_deref(), Some("kimi-k3"));
    assert_eq!(first.turns, 2);
    // user → assistant(call) → tool(result) → assistant(answer)
    assert_eq!(first.messages.len(), 4);
    assert_eq!(first.messages[2].tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(first.scores.len(), 1);
    assert!(first.dangling_tool_calls.is_empty());
    let finished = first.finished.as_ref().expect("terminal record");
    assert_eq!(finished.turns, 2);
    assert_eq!(finished.status, Status::Ok);

    insta::assert_debug_snapshot!(first);
}

/// The log cut right after a tool call (the crash window): the run state
/// ends before the tool message, the call dangles, nothing fails.
#[test]
fn crash_truncated_log_leaves_a_dangling_call() {
    let log = &full_log()[..4];
    let state = replay_trace(log);
    assert_eq!(state.turns, 1);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.dangling_tool_calls, vec!["call_1"]);
    assert!(state.finished.is_none());
}

/// Tolerant reader: a result whose call was never seen (an older crash cut
/// the log between them) still lands in the history.
#[test]
fn orphan_tool_result_still_appends() {
    let log = [
        full_log().into_iter().next().expect("start_run"),
        envelope(
            2,
            3,
            EventKind::ToolResult {
                call_id: "call_lost".into(),
                result: json!("partial output"),
            },
        ),
    ];
    let state = replay_trace(&log);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[1].tool_call_id.as_deref(), Some("call_lost"));
    assert!(state.dangling_tool_calls.is_empty());
}

/// An errored llm response (stream died mid-flight) appends its partial
/// message but does not count as a completed turn.
#[test]
fn errored_response_appends_but_does_not_count() {
    let mut log = full_log();
    log[2] = envelope(
        3,
        2,
        EventKind::LlmResponse {
            message: Message::text(Role::Assistant, "partial"),
            usage: None,
            finish: FinishReason::Other("reset".into()),
            outcome: TurnOutcome::Truncated,
            warnings: vec!["stream reset after 7 chunks".into()],
        },
    )
    .errored(EventError {
        kind: "network".into(),
        message: "connection reset".into(),
    });
    // The rest of the log is irrelevant to this assertion; cut it off.
    log.truncate(3);

    let state = replay_trace(&log);
    assert_eq!(state.turns, 0, "errored turns are not completed turns");
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.warnings, vec!["stream reset after 7 chunks"]);
}

/// A retried append carries the same event id (ADR-0002's idempotent-retry
/// seam): the duplicate folds once, not twice.
#[test]
fn duplicate_event_ids_fold_once() {
    let mut log = full_log();
    // Simulate a writer retry: the tool_result line appended twice.
    log.insert(5, log[4].clone());
    let state = replay_trace(&log);
    assert_eq!(
        state.messages.len(),
        4,
        "the retry must not duplicate the tool message"
    );
    assert!(state.dangling_tool_calls.is_empty());
}

/// A second, different `start_run` in one trace is a writer anomaly: the
/// first seed wins, the later one is ignored rather than resetting history.
#[test]
fn a_second_start_run_is_ignored() {
    let mut log = full_log();
    log.push(
        envelope(
            10,
            1,
            EventKind::Command(Command::StartRun {
                base: Box::new(ChatRequest::user_text("a different run entirely", 1_024)),
            }),
        )
        .with_attribute(attrs::PROVIDER, "deepseek"),
    );
    let state = replay_trace(&log);
    assert_eq!(state.messages.len(), 4, "history must stay the first run's");
    assert_eq!(state.provider.as_deref(), Some("kimi"));
}

/// The errored terminal record is the designed distinction between a clean
/// abort and a crash window: status and structured error must survive the
/// fold.
#[test]
fn errored_run_finished_records_the_failure() {
    let mut log = full_log();
    *log.last_mut().expect("terminal record") =
        envelope(9, 1, EventKind::RunFinished { turns: 16 }).errored(EventError {
            kind: "turn_limit".into(),
            message: "assistant turn limit (16) exceeded".into(),
        });
    let state = replay_trace(&log);
    let finished = state.finished.as_ref().expect("terminal record");
    assert_eq!(finished.status, Status::Error);
    assert_eq!(
        finished.error.as_ref().map(|error| error.kind.as_str()),
        Some("turn_limit")
    );
}

#[test]
fn empty_log_is_an_empty_state() {
    let state = replay_trace(&[]);
    assert_eq!(state.trace_id, "");
    assert!(state.messages.is_empty());
    assert_eq!(state.turns, 0);
    assert!(state.finished.is_none());
}
