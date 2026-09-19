//! Approval aggregation at the transport boundary, including legacy fallback.

use std::time::Duration;

use cadmus_contract::{
    Approval, Command, CommandSource, Event, EventKind, LiveItem, LiveKind, LiveSink, ToolCall,
};
use cadmus_transport::{Broadcaster, command_channel};

fn calls(count: usize) -> Vec<ToolCall> {
    (0..count)
        .map(|index| ToolCall {
            id: format!("call-{index}"),
            name: "write_file".into(),
            arguments: "{}".parse().expect("empty argument object"),
        })
        .collect()
}

fn request(seq: u64, request_id: &str, count: usize) -> LiveItem {
    LiveItem {
        seq,
        trace_id: "tr-approval".into(),
        kind: LiveKind::ApprovalRequested {
            request_id: request_id.into(),
            turn: 1,
            calls: calls(count),
            wait_timeout: Duration::from_secs(300),
        },
    }
}

fn recorded(seq: u64, command: Command) -> LiveItem {
    LiveItem {
        seq,
        trace_id: "tr-approval".into(),
        kind: LiveKind::Recorded {
            event: Box::new(Event::new(
                seq,
                format!("e{seq}"),
                "tr-approval".into(),
                format!("s{seq}"),
                Some("s0".into()),
                1_757_200_000_000 + seq,
                EventKind::Command(command),
            )),
        },
    }
}

#[test]
fn legacy_replies_use_original_positions_and_preserve_partial_decisions() {
    use Approval::{Approved, Rejected};

    let first = Rejected {
        comment: Some("first recorded decision".into()),
    };
    let second = Rejected {
        comment: Some("batch decision".into()),
    };
    let missing = Rejected { comment: None };
    for (reply, expected) in [
        (
            vec![
                Approved,
                second.clone(),
                missing.clone(),
                Approved,
                Approved,
            ],
            vec![first.clone(), second.clone(), Approved, Approved],
        ),
        (
            vec![Approved, second.clone(), missing.clone()],
            vec![first.clone(), second, Approved, missing.clone()],
        ),
        (
            vec![Approved],
            vec![first.clone(), missing.clone(), Approved, missing.clone()],
        ),
        (
            vec![],
            vec![first.clone(), missing.clone(), Approved, missing],
        ),
    ] {
        let broadcaster = Broadcaster::new();
        broadcaster.publish(&request(1, "ap", 4));
        for (seq, call_index, decision) in [(2, 0, first.clone()), (3, 2, Approved)] {
            broadcaster.publish(&recorded(
                seq,
                Command::ResolveApprovalCall {
                    command_id: format!("cmd-{seq}"),
                    request_id: "ap".into(),
                    call_index,
                    decision,
                },
            ));
        }
        broadcaster.publish(&recorded(
            4,
            Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "ap".into(),
                decisions: reply,
            },
        ));
        broadcaster.publish(&recorded(
            5,
            Command::ResolveApproval {
                command_id: "cmd-late-batch".into(),
                request_id: "ap".into(),
                decisions: vec![Approved; 4],
            },
        ));
        let sync = broadcaster.attach().sync;
        assert!(sync.in_flight.pending_approvals.is_empty());
        assert_eq!(sync.settled_approvals.len(), 1);
        assert_eq!(sync.settled_approvals[0].request_id, "ap");
        assert_eq!(sync.settled_approvals[0].calls, calls(4));
        assert_eq!(sync.settled_approvals[0].decisions, expected);
    }
}

#[test]
fn unknown_requests_and_out_of_range_indexes_leave_partial_approvals_unchanged() {
    let broadcaster = Broadcaster::new();
    broadcaster.publish(&request(1, "ap", 2));
    broadcaster.publish(&request(2, "other", 1));
    broadcaster.publish(&recorded(
        3,
        Command::ResolveApprovalCall {
            command_id: "cmd-first".into(),
            request_id: "ap".into(),
            call_index: 1,
            decision: Approval::Rejected { comment: None },
        },
    ));
    let before = broadcaster.attach().sync;
    for (offset, (request_id, call_index)) in [("unseen", 0), ("ap", 2), ("ap", usize::MAX)]
        .into_iter()
        .enumerate()
    {
        let seq = 4 + u64::try_from(offset).expect("small index");
        broadcaster.publish(&recorded(
            seq,
            Command::ResolveApprovalCall {
                command_id: format!("cmd-{seq}"),
                request_id: request_id.into(),
                call_index,
                decision: Approval::Approved,
            },
        ));
        let after = broadcaster.attach().sync;
        assert_eq!(after.as_of_seq, seq);
        assert_eq!(after.in_flight, before.in_flight);
        assert!(after.settled_approvals.is_empty());
    }
    broadcaster.publish(&recorded(
        7,
        Command::ResolveApproval {
            command_id: "cmd-unseen-batch".into(),
            request_id: "unseen".into(),
            decisions: vec![Approval::Approved],
        },
    ));
    assert_eq!(broadcaster.attach().sync.in_flight, before.in_flight);
    broadcaster.publish(&recorded(
        8,
        Command::ResolveApprovalCall {
            command_id: "cmd-final".into(),
            request_id: "ap".into(),
            call_index: 0,
            decision: Approval::Approved,
        },
    ));
    let after = broadcaster.attach().sync;
    assert_eq!(
        after.in_flight.pending_approvals,
        before.in_flight.pending_approvals[1..]
    );
    assert_eq!(after.settled_approvals.len(), 1);
    assert_eq!(
        after.settled_approvals[0].decisions,
        vec![Approval::Approved, Approval::Rejected { comment: None }]
    );
}

#[test]
fn empty_batches_still_settle_with_the_legacy_reply() {
    for decisions in [vec![], vec![Approval::Approved]] {
        let broadcaster = Broadcaster::new();
        broadcaster.publish(&request(1, "empty", 0));
        broadcaster.publish(&recorded(
            2,
            Command::ResolveApprovalCall {
                command_id: "cmd-invalid".into(),
                request_id: "empty".into(),
                call_index: 0,
                decision: Approval::Approved,
            },
        ));
        let waiting = broadcaster.attach().sync;
        assert_eq!(waiting.in_flight.pending_approvals.len(), 1);
        assert!(waiting.settled_approvals.is_empty());
        broadcaster.publish(&recorded(
            3,
            Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "empty".into(),
                decisions: decisions.clone(),
            },
        ));
        let sync = broadcaster.attach().sync;
        assert!(sync.in_flight.pending_approvals.is_empty());
        assert_eq!(sync.settled_approvals.len(), 1);
        assert_eq!(sync.settled_approvals[0].request_id, "empty");
        assert!(sync.settled_approvals[0].calls.is_empty());
        assert_eq!(sync.settled_approvals[0].decisions, decisions);
    }
}

#[test]
fn partial_batches_leave_the_settled_window_intact_until_the_final_decision() {
    let broadcaster = Broadcaster::new();
    let mut seq = 0;
    for index in 0..=cadmus_transport::SETTLED_APPROVAL_WINDOW {
        let request_id = format!("ap-{index}");
        seq += 1;
        broadcaster.publish(&request(seq, &request_id, 2));
        let before = broadcaster.attach().sync.settled_approvals;
        for call_index in [1, 0] {
            seq += 1;
            broadcaster.publish(&recorded(
                seq,
                Command::ResolveApprovalCall {
                    command_id: format!("cmd-{seq}"),
                    request_id: request_id.clone(),
                    call_index,
                    decision: Approval::Approved,
                },
            ));
            if call_index == 1 {
                assert_eq!(broadcaster.attach().sync.settled_approvals, before);
            }
        }
    }
    let sync = broadcaster.attach().sync;
    assert!(sync.in_flight.pending_approvals.is_empty());
    assert_eq!(
        sync.settled_approvals.len(),
        cadmus_transport::SETTLED_APPROVAL_WINDOW
    );
    for (index, settled) in sync.settled_approvals.iter().enumerate() {
        assert_eq!(settled.request_id, format!("ap-{}", index + 1));
        assert_eq!(settled.calls, calls(2));
        assert_eq!(settled.decisions, vec![Approval::Approved; 2]);
    }
}

#[test]
fn sending_a_call_resolution_has_no_effect_until_it_is_recorded() {
    let broadcaster = Broadcaster::new();
    broadcaster.publish(&request(1, "ap", 2));
    let (sender, receiver) = command_channel();
    for (seq, call_index) in [(2, 1), (3, 0)] {
        let before = broadcaster.attach().sync;
        let command = Command::ResolveApprovalCall {
            command_id: format!("cmd-{seq}"),
            request_id: "ap".into(),
            call_index,
            decision: Approval::Approved,
        };
        sender.send(command.clone()).expect("send");
        assert_eq!(broadcaster.attach().sync, before);
        assert_eq!(receiver.poll(), Some(command.clone()));
        assert_eq!(broadcaster.attach().sync, before);
        broadcaster.publish(&recorded(seq, command));
        let after = broadcaster.attach().sync;
        assert_eq!(after.as_of_seq, seq);
        if call_index == 1 {
            assert_eq!(after.in_flight.pending_approvals.len(), 1);
            assert_eq!(
                after.in_flight.pending_approvals[0].decisions,
                vec![None, Some(Approval::Approved)]
            );
            assert!(after.settled_approvals.is_empty());
        } else {
            assert!(after.in_flight.pending_approvals.is_empty());
            assert_eq!(after.settled_approvals.len(), 1);
            assert_eq!(
                after.settled_approvals[0].decisions,
                vec![Approval::Approved; 2]
            );
        }
    }
}
