//! Spike probe: does recreating the `Terminal` deliver dynamic band height on
//! stock ratatui — spike fact F1's untested escape hatch (ADR-0018)?
//!
//! Verdict criteria (the UX guardrails the maintainer set): zero history rows
//! lost or duplicated, bounded residue only, and the composer cursor stable
//! across every height change. The probe emulates a terminal with vt100, so
//! every assertion is deterministic; what it cannot judge is compositing
//! (flicker) — that stays with the manual terminal matrix in
//! `examples/inline_spike.rs`.
//!
//! The rig: a `Backend` impl that feeds a `vt100::Parser` the same escape
//! sequences ratatui-crossterm emits (CUP + symbol per cell, `\n` per
//! appended line, ED for clears). vt100 then applies real terminal semantics
//! — scrolling, scrollback, deferred wrap — instead of us re-deriving them.
//! Cursor-position queries are answered from the emulated screen, which is
//! exactly what a real terminal does; styles are omitted because SGR bytes
//! never move rows, and rows are what this probe judges.

use std::cell::RefCell;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::rc::Rc;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};

const SCREEN_ROWS: u16 = 24;
const SCREEN_COLS: u16 = 80;
/// Deep enough that no assertion below ever hits the scrollback cap.
const SCROLLBACK_LEN: usize = 500;

/// Shared handle to the emulated terminal; cloning gives a recreated
/// `Terminal` the same screen state — which is the whole point of the
/// recreation protocol (the OS terminal also outlives the `Terminal`).
#[derive(Clone)]
struct VtBackend {
    parser: Rc<RefCell<vt100::Parser>>,
}

impl VtBackend {
    fn new() -> Self {
        Self {
            parser: Rc::new(RefCell::new(vt100::Parser::new(
                SCREEN_ROWS,
                SCREEN_COLS,
                SCROLLBACK_LEN,
            ))),
        }
    }

    fn emit(&self, bytes: &str) {
        self.parser.borrow_mut().process(bytes.as_bytes());
    }
}

impl Backend for VtBackend {
    type Error = Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut out = String::new();
        for (x, y, cell) in content {
            // Naive per-cell CUP mirrors crossterm's semantics (the real
            // backend only elides contiguous moves); SGR omitted by design.
            let _ = write!(out, "\x1b[{};{}H{}", y + 1, x + 1, cell.symbol());
        }
        self.emit(&out);
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        // ratatui-crossterm emits plain `\n` x n; vt100 applies IND semantics
        // (cursor down, scrolling at the bottom margin) like a real terminal.
        self.emit(&"\n".repeat(usize::from(n)));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[?25l");
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[?25h");
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        let (row, col) = self.parser.borrow().screen().cursor_position();
        Ok(Position::new(col, row))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let Position { x, y } = position.into();
        self.emit(&format!("\x1b[{};{}H", y + 1, x + 1));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.emit("\x1b[2J");
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.emit(match clear_type {
            ClearType::All => "\x1b[2J",
            ClearType::AfterCursor => "\x1b[0J",
            ClearType::BeforeCursor => "\x1b[1J",
            ClearType::CurrentLine => "\x1b[2K",
            ClearType::UntilNewLine => "\x1b[0K",
        });
        Ok(())
    }

    fn size(&self) -> Result<Size, Self::Error> {
        let (rows, cols) = self.parser.borrow().screen().size();
        Ok(Size::new(cols, rows))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: self.size()?,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Read-side view over the emulated terminal: rows as the user would see
/// them, including what already scrolled off.
struct World {
    backend: VtBackend,
}

impl World {
    fn new() -> Self {
        Self {
            backend: VtBackend::new(),
        }
    }

    /// The shell session above the future TUI: printed before `Terminal`
    /// creation, like real pre-existing scrollback content.
    fn print_lines(&self, lines: &[String]) {
        for line in lines {
            self.backend.emit(&format!("{line}\r\n"));
        }
    }

    fn visible_rows(&self) -> Vec<String> {
        let parser = self.backend.parser.borrow();
        parser
            .screen()
            .rows(0, SCREEN_COLS)
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    fn scrollback_rows(&self) -> Vec<String> {
        let mut parser = self.backend.parser.borrow_mut();
        let screen = parser.screen_mut();
        // vt100 reads scrollback through a view offset, and one view is only
        // one screen tall: with more scrollback than that, page backwards
        // window by window or the newest rows silently fall off the read.
        screen.set_scrollback(usize::MAX);
        let depth = screen.scrollback();
        let mut rows = Vec::with_capacity(depth);
        let mut start = 0;
        while start < depth {
            screen.set_scrollback(depth - start);
            let take = (depth - start).min(usize::from(SCREEN_ROWS));
            rows.extend(
                screen
                    .rows(0, SCREEN_COLS)
                    .take(take)
                    .map(|row| row.trim_end().to_string()),
            );
            start += take;
        }
        screen.set_scrollback(0);
        rows
    }

    /// Every non-blank row the user can reach, oldest first: the strongest
    /// invariant this probe has — any lost, duplicated or stale row breaks
    /// the expected sequence.
    fn nonblank_rows(&self) -> Vec<String> {
        self.scrollback_rows()
            .into_iter()
            .chain(self.visible_rows())
            .filter(|row| !row.is_empty())
            .collect()
    }

    fn cursor(&self) -> (u16, u16) {
        self.backend.parser.borrow().screen().cursor_position()
    }
}

/// The TUI stand-in: a bottom-anchored band of status + stream tail +
/// composer over a transcript of flushed history rows.
struct ProbeApp {
    terminal: Terminal<VtBackend>,
    band_height: u16,
    composer_rows: u16,
    turn: u16,
    /// Composer cursor row offset within the band (bottom row by default);
    /// grow/shrink must restore it undisturbed.
    composer_cursor_offset: u16,
}

impl ProbeApp {
    fn boot(world: &World, band_height: u16, composer_rows: u16) -> Self {
        let terminal = Terminal::with_options(
            world.backend.clone(),
            TerminalOptions {
                viewport: Viewport::Inline(band_height),
            },
        )
        .expect("boot terminal");
        let mut app = Self {
            terminal,
            band_height,
            composer_rows,
            turn: 0,
            composer_cursor_offset: 0,
        };
        app.draw_band();
        app
    }

    fn band_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(format!(
            "STATUS·h{}·t{}",
            self.band_height, self.turn
        ))];
        let tail_rows = self.band_height - 1 - self.composer_rows;
        for i in 0..tail_rows {
            lines.push(Line::from(format!("TAIL·t{}·r{i}", self.turn)));
        }
        for i in 0..self.composer_rows {
            lines.push(Line::from(format!("PROMPT·c{i}")));
        }
        lines
    }

    fn draw_band(&mut self) {
        let cursor_offset = self.composer_cursor_offset;
        let lines = self.band_lines();
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Paragraph::new(lines), area);
                frame.set_cursor_position(Position::new(
                    2,
                    area.bottom().saturating_sub(1 + cursor_offset),
                ));
            })
            .expect("draw band");
    }

    /// The flush path: completed rows leave into scrollback above the band.
    fn flush_history(&mut self, rows: &[String]) {
        self.turn += 1;
        let height = u16::try_from(rows.len()).expect("probe rows fit u16");
        let lines: Vec<Line<'static>> = rows.iter().cloned().map(Line::from).collect();
        self.terminal
            .insert_before(height, |buf| {
                Paragraph::new(lines).render(buf.area, buf);
            })
            .expect("flush history");
        self.draw_band();
    }

    /// Grow protocol: push history up by delta with blank inserts (so the
    /// rows the taller band will cover are blanks, never visible content),
    /// park the cursor at the future band top, then recreate — the
    /// re-anchor's `append_lines` then lands exactly at the bottom row and
    /// scrolls nothing.
    fn grow(&mut self, new_height: u16) {
        // Height policy works on effective (screen-clamped) heights: ratatui
        // clamps the viewport too, but without an early no-op the anchor
        // math below underflows when the band already fills the screen.
        let new_height = new_height.min(SCREEN_ROWS);
        if new_height == self.band_height {
            return;
        }
        let delta = new_height - self.band_height;
        self.terminal
            .insert_before(delta, |_buf| {})
            .expect("grow: blank insert");
        let new_top = self.terminal.get_frame().area().y - delta;
        self.terminal
            .set_cursor_position(Position::new(0, new_top))
            .expect("grow: park cursor");
        self.recreate(new_height);
    }

    /// Shrink protocol: clear the old band (its vacated rows become the
    /// bounded blank residue the ADR accepts), park the cursor delta rows
    /// lower, recreate. The blank gap is consumed by later flushes.
    fn shrink(&mut self, new_height: u16) {
        let delta = self.band_height - new_height;
        self.terminal.clear().expect("shrink: clear band");
        let new_top = self.terminal.get_frame().area().y + delta;
        self.terminal
            .set_cursor_position(Position::new(0, new_top))
            .expect("shrink: park cursor");
        self.recreate(new_height);
    }

    /// The falsification baseline: recreate with no protocol, to document
    /// the residue the protocol exists to prevent.
    fn grow_naive(&mut self, new_height: u16) {
        self.recreate(new_height);
    }

    fn recreate(&mut self, new_height: u16) {
        self.terminal = Terminal::with_options(
            self.terminal.backend().clone(),
            TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )
        .expect("recreate terminal");
        self.band_height = new_height;
        self.draw_band();
    }

    fn expected_band(&self) -> Vec<String> {
        self.band_lines()
            .into_iter()
            .map(|line| line.to_string())
            .collect()
    }

    fn expected_cursor(&mut self) -> (u16, u16) {
        let top = self.terminal.get_frame().area().y;
        (top + self.band_height - 1 - self.composer_cursor_offset, 2)
    }
}

fn shell_lines(count: u16) -> Vec<String> {
    (0..count).map(|i| format!("sh$·cmd·{i:02}")).collect()
}

fn history_rows(turn: u16, count: u16) -> Vec<String> {
    (0..count).map(|i| format!("H{turn:02}·{i:02}")).collect()
}

/// The full-sequence assertion: scrollback + screen, oldest first, must be
/// exactly the emitted history followed by the current band — nothing lost,
/// nothing duplicated, nothing stale.
fn assert_world(world: &World, app: &mut ProbeApp, expected_history: &[String]) {
    let mut expected = expected_history.to_vec();
    expected.extend(app.expected_band());
    assert_eq!(
        world.nonblank_rows(),
        expected,
        "scrollback+screen sequence (visible: {:?}, scrollback: {:?})",
        world.visible_rows(),
        world.scrollback_rows()
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// Boot a bottom-anchored band over a 20-line shell session, then flush one
/// turn of history; returns the accumulated expected history.
fn boot_anchored(world: &World, band_height: u16, composer_rows: u16) -> (ProbeApp, Vec<String>) {
    let mut expected = shell_lines(20);
    world.print_lines(&expected);
    let mut app = ProbeApp::boot(world, band_height, composer_rows);
    let turn_rows = history_rows(app.turn + 1, 6);
    app.flush_history(&turn_rows);
    expected.extend(turn_rows);
    (app, expected)
}

#[test]
fn rig_sanity_fixed_height_anchors_and_inserts() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 8, 2);
    assert_world(&world, &mut app, &expected);
    // The band is bottom-anchored after the boot scroll: status sits 8 rows
    // above the screen bottom.
    let visible = world.visible_rows();
    assert_eq!(visible[16], "STATUS·h8·t1");
    assert_eq!(visible[23], "PROMPT·c1");
}

#[test]
fn naive_recreation_leaves_stale_band_rows() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    app.grow_naive(12);
    // The old band's image is scrolled up by the re-anchor and its top rows
    // survive above the new viewport: two STATUS generations on screen.
    let statuses = world
        .nonblank_rows()
        .into_iter()
        .filter(|row| row.starts_with("STATUS"))
        .count();
    assert_eq!(statuses, 2, "naive grow must show the residue it documents");
}

#[test]
fn grow_protocol_preserves_history_with_zero_residue() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world, 8, 2);
    app.grow(12);
    assert_world(&world, &mut app, &expected);
    // Streaming continues undisturbed on the taller band.
    let turn_rows = history_rows(app.turn + 1, 4);
    app.flush_history(&turn_rows);
    expected.extend(turn_rows);
    assert_world(&world, &mut app, &expected);
}

#[test]
fn shrink_protocol_leaves_only_bounded_blank_rows() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 12, 2);
    app.shrink(5);
    assert_world(&world, &mut app, &expected);
    let visible = world.visible_rows();
    // Exactly the vacated delta rows are blank — the bounded, self-healing
    // residue class (consumed by later flushes); the band re-anchors at the
    // bottom.
    let blank_run = visible[..19]
        .iter()
        .rev()
        .take_while(|row| row.is_empty())
        .count();
    assert_eq!(
        blank_run, 7,
        "the shrink residue is exactly the vacated rows"
    );
    assert_eq!(visible[19], "STATUS·h5·t1");
    assert_eq!(visible[23], "PROMPT·c1");
}

#[test]
fn grow_shrink_churn_stays_exact() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world, 8, 2);
    for _ in 0..6 {
        let turn_rows = history_rows(app.turn + 1, 5);
        app.flush_history(&turn_rows);
        expected.extend(turn_rows);
        app.grow(12);
        assert_world(&world, &mut app, &expected);
        let turn_rows = history_rows(app.turn + 1, 3);
        app.flush_history(&turn_rows);
        expected.extend(turn_rows);
        app.shrink(8);
        assert_world(&world, &mut app, &expected);
    }
}

#[test]
fn grow_at_session_top_without_history() {
    let world = World::new();
    let expected = shell_lines(1);
    world.print_lines(&expected);
    let mut app = ProbeApp::boot(&world, 6, 2);
    app.grow(12);
    assert_world(&world, &mut app, &expected);
}

#[test]
fn grow_restores_a_mid_band_composer_cursor() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 12, 4);
    app.composer_cursor_offset = 2;
    app.draw_band();
    app.grow(16);
    assert_world(&world, &mut app, &expected);
}

#[test]
fn grow_to_full_screen_height_clamps_and_stays_exact() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 8, 2);
    app.grow(SCREEN_ROWS);
    assert_world(&world, &mut app, &expected);
    // Beyond-screen requests clamp to the screen height (ratatui's
    // max_height rule): the band fills the screen, history lives in
    // scrollback, and future flushes insert straight into it.
    app.grow(SCREEN_ROWS + 6);
    assert_eq!(app.terminal.get_frame().area().height, SCREEN_ROWS);
    assert_world(&world, &mut app, &expected);
}
