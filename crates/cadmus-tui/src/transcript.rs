//! The view-model materialization (ADR-0018 item 10): protocol events flow
//! in, widget-readable rows flow out — widgets never re-derive state from
//! the event stream themselves. The transcript is a sequence of blocks:
//!
//! - **user prompts**, **tool-activity markers** and **outcome markers** —
//!   static blocks, complete at birth, so always fully flushable;
//! - **assistant messages** — [`Stream`] blocks over the markdown pipeline,
//!   live while their turn streams.
//!
//! Ordering invariant: only the tail block may be an open (unsealed)
//! assistant block — a turn's deltas end at its `llm_response`, and every
//! other event seals the open block structurally before landing. Every
//! block before the tail is therefore fully flushable at the next pump.
//!
//! Tool-result display policy — the live path and the attach/replay rebuild
//! render identical rows by construction. Every successful tool completion
//! lands in history: a perception tool (`read_file`, `list_dir`, `grep`) leaves
//! exactly one subtle line, `✓ name target` — no `▸` call marker (the
//! run-status row carries the live signal), no preview (file contents are
//! noise); an action tool renders `▸ name target` at call time, then
//! `✓ name target` plus a bounded preview (up to four lines, then a
//! truncation note) at completion. Failures keep their existing shapes: the
//! one-line `✗ name target: detail`, plus the bounded preview where the
//! provisional and approval-addressed paths already carried one.
//!
//! The snapshot/emission contract (the per-accessor re-render open item's
//! consumer): one [`Transcript::snapshot`] per pump appends every newly
//! stable row to the emission queue (a single pipeline render and a single
//! wrap pass per block, never one per accessor). Rows leave the queue only
//! through the app's paced drain, and a row is acked to the stream only
//! after its *successful* shell insert — the ack contract is unchanged
//! from the pre-queue model. The unstable tail is never rendered
//! (ADR-0018's 2026-09-20 second amendment): the band's liveness signal is
//! the `receiving…` row, fed by [`Snapshot::tail_live`] and the queue's
//! depth.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use cadmus_contract::{
    Approval, Command, Event, EventError, EventKind, LiveItem, LiveKind, Message, PendingApproval,
    Role, SettledApproval, Status, Sync, ToolCompletion,
};
use cadmus_ui::highlight::Highlighter;
use cadmus_ui::ir::{self, Slot};
use cadmus_ui::theme::{ColorDepth, Theme};
use ratatui::text::Line;

use crate::stream::Stream;
use crate::wrap::wrap_rows;

/// One assistant block: the markdown pipeline plus the scrollback/queue
/// bookkeeping in logical lines (the re-sync transfer's source).
struct Agent {
    stream: Stream,
    /// Logical lines confirmed in scrollback (acked after a successful
    /// shell insert — the drain owns this).
    acked: usize,
    /// Logical lines already queued for emission, cumulative (the
    /// snapshot's append cursor — the queued-but-undrained prefix of the
    /// pipeline's live lines is `emitted - acked`, since acked lines drop
    /// out of that list).
    emitted: usize,
}

/// One transcript block; see the module docs.
enum Block {
    /// Static content, complete at birth (prompts, markers): semantic
    /// logical lines — the SSOT re-wrapped on width change and replay.
    Static(Vec<ir::Line>),
    Agent(Agent),
}

/// Outstanding tool call tracking for live spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LiveCall {
    pub(crate) name: String,
    pub(crate) target: Option<String>,
}

/// Whether a tool is classified as a perception tool (read-only discovery/inspection).
fn is_perception_tool(name: &str) -> bool {
    matches!(name, "read_file" | "list_dir" | "grep")
}

/// What one applied item means for the status line (presentation only).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Light {
    /// No status change.
    None,
    /// Assistant output is streaming.
    Streaming,
    /// A tool is executing.
    Tool {
        name: String,
        target: Option<String>,
    },
    /// The run finished cleanly.
    Idle,
    /// The run failed.
    Failed,
}

/// One queued slice of a block's stable output: the display rows plus the
/// flush-plan step their successful insert confirms. The ack fires when the
/// slice's LAST row lands; a zero-row slice (a block whose lines were all
/// queued before it sealed — an open fence's body) fires on arrival at the
/// queue's head. `drained` marks that rows already left (the rewind's
/// keep-the-front test — see [`Transcript::rewind_queue`]).
struct Emission {
    rows: VecDeque<Line<'static>>,
    ack: FlushAck,
    drained: bool,
}

/// The per-block flush plan, aligned with the transcript's unflushed blocks
/// in order. Produced by [`Transcript::drain`], consumed by
/// [`Transcript::apply_flush`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlushAck {
    /// A static block leaves the band whole.
    WholeBlock,
    /// `lines` logical lines of an assistant block leave; `completes` marks
    /// the block fully flushed (sealed and nothing live left), so the
    /// flushed-block cursor advances past it.
    AgentLines { lines: usize, completes: bool },
}

/// One materialization pass over the not-yet-queued blocks: every newly
/// stable row appended to the emission queue (module docs), plus the band's
/// liveness input. The unstable tail is never rendered.
#[derive(Debug)]
pub struct Snapshot {
    /// Live (unstable) lines remain past the flushable prefix — the
    /// `receiving…` row's second disjunct (the queue's depth is the first).
    pub tail_live: bool,
}

/// One paced drain's haul: the rows for `InlineShell::flush` and the flush
/// plan confirming them after a successful insert.
#[derive(Debug)]
pub struct Drain {
    /// Drained display rows, in order.
    pub rows: Vec<Line<'static>>,
    /// The flush plan for [`Transcript::apply_flush`] — covers exactly the
    /// emissions whose rows ALL drained (a row is acked only after its
    /// successful shell insert).
    pub acks: Vec<FlushAck>,
}

/// The materialized view-model. See the module docs for the invariants.
pub struct Transcript {
    blocks: Vec<Block>,
    /// Leading blocks confirmed in scrollback (fully drained).
    flushed: usize,
    /// Leading blocks fully queued for emission (`flushed` ≤ `queued`; the
    /// snapshot's append cursor — block `queued` may be partially queued).
    queued: usize,
    /// The emission queue (ADR-0018's 2026-09-20 second amendment): stable
    /// rows pending the paced drain, in block order.
    queue: VecDeque<Emission>,
    /// The open assistant block's turn, when the tail is one.
    open_turn: Option<u32>,
    /// The client rule's position filter (ADR-0013 item 4): items with
    /// `seq ≤ as_of_seq` are dropped.
    as_of_seq: u64,
    /// Outstanding live span → tool call info. Provider ids can repeat even in
    /// one batch; each result retires its span.
    live_calls: HashMap<String, LiveCall>,
    /// Original approval slots retain their indices; taking a name marks its
    /// decision rendered, so retries and batch fallback cannot render it twice.
    approvals: HashMap<String, Vec<Option<String>>>,
    /// Only provisional outcomes still awaiting their durable result; span
    /// ids are run-unique, unlike provider call ids.
    completed_tools: HashSet<String>,
}

/// Push one block slice onto the emission queue (`rows` may be empty — the
/// completing-ack-only case, see [`Transcript::snapshot`]).
fn queue_emission(queue: &mut VecDeque<Emission>, rows: Vec<Line<'static>>, ack: FlushAck) {
    queue.push_back(Emission {
        rows: rows.into(),
        ack,
        drained: false,
    });
}

impl Transcript {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            flushed: 0,
            queued: 0,
            queue: VecDeque::new(),
            open_turn: None,
            as_of_seq: 0,
            live_calls: HashMap::new(),
            approvals: HashMap::new(),
            completed_tools: HashSet::new(),
        }
    }

    /// The user's submitted prompt as a static block (pushed by the app at
    /// submit — the prompt itself is not an event).
    pub fn push_user(&mut self, text: &str) {
        self.seal_open();
        let mut lines = Vec::new();
        if !self.blocks.is_empty() {
            lines.push(ir::Line::default()); // separator from the prior run
        }
        // The accent "❯ " marker is the user-voice cue; the prompt body
        // stays default (ADR-0017 slot wiring).
        lines.extend(text.lines().map(|line| {
            ir::Line::from_spans(vec![
                ir::Span::slotted("❯ ", Slot::Accent),
                ir::Span::plain(line),
            ])
        }));
        // A blank row before the response attaches.
        lines.push(ir::Line::default());
        self.blocks.push(Block::Static(lines));
    }

    /// A run-level failure not carried by the event stream (the loop task
    /// died before its terminal record).
    pub fn push_error(&mut self, text: &str) {
        self.seal_open();
        self.blocks.push(Block::Static(vec![error_line(format!(
            "run failed: {text}"
        ))]));
    }

    /// A static note block (the run-completion rows), complete at birth,
    /// so always fully flushable — the same contract as prompts and markers.
    pub fn push_note(&mut self, line: ir::Line) {
        self.seal_open();
        // A blank row separates the note from the run's tail, the same way
        // `push_user` separates the prompt from the prior run.
        let mut lines = Vec::new();
        if !self.blocks.is_empty() {
            lines.push(ir::Line::default());
        }
        lines.push(line);
        self.blocks.push(Block::Static(lines));
    }

    /// Apply one live item. Returns the status-light effect.
    pub fn apply_item(&mut self, item: &LiveItem) -> Light {
        // The one client rule (ADR-0013 item 4): drop positions at or below
        // the baseline, apply the rest — idempotent and retry-safe.
        if item.seq <= self.as_of_seq {
            return Light::None;
        }
        self.as_of_seq = self.as_of_seq.max(item.seq);
        match &item.kind {
            LiveKind::AssistantDelta { turn, chunk } => {
                let cadmus_contract::StreamChunk::TextDelta(text) = chunk else {
                    // Reasoning and tool-call fragments stay out of the
                    // transcript for now (collapsed-by-default lands with
                    // the approval/diff slice); the sealed response and the
                    // recorded calls carry the durable facts.
                    return Light::Streaming;
                };
                self.agent_block(*turn).stream.push_delta(text);
                Light::Streaming
            }
            LiveKind::ApprovalRequested {
                request_id, calls, ..
            } => {
                // The app holds the pending request for the band's dialog;
                // the transcript remembers the names so the recorded
                // resolution can name what was decided (the request itself
                // never reaches the log).
                self.approvals.insert(
                    request_id.clone(),
                    calls.iter().map(|call| Some(call.name.clone())).collect(),
                );
                Light::None
            }
            LiveKind::ToolCompleted { completion } => {
                self.push_completion(completion);
                Light::None
            }
            LiveKind::Recorded { event } => self.apply_event(event),
        }
    }

    /// Apply the attach/re-attach baseline (ADR-0013 items 3–5). The app
    /// pumps first, so everything flushable is already in scrollback; on a
    /// re-sync (`resync`) the rebuilt history then lands pre-flushed (the old
    /// scrollback rendering is the same fold, deterministic) — content that
    /// fell into the lag hole stays unrendered there (headless parity: the
    /// trajectory log is the intact record; the app marks the hole by
    /// flushing a resync marker row directly, outside the block model, so
    /// replays never reorder it against transferred rows). Whether this
    /// attach IS a re-sync is the drainer's fact, carried on the feed —
    /// never re-derived from view state. A first attach owns nothing of the
    /// baseline, so the rebuild renders block by block through the normal
    /// pump.
    ///
    /// The in-flight tail continues close to where the old one left off:
    /// same turn ⇒ its flushed prefix (already in scrollback) transfers by
    /// line count. That is sound for lines completed *before* the gap
    /// (deterministic parse over the same delta prefix). Across the gap the
    /// premise fails by construction — the old tail parsed around missing
    /// middle deltas — so when a flushed logical line spanned the hole its
    /// old rendering stays in scrollback uncorrected and the rebuilt
    /// (correct) line is suppressed: an accepted, marker-flagged
    /// presentation loss bounded to the lag window, next to the certain
    /// alternative of duplicating the whole prefix on every resync. Theme
    /// and depth play no part: the transfer measures lines, never rows.
    pub fn apply_sync(&mut self, sync: &Sync, width: u16, highlighter: &Highlighter, resync: bool) {
        let transfer = match (self.open_turn, self.blocks.last()) {
            (Some(turn), Some(Block::Agent(agent))) => Some((turn, agent.acked)),
            _ => None,
        };
        self.blocks.clear();
        self.flushed = 0;
        self.queued = 0;
        // The app's pre-sync pump drained the queue (replayed history must
        // not re-type — and the transfer below counts acked lines, so
        // undrained rows would be lost here); the rebuild starts it clean.
        self.queue.clear();
        self.open_turn = None;
        self.live_calls.clear();
        self.approvals.clear();
        self.as_of_seq = sync.as_of_seq;

        // The transport's message index includes seeded history. Provider
        // ids and even whole calls may repeat, so neither can locate a turn.
        // Partial decisions are durable too. Sync carries their positions,
        // not arrival order, so rebuild those records in original call order.
        let partial: Vec<SettledApproval> = sync
            .in_flight
            .pending_approvals
            .iter()
            .filter_map(partial_settlement)
            .collect();
        let mut placed: Vec<Vec<&SettledApproval>> = vec![Vec::new(); sync.history.messages.len()];
        let mut unplaced: Vec<&SettledApproval> = Vec::new();
        for settled in sync.settled_approvals.iter().chain(&partial) {
            let at = settled.message_index.filter(|&index| {
                sync.history
                    .messages
                    .get(index)
                    .is_some_and(|message| message.role == Role::Assistant)
            });
            match at {
                Some(at) => placed[at].push(settled),
                None => unplaced.push(settled),
            }
        }
        // Folded messages have no spans; their provider-id lookup must never
        // leak into the unique-span attribution of subsequent live events.
        let mut history_calls = HashMap::new();
        let pending_results = pending_result_names(sync);
        for (index, (message, settled)) in sync.history.messages.iter().zip(placed).enumerate() {
            self.push_history_message(
                message,
                &settled,
                &mut history_calls,
                pending_results.get(&index).map(String::as_str),
            );
        }
        for settled in unplaced {
            let names: Vec<String> = settled.calls.iter().map(|call| call.name.clone()).collect();
            self.blocks
                .push(Block::Static(resolution_lines(&names, &settled.decisions)));
        }
        // On a re-sync the rebuild lands pre-flushed: this transcript
        // already rendered the run's content and the pre-sync pump flushed
        // everything flushable, so the deterministic rebuild must not
        // re-enter scrollback (the transfer above is the one exception). A
        // first attach owns nothing of the baseline — the rebuild renders
        // block by block through the normal pump.
        if resync {
            self.flushed = self.blocks.len();
            self.queued = self.blocks.len();
        }
        self.sync_completions(&sync.in_flight.completed_tools, resync);
        if let Some(open) = &sync.in_flight.open_turn {
            self.blocks.push(Block::Agent(Agent {
                stream: {
                    let mut stream = Stream::new();
                    stream.push_delta(&open.partial.text);
                    stream
                },
                acked: 0,
                emitted: 0,
            }));
            self.open_turn = Some(open.turn);
            if let Some((turn, acked)) = transfer
                && turn == open.turn
                && acked > 0
            {
                let Some(Block::Agent(agent)) = self.blocks.last_mut() else {
                    unreachable!("the in-flight block was just pushed");
                };
                let flushable = agent.stream.render(width, highlighter).flushable_len();
                debug_assert!(
                    acked <= flushable,
                    "the transferred prefix ({acked}) must be flushable in the rebuilt tail \
                     ({flushable}): same deltas, deterministic parse"
                );
                let lines = acked.min(flushable);
                agent.stream.ack_flushed(lines);
                agent.acked = lines;
                // The transferred prefix is in scrollback already: it must
                // never re-queue.
                agent.emitted = lines;
            }
        }
        // The attach baseline seeds the approval names too: an attach
        // mid-wait replays the pending request(s) (ADR-0013 item 3), and
        // the recorded resolution names them.
        self.approvals
            .extend(sync.in_flight.pending_approvals.iter().map(|pending| {
                (
                    pending.request_id.clone(),
                    pending
                        .calls
                        .iter()
                        .enumerate()
                        .map(|(index, call)| {
                            if pending.decisions.get(index).is_some_and(Option::is_some) {
                                None
                            } else {
                                Some(call.name.clone())
                            }
                        })
                        .collect(),
                )
            }));
    }

    fn push_completion(&mut self, completion: &ToolCompletion) {
        if !self.completed_tools.insert(completion.span_id.clone()) {
            return;
        }
        // The live span supplies the target (the same lookup the error path
        // always made); after an attach the spans are gone and the label
        // falls back to the bare name.
        let target = self
            .live_calls
            .get(&completion.span_id)
            .and_then(|call| call.target.clone());
        let label = match &target {
            Some(target) => format!("{} {target}", completion.name),
            None => completion.name.clone(),
        };
        self.seal_open();
        if is_perception_tool(&completion.name) {
            if let Some(error) = &completion.error {
                let detail = error.message.lines().next().unwrap_or("failed");
                self.blocks.push(Block::Static(vec![error_line(format!(
                    "✗ {label}: {detail}"
                ))]));
            } else {
                self.blocks
                    .push(Block::Static(vec![perception_success_line(&label)]));
            }
        } else {
            self.blocks.push(Block::Static(completion_lines(
                completion,
                target.as_deref(),
            )));
        }
    }

    fn sync_completions(&mut self, completions: &[ToolCompletion], resync: bool) {
        let seen = std::mem::take(&mut self.completed_tools);
        // The old pump flushed everything before resync. Rebuild those
        // previews as pre-flushed, then expose any completions missed in the
        // gap. Keep both in replay without printing the old ones twice.
        if resync {
            for completion in completions
                .iter()
                .filter(|completion| seen.contains(&completion.span_id))
            {
                self.push_completion(completion);
            }
            self.flushed = self.blocks.len();
            self.queued = self.blocks.len();
        }
        for completion in completions {
            self.push_completion(completion);
        }
    }

    /// The client rule's current baseline (ADR-0013 item 4): the app
    /// consults it so the approval queue only takes requests the transcript
    /// will actually apply.
    #[must_use]
    pub fn as_of_seq(&self) -> u64 {
        self.as_of_seq
    }

    /// One materialization pass: append every newly stable row to the
    /// emission queue (the module docs own the contract). The walk resumes
    /// at the `queued` cursor; the open tail holds it (nothing may queue
    /// past an uncompleted block), and a sealed block's completing emission
    /// advances it.
    pub fn snapshot(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Snapshot {
        let tail = self.blocks.len().saturating_sub(1);
        let mut tail_live = false;
        while self.queued < self.blocks.len() {
            let index = self.queued;
            match &mut self.blocks[index] {
                Block::Static(lines) => {
                    let rows = wrap_rows(lines, width, theme, depth);
                    queue_emission(&mut self.queue, rows, FlushAck::WholeBlock);
                    self.queued += 1;
                }
                Block::Agent(agent) => {
                    let render = agent.stream.render(width, highlighter);
                    let live = render.live_lines();
                    let flushable = render.flushable_len();
                    let sealed = self.open_turn.is_none() || index != tail;
                    let completes = sealed && flushable == live.len();
                    tail_live = tail_live || flushable < live.len();
                    // The append cursor WITHIN the live lines: acked lines
                    // drop out of the pipeline's live list, so the
                    // queued-but-undrained prefix is the cumulative
                    // `emitted` minus `acked`. Per-line wrap independence:
                    // the prefix's rows are a row prefix of the whole — two
                    // disjoint wraps cost one.
                    let pending = agent.emitted - agent.acked;
                    let newly = flushable - pending;
                    agent.emitted = agent.acked + flushable;
                    if newly > 0 {
                        let rows = wrap_rows(&live[pending..flushable], width, theme, depth);
                        queue_emission(
                            &mut self.queue,
                            rows,
                            FlushAck::AgentLines {
                                lines: newly,
                                completes,
                            },
                        );
                    } else if completes {
                        // The seal completed a block whose lines were all
                        // queued already (an open fence's body): the
                        // completing ack rides a zero-row emission.
                        queue_emission(
                            &mut self.queue,
                            Vec::new(),
                            FlushAck::AgentLines {
                                lines: 0,
                                completes: true,
                            },
                        );
                    }
                    if completes {
                        self.queued += 1;
                    } else {
                        // The open tail holds the walk: nothing follows it.
                        break;
                    }
                }
            }
        }
        Snapshot { tail_live }
    }

    /// The queue's pending display rows — the pacing budget's depth input
    /// and the `receiving…` row's first disjunct.
    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.queue.iter().map(|emission| emission.rows.len()).sum()
    }

    /// Pop up to `budget` display rows from the queue (the paced drain).
    /// The returned acks cover exactly the emissions whose rows ALL left —
    /// the caller confirms them after a successful shell insert
    /// ([`Transcript::apply_flush`]), so a budget spent mid-emission holds
    /// its ack for the next drain.
    pub fn drain(&mut self, budget: usize) -> Drain {
        let mut rows = Vec::new();
        let mut acks = Vec::new();
        while rows.len() < budget {
            let Some(front) = self.queue.front_mut() else {
                break;
            };
            while rows.len() < budget
                && let Some(row) = front.rows.pop_front()
            {
                rows.push(row);
                front.drained = true;
            }
            if front.rows.is_empty() {
                acks.push(self.queue.pop_front().expect("the front emission").ack);
            }
        }
        Drain { rows, acks }
    }

    /// The width-change rewind: queued rows carry the old width's wrap, so
    /// the queue folds back into its blocks and the next snapshot re-queues
    /// at the new width. Sound because nothing queued is acked yet (the
    /// drain owns the ack contract) — what already drained stays in
    /// scrollback.
    ///
    /// The one exception is a partially drained front emission: its
    /// already-inserted prefix is scrollback stock ratatui cannot delete,
    /// so its remaining rows KEEP the old wrap (re-queuing them would
    /// duplicate that prefix) — the accepted, bounded, cosmetic cost. The
    /// front belongs to block `flushed` (a block completes only with its
    /// last emission, so every earlier block is fully drained); its ack
    /// says how many of the block's queued lines the kept slice covers —
    /// the rest of the block's queued lines fold back (later emissions of
    /// the SAME block included), and the cursor skips the block exactly
    /// when the kept slice finishes it (a static block, or an agent block
    /// whose completing ack the kept slice carries). Otherwise the
    /// snapshot re-evaluates it, so a completing ack re-queues if the
    /// block sealed since.
    pub fn rewind_queue(&mut self) {
        // The first block whose append cursor folds back wholesale: every
        // block after the kept slice's.
        let first = if self.queue.front().is_some_and(|front| front.drained) {
            let front = self.queue.pop_front().expect("the front emission");
            self.queue.clear();
            self.queued = match front.ack {
                // A static block's kept rows are its remainder: the cursor
                // skips it.
                FlushAck::WholeBlock => self.flushed + 1,
                FlushAck::AgentLines { lines, completes } => {
                    // The kept slice covers `lines` of this block's queued
                    // prefix; the rest of its queued lines fold back.
                    let Block::Agent(agent) = &mut self.blocks[self.flushed] else {
                        unreachable!("an agent ack implies an agent block");
                    };
                    agent.emitted = agent.acked + lines;
                    if completes {
                        self.flushed + 1
                    } else {
                        self.flushed
                    }
                }
            };
            self.queue.push_back(front);
            self.flushed + 1
        } else {
            self.queue.clear();
            self.queued = self.flushed;
            self.flushed
        };
        for block in &mut self.blocks[first..] {
            if let Block::Agent(agent) = block {
                agent.emitted = agent.acked;
            }
        }
    }

    /// Confirm a drain's flush plan (a successful shell insert).
    pub fn apply_flush(&mut self, acks: &[FlushAck]) {
        let mut index = self.flushed;
        for ack in acks {
            match ack {
                FlushAck::WholeBlock => index += 1,
                FlushAck::AgentLines { lines, completes } => {
                    let Block::Agent(agent) = &mut self.blocks[index] else {
                        unreachable!("the flush plan is aligned with the unflushed blocks");
                    };
                    agent.stream.ack_flushed(*lines);
                    agent.acked += lines;
                    if *completes {
                        index += 1;
                    }
                }
            }
        }
        self.flushed = index;
    }

    /// Resize replay (the shell's `on_resize` closure): the flushed history
    /// tail re-materialized from source at the new width, up to `max_rows`
    /// display rows, newest last. The live tail is excluded — it comes back
    /// with the band's own repaint. Per block: flushed static blocks replay
    /// whole; assistant blocks replay their flushed prefix (the committed
    /// document minus the still-live lines), which for a fully flushed block
    /// is everything. Blocks are walked newest-first and the walk stops once
    /// the window is full — a session's age never inflates a shrink replay.
    pub fn replay_tail(
        &mut self,
        max_rows: u16,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Vec<Line<'static>> {
        let mut rows: Vec<Line<'static>> = Vec::new();
        for (index, block) in self.blocks.iter_mut().enumerate().rev() {
            if rows.len() >= usize::from(max_rows) {
                break;
            }
            let mut block_rows = match block {
                Block::Static(lines) if index < self.flushed => {
                    wrap_rows(lines, width, theme, depth)
                }
                Block::Static(_) => Vec::new(),
                Block::Agent(agent) => {
                    agent
                        .stream
                        .replay_tail(u16::MAX, width, highlighter, theme, depth)
                }
            };
            // Keep only what still fits, then prepend (blocks older than the
            // window's edge render fully — per-block work is the floor).
            let room = usize::from(max_rows) - rows.len();
            block_rows = Stream::visible(&block_rows, u16::try_from(room).unwrap_or(u16::MAX));
            block_rows.extend(rows);
            rows = block_rows;
        }
        rows
    }

    /// The turn's assistant block, creating it on the turn's first delta;
    /// a different open turn is sealed structurally first (module docs).
    fn agent_block(&mut self, turn: u32) -> &mut Agent {
        if self.open_turn != Some(turn) {
            self.seal_open();
            self.blocks.push(Block::Agent(Agent {
                stream: Stream::new(),
                acked: 0,
                emitted: 0,
            }));
            self.open_turn = Some(turn);
        }
        let Some(Block::Agent(agent)) = self.blocks.last_mut() else {
            unreachable!("the block was just pushed");
        };
        agent
    }

    /// Seal the open assistant block with its own buffered source (the
    /// structural seal — deltas reconcile exactly with the response, so a
    /// saturated transport cannot truncate the transcript).
    fn seal_open(&mut self) {
        if self.open_turn.take().is_some()
            && let Some(Block::Agent(agent)) = self.blocks.last_mut()
        {
            let source = agent.stream.source().to_string();
            agent.stream.finalize(&source);
        }
    }

    fn apply_tool_call(&mut self, event: &Event, call: &cadmus_contract::ToolCall) -> Light {
        self.seal_open();
        let target = tool_target(call);
        self.live_calls.insert(
            event.span_id.clone(),
            LiveCall {
                name: call.name.clone(),
                target: target.clone(),
            },
        );
        // Perception calls render no marker: the run-status row carries the
        // live signal, and a success leaves its one quiet line at completion
        // (the module docs own the policy).
        if !is_perception_tool(&call.name) {
            self.blocks.push(Block::Static(vec![tool_call_line(call)]));
        }
        Light::Tool {
            name: call.name.clone(),
            target,
        }
    }

    fn apply_event(&mut self, event: &Event) -> Light {
        match &event.kind {
            EventKind::LlmResponse { message, .. } => {
                let text = message.text_body();
                let turn = event.turn();
                if turn.is_some() && turn == self.open_turn {
                    // The response is authoritative over the delta buffer.
                    let Some(Block::Agent(agent)) = self.blocks.last_mut() else {
                        unreachable!("an open turn means a tail assistant block");
                    };
                    agent.stream.finalize(&text);
                    self.open_turn = None;
                } else if !text.is_empty() {
                    // Defensive: a response whose deltas never arrived.
                    self.seal_open();
                    self.blocks.push(Block::Agent(Agent {
                        stream: {
                            let mut stream = Stream::new();
                            stream.push_delta(&text);
                            stream.finalize(&text);
                            stream
                        },
                        acked: 0,
                        emitted: 0,
                    }));
                }
                Light::Streaming
            }
            EventKind::ToolCall { call } => self.apply_tool_call(event, call),
            EventKind::ToolResult { call_id, result } => {
                let text = result.as_str();
                let display: &dyn std::fmt::Display = match &text {
                    Some(text) => text,
                    None => result,
                };
                self.push_result(event, call_id, display);
                Light::None
            }
            EventKind::InstructionInjected { path, .. } => {
                // Every Static push seals first: only the tail block may be an
                // open Agent, or the next delta for the same turn would land
                // after this one and hit `agent_block`'s unreachable arm.
                self.seal_open();
                self.blocks
                    .push(Block::Static(vec![instruction_line(path)]));
                Light::None
            }
            EventKind::Fold { folded, .. } => {
                self.seal_open();
                self.blocks
                    .push(Block::Static(vec![fold_line(folded.len())]));
                Light::None
            }
            EventKind::Command(Command::Steer { text, .. }) => {
                // A steer lands as a user message at application (ADR-0013's
                // record-on-effect rule); render it like a prompt.
                self.push_user(text);
                Light::None
            }
            EventKind::Command(Command::ResolveApproval {
                request_id,
                decisions,
                ..
            }) => {
                self.push_resolution(request_id, decisions);
                Light::None
            }
            EventKind::Command(Command::ResolveApprovalCall {
                request_id,
                call_index,
                decision,
                ..
            }) => {
                self.push_call_resolution(request_id, *call_index, decision);
                Light::None
            }
            EventKind::RunFinished { .. } => {
                self.live_calls.clear();
                self.approvals.clear();
                self.completed_tools.clear();
                if event.status == Status::Error {
                    let detail = event
                        .error
                        .as_ref()
                        .and_then(|error| error.message.lines().next())
                        .unwrap_or("failed");
                    self.seal_open();
                    self.blocks.push(Block::Static(vec![error_line(format!(
                        "run failed: {detail}"
                    ))]));
                    Light::Failed
                } else {
                    Light::Idle
                }
            }
            _ => Light::None,
        }
    }

    fn push_result(&mut self, event: &Event, call_id: &str, result: &dyn std::fmt::Display) {
        let call_info = self.live_calls.remove(&event.span_id);
        if self.completed_tools.remove(&event.span_id) {
            return;
        }
        // Attach may have missed the opening span. A provider id alone cannot
        // verify a successful result's label; legacy errors retain their fallback.
        if call_info.is_none() && event.status != Status::Error {
            return;
        }
        let (tool_name, tool_target) = match &call_info {
            Some(info) => (info.name.as_str(), info.target.as_deref()),
            None => (call_id, None),
        };
        let label = match tool_target {
            Some(target) => format!("{tool_name} {target}"),
            None => tool_name.to_string(),
        };
        self.seal_open();
        if event.status == Status::Error {
            let detail = event
                .error
                .as_ref()
                .and_then(|error| error.message.lines().next())
                .unwrap_or("failed");
            self.blocks.push(Block::Static(vec![error_line(format!(
                "✗ {label}: {detail}"
            ))]));
        } else if is_perception_tool(tool_name) {
            self.blocks
                .push(Block::Static(vec![perception_success_line(&label)]));
        } else {
            self.blocks
                .push(Block::Static(outcome_lines(&label, result, None, false)));
        }
    }

    /// The recorded resolution of one approval request: the durable half of
    /// the approval (the request was live-only). A resolve the transcript
    /// never saw a request for (a lag hole) names the request id itself.
    fn push_resolution(&mut self, request_id: &str, decisions: &[Approval]) {
        let slots = self
            .approvals
            .remove(request_id)
            .unwrap_or_else(|| vec![Some(request_id.to_string())]);
        let (names, decisions): (Vec<_>, Vec<_>) = slots
            .into_iter()
            .enumerate()
            .filter_map(|(index, name)| {
                name.map(|name| {
                    (
                        name,
                        decisions
                            .get(index)
                            .cloned()
                            .unwrap_or(Approval::Rejected { comment: None }),
                    )
                })
            })
            .unzip();
        self.seal_open();
        let lines = resolution_lines(&names, &decisions);
        if !lines.is_empty() {
            self.blocks.push(Block::Static(lines));
        }
    }

    fn push_call_resolution(&mut self, request_id: &str, call_index: usize, decision: &Approval) {
        let Some(slots) = self.approvals.get_mut(request_id) else {
            return;
        };
        let Some(name) = slots.get_mut(call_index).and_then(Option::take) else {
            return;
        };
        if slots.iter().all(Option::is_none) {
            self.approvals.remove(request_id);
        }
        self.seal_open();
        self.blocks.push(Block::Static(resolution_lines(
            &[name],
            std::slice::from_ref(decision),
        )));
    }

    /// History rebuild for [`Transcript::apply_sync`]: messages map onto the
    /// same block shapes the live path builds (deterministic fold).
    /// `settled` carries the approval batches whose calls this message
    /// proposed — their resolution lines precede its tool markers, the
    /// live path's order.
    fn push_history_message(
        &mut self,
        message: &Message,
        settled: &[&SettledApproval],
        history_calls: &mut HashMap<String, HistoryCall>,
        preview_tool: Option<&str>,
    ) {
        match message.role {
            Role::User => self.push_user(&message.text_body()),
            Role::Assistant => {
                let text = message.text_body();
                if !text.is_empty() {
                    self.blocks.push(Block::Agent(Agent {
                        stream: {
                            let mut stream = Stream::new();
                            stream.push_delta(&text);
                            stream.finalize(&text);
                            stream
                        },
                        acked: 0,
                        emitted: 0,
                    }));
                }
                for settled in settled {
                    let names: Vec<String> =
                        settled.calls.iter().map(|call| call.name.clone()).collect();
                    self.blocks
                        .push(Block::Static(resolution_lines(&names, &settled.decisions)));
                }
                for call in message.tool_calls() {
                    let label = match tool_target(call) {
                        Some(target) => format!("{} {target}", call.name),
                        None => call.name.clone(),
                    };
                    history_calls.insert(
                        call.id.clone(),
                        HistoryCall {
                            label,
                            perception: is_perception_tool(&call.name),
                        },
                    );

                    if !is_perception_tool(&call.name) {
                        self.blocks.push(Block::Static(vec![tool_call_line(call)]));
                    }
                }
            }
            Role::Tool => {
                if let Some(tool) = preview_tool {
                    self.blocks.push(Block::Static(outcome_lines(
                        tool,
                        &HistoryResult(message),
                        None,
                        message.is_error,
                    )));
                } else if message.is_error {
                    let call_id = message.tool_call_id.as_deref().unwrap_or("?");
                    let tool = history_calls
                        .get(call_id)
                        .map_or(call_id, |call| call.label.as_str());
                    let detail = message.text_body();
                    let detail = detail.lines().next().unwrap_or("failed");
                    self.blocks.push(Block::Static(vec![error_line(format!(
                        "✗ {tool}: {detail}"
                    ))]));
                } else {
                    // Every durable success lands (the module docs own the
                    // policy). The folded call's provider id is the only
                    // attribution left; as in the live path, a success whose
                    // label cannot be verified stays quiet.
                    let call_id = message.tool_call_id.as_deref().unwrap_or("?");
                    let Some(call) = history_calls.get(call_id) else {
                        return;
                    };
                    if call.perception {
                        self.blocks
                            .push(Block::Static(vec![perception_success_line(&call.label)]));
                    } else {
                        self.blocks.push(Block::Static(outcome_lines(
                            &call.label,
                            &HistoryResult(message),
                            None,
                            false,
                        )));
                    }
                }
            }
            Role::System => {}
        }
    }
}

/// A folded call's display facts, keyed by provider id in the history
/// rebuild: the marker label (name plus target when known) and whether the
/// call is a perception tool, whose success renders one quiet line, never a
/// preview.
struct HistoryCall {
    label: String,
    perception: bool,
}

/// Match `Message::text_body` without copying an entire result before the
/// preview budget can stop formatting it.
struct HistoryResult<'a>(&'a Message);

impl std::fmt::Display for HistoryResult<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut separator = "";
        for part in &self.0.content {
            if let cadmus_contract::ContentPart::Text { text } = part {
                formatter.write_str(separator)?;
                formatter.write_str(text)?;
                separator = "\n";
            }
        }
        Ok(())
    }
}

/// Interrupt can skip unanswered leading calls before recording later results.
/// Only the durable approval address verifies a result's label; neither result
/// order nor provider ids can. The request's anchor confines it to the
/// originating turn.
fn pending_result_names(sync: &Sync) -> HashMap<usize, String> {
    let mut names = HashMap::new();
    let messages = &sync.history.messages;
    for pending in &sync.in_flight.pending_approvals {
        if !(0..pending.calls.len())
            .any(|index| pending.decisions.get(index).is_none_or(Option::is_none))
        {
            continue;
        }
        let Some(anchor) = pending.message_index.filter(|&index| {
            messages
                .get(index)
                .is_some_and(|message| message.role == Role::Assistant)
        }) else {
            continue;
        };
        let end = messages
            .iter()
            .enumerate()
            .skip(anchor + 1)
            .find(|(_, message)| message.role == Role::Assistant)
            .map_or(messages.len(), |(index, _)| index);
        for result in &sync.history.tool_results {
            if result.request_id != pending.request_id
                || result.message_index <= anchor
                || result.message_index >= end
                || messages[result.message_index].role != Role::Tool
                || pending
                    .decisions
                    .get(result.call_index)
                    .is_none_or(Option::is_none)
            {
                continue;
            }
            if let Some(call) = pending.calls.get(result.call_index) {
                let label = match tool_target(call) {
                    Some(target) => format!("{} {target}", call.name),
                    None => call.name.clone(),
                };
                names.insert(result.message_index, label);
            }
        }
    }
    names
}

fn completion_lines(completion: &ToolCompletion, target: Option<&str>) -> Vec<ir::Line> {
    let name = truncate_cells(&completion.name, 48);
    let label = match target {
        Some(target) => format!("{name} {target}"),
        None => name,
    };
    let text = completion.result.as_str();
    let display: &dyn std::fmt::Display = match &text {
        Some(text) => text,
        None => &completion.result,
    };
    outcome_lines(
        &label,
        display,
        completion.error.as_ref(),
        completion.error.is_some(),
    )
}

/// The one history line a successful perception call leaves: the check names
/// what was inspected; the content itself stays out (the module docs own the
/// policy).
fn perception_success_line(label: &str) -> ir::Line {
    ir::Line::from_spans(vec![
        ir::Span::slotted("✓ ", Slot::Success),
        ir::Span::slotted(label, Slot::TextSubtle),
    ])
}

/// Both early and durable results share the same bounded preview. A durable
/// result lacks a full-batch index, so its caller supplies only a tool label.
fn outcome_lines(
    label: &str,
    result: &dyn std::fmt::Display,
    error: Option<&EventError>,
    failed: bool,
) -> Vec<ir::Line> {
    let header = if failed {
        ir::Line::from_spans(vec![
            ir::Span::slotted("✗ ", Slot::Error),
            ir::Span::slotted(label, Slot::Error),
        ])
    } else {
        ir::Line::from_spans(vec![
            ir::Span::slotted("✓ ", Slot::Success),
            ir::Span::slotted(label, Slot::Text),
        ])
    };
    let mut lines = vec![header];
    let mut preview = CompletionPreview::default();
    if let Some(error) = error {
        let _ = writeln!(preview, "{}: {}", error.kind, error.message);
    }
    let _ = write!(preview, "{result}");
    // Control bytes are untrusted tool output, not terminal instructions.
    let text: String = preview
        .text
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .collect();
    let total_lines = text.lines().count();
    lines.extend(text.lines().take(4).map(|line| {
        ir::Line::from_spans(vec![
            ir::Span::slotted("│ ", Slot::TextSubtle),
            ir::Span::slotted(truncate_cells(line, 120), Slot::TextSubtle),
        ])
    }));
    if total_lines > 4 {
        let count = total_lines - 4;
        lines.push(ir::Line::slotted(
            format!("│ ⋯ {count} more lines truncated"),
            Slot::TextSubtle,
        ));
    } else if preview.truncated {
        lines.push(ir::Line::slotted(
            "│ ⋯ result preview truncated",
            Slot::TextSubtle,
        ));
    }
    lines
}

const COMPLETION_PREVIEW_BYTES: usize = 512;

#[derive(Default)]
struct CompletionPreview {
    text: String,
    truncated: bool,
}

impl std::fmt::Write for CompletionPreview {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.truncated {
            return Err(std::fmt::Error);
        }
        let remaining = COMPLETION_PREVIEW_BYTES - self.text.len();
        if text.len() <= remaining {
            self.text.push_str(text);
            return Ok(());
        }
        let mut end = remaining;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        self.truncated = true;
        Err(std::fmt::Error)
    }
}

/// Adapt the decided subset for the same history-placement path as a
/// complete settlement; absent slots remain pending, never implicit denials.
fn partial_settlement(pending: &PendingApproval) -> Option<SettledApproval> {
    let (calls, decisions): (Vec<_>, Vec<_>) = pending
        .calls
        .iter()
        .zip(&pending.decisions)
        .filter_map(|(call, decision)| {
            decision
                .as_ref()
                .map(|decision| (call.clone(), decision.clone()))
        })
        .unzip();
    (!calls.is_empty()).then(|| SettledApproval {
        request_id: pending.request_id.clone(),
        message_index: pending.message_index,
        calls,
        decisions,
    })
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// A quiet activity/marker line.
pub(crate) fn subtle_line(text: impl Into<String>) -> ir::Line {
    ir::Line::from_spans(vec![ir::Span::slotted(text, Slot::TextSubtle)])
}

/// The run-completion note: `Worked for {elapsed}` — a run's wall-clock
/// cost as a permanent quiet row in scrollback (Codex's
/// `FinalMessageSeparator` precedent), the band run-status row's durable
/// twin. The app formats `elapsed` (its `format_elapsed` is the one home,
/// shared with the band row); the wording and slot live here.
pub(crate) fn worked_for_line(elapsed: &str) -> ir::Line {
    subtle_line(format!("Worked for {elapsed}"))
}

/// The failure twin of [`worked_for_line`], in the error slot.
pub(crate) fn failed_after_line(elapsed: &str) -> ir::Line {
    error_line(format!("Failed after {elapsed}"))
}

/// The tool-activity marker: the name plus the call's primary target, so a
/// run of same-name calls stays distinguishable (field report 2026-09-16:
/// 23 bare `→ list_dir` rows read as duplicates). The marker is one quiet
/// line, never a table — the expandable transcript view is the approval/diff
/// slice's. `pub(crate)`: the approval section's call lines mirror the shape
/// (one home per fact — the marker format lives here).
pub(crate) fn tool_marker(call: &cadmus_contract::ToolCall) -> String {
    match tool_target(call) {
        Some(target) => format!("▸ {} {target}", call.name),
        None => format!("▸ {}", call.name),
    }
}

/// A structured tool call line with semantic slot styling.
pub(crate) fn tool_call_line(call: &cadmus_contract::ToolCall) -> ir::Line {
    let mut spans = vec![
        ir::Span::slotted("▸ ", Slot::Accent),
        ir::Span::slotted(&call.name, Slot::Accent),
    ];
    if let Some(target) = tool_target(call) {
        spans.push(ir::Span::slotted(format!(" {target}"), Slot::TextSubtle));
    }
    ir::Line::from_spans(spans)
}

/// The marker's target: the first non-empty string among the keys the coding
/// tools use for their subject (`pattern` before `path` — grep's subject is
/// the pattern, its path the scope), first line only, width-capped.
fn tool_target(call: &cadmus_contract::ToolCall) -> Option<String> {
    let object = call.arguments.as_object()?;
    for key in ["pattern", "command", "path", "query", "file_path"] {
        if let Some(value) = object.get(key).and_then(|value| value.as_str()) {
            let first_line = value.lines().next().unwrap_or_default();
            if !first_line.is_empty() {
                return Some(truncate_cells(first_line, 48));
            }
        }
    }
    None
}

/// Grapheme-wise truncation to a display-cell budget, ellipsis on cut — the
/// marker never wraps a target across rows. `pub(crate)`: the band's
/// run-status row truncates its tool target by the same rule (one home).
pub(crate) fn truncate_cells(text: &str, max: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut kept = String::new();
    let mut width = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max {
            return format!("{kept}…");
        }
        kept.push_str(grapheme);
        width += grapheme_width;
    }
    kept
}

fn instruction_line(path: &str) -> ir::Line {
    ir::Line::from_spans(vec![
        ir::Span::slotted("ℹ ", Slot::Info),
        ir::Span::slotted(format!("instructions: {path}"), Slot::TextSubtle),
    ])
}

fn fold_line(count: usize) -> ir::Line {
    ir::Line::from_spans(vec![
        ir::Span::slotted("⑃ ", Slot::Info),
        ir::Span::slotted(
            format!("context folded: {count} result(s) compressed"),
            Slot::TextSubtle,
        ),
    ])
}

/// A failure marker line.
fn error_line(text: impl Into<String>) -> ir::Line {
    ir::Line::from_spans(vec![ir::Span::slotted(text, Slot::Error)])
}

/// The resolution's display lines, shared by the live path and the attach
/// rebuild: approved calls in the success slot, rejected calls in the error slot.
/// A missing decision is a denial (the gate's short-reply rule), so it reads
/// as a rejection.
fn resolution_lines(names: &[String], decisions: &[Approval]) -> Vec<ir::Line> {
    let mut approved = Vec::new();
    let mut rejected = Vec::new();
    for (index, name) in names.iter().enumerate() {
        match decisions.get(index) {
            Some(Approval::Approved) => approved.push(name.as_str()),
            _ => rejected.push(name.as_str()),
        }
    }
    let mut lines = Vec::new();
    if !approved.is_empty() {
        lines.push(ir::Line::slotted(
            format!("✓ approved: {}", approved.join(", ")),
            Slot::Success,
        ));
    }
    if !rejected.is_empty() {
        lines.push(ir::Line::slotted(
            format!("✗ rejected: {}", rejected.join(", ")),
            Slot::Error,
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use cadmus_contract::{
        EventError, InFlight, LiveKind, OpenTurn, RunState, StreamChunk, ToolCall,
        ToolResultProjection, TurnSnapshot, attrs,
    };

    use super::*;
    use crate::test_util::{highlighter, texts};

    fn delta(seq: u64, turn: u32, text: &str) -> LiveItem {
        LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::AssistantDelta {
                turn,
                chunk: StreamChunk::TextDelta(text.into()),
            },
        }
    }

    fn recorded(seq: u64, turn: u32, kind: EventKind) -> LiveItem {
        LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::Recorded {
                event: Box::new(
                    Event::new(
                        seq,
                        format!("ev-{seq}"),
                        "tr-test".into(),
                        "sp-1".into(),
                        None,
                        0,
                        kind,
                    )
                    .with_attribute(attrs::TURN, u64::from(turn)),
                ),
            },
        }
    }

    fn llm_response(seq: u64, turn: u32, text: &str) -> LiveItem {
        recorded(
            seq,
            turn,
            EventKind::LlmResponse {
                message: Message::text(Role::Assistant, text),
                usage: None,
                finish: cadmus_contract::FinishReason::Stop,
                outcome: cadmus_contract::TurnOutcome::Content,
                warnings: Vec::new(),
            },
        )
    }

    /// An attach baseline over the given history and settled window — the
    /// two inputs the rebuild placement consumes.
    fn sync_with(messages: Vec<Message>, settled: Vec<SettledApproval>) -> Sync {
        Sync {
            history: RunState {
                trace_id: "tr-test".into(),
                provider: None,
                model: None,
                messages,
                tool_results: Vec::new(),
                turns: 0,
                warnings: Vec::new(),
                scores: Vec::new(),
                dangling_tool_calls: Vec::new(),
                finished: None,
            },
            in_flight: InFlight {
                open_turn: None,
                pending_approvals: Vec::new(),
                completed_tools: Vec::new(),
            },
            settled_approvals: settled,
            as_of_seq: 0,
        }
    }

    fn snapshot(transcript: &mut Transcript) -> Snapshot {
        transcript.snapshot(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor)
    }

    /// One pump: queue the newly-stable rows, drain them all (pacing is the
    /// app's — the transcript's tests drain instantly), ack, and return the
    /// drained rows as text plus the liveness flag (the unstable tail's
    /// existence — the tail itself is never rendered).
    fn pump(transcript: &mut Transcript) -> (Vec<String>, bool) {
        let snapshot = snapshot(transcript);
        let drain = transcript.drain(usize::MAX);
        transcript.apply_flush(&drain.acks);
        (texts(&drain.rows), snapshot.tail_live)
    }

    /// Queue and drain everything, returning the rows with styles kept.
    fn drained_rows(transcript: &mut Transcript) -> Vec<Line<'static>> {
        snapshot(transcript);
        let drain = transcript.drain(usize::MAX);
        transcript.apply_flush(&drain.acks);
        drain.rows
    }

    #[test]
    fn the_user_prompt_marker_uses_the_accent_slot() {
        let mut transcript = Transcript::new();
        transcript.push_user("hi");
        let rows = drained_rows(&mut transcript);
        let row = &rows[0];
        assert_eq!(row.spans[0].content, "❯ ");
        assert_eq!(
            row.spans[0].style.fg,
            Some(ratatui::style::Color::Blue),
            "the accent slot resolves to the named blue at any depth"
        );
    }

    #[test]
    fn a_prompt_and_a_sealed_turn_flush_whole() {
        let mut transcript = Transcript::new();
        transcript.push_user("fix the bug");
        assert_eq!(
            transcript.apply_item(&delta(1, 1, "done\n")),
            Light::Streaming
        );
        transcript.apply_item(&llm_response(2, 1, "done\n"));
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["❯ fix the bug", "", "done"]);
        assert!(!tail_live);
        // Nothing left: a second pump flushes nothing.
        let (flushed, tail_live) = pump(&mut transcript);
        assert!(flushed.is_empty() && !tail_live);
    }

    #[test]
    fn the_open_tail_holds_its_incomplete_rows() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "one\n\ntwo\n"));
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["one", ""]);
        // The held row stays unstable — never rendered, only signaled.
        assert!(tail_live);
        // The held row flushes once the response seals the turn.
        transcript.apply_item(&llm_response(2, 1, "one\n\ntwo\n"));
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["two"]);
        assert!(!tail_live);
    }

    #[test]
    fn tool_activity_sequences_between_turns() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "reading\n"));
        transcript.apply_item(&llm_response(2, 1, "reading\n"));
        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({}),
        };
        assert_eq!(
            transcript.apply_item(&recorded(3, 1, EventKind::ToolCall { call: call.clone() })),
            Light::Tool {
                name: "write_file".into(),
                target: None,
            }
        );
        transcript.apply_item(&recorded(
            4,
            1,
            EventKind::ToolResult {
                call_id: "c1".into(),
                result: serde_json::json!("boom"),
            },
        ));
        transcript.apply_item(&delta(5, 2, "found it\n"));
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec!["reading", "▸ write_file", "✓ write_file", "│ boom"]
        );
        assert!(tail_live);
    }

    #[test]
    fn the_tool_marker_names_its_target() {
        let mut transcript = Transcript::new();
        for (seq, call) in [
            ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/cursor.rs", "offset": 10}),
            },
            // grep's subject is the pattern; the path is its scope.
            ToolCall {
                id: "c2".into(),
                name: "grep".into(),
                arguments: serde_json::json!({"path": "crates", "pattern": "max_tokens"}),
            },
            ToolCall {
                id: "c3".into(),
                name: "list_dir".into(),
                arguments: serde_json::json!({}),
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, call)| (index as u64 + 1, call))
        {
            transcript.apply_item(&recorded(seq, 1, EventKind::ToolCall { call }));
        }
        let (flushed, tail_live) = pump(&mut transcript);
        assert!(
            flushed.is_empty(),
            "perception tools stay in dynamic status only"
        );
        assert!(!tail_live);
    }

    #[test]
    fn the_action_tool_marker_names_its_target() {
        let mut transcript = Transcript::new();
        for (seq, call) in [
            ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "src/cursor.rs", "content": "test"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "cargo check"}),
            },
            ToolCall {
                id: "c3".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({"path": "crates/lib.rs"}),
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(index, call)| (index as u64 + 1, call))
        {
            transcript.apply_item(&recorded(seq, 1, EventKind::ToolCall { call }));
        }
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec![
                "▸ write_file src/cursor.rs",
                "▸ bash cargo check",
                "▸ edit_file crates/lib.rs",
            ]
        );
        assert!(!tail_live);
    }

    #[test]
    fn exploratory_turn_perception_calls_render_no_markers() {
        let mut transcript = Transcript::new();
        let mut calls = Vec::new();
        // 5 files read
        for i in 1..=5 {
            calls.push(ToolCall {
                id: format!("rf{i}"),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": format!("file_{i}.rs")}),
            });
        }
        // 2 directories listed
        for i in 1..=2 {
            calls.push(ToolCall {
                id: format!("ld{i}"),
                name: "list_dir".into(),
                arguments: serde_json::json!({"path": format!("dir_{i}")}),
            });
        }
        // 1 search
        calls.push(ToolCall {
            id: "gr1".into(),
            name: "grep".into(),
            arguments: serde_json::json!({"pattern": "fn main"}),
        });

        for (index, call) in calls.into_iter().enumerate() {
            transcript.apply_item(&recorded(index as u64 + 1, 1, EventKind::ToolCall { call }));
        }
        let (flushed, tail_live) = pump(&mut transcript);
        assert!(
            flushed.is_empty(),
            "perception calls render no marker lines (the run-status row carries them)"
        );
        assert!(!tail_live);
    }

    #[test]
    fn failed_perception_tool_emits_explicit_error_line() {
        let mut transcript = Transcript::new();
        let call = ToolCall {
            id: "c1".into(),
            name: "list_dir".into(),
            arguments: serde_json::json!({"path": ".git/opencode"}),
        };
        transcript.apply_item(&recorded(1, 1, EventKind::ToolCall { call }));
        let mut item = recorded(
            2,
            1,
            EventKind::ToolResult {
                call_id: "c1".into(),
                result: serde_json::json!("failed"),
            },
        );
        if let LiveKind::Recorded { event } = &mut item.kind {
            event.status = Status::Error;
            event.error = Some(EventError {
                kind: "fs".into(),
                message: "Not a directory".into(),
            });
        }
        transcript.apply_item(&item);
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✗ list_dir .git/opencode: Not a directory"]);
        for row in &flushed {
            assert!(!row.contains("turn"));
            assert!(!row.contains("tool call"));
        }
    }

    #[test]
    fn perception_outcomes_each_render_exactly_one_line() {
        let mut transcript = Transcript::new();
        // 2 successful read_file calls: one subtle line each, no marker, no preview.
        for (seq, path) in [(1_u64, "src/1.rs"), (3, "src/2.rs")] {
            transcript.apply_item(&tool_started(
                seq,
                &format!("sp-{seq}"),
                ToolCall {
                    id: format!("rf{seq}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": path}),
                },
            ));
            let mut done = completion(&format!("sp-{seq}"));
            done.name = "read_file".into();
            done.result = serde_json::json!("file contents");
            transcript.apply_item(&durable_completion(seq + 1, &done));
        }
        // 1 list_dir that fails: the unchanged one-line error.
        transcript.apply_item(&tool_started(
            5,
            "sp-5",
            ToolCall {
                id: "c_fail".into(),
                name: "list_dir".into(),
                arguments: serde_json::json!({"path": ".git/opencode"}),
            },
        ));
        let mut item = recorded(
            6,
            1,
            EventKind::ToolResult {
                call_id: "c_fail".into(),
                result: serde_json::json!("failed"),
            },
        );
        if let LiveKind::Recorded { event } = &mut item.kind {
            event.span_id = "sp-5".into();
            event.status = Status::Error;
            event.error = Some(EventError {
                kind: "fs".into(),
                message: "Not a directory".into(),
            });
        }
        transcript.apply_item(&item);
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec![
                "✓ read_file src/1.rs",
                "✓ read_file src/2.rs",
                "✗ list_dir .git/opencode: Not a directory",
            ]
        );
    }

    #[test]
    fn interleaved_action_and_perception_tools() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&recorded(
            1,
            1,
            EventKind::ToolCall {
                call: ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
            },
        ));
        transcript.apply_item(&recorded(
            2,
            1,
            EventKind::ToolCall {
                call: ToolCall {
                    id: "c2".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "src/out.rs"}),
                },
            },
        ));
        transcript.apply_item(&recorded(
            3,
            1,
            EventKind::ToolCall {
                call: ToolCall {
                    id: "c3".into(),
                    name: "list_dir".into(),
                    arguments: serde_json::json!({"path": "src"}),
                },
            },
        ));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["▸ write_file src/out.rs",]);
    }

    #[test]
    fn history_rebuild_renders_no_markers_for_perception_calls() {
        let calls = [
            ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            },
            ToolCall {
                id: "c2".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/lib.rs"}),
            },
            ToolCall {
                id: "c3".into(),
                name: "list_dir".into(),
                arguments: serde_json::json!({"path": "crates"}),
            },
            ToolCall {
                id: "c4".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "src/out.rs"}),
            },
        ];
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                cadmus_contract::ContentPart::Text {
                    text: "exploring".into(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[0].clone(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[1].clone(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[2].clone(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[3].clone(),
                },
            ],
            tool_call_id: None,
            is_error: false,
            opaque: None,
        };
        let sync = sync_with(
            vec![Message::user("find and update"), assistant],
            Vec::new(),
        );
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        assert_eq!(
            rows,
            vec![
                "❯ find and update",
                "",
                "exploring",
                "▸ write_file src/out.rs",
            ]
        );
    }

    #[test]
    fn perception_tool_completion_error_emits_error_line() {
        let mut transcript = Transcript::new();
        let mut failed = completion("span-fail");
        failed.name = "list_dir".into();
        failed.error = Some(EventError {
            kind: "fs".into(),
            message: "Not a directory".into(),
        });
        transcript.apply_item(&completed(1, failed));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✗ list_dir: Not a directory"]);
    }

    #[test]
    fn the_marker_truncates_a_long_target() {
        let target = format!("crates/{}", "very-long-directory-name/".repeat(8));
        let marker = tool_marker(&ToolCall {
            id: "c1".into(),
            name: "list_dir".into(),
            arguments: serde_json::json!({"path": target}),
        });
        assert!(marker.ends_with('…'), "truncated marker: {marker}");
        // The prefix plus the 48-cell cap plus the ellipsis, in cells.
        assert!(
            unicode_width::UnicodeWidthStr::width(marker.as_str()) <= 11 + 48 + 1,
            "bounded marker: {marker}"
        );
    }

    #[test]
    fn a_failed_result_names_its_tool() {
        let mut transcript = Transcript::new();
        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({}),
        };
        transcript.apply_item(&recorded(1, 1, EventKind::ToolCall { call }));
        let mut item = recorded(
            2,
            1,
            EventKind::ToolResult {
                call_id: "c1".into(),
                result: serde_json::json!("denied"),
            },
        );
        if let LiveKind::Recorded { event } = &mut item.kind {
            event.status = Status::Error;
            event.error = Some(EventError {
                kind: "approval_rejected".into(),
                message: "denied by the operator".into(),
            });
        }
        transcript.apply_item(&item);
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec!["▸ write_file", "✗ write_file: denied by the operator"]
        );
    }

    fn completion(span: &str) -> ToolCompletion {
        ToolCompletion {
            span_id: span.into(),
            turn: 1,
            message_call_index: 1,
            call_id: "duplicate".into(),
            name: "write_file".into(),
            result: serde_json::json!("updated file"),
            error: None,
        }
    }

    fn completed(seq: u64, completion: ToolCompletion) -> LiveItem {
        LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::ToolCompleted { completion },
        }
    }

    fn tool_started(seq: u64, span_id: &str, call: ToolCall) -> LiveItem {
        let mut item = recorded(seq, 1, EventKind::ToolCall { call });
        let LiveKind::Recorded { event } = &mut item.kind else {
            unreachable!()
        };
        event.span_id = span_id.into();
        item
    }

    fn durable_completion(seq: u64, completion: &ToolCompletion) -> LiveItem {
        let mut item = recorded(
            seq,
            completion.turn,
            EventKind::ToolResult {
                call_id: completion.call_id.clone(),
                result: completion.result.clone(),
            },
        );
        let LiveKind::Recorded { event } = &mut item.kind else {
            unreachable!()
        };
        event.span_id.clone_from(&completion.span_id);
        event.error.clone_from(&completion.error);
        if event.error.is_some() {
            event.status = Status::Error;
        }
        item
    }

    #[test]
    fn an_in_order_success_is_visible_before_the_remaining_approval() {
        let mut transcript = Transcript::new();
        let calls = gated_calls();
        transcript.apply_item(&approval_request(1, "ap1", calls.clone()));
        transcript.apply_item(&call_resolution(2, "ap1", 0, Approval::Approved));
        transcript.apply_item(&tool_started(3, "first", calls[0].clone()));
        let mut first = completion("first");
        first.call_id.clone_from(&calls[0].id);
        transcript.apply_item(&durable_completion(4, &first));
        assert_eq!(
            pump(&mut transcript).0,
            [
                "✓ approved: write_file",
                "▸ write_file",
                "✓ write_file",
                "│ updated file",
            ]
        );
        assert!(
            transcript.approvals["ap1"][1].is_some(),
            "feedback precedes the sibling decision"
        );
        assert!(transcript.live_calls.is_empty());
        assert!(
            transcript.completed_tools.is_empty(),
            "there was no provisional completion"
        );
        transcript.apply_item(&call_resolution(5, "ap1", 1, Approval::Approved));
        transcript.apply_item(&tool_started(6, "second", calls[1].clone()));
        let mut second = completion("second");
        second.call_id.clone_from(&calls[1].id);
        transcript.apply_item(&durable_completion(7, &second));
        assert_eq!(
            pump(&mut transcript).0,
            [
                "✓ approved: edit_file",
                "▸ edit_file",
                "✓ edit_file",
                "│ updated file",
            ],
            "a settled sibling changes nothing: every success renders the same way"
        );
        assert!(transcript.live_calls.is_empty());
    }

    #[test]
    fn same_id_live_errors_use_their_outstanding_span_names() {
        for approval_open in [false, true] {
            let mut transcript = Transcript::new();
            let mut calls = gated_calls();
            calls[1].id = calls[0].id.clone();
            if approval_open {
                let mut batch = calls.clone();
                batch.push(calls[0].clone());
                transcript.apply_item(&approval_request(1, "ap1", batch));
                transcript.apply_item(&call_resolution(2, "ap1", 0, Approval::Approved));
                transcript.apply_item(&call_resolution(3, "ap1", 1, Approval::Approved));
                pump(&mut transcript);
            }
            // Serial execution can still open B's span before A's buffered
            // result lands. The duplicate provider id cannot rename A.
            transcript.apply_item(&tool_started(4, "first", calls[0].clone()));
            transcript.apply_item(&tool_started(5, "second", calls[1].clone()));
            assert_eq!(transcript.live_calls.len(), 2);
            let mut failed = completion("first");
            failed.call_id.clone_from(&calls[0].id);
            failed.error = Some(EventError {
                kind: "tool".into(),
                message: "write failed".into(),
            });
            failed.result = serde_json::json!({"retry": false});
            transcript.apply_item(&durable_completion(6, &failed));
            let rows = pump(&mut transcript).0;
            assert_eq!(
                rows,
                ["▸ write_file", "▸ edit_file", "✗ write_file: write failed"],
                "approval state changes nothing: failures keep the one-line shape"
            );
            assert_eq!(transcript.live_calls.len(), 1);
            assert_eq!(transcript.live_calls["second"].name, "edit_file");
            failed.span_id = "second".into();
            failed.error.as_mut().unwrap().message = "edit failed".into();
            transcript.apply_item(&durable_completion(7, &failed));
            assert!(pump(&mut transcript).0[0].contains("edit_file"));
            assert!(transcript.live_calls.is_empty());
        }
    }

    #[test]
    fn history_call_ids_never_supply_live_result_names() {
        let sync = sync_with(history_with_repeated_calls(), Vec::new());
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        pump(&mut transcript);
        let mut failed = completion("unseen-span");
        failed.call_id = "c1".into();
        failed.error = Some(EventError {
            kind: "tool".into(),
            message: "failed".into(),
        });
        transcript.apply_item(&durable_completion(1, &failed));
        assert_eq!(
            pump(&mut transcript).0,
            ["✗ c1: failed"],
            "span-free history is not evidence of a live tool's identity"
        );
        transcript.apply_item(&tool_started(2, "outstanding", gated_calls().remove(0)));
        transcript.apply_item(&recorded(3, 1, EventKind::RunFinished { turns: 1 }));
        assert!(transcript.live_calls.is_empty());
    }

    #[test]
    fn an_unobserved_live_success_stays_quiet() {
        // The span was never observed, so the provider id alone cannot verify
        // the label — pending approval or not, nothing renders.
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&durable_completion(2, &completion("unseen-span")));
        assert!(pump(&mut transcript).0.is_empty());
    }

    #[test]
    fn a_live_action_success_renders_marker_outcome_and_preview_without_approvals() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&tool_started(
            1,
            "sp-1",
            ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "src/out.rs"}),
            },
        ));
        let mut done = completion("sp-1");
        done.result = serde_json::json!("wrote 3 lines\n+fn main() {}\n+}\ndone");
        transcript.apply_item(&completed(2, done.clone()));
        assert_eq!(
            pump(&mut transcript).0,
            [
                "▸ write_file src/out.rs",
                "✓ write_file src/out.rs",
                "│ wrote 3 lines",
                "│ +fn main() {}",
                "│ +}",
                "│ done",
            ],
            "the provisional completion renders with no approval pending"
        );
        // The durable echo rides the same span: no double render.
        transcript.apply_item(&durable_completion(3, &done));
        assert!(pump(&mut transcript).0.is_empty());
    }

    /// The divergence regression lock: one session rendered live and rebuilt
    /// from an attach baseline produces the same rows.
    #[test]
    fn a_live_success_and_its_attach_replay_render_the_same_rows() {
        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": "src/out.rs"}),
        };
        let mut live = Transcript::new();
        live.apply_item(&tool_started(1, "sp-1", call.clone()));
        let mut done = completion("sp-1");
        done.result = serde_json::json!("wrote src/out.rs");
        live.apply_item(&durable_completion(2, &done));
        let live_rows = pump(&mut live).0;
        // The same session folded into an attach baseline.
        let assistant = Message {
            role: Role::Assistant,
            content: vec![cadmus_contract::ContentPart::ToolCall { call }],
            tool_call_id: None,
            is_error: false,
            opaque: None,
        };
        let sync = sync_with(
            vec![
                assistant,
                Message::tool_result("c1", serde_json::json!("wrote src/out.rs")),
            ],
            Vec::new(),
        );
        let mut replayed = Transcript::new();
        replayed.apply_sync(&sync, 80, highlighter(), false);
        let replay_rows = pump(&mut replayed).0;
        assert_eq!(
            live_rows,
            [
                "▸ write_file src/out.rs",
                "✓ write_file src/out.rs",
                "│ wrote src/out.rs",
            ]
        );
        assert_eq!(replay_rows, live_rows, "live ≡ replay");
    }

    #[test]
    fn a_perception_success_leaves_exactly_one_subtle_line() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&tool_started(
            1,
            "sp-1",
            ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            },
        ));
        let mut done = completion("sp-1");
        done.name = "read_file".into();
        done.result = serde_json::json!("fn main() {}\n// many more lines".repeat(20));
        transcript.apply_item(&durable_completion(2, &done));
        let rows = drained_rows(&mut transcript);
        assert_eq!(
            texts(&rows),
            ["✓ read_file src/main.rs"],
            "no marker, no preview"
        );
        let row = &rows[0];
        assert_eq!(row.spans[0].content, "✓ ");
        assert_eq!(
            row.spans[0].style.fg,
            Some(ratatui::style::Color::Green),
            "the check rides the success slot"
        );
        assert_eq!(row.spans[1].content, "read_file src/main.rs");
        assert_eq!(
            row.spans[1].style.fg,
            Some(ratatui::style::Color::DarkGray),
            "the label stays subtle"
        );
    }

    /// The gate is gone: a pending approval changes no result rows.
    #[test]
    fn an_approval_wait_changes_no_result_rows() {
        let run_rows = |with_approval: bool| {
            let mut transcript = Transcript::new();
            if with_approval {
                transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
            }
            transcript.apply_item(&tool_started(
                2,
                "sp-success",
                ToolCall {
                    id: "c1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "src/out.rs"}),
                },
            ));
            let mut done = completion("sp-success");
            done.result = serde_json::json!("wrote src/out.rs");
            transcript.apply_item(&durable_completion(3, &done));
            transcript.apply_item(&tool_started(
                4,
                "sp-failure",
                ToolCall {
                    id: "c2".into(),
                    name: "edit_file".into(),
                    arguments: serde_json::json!({"path": "src/lib.rs"}),
                },
            ));
            let mut failed = completion("sp-failure");
            failed.call_id = "c2".into();
            failed.error = Some(EventError {
                kind: "tool".into(),
                message: "edit failed".into(),
            });
            transcript.apply_item(&durable_completion(5, &failed));
            pump(&mut transcript).0
        };
        let without_approval = run_rows(false);
        assert_eq!(
            without_approval,
            [
                "▸ write_file src/out.rs",
                "✓ write_file src/out.rs",
                "│ wrote src/out.rs",
                "▸ edit_file src/lib.rs",
                "✗ edit_file src/lib.rs: edit failed",
            ]
        );
        assert_eq!(
            run_rows(true),
            without_approval,
            "approval-pending output is byte-identical"
        );
    }

    #[test]
    fn completion_feedback_is_visible_before_a_sibling_is_decided() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&call_resolution(2, "ap1", 1, Approval::Approved));
        let completion = completion("second");
        transcript.apply_item(&tool_started(
            3,
            "second",
            ToolCall {
                id: completion.call_id.clone(),
                name: completion.name.clone(),
                arguments: serde_json::json!({}),
            },
        ));
        transcript.apply_item(&completed(4, completion.clone()));
        transcript.apply_item(&completed(5, completion.clone()));
        assert_eq!(
            pump(&mut transcript).0,
            [
                "✓ approved: edit_file",
                "▸ write_file",
                "✓ write_file",
                "│ updated file",
            ]
        );
        assert!(transcript.approvals["ap1"][0].is_some());
        transcript.apply_item(&durable_completion(6, &completion));
        assert!(pump(&mut transcript).0.is_empty());
        assert!(transcript.live_calls.is_empty());
        assert!(transcript.completed_tools.is_empty());
    }

    #[test]
    fn completion_errors_deduplicate_by_span_not_provider_call_id() {
        let mut transcript = Transcript::new();
        let mut failed = completion("second");
        failed.error = Some(EventError {
            kind: "tool".into(),
            message: "write failed".into(),
        });
        failed.result = serde_json::json!({"retry": false});
        transcript.apply_item(&completed(1, failed.clone()));
        let rows = pump(&mut transcript).0;
        assert_eq!(
            rows,
            [
                "✗ write_file",
                "│ tool: write failed",
                "│ {\"retry\":false}"
            ]
        );
        let mut sibling = failed.clone();
        sibling.span_id = "first".into();
        transcript.apply_item(&durable_completion(2, &sibling));
        assert_eq!(pump(&mut transcript).0, ["✗ duplicate: write failed"]);
        transcript.apply_item(&durable_completion(3, &failed));
        assert!(
            pump(&mut transcript).0.is_empty(),
            "the preview's own durable error must not render twice"
        );
    }

    #[test]
    fn completion_sync_restores_previews_without_double_rendering_after_lag() {
        let mut sync = sync_with(Vec::new(), Vec::new());
        sync.as_of_seq = 3;
        sync.in_flight.completed_tools.push(completion("second"));
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        assert_eq!(pump(&mut transcript).0.len(), 2);
        let mut third = completion("third");
        third.message_call_index = 2;
        third.result = serde_json::json!("another update");
        sync.in_flight.completed_tools.push(third.clone());
        sync.as_of_seq = 5;
        transcript.apply_sync(&sync, 80, highlighter(), true);
        assert_eq!(
            pump(&mut transcript).0,
            ["✓ write_file", "│ another update"]
        );
        transcript.apply_sync(&sync, 80, highlighter(), true);
        assert!(pump(&mut transcript).0.is_empty());
        let replay = texts(&transcript.replay_tail(
            20,
            80,
            highlighter(),
            &Theme::ansi(),
            ColorDepth::Truecolor,
        ));
        assert_eq!(
            replay
                .iter()
                .filter(|row| row.contains("✓ write_file"))
                .count(),
            2
        );
        transcript.apply_item(&completed(5, third.clone()));
        transcript.apply_item(&durable_completion(6, &completion("second")));
        transcript.apply_item(&durable_completion(7, &third));
        assert!(pump(&mut transcript).0.is_empty());
        assert!(transcript.completed_tools.is_empty());
    }

    #[test]
    fn completion_preview_bounds_text_structured_output_and_control_bytes() {
        let mut completion = completion("span");
        for result in [
            serde_json::json!("\x1b[31m界\n".repeat(1000)),
            serde_json::json!({"data": "界".repeat(10000)}),
        ] {
            completion.result = result;
            let lines = completion_lines(&completion, None);
            assert!(lines.len() <= 6);
            assert!(lines.iter().map(|line| line.text().len()).sum::<usize>() < 1024);
            assert!(lines.last().unwrap().text().contains("truncated"));
            assert!(lines.iter().all(|line| !line.text().contains('\x1b')));
        }
    }

    #[test]
    fn the_rewind_requeues_pending_rows_at_the_new_width() {
        // A closed block queues at 80; one row drains (the front is
        // partially drained). The rewind keeps the front's remaining rows
        // at the old wrap — re-queuing them would duplicate the inserted
        // prefix — and the seal's rows come out at the new width. No row
        // is lost, none duplicated.
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, &format!("{long}\n\nx\n")));
        let at_80 = |transcript: &mut Transcript| {
            transcript.snapshot(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor)
        };
        let at_40 = |transcript: &mut Transcript| {
            transcript.snapshot(40, highlighter(), &Theme::ansi(), ColorDepth::Truecolor)
        };
        at_80(&mut transcript);
        let drain = transcript.drain(1);
        transcript.apply_flush(&drain.acks);
        assert_eq!(texts(&drain.rows).len(), 1, "one row out at 80");
        let first_row = texts(&drain.rows);

        transcript.rewind_queue();
        // The re-snapshot queues nothing new (the kept front still covers
        // the queued prefix); the kept rows drain unchanged.
        at_40(&mut transcript);
        let drain = transcript.drain(usize::MAX);
        transcript.apply_flush(&drain.acks);
        let mut emitted = first_row;
        emitted.extend(texts(&drain.rows));
        assert_eq!(
            emitted,
            texts(&wrap_rows(
                &[ir::Line::plain(long), ir::Line::default()],
                80,
                &Theme::ansi(),
                ColorDepth::Truecolor
            )),
            "the kept front completed the block's 80-column rows, once"
        );

        // The seal's rows queue at the new width.
        transcript.apply_item(&llm_response(2, 1, &format!("{long}\n\nx\n")));
        at_40(&mut transcript);
        let drain = transcript.drain(usize::MAX);
        transcript.apply_flush(&drain.acks);
        assert_eq!(
            texts(&drain.rows),
            texts(&wrap_rows(
                &[ir::Line::plain("x")],
                40,
                &Theme::ansi(),
                ColorDepth::Truecolor
            )),
            "the seal re-wrapped at the new width"
        );
    }

    #[test]
    fn the_rewind_with_an_untouched_queue_rewraps_everything() {
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, &format!("{long}\n\nx\n")));
        transcript.snapshot(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        // Nothing drained: the full queue folds back and re-wraps.
        transcript.rewind_queue();
        transcript.snapshot(40, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        let drain = transcript.drain(usize::MAX);
        transcript.apply_flush(&drain.acks);
        assert_eq!(
            texts(&drain.rows),
            texts(&wrap_rows(
                &[ir::Line::plain(long), ir::Line::default()],
                40,
                &Theme::ansi(),
                ColorDepth::Truecolor
            )),
            "the untouched queue re-wrapped wholesale at the new width"
        );
    }

    #[test]
    fn replay_covers_the_open_tails_flushed_prefix_only() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "alpha\n\nbeta\n"));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["alpha", ""]);
        let replay =
            transcript.replay_tail(10, 80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(texts(&replay), vec!["alpha", ""]);
    }

    #[test]
    fn resync_rebuilds_with_the_transfer() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "alpha\n\nbeta\n"));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["alpha", ""]);
        // Lag: the turn continued while we were behind. Re-attach carries the
        // full in-flight text; the flushed prefix (2 lines) stays in
        // scrollback via the transfer. The marker row is the app's, flushed
        // directly outside the block model (see `apply_sync`).
        let sync = Sync {
            history: RunState {
                trace_id: "tr-test".into(),
                provider: None,
                model: None,
                messages: Vec::new(),
                tool_results: Vec::new(),
                turns: 0,
                warnings: Vec::new(),
                scores: Vec::new(),
                dangling_tool_calls: Vec::new(),
                finished: None,
            },
            in_flight: InFlight {
                open_turn: Some(OpenTurn {
                    turn: 1,
                    partial: TurnSnapshot {
                        text: "alpha\n\nbeta\nmore\n".into(),
                        ..TurnSnapshot::default()
                    },
                }),
                pending_approvals: Vec::new(),
                completed_tools: Vec::new(),
            },
            settled_approvals: Vec::new(),
            as_of_seq: 4,
        };
        transcript.apply_sync(&sync, 80, highlighter(), true);
        let (flushed, tail_live) = pump(&mut transcript);
        // The rebuilt tail re-shows nothing (one paragraph — soft breaks
        // join into one logical line, still unstable), and nothing
        // re-flushes.
        assert_eq!(flushed, Vec::<String>::new());
        assert!(tail_live);
        // The transferred prefix replays, the live rows do not.
        let replay =
            transcript.replay_tail(10, 80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(texts(&replay), vec!["alpha", ""]);
        // Items at or below the baseline are dropped (the client rule).
        assert_eq!(transcript.apply_item(&delta(3, 1, "stale\n")), Light::None);
        let (_, tail_live) = pump(&mut transcript);
        assert!(tail_live);
    }

    #[test]
    fn a_steer_lands_as_a_prompt_block() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&recorded(
            1,
            1,
            EventKind::Command(Command::Steer {
                command_id: "cmd-1".into(),
                text: "also check tests".into(),
                mode: cadmus_contract::SteerMode::Queue,
            }),
        ));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["❯ also check tests", ""]);
    }

    fn approval_request(seq: u64, request_id: &str, calls: Vec<ToolCall>) -> LiveItem {
        LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::ApprovalRequested {
                request_id: request_id.into(),
                turn: 1,
                calls,
                wait_timeout: std::time::Duration::from_secs(300),
            },
        }
    }

    fn gated_calls() -> Vec<ToolCall> {
        vec![
            ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({}),
            },
        ]
    }

    #[test]
    fn an_approved_resolution_names_the_calls_quietly() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        // The request itself renders nothing; the resolution names the calls.
        transcript.apply_item(&recorded(
            2,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-1".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Approved, Approval::Approved],
            }),
        ));
        let (flushed, tail_live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✓ approved: write_file, edit_file"]);
        assert!(!tail_live);
        // The settled request leaves the map: a duplicate resolve names the id.
        transcript.apply_item(&recorded(
            3,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-2".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Approved],
            }),
        ));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✓ approved: ap1"]);
    }

    fn call_resolution(
        seq: u64,
        request_id: &str,
        call_index: usize,
        decision: Approval,
    ) -> LiveItem {
        recorded(
            seq,
            1,
            EventKind::Command(Command::ResolveApprovalCall {
                command_id: format!("cmd-{seq}"),
                request_id: request_id.into(),
                call_index,
                decision,
            }),
        )
    }

    #[test]
    fn per_call_records_ignore_duplicates_and_leave_batch_indices_intact() {
        let mut transcript = Transcript::new();
        let mut calls = gated_calls();
        calls[1].id = calls[0].id.clone();
        transcript.apply_item(&approval_request(1, "ap1", calls));
        transcript.apply_item(&call_resolution(2, "unknown", 0, Approval::Approved));
        transcript.apply_item(&call_resolution(3, "ap1", usize::MAX, Approval::Approved));
        transcript.apply_item(&call_resolution(4, "ap1", 1, Approval::Approved));
        transcript.apply_item(&call_resolution(
            5,
            "ap1",
            1,
            Approval::Rejected { comment: None },
        ));
        assert_eq!(pump(&mut transcript).0, ["✓ approved: edit_file"]);
        transcript.apply_item(&recorded(
            6,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "ap1".into(),
                decisions: Vec::new(),
            }),
        ));
        assert_eq!(pump(&mut transcript).0, ["✗ rejected: write_file"]);
        transcript.apply_item(&call_resolution(7, "ap1", 0, Approval::Approved));
        assert!(pump(&mut transcript).0.is_empty());
    }

    #[test]
    fn a_batch_fallback_uses_original_positions_after_an_individual_rejection() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&call_resolution(
            2,
            "ap1",
            0,
            Approval::Rejected { comment: None },
        ));
        assert_eq!(pump(&mut transcript).0, ["✗ rejected: write_file"]);
        transcript.apply_item(&recorded(
            3,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Rejected { comment: None }, Approval::Approved],
            }),
        ));
        assert_eq!(pump(&mut transcript).0, ["✓ approved: edit_file"]);
    }

    #[test]
    fn partial_sync_rebuilds_decisions_once_and_keeps_siblings_open() {
        let mut sync = sync_with(Vec::new(), Vec::new());
        sync.as_of_seq = 4;
        sync.in_flight.pending_approvals.push(PendingApproval {
            request_id: "ap1".into(),
            turn: 1,
            message_index: None,
            calls: gated_calls(),
            decisions: vec![None, Some(Approval::Approved)],
            wait_timeout: std::time::Duration::from_secs(300),
        });
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        assert_eq!(pump(&mut transcript).0, ["✓ approved: edit_file"]);
        transcript.apply_sync(&sync, 80, highlighter(), true);
        assert!(
            pump(&mut transcript).0.is_empty(),
            "resync cannot reflush an old decision"
        );
        transcript.apply_item(&call_resolution(4, "ap1", 0, Approval::Approved));
        transcript.apply_item(&call_resolution(
            5,
            "ap1",
            1,
            Approval::Rejected { comment: None },
        ));
        assert!(
            pump(&mut transcript).0.is_empty(),
            "stale and duplicate answers have no effect"
        );
        transcript.apply_item(&call_resolution(
            6,
            "ap1",
            0,
            Approval::Rejected { comment: None },
        ));
        assert_eq!(pump(&mut transcript).0, ["✗ rejected: write_file"]);
        assert!(transcript.approvals.is_empty());
    }

    #[test]
    fn a_rejected_resolution_reads_in_the_error_slot() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&recorded(
            2,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-1".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Approved, Approval::Rejected { comment: None }],
            }),
        ));
        let rows = drained_rows(&mut transcript);
        assert_eq!(
            texts(&rows),
            vec!["✓ approved: write_file", "✗ rejected: edit_file"]
        );
        let rejected = &rows[1];
        assert_eq!(
            rejected.spans[0].style.fg,
            Some(ratatui::style::Color::Red),
            "the rejection rides the error slot"
        );
    }

    #[test]
    fn a_short_reply_denies_the_remainder() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&recorded(
            2,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-1".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Approved],
            }),
        ));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec!["✓ approved: write_file", "✗ rejected: edit_file"]
        );
    }

    #[test]
    fn a_sync_mid_wait_seeds_the_resolution_names() {
        // Attach during the wait: the dialog's request replays through the
        // sync baseline, and the recorded resolution still names the calls.
        let mut transcript = Transcript::new();
        let sync = Sync {
            history: RunState {
                trace_id: "tr-test".into(),
                provider: None,
                model: None,
                messages: Vec::new(),
                tool_results: Vec::new(),
                turns: 0,
                warnings: Vec::new(),
                scores: Vec::new(),
                dangling_tool_calls: Vec::new(),
                finished: None,
            },
            in_flight: InFlight {
                open_turn: None,
                completed_tools: Vec::new(),
                pending_approvals: vec![cadmus_contract::PendingApproval {
                    request_id: "ap9".into(),
                    turn: 1,
                    message_index: None,
                    calls: gated_calls(),
                    decisions: Vec::new(),
                    wait_timeout: std::time::Duration::from_secs(300),
                }],
            },
            settled_approvals: Vec::new(),
            as_of_seq: 0,
        };
        transcript.apply_sync(&sync, 80, highlighter(), false);
        transcript.apply_item(&recorded(
            1,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-1".into(),
                request_id: "ap9".into(),
                decisions: vec![Approval::Approved, Approval::Approved],
            }),
        ));
        let (flushed, _) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✓ approved: write_file, edit_file"]);
    }

    /// The settle-before-attach replay: the settled batch rides the sync
    /// baseline and the rebuild renders its record where the live path
    /// did — after the turn's text, before its tool markers, with the
    /// rejection's consequence still behind.
    #[test]
    fn a_settled_approval_replays_between_the_text_and_the_tool_markers() {
        let calls = gated_calls();
        let assistant = cadmus_contract::Message {
            role: Role::Assistant,
            content: vec![
                cadmus_contract::ContentPart::Text {
                    text: "will do".into(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[0].clone(),
                },
                cadmus_contract::ContentPart::ToolCall {
                    call: calls[1].clone(),
                },
            ],
            tool_call_id: None,
            is_error: false,
            opaque: None,
        };
        let sync = sync_with(
            vec![
                cadmus_contract::Message::user("change it"),
                assistant,
                cadmus_contract::Message::tool_error(
                    "c2",
                    serde_json::json!("rejected by a human"),
                ),
            ],
            vec![cadmus_contract::SettledApproval {
                request_id: "ap1".into(),
                message_index: Some(1),
                calls: calls.clone(),
                decisions: vec![Approval::Approved, Approval::Rejected { comment: None }],
            }],
        );
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        // A first attach owns nothing of the baseline: the rebuild renders
        // through the normal pump, the settled record in the live order.
        let (flushed, tail_live) = pump(&mut transcript);
        assert!(!tail_live);
        assert_eq!(
            flushed,
            vec![
                "❯ change it",
                "",
                "will do",
                "✓ approved: write_file",
                "✗ rejected: edit_file",
                "▸ write_file",
                "▸ edit_file",
                "✗ edit_file: rejected by a human",
            ]
        );
    }

    fn history_with_repeated_calls() -> Vec<Message> {
        let mut first = Message::text(Role::Assistant, "seeded turn");
        first.content.extend(
            gated_calls()
                .into_iter()
                .map(|call| cadmus_contract::ContentPart::ToolCall { call }),
        );
        let mut second = first.clone();
        second.content[0] = cadmus_contract::ContentPart::Text {
            text: "current turn one".into(),
        };
        let mut third = first.clone();
        third.content[0] = cadmus_contract::ContentPart::Text {
            text: "current turn two".into(),
        };
        vec![
            Message::user("seeded prompt"),
            first,
            Message::user("current prompt"),
            second,
            third,
        ]
    }

    fn pending_durable_baseline() -> Sync {
        let call = gated_calls().remove(0);
        let mut assistant = Message::text(Role::Assistant, "current turn");
        assistant.content.extend(
            [call.clone(), call.clone()]
                .into_iter()
                .map(|call| cadmus_contract::ContentPart::ToolCall { call }),
        );
        let mut sync = sync_with(
            vec![
                Message::user("change it"),
                assistant,
                Message::tool_result(call.id.clone(), serde_json::json!("first change written")),
            ],
            Vec::new(),
        );
        sync.as_of_seq = 10;
        sync.history.tool_results.push(ToolResultProjection {
            message_index: 2,
            request_id: "ap1".into(),
            call_index: 0,
        });
        sync.in_flight.pending_approvals.push(PendingApproval {
            request_id: "ap1".into(),
            turn: 1,
            message_index: Some(1),
            calls: vec![call.clone(), call],
            decisions: vec![Some(Approval::Approved), None],
            wait_timeout: std::time::Duration::from_secs(300),
        });
        sync
    }

    #[test]
    fn a_fresh_partial_attach_previews_the_durable_success_without_a_completion() {
        let sync = pending_durable_baseline();
        assert!(sync.in_flight.completed_tools.is_empty());
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let (rows, tail_live) = pump(&mut transcript);
        assert_eq!(
            rows,
            [
                "❯ change it",
                "",
                "current turn",
                "✓ approved: write_file",
                "▸ write_file",
                "▸ write_file",
                "✓ write_file",
                "│ first change written"
            ]
        );
        assert!(!tail_live);
        assert!(transcript.approvals["ap1"][1].is_some());
        assert!(pump(&mut transcript).0.is_empty());
        transcript.apply_sync(&sync, 80, highlighter(), true);
        assert!(
            pump(&mut transcript).0.is_empty(),
            "the durable preview stays preflushed on resync"
        );
        let replay = texts(&transcript.replay_tail(
            20,
            80,
            highlighter(),
            &Theme::ansi(),
            ColorDepth::Truecolor,
        ));
        assert_eq!(
            replay
                .iter()
                .filter(|row| row.contains("first change written"))
                .count(),
            1
        );
    }

    #[test]
    fn a_fresh_interrupted_partial_attach_attributes_only_bs_result_with_duplicate_ids() {
        let mut sync = pending_durable_baseline();
        let mut calls = gated_calls();
        calls[0].name = "A".into();
        calls[1].name = "B".into();
        calls[1].id = calls[0].id.clone();
        sync.history.messages[1].content = calls
            .iter()
            .cloned()
            .map(|call| cadmus_contract::ContentPart::ToolCall { call })
            .collect();
        sync.history.messages[2] =
            Message::tool_result(calls[1].id.clone(), serde_json::json!("B completed"));
        sync.history.tool_results[0].call_index = 1;
        let pending = &mut sync.in_flight.pending_approvals[0];
        pending.calls = calls;
        pending.decisions = vec![None, Some(Approval::Approved)];
        assert!(sync.in_flight.completed_tools.is_empty());
        assert_eq!(
            pending_result_names(&sync),
            HashMap::from([(2, "B".to_string())])
        );

        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        assert!(rows.windows(2).any(|rows| rows == ["✓ B", "│ B completed"]));
        assert!(!rows.iter().any(|row| row == "✓ A"));
        assert!(transcript.approvals["ap1"][0].is_some());
    }

    #[test]
    fn pending_baseline_results_use_approval_addresses_and_stay_in_the_anchored_turn() {
        let mut sync = pending_durable_baseline();
        let mut seeded = sync.history.messages[1].clone();
        seeded.content[0] = cadmus_contract::ContentPart::Text {
            text: "seeded turn".into(),
        };
        sync.history.messages.splice(
            0..0,
            [
                seeded,
                Message::tool_result("c1", serde_json::json!("old success")),
            ],
        );
        sync.in_flight.pending_approvals[0].message_index = Some(3);
        // Perception has no approval address, even though its result is first
        // in the full assistant batch. Every provider id deliberately repeats.
        sync.history.messages[3].content.insert(
            1,
            cadmus_contract::ContentPart::ToolCall {
                call: ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({}),
                },
            },
        );
        sync.history.messages.insert(
            4,
            Message::tool_result("c1", serde_json::json!("read result")),
        );
        sync.history
            .messages
            .push(Message::text(Role::Assistant, "later turn"));
        sync.history.messages.push(Message::tool_result(
            "c1",
            serde_json::json!("later success"),
        ));
        sync.history.tool_results[0].message_index = 5;
        // Even matching request metadata cannot move a result across the anchor.
        for message_index in [1, 7] {
            sync.history.tool_results.push(ToolResultProjection {
                message_index,
                request_id: "ap1".into(),
                call_index: 0,
            });
        }
        assert_eq!(
            pending_result_names(&sync),
            HashMap::from([(5, "write_file".to_string())])
        );
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        // The anchored result keeps its approval-verified label…
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ write_file", "│ first change written"])
        );
        // …and every other durable success lands too: the policy renders
        // successes everywhere. Provider ids deliberately repeat, so the
        // unaddressed results attribute by id, last-wins — the perception
        // result included; only an approval address verifies a label.
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ write_file", "│ old success"])
        );
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ write_file", "│ read result"])
        );
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ write_file", "│ later success"])
        );
    }

    #[test]
    fn a_pending_baseline_result_uses_the_bounded_preview_for_text_and_errors() {
        let mut sync = pending_durable_baseline();
        let result = sync.history.messages.last_mut().unwrap();
        result.is_error = true;
        result.content = vec![
            cadmus_contract::ContentPart::Text {
                text: "first error line".into(),
            },
            cadmus_contract::ContentPart::Text {
                text: "detail\n".repeat(1000),
            },
        ];
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        let at = rows.iter().position(|row| row == "✗ write_file").unwrap();
        assert_eq!(rows[at + 1], "│ first error line");
        assert_eq!(rows.last().unwrap(), "│ ⋯ 68 more lines truncated");
        assert_eq!(rows[at..].len(), 6);
    }

    #[test]
    fn successful_history_renders_without_an_open_anchored_batch() {
        let baseline = pending_durable_baseline();
        let mut no_pending = baseline.clone();
        no_pending.in_flight.pending_approvals.clear();
        no_pending.history.messages.push(Message::tool_result(
            "c1",
            serde_json::json!("second change written"),
        ));
        let mut settled = baseline.clone();
        settled.in_flight.pending_approvals[0].decisions = vec![Some(Approval::Approved); 2];
        let mut missing_anchor = baseline.clone();
        missing_anchor.in_flight.pending_approvals[0].message_index = None;
        let mut invalid_anchor = baseline.clone();
        invalid_anchor.in_flight.pending_approvals[0].message_index = Some(0);
        let mut no_metadata = baseline.clone();
        no_metadata.history.tool_results.clear();
        let mut wrong_request = baseline.clone();
        wrong_request.history.tool_results[0].request_id = "another request".into();
        let mut unanswered_call = baseline.clone();
        unanswered_call.history.tool_results[0].call_index = 1;
        let mut invalid_call = baseline.clone();
        invalid_call.history.tool_results[0].call_index = usize::MAX;
        let mut invalid_result = baseline;
        invalid_result.history.tool_results[0].message_index = usize::MAX;
        for sync in [
            no_pending,
            settled,
            missing_anchor,
            invalid_anchor,
            no_metadata,
            wrong_request,
            unanswered_call,
            invalid_call,
            invalid_result,
        ] {
            let mut transcript = Transcript::new();
            transcript.apply_sync(&sync, 80, highlighter(), false);
            let rows = pump(&mut transcript).0;
            assert!(
                rows.windows(2)
                    .any(|rows| rows == ["✓ write_file", "│ first change written"]),
                "the durable success renders; approval state only affects attribution"
            );
        }
    }

    #[test]
    fn a_legacy_pending_result_retains_error_rendering_without_a_success_preview() {
        let mut sync = pending_durable_baseline();
        sync.history.tool_results.clear();
        sync.history.messages[2] = Message::tool_error("c1", serde_json::json!("legacy failure"));
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        assert_eq!(rows.last().unwrap(), "✗ write_file: legacy failure");
    }

    #[test]
    fn approval_anchors_distinguish_identical_calls_across_seeded_and_current_turns() {
        let mut sync = sync_with(
            history_with_repeated_calls(),
            vec![SettledApproval {
                request_id: "settled".into(),
                message_index: Some(3),
                calls: gated_calls(),
                decisions: vec![Approval::Approved; 2],
            }],
        );
        sync.in_flight.pending_approvals.push(PendingApproval {
            request_id: "partial".into(),
            turn: 2,
            message_index: Some(4),
            calls: gated_calls(),
            decisions: vec![None, Some(Approval::Rejected { comment: None })],
            wait_timeout: std::time::Duration::from_secs(300),
        });
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        let current = rows
            .iter()
            .position(|row| row == "current turn one")
            .unwrap();
        let next = rows
            .iter()
            .position(|row| row == "current turn two")
            .unwrap();
        assert_eq!(rows[current + 1], "✓ approved: write_file, edit_file");
        assert_eq!(rows[next + 1], "✗ rejected: edit_file");
        assert!(
            !rows[..current]
                .iter()
                .any(|row| row.contains("approved") || row.contains("rejected"))
        );
    }

    #[test]
    fn absent_invalid_or_non_assistant_anchors_render_at_the_tail() {
        for message_index in [None, Some(usize::MAX), Some(0)] {
            let sync = sync_with(
                history_with_repeated_calls(),
                vec![SettledApproval {
                    request_id: "ap1".into(),
                    message_index,
                    calls: gated_calls(),
                    decisions: vec![Approval::Approved; 2],
                }],
            );
            let mut transcript = Transcript::new();
            transcript.apply_sync(&sync, 80, highlighter(), false);
            let rows = pump(&mut transcript).0;
            assert_eq!(rows.last().unwrap(), "✓ approved: write_file, edit_file");
        }
    }

    /// The crash window: the batch settled but its turn's response never
    /// folded, so no message carries its calls. The explicit record still
    /// renders — after the history, in window order.
    #[test]
    fn a_settled_approval_without_a_matching_message_renders_at_the_end() {
        let sync = sync_with(
            vec![cadmus_contract::Message::user("change it")],
            vec![cadmus_contract::SettledApproval {
                request_id: "ap1".into(),
                message_index: None,
                calls: gated_calls(),
                decisions: vec![Approval::Approved, Approval::Rejected { comment: None }],
            }],
        );
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let (flushed, tail_live) = pump(&mut transcript);
        assert!(!tail_live);
        assert_eq!(
            flushed,
            vec![
                "❯ change it",
                "",
                "✓ approved: write_file",
                "✗ rejected: edit_file",
            ]
        );
    }
}
