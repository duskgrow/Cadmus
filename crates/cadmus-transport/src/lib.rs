//! The in-process client protocol endpoint (ADR-0002's "in-process channel
//! first", ADR-0013): the [`Broadcaster`] is one run's live-stream hub — the
//! loop publishes through it (it is the [`LiveSink`]), clients
//! [`attach`](Broadcaster::attach) for the sync-on-subscribe handshake plus
//! the live tail, and the command channel ([`command_channel`]) carries the
//! upstream.
//!
//! The handshake's TOCTOU hole is closed with a lock, not a ring buffer:
//! attach snapshots and subscribes atomically, so the backfill set is empty
//! and the client rule (`drop positions ≤ as_of_seq`, apply the rest) needs
//! no gap repair in-process. Deltas are folded incrementally into a replica
//! of the loop's own [`MessageAssembler`] — bounded state, and no second
//! implementation of the aggregation semantics (item 3).
//!
//! Everything here is in-memory: no IO, no clocks, no id minting (the loop
//! stamps every position). The stdio/NDJSON and phase-5 socket transports
//! run the same contract-test suite.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cadmus_contract::testing::ProtocolSubject;
use cadmus_contract::{
    Approval, Attachment, Command, CommandSource, Event, EventKind, InFlight, LiveItem, LiveKind,
    LiveSink, LiveUpdate, OpenTurn, PendingApproval, SettledApproval, Sync, TimedRecv,
    ToolCompletion,
};
use cadmus_core::{MessageAssembler, latest_response_anchor, replay_trace};

/// The default per-subscriber queue bound. Deltas arrive at token rate, so
/// a stalled renderer eventually lags — and is told to re-sync (item 5),
/// never awaited.
pub const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

/// How many settled approval batches the attach baseline remembers. An
/// interactive session's settled batches are few; the window is render
/// support for the rebuild (the trajectory log owns the record), so a
/// small bound suffices — older settlements keep rendering through their
/// consequences in the fold.
pub const SETTLED_APPROVAL_WINDOW: usize = 32;

/// One run's live-stream hub. See the crate docs.
pub struct Broadcaster {
    state: Mutex<State>,
    queue_capacity: usize,
}

#[derive(Default)]
struct State {
    /// Every durable event republished so far — the attach-time fold source
    /// (attach to finished or foreign traces reads the log file instead;
    /// that path lands with the session picker).
    events: Vec<Event>,
    /// Recorded event ids already applied, so a duplicate republication cannot
    /// apply the aggregator's transitions twice (the fold dedups separately).
    seen_event_ids: HashSet<String>,
    /// The open turn's replica of the loop's assembler, fed the same deltas.
    open_turn: Option<(u32, MessageAssembler)>,
    /// Approval requests not yet settled by a recorded resolve.
    pending_approvals: Vec<PendingApproval>,
    /// Provisional outcomes only; durable results retire them by unique span.
    completed_tools: Vec<ToolCompletion>,
    /// Settled batches awaiting an attach, oldest first, capped at
    /// [`SETTLED_APPROVAL_WINDOW`]: the sync baseline's explicit record of
    /// decisions the log holds but the fold does not replay.
    settled_approvals: VecDeque<SettledApproval>,
    /// The highest position published — the handshake's `as_of_seq`.
    last_seq: u64,
    subscribers: Vec<Subscription>,
    /// Set by [`Broadcaster::close`]: the run is over, tails end, publish is
    /// a no-op. A late attach still gets the full final baseline.
    closed: bool,
}

struct Subscription {
    sender: mpsc::SyncSender<LiveUpdate>,
    /// Set when a publish found the queue full: the tail reports `Lagged`
    /// after draining what it has, then ends.
    lagged: Arc<AtomicBool>,
}

impl Broadcaster {
    #[must_use]
    pub fn new() -> Self {
        Self::with_queue_capacity(DEFAULT_QUEUE_CAPACITY)
    }

    #[must_use]
    pub fn with_queue_capacity(queue_capacity: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            queue_capacity,
        }
    }

    #[must_use]
    pub fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    /// Ends every open tail (the run is over): receivers drain what they
    /// have and see the end. Publishing after close is a no-op; a late
    /// attach gets the final baseline with an immediately-ended tail.
    pub fn close(&self) {
        let mut state = self.state.lock().expect("broadcaster poisoned");
        state.closed = true;
        Self::retire_pending_approvals(&mut state);
        state.completed_tools.clear();
        state.subscribers.clear();
    }

    /// The sync-on-subscribe handshake (item 3): the baseline is the fold of
    /// everything recorded so far plus the live replica's snapshot, current
    /// as of `as_of_seq`; the tail then carries every later item. Snapshot
    /// and subscription happen under one lock, so no publish can slip
    /// between them (item 4's TOCTOU).
    #[must_use]
    pub fn attach(&self) -> Attachment {
        let mut state = self.state.lock().expect("broadcaster poisoned");
        let in_flight = InFlight {
            open_turn: state.open_turn.as_ref().map(|(turn, assembler)| OpenTurn {
                turn: *turn,
                partial: assembler.snapshot(),
            }),
            pending_approvals: state.pending_approvals.clone(),
            completed_tools: state.completed_tools.clone(),
        };
        let sync = Sync {
            history: replay_trace(&state.events),
            in_flight,
            settled_approvals: state.settled_approvals.iter().cloned().collect(),
            as_of_seq: state.last_seq,
        };
        let (sender, receiver) = mpsc::sync_channel(self.queue_capacity);
        let lagged = Arc::new(AtomicBool::new(false));
        // A finished run: the baseline is complete and the tail ends at
        // once — the sender never lands in a subscriber list, so its drop
        // at scope end makes this receiver read the end.
        if !state.closed {
            state.subscribers.push(Subscription {
                sender,
                lagged: lagged.clone(),
            });
        }
        Attachment {
            sync,
            tail: Box::new(Tail {
                receiver,
                lagged,
                lagged_reported: false,
            }),
        }
    }

    /// Folds one published item into the attach-time replica state.
    fn track(state: &mut State, item: &LiveItem) {
        // The loop mints strictly increasing positions; a violation means a
        // publisher bug, and the `max` would silently mask it for clients
        // applying "drop ≤ as_of_seq".
        debug_assert!(
            item.seq > state.last_seq,
            "live items must publish in strictly increasing seq order ({} after {})",
            item.seq,
            state.last_seq
        );
        state.last_seq = state.last_seq.max(item.seq);
        match &item.kind {
            LiveKind::Recorded { event } => Self::track_recorded(state, event),
            LiveKind::AssistantDelta { turn, chunk } => {
                let replica = match &mut state.open_turn {
                    Some((open, assembler)) if *open == *turn => assembler,
                    // A new turn's first delta (or a turn whose response was
                    // never recorded) resets the replica.
                    _ => &mut state.open_turn.insert((*turn, MessageAssembler::new())).1,
                };
                replica.push(chunk.clone());
            }
            LiveKind::ToolCompleted { completion } => {
                // Only the current batch's unrecorded outcomes belong here,
                // never a run-long cache keyed by provider call ids.
                state
                    .completed_tools
                    .retain(|done| done.turn == completion.turn);
                if let Some(previous) = state
                    .completed_tools
                    .iter_mut()
                    .find(|done| done.span_id == completion.span_id)
                {
                    previous.clone_from(completion);
                } else {
                    state.completed_tools.push(completion.clone());
                }
            }
            LiveKind::ApprovalRequested {
                request_id,
                turn,
                calls,
                wait_timeout,
            } => state.pending_approvals.push(PendingApproval {
                request_id: request_id.clone(),
                turn: *turn,
                // One walk over the log per gated batch — bounded by the run,
                // and far cheaper than the attach-time fold reading the same
                // vec; a kept counter would have to mirror that fold by hand.
                message_index: latest_response_anchor(&state.events)
                    .filter(|(response_turn, _)| response_turn == turn)
                    .map(|(_, index)| index),
                calls: calls.clone(),
                decisions: vec![None; calls.len()],
                wait_timeout: *wait_timeout,
            }),
        }
    }

    fn track_recorded(state: &mut State, event: &Event) {
        // The fold dedups by id too; here the guard keeps a duplicate
        // republication from applying the aggregator's own transitions twice.
        if !state.seen_event_ids.insert(event.id.clone()) {
            return;
        }
        match &event.kind {
            EventKind::LlmResponse { .. } => {
                // The completed message now lives in the fold, replacing the
                // open turn's delta replica (item 5's zero-jump rule).
                if state.open_turn.as_ref().map(|(open, _)| *open) == event.turn() {
                    state.open_turn = None;
                }
            }
            EventKind::ToolResult { .. } => {
                state
                    .completed_tools
                    .retain(|done| done.span_id != event.span_id);
            }
            EventKind::LlmRequest { .. } => state.completed_tools.clear(),
            EventKind::Command(Command::ResolveApproval {
                request_id,
                decisions,
                ..
            }) => Self::resolve_approval_batch(state, request_id, decisions),
            EventKind::Command(Command::ResolveApprovalCall {
                request_id,
                call_index,
                decision,
                ..
            }) => Self::resolve_approval_call(state, request_id, *call_index, decision),
            EventKind::RunFinished { .. } => {
                state.open_turn = None;
                Self::retire_pending_approvals(state);
                state.completed_tools.clear();
            }
            EventKind::Command(
                Command::StartRun { .. } | Command::Steer { .. } | Command::Interrupt { .. },
            )
            | EventKind::InstructionInjected { .. }
            | EventKind::ToolCall { .. }
            | EventKind::EvalScore(_)
            | EventKind::Fold { .. } => {}
        }
        state.events.push(event.clone());
    }

    fn resolve_approval_call(
        state: &mut State,
        request_id: &str,
        call_index: usize,
        decision: &Approval,
    ) {
        let Some(position) = state
            .pending_approvals
            .iter()
            .position(|pending| pending.request_id == request_id)
        else {
            return;
        };

        let complete = {
            let pending = &mut state.pending_approvals[position];
            if call_index >= pending.calls.len() || pending.decisions[call_index].is_some() {
                return;
            }
            // Addressing is by the original batch position, not by call id:
            // duplicate provider ids must still have independent slots.
            pending.decisions[call_index] = Some(decision.clone());
            pending.decisions.iter().all(Option::is_some)
        };

        if complete {
            let pending = state.pending_approvals.remove(position);
            let decisions = pending
                .decisions
                .into_iter()
                .map(|decision| decision.expect("a complete approval has every decision"))
                .collect();
            Self::remember_settlement(
                state,
                SettledApproval {
                    request_id: pending.request_id,
                    message_index: pending.message_index,
                    calls: pending.calls,
                    decisions,
                },
            );
        }
    }

    fn resolve_approval_batch(state: &mut State, request_id: &str, decisions: &[Approval]) {
        let Some(position) = state
            .pending_approvals
            .iter()
            .position(|pending| pending.request_id == request_id)
        else {
            return;
        };

        let pending = state.pending_approvals.remove(position);
        let decisions = if pending.calls.is_empty() {
            // Preserve the legacy settlement shape for empty batches.
            decisions.to_vec()
        } else {
            pending
                .decisions
                .into_iter()
                .enumerate()
                .map(|(index, recorded)| {
                    // Batch replies retain original positions, even for slots
                    // already settled individually. Missing answers deny.
                    recorded.unwrap_or_else(|| {
                        decisions
                            .get(index)
                            .cloned()
                            .unwrap_or(Approval::Rejected { comment: None })
                    })
                })
                .collect()
        };
        Self::remember_settlement(
            state,
            SettledApproval {
                request_id: pending.request_id,
                message_index: pending.message_index,
                calls: pending.calls,
                decisions,
            },
        );
    }

    fn retire_pending_approvals(state: &mut State) {
        for pending in std::mem::take(&mut state.pending_approvals) {
            // Termination is not a resolution command: preserve known answers
            // in relative call order, without inventing denials for siblings.
            let (calls, decisions): (Vec<_>, Vec<_>) = pending
                .calls
                .into_iter()
                .zip(pending.decisions)
                .filter_map(|(call, decision)| decision.map(|decision| (call, decision)))
                .unzip();
            if !decisions.is_empty() {
                Self::remember_settlement(
                    state,
                    SettledApproval {
                        request_id: pending.request_id,
                        message_index: pending.message_index,
                        calls,
                        decisions,
                    },
                );
            }
        }
    }

    fn remember_settlement(state: &mut State, settled: SettledApproval) {
        state.settled_approvals.push_back(settled);
        while state.settled_approvals.len() > SETTLED_APPROVAL_WINDOW {
            state.settled_approvals.pop_front();
        }
    }
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveSink for Broadcaster {
    fn publish(&self, item: &LiveItem) {
        let mut state = self.state.lock().expect("broadcaster poisoned");
        if state.closed {
            return;
        }
        Self::track(&mut state, item);
        state.subscribers.retain(|subscription| {
            let update = LiveUpdate::Item {
                item: Box::new(item.clone()),
            };
            match subscription.sender.try_send(update) {
                Ok(()) => true,
                // The queue is full: the client has a gap it can never
                // repair — flag the lag (its tail reports it after draining)
                // and drop the subscription.
                Err(mpsc::TrySendError::Full(_)) => {
                    subscription.lagged.store(true, Ordering::Relaxed);
                    false
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

// The contract suite drives the broadcaster through this impl. It lives in
// the lib — the orphan rule forbids it in the integration-test crate —
// matching the `ContractSubject for ReplayProvider` precedent.
impl ProtocolSubject for Broadcaster {
    fn publish(&self, item: &LiveItem) {
        LiveSink::publish(self, item);
    }

    fn attach(&self) -> Attachment {
        self.attach()
    }

    fn queue_capacity(&self) -> usize {
        self.queue_capacity()
    }
}

/// One attachment's live tail: queued items, then the lag marker if the
/// subscription was dropped for falling behind, then the end.
struct Tail {
    receiver: mpsc::Receiver<LiveUpdate>,
    lagged: Arc<AtomicBool>,
    lagged_reported: bool,
}

impl Iterator for Tail {
    type Item = LiveUpdate;

    fn next(&mut self) -> Option<LiveUpdate> {
        // The publisher is gone (run ended) or this subscription was dropped
        // for lagging — the two ends a client must tell apart.
        if let Ok(update) = self.receiver.recv() {
            Some(update)
        } else if !self.lagged_reported && self.lagged.load(Ordering::Relaxed) {
            self.lagged_reported = true;
            Some(LiveUpdate::Lagged)
        } else {
            None
        }
    }
}

/// The upstream command channel (ADR-0013 item 6): unbounded, so a client
/// (or an auto-resolving policy) never blocks the loop mid-publish;
/// commands are deduped by the loop's inbox, so an over-full channel is a
/// client bug, not a backpressure signal.
#[must_use]
pub fn command_channel() -> (CommandSender, CommandReceiver) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        CommandSender(sender),
        CommandReceiver {
            receiver: tokio::sync::Mutex::new(receiver),
        },
    )
}

/// The send half; cloneable — every client of a run shares one channel.
#[derive(Clone)]
pub struct CommandSender(tokio::sync::mpsc::UnboundedSender<Command>);

impl CommandSender {
    /// `Err(command)` when the run is gone — the caller's signal to stop
    /// retrying, not to buffer.
    pub fn send(&self, command: Command) -> Result<(), Command> {
        self.0.send(command).map_err(|error| error.0)
    }
}

/// The receive half: the loop's [`CommandSource`] port.
pub struct CommandReceiver {
    receiver: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Command>>,
}

#[async_trait::async_trait]
impl CommandSource for CommandReceiver {
    async fn recv(&self) -> Option<Command> {
        self.receiver.lock().await.recv().await
    }

    async fn recv_timeout(&self, duration: std::time::Duration) -> TimedRecv {
        // The interactive client's pairing rule (ADR-0008 item 4): the wait
        // on a human carries a deny deadline — core's gate races against
        // this without core touching a runtime.
        match tokio::time::timeout(duration, self.recv()).await {
            Ok(Some(command)) => TimedRecv::Command(command),
            Ok(None) => TimedRecv::Closed,
            Err(_) => TimedRecv::TimedOut,
        }
    }

    fn poll(&self) -> Option<Command> {
        // `recv` holds this lock across its await, so while a retained receive
        // future is parked (the gate's approval wait) the lock is held and a
        // poll here is a no-op by construction — the parked future is what
        // delivers the next command, which is why dispatch polls it before
        // admitting work. Otherwise (single loop task) `None` is "nothing
        // buffered".
        self.receiver.try_lock().ok()?.try_recv().ok()
    }
}

/// A [`LiveSink`] that drops everything — for runs nobody watches (eval).
#[derive(Debug, Default, Clone, Copy)]
pub struct Blackhole;

impl LiveSink for Blackhole {
    fn publish(&self, _item: &LiveItem) {}
}

#[cfg(test)]
mod tests {
    use cadmus_contract::Command;

    use super::*;

    /// The deadline mechanics the gate's human-wait timeout rides on: a
    /// command inside the deadline arrives, silence past it reports
    /// `TimedOut`, and a closed channel reports `Closed` either way.
    #[tokio::test]
    async fn recv_timeout_reports_silence_commands_and_closure() {
        let (sender, receiver) = command_channel();

        // Silence past the deadline: real time, the outcome is the point.
        assert!(
            matches!(
                receiver
                    .recv_timeout(std::time::Duration::from_millis(20))
                    .await,
                TimedRecv::TimedOut
            ),
            "silence past the deadline gives up"
        );

        sender
            .send(Command::Interrupt {
                command_id: "cmd-1".into(),
            })
            .expect("send");
        assert!(matches!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .await,
            TimedRecv::Command(Command::Interrupt { .. })
        ));

        drop(sender);
        assert!(matches!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(50))
                .await,
            TimedRecv::Closed
        ));
    }
}
