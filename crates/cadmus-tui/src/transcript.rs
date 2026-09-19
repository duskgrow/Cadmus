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
//! The snapshot contract (the per-accessor re-render open item's consumer):
//! one [`Transcript::snapshot`] per pump batch drives the flush, the band
//! render and the layout input — a single pipeline render and a single wrap
//! pass per block, never one per accessor.

use std::collections::{HashMap, HashSet};
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

/// One assistant block: the markdown pipeline plus the count of logical
/// lines already in scrollback (the re-sync transfer's source).
struct Agent {
    stream: Stream,
    acked: usize,
}

/// One transcript block; see the module docs.
enum Block {
    /// Static content, complete at birth (prompts, markers): semantic
    /// logical lines — the SSOT re-wrapped on width change and replay.
    Static(Vec<ir::Line>),
    Agent(Agent),
}

/// What one applied item means for the status line (presentation only).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Light {
    /// No status change.
    None,
    /// Assistant output is streaming.
    Streaming,
    /// A tool is executing.
    Tool(String),
    /// The run finished cleanly.
    Idle,
    /// The run failed.
    Failed,
}

/// The per-block flush plan, aligned with the transcript's unflushed blocks
/// in order. Produced by [`Transcript::snapshot`], consumed by
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

/// One materialization pass: the flush plan and rows plus the band's
/// post-flush tail, from a single render/wrap sweep (module docs).
#[derive(Debug)]
pub struct Snapshot {
    /// Rows that leave the band into scrollback on this pump, in order.
    pub flush_rows: Vec<Line<'static>>,
    /// The flush plan for [`Transcript::apply_flush`].
    pub acks: Vec<FlushAck>,
    /// The band's post-flush stream-tail rows (bottom-anchored by the app).
    pub live_rows: Vec<Line<'static>>,
}

/// The materialized view-model. See the module docs for the invariants.
pub struct Transcript {
    blocks: Vec<Block>,
    /// Leading blocks that have left the band into scrollback.
    flushed: usize,
    /// The open assistant block's turn, when the tail is one.
    open_turn: Option<u32>,
    /// The client rule's position filter (ADR-0013 item 4): items with
    /// `seq ≤ as_of_seq` are dropped.
    as_of_seq: u64,
    /// Outstanding live span → tool name. Provider ids can repeat even in
    /// one batch; each result retires its span, including silent successes.
    live_calls: HashMap<String, String>,
    /// Original approval slots retain their indices; taking a name marks its
    /// decision rendered, so retries and batch fallback cannot render it twice.
    approvals: HashMap<String, Vec<Option<String>>>,
    /// Only provisional outcomes still awaiting their durable result; span
    /// ids are run-unique, unlike provider call ids.
    completed_tools: HashSet<String>,
}

impl Transcript {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            flushed: 0,
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
        // The accent "> " marker is the user-voice cue; the prompt body
        // stays default (ADR-0017 slot wiring).
        lines.extend(text.lines().map(|line| {
            ir::Line::from_spans(vec![
                ir::Span::slotted("> ", Slot::Accent),
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
                pending_results.get(&index).copied(),
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
        self.seal_open();
        self.blocks
            .push(Block::Static(completion_lines(completion)));
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

    /// One materialization pass over the unflushed blocks: the flush plan
    /// and rows plus the post-flush live tail. See the module docs.
    pub fn snapshot(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Snapshot {
        let tail = self.blocks.len().saturating_sub(1);
        let mut flush_rows = Vec::new();
        let mut live_rows = Vec::new();
        let mut acks = Vec::new();
        for (offset, block) in self.blocks[self.flushed..].iter_mut().enumerate() {
            let index = self.flushed + offset;
            match block {
                Block::Static(lines) => {
                    flush_rows.extend(wrap_rows(lines, width, theme, depth));
                    acks.push(FlushAck::WholeBlock);
                }
                Block::Agent(agent) => {
                    let render = agent.stream.render(width, highlighter);
                    let live = render.live_lines();
                    let flushable = render.flushable_len();
                    let sealed = self.open_turn.is_none() || index != tail;
                    let completes = sealed && flushable == live.len();
                    // Per-line wrap independence: the prefix's rows are a row
                    // prefix of the whole — two disjoint wraps cost one.
                    flush_rows.extend(wrap_rows(&live[..flushable], width, theme, depth));
                    live_rows.extend(wrap_rows(&live[flushable..], width, theme, depth));
                    if flushable > 0 || completes {
                        acks.push(FlushAck::AgentLines {
                            lines: flushable,
                            completes,
                        });
                    }
                }
            }
        }
        Snapshot {
            flush_rows,
            acks,
            live_rows,
        }
    }

    /// Confirm the snapshot's flush plan (a successful shell flush).
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

    /// Every unflushed row (flushable prefix + open tail) at `width` — the
    /// band content a resize repaint needs before the next pump's flush.
    pub fn unflushed_rows(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Vec<Line<'static>> {
        let snapshot = self.snapshot(width, highlighter, theme, depth);
        let mut rows = snapshot.flush_rows;
        rows.extend(snapshot.live_rows);
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
                    }));
                }
                Light::Streaming
            }
            EventKind::ToolCall { call } => {
                self.seal_open();
                self.live_calls
                    .insert(event.span_id.clone(), call.name.clone());
                self.blocks
                    .push(Block::Static(vec![subtle_line(tool_marker(call))]));
                Light::Tool(call.name.clone())
            }
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
                self.blocks.push(Block::Static(vec![subtle_line(format!(
                    "+ instructions: {path}"
                ))]));
                Light::None
            }
            EventKind::Fold { folded, .. } => {
                self.seal_open();
                self.blocks.push(Block::Static(vec![subtle_line(format!(
                    "⑃ context folded: {} result(s) compressed",
                    folded.len()
                ))]));
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
        let name = self.live_calls.remove(&event.span_id);
        if self.completed_tools.remove(&event.span_id) {
            return;
        }
        // Attach may have missed the opening span. A provider id alone cannot
        // verify a successful result's label; legacy errors retain their fallback.
        if name.is_none() && event.status != Status::Error {
            return;
        }
        let tool = name.as_deref().unwrap_or(call_id);
        if self
            .approvals
            .values()
            .any(|slots| slots.iter().any(Option::is_some))
        {
            // In-order results are already durable, but still inform a human
            // deciding a sibling. No full-batch index exists on this event.
            let lines = outcome_lines(
                &truncate_cells(tool, 48),
                result,
                event.error.as_ref(),
                event.status == Status::Error,
            );
            self.seal_open();
            self.blocks.push(Block::Static(lines));
        } else if event.status == Status::Error {
            let detail = event
                .error
                .as_ref()
                .and_then(|error| error.message.lines().next())
                .unwrap_or("failed");
            self.seal_open();
            self.blocks.push(Block::Static(vec![error_line(format!(
                "✗ {tool}: {detail}"
            ))]));
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
        history_calls: &mut HashMap<String, String>,
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
                    }));
                }
                for settled in settled {
                    let names: Vec<String> =
                        settled.calls.iter().map(|call| call.name.clone()).collect();
                    self.blocks
                        .push(Block::Static(resolution_lines(&names, &settled.decisions)));
                }
                for call in message.tool_calls() {
                    history_calls.insert(call.id.clone(), call.name.clone());
                    self.blocks
                        .push(Block::Static(vec![subtle_line(tool_marker(call))]));
                }
            }
            Role::Tool => {
                if let Some(tool) = preview_tool {
                    self.blocks.push(Block::Static(outcome_lines(
                        &truncate_cells(tool, 48),
                        &HistoryResult(message),
                        None,
                        message.is_error,
                    )));
                } else if message.is_error {
                    let call_id = message.tool_call_id.as_deref().unwrap_or("?");
                    let tool = history_calls.get(call_id).map_or(call_id, String::as_str);
                    let detail = message.text_body();
                    let detail = detail.lines().next().unwrap_or("failed");
                    self.blocks.push(Block::Static(vec![error_line(format!(
                        "✗ {tool}: {detail}"
                    ))]));
                }
            }
            Role::System => {}
        }
    }
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
/// Only the durable approval address verifies a name; neither result order nor
/// provider ids can. The request's anchor confines it to the originating turn.
fn pending_result_names(sync: &Sync) -> HashMap<usize, &str> {
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
                names.insert(result.message_index, call.name.as_str());
            }
        }
    }
    names
}

fn completion_lines(completion: &ToolCompletion) -> Vec<ir::Line> {
    let name = truncate_cells(&completion.name, 48);
    // The index is the call's position in the assistant message, not in the
    // approval batch — labelled "tool call" so it never reads as the dialog's
    // "M/N", which numbers the gated subset.
    let label = format!(
        "{name} (turn {}, tool call {})",
        completion.turn,
        completion.message_call_index.saturating_add(1)
    );
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

/// Both early and durable results share the same bounded preview. A durable
/// result lacks a full-batch index, so its caller supplies only a tool label.
fn outcome_lines(
    label: &str,
    result: &dyn std::fmt::Display,
    error: Option<&EventError>,
    failed: bool,
) -> Vec<ir::Line> {
    let mut lines = vec![if failed {
        error_line(format!("✗ failed {label}"))
    } else {
        subtle_line(format!("✓ completed {label}"))
    }];
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
    lines.extend(
        text.lines()
            .take(4)
            .map(|line| subtle_line(format!("  {}", truncate_cells(line, 120)))),
    );
    if preview.truncated || text.lines().count() > 4 {
        lines.push(subtle_line("  ⋯ result preview truncated"));
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

/// The tool-activity marker: the name plus the call's primary target, so a
/// run of same-name calls stays distinguishable (field report 2026-09-16:
/// 23 bare `→ list_dir` rows read as duplicates). The marker is one quiet
/// line, never a table — the expandable transcript view is the approval/diff
/// slice's. `pub(crate)`: the approval section's call lines mirror the shape
/// (one home per fact — the marker format lives here).
pub(crate) fn tool_marker(call: &cadmus_contract::ToolCall) -> String {
    match tool_target(call) {
        Some(target) => format!("→ {} {target}", call.name),
        None => format!("→ {}", call.name),
    }
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
/// marker never wraps a target across rows.
fn truncate_cells(text: &str, max: usize) -> String {
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

/// A failure marker line.
fn error_line(text: impl Into<String>) -> ir::Line {
    ir::Line::from_spans(vec![ir::Span::slotted(text, Slot::Error)])
}

/// The resolution's display lines, shared by the live path and the attach
/// rebuild: approved calls quiet, rejected calls in the error slot. A
/// missing decision is a denial (the gate's short-reply rule), so it reads
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
        lines.push(subtle_line(format!("✓ approved {}", approved.join(", "))));
    }
    if !rejected.is_empty() {
        lines.push(error_line(format!("✗ rejected {}", rejected.join(", "))));
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

    /// One pump: snapshot, ack, return (flushed, live) as plain text.
    fn pump(transcript: &mut Transcript) -> (Vec<String>, Vec<String>) {
        let snapshot = snapshot(transcript);
        transcript.apply_flush(&snapshot.acks);
        (texts(&snapshot.flush_rows), texts(&snapshot.live_rows))
    }

    #[test]
    fn the_user_prompt_marker_uses_the_accent_slot() {
        let mut transcript = Transcript::new();
        transcript.push_user("hi");
        let snapshot = snapshot(&mut transcript);
        let row = &snapshot.flush_rows[0];
        assert_eq!(row.spans[0].content, "> ");
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["> fix the bug", "", "done"]);
        assert_eq!(live, Vec::<String>::new());
        // Nothing left: a second pump flushes nothing.
        let (flushed, live) = pump(&mut transcript);
        assert_eq!((flushed, live), (Vec::new(), Vec::new()));
    }

    #[test]
    fn the_open_tail_holds_its_incomplete_rows() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "one\n\ntwo\n"));
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["one", ""]);
        assert_eq!(live, vec!["two"]);
        // The held row flushes once the response seals the turn.
        transcript.apply_item(&llm_response(2, 1, "one\n\ntwo\n"));
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["two"]);
        assert_eq!(live, Vec::<String>::new());
    }

    #[test]
    fn tool_activity_sequences_between_turns() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&delta(1, 1, "reading\n"));
        transcript.apply_item(&llm_response(2, 1, "reading\n"));
        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        assert_eq!(
            transcript.apply_item(&recorded(3, 1, EventKind::ToolCall { call: call.clone() })),
            Light::Tool("read_file".into())
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["reading", "→ read_file"]);
        assert_eq!(live, vec!["found it"]);
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(
            flushed,
            vec![
                "→ read_file src/cursor.rs",
                "→ grep max_tokens",
                "→ list_dir",
            ]
        );
        assert_eq!(live, Vec::<String>::new());
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
            vec!["→ write_file", "✗ write_file: denied by the operator"]
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
                "✓ approved write_file",
                "→ write_file",
                "✓ completed write_file",
                "  updated file",
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
            ["✓ approved edit_file", "→ edit_file"],
            "settled approvals do not broaden ordinary success output"
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
            if approval_open {
                assert_eq!(
                    rows,
                    [
                        "→ write_file",
                        "→ edit_file",
                        "✗ failed write_file",
                        "  tool: write failed",
                        "  {\"retry\":false}"
                    ]
                );
            } else {
                assert_eq!(
                    rows,
                    ["→ write_file", "→ edit_file", "✗ write_file: write failed"]
                );
            }
            assert_eq!(transcript.live_calls.len(), 1);
            assert_eq!(transcript.live_calls["second"], "edit_file");
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
    fn an_unobserved_live_success_stays_quiet_during_a_pending_approval() {
        let mut transcript = Transcript::new();
        transcript.apply_item(&approval_request(1, "ap1", gated_calls()));
        transcript.apply_item(&durable_completion(2, &completion("unseen-span")));
        assert!(pump(&mut transcript).0.is_empty());
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
                "✓ approved edit_file",
                "→ write_file",
                "✓ completed write_file (turn 1, tool call 2)",
                "  updated file",
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
                "✗ failed write_file (turn 1, tool call 2)",
                "  tool: write failed",
                "  {\"retry\":false}"
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
            [
                "✓ completed write_file (turn 1, tool call 3)",
                "  another update"
            ]
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
                .filter(|row| row.contains("✓ completed"))
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
            let lines = completion_lines(&completion);
            assert!(lines.len() <= 6);
            assert!(lines.iter().map(|line| line.text().len()).sum::<usize>() < 1024);
            assert!(lines.last().unwrap().text().contains("truncated"));
            assert!(lines.iter().all(|line| !line.text().contains('\x1b')));
        }
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
        let (flushed, live) = pump(&mut transcript);
        // The rebuilt tail re-shows only what never left (one paragraph —
        // soft breaks join into one logical line), and nothing re-flushes.
        assert_eq!(flushed, Vec::<String>::new());
        assert_eq!(live, vec!["beta more"]);
        // The transferred prefix replays, the live rows do not.
        let replay =
            transcript.replay_tail(10, 80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(texts(&replay), vec!["alpha", ""]);
        // Items at or below the baseline are dropped (the client rule).
        assert_eq!(transcript.apply_item(&delta(3, 1, "stale\n")), Light::None);
        let (_, live) = pump(&mut transcript);
        assert_eq!(live, vec!["beta more"]);
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
        assert_eq!(flushed, vec!["> also check tests", ""]);
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(flushed, vec!["✓ approved write_file, edit_file"]);
        assert_eq!(live, Vec::<String>::new());
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
        assert_eq!(flushed, vec!["✓ approved ap1"]);
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
        assert_eq!(pump(&mut transcript).0, ["✓ approved edit_file"]);
        transcript.apply_item(&recorded(
            6,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "ap1".into(),
                decisions: Vec::new(),
            }),
        ));
        assert_eq!(pump(&mut transcript).0, ["✗ rejected write_file"]);
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
        assert_eq!(pump(&mut transcript).0, ["✗ rejected write_file"]);
        transcript.apply_item(&recorded(
            3,
            1,
            EventKind::Command(Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id: "ap1".into(),
                decisions: vec![Approval::Rejected { comment: None }, Approval::Approved],
            }),
        ));
        assert_eq!(pump(&mut transcript).0, ["✓ approved edit_file"]);
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
        assert_eq!(pump(&mut transcript).0, ["✓ approved edit_file"]);
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
        assert_eq!(pump(&mut transcript).0, ["✗ rejected write_file"]);
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
        let snapshot = snapshot(&mut transcript);
        let rows = texts(&snapshot.flush_rows);
        assert_eq!(rows, vec!["✓ approved write_file", "✗ rejected edit_file"]);
        let rejected = &snapshot.flush_rows[1];
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
            vec!["✓ approved write_file", "✗ rejected edit_file"]
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
        assert_eq!(flushed, vec!["✓ approved write_file, edit_file"]);
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(live, Vec::<String>::new());
        assert_eq!(
            flushed,
            vec![
                "> change it",
                "",
                "will do",
                "✓ approved write_file",
                "✗ rejected edit_file",
                "→ write_file",
                "→ edit_file",
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
        let (rows, live) = pump(&mut transcript);
        assert_eq!(
            rows,
            [
                "> change it",
                "",
                "current turn",
                "✓ approved write_file",
                "→ write_file",
                "→ write_file",
                "✓ completed write_file",
                "  first change written"
            ]
        );
        assert!(live.is_empty());
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
        assert_eq!(pending_result_names(&sync), HashMap::from([(2, "B")]));

        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ completed B", "  B completed"])
        );
        assert!(!rows.iter().any(|row| row == "✓ completed A"));
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
            HashMap::from([(5, "write_file")])
        );
        let mut transcript = Transcript::new();
        transcript.apply_sync(&sync, 80, highlighter(), false);
        let rows = pump(&mut transcript).0;
        assert!(!rows.iter().any(|row| row.contains("read result")));
        assert!(
            rows.windows(2)
                .any(|rows| rows == ["✓ completed write_file", "  first change written"])
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.contains("old success") || row.contains("later success"))
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
        let at = rows
            .iter()
            .position(|row| row == "✗ failed write_file")
            .unwrap();
        assert_eq!(rows[at + 1], "  first error line");
        assert_eq!(rows.last().unwrap(), "  ⋯ result preview truncated");
        assert_eq!(rows[at..].len(), 6);
    }

    #[test]
    fn successful_history_stays_quiet_without_an_open_anchored_batch() {
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
                !rows
                    .iter()
                    .any(|row| row.contains("first change written")
                        || row.starts_with("✓ completed"))
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
        assert_eq!(rows[current + 1], "✓ approved write_file, edit_file");
        assert_eq!(rows[next + 1], "✗ rejected edit_file");
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
            assert_eq!(rows.last().unwrap(), "✓ approved write_file, edit_file");
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
        let (flushed, live) = pump(&mut transcript);
        assert_eq!(live, Vec::<String>::new());
        assert_eq!(
            flushed,
            vec![
                "> change it",
                "",
                "✓ approved write_file",
                "✗ rejected edit_file",
            ]
        );
    }
}
