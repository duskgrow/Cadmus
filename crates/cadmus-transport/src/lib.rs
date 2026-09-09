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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use cadmus_contract::testing::ProtocolSubject;
use cadmus_contract::{
    Attachment, Command, CommandSource, Event, EventKind, InFlight, LiveItem, LiveKind, LiveSink,
    LiveUpdate, OpenTurn, PendingApproval, Sync, attrs,
};
use cadmus_core::{MessageAssembler, replay_trace};

/// The default per-subscriber queue bound. Deltas arrive at token rate, so
/// a stalled renderer eventually lags — and is told to re-sync (item 5),
/// never awaited.
pub const DEFAULT_QUEUE_CAPACITY: usize = 4_096;

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
    /// The open turn's replica of the loop's assembler, fed the same deltas.
    open_turn: Option<(u32, MessageAssembler)>,
    /// Approval requests not yet settled by a recorded resolve.
    pending_approvals: Vec<PendingApproval>,
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
        };
        let sync = Sync {
            history: replay_trace(&state.events),
            in_flight,
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
            LiveKind::Recorded { event } => {
                match &event.kind {
                    EventKind::LlmResponse { .. } => {
                        // The response reconciles the delta buffer: the open
                        // turn's replica is retired, the completed message
                        // now lives in the fold (item 5's zero-jump rule).
                        let turn = turn_of(event);
                        if state.open_turn.as_ref().map(|(open, _)| *open) == turn {
                            state.open_turn = None;
                        }
                    }
                    EventKind::Command(Command::ResolveApproval { request_id, .. }) => {
                        state
                            .pending_approvals
                            .retain(|pending| pending.request_id != *request_id);
                    }
                    EventKind::RunFinished { .. } => {
                        state.open_turn = None;
                        state.pending_approvals.clear();
                    }
                    _ => {}
                }
                state.events.push((**event).clone());
            }
            LiveKind::AssistantDelta { turn, chunk } => {
                let replica = match &mut state.open_turn {
                    Some((open, assembler)) if *open == *turn => assembler,
                    // A new turn's first delta (or a turn whose response was
                    // never recorded) resets the replica.
                    _ => &mut state.open_turn.insert((*turn, MessageAssembler::new())).1,
                };
                replica.push(chunk.clone());
            }
            LiveKind::ApprovalRequested {
                request_id,
                turn,
                calls,
            } => state.pending_approvals.push(PendingApproval {
                request_id: request_id.clone(),
                turn: *turn,
                calls: calls.clone(),
            }),
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

/// The `selfevol.turn` attribute as the loop stamps it (1-based).
fn turn_of(event: &Event) -> Option<u32> {
    let value = event.attributes.get(attrs::TURN)?.as_u64()?;
    u32::try_from(value).ok()
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

    fn poll(&self) -> Option<Command> {
        // A contended lock means the loop itself is parked in `recv` — in
        // which case no poll is running (single loop task), so `None` here
        // is always "nothing buffered".
        self.receiver.try_lock().ok()?.try_recv().ok()
    }
}

/// A [`LiveSink`] that drops everything — for runs nobody watches (eval).
#[derive(Debug, Default, Clone, Copy)]
pub struct Blackhole;

impl LiveSink for Blackhole {
    fn publish(&self, _item: &LiveItem) {}
}
