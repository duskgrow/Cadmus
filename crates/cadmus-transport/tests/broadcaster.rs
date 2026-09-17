//! The in-process broadcaster runs the client-protocol contract suite
//! (ADR-0013 item 10), plus the insta lock of a serialized `Sync` payload
//! over the suite's fixed script and the command channel's semantics.

use cadmus_contract::testing::fixed_sync_script;
use cadmus_contract::{Command, CommandSource, LiveSink, LiveUpdate};

use cadmus_transport::{Blackhole, Broadcaster, command_channel};

// The full attach semantics: sync baseline, drop ≤ as_of_seq, pending
// approvals, lag recovery, turn-close reconciliation.
cadmus_contract::client_protocol_tests!(Broadcaster::new);

/// The serialized `Sync` over the fixed script is the byte-level lock of the
/// handshake shape (ADR-0013's consequence on insta gates).
#[test]
fn sync_payload_over_the_fixed_script_is_locked() {
    let broadcaster = Broadcaster::new();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    let attachment = broadcaster.attach();
    insta::assert_json_snapshot!(attachment.sync);
}

/// Steers and interrupts do not settle an approval request — only its
/// resolve does.
#[test]
fn only_the_matching_resolve_clears_a_pending_approval() {
    let broadcaster = Broadcaster::new();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 6,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::ApprovalRequested {
                request_id: "ap9".into(),
                turn: 1,
                calls: vec![],
                wait_timeout: std::time::Duration::from_secs(300),
            },
        },
    );
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 7,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::Recorded {
                event: Box::new(cadmus_contract::Event::new(
                    7,
                    "e7".into(),
                    "tr-suite".into(),
                    "s7".into(),
                    Some("s0".into()),
                    1_757_200_000_007,
                    cadmus_contract::EventKind::Command(Command::Interrupt {
                        command_id: "cmd-1".into(),
                    }),
                )),
            },
        },
    );
    let attachment = broadcaster.attach();
    assert_eq!(
        attachment.sync.in_flight.pending_approvals.len(),
        1,
        "an interrupt settles no approval"
    );
}

/// The settled window is bounded: past the cap the oldest settlements
/// evict — the window is render support, the trajectory log owns the
/// record.
#[test]
fn the_settled_window_evicts_the_oldest() {
    let broadcaster = Broadcaster::new();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    let total = cadmus_transport::SETTLED_APPROVAL_WINDOW + 1;
    for index in 0..total {
        let seq = 6 + u64::try_from(index).expect("index") * 2;
        LiveSink::publish(
            &broadcaster,
            &cadmus_contract::LiveItem {
                seq,
                trace_id: "tr-suite".into(),
                kind: cadmus_contract::LiveKind::ApprovalRequested {
                    request_id: format!("ap{index}"),
                    turn: 1,
                    calls: vec![],
                    wait_timeout: std::time::Duration::from_secs(300),
                },
            },
        );
        LiveSink::publish(
            &broadcaster,
            &cadmus_contract::LiveItem {
                seq: seq + 1,
                trace_id: "tr-suite".into(),
                kind: cadmus_contract::LiveKind::Recorded {
                    event: Box::new(cadmus_contract::Event::new(
                        seq + 1,
                        format!("e{}", seq + 1),
                        "tr-suite".into(),
                        format!("s{}", seq + 1),
                        Some("s0".into()),
                        1_757_200_000_007,
                        cadmus_contract::EventKind::Command(Command::ResolveApproval {
                            command_id: format!("cmd-{index}"),
                            request_id: format!("ap{index}"),
                            decisions: vec![cadmus_contract::Approval::Approved],
                        }),
                    )),
                },
            },
        );
    }
    let attachment = broadcaster.attach();
    let settled = &attachment.sync.settled_approvals;
    assert_eq!(
        settled.len(),
        cadmus_transport::SETTLED_APPROVAL_WINDOW,
        "the window holds the newest settlements"
    );
    assert_eq!(
        settled[0].request_id, "ap1",
        "the oldest settlement evicted (window starts at the second)"
    );
    assert_eq!(
        settled[settled.len() - 1].request_id,
        format!("ap{}", total - 1)
    );
}

/// A resolve the aggregator never saw a request for (a lossy gap's answer)
/// settles nothing: no settled record, and the pending queue untouched by
/// a foreign id.
#[test]
fn a_resolve_for_an_unseen_request_settles_nothing() {
    let broadcaster = Broadcaster::new();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 6,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::Recorded {
                event: Box::new(cadmus_contract::Event::new(
                    6,
                    "e6".into(),
                    "tr-suite".into(),
                    "s6".into(),
                    Some("s0".into()),
                    1_757_200_000_007,
                    cadmus_contract::EventKind::Command(Command::ResolveApproval {
                        command_id: "cmd-1".into(),
                        request_id: "ap-never-published".into(),
                        decisions: vec![cadmus_contract::Approval::Approved],
                    }),
                )),
            },
        },
    );
    let attachment = broadcaster.attach();
    assert!(attachment.sync.settled_approvals.is_empty());
    assert!(attachment.sync.in_flight.pending_approvals.is_empty());
}

/// Settled batches survive the run's terminal record: an attach after the
/// finish replays the full history's decisions, not only the fold's
/// consequences.
#[test]
fn settled_approvals_survive_run_finished() {
    let broadcaster = Broadcaster::new();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 6,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::ApprovalRequested {
                request_id: "ap9".into(),
                turn: 1,
                calls: vec![],
                wait_timeout: std::time::Duration::from_secs(300),
            },
        },
    );
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 7,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::Recorded {
                event: Box::new(cadmus_contract::Event::new(
                    7,
                    "e7".into(),
                    "tr-suite".into(),
                    "s7".into(),
                    Some("s0".into()),
                    1_757_200_000_007,
                    cadmus_contract::EventKind::Command(Command::ResolveApproval {
                        command_id: "cmd-1".into(),
                        request_id: "ap9".into(),
                        decisions: vec![cadmus_contract::Approval::Rejected {
                            comment: Some("not today".into()),
                        }],
                    }),
                )),
            },
        },
    );
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 8,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::Recorded {
                event: Box::new(cadmus_contract::Event::new(
                    8,
                    "e8".into(),
                    "tr-suite".into(),
                    "s8".into(),
                    Some("s0".into()),
                    1_757_200_000_008,
                    cadmus_contract::EventKind::RunFinished { turns: 1 },
                )),
            },
        },
    );
    let attachment = broadcaster.attach();
    assert_eq!(attachment.sync.settled_approvals.len(), 1);
    assert_eq!(attachment.sync.settled_approvals[0].request_id, "ap9");
}

/// The command channel: order, poll non-blocking, close semantics.
#[tokio::test]
async fn command_channel_is_fifo_and_close_aware() {
    let (sender, receiver) = command_channel();
    assert_eq!(receiver.poll(), None, "poll never blocks on empty");
    sender
        .send(Command::Interrupt {
            command_id: "cmd-1".into(),
        })
        .expect("send");
    sender
        .send(Command::Interrupt {
            command_id: "cmd-2".into(),
        })
        .expect("send");
    assert_eq!(
        receiver
            .poll()
            .and_then(|command| command.command_id().map(str::to_owned)),
        Some("cmd-1".into()),
        "poll drains first"
    );
    let next = receiver.recv().await.expect("the second command");
    assert_eq!(next.command_id(), Some("cmd-2"));
    drop(sender);
    assert_eq!(receiver.recv().await, None, "closed reads as None");
}

/// The blackhole sink drops everything and never panics.
#[test]
fn blackhole_swallows() {
    let item = &fixed_sync_script()[0];
    LiveSink::publish(&Blackhole, item);
}

/// `close` ends open tails, makes publish a no-op, and still serves the
/// final baseline to a late attach — the renderer-termination guarantee
/// `run_chat` leans on.
#[test]
fn close_ends_tails_and_freezes_the_baseline() {
    let broadcaster = Broadcaster::new();
    let attachment = broadcaster.attach();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    broadcaster.close();

    // The open tail drains what was queued, then ends — never a lag marker.
    let updates: Vec<_> = attachment.tail.collect();
    assert_eq!(updates.len(), 5);
    assert!(
        updates
            .iter()
            .all(|update| matches!(update, LiveUpdate::Item { .. }))
    );

    // Publish after close is dropped; a late attach gets the final state.
    LiveSink::publish(
        &broadcaster,
        &cadmus_contract::LiveItem {
            seq: 6,
            trace_id: "tr-suite".into(),
            kind: cadmus_contract::LiveKind::AssistantDelta {
                turn: 1,
                chunk: cadmus_contract::StreamChunk::TextDelta("late".into()),
            },
        },
    );
    let late = broadcaster.attach();
    assert_eq!(late.sync.as_of_seq, 5);
    assert_eq!(
        late.sync
            .in_flight
            .open_turn
            .as_ref()
            .map(|open| open.partial.text.as_str()),
        Some("partial")
    );
    assert_eq!(late.tail.count(), 0, "a closed run's tail ends at once");
}

/// A subscriber that drains promptly never sees the lag marker.
#[test]
fn an_attentive_subscriber_never_lags() {
    let broadcaster = Broadcaster::new();
    let attachment = broadcaster.attach();
    for item in fixed_sync_script() {
        LiveSink::publish(&broadcaster, &item);
    }
    let updates: Vec<_> = attachment.tail.take(5).collect();
    assert!(
        updates
            .iter()
            .all(|update| matches!(update, LiveUpdate::Item { .. })),
        "an attentive tail is all items"
    );
}
