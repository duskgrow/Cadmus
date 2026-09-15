//! The band's height function and widget layout rules (ADR-0018, both
//! 2026-09-14 amendments): the desired band height over content, capped at
//! the screen (Codex's `desired_height` precedent), and the per-frame
//! re-split among the widgets inside the band — stream tail on top, composer,
//! status line at the bottom.
//!
//! The rules, pinned here per the amendments' delegation ("the layout rules
//! already scoped in amendment item 1 (composer cap, short-window corner)"):
//!
//! - A growing composer **borrows rows from the stream area**, never pushes
//!   the band taller than the content wants — and past
//!   [`COMPOSER_MAX_ROWS`] the composer scrolls internally instead of
//!   growing at all (the composer cap).
//! - The composer never takes more than half the screen (rounded up): the
//!   stream keeps a share even on short windows — the short-window corner,
//!   where interaction still wins (the composer gets at least one row, and
//!   the status line is the first casualty below two rows).
//! - Height changes are event-driven (composer line crossings, held-block
//!   settle, resize), never per-frame: callers recompute on those events and
//!   [`crate::shell::InlineShell::set_height`] no-ops on equality.
//!
//! Pure functions; every input is injected (AGENTS.md).

/// The composer's absolute row cap; past it the composer scrolls internally
/// (amendment item 1). Twelve keeps a capped composer under half of a
/// default 24-row screen, where the half-screen rule then agrees.
pub const COMPOSER_MAX_ROWS: u16 = 12;

/// The status line is always one row (ADR-0011 floor: model, cwd+git,
/// context-usage %, session cost).
pub const STATUS_ROWS: u16 = 1;

/// Content metrics at the current width, measured by the widgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutInput {
    /// Terminal rows.
    pub screen_rows: u16,
    /// The stream tail's live (unflushed) row count ([`crate::stream::Stream::live_row_count`]).
    pub stream_rows: u16,
    /// The composer's desired row count ([`crate::composer::Composer::desired_rows`]).
    pub composer_rows: u16,
}

/// The split: the band's total height plus each widget's visible rows,
/// top to bottom: stream, composer, status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandLayout {
    /// The desired band height (screen-capped) — the height function's
    /// output, fed to `InlineShell::set_height`.
    pub band_height: u16,
    /// Rows the stream tail may show (bottom-anchored).
    pub stream_rows: u16,
    /// Rows the composer may show (it scrolls internally past this).
    pub composer_rows: u16,
    /// Rows the status line gets.
    pub status_rows: u16,
}

/// The height function and the re-split, one pure computation.
#[must_use]
pub fn layout(input: &LayoutInput) -> BandLayout {
    let &LayoutInput {
        screen_rows,
        stream_rows,
        composer_rows,
    } = input;
    if screen_rows == 0 {
        return BandLayout {
            band_height: 0,
            stream_rows: 0,
            composer_rows: 0,
            status_rows: 0,
        };
    }
    // The short-window corner: below two rows the status line goes; the
    // composer always keeps its row.
    let status_rows = if screen_rows >= 2 { STATUS_ROWS } else { 0 };
    // The composer cap, both absolute and half-screen (rounded up).
    let half_screen = screen_rows.div_ceil(2);
    let composer_cap = COMPOSER_MAX_ROWS.min(half_screen).max(1);
    let composer_rows = composer_rows.min(composer_cap);
    let band_height = (status_rows + composer_rows + stream_rows).min(screen_rows);
    let stream_visible = band_height.saturating_sub(status_rows + composer_rows);
    BandLayout {
        band_height,
        stream_rows: stream_visible,
        composer_rows,
        status_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(screen_rows: u16, stream_rows: u16, composer_rows: u16) -> BandLayout {
        layout(&LayoutInput {
            screen_rows,
            stream_rows,
            composer_rows,
        })
    }

    #[test]
    fn an_idle_band_is_composer_plus_status() {
        let layout = split(24, 0, 1);
        assert_eq!(layout.band_height, 2);
        assert_eq!(layout.stream_rows, 0);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
    }

    #[test]
    fn the_band_grows_with_content_and_caps_at_the_screen() {
        let layout = split(24, 30, 1);
        assert_eq!(layout.band_height, 24);
        assert_eq!(layout.stream_rows, 22);
    }

    #[test]
    fn a_growing_composer_borrows_rows_from_the_stream() {
        let layout = split(24, 20, 5);
        assert_eq!(layout.band_height, 24);
        assert_eq!(layout.composer_rows, 5);
        assert_eq!(layout.stream_rows, 18);
    }

    #[test]
    fn the_composer_caps_at_the_absolute_maximum() {
        let layout = split(24, 0, 20);
        assert_eq!(layout.composer_rows, COMPOSER_MAX_ROWS);
        assert_eq!(layout.band_height, 1 + COMPOSER_MAX_ROWS);
    }

    #[test]
    fn the_composer_never_takes_more_than_half_the_screen() {
        let layout = split(10, 5, 12);
        assert_eq!(layout.composer_rows, 5);
        assert_eq!(layout.stream_rows, 4);
        assert_eq!(layout.band_height, 10);
    }

    #[test]
    fn the_short_window_corner_drops_status_first() {
        let layout = split(2, 10, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
        assert_eq!(layout.stream_rows, 0);
        assert_eq!(layout.band_height, 2);
        let layout = split(1, 10, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 1);
    }

    #[test]
    fn a_zero_row_screen_lays_out_nothing() {
        assert_eq!(
            split(0, 10, 3),
            BandLayout {
                band_height: 0,
                stream_rows: 0,
                composer_rows: 0,
                status_rows: 0,
            }
        );
    }
}
