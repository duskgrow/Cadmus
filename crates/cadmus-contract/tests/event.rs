//! Wire-shape locks for the trajectory event schema (ADR-0005): serde
//! round-trips for every kind, additive-evolution tolerance (unknown fields
//! ignored, missing optional fields defaulted, a flat one-line shape), and
//! one canonical snapshot of a fully populated event.

use cadmus_contract::{
    Approval, ChatRequest, Command, EstimateSource, Event, EventError, EventKind, FinishReason,
    FoldedRef, Message, Role, ScoreEvent, Status, SteerMode, ToolCall, TurnOutcome, Usage, attrs,
};
use serde_json::json;

fn sample_event() -> Event {
    Event::new(
        3,
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
        EventKind::LlmRequest { trailer: None },
        EventKind::LlmRequest {
            trailer: Some("[cadmus status]\ncwd: /repo\n".into()),
        },
        EventKind::InstructionInjected {
            path: "/repo/crates/x/AGENTS.md".into(),
            content: "crate rules".into(),
        },
        EventKind::Fold {
            folded: vec![FoldedRef {
                event_id: "e9".into(),
                call_id: "c4".into(),
                spill: "2026/09/10/tr-x.artifacts/m3.txt".into(),
                original_bytes: 45_210,
            }],
            estimate: 102_400,
            estimator: EstimateSource::Chars4,
        },
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
            prefix: None,
        }),
        EventKind::Command(Command::StartRun {
            base: Box::new(ChatRequest::user_text("fix the typo", 4_096)),
            prefix: Some(cadmus_contract::PrefixRecord {
                hash: "4fe2a901c5e0b3a8".into(),
                system: "You are Cadmus.".into(),
                instructions: vec![cadmus_contract::InstructionFile {
                    path: "/repo/AGENTS.md".into(),
                    content: "project rules".into(),
                }],
                skills: vec![cadmus_contract::SkillSummary {
                    name: "pr-preflight".into(),
                    description: "review a PR before opening it".into(),
                }],
            }),
        }),
        EventKind::Command(Command::ResolveApproval {
            command_id: "cmd-1".into(),
            request_id: "ap7".into(),
            decisions: vec![
                Approval::Approved,
                Approval::Rejected {
                    comment: Some("not this one".into()),
                },
            ],
        }),
        EventKind::Command(Command::Steer {
            command_id: "cmd-2".into(),
            text: "also check the tests".into(),
            mode: SteerMode::Inject,
        }),
        EventKind::Command(Command::Interrupt {
            command_id: "cmd-3".into(),
        }),
        EventKind::RunFinished { turns: 2 },
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let seq = u64::try_from(index).expect("small index");
        let event = Event::new(
            seq,
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
        9,
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
/// documented defaults (status ok, no error, empty attributes). A
/// pre-protocol line carries no `seq` and defaults to position zero.
#[test]
fn unknown_and_missing_fields_are_tolerated() {
    let line = r#"{"id":"e1","trace_id":"tr","span_id":"s1","time_unix_ms":1,"kind":"run_finished","turns":1,"future_field":42}"#;
    let event: Event = serde_json::from_str(line).expect("tolerant parse");
    assert_eq!(event.seq, 0, "a pre-protocol line defaults to seq zero");
    assert_eq!(event.status, Status::Ok);
    assert_eq!(event.error, None);
    assert!(event.attributes.is_empty());
    assert_eq!(event.kind, EventKind::RunFinished { turns: 1 });
}
