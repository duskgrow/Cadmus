//! The streaming-markdown pipeline (ADR-0018 item 4): the source is the
//! SSOT, rendering is derived. Deltas append to the source; only the
//! committed prefix (up to the last newline) is parsed; rendered logical
//! lines flush continuously once their shape can no longer change.
//!
//! The contract with the renderer, point by point of the ADR item:
//! 1. Newline gating: a trailing partial line never renders.
//! 2. Incremental rendering at top-level block boundaries: closed blocks
//!    render once and are cached; only the open tail re-renders. Parsing is
//!    a full reparse of the committed prefix per render — sanctioned by the
//!    item-1 admission record ("full reparse per chunk") — while the
//!    expensive work (syntect) stays incremental per fence line.
//! 3. The open-fence fast path continues syntect state per complete line;
//!    a maybe-closing line (trimmed-start begins with a backtick or tilde
//!    run) forces a full body re-render. False positives cost one re-render.
//! 4. Tables hold back from the header until the parser closes them, and
//!    transpose to key/value records when too narrow.
//! 5. `finalize` replaces the buffered source with the authoritative item:
//!    a saturated transport cannot truncate the transcript.
//! 6. Flush is a PREFIX of the live lines: one held line blocks everything
//!    after it. Flushed lines are drained by `ack_flushed` and never
//!    reappear in `live_lines`.
//! 7. Reclassifiable variants stay shape-identical: a heading renders with
//!    the same logical-line shape as its paragraph form (markers consumed,
//!    content bold); lists use uniform spacing, ignoring tight/loose. The
//!    remaining reclassification — reference-style link definitions — is
//!    zero-width inline styling: when the definition set changes, the
//!    render cache is invalidated back to the first unflushed line.
//!    Flushed rows keep their old styling (accepted cosmetic cost,
//!    self-healed by the next resize reflow).
//!
//! Flushability by block kind, while a block is the open tail: paragraphs,
//! quotes, HTML blocks and tables hold (a paragraph can lazy-continue or
//! reclassify as a setext heading; table column widths depend on all rows);
//! lists flush completed items (the open item can lazy-continue, so the
//! item is the unit); fence and indented-code bodies are literal and flush
//! line by line (except a trailing maybe-closing fence line, which could
//! still turn out to be the zero-row closer); headings and thematic breaks
//! are stable the moment they parse. Everything flushes at `finalize`.

use std::collections::{BTreeSet, HashMap};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::highlight::{FenceHighlighter, Highlighter, MAX_SNIPPET_BYTES};
use crate::ir::{Color, Line, Modifiers, Slot, Span, Style};

/// The streaming-markdown pipeline: source SSOT plus the render cache. See
/// the module docs for the invariants.
pub struct MarkdownStream {
    /// Everything ever pushed (or the finalized source); the SSOT.
    source: String,
    finalized: bool,
    /// Last render's width; the table fit decision depends on it, so a
    /// change invalidates the live render.
    width: Option<u16>,
    /// The reference-link definition set at the last render (item 4.7's
    /// invalidation trigger).
    ref_defs: BTreeSet<String>,
    /// The render cache: one entry per top-level block, never reordered.
    /// Fully flushed blocks stay as extent anchors so indices keep matching
    /// the parse and new blocks still get their separator.
    blocks: Vec<Block>,
    /// Flushed-line counts of dropped blocks, keyed by source start: a
    /// re-rendered block re-drops that many regenerated lines so flushed
    /// rows never reappear.
    pending_flushed: HashMap<usize, usize>,
    render: Render,
}

impl MarkdownStream {
    #[must_use]
    pub fn new() -> Self {
        Self {
            source: String::new(),
            finalized: false,
            width: None,
            ref_defs: BTreeSet::new(),
            blocks: Vec::new(),
            pending_flushed: HashMap::new(),
            render: Render::default(),
        }
    }

    /// Append a raw delta. Nothing renders until `render` — and then only
    /// up to the last newline (newline gating).
    pub fn push_delta(&mut self, text: &str) {
        self.source.push_str(text);
    }

    /// Replace the buffered source with the authoritative complete item and
    /// close every open unit (item 4.5). Assumes the buffered source is a
    /// prefix of `full_source` (the transport's contract); under divergence
    /// the invalidation degrades to re-rendering from the first live
    /// block's old offset.
    pub fn finalize(&mut self, full_source: &str) {
        self.source.clear();
        self.source.push_str(full_source);
        self.finalized = true;
        self.invalidate();
    }

    /// The buffered source (the SSOT), including any uncommitted partial
    /// trailing line.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The committed prefix of the source: everything up to the last
    /// newline — or the whole source once finalized. This is the region the
    /// renderer can ever have shown, so it is what a from-source replay
    /// (resize reflow) must render.
    #[must_use]
    pub fn committed_source(&self) -> &str {
        if self.finalized {
            &self.source
        } else {
            let committed_len = self.source.rfind('\n').map_or(0, |index| index + 1);
            &self.source[..committed_len]
        }
    }

    /// Render the committed prefix at `width`, advancing the incremental
    /// cache. The width steers only the table fit decision; everything else
    /// emits logical lines (wrapping is the renderer's job).
    pub fn render(&mut self, width: u16, highlighter: &Highlighter) -> &Render {
        let committed_len = if self.finalized {
            self.source.len()
        } else {
            self.source.rfind('\n').map_or(0, |index| index + 1)
        };
        if self.width != Some(width) {
            if self.width.is_some() {
                // Resize replays history from `source` via
                // `render_document`; here only the live tail re-renders.
                self.invalidate();
            }
            self.width = Some(width);
        }
        let ref_defs = scan_ref_defs(&self.source[..committed_len]);
        if ref_defs != self.ref_defs {
            self.ref_defs = ref_defs;
            self.invalidate();
        }
        let committed = &self.source[..committed_len];
        let drafts = parse_drafts(committed);
        let tail_open = !self.finalized && !last_line_is_blank(committed);
        reconcile(
            &mut self.blocks,
            &mut self.pending_flushed,
            committed,
            &drafts,
            tail_open,
            width,
            highlighter,
        );
        for block in &mut self.blocks {
            block.flushable = compute_flushable(block, self.finalized);
        }
        let mut live = Vec::new();
        for block in &self.blocks {
            live.extend(block.lines.iter().cloned());
        }
        let mut flushable_len = 0;
        for block in &self.blocks {
            let flushable = block.flushable.min(block.lines.len());
            flushable_len += flushable;
            if flushable < block.lines.len() {
                break;
            }
        }
        self.render = Render {
            live,
            flushable_len,
        };
        &self.render
    }

    /// Drop the first `n` live lines (they moved to scrollback).
    /// Precondition: `n <= Render::flushable_len` of the last render.
    pub fn ack_flushed(&mut self, lines: usize) {
        let n = lines.min(self.render.flushable_len);
        debug_assert_eq!(n, lines, "ack_flushed past the flushable prefix");
        let mut remaining = n;
        for block in &mut self.blocks {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(block.lines.len());
            block.lines.drain(..take);
            block.flushed += take;
            block.open_flushable = block.open_flushable.saturating_sub(take);
            remaining -= take;
        }
        self.render.live.drain(..n);
        self.render.flushable_len -= n;
    }

    /// Re-render everything from the first block that still has live lines;
    /// fully flushed blocks keep their rendering (item 4.7).
    fn invalidate(&mut self) {
        if let Some(first_live) = self.blocks.iter().position(|block| !block.lines.is_empty()) {
            drop_blocks_from(&mut self.blocks, &mut self.pending_flushed, first_live);
        }
    }
}

impl Default for MarkdownStream {
    fn default() -> Self {
        Self::new()
    }
}

/// A snapshot of the unflushed logical lines plus the flushable prefix
/// length. Owned by the stream; replaced by every `render`.
#[derive(Clone, Debug, Default)]
pub struct Render {
    live: Vec<Line>,
    flushable_len: usize,
}

impl Render {
    /// Unflushed logical lines, oldest first.
    #[must_use]
    pub fn live_lines(&self) -> &[Line] {
        &self.live
    }

    /// The length of the flushable PREFIX of `live_lines` (item 4.6).
    #[must_use]
    pub fn flushable_len(&self) -> usize {
        self.flushable_len
    }
}

/// One-shot full render of a complete document — resize replay from the
/// source SSOT, and the test oracle for the streaming path.
#[must_use]
pub fn render_document(source: &str, width: u16, highlighter: &Highlighter) -> Vec<Line> {
    let mut stream = MarkdownStream::new();
    stream.finalize(source);
    stream.render(width, highlighter).live_lines().to_vec()
}

// ============================================================================
// Block extents and the render cache
// ============================================================================

/// A top-level block's identity. Equality is part of the cache contract: a
/// same-start block whose kind changed (a paragraph reclassified as a
/// setext heading) is dropped and re-rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Heading,
    Quote,
    Fence {
        /// The info string's first word; may be unknown to syntect.
        lang: String,
    },
    IndentedCode,
    List {
        start: Option<u64>,
    },
    Table {
        cols: usize,
    },
    Rule,
    Html,
    /// Block kinds this option set cannot produce; renders zero lines.
    Other,
}

impl Kind {
    fn from_tag(tag: &Tag) -> Self {
        match tag {
            Tag::Paragraph => Self::Paragraph,
            Tag::Heading { .. } => Self::Heading,
            Tag::BlockQuote(_) => Self::Quote,
            Tag::CodeBlock(CodeBlockKind::Fenced(info)) => Self::Fence {
                lang: info.split_whitespace().next().unwrap_or("").to_string(),
            },
            Tag::CodeBlock(CodeBlockKind::Indented) => Self::IndentedCode,
            Tag::List(start) => Self::List { start: *start },
            Tag::Table(alignments) => Self::Table {
                cols: alignments.len(),
            },
            Tag::HtmlBlock => Self::Html,
            _ => Self::Other,
        }
    }
}

/// A top-level block's source extent and kind — the incrementality unit.
/// Events are re-collected on demand, for dirty drafts only.
#[derive(Debug)]
struct Draft {
    start: usize,
    end: usize,
    kind: Kind,
}

fn parser_options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// Split the committed source into top-level block extents. `Rule` is the
/// one block-level leaf event; every other block arrives as a Start/End
/// pair (balanced even at the committed end of input).
fn parse_drafts(src: &str) -> Vec<Draft> {
    let mut drafts = Vec::new();
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(src, parser_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    drafts.push(Draft {
                        start: range.start,
                        end: range.end,
                        kind: Kind::from_tag(&tag),
                    });
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(draft) = drafts.last_mut()
                {
                    draft.end = range.end;
                }
            }
            Event::Rule if depth == 0 => {
                drafts.push(Draft {
                    start: range.start,
                    end: range.end,
                    kind: Kind::Rule,
                });
            }
            _ => {}
        }
    }
    drafts
}

/// Re-parse and collect the inner events of the drafts whose start offsets
/// are in `wanted` — one pass for all dirty blocks, and only those pay the
/// clone into owned events.
fn collect_block_events(
    src: &str,
    wanted: &BTreeSet<usize>,
) -> HashMap<usize, Vec<Event<'static>>> {
    let mut out: HashMap<usize, Vec<Event<'static>>> = HashMap::new();
    let mut depth = 0usize;
    let mut current: Option<usize> = None;
    for (event, range) in Parser::new_ext(src, parser_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 && wanted.contains(&range.start) {
                    current = Some(range.start);
                    out.insert(range.start, Vec::new());
                } else if let Some(key) = current
                    && let Some(events) = out.get_mut(&key)
                {
                    events.push(Event::Start(tag.into_static()));
                }
                depth += 1;
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    current = None;
                } else if let Some(key) = current
                    && let Some(events) = out.get_mut(&key)
                {
                    events.push(Event::End(tag));
                }
            }
            event => {
                if depth > 0
                    && let Some(key) = current
                    && let Some(events) = out.get_mut(&key)
                {
                    events.push(event.into_static());
                }
            }
        }
    }
    out
}

/// One cached top-level block: the rendered live lines plus the bookkeeping
/// for incremental updates, flush computation, and re-render alignment.
struct Block {
    start: usize,
    end: usize,
    kind: Kind,
    closed: bool,
    /// A separator line precedes the content (every block but the first).
    /// It flushes with the earlier block (item 4.6).
    sep: bool,
    /// Lines flushed out of this block since `start` (monotonic): the live
    /// `lines` are the conceptual render minus its leading `flushed` lines,
    /// and re-renders re-drop exactly that many regenerated lines.
    flushed: usize,
    /// The live lines: the conceptual render without the flushed prefix.
    lines: Vec<Line>,
    /// Leading flushable count of `lines` (recomputed by
    /// `compute_flushable`).
    flushable: usize,
    /// Leading flushable count of `lines` if the block is the open tail
    /// (fences instead compute theirs from the trailing line's shape).
    open_flushable: usize,
    /// Open-fence fast-path state (item 4.3).
    fence: Option<FenceState>,
}

/// Incremental state of one open fence.
struct FenceState {
    /// `None` = plain mode (unknown language): lines render literal, so no
    /// maybe-closing fallback is ever needed.
    hl: Option<FenceHighlighter>,
    /// Body lines rendered so far, counting flushed ones.
    body_lines: usize,
}

/// A block's rendered content (no separator).
struct Content {
    lines: Vec<Line>,
    /// Leading flushable count over `lines` if the block is open.
    open_flushable: usize,
    fence: Option<FenceState>,
}

impl Content {
    fn plain(lines: Vec<Line>, open_flushable: usize) -> Self {
        Self {
            lines,
            open_flushable,
            fence: None,
        }
    }
}

/// Assemble a block's live lines from a full content render, realigning to
/// the flushed prefix: the regenerated leading `flushed` lines are dropped
/// again so already-flushed rows never reappear (item 4.7).
fn assemble(
    sep: bool,
    mut content: Vec<Line>,
    open_flushable: usize,
    flushed: usize,
) -> (Vec<Line>, usize) {
    if sep {
        content.insert(0, Line::default());
    }
    let conceptual = usize::from(sep) + open_flushable;
    let drop = flushed.min(content.len());
    content.drain(..drop);
    (content, conceptual.saturating_sub(drop))
}

/// Reconcile the cache with a fresh parse: clean prefix (extent and kind
/// identical), one tail-growth update, then drop-and-re-render from the
/// first mismatch. Closed blocks never change their extent — the parser
/// guarantees block boundaries are stable once a following block exists.
fn reconcile(
    blocks: &mut Vec<Block>,
    pending_flushed: &mut HashMap<usize, usize>,
    src: &str,
    drafts: &[Draft],
    tail_open: bool,
    width: u16,
    highlighter: &Highlighter,
) {
    let mut i = 0;
    while i < drafts.len() && i < blocks.len() {
        let block = &blocks[i];
        let draft = &drafts[i];
        if block.start == draft.start && block.end == draft.end && block.kind == draft.kind {
            blocks[i].closed = closed_at(src, drafts, i, tail_open);
            i += 1;
        } else {
            break;
        }
    }
    if i < drafts.len() {
        // Everything from here on re-renders; collect events in one parse.
        let wanted: BTreeSet<usize> = drafts[i..].iter().map(|draft| draft.start).collect();
        let events_map = collect_block_events(src, &wanted);
        // Tail growth: the last cached block kept its start and kind.
        if i < blocks.len()
            && i + 1 == blocks.len()
            && blocks[i].start == drafts[i].start
            && blocks[i].kind == drafts[i].kind
        {
            let events = events_map
                .get(&drafts[i].start)
                .map_or(&[][..], Vec::as_slice);
            let closed = closed_at(src, drafts, i, tail_open);
            update_block(
                &mut blocks[i],
                &drafts[i],
                events,
                closed,
                width,
                highlighter,
            );
            i += 1;
        }
        if i < blocks.len() {
            drop_blocks_from(blocks, pending_flushed, i);
        }
        for (j, draft) in drafts.iter().enumerate().skip(i) {
            let events = events_map.get(&draft.start).map_or(&[][..], Vec::as_slice);
            let closed = closed_at(src, drafts, j, tail_open);
            blocks.push(render_block(
                j,
                draft,
                events,
                closed,
                pending_flushed,
                width,
                highlighter,
            ));
        }
    }
    if drafts.len() < blocks.len() {
        // Only reachable if the source diverged under `finalize`; drop the
        // orphans rather than trust their extents.
        drop_blocks_from(blocks, pending_flushed, drafts.len());
    }
}

/// A draft is closed when a later block exists, when the committed source
/// ends with a blank line, or (fences) when its closer line arrived —
/// except that a trailing blank line is fence BODY, never a closer: a fence
/// closes only on its closer line, a following block, or `finalize`. Marking
/// a blank-rich open fence closed would discard its incremental syntect
/// state and rebuild it per blank line (measured quadratic on the streaming
/// hot path — the fast path item 4 exists to protect).
fn closed_at(src: &str, drafts: &[Draft], i: usize, tail_open: bool) -> bool {
    i + 1 < drafts.len()
        || match &drafts[i].kind {
            Kind::Fence { .. } => fence_closed(src, &drafts[i]),
            _ => !tail_open,
        }
}

/// Fresh-render one block.
fn render_block(
    index: usize,
    draft: &Draft,
    events: &[Event<'static>],
    closed: bool,
    pending_flushed: &mut HashMap<usize, usize>,
    width: u16,
    highlighter: &Highlighter,
) -> Block {
    let sep = index > 0;
    let content = render_content(&draft.kind, events, closed, width, highlighter);
    let flushed = pending_flushed.remove(&draft.start).unwrap_or(0);
    let (lines, open_flushable) = assemble(sep, content.lines, content.open_flushable, flushed);
    Block {
        start: draft.start,
        end: draft.end,
        kind: draft.kind.clone(),
        closed,
        sep,
        flushed,
        lines,
        flushable: 0,
        open_flushable,
        fence: content.fence,
    }
}

/// Re-render the open tail in place after its extent grew (or it closed).
/// Non-fence blocks re-render whole — their rendering is cheap; the fence
/// keeps its incremental syntect state (item 4.3).
fn update_block(
    block: &mut Block,
    draft: &Draft,
    events: &[Event<'static>],
    closed: bool,
    width: u16,
    highlighter: &Highlighter,
) {
    block.end = draft.end;
    if matches!(block.kind, Kind::Fence { .. }) && !block.closed {
        // Catches the body up incrementally; when only the closer line
        // arrived there is nothing new (it renders zero rows) and the
        // fast-path lines stand — identical to a fresh `highlight_snippet`.
        fence_update(block, events, highlighter);
    } else {
        let content = render_content(&block.kind, events, closed, width, highlighter);
        let (lines, open_flushable) = assemble(
            block.sep,
            content.lines,
            content.open_flushable,
            block.flushed,
        );
        block.lines = lines;
        block.open_flushable = open_flushable;
        block.fence = content.fence;
    }
    block.closed = closed;
}

/// Drop cached blocks from `from`, preserving each block's flushed-line
/// count so a later re-render at the same source start re-aligns.
fn drop_blocks_from(
    blocks: &mut Vec<Block>,
    pending_flushed: &mut HashMap<usize, usize>,
    from: usize,
) {
    for block in blocks.drain(from..) {
        if block.flushed > 0 {
            pending_flushed.insert(block.start, block.flushed);
        }
    }
}

/// The leading flushable count of a block's live lines (item 4.6).
fn compute_flushable(block: &Block, finalized: bool) -> usize {
    if block.closed || finalized {
        return block.lines.len();
    }
    if matches!(block.kind, Kind::Fence { .. }) {
        // Body lines are literal: flushable even while the fence is open,
        // except a trailing maybe-closing line — it could still turn out to
        // be the closer, which renders zero rows (a shape change).
        let sep_live = usize::from(block.sep && block.flushed == 0);
        let body_live = block.lines.len() - sep_live;
        let hold = match block.lines.last() {
            Some(last) if body_live > 0 && maybe_closing(&last.text()) => 1,
            _ => 0,
        };
        return block.lines.len() - hold;
    }
    block.open_flushable.min(block.lines.len())
}

/// Render a block's content (no separator) from its events.
fn render_content(
    kind: &Kind,
    events: &[Event<'static>],
    closed: bool,
    width: u16,
    highlighter: &Highlighter,
) -> Content {
    match kind {
        Kind::Paragraph => Content::plain(render_inlines(events, Modifiers::default()), 0),
        Kind::Heading => {
            let lines = render_inlines(events, bold_mods());
            // Shape-identical to the paragraph form and unreclassifiable
            // once parsed: flushable even as the open tail.
            let open_flushable = lines.len();
            Content::plain(lines, open_flushable)
        }
        Kind::Quote => Content::plain(render_quote(events, width, highlighter), 0),
        Kind::Fence { lang } => {
            let body = code_body(events);
            if closed {
                Content::plain(highlighter.highlight_snippet(lang, &body), 0)
            } else {
                let (lines, state) = replay_fence(lang, &body, highlighter);
                Content {
                    lines,
                    // Computed per render from the trailing line's shape.
                    open_flushable: 0,
                    fence: Some(state),
                }
            }
        }
        // Literal lines: stable even while open (a trailing line cannot be
        // reclassified once the parser made it indented code).
        Kind::IndentedCode => {
            let lines: Vec<Line> = code_body(events).lines().map(Line::plain).collect();
            let open_flushable = lines.len();
            Content::plain(lines, open_flushable)
        }
        Kind::List { start } => {
            let (lines, last_item_start) = render_list(events, *start, 0, width, highlighter);
            Content::plain(lines, last_item_start)
        }
        Kind::Table { cols } => Content::plain(render_table(events, *cols, width), 0),
        Kind::Rule => Content::plain(vec![thematic_break_line()], 1),
        Kind::Html => Content::plain(html_lines(events), 0),
        Kind::Other => Content::plain(Vec::new(), 0),
    }
}

/// Advance an open fence (item 4.3): new complete body lines continue the
/// syntect state; a maybe-closing line among them forces a full body
/// re-render. Plain mode (unknown language) appends literal lines and never
/// needs the fallback.
fn fence_update(block: &mut Block, events: &[Event<'static>], highlighter: &Highlighter) {
    let Kind::Fence { lang } = &block.kind else {
        return;
    };
    let body = code_body(events);
    let total = body.lines().count();
    let Some(state) = &mut block.fence else {
        return;
    };
    if total <= state.body_lines {
        return;
    }
    let lang = lang.clone();
    let new: Vec<&str> = body.lines().skip(state.body_lines).collect();
    let fallback = new.iter().any(|line| maybe_closing(line));
    match &mut state.hl {
        Some(hl) if !fallback => {
            for line in new {
                let rendered = hl.push_line(line, highlighter);
                block.lines.push(rendered);
            }
            state.body_lines = total;
        }
        None => {
            for line in new {
                block.lines.push(Line::plain(line));
            }
            state.body_lines = total;
        }
        _ => {
            let (rendered, fresh_state) = replay_fence(&lang, &body, highlighter);
            let (lines, _) = assemble(block.sep, rendered, 0, block.flushed);
            block.lines = lines;
            *state = fresh_state;
        }
    }
}

/// Full body render retaining the resumed state: the initial render of an
/// open fence and the maybe-closing fallback. Over-limit bodies render
/// plain (mirrors `Highlighter::highlight_snippet`'s guard).
fn replay_fence(lang: &str, body: &str, highlighter: &Highlighter) -> (Vec<Line>, FenceState) {
    let body_lines = body.lines().count();
    let plain = || {
        (
            body.lines().map(Line::plain).collect::<Vec<_>>(),
            FenceState {
                hl: None,
                body_lines,
            },
        )
    };
    if body.len() > MAX_SNIPPET_BYTES {
        return plain();
    }
    match highlighter.open_fence(lang) {
        Some(mut fence) => {
            let lines = body
                .lines()
                .map(|line| fence.push_line(line, highlighter))
                .collect();
            (
                lines,
                FenceState {
                    hl: Some(fence),
                    body_lines,
                },
            )
        }
        None => plain(),
    }
}

// ============================================================================
// Block-kind renderers
// ============================================================================

/// The literal body of a code block: pulldown's Text events carry the raw
/// lines (indented blocks already stripped of their four-space indent), so
/// the fence opener/closer never leak into the body.
fn code_body(events: &[Event<'static>]) -> String {
    let mut body = String::new();
    for event in events {
        if let Event::Text(text) = event {
            body.push_str(text);
        }
    }
    body
}

/// A line whose trimmed-start begins with a backtick or tilde run: it might
/// close the open fence — or be content; false positives cost one
/// re-render.
fn maybe_closing(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('`') || trimmed.starts_with('~')
}

/// Parse a fence opener line: a trimmed-start run of at least three
/// backticks or tildes. Returns the fence character and run length.
fn fence_opener(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let &first = trimmed.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let run = trimmed.bytes().take_while(|&b| b == first).count();
    (run >= 3).then_some((first, run))
}

/// Whether the fence draft closed with a fence line (as opposed to being
/// open at the committed end). The opener cannot be its own closer, so
/// detection needs at least two extent lines.
fn fence_closed(src: &str, draft: &Draft) -> bool {
    let extent = &src[draft.start..draft.end.min(src.len())];
    let mut lines = extent.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let Some((ch, run)) = fence_opener(first) else {
        return false;
    };
    let Some(last) = lines.next_back() else {
        return false;
    };
    let trimmed = last.trim_start();
    let closer = trimmed.bytes().take_while(|&b| b == ch).count();
    closer >= run && trimmed[closer..].trim().is_empty()
}

/// Render a nested block group at absolute indent `base` with `width`
/// columns available after that indent — the recursive renderer for
/// list-item and quote children. Top-level blocks go through
/// `render_content` instead (the stateful fence fast path lives there).
fn render_group(group: &Group, base: usize, width: u16, highlighter: &Highlighter) -> Vec<Line> {
    let lines = match &group.kind {
        Kind::List { start } => {
            return render_list(group.events, *start, base, width, highlighter).0;
        }
        Kind::Paragraph => render_inlines(group.events, Modifiers::default()),
        Kind::Heading => render_inlines(group.events, bold_mods()),
        Kind::Quote => render_quote(group.events, width, highlighter),
        // Nested fences re-render whole via the one-shot path: the fast
        // path is reserved for top-level fences (the streaming case).
        Kind::Fence { lang } => highlighter.highlight_snippet(lang, &code_body(group.events)),
        Kind::IndentedCode => code_body(group.events).lines().map(Line::plain).collect(),
        Kind::Table { cols } => render_table(group.events, *cols, width),
        Kind::Rule => vec![thematic_break_line()],
        Kind::Html => html_lines(group.events),
        Kind::Other => Vec::new(),
    };
    with_indent(lines, base)
}

/// Block quotes: every content line gets a `> ` prefix in `TextSubtle`, the
/// content styled normally after it. Nested blocks keep their shape (item
/// 4.6); child blocks are separated by one blank (prefixed) line, mirroring
/// the top-level separation rule. The fit width shrinks by the prefix.
fn render_quote(events: &[Event<'static>], width: u16, highlighter: &Highlighter) -> Vec<Line> {
    let inner_width = width.saturating_sub(2);
    let mut lines: Vec<Line> = Vec::new();
    for group in group_blocks(events) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(render_group(&group, 0, inner_width, highlighter));
    }
    for line in &mut lines {
        line.spans.insert(0, Span::slotted("> ", Slot::TextSubtle));
    }
    lines
}

/// A list with uniform spacing (tight/loose ignored, item 4.7): no blank
/// lines anywhere inside the block. Unordered markers are `- `; ordered
/// markers keep the source's start number (pulldown normalizes the
/// numbering and the `)` delimiter). Nesting indents two spaces per level;
/// an item's continuation lines align under its content. Returns the
/// content lines plus the index where the last item begins — the
/// open-item flush boundary (completed items flush when the next item
/// starts; the last item can still lazy-continue).
fn render_list(
    events: &[Event<'static>],
    start: Option<u64>,
    base: usize,
    width: u16,
    highlighter: &Highlighter,
) -> (Vec<Line>, usize) {
    let mut lines: Vec<Line> = Vec::new();
    let mut last_item_start = 0usize;
    let mut ordinal = start;
    for item_events in item_groups(events) {
        let marker = match ordinal {
            Some(n) => {
                ordinal = Some(n.saturating_add(1));
                format!("{n}. ")
            }
            None => "- ".to_string(),
        };
        let marker_width = marker.len(); // ASCII markers: len == width
        last_item_start = lines.len();
        let mut item_lines: Vec<Line> = Vec::new();
        for group in group_blocks(item_events) {
            // Nested lists indent two spaces per level; every other child
            // aligns under the item's content.
            let child_indent = if matches!(group.kind, Kind::List { .. }) {
                2
            } else {
                marker_width
            };
            let child_base = base + child_indent;
            let child_width = width.saturating_sub(u16::try_from(child_base).unwrap_or(u16::MAX));
            let mut child_lines = render_group(&group, child_base, child_width, highlighter);
            if item_lines.is_empty()
                && let Some(first_line) = child_lines.first_mut()
            {
                splice_marker(first_line, base, &marker, child_base);
            }
            item_lines.extend(child_lines);
        }
        if item_lines.is_empty() {
            // An empty item renders as its bare marker.
            item_lines.push(Line::plain(format!(
                "{}{}",
                " ".repeat(base),
                marker.trim_end()
            )));
        }
        lines.extend(item_lines);
    }
    (lines, last_item_start)
}

/// Replace a line's leading indent with the item marker, padding back to
/// the child indent. For wide ordered markers above a nested list the
/// padding is zero and the nest shifts right by the difference (a cosmetic
/// corner; the two-space rule targets the canonical unordered case).
fn splice_marker(line: &mut Line, base: usize, marker: &str, child_base: usize) {
    let mut prefix = format!("{}{}", " ".repeat(base), marker);
    while prefix.len() < child_base {
        prefix.push(' ');
    }
    match line.spans.first_mut() {
        Some(first) if first.style == Style::default() && first.text.trim().is_empty() => {
            first.text = prefix;
        }
        _ => line.spans.insert(0, Span::plain(prefix)),
    }
}

/// Split a list's events into per-item inner slices (`Start(Item)` to its
/// matching `End(Item)`).
fn item_groups<'a>(events: &'a [Event<'static>]) -> Vec<&'a [Event<'static>]> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if matches!(events[i], Event::Start(Tag::Item)) {
            let mut depth = 1usize;
            let mut j = i + 1;
            while j < events.len() && depth > 0 {
                match events[j] {
                    Event::Start(_) => depth += 1,
                    Event::End(_) => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            items.push(&events[i + 1..j.saturating_sub(1)]);
            i = j;
        } else {
            i += 1; // nothing but items lives directly in a list
        }
    }
    items
}

/// A GFM pipe table. Natural column widths are the max cell display width
/// (plain text; styles are zero-width). Rows render padded to column width
/// and joined with a subtle ` | ` — no outer pipes, the header row bold;
/// alignment markers are ignored. When the padded total exceeds `width`
/// the table transposes to key/value records (item 4.4).
fn render_table(events: &[Event<'static>], cols: usize, width: u16) -> Vec<Line> {
    let (header, rows) = table_cells(events);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; cols];
    for (ci, cell) in header.iter().enumerate().take(cols) {
        widths[ci] = widths[ci].max(cell.width());
    }
    for row in &rows {
        for (ci, cell) in row.iter().enumerate().take(cols) {
            widths[ci] = widths[ci].max(cell.width());
        }
    }
    let total = widths.iter().sum::<usize>() + 3 * cols.saturating_sub(1);
    if total > usize::from(width) {
        return transpose_table(&header, &rows, cols);
    }
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(flat_row(&header, &widths));
    for row in &rows {
        lines.push(flat_row(row, &widths));
    }
    lines
}

/// One table row: cells padded to column width, joined with a subtle
/// ` | `. The last column is not padded (trailing spaces buy nothing).
fn flat_row(cells: &[Line], widths: &[usize]) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    for (ci, width) in widths.iter().enumerate() {
        if ci > 0 {
            spans.push(Span::slotted(" | ", Slot::TextSubtle));
        }
        match cells.get(ci) {
            Some(cell) => {
                spans.extend(cell.spans.iter().cloned());
                let pad = width.saturating_sub(cell.width());
                if ci + 1 < widths.len() && pad > 0 {
                    spans.push(Span::plain(" ".repeat(pad)));
                }
            }
            None if ci + 1 < widths.len() => {
                spans.push(Span::plain(" ".repeat(*width)));
            }
            None => {}
        }
    }
    Line::from_spans(spans)
}

/// The narrow fit: one logical line per cell, `header: cell`, records
/// back-to-back (item 4.4 — this doubles as the table's streaming
/// presentation once settled). Cell inline styling is preserved.
fn transpose_table(header: &[Line], rows: &[Vec<Line>], cols: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    for row in rows {
        for ci in 0..cols {
            let mut spans: Vec<Span> = Vec::new();
            if let Some(head) = header.get(ci) {
                spans.extend(head.spans.iter().cloned());
            }
            spans.push(Span::plain(": "));
            if let Some(cell) = row.get(ci) {
                spans.extend(cell.spans.iter().cloned());
            }
            lines.push(Line::from_spans(spans));
        }
    }
    lines
}

/// Extract header and body cells; each cell is its inline content rendered
/// to one logical fragment (GFM cells hold no block structure). The header
/// renders bold.
fn table_cells(events: &[Event<'static>]) -> (Vec<Line>, Vec<Vec<Line>>) {
    let mut header: Vec<Line> = Vec::new();
    let mut rows: Vec<Vec<Line>> = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableHead) => {
                let (cells, next) = read_cells(events, i + 1, TagEnd::TableHead);
                header = cells.iter().map(|&cell| render_cell(cell, true)).collect();
                i = next;
            }
            Event::Start(Tag::TableRow) => {
                let (cells, next) = read_cells(events, i + 1, TagEnd::TableRow);
                rows.push(cells.iter().map(|&cell| render_cell(cell, false)).collect());
                i = next;
            }
            _ => i += 1,
        }
    }
    (header, rows)
}

/// Collect the cell event slices of one head/body section until `end`.
fn read_cells<'a>(
    events: &'a [Event<'static>],
    mut i: usize,
    end: TagEnd,
) -> (Vec<&'a [Event<'static>]>, usize) {
    let mut cells = Vec::new();
    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::TableCell) => {
                let mut j = i + 1;
                while j < events.len() && !matches!(events[j], Event::End(TagEnd::TableCell)) {
                    j += 1;
                }
                cells.push(&events[i + 1..j]);
                i = j + 1;
            }
            Event::End(tag_end) if *tag_end == end => return (cells, i + 1),
            _ => i += 1,
        }
    }
    (cells, i)
}

/// A cell renders with its inline styling preserved; the header is bold.
fn render_cell(events: &[Event<'static>], bold: bool) -> Line {
    let base = if bold {
        bold_mods()
    } else {
        Modifiers::default()
    };
    let mut lines = render_inlines(events, base);
    if lines.len() <= 1 {
        return lines.pop().unwrap_or_default();
    }
    // GFM cells cannot produce hard breaks; fold defensively.
    let mut spans = lines.remove(0).spans;
    for line in lines {
        spans.push(Span::plain(" "));
        spans.extend(line.spans);
    }
    Line::from_spans(spans)
}

/// One subtle `---` line for a thematic break.
fn thematic_break_line() -> Line {
    Line::from_spans(vec![Span::slotted("---", Slot::TextSubtle)])
}

/// HTML blocks render literal (the `html` feature stays off — ADR-0018
/// item 1's admission record). Each Html event carries one source line
/// including its break.
fn html_lines(events: &[Event<'static>]) -> Vec<Line> {
    let mut text = String::new();
    for event in events {
        if let Event::Html(chunk) = event {
            text.push_str(chunk);
        }
    }
    text.lines().map(Line::plain).collect()
}

/// Prepend an absolute indent to every line (no-op at zero).
fn with_indent(mut lines: Vec<Line>, base: usize) -> Vec<Line> {
    if base == 0 {
        return lines;
    }
    let prefix = " ".repeat(base);
    for line in &mut lines {
        line.spans.insert(0, Span::plain(prefix.clone()));
    }
    lines
}

fn bold_mods() -> Modifiers {
    Modifiers {
        bold: true,
        ..Modifiers::default()
    }
}

// ============================================================================
// Inline rendering
// ============================================================================

/// One inline style mark on the modifier stack. `Image` blanks the style:
/// alt text renders plain.
enum Mark {
    Mods(TagEnd, Modifiers),
    Image,
}

/// The style under the mark stack: modifiers combine through nesting; an
/// enclosing image resets to plain.
fn current_style(stack: &[Mark], base: Modifiers) -> Style {
    if stack.iter().any(|mark| matches!(mark, Mark::Image)) {
        return Style::default();
    }
    let mut mods = base;
    for mark in stack {
        if let Mark::Mods(_, mark_mods) = mark {
            mods.bold |= mark_mods.bold;
            mods.italic |= mark_mods.italic;
            mods.underline |= mark_mods.underline;
            mods.strikethrough |= mark_mods.strikethrough;
        }
    }
    Style {
        fg: None,
        bg: None,
        mods,
    }
}

/// Append a styled run, merging with the previous span on equal style.
fn push_span(spans: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.text.push_str(text);
        return;
    }
    spans.push(Span {
        text: text.to_string(),
        style,
    });
}

/// Render inline events to logical lines (one per hard-break-separated
/// row). `base` seeds the modifiers (headings and table headers render
/// bold).
fn render_inlines(events: &[Event<'static>], base: Modifiers) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut stack: Vec<Mark> = Vec::new();
    for event in events {
        match event {
            Event::Start(tag) => match tag {
                Tag::Strong => stack.push(Mark::Mods(
                    TagEnd::Strong,
                    Modifiers {
                        bold: true,
                        ..Modifiers::default()
                    },
                )),
                Tag::Emphasis => stack.push(Mark::Mods(
                    TagEnd::Emphasis,
                    Modifiers {
                        italic: true,
                        ..Modifiers::default()
                    },
                )),
                Tag::Strikethrough => stack.push(Mark::Mods(
                    TagEnd::Strikethrough,
                    Modifiers {
                        strikethrough: true,
                        ..Modifiers::default()
                    },
                )),
                // Link text underlines; the destination never shows inline.
                Tag::Link { .. } => stack.push(Mark::Mods(
                    TagEnd::Link,
                    Modifiers {
                        underline: true,
                        ..Modifiers::default()
                    },
                )),
                Tag::Image { .. } => stack.push(Mark::Image),
                _ => {}
            },
            Event::End(end) => {
                let matched = match stack.last() {
                    Some(Mark::Mods(tag_end, _)) => tag_end == end,
                    Some(Mark::Image) => *end == TagEnd::Image,
                    None => false,
                };
                if matched {
                    stack.pop();
                }
            }
            Event::Text(text) => push_span(&mut spans, text, current_style(&stack, base)),
            Event::Code(text) => {
                // Inline code: subtle foreground (accent restraint,
                // ADR-0017), modifiers composing with the surroundings.
                let mut style = current_style(&stack, base);
                if !stack.iter().any(|mark| matches!(mark, Mark::Image)) {
                    style.fg = Some(Color::Slot(Slot::TextSubtle));
                }
                push_span(&mut spans, text, style);
            }
            Event::SoftBreak => push_span(&mut spans, " ", current_style(&stack, base)),
            Event::HardBreak => lines.push(Line::from_spans(std::mem::take(&mut spans))),
            // Literal text (the `html` feature stays off).
            Event::InlineHtml(text) | Event::Html(text) => {
                push_span(&mut spans, text, current_style(&stack, base));
            }
            _ => {}
        }
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from_spans(spans));
    }
    lines
}

/// A nested block group: the child blocks of a list item or a quote.
struct Group<'a> {
    kind: Kind,
    events: &'a [Event<'static>],
}

/// Split child events into block groups. Tight-list items carry their
/// paragraph content as bare inline events (pulldown emits no Paragraph
/// tags for them), so a run of inline-level events groups as an implicit
/// paragraph — this is what makes tight and loose lists render identically
/// (uniform spacing, item 4.7).
fn group_blocks<'a>(events: &'a [Event<'static>]) -> Vec<Group<'a>> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Start(tag) if !is_inline_tag(tag) => {
                let mut depth = 1usize;
                let mut j = i + 1;
                while j < events.len() && depth > 0 {
                    match events[j] {
                        Event::Start(_) => depth += 1,
                        Event::End(_) => depth -= 1,
                        _ => {}
                    }
                    j += 1;
                }
                groups.push(Group {
                    kind: Kind::from_tag(tag),
                    events: &events[i + 1..j.saturating_sub(1)],
                });
                i = j;
            }
            Event::Rule => {
                groups.push(Group {
                    kind: Kind::Rule,
                    events: &[],
                });
                i += 1;
            }
            _ => {
                let mut j = i;
                while j < events.len() {
                    match &events[j] {
                        Event::Start(tag) if !is_inline_tag(tag) => break,
                        Event::Rule => break,
                        _ => j += 1,
                    }
                }
                groups.push(Group {
                    kind: Kind::Paragraph,
                    events: &events[i..j],
                });
                i = j;
            }
        }
    }
    groups
}

fn is_inline_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

// ============================================================================
// Stream-level helpers
// ============================================================================

/// The reference-link definition set of the committed source — item 4.7's
/// invalidation trigger. Hand-rolled: `[label]: dest` lines, kept whole and
/// trimmed. Conservative misses (a definition-looking line inside a fence
/// false-positives; a multi-line title is invisible) cost at most a
/// spurious or skipped re-render — link resolution itself is always the
/// parser's.
fn scan_ref_defs(src: &str) -> BTreeSet<String> {
    src.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let rest = trimmed.strip_prefix('[')?;
            let close = rest.find("]:")?;
            let label = &rest[..close];
            let dest = rest[close + 2..].trim();
            if label.is_empty() || dest.is_empty() {
                return None;
            }
            Some(trimmed.to_string())
        })
        .collect()
}

/// Newline-gated openness: the last block stays open (re-rendered on the
/// next delta) unless the committed source ends with a blank line.
fn last_line_is_blank(committed: &str) -> bool {
    committed
        .lines()
        .next_back()
        .is_none_or(|line| line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    fn highlighter() -> &'static Highlighter {
        static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
        HIGHLIGHTER.get_or_init(Highlighter::new)
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(Line::text).collect()
    }

    /// The live line texts plus the flushable prefix length.
    fn state(stream: &mut MarkdownStream, width: u16) -> (Vec<String>, usize) {
        let render = stream.render(width, highlighter());
        (texts(render.live_lines()), render.flushable_len())
    }

    #[test]
    fn a_partial_line_never_renders() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("hel");
        let (live, flushable) = state(&mut stream, 80);
        assert!(live.is_empty());
        assert_eq!(flushable, 0);
        stream.push_delta("lo\n");
        let (live, _) = state(&mut stream, 80);
        assert_eq!(live, vec!["hello"]);
    }

    #[test]
    fn an_open_paragraph_holds_until_completed() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("one\n");
        let (_, flushable) = state(&mut stream, 80);
        assert_eq!(flushable, 0);
        stream.push_delta("\n");
        let (_, flushable) = state(&mut stream, 80);
        assert_eq!(flushable, 1);
    }

    #[test]
    fn a_completed_paragraph_flushes_with_its_separator() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("one\n\ntwo\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["one", "", "two"]);
        // "one" and the separator flush; the open "two" holds.
        assert_eq!(flushable, 2);
    }

    #[test]
    fn completed_list_items_flush_when_the_next_item_starts() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("- a\n");
        let (_, flushable) = state(&mut stream, 80);
        assert_eq!(flushable, 0, "a lone open item holds");
        stream.push_delta("- b\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["- a", "- b"]);
        assert_eq!(flushable, 1, "the first item completed; the second is open");
        stream.push_delta("\n");
        let (_, flushable) = state(&mut stream, 80);
        assert_eq!(flushable, 2);
    }

    #[test]
    fn fence_body_lines_flush_while_the_fence_is_open() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("```rust\nfn a() {}\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["fn a() {}"]);
        assert_eq!(flushable, 1);
    }

    #[test]
    fn a_trailing_maybe_closing_line_holds_back() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("```\nalpha\n``\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["alpha", "``"]);
        assert_eq!(flushable, 1, "the maybe-closing line holds");
        stream.push_delta("beta\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["alpha", "``", "beta"]);
        assert_eq!(flushable, 3, "proven content, the line now flushes");
    }

    #[test]
    fn a_maybe_closing_line_triggers_a_full_body_rerender() {
        // With a known language the fallback must keep the highlight
        // identical to the one-shot path.
        let mut stream = MarkdownStream::new();
        stream.push_delta("```rust\nlet a = 1;\n``\n");
        let rendered = stream.render(80, highlighter()).live_lines().to_vec();
        let expected = highlighter().highlight_snippet("rust", "let a = 1;\n``\n");
        assert_eq!(rendered, expected);
    }

    #[test]
    fn a_blank_line_inside_an_open_fence_keeps_the_fast_path() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("```rust\nlet a = 1;\n\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["let a = 1;", ""]);
        assert_eq!(flushable, 2, "the blank body line is literal and flushes");
        // The trailing blank is body, not a closer: the fence stays open
        // and keeps its incremental highlight state (a closed-fence
        // re-render would have dropped `fence` to None — and rebuilding
        // that state per blank line is quadratic on the hot path).
        assert!(!stream.blocks[0].closed);
        assert!(stream.blocks[0].fence.is_some());
        stream.ack_flushed(2);
        stream.push_delta("let b = 2;\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["let b = 2;"]);
        assert_eq!(flushable, 1);
        assert!(!stream.blocks[0].closed);
        assert!(stream.blocks[0].fence.is_some());
    }

    #[test]
    fn a_fence_with_its_closer_arrived_is_fully_flushable() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("```rust\nlet x = 1;\n```\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["let x = 1;"]);
        assert!(stream.blocks[0].closed, "the closer line closes the fence");
        assert_eq!(flushable, 1, "the closer commits the whole body");
    }

    #[test]
    fn an_open_table_holds_from_its_header_until_closed() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("| a |\n| --- |\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["a"]);
        assert_eq!(flushable, 0, "the header row is held");
        stream.push_delta("| 1 |\n");
        let (_, flushable) = state(&mut stream, 80);
        assert_eq!(flushable, 0);
        // A blank line closes the table (a plain text line would join it
        // as another row, per GFM).
        stream.push_delta("\nafter\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["a", "1", "", "after"]);
        assert_eq!(
            flushable, 3,
            "the settled table flushes; the open paragraph holds"
        );
    }

    #[test]
    fn the_flush_prefix_stops_at_the_first_held_line() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("done\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["done", "", "a | b", "1 | 2"]);
        assert_eq!(flushable, 2, "the held table blocks its own lines only");
    }

    #[test]
    fn blocks_are_separated_by_one_empty_line() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("one\n\n# two\n\nthree\n\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["one", "", "two", "", "three"]);
        assert_eq!(flushable, 5);
    }

    #[test]
    fn lists_use_uniform_spacing_ignoring_tight_and_loose() {
        let tight = render_document("- a\n- b\n\n", 80, highlighter());
        let loose = render_document("- a\n\n- b\n\n", 80, highlighter());
        assert_eq!(tight, loose);
        assert_eq!(texts(&tight), vec!["- a", "- b"]);
    }

    #[test]
    fn ordered_lists_keep_the_source_number() {
        let lines = render_document("3. a\n4. b\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["3. a", "4. b"]);
    }

    #[test]
    fn nested_lists_indent_two_spaces_per_level() {
        let lines = render_document("- a\n  - b\n    - c\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["- a", "  - b", "    - c"]);
    }

    #[test]
    fn list_continuation_lines_align_under_the_content() {
        let lines = render_document("- foo  \n  bar\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["- foo", "  bar"]);
        let ordered = render_document("10. foo  \n    bar\n\n", 80, highlighter());
        assert_eq!(texts(&ordered), vec!["10. foo", "    bar"]);
    }

    #[test]
    fn atx_headings_render_bold_in_paragraph_shape() {
        let lines = render_document("# Title\n\n", 80, highlighter());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "Title");
        assert!(lines[0].spans.iter().all(|span| span.style.mods.bold));
    }

    #[test]
    fn a_setext_heading_reclassifies_the_open_paragraph() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("foo\n");
        let render = stream.render(80, highlighter());
        assert_eq!(render.flushable_len(), 0, "the open paragraph holds");
        assert!(
            render.live_lines()[0]
                .spans
                .iter()
                .all(|span| !span.style.mods.bold)
        );
        stream.push_delta("===\n");
        let lines = stream.render(80, highlighter()).live_lines().to_vec();
        assert_eq!(lines.len(), 1, "one logical line, same shape");
        assert_eq!(lines[0].text(), "foo");
        assert!(lines[0].spans.iter().all(|span| span.style.mods.bold));
    }

    #[test]
    fn soft_breaks_render_as_spaces() {
        let lines = render_document("foo\nbar\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["foo bar"]);
    }

    #[test]
    fn hard_breaks_split_logical_lines() {
        let lines = render_document("foo  \nbar\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["foo", "bar"]);
    }

    #[test]
    fn inline_modifiers_combine_when_nested() {
        let lines = render_document("***both*** ~~strike~~\n\n", 80, highlighter());
        let both = &lines[0].spans[0];
        assert_eq!(both.text, "both");
        assert!(both.style.mods.bold && both.style.mods.italic);
        let strike = &lines[0].spans[2];
        assert_eq!(strike.text, "strike");
        assert!(strike.style.mods.strikethrough);
    }

    #[test]
    fn inline_code_uses_the_subtle_slot() {
        let lines = render_document("a `code` b\n\n", 80, highlighter());
        let code = &lines[0].spans[1];
        assert_eq!(code.text, "code");
        assert_eq!(code.style.fg, Some(Color::Slot(Slot::TextSubtle)));
    }

    #[test]
    fn links_render_their_text_underlined_without_the_url() {
        let lines = render_document("[text](https://example.com)\n\n", 80, highlighter());
        assert_eq!(lines[0].text(), "text");
        assert!(lines[0].spans.iter().all(|span| span.style.mods.underline));
        assert!(!lines[0].text().contains("https"));
    }

    #[test]
    fn images_render_their_alt_text_plain() {
        let lines = render_document("![alt](img.png)\n\n", 80, highlighter());
        assert_eq!(lines[0].text(), "alt");
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|span| span.style == Style::default())
        );
    }

    #[test]
    fn code_fences_render_body_lines_without_markers() {
        let lines = render_document("```rust\nfn main() {}\n```\n\n", 80, highlighter());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "fn main() {}");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(..)))),
            "the body is highlighted"
        );
    }

    #[test]
    fn unknown_fence_languages_render_plain_literal_lines() {
        let lines = render_document("```notalang\nx = 1\n```\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["x = 1"]);
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|span| span.style == Style::default())
        );
    }

    #[test]
    fn an_unclosed_fence_renders_literal_body_at_finalize() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("```\ncode\n");
        stream.finalize("```\ncode\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["code"]);
        assert_eq!(flushable, 1);
    }

    #[test]
    fn indented_code_renders_literal_lines() {
        let lines = render_document("    fn x() {}\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["fn x() {}"]);
    }

    #[test]
    fn tables_pad_columns_to_their_natural_width() {
        let lines = render_document(
            "| a | bb |\n| --- | --- |\n| ccc | d |\n\n",
            80,
            highlighter(),
        );
        assert_eq!(texts(&lines), vec!["a   | bb", "ccc | d"]);
        let header = &lines[0];
        assert!(header.spans[0].style.mods.bold);
        let sep = header
            .spans
            .iter()
            .find(|span| span.text == " | ")
            .expect("the join separator span");
        assert_eq!(sep.style.fg, Some(Color::Slot(Slot::TextSubtle)));
    }

    #[test]
    fn narrow_tables_transpose_to_key_value_records() {
        let source = "| name | value |\n| --- | --- |\n| alpha | 1 |\n| beta | 200 |\n\n";
        let lines = render_document(source, 12, highlighter());
        assert_eq!(
            texts(&lines),
            vec!["name: alpha", "value: 1", "name: beta", "value: 200"]
        );
        assert!(
            lines[0].spans[0].style.mods.bold,
            "the header part stays bold"
        );
    }

    #[test]
    fn table_cell_styling_survives_transposition() {
        let lines = render_document(
            "| h | g |\n| --- | --- |\n| *x* | y |\n\n",
            4,
            highlighter(),
        );
        assert_eq!(texts(&lines), vec!["h: x", "g: y"]);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.text == "x" && span.style.mods.italic)
        );
    }

    #[test]
    fn thematic_breaks_render_a_subtle_rule() {
        let lines = render_document("one\n\n---\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["one", "", "---"]);
        assert_eq!(
            lines[2].spans[0].style.fg,
            Some(Color::Slot(Slot::TextSubtle))
        );
    }

    #[test]
    fn block_quotes_prefix_every_content_line() {
        let lines = render_document("> one\n>\n> two\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["> one", "> ", "> two"]);
        for line in &lines {
            assert_eq!(
                line.spans[0].style.fg,
                Some(Color::Slot(Slot::TextSubtle)),
                "the prefix is subtle"
            );
        }
    }

    #[test]
    fn nested_blocks_inside_quotes_keep_their_shape() {
        let lines = render_document("> - x\n> - y\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["> - x", "> - y"]);
    }

    #[test]
    fn html_blocks_render_literal_text() {
        let lines = render_document("<div>\nhi\n</div>\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["<div>", "hi", "</div>"]);
    }

    #[test]
    fn inline_html_renders_literal_text() {
        let lines = render_document("a <b>c</b> d\n\n", 80, highlighter());
        assert_eq!(texts(&lines), vec!["a <b>c</b> d"]);
    }

    #[test]
    fn a_late_reference_definition_restyles_unflushed_links() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("see [the link][r]\n\n");
        let (live, _) = state(&mut stream, 80);
        assert_eq!(live, vec!["see [the link][r]"], "unresolved: literal");
        stream.push_delta("[r]: /url\n");
        let lines = stream.render(80, highlighter()).live_lines().to_vec();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "see the link");
        assert!(
            lines[0].spans.iter().any(|span| span.style.mods.underline),
            "the resolved link underlines"
        );
    }

    #[test]
    fn flushed_lines_survive_reference_definition_changes() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("see [the link][r]\n\n");
        let (_, flushable) = state(&mut stream, 80);
        stream.ack_flushed(flushable);
        stream.push_delta("[r]: /url\n");
        let (live, _) = state(&mut stream, 80);
        assert!(live.is_empty(), "flushed rows never reappear");
    }

    #[test]
    fn ack_flushed_drops_lines_permanently() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("one\n\ntwo\n\n");
        let (_, flushable) = state(&mut stream, 80);
        stream.ack_flushed(flushable);
        let (live, _) = state(&mut stream, 80);
        assert!(live.is_empty());
        stream.push_delta("three\n\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["", "three"], "the separator survives the flush");
        assert_eq!(flushable, 2);
    }

    #[test]
    fn finalize_replaces_the_source_and_closes_everything() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("one\n\ntrunc");
        stream.finalize("one\n\ntwo\n\nthree\n");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["one", "", "two", "", "three"]);
        assert_eq!(flushable, 5);
    }

    #[test]
    fn finalize_commits_a_trailing_partial_line() {
        let mut stream = MarkdownStream::new();
        stream.finalize("no newline here");
        let (live, flushable) = state(&mut stream, 80);
        assert_eq!(live, vec!["no newline here"]);
        assert_eq!(flushable, 1);
    }

    #[test]
    fn a_width_change_re_renders_live_tables() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("| abc | def |\n| --- | --- |\n| 1 | 2 |\n\n");
        let (live, _) = state(&mut stream, 80);
        assert_eq!(live, vec!["abc | def", "1   | 2"]);
        let (live, _) = state(&mut stream, 8);
        assert_eq!(live, vec!["abc: 1", "def: 2"]);
    }

    #[test]
    fn render_document_matches_the_finalized_stream() {
        let source = "# Title\n\npara *with* styles\n\n- a\n- b\n\n```rust\nfn main() {}\n```\n";
        let direct = render_document(source, 60, highlighter());
        let mut stream = MarkdownStream::new();
        stream.push_delta(source);
        stream.finalize(source);
        let streamed = stream.render(60, highlighter()).live_lines().to_vec();
        assert_eq!(direct, streamed);
    }

    #[test]
    fn an_empty_source_renders_nothing() {
        let mut stream = MarkdownStream::new();
        let (live, flushable) = state(&mut stream, 80);
        assert!(live.is_empty());
        assert_eq!(flushable, 0);
        assert!(render_document("", 80, highlighter()).is_empty());
    }

    #[test]
    fn a_mixed_document_snapshot() {
        let source = "# Release notes\n\nThe *stream* stays `readable` while it lands.\n\n\
             - first item\n- second with [a link][ref]\n  - nested\n\n\
             ```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n\
             | name | value |\n| --- | --- |\n| alpha | 1 |\n| beta | 200 |\n\n\
             ---\n\n> quoted *tail*\n\n[ref]: https://example.com\n";
        insta::assert_debug_snapshot!(render_document(source, 60, highlighter()));
    }

    #[test]
    fn a_narrow_document_snapshot() {
        let source = "| name | value |\n| --- | --- |\n| alpha | 1 |\n| beta | 200 |\n\n\
             done\n\n";
        insta::assert_debug_snapshot!(render_document(source, 12, highlighter()));
    }

    #[test]
    fn a_stream_mid_flight_snapshot() {
        let mut stream = MarkdownStream::new();
        stream.push_delta("# Notes\n\npara *one*\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n");
        let render = stream.render(60, highlighter());
        insta::assert_debug_snapshot!((render.flushable_len(), render.live_lines()));
    }
}
