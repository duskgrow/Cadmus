//! The cursor tracker: a [`Backend`] adapter that answers
//! `get_cursor_position` from protocol state instead of CPR round-trips.
//!
//! Why this exists: every cursor-position query (`ESC[6n`) goes through
//! crossterm's process-global event reader, whose lock the input broker's
//! parked reader thread *holds while idle*. A query issued in that steady
//! state stalls for the full two-second lock timeout and then fails —
//! verified end to end on a real pty (2026-09-16): the terminal answered
//! immediately, yet the query returned "cursor position could not be read
//! within a normal duration" after 2.0 s. ratatui's inline viewport queries
//! the cursor inside `Terminal::with_options` (construction/recreation),
//! `Terminal::clear` (which the portable `insert_before` calls on its way
//! out — so an ordinary prompt flush queried) and `Terminal::resize`, so
//! the first submitted line was enough to kill the session.
//!
//! The tracker removes every post-boot query. It is seeded once with the
//! real position at construction — the session's one CPR, which must run
//! before the input broker exists (the app boundary in `app::run` owns that
//! order) — and then tracks the cursor through the shell's own ops:
//! `set_cursor_position` sets it, `append_lines` advances it with
//! scroll-at-bottom clamping, clears never move it. Exactness is only
//! needed at ratatui's query points, and each is preceded by an explicit
//! park (the grow/shrink protocol) or by a draw (which ends in
//! `set_cursor_position` via the frame cursor):
//!
//! - construction/recreation and `clear`'s snapshot-restore read the
//!   cursor; recreation is always preceded by the shell parking the cursor
//!   at the future band top;
//! - `resize` reads it after the terminal reflowed — the one approximate
//!   read (reflow moves content without telling us). An approximate
//!   re-anchor strictly beats the old behavior: a two-second stall followed
//!   by abandoning the resize.
//!
//! Ops whose cursor side effect is deliberately not modeled — cell `draw`s
//! (the real cursor lands past the last written cell) and `scroll_region_*`
//! (the trait leaves the aftermath undefined) — leave the tracked value
//! untouched: no query point observes it before the next park. Enabling
//! ratatui's `scrolling-regions` feature later adds two required trait
//! methods — delegate them and keep this rule.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// See the module docs for the contract. `Clone` because the shell's
/// recreation clones the backend handle — the clone carries the just-parked
/// position, which is exactly what the new `Terminal`'s anchor query needs.
#[derive(Clone, Debug)]
pub struct CursorTracker<B> {
    inner: B,
    cursor: Position,
}

impl<B: Backend> CursorTracker<B> {
    /// Seed the tracker with the terminal's real cursor position — the
    /// session's one CPR round-trip; see the module docs for the
    /// before-the-broker ordering contract. A failure here is the honest
    /// "this terminal cannot answer CPR" signal the `cadmus::tui`
    /// diagnostic's help text speaks of.
    pub fn new(mut inner: B) -> Result<Self, B::Error> {
        let cursor = inner.get_cursor_position()?;
        Ok(Self { inner, cursor })
    }
}

impl<B: Backend> Backend for CursorTracker<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        // Raw-mode `\n` is IND: same column, one row down, clamped at the
        // last row (the content scrolls instead of the cursor advancing).
        let last_row = self.inner.size()?.height.saturating_sub(1);
        self.cursor.y = self.cursor.y.saturating_add(n).min(last_row);
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.cursor = position;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        // ED sequences never move the cursor.
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal model whose real cursor moves the way a raw-mode
    /// terminal's would, so tests can assert tracked == real.
    #[derive(Clone)]
    struct FakeTerminal {
        real: Position,
        rows: u16,
    }

    impl Backend for FakeTerminal {
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, _content: I) -> Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            Ok(())
        }

        fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
            self.real.y = self.real.y.saturating_add(n).min(self.rows - 1);
            Ok(())
        }

        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
            Ok(self.real)
        }

        fn set_cursor_position<P: Into<Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Self::Error> {
            self.real = position.into();
            Ok(())
        }

        fn clear(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn clear_region(&mut self, _clear_type: ClearType) -> Result<(), Self::Error> {
            Ok(())
        }

        fn size(&self) -> Result<Size, Self::Error> {
            Ok(Size::new(80, self.rows))
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

    #[test]
    fn construction_seeds_the_real_position() {
        let terminal = FakeTerminal {
            real: Position::new(7, 19),
            rows: 24,
        };
        let mut tracker = CursorTracker::new(terminal).unwrap();
        assert_eq!(tracker.get_cursor_position().unwrap(), Position::new(7, 19));
    }

    #[test]
    fn set_position_tracks_the_real_cursor() {
        let terminal = FakeTerminal {
            real: Position::new(0, 0),
            rows: 24,
        };
        let mut tracker = CursorTracker::new(terminal).unwrap();
        tracker.set_cursor_position(Position::new(3, 5)).unwrap();
        assert_eq!(tracker.inner.real, Position::new(3, 5));
        assert_eq!(tracker.get_cursor_position().unwrap(), Position::new(3, 5));
    }

    #[test]
    fn appends_advance_and_clamp_at_the_last_row() {
        let terminal = FakeTerminal {
            real: Position::new(4, 20),
            rows: 24,
        };
        let mut tracker = CursorTracker::new(terminal).unwrap();
        tracker.append_lines(2).unwrap();
        assert_eq!(tracker.get_cursor_position().unwrap(), Position::new(4, 22));
        tracker.append_lines(10).unwrap();
        assert_eq!(tracker.get_cursor_position().unwrap(), Position::new(4, 23));
        assert_eq!(tracker.inner.real, tracker.cursor);
    }

    #[test]
    fn draws_and_clears_leave_the_tracked_position_untouched() {
        let terminal = FakeTerminal {
            real: Position::new(1, 2),
            rows: 24,
        };
        let mut tracker = CursorTracker::new(terminal).unwrap();
        let cell = Cell::default();
        let cells = [(0u16, 0u16, &cell)];
        tracker.draw(cells.into_iter()).unwrap();
        tracker.clear().unwrap();
        tracker.clear_region(ClearType::All).unwrap();
        assert_eq!(tracker.get_cursor_position().unwrap(), Position::new(1, 2));
    }
}
