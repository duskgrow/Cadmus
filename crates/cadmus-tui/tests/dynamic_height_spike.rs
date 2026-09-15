//! The inline shell's deterministic suite: a vt100-emulated terminal locks
//! the recreation protocol's row bookkeeping (ADR-0018, second 2026-09-14
//! amendment) — zero history rows lost or duplicated, bounded residue only,
//! the composer cursor stable across every height change, and exactly one
//! 2026h wrapper per op.
//!
//! Born as the dynamic-height spike probe; the verdict evidence and the
//! real-terminal matrix live in
//! `docs/research/2026-09-14-terminal-recreation-spike.md`. The suite now
//! drives the production mechanism (`cadmus_tui::shell::InlineShell`) — what
//! vt100 cannot judge (compositing, flicker) stays with the manual matrix in
//! `examples/inline_spike.rs`.
//!
//! The rig: a `Backend` impl that feeds a `vt100::Parser` the same escape
//! sequences ratatui-crossterm emits (CUP + symbol per cell, `\n` per
//! appended line, ED for clears). vt100 then applies real terminal semantics
//! — scrolling, scrollback, deferred wrap — instead of us re-deriving them.
//! Cursor-position queries are answered from the emulated screen, which is
//! exactly what a real terminal does; styles are omitted because SGR bytes
//! never move rows, and rows are what this suite judges. Guard bytes (2026h)
//! go to a separate sink — vt100 ignores them, and the wrapper structure is
//! asserted verbatim instead.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::rc::Rc;

use cadmus_tui::shell::InlineShell;
use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

const SCREEN_ROWS: u16 = 24;
const SCREEN_COLS: u16 = 80;
/// Deep enough that no assertion below ever hits the scrollback cap.
const SCROLLBACK_LEN: usize = 500;

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";

/// Records the shell's guard bytes. Guards never reach the vt100 parser (the
/// screen model ignores them), so the one-wrapper invariant is asserted on
/// this verbatim recording instead.
#[derive(Clone, Default)]
struct GuardSink {
    log: Rc<RefCell<Vec<u8>>>,
}

impl GuardSink {
    /// Drain the recording: each op should leave exactly one begin/end pair.
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.log.borrow_mut())
    }
}

impl Write for GuardSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.log.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Shared handle to the emulated terminal; cloning gives a recreated
/// `Terminal` the same screen state — which is the whole point of the
/// recreation protocol (the OS terminal also outlives the `Terminal`).
/// `fail_queries` injects cursor-query failure on demand: the tolerance
/// contract (spike discipline 3) only exists to be tested.
#[derive(Clone)]
struct VtBackend {
    parser: Rc<RefCell<vt100::Parser>>,
    fail_queries: Rc<std::cell::Cell<bool>>,
}

impl VtBackend {
    fn new() -> Self {
        Self {
            parser: Rc::new(RefCell::new(vt100::Parser::new(
                SCREEN_ROWS,
                SCREEN_COLS,
                SCROLLBACK_LEN,
            ))),
            fail_queries: Rc::new(std::cell::Cell::new(false)),
        }
    }

    fn emit(&self, bytes: &str) {
        self.parser.borrow_mut().process(bytes.as_bytes());
    }
}

impl Backend for VtBackend {
    // The shell unifies guard emission and backend errors on io::Error; the
    // rig never fails, so any flavor would do.
    type Error = io::Error;

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
        if self.fail_queries.get() {
            return Err(io::Error::other("injected CPR failure"));
        }
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

    fn resize(&self, rows: u16, cols: u16) {
        self.backend
            .parser
            .borrow_mut()
            .screen_mut()
            .set_size(rows, cols);
    }

    /// Make cursor-position queries fail until cleared — a real terminal's
    /// CPR times out under resize storms and quirky stdio (spike fact F3).
    fn fail_queries(&self, fail: bool) {
        self.backend.fail_queries.set(fail);
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
    /// invariant this suite has — any lost, duplicated or stale row breaks
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

/// The band's content, derived from the frame's live height so the repaint
/// after a recreation already shows the new geometry.
fn band_lines(band_height: u16, turn: u16, composer_rows: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!("STATUS·h{band_height}·t{turn}"))];
    let tail_rows = band_height - 1 - composer_rows;
    for i in 0..tail_rows {
        lines.push(Line::from(format!("TAIL·t{turn}·r{i}")));
    }
    for i in 0..composer_rows {
        lines.push(Line::from(format!("PROMPT·c{i}")));
    }
    lines
}

/// An owned renderer over the content model — the materialize-then-draw
/// pattern: the closure captures data, never a borrow of the app, so it can
/// run inside shell ops that hold the terminal mutably.
fn band_render(
    turn: u16,
    composer_rows: u16,
    cursor_offset: u16,
) -> impl FnOnce(&mut Frame<'_>) + use<> {
    move |frame: &mut Frame<'_>| {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(band_lines(area.height, turn, composer_rows)),
            area,
        );
        frame.set_cursor_position(Position::new(
            2,
            area.bottom().saturating_sub(1 + cursor_offset),
        ));
    }
}

/// The TUI stand-in: the shell owns the terminal; the app model is just a
/// turn counter, the composer geometry and the cursor offset — the stand-in
/// for view-model state the widgets will own.
struct ProbeApp {
    shell: InlineShell<VtBackend, GuardSink>,
    guard_sink: GuardSink,
    composer_rows: u16,
    turn: u16,
    /// Composer cursor row offset within the band (bottom row by default);
    /// grow/shrink must restore it undisturbed.
    composer_cursor_offset: u16,
}

impl ProbeApp {
    fn boot(world: &World, band_height: u16, composer_rows: u16) -> Self {
        let guard_sink = GuardSink::default();
        let shell = InlineShell::new(world.backend.clone(), guard_sink.clone(), band_height)
            .expect("boot shell");
        let mut app = Self {
            shell,
            guard_sink,
            composer_rows,
            turn: 0,
            composer_cursor_offset: 0,
        };
        app.draw_band();
        app
    }

    fn render(&self) -> impl FnOnce(&mut Frame<'_>) + use<> {
        band_render(self.turn, self.composer_rows, self.composer_cursor_offset)
    }

    fn draw_band(&mut self) {
        let render = self.render();
        self.shell.draw(render);
    }

    /// The flush path: completed rows leave into scrollback above the band.
    fn flush_history(&mut self, rows: &[String]) {
        self.turn += 1;
        let lines: Vec<Line<'static>> = rows.iter().cloned().map(Line::from).collect();
        let render = self.render();
        self.shell.flush(&lines, render).expect("flush history");
    }

    fn grow(&mut self, new_height: u16) {
        let render = self.render();
        self.shell.set_height(new_height, render).expect("grow");
    }

    fn shrink(&mut self, new_height: u16) {
        let render = self.render();
        self.shell.set_height(new_height, render).expect("shrink");
    }

    fn on_resize(
        &mut self,
        cols: u16,
        rows: u16,
        replay_tail: impl FnOnce(u16, u16) -> Vec<Line<'static>>,
    ) {
        let render = self.render();
        self.shell
            .on_resize(cols, rows, replay_tail, render)
            .expect("resize");
    }

    fn expected_band(&self) -> Vec<String> {
        band_lines(self.shell.band_height(), self.turn, self.composer_rows)
            .into_iter()
            .map(|line| line.to_string())
            .collect()
    }

    fn expected_cursor(&mut self) -> (u16, u16) {
        let top = self.shell.band_area().y;
        (
            top + self.shell.band_height() - 1 - self.composer_cursor_offset,
            2,
        )
    }
}

/// The falsification baseline: a bare `Terminal` recreation with no protocol,
/// bypassing the shell on purpose — proves the world readers can see the
/// residue the shell's protocol exists to prevent (an assertion suite that
/// never sees red proves nothing).
fn grow_naive(world: &World, app: &ProbeApp, new_height: u16) {
    let mut bare = Terminal::with_options(
        world.backend.clone(),
        TerminalOptions {
            viewport: Viewport::Inline(new_height),
        },
    )
    .expect("naive recreate");
    bare.draw(|frame| {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(band_lines(new_height, app.turn, app.composer_rows)),
            area,
        );
    })
    .expect("naive draw");
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
    let (app, _expected) = boot_anchored(&world, 8, 2);
    grow_naive(&world, &app, 12);
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
    assert_eq!(app.shell.band_area().height, SCREEN_ROWS);
    assert_world(&world, &mut app, &expected);
}

/// The one-wrapper invariant, asserted on the guard sink's verbatim bytes:
/// each op — including resize-with-replay, the path the spike harness once
/// nested — emits exactly one begin/end pair.
#[test]
fn one_wrapper_per_op_and_never_nested() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    let one_wrapper = [BSU, ESU].concat();

    app.guard_sink.take(); // boot's draw + the first flush

    app.draw_band();
    assert_eq!(app.guard_sink.take(), one_wrapper, "draw");

    app.flush_history(&history_rows(2, 3));
    assert_eq!(app.guard_sink.take(), one_wrapper, "flush");

    app.grow(12);
    assert_eq!(app.guard_sink.take(), one_wrapper, "grow");

    app.shrink(8);
    assert_eq!(app.guard_sink.take(), one_wrapper, "shrink");

    world.resize(SCREEN_ROWS, 60);
    app.on_resize(60, SCREEN_ROWS, |_max_rows, _width| {
        vec![Line::from("REPLAY")]
    });
    assert_eq!(app.guard_sink.take(), one_wrapper, "resize with replay");

    world.resize(SCREEN_ROWS, 64);
    app.on_resize(64, SCREEN_ROWS, |_max_rows, _width| vec![]);
    assert_eq!(app.guard_sink.take(), one_wrapper, "plain resize");
}

/// Width-shrink: stock ratatui clears the screen (the visible history with
/// it); the shell replays the still-visible tail from source. Scrollback is
/// untouched, exactly the replayed rows return above the band, and the band
/// repaints intact below them. The transcript is deliberately taller than
/// the replay window so the cap itself is asserted; the growth leg pins the
/// shrink trigger's direction (a stale width would fire a spurious replay).
#[test]
fn width_shrink_replays_the_visible_tail_from_source() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    // Two more turns: the transcript (18 rows) exceeds the replay window
    // (screen − band = 16), so a wrong cap shows up as a row diff.
    app.flush_history(&history_rows(2, 6));
    app.flush_history(&history_rows(3, 6));
    let transcript = history_rows(1, 6)
        .into_iter()
        .chain(history_rows(2, 6))
        .chain(history_rows(3, 6))
        .collect::<Vec<_>>();
    let pre_scrollback = world.scrollback_rows();

    let replay_rows = transcript.clone();
    world.resize(SCREEN_ROWS, 60);
    app.on_resize(60, SCREEN_ROWS, move |max_rows, _width| {
        replay_rows
            .iter()
            .skip(replay_rows.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });
    assert_eq!(app.shell.width(), 60, "the shrink trigger's width state");

    // The clear took the still-visible rows with it; scrollback is untouched
    // and exactly the windowed tail returns above the repainted band.
    assert_eq!(
        world.scrollback_rows(),
        pre_scrollback,
        "width shrink must not touch scrollback"
    );
    let expected_after_shrink = |app: &mut ProbeApp| {
        let mut expected_now = pre_scrollback.clone();
        // The window is screen minus band: exactly the last 16 of 18 rows.
        expected_now.extend(transcript[2..].iter().cloned());
        expected_now.extend(app.expected_band());
        expected_now
    };
    assert_eq!(
        world.nonblank_rows(),
        expected_after_shrink(&mut app),
        "visible: {:?}, scrollback: {:?}",
        world.visible_rows(),
        world.scrollback_rows()
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");

    // Growth: the shrink trigger must not fire (the replay closure would
    // panic), and the width state follows.
    world.resize(SCREEN_ROWS, 70);
    app.on_resize(70, SCREEN_ROWS, |_max_rows, _width| {
        unreachable!("replay must not fire on width growth")
    });
    assert_eq!(app.shell.width(), 70);
    assert_eq!(
        world.nonblank_rows(),
        expected_after_shrink(&mut app),
        "growth leg must leave the world untouched"
    );

    // A fresh shrink replays again from the same source.
    world.resize(SCREEN_ROWS, 60);
    let replay_rows = transcript.clone();
    app.on_resize(60, SCREEN_ROWS, move |max_rows, _width| {
        replay_rows
            .iter()
            .skip(replay_rows.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });
    assert_eq!(
        world.nonblank_rows(),
        expected_after_shrink(&mut app),
        "a second shrink re-materializes the same tail"
    );
}

/// Spike discipline 3, locked: a CPR timeout inside `draw`'s autoresize is
/// tolerated and counted, the wrapper stays balanced, and the next op
/// re-anchors and repaints — drawing never kills a run.
#[test]
fn draw_tolerates_a_failed_reanchor_and_recovers() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    app.guard_sink.take();

    // Backend size drifts from ratatui's last-known area, so the next draw's
    // autoresize re-anchors — into an injected CPR failure.
    world.resize(20, SCREEN_COLS);
    world.fail_queries(true);
    app.draw_band();
    assert_eq!(app.shell.stats().tolerated_draw_errors, 1);
    assert_eq!(
        app.guard_sink.take(),
        [BSU, ESU].concat(),
        "a failed op still closes its wrapper"
    );

    world.fail_queries(false);
    app.draw_band();
    assert_eq!(app.shell.stats().tolerated_draw_errors, 1);
    assert_eq!(app.shell.band_area().height, 8, "the next op re-anchored");
}

/// The explicit resize path tolerates the same failure: counted, replay
/// skipped, wrapper balanced, band height untouched.
#[test]
fn resize_reanchor_failure_is_tolerated() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    app.guard_sink.take();

    world.fail_queries(true);
    app.on_resize(64, SCREEN_ROWS, |_max_rows, _width| {
        unreachable!("replay must be skipped when the re-anchor fails")
    });
    assert_eq!(app.shell.stats().tolerated_resize_errors, 1);
    assert_eq!(app.guard_sink.take(), [BSU, ESU].concat());
    assert_eq!(app.shell.band_height(), 8);
    assert_eq!(app.shell.width(), 64);
}

/// Screen shorter than the band: ratatui clamps the viewport on resize, the
/// recorded (requested) height survives, and height requests inside the
/// clamp are inert no-ops — no guard bytes, no emitted sequences.
#[test]
fn short_screen_clamps_and_regrows() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);

    world.resize(5, SCREEN_COLS);
    app.on_resize(SCREEN_COLS, 5, |_max_rows, _width| vec![]);
    assert_eq!(app.shell.band_height(), 8, "requested height survives");
    assert_eq!(
        app.shell.band_area().height,
        5,
        "viewport clamped on-screen"
    );

    app.guard_sink.take();
    app.grow(7); // inside the clamp on both sides: inert
    assert_eq!(
        app.guard_sink.take(),
        Vec::<u8>::new(),
        "no-op emits nothing"
    );
    assert_eq!(app.shell.band_area().height, 5);

    world.resize(SCREEN_ROWS, SCREEN_COLS);
    app.on_resize(SCREEN_COLS, SCREEN_ROWS, |_max_rows, _width| vec![]);
    assert_eq!(
        app.shell.band_area().height,
        8,
        "viewport re-expands with the screen"
    );
    app.grow(12);
    assert_eq!(app.shell.band_area().height, 12);
}
