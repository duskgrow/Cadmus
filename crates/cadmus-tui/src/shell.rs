//! The inline shell owns terminal geometry and history insertion (ADR-0018).
//! Stock Inline reserves the initial band; afterwards stock Fixed renders
//! the rectangle the shell owns. Ratatui must not independently resize or
//! clear history between a raw insert and its acknowledgement.
//!
//! Every multi-step mutation has exactly one synchronized-update wrapper,
//! on the backend's output stream. Draw/resize failures are tolerated and
//! counted; history insert and structural clear failures propagate. The
//! real backend is [`crate::cursor::CursorTracker`]: its one boot CPR runs
//! before the input broker, and no later operation needs a cursor query.

use std::io::{self, Write};

use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::backend::Backend;
use ratatui::layout::{Rect, Size};
use ratatui::text::Line;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

pub use crate::history::ScrollbackStrategy;

/// Mechanism counters for the quirk harness; never load-bearing state.
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

/// The backend and guard writer must share the same output stream. Clone
/// duplicates the backend handle, not the underlying terminal.
pub struct InlineShell<B: Backend<Error = io::Error> + Clone + Write, W: Write> {
    terminal: Terminal<B>,
    guard: W,
    /// Requested height survives a temporarily shorter screen.
    band_height: u16,
    /// Last debounced width, not consumed by an intervening height change.
    width: u16,
    screen: Size,
    /// A shrink observed between debounce ticks still owes a source replay,
    /// even if the window grows again before the notification arrives.
    replay_pending: bool,
    /// A grow fitted between debounce ticks still owes the tail refill:
    /// `fit_screen` re-glues the band but only the debounced replay can
    /// re-materialize the revealed rows from source.
    grow_refit_pending: bool,
    stats: ShellStats,
    scrollback: ScrollbackStrategy,
}

impl<B: Backend<Error = io::Error> + Clone + Write, W: Write> InlineShell<B, W> {
    /// Reserve the initial band at the cursor. On the real terminal this
    /// reads the cursor tracker's seed, never a second CPR.
    pub fn new(
        backend: B,
        guard: W,
        band_height: u16,
        scrollback: ScrollbackStrategy,
    ) -> io::Result<Self> {
        let screen = backend.size()?;
        let band_height = clamp_height(band_height, screen.height);
        let mut initial = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(band_height),
            },
        )?;
        let area = initial.get_frame().area();
        let terminal = Terminal::with_options(
            initial.backend().clone(),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )?;
        let mut shell = Self {
            terminal,
            guard,
            band_height,
            width: screen.width,
            screen,
            replay_pending: false,
            grow_refit_pending: false,
            stats: ShellStats::default(),
            scrollback,
        };
        shell.clear_band()?;
        Ok(shell)
    }

    #[must_use]
    pub fn band_height(&self) -> u16 {
        self.band_height
    }

    /// The actual band rectangle; callers must not infer it from the cursor.
    pub fn band_area(&mut self) -> Rect {
        self.terminal.get_frame().area()
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn screen_rows(&mut self) -> u16 {
        self.terminal
            .size()
            .map_or(self.screen.height, |size| size.height)
    }

    /// Match `set_height`'s no-op without emitting a wrapper.
    pub fn needs_height_change(&mut self, desired: u16) -> bool {
        self.terminal.size().is_ok_and(|screen| {
            clamp_height(desired, screen.height) != clamp_height(self.band_height, screen.height)
        })
    }

    #[must_use]
    pub fn stats(&self) -> ShellStats {
        self.stats
    }

    /// Drawing cannot kill a run. The next operation retries geometry and
    /// repaint after failure.
    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) {
        if self
            .guarded(|shell| {
                shell.tolerated_draw(render);
                Ok(())
            })
            .is_err()
        {
            self.stats.tolerated_draw_errors += 1;
        }
    }

    /// Insert completed display rows, then repaint under the same wrapper.
    /// A successful return permits the caller to acknowledge the rows.
    pub fn flush(
        &mut self,
        rows: &[Line<'_>],
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.guarded(|shell| {
            shell.sync_size()?;
            shell.insert_history(rows)?;
            shell.stats.inserts += 1;
            shell.stats.inserted_rows += rows.len();
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    /// Grow into the blank buffer first, scroll only the overflow. Shrink
    /// keeps the top edge, leaving vacated rows below the band, not inside
    /// the transcript (ADR-0018's high-water amendment).
    pub fn set_height(
        &mut self,
        desired: u16,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        let screen = self.terminal.size()?;
        let new_height = clamp_height(desired, screen.height);
        let current = clamp_height(self.band_height, screen.height);
        if new_height == current {
            return Ok(());
        }
        self.guarded(|shell| {
            shell.sync_size()?;
            let old = shell.band_area();
            let target = clamp_height(desired, shell.screen.height);
            shell.clear_band()?;
            let top = old.y.min(shell.screen.height.saturating_sub(target));
            let displaced = old.y - top;
            if displaced > 0 {
                crate::history::scroll_history(
                    shell.terminal.backend_mut(),
                    old.y,
                    shell.screen.height,
                    displaced,
                    shell.scrollback,
                )?;
            }
            // No blank-history insertion is needed once the shell owns the
            // rectangle. Publish the new geometry before resampling: if a
            // resize raced the scroll, fit_screen preserves the displaced
            // history using this new boundary, not the old height's math.
            shell.band_height = target;
            shell.install_area(Rect::new(0, top, shell.screen.width, target))?;
            shell.sync_size()?;
            if target > old.height {
                shell.stats.grows += 1;
            } else {
                shell.stats.shrinks += 1;
            }
            shell.clear_band()?;
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    /// Debounced resize owns the destructive width-shrink replay and the
    /// grow refit. Ordinary draws and inserts may fit the band meanwhile,
    /// but never clear history or consume the replay markers. `replay_tail`
    /// supplies only displayed content from the caller's source, bounded to
    /// the visible window. Rows still visible but above the replay window
    /// would die with the clear, so on shrink they leave into scrollback
    /// first (ADR-0018, 2026-09-22).
    pub fn on_resize(
        &mut self,
        cols: u16,
        rows: u16,
        replay_tail: impl FnOnce(u16, u16) -> Vec<Line<'static>>,
        render: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        self.guarded(|shell| {
            let grew = rows > shell.screen.height || shell.grow_refit_pending;
            if shell.fit_screen(Size::new(cols, rows)).is_err() {
                shell.stats.tolerated_resize_errors += 1;
                return Ok(());
            }
            let shrunk = cols < shell.width || shell.replay_pending;
            if shrunk {
                // The band hugs the history, so the band's top edge is also
                // the count of visible history rows. The clear erases them
                // and the replay re-materializes only the tail that fits the
                // new window — push the whole window into scrollback first
                // or the rows in between are lost on every terminal.
                let visible = shell.band_area().y;
                if visible > 0 {
                    crate::history::scroll_history(
                        shell.terminal.backend_mut(),
                        visible,
                        rows,
                        visible,
                        shell.scrollback,
                    )?;
                }
                crate::history::clear_below(shell.terminal.backend_mut(), 0, rows)?;
                shell.install_area(Rect::new(0, 0, cols, clamp_height(shell.band_height, rows)))?;
                shell.clear_band()?;
                let tail = replay_tail(rows.saturating_sub(shell.band_height), cols);
                if !tail.is_empty() {
                    shell.insert_history(&tail)?;
                    shell.stats.inserts += 1;
                    shell.stats.inserted_rows += tail.len();
                    shell.stats.shrink_replays += 1;
                }
            } else if grew {
                // The interim fit re-glued the band; the settle refills the
                // revealed rows from source and erases any drag-window
                // residue, making the final world identical on every
                // terminal class. The refill re-materializes only source
                // rows, so the whole visible window leaves into scrollback
                // first — the shrink's save-then-clear rule, for the same
                // reason: rows above the band are not necessarily in the
                // source (pre-boot shell output; rows a restoring terminal
                // just pulled OUT of its scrollback — tmux and ConPTY both
                // consume), and bounding the push to the grow amount would
                // lose the rest on every terminal (ADR-0018, 2026-09-22
                // 4th). The window's overlap with the replayed tail is the
                // accepted bounded duplication.
                let visible = shell.band_area().y;
                if visible > 0 {
                    crate::history::scroll_history(
                        shell.terminal.backend_mut(),
                        visible,
                        rows,
                        visible,
                        shell.scrollback,
                    )?;
                }
                crate::history::clear_below(shell.terminal.backend_mut(), 0, rows)?;
                shell.install_area(Rect::new(0, 0, cols, clamp_height(shell.band_height, rows)))?;
                shell.clear_band()?;
                let tail = replay_tail(rows.saturating_sub(shell.band_height), cols);
                if !tail.is_empty() {
                    shell.insert_history(&tail)?;
                    shell.stats.inserts += 1;
                    shell.stats.inserted_rows += tail.len();
                }
            }
            shell.width = cols;
            shell.replay_pending = false;
            shell.grow_refit_pending = false;
            shell.tolerated_draw(render);
            Ok(())
        })
    }

    /// The public Fixed viewport constructor installs known geometry without
    /// querying, reserving rows, or implicitly clearing the user's history.
    fn install_area(&mut self, area: Rect) -> io::Result<()> {
        if self.band_area() != area {
            self.terminal = Terminal::with_options(
                self.terminal.backend().clone(),
                TerminalOptions {
                    viewport: Viewport::Fixed(area),
                },
            )?;
        }
        self.terminal.set_cursor_position(area.as_position())
    }

    fn clear_band(&mut self) -> io::Result<()> {
        let area = self.band_area();
        crate::history::clear_below(
            self.terminal.backend_mut(),
            area.y,
            self.screen.height.max(area.bottom()),
        )?;
        self.terminal.set_cursor_position(area.as_position())?;
        // Invalidate both buffers after raw writes, including default spaces.
        self.terminal.swap_buffers();
        self.terminal.swap_buffers();
        Ok(())
    }

    fn sync_size(&mut self) -> io::Result<()> {
        let screen = self.terminal.size()?;
        self.fit_screen(screen)
    }

    /// Fit only the band to physical dimensions. The terminal can resize
    /// during a raw write; this must never invoke ratatui's whole-screen
    /// clear afterwards. Source replay waits for the debounced event.
    fn fit_screen(&mut self, screen: Size) -> io::Result<()> {
        if screen == self.screen {
            return Ok(());
        }
        let old = self.band_area();
        let height = clamp_height(self.band_height, screen.height);
        let bottom_limit = screen.height.saturating_sub(height);
        let grow = screen.height.saturating_sub(self.screen.height);
        // On grow the band image moves differently per terminal class and no
        // query can tell the classes apart (CPR times out under resize
        // storms — exactly when it would be needed). Restoring terminals
        // (ConPTY/Windows Terminal, reflow xterm) shift every row down by
        // the grow amount — band image included — so the band follows its
        // content; clearing rows at the old top would erase shifted history.
        // Top-anchoring terminals (tmux, Zed, vt100) keep content put: a
        // bottom-glued band is re-glued to the new bottom edge (its vacated
        // rows are the shell's own band image plus blanks, so erasing them
        // loses nothing and leaves no ghost), and a content-hugging band
        // keeps hugging. The debounced `on_resize` replay refills the
        // revealed rows from source, so the settled world is identical on
        // both classes (ADR-0018, 2026-09-22 4th amendment).
        let restoring = self.scrollback == ScrollbackStrategy::FullScreen;
        let old_limit = self
            .screen
            .height
            .saturating_sub(clamp_height(self.band_height, self.screen.height));
        let glued = old.y >= old_limit;
        let content_top = if restoring || glued {
            old.y.saturating_add(grow)
        } else {
            old.y
        };
        let top = content_top.min(bottom_limit);
        // The clamped-band recovery can re-expand the band over rows above
        // its image: those are history — scroll the region out first (its
        // top rows depart into scrollback, exactly the space the recovery
        // needs). Shrink keeps the old rule; a re-glued band moving down
        // covers only its own vacated image and blanks.
        let history_bottom = if grow > 0 && restoring {
            content_top
        } else {
            old.y.min(screen.height)
        };
        let displaced = history_bottom.saturating_sub(top);
        if grow > 0 && !restoring && top > old.y {
            // The band moves down off its image: erase the vacated rows
            // before the displaced scroll can drag image rows up into the
            // transcript. Safe on this class — the rows are the shell's own
            // band image and the grow's blanks, never history.
            crate::history::clear_below(self.terminal.backend_mut(), old.y, screen.height)?;
        }
        if displaced > 0 {
            crate::history::scroll_history(
                self.terminal.backend_mut(),
                history_bottom,
                screen.height,
                displaced,
                self.scrollback,
            )?;
        }
        self.replay_pending |= screen.width < self.screen.width;
        self.grow_refit_pending |= grow > 0;
        self.install_area(Rect::new(0, top, screen.width, height))?;
        self.screen = screen;
        self.clear_band()
    }

    fn insert_history(&mut self, rows: &[Line<'_>]) -> io::Result<()> {
        let before = self.band_area();
        let after = crate::history::insert(
            self.terminal.backend_mut(),
            rows,
            before,
            self.screen,
            self.scrollback,
        )?;
        self.install_area(after)?;
        // The raw writer's flush can race a physical resize. Fit before
        // clearing: an out-of-screen CUP would clamp onto a history row.
        self.sync_size()?;
        self.clear_band()
    }

    fn tolerated_draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) {
        if self
            .sync_size()
            .and_then(|()| self.terminal.draw(render).map(|_| ()))
            .is_err()
        {
            self.stats.tolerated_draw_errors += 1;
        }
    }

    /// Always attempt the closing wrapper, even when a structural write
    /// failed. Both handles must target the same terminal stream.
    fn guarded(&mut self, op: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        execute!(&mut self.guard, BeginSynchronizedUpdate)?;
        let result = op(self);
        let end = execute!(&mut self.guard, EndSynchronizedUpdate);
        result.and(end)
    }
}

fn clamp_height(desired: u16, screen_rows: u16) -> u16 {
    desired.clamp(1, screen_rows.max(1))
}
