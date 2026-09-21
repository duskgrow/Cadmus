//! The composer — Cadmus's self-built multiline prompt editor (ADR-0018
//! item 6, ADR-0012's "editor-grade multiline input" floor: selection, undo,
//! word operations). Library code only: keybindings and the Ctrl-G out to
//! `$EDITOR` are app-level wiring owned by the input layer; every editing
//! operation here is a plain method so the keymap-as-data layer can bind any
//! of them.
//!
//! Design contracts:
//!
//! - **Grapheme-correctness**: the buffer is a `Vec<String>` of lines (no
//!   `\n` inside); the cursor column is a *byte* offset that always rests on
//!   an extended-grapheme boundary. Left/right movement crosses line
//!   boundaries at the ends; up/down keeps a preferred display column.
//! - **Snapshot undo**: each undo unit stores the full (lines, cursor)
//!   *before* the mutation, bounded to `MAX_UNDO_UNITS` units and
//!   `MAX_UNDO_BYTES` of stored text, oldest evicted first. Coalescing:
//!   runs of consecutive single-grapheme [`Composer::insert_str`] calls (no
//!   newline) merge into one unit, as do runs of consecutive
//!   [`Composer::backspace`] calls; any other op or any cursor move breaks a
//!   run. A multi-grapheme `insert_str` (the paste path) is always its own
//!   unit. No redo — no consumer yet (ADR-0018's pseudo-requirement
//!   discipline).
//! - **Insert filtering**: `\r\n`/`\r` normalize to `\n` (line split), `\t`
//!   expands to 4 spaces (terminal tab stops would break the width math),
//!   other control chars (C0 < `0x20`, DEL and the C1 range `0x7F..=0x9F`)
//!   are dropped; everything else inserts verbatim. A fully-filtered insert
//!   is a no-op.
//! - **One layout truth**: the pure `Composer::layout` helper hard-wraps
//!   by display width (editor-style: a grapheme that doesn't fit moves to
//!   the next row whole — never word wrap, the inline spike's `wrap_line` is
//!   the reference) and computes the cursor's wrapped (x, y);
//!   [`Composer::desired_rows`] and [`Composer::render`] both consume it, so
//!   the band height and the drawn cursor can never drift.
//! - **Determinism seam** (AGENTS.md): [`PasteBurst`] is a pure classifier
//!   over injected instants — it never calls `Instant::now()` itself.
//!
//! Deliberately not built (ADR-0018 pseudo-requirement verdicts): vim mode,
//! registers, macros, marks, kill buffer — no consumer yet; the op-method
//! surface keeps them possible.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Clear;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Undo stack depth bound (ADR-0018 item 6).
const MAX_UNDO_UNITS: usize = 64;
/// Undo stack byte bound: `1 MiB` of stored text. A single unit larger than
/// the budget is still retained — undo must never silently die.
const MAX_UNDO_BYTES: usize = 1024 * 1024;

/// The prompt prefix rendered on the first line (2 display columns wide).
const PROMPT_PREFIX: &str = "❯ ";

/// The gutter rendered on continuation lines (2 display columns wide).
const CONTINUATION_GUTTER: &str = "  ";

/// Display width of the composer's prompt prefix and continuation gutter.
const GUTTER_WIDTH: u16 = 2;

/// Default placeholder text rendered when the composer buffer is empty
/// (idle: no run's keys to name).
pub(crate) const DEFAULT_PLACEHOLDER: &str = "Ask anything";

/// The placeholder while a run is active — the truthfulness rule: it names
/// only the keys that work mid-run (Enter steers at the next request
/// boundary, Tab queues to the finish line, Esc interrupts — ADR-0018's
/// 2026-09-21 binding amendment). While the approval dialog is open Tab
/// belongs to it, and [`DIALOG_PLACEHOLDER`] tells that truth instead.
pub(crate) const RUNNING_PLACEHOLDER: &str = "Enter to steer · Tab to queue · Esc to interrupt";

/// The running placeholder while the approval dialog is open: the dialog
/// owns Tab (its own hint row names that), so the composer names only the
/// keys still its own.
pub(crate) const DIALOG_PLACEHOLDER: &str = "Enter to steer · Esc to interrupt";

/// A (line index, byte column) buffer position.
type Pos = (usize, usize);

/// The undo-coalescing run state: which repeating op the last unit belongs
/// to. Only single-grapheme inserts and backspaces form runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Run {
    Insert,
    Backspace,
}

/// One undo unit: the full (lines, cursor) snapshot *before* the mutation.
#[derive(Clone, Debug)]
struct UndoUnit {
    lines: Vec<String>,
    cursor: Pos,
}

impl UndoUnit {
    /// The unit's share of the byte budget: stored text bytes.
    fn size(&self) -> usize {
        self.lines.iter().map(String::len).sum()
    }
}

/// The multiline prompt editor. See the module docs for the contracts.
#[derive(Debug)]
pub struct Composer {
    /// The buffer: at least one line, no `\n` inside any line.
    lines: Vec<String>,
    /// The cursor: line index + byte column on a grapheme boundary.
    cursor: Pos,
    /// The selection's fixed end; the moving end is `cursor`.
    anchor: Option<Pos>,
    /// Sticky display column for up/down movement.
    preferred_col: Option<usize>,
    /// The snapshot stack, oldest first.
    undo: VecDeque<UndoUnit>,
    /// Sum of the stack's [`UndoUnit::size`].
    undo_bytes: usize,
    /// The live coalescing run, if any.
    run: Option<Run>,
    /// First visible wrapped row; adjusted on every render.
    scroll: usize,
    /// Placeholder text rendered when the buffer is empty.
    placeholder: String,
}

impl Composer {
    /// An empty composer: one empty line, cursor at the origin.
    #[must_use]
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            anchor: None,
            preferred_col: None,
            undo: VecDeque::new(),
            undo_bytes: 0,
            run: None,
            scroll: 0,
            placeholder: String::from(DEFAULT_PLACEHOLDER),
        }
    }

    /// The placeholder text rendered when the buffer is empty.
    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// Set the placeholder text rendered when the buffer is empty.
    pub fn set_placeholder(&mut self, text: impl Into<String>) {
        self.placeholder = text.into();
    }

    /// The whole buffer, lines joined with `\n`.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether the buffer holds any text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// The cursor as (line index, byte column).
    #[must_use]
    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    /// The selection as ordered (start, end) positions, or `None` when no
    /// selection is active (a degenerate zero-length anchor is no
    /// selection).
    #[must_use]
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        self.selection_range()
    }

    /// Reset to empty. Undoable: one unit.
    pub fn clear(&mut self) {
        if self.is_empty() && self.anchor.is_none() {
            return;
        }
        self.push_undo();
        self.lines.clear();
        self.lines.push(String::new());
        self.cursor = (0, 0);
        self.anchor = None;
        self.preferred_col = None;
        self.run = None;
    }

    /// Insert text at the cursor, replacing any active selection. Filtering
    /// per the module docs. Runs of consecutive single-grapheme inserts (no
    /// newline) coalesce into one undo unit; a multi-grapheme insert is
    /// always its own unit.
    pub fn insert_str(&mut self, s: &str) {
        let text = filter_insert(s);
        if text.is_empty() {
            return;
        }
        let single = !text.contains('\n') && text.graphemes(true).nth(1).is_none();
        let coalesce = single && self.run == Some(Run::Insert) && self.selection_range().is_none();
        if !coalesce {
            self.push_undo();
        }
        self.remove_selection();
        self.insert_text(&text);
        self.run = if single { Some(Run::Insert) } else { None };
        self.preferred_col = None;
    }

    /// Split the line at the cursor. Always its own undo unit.
    pub fn insert_newline(&mut self) {
        self.push_undo();
        self.remove_selection();
        self.insert_text("\n");
        self.run = None;
        self.preferred_col = None;
    }

    /// Delete one grapheme back, joining lines at a line start. With an
    /// active selection, deletes the selection instead (one undo unit).
    /// Consecutive backspaces coalesce into one undo unit.
    pub fn backspace(&mut self) {
        if self.delete_selection_as_unit() {
            return;
        }
        let (line, col) = self.cursor;
        if line == 0 && col == 0 {
            return;
        }
        if self.run != Some(Run::Backspace) {
            self.push_undo();
        }
        self.run = Some(Run::Backspace);
        self.preferred_col = None;
        if col == 0 {
            let prev_len = self.lines[line - 1].len();
            let removed = self.lines.remove(line);
            self.lines[line - 1].push_str(&removed);
            self.cursor = (line - 1, prev_len);
        } else {
            let start = prev_grapheme_boundary(&self.lines[line], col);
            self.lines[line].replace_range(start..col, "");
            self.cursor = (line, start);
        }
    }

    /// Delete one grapheme forward, joining lines at a line end. With an
    /// active selection, deletes the selection instead. Always its own undo
    /// unit.
    pub fn delete_forward(&mut self) {
        if self.delete_selection_as_unit() {
            return;
        }
        let (line, col) = self.cursor;
        if line == self.lines.len() - 1 && col == self.lines[line].len() {
            return;
        }
        self.push_undo();
        self.run = None;
        self.preferred_col = None;
        if col == self.lines[line].len() {
            let next = self.lines.remove(line + 1);
            self.lines[line].push_str(&next);
        } else {
            let end = next_grapheme_boundary(&self.lines[line], col);
            self.lines[line].replace_range(col..end, "");
        }
    }

    /// Delete back to the word-start the cursor would move to (same
    /// semantics as [`Composer::move_word_left`]). Always its own undo unit.
    pub fn delete_word_back(&mut self) {
        if self.delete_selection_as_unit() {
            return;
        }
        let target = self.word_left_pos();
        if target == self.cursor {
            return;
        }
        self.push_undo();
        let cursor = self.cursor;
        self.delete_range(target, cursor);
        self.run = None;
        self.preferred_col = None;
    }

    /// Delete forward to the word-end the cursor would move to (same
    /// semantics as [`Composer::move_word_right`]). Always its own undo
    /// unit.
    pub fn delete_word_forward(&mut self) {
        if self.delete_selection_as_unit() {
            return;
        }
        let target = self.word_right_pos();
        if target == self.cursor {
            return;
        }
        self.push_undo();
        let cursor = self.cursor;
        self.delete_range(cursor, target);
        self.run = None;
        self.preferred_col = None;
    }

    /// One grapheme left, crossing to the previous line's end at a line
    /// start. `select` extends the selection from its anchor; otherwise the
    /// selection is cleared.
    pub fn move_left(&mut self, select: bool) {
        self.preferred_col = None;
        let (line, col) = self.cursor;
        let pos = if col > 0 {
            (line, prev_grapheme_boundary(&self.lines[line], col))
        } else if line > 0 {
            (line - 1, self.lines[line - 1].len())
        } else {
            (0, 0)
        };
        self.move_to(pos, select);
    }

    /// One grapheme right, crossing to the next line's start at a line end.
    pub fn move_right(&mut self, select: bool) {
        self.preferred_col = None;
        let (line, col) = self.cursor;
        let pos = if col < self.lines[line].len() {
            (line, next_grapheme_boundary(&self.lines[line], col))
        } else if line + 1 < self.lines.len() {
            (line + 1, 0)
        } else {
            (line, col)
        };
        self.move_to(pos, select);
    }

    /// One line up, keeping the preferred display column; on the first
    /// line, collapses to the line start (editors' convention).
    pub fn move_up(&mut self, select: bool) {
        let (line, col) = self.cursor;
        let preferred = self
            .preferred_col
            .unwrap_or_else(|| display_col(&self.lines[line], col));
        let pos = if line == 0 {
            (0, 0)
        } else {
            (
                line - 1,
                col_at_display_width(&self.lines[line - 1], preferred),
            )
        };
        self.preferred_col = Some(preferred);
        self.move_to(pos, select);
    }

    /// One line down, keeping the preferred display column; on the last
    /// line, collapses to the line end.
    pub fn move_down(&mut self, select: bool) {
        let (line, col) = self.cursor;
        let preferred = self
            .preferred_col
            .unwrap_or_else(|| display_col(&self.lines[line], col));
        let pos = if line + 1 == self.lines.len() {
            (line, self.lines[line].len())
        } else {
            (
                line + 1,
                col_at_display_width(&self.lines[line + 1], preferred),
            )
        };
        self.preferred_col = Some(preferred);
        self.move_to(pos, select);
    }

    /// To the start of the current or previous word (Emacs/readline
    /// two-phase: skip separators, then the word), crossing line
    /// boundaries. A grapheme is a word constituent when its first char is
    /// alphanumeric, so `é` as e+combining stays one unit.
    pub fn move_word_left(&mut self, select: bool) {
        self.preferred_col = None;
        let pos = self.word_left_pos();
        self.move_to(pos, select);
    }

    /// To the end of the current or next word, crossing line boundaries.
    pub fn move_word_right(&mut self, select: bool) {
        self.preferred_col = None;
        let pos = self.word_right_pos();
        self.move_to(pos, select);
    }

    /// To the line start.
    pub fn move_home(&mut self, select: bool) {
        self.preferred_col = None;
        let pos = (self.cursor.0, 0);
        self.move_to(pos, select);
    }

    /// To the line end.
    pub fn move_end(&mut self, select: bool) {
        self.preferred_col = None;
        let line = self.cursor.0;
        let pos = (line, self.lines[line].len());
        self.move_to(pos, select);
    }

    /// Delete the active selection; returns whether anything was deleted.
    /// Undoable as one unit. Called implicitly (without its own unit) by
    /// every edit when a selection is active.
    pub fn delete_selection(&mut self) -> bool {
        if self.selection_range().is_none() {
            self.anchor = None;
            return false;
        }
        self.push_undo();
        self.run = None;
        self.preferred_col = None;
        self.remove_selection()
    }

    /// Undo the newest unit; `false` when the stack is empty. Restores the
    /// pre-mutation (lines, cursor) and clears the selection.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop_back() {
            Some(unit) => {
                self.undo_bytes -= unit.size();
                self.lines = unit.lines;
                self.cursor = unit.cursor;
                self.anchor = None;
                self.preferred_col = None;
                self.run = None;
                true
            }
            None => false,
        }
    }

    /// The hard-wrapped row count at `width` (minimum 1) — feeds the band's
    /// height function. Computed for the available width after subtracting
    /// the 2-column gutter. Includes the cursor's continuation row when the
    /// cursor rests exactly at a full row's end (see `Layout::row_count`).
    #[must_use]
    pub fn desired_rows(&self, width: u16) -> u16 {
        u16::try_from(self.layout(width).row_count())
            .unwrap_or(u16::MAX)
            .max(1)
    }

    /// Draw the visible window of wrapped rows into `area`, scrolling so the
    /// cursor row stays visible. Renders a prompt prefix on the first line
    /// (in `prompt` — the theme's accent) and a blank continuation gutter on
    /// subsequent lines. When empty, renders the placeholder. Selected cells
    /// get `selection` patched over `base`; everything else gets `base`. The
    /// terminal cursor is placed at `gutter_width + layout.cursor_x` inside
    /// `area`.
    pub fn render(
        &mut self,
        area: Rect,
        frame: &mut Frame<'_>,
        base: Style,
        selection: Style,
        prompt: Style,
    ) {
        if area.is_empty() {
            return;
        }
        let layout = self.layout(area.width);
        let height = usize::from(area.height);
        if layout.cursor_y < self.scroll {
            self.scroll = layout.cursor_y;
        } else if layout.cursor_y >= self.scroll + height {
            self.scroll = layout.cursor_y + 1 - height;
        }
        self.scroll = self.scroll.min(layout.row_count().saturating_sub(height));

        frame.render_widget(Clear, area);
        let buf = frame.buffer_mut();
        buf.set_style(area, base);

        let placeholder_style = base.patch(Style::default().dim());
        let sel = self.selection_range();
        for (y, row) in (area.top()..area.bottom()).zip(layout.rows.iter().skip(self.scroll)) {
            let mut x = area.left();
            let is_first_line = row.line == 0 && row.start == 0;
            let (gutter, gutter_style) = if is_first_line {
                (PROMPT_PREFIX, prompt)
            } else {
                (CONTINUATION_GUTTER, base)
            };
            let max_width = usize::from(area.right().saturating_sub(x));
            x = buf.set_stringn(x, y, gutter, max_width, gutter_style).0;

            if self.is_empty() {
                let max_width = usize::from(area.right().saturating_sub(x));
                buf.set_stringn(x, y, &self.placeholder, max_width, placeholder_style);
            } else {
                for (offset, grapheme) in UnicodeSegmentation::grapheme_indices(
                    &self.lines[row.line][row.start..row.end],
                    true,
                ) {
                    let col = row.start + offset;
                    let style = match sel {
                        Some((start, end)) if start <= (row.line, col) && (row.line, col) < end => {
                            base.patch(selection)
                        }
                        _ => base,
                    };
                    let max_width = usize::from(area.right().saturating_sub(x));
                    x = buf.set_stringn(x, y, grapheme, max_width, style).0;
                }
            }
        }
        if layout.cursor_y >= layout.rows.len() && layout.cursor_y >= self.scroll {
            let cont_offset = layout.cursor_y - self.scroll;
            if cont_offset < height {
                let cont_y = area.top() + u16::try_from(cont_offset).unwrap_or_default();
                let max_width = usize::from(area.right().saturating_sub(area.left()));
                buf.set_stringn(area.left(), cont_y, CONTINUATION_GUTTER, max_width, base);
            }
        }
        let cursor_x =
            u16::try_from(usize::from(GUTTER_WIDTH) + layout.cursor_x).unwrap_or_default();
        let cursor_y = u16::try_from(layout.cursor_y - self.scroll).unwrap_or_default();
        frame.set_cursor_position((
            area.x.saturating_add(cursor_x),
            area.y.saturating_add(cursor_y),
        ));
    }

    /// The ordered selection range, or `None` when there is no anchor or
    /// the anchor equals the cursor.
    fn selection_range(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
        }
    }

    /// Snapshot the current (lines, cursor) as one undo unit, evicting the
    /// oldest units while over either bound. The newest unit is never
    /// evicted (see [`MAX_UNDO_BYTES`]).
    fn push_undo(&mut self) {
        let unit = UndoUnit {
            lines: self.lines.clone(),
            cursor: self.cursor,
        };
        self.undo_bytes += unit.size();
        self.undo.push_back(unit);
        while self.undo.len() > 1
            && (self.undo.len() > MAX_UNDO_UNITS || self.undo_bytes > MAX_UNDO_BYTES)
        {
            if let Some(evicted) = self.undo.pop_front() {
                self.undo_bytes -= evicted.size();
            }
        }
    }

    /// The destructive ops' preamble: with an active selection, the op's
    /// undo unit covers deleting it — and deletion *is* the op. Returns
    /// whether a selection was deleted.
    fn delete_selection_as_unit(&mut self) -> bool {
        if self.selection_range().is_none() {
            self.anchor = None;
            return false;
        }
        self.push_undo();
        self.remove_selection();
        self.run = None;
        self.preferred_col = None;
        true
    }

    /// Delete the selection without recording an undo unit (the caller's
    /// unit covers it). Returns whether anything was deleted.
    fn remove_selection(&mut self) -> bool {
        if let Some((start, end)) = self.selection_range() {
            self.delete_range(start, end);
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Delete `from..to` (ordered, on grapheme boundaries) and leave the
    /// cursor at `from`. The caller records the undo unit.
    fn delete_range(&mut self, from: Pos, to: Pos) {
        let (from_line, from_col) = from;
        let (to_line, to_col) = to;
        if from_line == to_line {
            self.lines[from_line].replace_range(from_col..to_col, "");
        } else {
            let tail = self.lines[to_line][to_col..].to_owned();
            self.lines[from_line].truncate(from_col);
            self.lines[from_line].push_str(&tail);
            self.lines.drain(from_line + 1..=to_line);
        }
        self.cursor = from;
    }

    /// Insert already-filtered `text` at the cursor, splitting lines on
    /// `\n`; the cursor lands after the inserted text.
    fn insert_text(&mut self, text: &str) {
        let (line, col) = self.cursor;
        let tail = self.lines[line].split_off(col);
        let mut parts = text.split('\n');
        self.lines[line].push_str(parts.next().unwrap_or_default());
        let mut end_line = line;
        for part in parts {
            end_line += 1;
            self.lines.insert(end_line, part.to_owned());
        }
        let end_col = self.lines[end_line].len();
        self.lines[end_line].push_str(&tail);
        self.cursor = (end_line, end_col);
    }

    /// Shared cursor setter: `select` extends the selection from its anchor
    /// (anchoring at the old cursor when none), otherwise clears it. Any
    /// move breaks an undo-coalescing run.
    fn move_to(&mut self, pos: Pos, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.run = None;
    }

    /// The joined buffer plus the cursor's flat byte offset in it (lines
    /// join with one `\n` byte each).
    fn flat_offset(&self) -> (String, usize) {
        let mut offset = 0;
        for line in &self.lines[..self.cursor.0] {
            offset += line.len() + 1;
        }
        (self.text(), offset + self.cursor.1)
    }

    /// The inverse of [`Composer::flat_offset`]'s offset math.
    fn pos_of_offset(&self, offset: usize) -> Pos {
        let mut rest = offset;
        for (index, line) in self.lines.iter().enumerate() {
            if rest <= line.len() {
                return (index, rest);
            }
            rest -= line.len() + 1;
        }
        (
            self.lines.len() - 1,
            self.lines.last().map_or(0, String::len),
        )
    }

    /// [`Composer::move_word_left`]'s target.
    fn word_left_pos(&self) -> Pos {
        let (text, offset) = self.flat_offset();
        let graphemes: Vec<(usize, &str)> =
            UnicodeSegmentation::grapheme_indices(&text[..offset], true).collect();
        let mut i = graphemes.len();
        while i > 0 && !is_word_grapheme(graphemes[i - 1].1) {
            i -= 1;
        }
        while i > 0 && is_word_grapheme(graphemes[i - 1].1) {
            i -= 1;
        }
        let target = graphemes.get(i).map_or(0, |g| g.0);
        self.pos_of_offset(target)
    }

    /// [`Composer::move_word_right`]'s target.
    fn word_right_pos(&self) -> Pos {
        let (text, offset) = self.flat_offset();
        let graphemes: Vec<(usize, &str)> =
            UnicodeSegmentation::grapheme_indices(&text[offset..], true).collect();
        let mut i = 0;
        while i < graphemes.len() && !is_word_grapheme(graphemes[i].1) {
            i += 1;
        }
        while i < graphemes.len() && is_word_grapheme(graphemes[i].1) {
            i += 1;
        }
        let target = graphemes.get(i).map_or(text.len(), |g| offset + g.0);
        self.pos_of_offset(target)
    }

    /// The one layout truth: hard-wrap every line by display width (reduced by
    /// the gutter) and locate the cursor's wrapped (x, y). Feeds both
    /// [`Composer::desired_rows`] and [`Composer::render`].
    fn layout(&self, width: u16) -> Layout {
        let width = usize::from(width.saturating_sub(GUTTER_WIDTH)).max(1);
        let mut rows = Vec::new();
        let mut cursor_x = 0;
        let mut cursor_y = 0;
        let mut placed = false;
        for (index, line) in self.lines.iter().enumerate() {
            let mut start = 0;
            let mut x = 0;
            for (gi, grapheme) in UnicodeSegmentation::grapheme_indices(line.as_str(), true) {
                let grapheme_width = UnicodeWidthStr::width(grapheme);
                if x + grapheme_width > width && x > 0 {
                    rows.push(Row {
                        line: index,
                        start,
                        end: gi,
                    });
                    start = gi;
                    x = 0;
                }
                if !placed && index == self.cursor.0 && gi == self.cursor.1 {
                    cursor_x = x;
                    cursor_y = rows.len();
                    placed = true;
                }
                x += grapheme_width;
            }
            rows.push(Row {
                line: index,
                start,
                end: line.len(),
            });
            if !placed && index == self.cursor.0 && self.cursor.1 == line.len() {
                if x == width {
                    // Exact fill: the cursor wraps to the continuation row.
                    cursor_x = 0;
                    cursor_y = rows.len();
                } else {
                    cursor_x = x;
                    cursor_y = rows.len() - 1;
                }
                placed = true;
            }
        }
        Layout {
            rows,
            cursor_x,
            cursor_y,
        }
    }
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

/// One hard-wrapped row: a byte slice of one source line.
#[derive(Clone, Copy, Debug)]
struct Row {
    line: usize,
    start: usize,
    end: usize,
}

/// The wrapped layout shared by [`Composer::desired_rows`] and
/// [`Composer::render`]: rows plus the cursor's wrapped (x, y).
#[derive(Debug)]
struct Layout {
    rows: Vec<Row>,
    cursor_x: usize,
    cursor_y: usize,
}

impl Layout {
    /// Rows the band must show: the wrapped text rows, plus one
    /// continuation row when the cursor rests exactly at a full row's end —
    /// the terminal auto-wrap corner, where the cursor needs a cell of its
    /// own, so the height grows exactly when the cursor does.
    fn row_count(&self) -> usize {
        self.rows.len().max(self.cursor_y + 1)
    }
}

/// A grapheme counts as a word constituent when its first char is
/// alphanumeric (so `é` as e+combining stays one unit).
fn is_word_grapheme(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(char::is_alphanumeric)
}

/// The grapheme boundary immediately before `col` (which rests on one).
fn prev_grapheme_boundary(line: &str, col: usize) -> usize {
    UnicodeSegmentation::grapheme_indices(line, true)
        .map(|(index, _)| index)
        .take_while(|index| *index < col)
        .last()
        .unwrap_or(0)
}

/// The grapheme boundary immediately after `col` (which rests on one).
fn next_grapheme_boundary(line: &str, col: usize) -> usize {
    UnicodeSegmentation::grapheme_indices(line, true)
        .map(|(index, grapheme)| index + grapheme.len())
        .find(|end| *end > col)
        .unwrap_or(line.len())
}

/// The display column (accumulated grapheme widths) of byte `col` in
/// `line`; `col` rests on a grapheme boundary, so no cluster is split.
fn display_col(line: &str, col: usize) -> usize {
    UnicodeWidthStr::width(&line[..col])
}

/// The byte column on `line` at display column `preferred`: the last
/// grapheme boundary whose display end does not exceed it.
fn col_at_display_width(line: &str, preferred: usize) -> usize {
    let mut x = 0;
    let mut col = 0;
    for (index, grapheme) in UnicodeSegmentation::grapheme_indices(line, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if x + grapheme_width > preferred {
            break;
        }
        x += grapheme_width;
        col = index + grapheme.len();
    }
    col
}

/// The insert filter (module docs): line-ending normalization, tab
/// expansion, control-char drop.
fn filter_insert(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if c < ' ' || ('\u{7F}'..='\u{9F}').contains(&c) => {}
            c => out.push(c),
        }
    }
    out
}

/// The paste-burst heuristic for terminals without bracketed paste
/// (ADR-0018 item 6). Bracketed paste arrives as whole
/// [`Composer::insert_str`] calls and needs no detection; for the rest, the
/// input broker classifies each incoming printable char: a burst starts
/// when at least [`PasteBurst::MIN_CHARS`] printable chars arrive with
/// inter-arrival gaps of at most [`PasteBurst::START_GAP`] (a fast typist
/// is ~60+ ms/char; paste is near-instant) and ends after a gap beyond
/// [`PasteBurst::END_GAP`]. A non-printable event
/// ([`PasteBurst::record_non_printable`]) ends it immediately.
///
/// Time is always injected (AGENTS.md determinism seam).
#[derive(Debug)]
pub struct PasteBurst {
    /// The previous printable char's arrival.
    last: Option<Instant>,
    /// Consecutive fast arrivals in the current run.
    fast_run: usize,
    /// Whether a burst is in progress.
    in_burst: bool,
}

impl PasteBurst {
    /// Inter-arrival gap that marks machine-fast input.
    pub const START_GAP: Duration = Duration::from_millis(20);
    /// Consecutive fast arrivals that open a burst.
    pub const MIN_CHARS: usize = 3;
    /// A gap beyond this closes the burst (paste delivery can be chunky, so
    /// the end threshold is looser than the start threshold).
    pub const END_GAP: Duration = Duration::from_millis(50);

    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            fast_run: 0,
            in_burst: false,
        }
    }

    /// Record one incoming printable char at `now`; returns whether a burst
    /// is in progress (the char is paste-suspect).
    pub fn record_printable(&mut self, now: Instant) -> bool {
        match self.last {
            None => self.fast_run = 1,
            Some(prev) if self.in_burst => {
                if now.saturating_duration_since(prev) > Self::END_GAP {
                    self.in_burst = false;
                    self.fast_run = 1;
                }
            }
            Some(prev) => {
                if now.saturating_duration_since(prev) <= Self::START_GAP {
                    self.fast_run += 1;
                } else {
                    self.fast_run = 1;
                }
                self.in_burst = self.fast_run >= Self::MIN_CHARS;
            }
        }
        self.last = Some(now);
        self.in_burst
    }

    /// Record a non-printable event (key, control): ends any burst and
    /// resets the arrival chain.
    pub fn record_non_printable(&mut self) {
        self.last = None;
        self.fast_run = 0;
        self.in_burst = false;
    }
}

impl Default for PasteBurst {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Position;
    use ratatui::style::{Color, Modifier};

    use super::*;

    fn composer_with(text: &str) -> Composer {
        let mut composer = Composer::new();
        composer.insert_str(text);
        composer
    }

    #[test]
    fn insert_backspace_and_delete_forward_edit_in_place() {
        let mut composer = composer_with("hello");
        assert_eq!(composer.cursor(), (0, 5));
        composer.backspace();
        assert_eq!(composer.text(), "hell");
        assert_eq!(composer.cursor(), (0, 4));
        // At the very end: a no-op, and no undo unit is recorded.
        composer.delete_forward();
        assert_eq!(composer.text(), "hell");
        composer.move_home(false);
        composer.delete_forward();
        assert_eq!(composer.text(), "ell");
        assert_eq!(composer.cursor(), (0, 0));
        assert!(!composer.is_empty());
        composer.clear();
        assert!(composer.is_empty());
        assert_eq!(composer.cursor(), (0, 0));
    }

    #[test]
    fn a_newline_splits_and_backspace_rejoins_the_line() {
        let mut composer = composer_with("ab");
        composer.insert_newline();
        composer.insert_str("cd");
        assert_eq!(composer.text(), "ab\ncd");
        assert_eq!(composer.cursor(), (1, 2));
        composer.move_home(false);
        composer.backspace();
        assert_eq!(composer.text(), "abcd");
        assert_eq!(composer.cursor(), (0, 2));
        composer.delete_forward();
        composer.delete_forward();
        assert_eq!(composer.text(), "ab");
        // Split mid-line: the tail moves to the new line.
        composer.move_home(false);
        composer.move_right(false);
        composer.insert_newline();
        assert_eq!(composer.text(), "a\nb");
        assert_eq!(composer.cursor(), (1, 0));
    }

    #[test]
    fn the_cursor_moves_by_grapheme_over_emoji_cjk_and_combining_marks() {
        // 👨‍👩‍👧‍👦 is one grapheme (25 bytes), 好 is 3 bytes wide 2, and the
        // final é is e + U+0301 (3 bytes): byte cols 0,1,26,27,30,33.
        let mut composer = composer_with("a👨\u{200D}👩\u{200D}👧\u{200D}👦b好e\u{301}");
        assert_eq!(composer.cursor(), (0, 33));
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 30)); // before é, never inside it
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 27)); // before 好
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 26)); // before b
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 1)); // before the family emoji
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 0));
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 0));
        composer.move_right(false);
        composer.move_right(false);
        assert_eq!(composer.cursor(), (0, 26));
    }

    #[test]
    fn left_and_right_cross_line_boundaries() {
        let mut composer = composer_with("ab\ncd");
        composer.move_home(false);
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 2));
        composer.move_right(false);
        assert_eq!(composer.cursor(), (1, 0));
    }

    #[test]
    fn word_ops_cross_line_boundaries() {
        let mut composer = composer_with("foo bar\nbaz quux");
        composer.move_word_left(false);
        assert_eq!(composer.cursor(), (1, 4));
        composer.move_word_left(false);
        assert_eq!(composer.cursor(), (1, 0));
        // At a line start, word-left crosses into the previous line's word.
        composer.move_word_left(false);
        assert_eq!(composer.cursor(), (0, 4));
        composer.move_word_left(false);
        assert_eq!(composer.cursor(), (0, 0));
        composer.move_word_right(false);
        assert_eq!(composer.cursor(), (0, 3));
        composer.move_word_right(false);
        assert_eq!(composer.cursor(), (0, 7));
        // Word-right crosses the newline to the end of "baz".
        composer.move_word_right(false);
        assert_eq!(composer.cursor(), (1, 3));
    }

    #[test]
    fn word_deletion_follows_word_movement() {
        let mut composer = composer_with("foo bar");
        composer.delete_word_back();
        assert_eq!(composer.text(), "foo ");
        assert_eq!(composer.cursor(), (0, 4));
        composer.move_home(false);
        composer.delete_word_forward();
        assert_eq!(composer.text(), " ");
        // Forward deletion crosses the newline: separators, then the word.
        let mut composer = composer_with("ab\ncd");
        composer.move_home(false);
        composer.move_left(false);
        assert_eq!(composer.cursor(), (0, 2));
        composer.delete_word_forward();
        assert_eq!(composer.text(), "ab");
        // Combining-mark words delete as one unit.
        let mut composer = composer_with("e\u{301}x y");
        composer.delete_word_back();
        assert_eq!(composer.text(), "e\u{301}x ");
    }

    #[test]
    fn selection_extends_from_its_anchor_and_edits_replace_it() {
        let mut composer = composer_with("hello world");
        composer.move_home(false);
        composer.move_word_right(true);
        assert_eq!(composer.selection(), Some(((0, 0), (0, 5))));
        // Extending again keeps the original anchor.
        composer.move_right(true);
        assert_eq!(composer.selection(), Some(((0, 0), (0, 6))));
        // A backward selection reports ordered bounds.
        composer.move_end(false);
        composer.move_left(true);
        composer.move_left(true);
        assert_eq!(composer.selection(), Some(((0, 9), (0, 11))));
        // An edit replaces the selection and drops the anchor.
        composer.insert_str("?");
        assert_eq!(composer.text(), "hello wor?");
        assert_eq!(composer.selection(), None);
        assert_eq!(composer.cursor(), (0, 10));
        // The replace was one undo unit.
        assert!(composer.undo());
        assert_eq!(composer.text(), "hello world");
    }

    #[test]
    fn delete_selection_reports_whether_it_deleted() {
        let mut composer = composer_with("abc");
        assert!(!composer.delete_selection());
        composer.move_home(false);
        composer.move_right(true);
        composer.move_right(true);
        assert!(composer.delete_selection());
        assert_eq!(composer.text(), "c");
        assert!(!composer.delete_selection());
        // Backspace with an active selection deletes only the selection.
        composer.move_end(false);
        composer.move_left(true);
        composer.backspace();
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn undo_restores_snapshots_in_reverse_order() {
        let mut composer = Composer::new();
        composer.insert_str("one");
        composer.insert_newline();
        composer.insert_str("two");
        assert!(composer.undo());
        assert_eq!(composer.text(), "one\n");
        assert_eq!(composer.cursor(), (1, 0));
        assert!(composer.undo());
        assert_eq!(composer.text(), "one");
        assert!(composer.undo());
        assert_eq!(composer.text(), "");
        assert!(!composer.undo());
    }

    #[test]
    fn consecutive_typing_coalesces_into_one_undo_unit() {
        let mut composer = Composer::new();
        composer.insert_str("a");
        composer.insert_str("b");
        composer.insert_str("c");
        assert_eq!(composer.text(), "abc");
        assert!(composer.undo());
        assert_eq!(composer.text(), "");
        assert!(!composer.undo());
    }

    #[test]
    fn consecutive_backspaces_coalesce_into_one_undo_unit() {
        let mut composer = composer_with("abc");
        composer.backspace();
        composer.backspace();
        composer.backspace();
        assert_eq!(composer.text(), "");
        assert!(composer.undo());
        assert_eq!(composer.text(), "abc");
        // The paste that seeded the text is the next unit.
        assert!(composer.undo());
        assert!(!composer.undo());
    }

    #[test]
    fn a_cursor_move_breaks_a_coalescing_run() {
        let mut composer = Composer::new();
        composer.insert_str("a");
        composer.insert_str("b");
        composer.move_left(false);
        composer.move_right(false);
        composer.insert_str("c");
        assert!(composer.undo());
        assert_eq!(composer.text(), "ab");
        assert!(composer.undo());
        assert_eq!(composer.text(), "");
        assert!(!composer.undo());
    }

    #[test]
    fn a_multi_grapheme_insert_is_always_its_own_undo_unit() {
        let mut composer = Composer::new();
        composer.insert_str("ab");
        composer.insert_str("cd");
        assert!(composer.undo());
        assert_eq!(composer.text(), "ab");
        assert!(composer.undo());
        assert_eq!(composer.text(), "");
        assert!(!composer.undo());
        // A single grapheme after a paste starts a fresh run, not a merge.
        composer.insert_str("xy");
        composer.insert_str("z");
        assert!(composer.undo());
        assert_eq!(composer.text(), "xy");
    }

    #[test]
    fn the_undo_stack_evicts_beyond_64_units() {
        let mut composer = Composer::new();
        for _ in 0..100 {
            composer.insert_newline();
        }
        let mut undos = 0;
        while composer.undo() {
            undos += 1;
        }
        assert_eq!(undos, 64);
        assert_eq!(composer.text(), "\n".repeat(36));
    }

    #[test]
    fn the_undo_stack_evicts_beyond_one_mebibyte() {
        let mut composer = Composer::new();
        let chunk = "x".repeat(400 * 1024);
        composer.insert_str(&chunk);
        composer.insert_str(&chunk);
        composer.insert_str(&chunk);
        // Units stored 0, 400 KiB and 800 KiB of before-state; eviction
        // drops the oldest two, keeping the newest even though it is the
        // only one left.
        assert!(composer.undo());
        assert_eq!(composer.text().len(), 800 * 1024);
        assert!(!composer.undo());
    }

    #[test]
    fn undo_on_an_empty_stack_is_false() {
        let mut composer = Composer::new();
        assert!(!composer.undo());
    }

    #[test]
    fn desired_rows_counts_hard_wrapped_rows() {
        let mut composer = composer_with("abc");
        assert_eq!(composer.desired_rows(6), 1);
        // Width 0 clamps to 1: three wrapped rows, plus the cursor's
        // continuation row (every row is exactly filled).
        assert_eq!(composer.desired_rows(0), 4);
        composer.move_home(false);
        assert_eq!(composer.desired_rows(0), 3);
        composer.insert_str("de");
        assert_eq!(composer.desired_rows(6), 2);
        // Two logical lines each wrap independently; the cursor rests at a
        // full row's end here, so its continuation row is counted.
        let mut composer = composer_with("ab\ncd");
        assert_eq!(composer.desired_rows(4), 3);
        composer.move_home(false);
        assert_eq!(composer.desired_rows(4), 2);
        assert_eq!(Composer::new().desired_rows(10), 1);
    }

    #[test]
    fn a_full_row_end_grows_the_height_only_while_the_cursor_rests_there() {
        let mut composer = composer_with("abcd");
        assert_eq!(composer.desired_rows(6), 2);
        composer.move_left(false);
        assert_eq!(composer.desired_rows(6), 1);
    }

    #[test]
    fn wide_graphemes_move_to_the_next_row_whole() {
        // 好(2) + a(1) fills 3 of 4; the second 好 doesn't fit and wraps.
        let composer = composer_with("好a好");
        assert_eq!(composer.desired_rows(6), 2);
        // An exact fill counts the cursor's continuation row.
        let composer = composer_with("好ab");
        assert_eq!(composer.desired_rows(6), 2);
    }

    #[test]
    fn render_scrolls_to_keep_the_cursor_row_visible() {
        let mut composer = composer_with("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9");
        let backend = TestBackend::new(8, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        // Rows 7..=9 are visible; the cursor sits at end of l9.
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 2));
        terminal
            .backend()
            .assert_buffer_lines(["  l7    ", "  l8    ", "  l9    "]);
        // Scrolling back up follows the cursor.
        for _ in 0..9 {
            composer.move_up(false);
        }
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 0));
        terminal
            .backend()
            .assert_buffer_lines(["❯ l0    ", "  l1    ", "  l2    "]);
    }

    #[test]
    fn render_highlights_the_selected_cells() {
        let mut composer = composer_with("hello");
        composer.move_home(false);
        composer.move_right(true);
        composer.move_right(true);
        composer.move_right(true);
        let backend = TestBackend::new(7, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default().bg(Color::Blue),
                    Style::default(),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Reset);
        assert_eq!(buf.cell((1, 0)).unwrap().bg, Color::Reset);
        for x in 2..5 {
            assert_eq!(buf.cell((x, 0)).unwrap().bg, Color::Blue);
        }
        assert_eq!(buf.cell((5, 0)).unwrap().bg, Color::Reset);
        assert_eq!(buf.cell((6, 0)).unwrap().bg, Color::Reset);
    }

    #[test]
    fn render_places_the_cursor_inside_the_area_with_its_offset() {
        // The exact-fill corner: "abcd" at available width 4 wraps the cursor to the
        // continuation row. Area width 6 accounts for the 2-column gutter.
        let mut composer = composer_with("abcd");
        let backend = TestBackend::new(10, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    Rect::new(2, 1, 6, 2),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 2));
        terminal.backend().assert_buffer_lines([
            "          ",
            "  ❯ abcd  ",
            "          ",
            "          ",
        ]);
    }

    #[test]
    fn render_empty_composer_shows_prompt_and_placeholder() {
        let mut composer = Composer::new();
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default().fg(Color::Cyan),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(2, 0));
        let buf = terminal.backend().buffer();
        let line: String = (0..40)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect();
        assert_eq!(line, "❯ Ask anything                          ");
        // The prompt prefix reads in the prompt style (the app's accent).
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Cyan);
        for x in 2..14 {
            assert!(buf.cell((x, 0)).unwrap().modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn custom_placeholder_and_clearing() {
        let mut composer = Composer::new();
        composer.set_placeholder("Type a prompt...");
        assert_eq!(composer.placeholder(), "Type a prompt...");
        let backend = TestBackend::new(20, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(2, 0));
        let buf = terminal.backend().buffer();
        let line: String = (0..20)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect();
        assert_eq!(line, "❯ Type a prompt...  ");
        for x in 2..18 {
            assert!(buf.cell((x, 0)).unwrap().modifier.contains(Modifier::DIM));
        }

        composer.insert_str("hi");
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(4, 0));
        terminal
            .backend()
            .assert_buffer_lines(["❯ hi                "]);

        composer.clear();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(2, 0));
        let buf = terminal.backend().buffer();
        let line: String = (0..20)
            .map(|x| buf.cell((x, 0)).unwrap().symbol())
            .collect();
        assert_eq!(line, "❯ Type a prompt...  ");
    }

    #[test]
    fn multiline_gutter_alignment() {
        let mut composer = composer_with("first line\nsecond line");
        let backend = TestBackend::new(15, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                composer.render(
                    frame.area(),
                    frame,
                    Style::default(),
                    Style::default(),
                    Style::default(),
                );
            })
            .unwrap();
        terminal
            .backend()
            .assert_buffer_lines(["❯ first line   ", "  second line  "]);
    }

    #[test]
    fn up_and_down_keep_the_preferred_display_column() {
        let mut composer = composer_with("好ab\nxy\nwxyz");
        // Reach the end of line 0 (display col 4, byte col 5).
        composer.move_up(false);
        composer.move_up(false);
        assert_eq!(composer.cursor(), (0, 5));
        composer.move_down(false);
        // "xy" is shorter: clamp to its end.
        assert_eq!(composer.cursor(), (1, 2));
        composer.move_down(false);
        // The preferred column survives the clamped hop.
        assert_eq!(composer.cursor(), (2, 4));
        composer.move_up(false);
        composer.move_up(false);
        assert_eq!(composer.cursor(), (0, 5));
        // A preferred column that straddles a wide grapheme lands before it.
        composer.move_home(false);
        composer.move_down(false);
        composer.move_right(false);
        composer.move_up(false);
        assert_eq!(composer.cursor(), (0, 0));
    }

    #[test]
    fn a_burst_starts_continues_and_ends_on_a_slow_gap() {
        let t0 = Instant::now();
        let mut burst = PasteBurst::new();
        assert!(!burst.record_printable(t0));
        assert!(!burst.record_printable(t0 + Duration::from_millis(10)));
        // The third fast arrival opens the burst.
        assert!(burst.record_printable(t0 + Duration::from_millis(20)));
        // A 30 ms gap is past START_GAP but inside END_GAP: the burst holds.
        assert!(burst.record_printable(t0 + Duration::from_millis(50)));
        // A 60 ms gap ends it; that char is the first non-burst char.
        assert!(!burst.record_printable(t0 + Duration::from_millis(110)));
    }

    #[test]
    fn a_slow_typist_never_starts_a_burst() {
        let t0 = Instant::now();
        let mut burst = PasteBurst::new();
        for i in 0..10 {
            assert!(!burst.record_printable(t0 + Duration::from_millis(60 * i)));
        }
        // Two fast chars are not enough.
        assert!(!burst.record_printable(t0 + Duration::from_millis(605)));
        assert!(!burst.record_printable(t0 + Duration::from_millis(610)));
    }

    #[test]
    fn a_non_printable_event_ends_the_burst_immediately() {
        let t0 = Instant::now();
        let mut burst = PasteBurst::new();
        burst.record_printable(t0);
        burst.record_printable(t0 + Duration::from_millis(5));
        assert!(burst.record_printable(t0 + Duration::from_millis(10)));
        burst.record_non_printable();
        // The arrival chain is reset, not just the flag.
        assert!(!burst.record_printable(t0 + Duration::from_millis(15)));
        assert!(!burst.record_printable(t0 + Duration::from_millis(20)));
    }

    #[test]
    fn insert_filters_control_chars_and_normalizes_line_endings() {
        let mut composer = Composer::new();
        composer.insert_str("a\tb\r\nc\rd\u{0}e\u{7}f\u{7F}g");
        assert_eq!(composer.text(), "a    b\nc\ndefg");
        // The C1 range drops too (ratatui strips control chars at the cell
        // boundary anyway; the filter keeps the buffer clean by contract).
        let mut composer = Composer::new();
        composer.insert_str("a\u{85}\u{9F}b");
        assert_eq!(composer.text(), "ab");
        // A fully filtered insert is a no-op: no undo unit, no run break.
        let mut composer = Composer::new();
        composer.insert_str("a");
        composer.insert_str("\u{0}");
        composer.insert_str("b");
        assert_eq!(composer.text(), "ab");
        assert!(composer.undo());
        assert_eq!(composer.text(), "");
    }

    #[test]
    fn clear_is_a_single_undo_unit() {
        let mut composer = composer_with("ab\ncd");
        composer.clear();
        assert!(composer.is_empty());
        assert!(composer.undo());
        assert_eq!(composer.text(), "ab\ncd");
        // Clearing an empty composer is a no-op.
        let mut composer = Composer::new();
        composer.clear();
        assert!(!composer.undo());
    }
}
