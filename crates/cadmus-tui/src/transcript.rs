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

use std::collections::HashMap;

use cadmus_contract::{
    Command, Event, EventKind, LiveItem, LiveKind, Message, Role, Status, Sync, attrs,
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
    /// call id → tool name, so a failed result's marker can name its tool.
    calls: HashMap<String, String>,
}

impl Transcript {
    #[must_use]
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            flushed: 0,
            open_turn: None,
            as_of_seq: 0,
            calls: HashMap::new(),
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
        lines.extend(text.lines().map(|line| {
            ir::Line::from_spans(vec![
                ir::Span::slotted("> ", Slot::TextSubtle),
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
            LiveKind::ApprovalRequested { .. } => {
                // Answered by the wiring's auto-resolver for now; the
                // interactive dialog lands with the approval slice.
                Light::None
            }
            LiveKind::Recorded { event } => self.apply_event(event),
        }
    }

    /// Apply the attach/re-attach baseline (ADR-0013 items 3–5). The app
    /// pumps first, so everything flushable is already in scrollback; the
    /// rebuilt history then lands pre-flushed (the old scrollback rendering
    /// is the same fold, deterministic) — content that fell into the lag
    /// hole stays unrendered there (headless parity: the trajectory log is
    /// the intact record; the app marks the hole by flushing a resync
    /// marker row directly, outside the block model, so replays never
    /// reorder it against transferred rows). Whether this attach IS a
    /// re-sync is the drainer's fact, carried on the feed — never
    /// re-derived from view state.
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
    pub fn apply_sync(&mut self, sync: &Sync, width: u16, highlighter: &Highlighter) {
        let transfer = match (self.open_turn, self.blocks.last()) {
            (Some(turn), Some(Block::Agent(agent))) => Some((turn, agent.acked)),
            _ => None,
        };
        self.blocks.clear();
        self.flushed = 0;
        self.open_turn = None;
        self.calls.clear();
        self.as_of_seq = sync.as_of_seq;

        for message in &sync.history.messages {
            self.push_history_message(message);
        }
        // History lands pre-flushed (see the doc comment).
        self.flushed = self.blocks.len();
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
        // `Sync.in_flight.pending_approvals` renders nothing yet (see
        // `apply_item`'s ApprovalRequested arm).
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
                let turn = turn_of(event);
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
                self.calls.insert(call.id.clone(), call.name.clone());
                self.blocks
                    .push(Block::Static(vec![subtle_line(format!("→ {}", call.name))]));
                Light::Tool(call.name.clone())
            }
            EventKind::ToolResult { call_id, .. } if event.status == Status::Error => {
                let tool = self
                    .calls
                    .get(call_id)
                    .map_or(call_id.as_str(), String::as_str);
                let detail = event
                    .error
                    .as_ref()
                    .and_then(|error| error.message.lines().next())
                    .unwrap_or("failed");
                self.blocks.push(Block::Static(vec![error_line(format!(
                    "✗ {tool}: {detail}"
                ))]));
                Light::None
            }
            EventKind::InstructionInjected { path, .. } => {
                self.blocks.push(Block::Static(vec![subtle_line(format!(
                    "+ instructions: {path}"
                ))]));
                Light::None
            }
            EventKind::Fold { folded, .. } => {
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
            EventKind::RunFinished { .. } => {
                if event.status == Status::Error {
                    let detail = event
                        .error
                        .as_ref()
                        .and_then(|error| error.message.lines().next())
                        .unwrap_or("failed");
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

    /// History rebuild for [`Transcript::apply_sync`]: messages map onto the
    /// same block shapes the live path builds (deterministic fold).
    fn push_history_message(&mut self, message: &Message) {
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
                for call in message.tool_calls() {
                    self.calls.insert(call.id.clone(), call.name.clone());
                    self.blocks
                        .push(Block::Static(vec![subtle_line(format!("→ {}", call.name))]));
                }
            }
            Role::Tool => {
                if message.is_error {
                    let call_id = message.tool_call_id.as_deref().unwrap_or("?");
                    let tool = self.calls.get(call_id).map_or(call_id, String::as_str);
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

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// A quiet activity/marker line.
fn subtle_line(text: impl Into<String>) -> ir::Line {
    ir::Line::from_spans(vec![ir::Span::slotted(text, Slot::TextSubtle)])
}

/// A failure marker line.
fn error_line(text: impl Into<String>) -> ir::Line {
    ir::Line::from_spans(vec![ir::Span::slotted(text, Slot::Error)])
}

/// The `selfevol.turn` attribute as the loop stamps it (1-based) — the same
/// helper the transport keeps private; the frontend cannot link core or
/// transport (ADR-0018 item 10), so the five lines live here too.
fn turn_of(event: &Event) -> Option<u32> {
    let value = event.attributes.get(attrs::TURN)?.as_u64()?;
    u32::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use cadmus_contract::{
        EventError, InFlight, LiveKind, OpenTurn, RunState, StreamChunk, ToolCall, TurnSnapshot,
    };

    use super::*;

    fn highlighter() -> &'static Highlighter {
        static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
        HIGHLIGHTER.get_or_init(Highlighter::new)
    }

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

    fn texts(rows: &[Line<'static>]) -> Vec<String> {
        rows.iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
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
            },
            as_of_seq: 4,
        };
        transcript.apply_sync(&sync, 80, highlighter());
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
}
