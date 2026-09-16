//! The inline shell — the cadmus-tui library layer that owns the raw
//! terminal and the band's lifecycle (anchor, height, `Terminal` recreation,
//! resize reflow, guarded draws) and frames the band the widgets live in
//! (ADR-0018, both 2026-09-14 amendments). The event loop, input broker and
//! widgets build on top of it.
//!
//! Contracts this module enforces:
//!
//! - **One wrapper**: every multi-step terminal mutation is wrapped in
//!   exactly one synchronized-update (2026h) guard, emitted only here, never
//!   nested — real terminals end the update at the first ESU, so a nested
//!   pair would leave the op's tail unguarded (the spike harness nested the
//!   shrink-replay guard inside the resize guard; this layer removes that).
//! - **Guard-stream integrity**: guard bytes go through the same stream the
//!   backend writes to — the spike's tee lesson: routing them to a parallel
//!   raw-stdout handle once punched a hole in the capture.
//! - **Cursor-query tolerance**: the CPR round-trips inside `draw`/`resize`
//!   time out under resize storms and quirky stdio (spike fact F3); those
//!   failures are tolerated and counted in [`ShellStats`], never fatal — the
//!   next op re-anchors and repaints. Insert/clear/recreate failures are
//!   structural and propagate.
//! - **Quiesced stdin at (re)construction**: `Terminal::with_options` issues
//!   a CPR query that races stdin readers (upstream ratatui #2640, open).
//!   Callers must hold all stdin readers quiesced across [`InlineShell::new`]
//!   and [`InlineShell::set_height`] — the input broker's quiesce guard
//!   ([`crate::input::InputBroker::quiesce`]) is the designated seam.

use std::io::{self, Write};

use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::backend::Backend;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

/// Mechanism counters, exposed for the quirk harness's diagnostics and
/// sidecar — self-report for observation, never load-bearing for behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShellStats {
    pub inserts: usize,
    pub inserted_rows: usize,
    pub grows: usize,
    pub shrinks: usize,
    pub shrink_replays: usize,
    pub tolerated_draw_errors: usize,
    pub tolerated_resize_errors: usize,
}

/// See the module docs for the contracts. `B` is io-flavored so guard
/// emission and backend errors share one channel; `Clone` because the OS
/// terminal outlives the `Terminal` — recreation clones the backend handle.
pub struct InlineShell<B: Backend<Error = io::Error> + Clone, W: Write> {
    terminal: Terminal<B>,
    /// Guard-sequence sink: a handle onto the backend's stream (e.g. the
    /// harness's tee), never a parallel raw-stdout handle.
    guard: W,
    /// The effective (screen-clamped) band height.
    band_height: u16,
    /// Last known terminal width — the width-shrink replay trigger.
    width: u16,
    stats: ShellStats,
}

impl<B: Backend<Error = io::Error> + Clone, W: Write> InlineShell<B, W> {
    /// Claim an inline band of `band_height` rows (clamped to the screen) at
    /// the cursor and clear it. Two CPR round-trips (construction + clear) —
    /// the quiesced-stdin contract applies.
    pub fn new(backend: B, guard: W, band_height: u16) -> io::Result<Self> {
        let screen = backend.size()?;
        let band_height = clamp_height(band_height, screen.height);
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(band_height),
            },
        )?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            guard,
            band_height,
            width: screen.width,
            stats: ShellStats::default(),
        })
    }

    #[must_use]
    pub fn band_height(&self) -> u16 {
        self.band_height
    }

    /// The band's current on-screen area, for layout and cursor math.
    pub fn band_area(&mut self) -> Rect {
        self.terminal.get_frame().area()
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The screen's current row count — the layout function's input.
    pub fn screen_rows(&mut self) -> u16 {
        self.terminal
            .size()
            .map_or(self.band_height, |size| size.height)
    }

    /// Whether `set_height(desired)` would do anything (its equality no-op,
    /// exposed so the app can skip the quiesce window when nothing would
    /// change — the recreation seam is for real height changes only).
    pub fn needs_height_change(&mut self, desired: u16) -> bool {
        let Ok(screen_rows) = self.terminal.size().map(|size| size.height) else {
            return false;
        };
        clamp_height(desired, screen_rows) != clamp_height(self.band_height, screen_rows)
    }

    #[must_use]
    pub fn stats(&self) -> ShellStats {
        self.stats
    }

    /// Repaint the band under one wrapper; failure is tolerated and counted
    /// (the next op repaints) — drawing is never allowed to kill a run.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) {
        if self
            .guarded(|shell| {
                shell.tolerated_draw(render);
                Ok(())
            })
            .is_err()
        {
            // The guard stream itself failed (draw errors are counted inside
            // `tolerated_draw`): the terminal is likely wedged mid-guard, but
            // drawing must never kill a run.
            self.stats.tolerated_draw_errors += 1;
        }
    }

    /// Completed rows leave the band into real scrollback: insert them above
    /// the viewport, then repaint (the portable insert path clears the
    /// viewport on its way out). One wrapper around the pair.
    pub fn flush(
        &mut self,
        rows: &[Line<'_>],
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
        self.guarded(|shell| {
            shell.terminal.insert_before(height, |buf| {
                Paragraph::new(rows.to_vec()).render(buf.area, buf);
            })?;
            shell.stats.inserts += 1;
            shell.stats.inserted_rows += rows.len();
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    /// The dynamic-height mechanism, event-driven (composer line crossings,
    /// held-block settle, resize) and never per-frame. Policy works on
    /// effective (screen-clamped) heights: both `desired` and the current
    /// height clamp to the screen before comparing, and equality no-ops —
    /// without the early no-op the grow math underflows when the band fills
    /// the screen. Grow: insert Δ blanks above the band, park the cursor at
    /// the future band top, recreate — the re-anchor's append lands exactly
    /// at the bottom row (zero scroll, zero residue). Shrink: clear the old
    /// band, park Δ rows lower, recreate — the vacated Δ rows are the bounded
    /// blank residue, consumed by later flushes (ADR-0018's second 2026-09-14
    /// amendment). One wrapper around insert/clear + recreate + draw.
    pub fn set_height(
        &mut self,
        desired: u16,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let screen_rows = self.terminal.size()?.height;
        let new_height = clamp_height(desired, screen_rows);
        let current = clamp_height(self.band_height, screen_rows);
        if new_height == current {
            return Ok(());
        }
        if new_height > current {
            self.grow_from(new_height, current, render)
        } else {
            self.shrink_from(new_height, current, render)
        }
    }

    /// Resize handling under one wrapper: re-anchor (a CPR round-trip whose
    /// failure is tolerated and counted — the next resize or draw
    /// re-anchors), then on a horizontal shrink replay the still-visible
    /// history tail from source (stock ratatui clears the screen on shrink).
    /// `replay_tail(max_rows, width)` returns up to `max_rows` wrapped rows
    /// from the caller's event source — the shell never retains history.
    ///
    /// Width follows the terminal. The recorded band height is the requested
    /// height, not re-clamped here: ratatui re-clamps the viewport to the
    /// screen on every resize (and re-expands when the screen grows back),
    /// so the on-screen effective height is always `min(requested, screen)`
    /// and `set_height` derives its deltas from that. Re-deriving the desired
    /// height after a resize is the caller's job (height policy is
    /// event-driven). Processing cadence (the ~75 ms debounce, spike
    /// discipline 2) belongs to the event loop, not here.
    pub fn on_resize(
        &mut self,
        cols: u16,
        rows: u16,
        replay_tail: impl FnOnce(u16, u16) -> Vec<Line<'static>>,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let shrunk = cols < self.width;
        self.width = cols;
        self.guarded(|shell| {
            if shell.terminal.resize(Rect::new(0, 0, cols, rows)).is_err() {
                shell.stats.tolerated_resize_errors += 1;
                return Ok(());
            }
            if shrunk {
                let visible_history = rows.saturating_sub(shell.band_height);
                let tail = replay_tail(visible_history, cols);
                if !tail.is_empty() {
                    let height = u16::try_from(tail.len()).unwrap_or(u16::MAX);
                    let len = tail.len();
                    shell.terminal.insert_before(height, |buf| {
                        Paragraph::new(tail).render(buf.area, buf);
                    })?;
                    shell.stats.inserts += 1;
                    shell.stats.inserted_rows += len;
                    shell.stats.shrink_replays += 1;
                }
            }
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    fn grow_from(
        &mut self,
        new_height: u16,
        current: u16,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let delta = new_height - current;
        self.guarded(|shell| {
            shell.terminal.insert_before(delta, |_buf| {})?;
            let area_y = shell.terminal.get_frame().area().y;
            debug_assert!(
                area_y >= delta,
                "grow invariant: a bottom-anchored band of {current} rows on a \
                 clamped screen always has Δ={delta} rows of room above"
            );
            let new_top = area_y - delta;
            shell
                .terminal
                .set_cursor_position(Position::new(0, new_top))?;
            shell.recreate(new_height)?;
            shell.stats.grows += 1;
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    fn shrink_from(
        &mut self,
        new_height: u16,
        current: u16,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let delta = current - new_height;
        self.guarded(|shell| {
            shell.terminal.clear()?;
            let new_top = shell.terminal.get_frame().area().y + delta;
            shell
                .terminal
                .set_cursor_position(Position::new(0, new_top))?;
            shell.recreate(new_height)?;
            shell.stats.shrinks += 1;
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    /// The recreation seam (spike fact F1's escape hatch, adopted by the
    /// second 2026-09-14 amendment). Construction issues a CPR query — the
    /// module docs' quiesced-stdin contract applies. Width re-syncs here:
    /// a resize landing inside a quiesce window is missed by the input
    /// contract, and recreation is the one place that always re-reads the
    /// terminal (the next debounced resize still owns the replay path).
    fn recreate(&mut self, new_height: u16) -> io::Result<()> {
        self.terminal = Terminal::with_options(
            self.terminal.backend().clone(),
            TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )?;
        self.band_height = new_height;
        self.width = self.terminal.size()?.width;
        Ok(())
    }

    /// Draw, tolerating CPR-class failure (spike discipline 3).
    fn tolerated_draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) {
        if self.terminal.draw(render).is_err() {
            self.stats.tolerated_draw_errors += 1;
        }
    }

    /// One 2026h wrapper around `op`: begin, run, always end — a leaked open
    /// guard would wedge the terminal's compositing for the rest of the run.
    fn guarded(&mut self, op: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        execute!(&mut self.guard, BeginSynchronizedUpdate)?;
        let result = op(self);
        let end = execute!(&mut self.guard, EndSynchronizedUpdate);
        result.and(end)
    }
}

/// Height policy works on effective (screen-clamped) heights; a band is at
/// least one row.
fn clamp_height(desired: u16, screen_rows: u16) -> u16 {
    desired.clamp(1, screen_rows.max(1))
}
