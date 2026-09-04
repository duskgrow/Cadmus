//! Wire-shape locks for the trajectory event schema (ADR-0005): serde
//! round-trips for every kind, additive-evolution tolerance (unknown fields
//! ignored, missing optional fields defaulted, a flat one-line shape), and
//! one canonical snapshot of a fully populated event.

use cadmus_contract::{
    ChatRequest, Command, Event, EventError, EventKind, FinishReason, Message, Role, ScoreEvent,
    Status, ToolCall, TurnOutcome, Usage, attrs,
};
use serde_json::json;

fn sample_event() -> Event {
    Event::new(
        "e3".into(),
        "tr_01JZKX9A".into(),
        "s2".into(),
        Some("s1".into()),
        1_757_200_000_042,
        EventKind::LlmResponse {
            message: Message::text(Role::Assistant, "There is one TODO at line 42."),
            usage: Some(Usage {
                input: 1_280,
                cache_read: 512,
                output: 42,
                ..Usage::default()
            }),
            finish: FinishReason::Stop,
            outcome: TurnOutcome::Content,
            warnings: vec!["tool call call_9 has malformed arguments; call quarantined".into()],
        },
    )
    .with_attribute(attrs::PROVIDER, "kimi")
    .with_attribute(attrs::MODEL, "kimi-k3")
    .with_attribute(attrs::TURN, 1)
}

/// The canonical shape of one JSONL line: envelope first, `kind` tag with
/// the payload flattened into the same object, `selfevol.*` attributes last.
#[test]
fn canonical_event_shape_is_locked() {
    insta::assert_json_snapshot!(sample_event());
}

/// A JSONL line must be exactly one line — pretty printing or embedded
/// newlines would break the append-only format.
#[test]
fn serializes_to_a_single_line() {
    let line = serde_json::to_string(&sample_event()).expect("serialize");
    assert!(!line.contains('\n'), "event line must not contain newlines");
    let back: Event = serde_json::from_str(&line).expect("round-trip");
    assert_eq!(back, sample_event());
}

/// Every kind round-trips through the line format unchanged.
#[test]
fn every_kind_round_trips() {
    let kinds = vec![
        EventKind::LlmRequest,
        sample_event().kind,
        EventKind::ToolCall {
            call: ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "src/main.rs"}),
            },
        },
        EventKind::ToolResult {
            call_id: "call_1".into(),
            result: json!("fn main() {}"),
        },
        EventKind::EvalScore(ScoreEvent {
            case_id: "fix-typo".into(),
            metric: "tests_pass".into(),
            score: 1.0,
            passed: Some(true),
        }),
        EventKind::Command(Command::StartRun {
            base: Box::new(ChatRequest::user_text("fix the typo", 4_096)),
        }),
        EventKind::RunFinished { turns: 2 },
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let event = Event::new(
            format!("e{index}"),
            "tr_round".into(),
            "s1".into(),
            None,
            1_757_200_000_000,
            kind,
        );
        let line = serde_json::to_string(&event).expect("serialize");
        let back: Event = serde_json::from_str(&line).expect("deserialize");
        assert_eq!(back, event, "kind at index {index} must round-trip");
    }
}

/// An errored event keeps its structured detail across the round-trip.
#[test]
fn errored_event_round_trips() {
    let event = Event::new(
        "e9".into(),
        "tr_fail".into(),
        "s1".into(),
        None,
        1_757_200_000_000,
        EventKind::RunFinished { turns: 16 },
    )
    .errored(EventError {
        kind: "turn_limit".into(),
        message: "assistant turn limit (16) exceeded".into(),
    });
    let line = serde_json::to_string(&event).expect("serialize");
    let back: Event = serde_json::from_str(&line).expect("deserialize");
    assert_eq!(back, event);
    assert_eq!(back.status, Status::Error);
}

/// Additive evolution tolerance: a line from a newer writer with unknown
/// envelope fields, and with optional fields absent, still parses — with
/// documented defaults (status ok, no error, empty attributes).
#[test]
fn unknown_and_missing_fields_are_tolerated() {
    let line = r#"{"id":"e1","trace_id":"tr","span_id":"s1","time_unix_ms":1,"kind":"run_finished","turns":1,"future_field":42}"#;
    let event: Event = serde_json::from_str(line).expect("tolerant parse");
    assert_eq!(event.status, Status::Ok);
    assert_eq!(event.error, None);
    assert!(event.attributes.is_empty());
    assert_eq!(event.kind, EventKind::RunFinished { turns: 1 });
}
