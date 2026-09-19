//! Additive approval protocol evolution: old batches retain their meaning,
//! per-call commands cannot be mistaken for a batch, and old syncs still read.

use cadmus_contract::{
    Approval, Command, Event, EventKind, InFlight, LiveItem, LiveKind, Message, PendingApproval,
    RunState, SettledApproval, ToolCompletion, ToolResultProjection, attrs,
};
use serde_json::json;

#[test]
fn batch_and_per_call_commands_have_distinct_wire_shapes() {
    let batch = Command::ResolveApproval {
        command_id: "batch".into(),
        request_id: "ap1".into(),
        decisions: vec![Approval::Approved],
    };
    assert_eq!(
        serde_json::to_value(&batch).expect("serialize batch"),
        json!({
            "command": "resolve_approval", "command_id": "batch",
            "request_id": "ap1", "decisions": [{"decision": "approved"}]
        })
    );
    for decision in [
        Approval::Approved,
        Approval::Rejected {
            comment: Some("leave it".into()),
        },
    ] {
        let command = Command::ResolveApprovalCall {
            command_id: "one".into(),
            request_id: "ap1".into(),
            call_index: 2,
            decision,
        };
        let value = serde_json::to_value(&command).expect("serialize call");
        assert_eq!(value["command"], "resolve_approval_call");
        assert_eq!(value["call_index"], 2);
        assert!(value.get("decisions").is_none());
        assert_eq!(command.command_id(), Some("one"));
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), command);
    }
}

#[test]
fn malformed_call_addresses_and_unknown_commands_fail_loudly() {
    for index in [json!(-1), json!(1.5), json!("2"), json!(null)] {
        assert!(
            serde_json::from_value::<Command>(json!({
                "command": "resolve_approval_call", "command_id": "one",
                "request_id": "ap1", "call_index": index,
                "decision": {"decision": "approved"}
            }))
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<Command>(json!({
            "command": "future_resolution", "command_id": "one"
        }))
        .is_err()
    );
}

#[test]
fn older_attach_records_default_new_fields_without_fabricating_decisions() {
    let pending: PendingApproval = serde_json::from_value(json!({
        "request_id": "ap1", "turn": 1, "calls": [],
        "wait_timeout": {"secs": 300, "nanos": 0}
    }))
    .expect("old pending request");
    assert!(pending.decisions.is_empty());
    assert_eq!(pending.message_index, None);
    let settled: SettledApproval = serde_json::from_value(json!({
        "request_id": "ap1", "calls": [], "decisions": []
    }))
    .expect("old settlement");
    assert_eq!(settled.message_index, None);
    let in_flight: InFlight = serde_json::from_value(json!({})).expect("old flight");
    assert!(in_flight.completed_tools.is_empty());
    assert_eq!(serde_json::to_value(in_flight).unwrap(), json!({}));
}

#[test]
fn tool_result_projection_defaults_empty_and_round_trips_without_changing_messages() {
    let legacy = json!({
        "trace_id": "trace", "messages": [], "turns": 0
    });
    let mut state: RunState = serde_json::from_value(legacy.clone()).expect("old history");
    assert!(state.tool_results.is_empty());
    // The empty projection stays off the wire, which is the compatibility
    // surface. `RunState` derives `Debug`, so the debug form prints it — a
    // hand-written impl (kept only to freeze one legacy snapshot) would have
    // silently hidden this field and every future one.
    assert_eq!(serde_json::to_value(&state).unwrap(), legacy);

    state
        .messages
        .push(Message::tool_result("duplicate", json!("B finished")));
    let messages = serde_json::to_value(&state.messages).unwrap();
    state.tool_results.push(ToolResultProjection {
        message_index: 0,
        request_id: "ap1".into(),
        call_index: 1,
    });
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["messages"], messages);
    assert_eq!(
        value["tool_results"],
        json!([{
            "message_index": 0, "request_id": "ap1", "call_index": 1
        }])
    );
    assert_eq!(serde_json::from_value::<RunState>(value).unwrap(), state);
}

#[test]
fn tool_result_approval_attributes_leave_the_legacy_event_shape_intact() {
    let legacy = json!({
        "id": "e1", "seq": 1, "trace_id": "trace", "span_id": "s1",
        "time_unix_ms": 0, "kind": "tool_result", "status": "ok",
        "call_id": "duplicate", "result": "B finished"
    });
    let event: Event = serde_json::from_value(legacy.clone()).expect("old tool result");
    assert!(event.attributes.is_empty());
    assert_eq!(serde_json::to_value(&event).unwrap(), legacy);
    let kind = event.kind.clone();
    assert!(matches!(kind, EventKind::ToolResult { .. }));
    let attributed = event
        .with_attribute(attrs::APPROVAL_REQUEST_ID, "ap1")
        .with_attribute(attrs::APPROVAL_CALL_INDEX, 1);
    let mut value = serde_json::to_value(&attributed).unwrap();
    assert_eq!(
        serde_json::from_value::<Event>(value.clone()).unwrap(),
        attributed
    );
    assert_eq!(attributed.kind, kind);
    assert_eq!(
        value.as_object_mut().unwrap().remove("attributes"),
        Some(json!({
            "selfevol.approval.request_id": "ap1", "selfevol.approval.call_index": 1
        }))
    );
    assert_eq!(value, legacy);
}

#[test]
fn early_completion_round_trips_without_becoming_a_durable_event() {
    let item = LiveItem {
        seq: 9,
        trace_id: "trace".into(),
        kind: LiveKind::ToolCompleted {
            completion: ToolCompletion {
                span_id: "s2".into(),
                turn: 1,
                message_call_index: 2,
                call_id: "duplicate".into(),
                name: "write_file".into(),
                result: json!({"written": true}),
                error: None,
            },
        },
    };
    let value = serde_json::to_value(&item).expect("serialize completion");
    assert_eq!(value["kind"], "tool_completed");
    assert_eq!(serde_json::from_value::<LiveItem>(value).unwrap(), item);
}
