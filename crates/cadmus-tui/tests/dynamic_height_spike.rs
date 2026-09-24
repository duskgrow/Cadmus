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
use common::{BSU, ESU, GuardSink, SCREEN_COLS, SCREEN_ROWS, VtBackend, World, hard_wrap};
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
        Self::boot_with_strategy(
            world,
            band_height,
            composer_rows,
            ScrollbackStrategy::FullScreen,
        )
    }

    /// The strategy doubles as the terminal's anchor class on grow
    /// (ADR-0018, 2026-09-22 4th amendment): `FullScreen` follows the
    /// content (restoring terminals), `Standard` keeps the top edge.
    fn boot_with_strategy(
        world: &World,
        band_height: u16,
        composer_rows: u16,
        strategy: ScrollbackStrategy,
    ) -> Self {
        let guard_sink = GuardSink::default();
        let shell = InlineShell::new(
            world.backend.clone(),
            guard_sink.clone(),
            band_height,
            strategy,
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
    boot_anchored_with_strategy(
        world,
        band_height,
        composer_rows,
        ScrollbackStrategy::FullScreen,
    )
}

fn boot_anchored_with_strategy(
    world: &World,
    band_height: u16,
    composer_rows: u16,
    strategy: ScrollbackStrategy,
) -> (ProbeApp, Vec<String>) {
    let mut expected = shell_lines(20);
    world.print_lines(&expected);
    let mut app = ProbeApp::boot_with_strategy(world, band_height, composer_rows, strategy);
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

/// Width-shrink: the visible window leaves into scrollback first (the
/// replay below restores only the tail that fits the new window — without
/// the push, the rows in between would die with the clear on every
/// terminal), then the shell replays the still-visible tail from source.
/// The transcript is deliberately taller than the replay window so the cap
/// itself is asserted; the growth leg pins the shrink trigger's direction
/// (a stale width would fire a spurious replay).
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

    // The push moves the whole visible window (the last 16 of 18 rows) into
    // scrollback; exactly the windowed tail then returns above the band.
    let mut scrollback_after_shrink = pre_scrollback.clone();
    scrollback_after_shrink.extend(transcript[2..].iter().cloned());
    assert_eq!(
        world.scrollback_rows(),
        scrollback_after_shrink,
        "the visible window leaves into scrollback before the clear"
    );
    let expected_after_shrink = |app: &mut ProbeApp| {
        let mut expected_now = scrollback_after_shrink.clone();
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

    // A fresh shrink replays again from the same source: the first shrink's
    // replayed tail is the visible window now, so it leaves into scrollback
    // before the same tail returns above the band.
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
    let mut expected_second = scrollback_after_shrink.clone();
    expected_second.extend(transcript[2..].iter().cloned());
    expected_second.extend(transcript[2..].iter().cloned());
    expected_second.extend(app.expected_band());
    assert_eq!(
        world.nonblank_rows(),
        expected_second,
        "a second shrink re-materializes the same tail"
    );
}

/// The 2026-09-22 regression pin: rows long enough to re-wrap TALLER at the
/// new width. A width shrink erases the visible window and the replay
/// restores only the tail that still fits — without first scrolling the
/// window into scrollback, the rows past the window's top edge died with
/// the clear on every terminal (reflow or not). The short-row shrink test
/// above cannot see the loss: its rows re-wrap to the same height.
#[test]
fn width_shrink_saves_now_orphaned_rows_into_scrollback() {
    let world = World::new();
    let mut app = ProbeApp::boot(&world, 8, 2);
    // 100 cells per logical row: 2 display rows at 80 columns, 3 at 40.
    let source: Vec<String> = (0..12)
        .map(|i| format!("H{i:02}·{}", "x".repeat(93)))
        .collect();
    for chunk in source.chunks(4) {
        app.flush_history(chunk);
    }
    // 24 display rows at 80 columns: 8 in scrollback, the last 16 visible.
    assert_eq!(app.shell.band_area().y, 16);

    world.resize(SCREEN_ROWS, 40);
    // Capture what the terminal holds once it has resized (vt100 truncates,
    // the non-reflow class): the push must copy exactly this state.
    let pre_scrollback = world.scrollback_rows();
    let window = world.visible_rows()[..16].to_vec();

    let replay_source = source.clone();
    app.on_resize(40, SCREEN_ROWS, move |max_rows, width| {
        let rows: Vec<String> = replay_source
            .iter()
            .flat_map(|row| hard_wrap(row, width))
            .collect();
        let skip = rows.len().saturating_sub(usize::from(max_rows));
        rows.into_iter().skip(skip).map(Line::from).collect()
    });

    // Nothing is lost: scrollback holds the pre-shrink scrollback plus the
    // whole pushed window; the re-wrapped tail (36 rows at 40 columns, the
    // last 16) fills the screen above the band.
    assert_eq!(world.scrollback_rows().len(), 8 + 16);
    let mut expected = pre_scrollback;
    expected.extend(window);
    expected.extend(source.iter().flat_map(|row| hard_wrap(row, 40)).skip(20));
    let mut expected_world = expected;
    expected_world.extend(app.expected_band());
    assert_eq!(
        world.nonblank_rows(),
        expected_world,
        "visible: {:?}, scrollback: {:?}",
        world.visible_rows(),
        world.scrollback_rows()
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// The 2026-09-22 height-grow regression pin, restoring-terminal leg
/// (ConPTY/Windows Terminal, reflow xterm): a taller window reveals
/// scrollback rows at the top and shifts every content row down — band
/// image included. The interim fit follows the content; the debounced
/// settle then refills the taller window from source (2026-09-22 4th
/// amendment), re-materializing exactly the rows the terminal restored.
/// vt100 cannot restore on its own, so `restore_on_grow` rebuilds the
/// state; the leg boots the `FullScreen` class restoring terminals map to.
#[test]
fn height_grow_follows_the_content_on_a_restoring_terminal() {
    let world = World::new();
    let (mut app, _expected) =
        boot_anchored_with_strategy(&world, 8, 2, ScrollbackStrategy::FullScreen);
    let pre = world.visible_rows();

    // Grow 24 → 30: six scrollback rows reappear at the top.
    let restored: Vec<String> = (0..6).map(|i| format!("RESTORED-{i:02}")).collect();
    world.restore_on_grow(&restored);
    let mut tail = restored.clone();
    tail.extend(pre[..16].iter().cloned());
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, 30, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    // The settled world: the refilled tail above, the band bottom-glued —
    // restored rows, shell rows and the newest history all intact.
    assert_eq!(app.shell.band_area().y, 16 + 6);
    let visible = world.visible_rows();
    assert_eq!(
        visible[..22],
        tail[..],
        "history above the band: {visible:?}"
    );
    assert_eq!(visible[22..30], app.expected_band()[..], "repainted band");
    assert!(
        visible[30..].iter().all(String::is_empty),
        "nothing below the band"
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// The same grow on a top-anchoring terminal (the vt100 default, tmux's
/// and Zed's class), booted as the `Standard` class (ADR-0018, 2026-09-22
/// 4th amendment): the interim fit erases the band's own image and re-glues
/// it to the new bottom edge — no ghost, nothing scrolled — and the
/// debounced settle refills the revealed rows from source. The settled
/// world is exactly the restoring terminal's, reached without the
/// terminal's help: band glued, taller tail, scrollback untouched.
#[test]
fn height_grow_on_a_top_anchoring_terminal_reglues_and_refills() {
    let world = World::new();
    let (mut app, expected) =
        boot_anchored_with_strategy(&world, 8, 2, ScrollbackStrategy::Standard);
    let pre_scrollback = world.scrollback_rows();

    world.resize(30, SCREEN_COLS);
    // The source's newest 22 displayed rows: the refill re-materializes the
    // six rows the grow revealed (expected[4..10]) above the still-visible
    // sixteen.
    let tail: Vec<String> = expected[expected.len() - 22..].to_vec();
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, 30, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    assert_eq!(app.shell.band_area().y, 22, "band re-glued to the bottom");
    let visible = world.visible_rows();
    assert_eq!(visible[..22], tail[..], "the refilled tail");
    assert_eq!(visible[22..30], app.expected_band()[..], "repainted band");
    assert_eq!(
        world.scrollback_rows(),
        pre_scrollback,
        "the grow pushed nothing into scrollback"
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");

    // A later width shrink still saves the window and replays from source;
    // Standard's partial-region departures are vt100's standing blind spot,
    // so the pin is the visible world after the replay.
    let replay_rows: Vec<String> = (0..6).map(|i| format!("H01·{i:02}")).collect();
    let replay = replay_rows.clone();
    world.resize(30, 60);
    app.on_resize(60, 30, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });
    let visible = world.visible_rows();
    assert_eq!(visible[..6], replay_rows[..], "the replayed tail");
    assert_eq!(app.shell.band_area().y, 6);
}

/// The disclosed misclassification cost, bounded to one composer image per
/// gesture (ADR-0018, 2026-09-22 4th amendment): a terminal detected as
/// the restoring class (`FullScreen`) that actually top-anchors leaves one
/// band ghost above the followed band. The settle's refill erases it from
/// the screen — but the save-push scrolls the whole visible window out
/// first (rows above the band are not necessarily in the source, so the
/// push may not be bounded to the grow amount: loss is the bad direction,
/// duplication the accepted one), and the ghost rides along into
/// scrollback. Pinned: a six-row flush inside the window, then the settle
/// — screen clean, ghost archived exactly once.
#[test]
fn height_grow_with_a_mismatched_strategy_settles_clean() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 8, 2);
    let old_band = app.expected_band();
    let pre_scrollback = world.scrollback_rows();

    world.resize(30, SCREEN_COLS);
    // Inside the drag window: the follow left the band image at rows
    // 16..24 while the band repaints at 22..30. A flush lands before the
    // debounced settle; its full-screen scroll moves six history rows into
    // scrollback.
    let turn_rows = history_rows(app.turn + 1, 6);
    app.flush_history(&turn_rows);
    let mut source = expected.clone();
    source.extend(turn_rows.iter().cloned());

    let tail: Vec<String> = source[source.len() - 22..].to_vec();
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, 30, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    assert_eq!(app.shell.band_area().y, 22);
    let visible = world.visible_rows();
    assert_eq!(visible[..22], tail[..], "the refilled tail, ghost erased");
    assert_eq!(visible[22..30], app.expected_band()[..], "repainted band");
    // The whole-window push departed every visible row: the remaining
    // history, the six flushed rows — and the ghost's six rows, the
    // disclosed misclassification cost (one composer image per gesture).
    let mut expected_scrollback = pre_scrollback;
    expected_scrollback.extend(expected[10..26].iter().cloned());
    expected_scrollback.extend(old_band[..6].iter().cloned());
    expected_scrollback.extend(turn_rows.iter().cloned());
    assert_eq!(
        world.scrollback_rows(),
        expected_scrollback,
        "the window push swept the ghost along — bounded, disclosed"
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// The grow settle's save-then-clear rule, pinned against the review
/// finding (ADR-0018, 2026-09-22 4th amendment): the refill's source
/// covers only transcript rows, so bounding the push to the revealed
/// block would lose the still-visible pre-boot rows above it on every
/// terminal. The whole window departs into scrollback first — the
/// shrink's rule, for the shrink's reason (loss is the bad direction,
/// duplication accepted).
#[test]
fn height_grow_saves_rows_the_source_cannot_replay() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 8, 2);
    let old_band = app.expected_band();
    let pre_scrollback = world.scrollback_rows();

    world.resize(30, SCREEN_COLS);
    // The source holds only the transcript turn rows: the pre-boot shell
    // rows above them are not replayable.
    let replay: Vec<String> = expected[20..].to_vec();
    let tail = replay.clone();
    app.on_resize(SCREEN_COLS, 30, move |max_rows, _width| {
        tail.iter()
            .skip(tail.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    // Every visible row above the band departed into scrollback first
    // (the vacated band image rides along on this top-anchoring rig — the
    // disclosed mismatch sweep): nothing the source cannot rebuild is lost.
    let mut expected_scrollback = pre_scrollback;
    expected_scrollback.extend(expected[10..26].iter().cloned());
    expected_scrollback.extend(old_band[..6].iter().cloned());
    assert_eq!(world.scrollback_rows(), expected_scrollback);
    let visible = world.visible_rows();
    assert_eq!(visible[..6], replay[..], "the replayed transcript tail");
    assert_eq!(app.shell.band_area().y, 6, "band hugs the short refill");
}

/// The clamp recovery on a top-anchoring terminal, booted `Standard`
/// (ADR-0018, 2026-09-22 4th amendment): nothing moved while clamped, so
/// the interim fit erases the clamp image (the shell's own rows) and
/// re-glues the band to the bottom; the debounced settle then refills the
/// revealed rows from source. No displaced scroll, no leak, no ghost —
/// the clamp image never reaches scrollback.
#[test]
fn regrow_from_clamp_on_a_top_anchoring_terminal_reglues_and_refills() {
    let world = World::new();
    let (mut app, expected) =
        boot_anchored_with_strategy(&world, 8, 2, ScrollbackStrategy::Standard);

    world.resize(5, SCREEN_COLS);
    app.on_resize(SCREEN_COLS, 5, |_max_rows, _width| {
        unreachable!("height-only shrink must not replay")
    });
    assert_eq!(app.shell.band_area().height, 5, "clamped band covers");
    let pre_scrollback = world.scrollback_rows();

    world.resize(SCREEN_ROWS, SCREEN_COLS);
    let tail: Vec<String> = expected[expected.len() - 16..].to_vec();
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, SCREEN_ROWS, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    assert_eq!(
        app.shell.band_area(),
        ratatui::layout::Rect::new(0, 16, SCREEN_COLS, 8)
    );
    let visible = world.visible_rows();
    assert_eq!(visible[..16], tail[..], "the refilled tail");
    assert_eq!(visible[16..24], app.expected_band()[..], "repainted band");
    assert_eq!(
        world.scrollback_rows(),
        pre_scrollback,
        "the regrow pushed nothing into scrollback"
    );
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// The clamp recovery under a mismatched restoring strategy, on a
/// top-anchoring rig: the follow's displaced scroll counts the clamped
/// band image's own top rows as the region's content — they depart into
/// permanent scrollback before the debounced settle erases the rest of
/// the image and rebuilds from source. The leak shrinks from "ghost plus
/// scrollback" to exactly those displaced rows (ADR-0018, 2026-09-22 4th
/// amendment), pinned here as the disclosed misclassification cost.
#[test]
fn regrow_from_clamp_with_a_mismatched_strategy_leaks_the_clamp_image() {
    let world = World::new();
    let (mut app, expected) = boot_anchored(&world, 8, 2);

    world.resize(5, SCREEN_COLS);
    app.on_resize(SCREEN_COLS, 5, |_max_rows, _width| {
        unreachable!("height-only shrink must not replay")
    });
    assert_eq!(app.shell.band_area().height, 5, "clamped band covers");
    let clamped_image = band_lines(5, app.turn, app.composer_rows)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    let pre_scrollback = world.scrollback_rows();

    world.resize(SCREEN_ROWS, SCREEN_COLS);
    let tail: Vec<String> = expected[expected.len() - 16..].to_vec();
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, SCREEN_ROWS, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    // The displaced scroll pushed the clamped image's top three rows into
    // scrollback and the settle's save-push swept the remaining two (plus
    // its blank rows) — the whole image leaks under this double mismatch
    // (wrong class on a clamp recovery); the refill rebuilt the window
    // from source.
    assert_eq!(
        app.shell.band_area(),
        ratatui::layout::Rect::new(0, 16, SCREEN_COLS, 8)
    );
    let scrollback = world.scrollback_rows();
    let mut expected_scrollback = pre_scrollback;
    expected_scrollback.extend(clamped_image.iter().cloned());
    expected_scrollback.extend(std::iter::repeat_n(String::new(), 14));
    assert_eq!(scrollback, expected_scrollback, "the disclosed leak");
    let visible = world.visible_rows();
    assert_eq!(visible[..16], tail[..], "the refilled tail, image erased");
    assert_eq!(visible[16..24], app.expected_band()[..], "repainted band");
    assert_eq!(world.cursor(), app.expected_cursor(), "composer cursor");
}

/// Clamp recovery on a restoring terminal: shrink to a screen shorter than
/// the band (it clamps to cover), then regrow — the band image sits at the
/// bottom with restored history above. Re-expanding the band over rows
/// above its image must scroll that history out, not erase it.
#[test]
fn regrow_from_a_clamped_screen_preserves_the_restored_history() {
    let world = World::new();
    let (mut app, _expected) = boot_anchored(&world, 8, 2);

    world.resize(5, SCREEN_COLS);
    app.on_resize(SCREEN_COLS, 5, |_max_rows, _width| {
        unreachable!("height-only must not replay")
    });
    assert_eq!(app.shell.band_area().height, 5, "clamped band covers");

    // Regrow to 24: 19 rows reappear above the five-row band image.
    let restored: Vec<String> = (0..19).map(|i| format!("RESTORED-{i:02}")).collect();
    world.restore_on_grow(&restored);
    let tail: Vec<String> = restored[3..].to_vec();
    let replay = tail.clone();
    app.on_resize(SCREEN_COLS, SCREEN_ROWS, move |max_rows, _width| {
        replay
            .iter()
            .skip(replay.len().saturating_sub(usize::from(max_rows)))
            .cloned()
            .map(Line::from)
            .collect()
    });

    // The band re-expanded at the bottom; the three rows its re-expansion
    // covers left into scrollback during the interim fit, and the settle's
    // save-push returned every remaining restored row — on a consuming
    // terminal (tmux, ConPTY) that is exactly the save, net zero; vt100's
    // copying restore shows the full cycle. The refill then re-materialized
    // the survivors from source — nothing lost.
    assert_eq!(
        app.shell.band_area(),
        ratatui::layout::Rect::new(0, 16, SCREEN_COLS, 8)
    );
    let visible = world.visible_rows();
    assert_eq!(visible[..16], tail[..], "the surviving restored rows");
    assert_eq!(visible[16..24], app.expected_band()[..], "repainted band");
    let scrollback = world.scrollback_rows();
    assert_eq!(
        scrollback[scrollback.len() - 19..],
        restored[..],
        "every restored row is back in scrollback: {scrollback:?}"
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

    // The shrink pushes the visible window into scrollback before the clear.
    let window = world.visible_rows()[..usize::from(app.shell.band_area().y)].to_vec();
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
    expected.extend(window);
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
