//! Shutdown, history anchors, and provisional outcomes in the attach baseline.

use std::time::Duration;

use cadmus_contract::{
    Approval, ChatRequest, Command, Event, EventError, EventKind, FinishReason, LiveItem, LiveKind,
    LiveSink, Message, Role, ToolCall, ToolCompletion, TurnOutcome, attrs, error_kinds,
};
use cadmus_transport::{Broadcaster, SETTLED_APPROVAL_WINDOW};

fn recorded(seq: u64, kind: EventKind, turn: Option<u32>) -> LiveItem {
    let mut event = Event::new(
        seq,
        format!("e{seq}"),
        "tr-sync".into(),
        format!("s{seq}"),
        Some("s0".into()),
        1_757_200_000_000 + seq,
        kind,
    );
    if let Some(turn) = turn {
        event = event.with_attribute(attrs::TURN, turn);
    }
    LiveItem {
        seq,
        trace_id: "tr-sync".into(),
        kind: LiveKind::Recorded {
            event: Box::new(event),
        },
    }
}

fn calls() -> Vec<ToolCall> {
    (0..4)
        .map(|index| ToolCall {
            id: "duplicate-id".into(),
            name: "write_file".into(),
            arguments: format!("{{\"file\":{index}}}")
                .parse()
                .expect("argument object"),
        })
        .collect()
}

fn request(seq: u64, request_id: &str, turn: u32, calls: Vec<ToolCall>) -> LiveItem {
    LiveItem {
        seq,
        trace_id: "tr-sync".into(),
        kind: LiveKind::ApprovalRequested {
            request_id: request_id.into(),
            turn,
            calls,
            wait_timeout: Duration::from_secs(300),
        },
    }
}

fn response(seq: u64, turn: Option<u32>) -> LiveItem {
    let mut message = Message::text(Role::Assistant, "proposal");
    message.content.extend(
        calls()
            .into_iter()
            .map(|call| cadmus_contract::ContentPart::ToolCall { call }),
    );
    recorded(
        seq,
        EventKind::LlmResponse {
            message,
            usage: None,
            finish: FinishReason::ToolCalls,
            outcome: TurnOutcome::Content,
            warnings: vec![],
        },
        turn,
    )
}

fn completion(seq: u64, turn: u32) -> LiveItem {
    LiveItem {
        seq,
        trace_id: "tr-sync".into(),
        kind: LiveKind::ToolCompleted {
            completion: ToolCompletion {
                span_id: format!("tool-{turn}"),
                turn,
                message_call_index: 3,
                call_id: "duplicate-id".into(),
                name: "write_file".into(),
                result: "buffered result".into(),
                error: None,
            },
        },
    }
}

#[test]
fn termination_preserves_partial_decisions_without_fabricating_denials() {
    for terminal_record in [true, false] {
        let broadcaster = Broadcaster::new();
        broadcaster.publish(&response(1, Some(1)));
        broadcaster.publish(&request(2, "partial", 1, calls()));
        let rejected = Approval::Rejected {
            comment: Some("keep it".into()),
        };
        for (seq, call_index, decision) in [(3, 3, Approval::Approved), (4, 1, rejected.clone())] {
            broadcaster.publish(&recorded(
                seq,
                EventKind::Command(Command::ResolveApprovalCall {
                    command_id: format!("cmd-{seq}"),
                    request_id: "partial".into(),
                    call_index,
                    decision,
                }),
                Some(1),
            ));
        }
        broadcaster.publish(&request(5, "unanswered", 1, calls()));
        broadcaster.publish(&request(6, "empty", 1, vec![]));
        broadcaster.publish(&completion(7, 1));
        broadcaster.publish(&recorded(
            8,
            EventKind::Command(Command::Interrupt {
                command_id: "cmd-interrupt".into(),
            }),
            Some(1),
        ));
        let interrupted = broadcaster.attach().sync;
        assert_eq!(interrupted.in_flight.pending_approvals.len(), 3);
        assert_eq!(interrupted.in_flight.completed_tools.len(), 1);
        assert!(interrupted.settled_approvals.is_empty());
        if terminal_record {
            let mut finish = recorded(9, EventKind::RunFinished { turns: 1 }, None);
            let LiveKind::Recorded { event } = &mut finish.kind else {
                unreachable!()
            };
            **event = event.clone().errored(EventError {
                kind: error_kinds::INTERRUPTED.into(),
                message: "interrupted".into(),
            });
            broadcaster.publish(&finish);
        } else {
            broadcaster.close();
        }
        let after = broadcaster.attach().sync;
        assert_eq!(after.history.finished.is_some(), terminal_record);
        assert!(after.in_flight.pending_approvals.is_empty());
        assert!(after.in_flight.completed_tools.is_empty());
        assert_eq!(after.settled_approvals.len(), 1);
        let settled = &after.settled_approvals[0];
        assert_eq!(settled.request_id, "partial");
        assert_eq!(settled.message_index, Some(0));
        assert_eq!(settled.calls, vec![calls()[1].clone(), calls()[3].clone()]);
        assert_eq!(settled.decisions, vec![rejected, Approval::Approved]);
        broadcaster.close();
        broadcaster.close();
        let closed = broadcaster.attach();
        assert_eq!(
            closed.sync, after,
            "close does not duplicate a subset settlement"
        );
        assert_eq!(closed.tail.count(), 0);
    }
}

#[test]
fn terminated_partial_batches_share_the_bounded_settled_window() {
    for terminal_record in [true, false] {
        let broadcaster = Broadcaster::new();
        let mut seq = 0;
        for index in 0..=SETTLED_APPROVAL_WINDOW {
            let request_id = format!("ap-{index}");
            seq += 1;
            broadcaster.publish(&request(seq, &request_id, 1, calls()));
            seq += 1;
            broadcaster.publish(&recorded(
                seq,
                EventKind::Command(Command::ResolveApprovalCall {
                    command_id: format!("cmd-{seq}"),
                    request_id,
                    call_index: 2,
                    decision: Approval::Approved,
                }),
                Some(1),
            ));
        }
        seq += 1;
        broadcaster.publish(&request(seq, "unanswered", 1, calls()));
        if terminal_record {
            broadcaster.publish(&recorded(
                seq + 1,
                EventKind::RunFinished { turns: 1 },
                None,
            ));
        } else {
            broadcaster.close();
        }
        let sync = broadcaster.attach().sync;
        assert!(sync.in_flight.pending_approvals.is_empty());
        assert_eq!(sync.settled_approvals.len(), SETTLED_APPROVAL_WINDOW);
        for (index, settled) in sync.settled_approvals.iter().enumerate() {
            assert_eq!(settled.request_id, format!("ap-{}", index + 1));
            assert_eq!(settled.calls, vec![calls()[2].clone()]);
            assert_eq!(settled.decisions, vec![Approval::Approved]);
            assert_eq!(settled.message_index, None);
        }
        broadcaster.close();
        assert_eq!(broadcaster.attach().sync, sync);
    }
}

#[test]
fn message_indexes_match_replays_duplicate_and_missing_metadata_rules() {
    let broadcaster = Broadcaster::new();
    broadcaster.publish(&response(1, Some(99)));
    let mut base = ChatRequest::user_text("seed", 4_096);
    base.messages.extend([
        Message::text(Role::Assistant, "seed response"),
        Message::user("again"),
    ]);
    broadcaster.publish(&recorded(
        2,
        EventKind::Command(Command::StartRun {
            base: Box::new(base),
            prefix: None,
        }),
        None,
    ));
    broadcaster.publish(&request(3, "replaced-history", 99, calls()));
    broadcaster.publish(&recorded(
        4,
        EventKind::Command(Command::StartRun {
            base: Box::new(ChatRequest::user_text("ignored", 4_096)),
            prefix: None,
        }),
        None,
    ));
    let mut partial = response(5, Some(1));
    let LiveKind::Recorded { event } = &mut partial.kind else {
        unreachable!()
    };
    **event = event.clone().errored(EventError {
        kind: error_kinds::INTERRUPTED.into(),
        message: "partial".into(),
    });
    broadcaster.publish(&partial);
    partial.seq = 6;
    let LiveKind::Recorded { event } = &mut partial.kind else {
        unreachable!()
    };
    event.seq = 6;
    broadcaster.publish(&partial);
    broadcaster.publish(&request(7, "known", 1, calls()));
    broadcaster.publish(&response(8, None));
    broadcaster.publish(&request(9, "missing-turn", 1, calls()));
    broadcaster.publish(&response(10, Some(2)));
    broadcaster.publish(&request(11, "old-turn", 1, calls()));
    broadcaster.publish(&request(12, "latest", 2, calls()));
    let sync = broadcaster.attach().sync;
    assert_eq!(sync.history.messages.len(), 6);
    assert_eq!(
        sync.history.turns, 3,
        "errored responses append but do not count as completed turns"
    );
    assert_eq!(
        sync.in_flight
            .pending_approvals
            .iter()
            .map(|pending| pending.message_index)
            .collect::<Vec<_>>(),
        vec![None, Some(3), None, None, Some(5)]
    );
    assert_eq!(sync.history.messages[3].text_body(), "proposal");
    assert_eq!(sync.history.messages[5].text_body(), "proposal");
}

#[test]
fn provisional_completions_do_not_accumulate_across_tool_batches() {
    let broadcaster = Broadcaster::new();
    for turn in 1..=40 {
        let seq = u64::from(turn) * 3;
        broadcaster.publish(&recorded(
            seq,
            EventKind::LlmRequest { trailer: None },
            Some(turn),
        ));
        assert!(
            broadcaster
                .attach()
                .sync
                .in_flight
                .completed_tools
                .is_empty()
        );
        broadcaster.publish(&completion(seq + 1, turn));
        broadcaster.publish(&completion(seq + 2, turn));
        let sync = broadcaster.attach().sync;
        assert_eq!(sync.in_flight.completed_tools.len(), 1);
        assert_eq!(sync.in_flight.completed_tools[0].turn, turn);
        assert!(sync.history.messages.is_empty());
    }
    broadcaster.publish(&completion(123, 41));
    let sync = broadcaster.attach().sync;
    assert_eq!(sync.in_flight.completed_tools.len(), 1);
    assert_eq!(sync.in_flight.completed_tools[0].turn, 41);
    broadcaster.close();
    assert!(
        broadcaster
            .attach()
            .sync
            .in_flight
            .completed_tools
            .is_empty()
    );
}
