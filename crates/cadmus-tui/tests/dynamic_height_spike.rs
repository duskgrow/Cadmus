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
//! `examples/inline_spike.rs`. The vt100 rig lives in `tests/common/` and is
//! shared with `stream_flush.rs`.

mod common;

use cadmus_tui::shell::{InlineShell, ScrollbackStrategy};
use common::{BSU, ESU, GuardSink, SCREEN_COLS, SCREEN_ROWS, VtBackend, World};
use ratatui::layout::Position;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};
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
        let shell = InlineShell::new(
            world.backend.clone(),
            guard_sink.clone(),
            band_height,
            ScrollbackStrategy::FullScreen,
        )
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
fn shrink_protocol_parks_the_blank_buffer_below_the_band() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 12, 2);
    app.shrink(5);
    assert_world(&world, &mut app, &expected);
    let visible = world.visible_rows();
    // Top-anchored: the band keeps its old top edge and the vacated Δ rows
    // sit BELOW it as a blank buffer — re-absorbed by later growth, never a
    // gap inside the transcript above. The boot band is [12, 24): the
    // shrunk band is [12, 17), the buffer [17, 24).
    assert_eq!(visible[12], "STATUS·h5·t1");
    assert_eq!(visible[16], "PROMPT·c1");
    assert!(
        visible[17..].iter().all(String::is_empty),
        "the buffer below the band is exactly the vacated rows: {:?}",
        &visible[17..]
    );
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

/// The stale-width window: the terminal has resized but the debounce still
/// holds the notification. Inserts and later replay must agree on geometry,
/// never move the band back over rows that have just been acknowledged.
#[test]
fn a_flush_in_the_stale_width_window_survives_the_resize() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world, 8, 2);

    // The terminal resizes first; this flush lands in the stale-width
    // window (the shell still thinks 80 columns).
    world.resize(SCREEN_ROWS, 100);
    let turn_rows = history_rows(app.turn + 1, 3);
    app.flush_history(&turn_rows);
    expected.extend(turn_rows);

    // The debounced resize lands; the window's rows must all survive, and
    // the transcript keeps its exact sequence afterwards.
    app.on_resize(100, SCREEN_ROWS, |_, _| Vec::new());
    assert_world(&world, &mut app, &expected);

    // Once more at the new width: another stale-window pair (the terminal
    // grows again while the shell still thinks 100), then its resize.
    world.resize(SCREEN_ROWS, 120);
    let turn_rows = history_rows(app.turn + 1, 2);
    app.flush_history(&turn_rows);
    expected.extend(turn_rows);
    app.on_resize(120, SCREEN_ROWS, |_, _| Vec::new());
    assert_world(&world, &mut app, &expected);
}

/// Post-boot geometry is owned by the shell, not ratatui's inline cursor
/// re-anchor. Even a size drift discovered during draw must never issue CPR.
#[test]
fn draw_adapts_to_size_drift_without_postboot_cursor_queries() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    app.guard_sink.take();
    let boot_queries = world.cursor_queries();

    world.resize(20, SCREEN_COLS);
    world.fail_queries(true);
    for _ in 0..2 {
        app.draw_band();
        assert_eq!(world.cursor_queries(), boot_queries, "no post-boot CPR");
        assert_eq!(app.shell.stats().tolerated_draw_errors, 0);
        assert_eq!(app.shell.stats().tolerated_resize_errors, 0);
        assert_eq!(app.guard_sink.take(), [BSU, ESU].concat());
        let area = app.shell.band_area();
        assert_eq!(area.height, 8);
        assert!(area.bottom() <= 20, "the band fits the physical screen");
        assert_eq!(
            world.visible_rows()[usize::from(area.y)..usize::from(area.bottom())],
            app.expected_band()
        );
        assert_eq!(world.cursor(), app.expected_cursor());
    }
}

/// A blocked CPR channel cannot suppress an explicit resize or its replay:
/// the initial anchor is the last geometry step allowed to query the backend.
#[test]
fn resize_replays_without_postboot_cursor_queries() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);
    app.guard_sink.take();
    let boot_queries = world.cursor_queries();
    let mut expected = world.scrollback_rows();
    let mut replayed = false;

    world.fail_queries(true);
    world.resize(SCREEN_ROWS, 64);
    app.on_resize(64, SCREEN_ROWS, |max_rows, width| {
        replayed = true;
        assert_eq!((max_rows, width), (16, 64));
        vec![Line::from("REPLAY"), Line::from("REPLAY TAIL")]
    });
    assert!(replayed, "a blocked CPR channel must not skip replay");
    assert_eq!(world.cursor_queries(), boot_queries);
    assert_eq!(app.shell.stats().tolerated_resize_errors, 0);
    assert_eq!(app.shell.stats().tolerated_draw_errors, 0);
    assert_eq!(app.guard_sink.take(), [BSU, ESU].concat());
    assert_eq!(app.shell.band_height(), 8);
    assert_eq!(app.shell.width(), 64);
    expected.extend(["REPLAY".into(), "REPLAY TAIL".into()]);
    assert_world(&world, &mut app, &expected);
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
