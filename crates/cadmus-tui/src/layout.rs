//! The band's height function and widget layout rules (ADR-0018, both
//! 2026-09-14 amendments): the desired band height over content, capped at
//! the screen (Codex's `desired_height` precedent), and the per-frame
//! re-split among the widgets inside the band — stream tail on top, the
//! approval section (the pending request's dialog) above the composer, the
//! composer, status line at the bottom.
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
//!   where interaction still wins. Widget priority while the window is
//!   short: the pending dialog first (it blocks the run until answered),
//!   then the composer (it keeps its row), the status line the first
//!   casualty below two rows.
//! - The approval section borrows rows from the stream exactly like the
//!   composer: capped at [`APPROVAL_MAX_ROWS`] (past it the render keeps the
//!   section's head — internal scrolling is the cumulative-diff slice's),
//!   never pushes the band taller than the content wants.
//! - Height changes are event-driven (composer line crossings, held-block
//!   settle, resize), never per-frame: callers recompute on those events and
//!   [`crate::shell::InlineShell::set_height`] no-ops on equality.
//!
//! Pure functions; every input is injected (AGENTS.md).

/// The composer's absolute row cap; past it the composer scrolls internally
/// (amendment item 1). Twelve keeps a capped composer under half of a
/// default 24-row screen, where the half-screen rule then agrees.
pub const COMPOSER_MAX_ROWS: u16 = 12;

/// The approval section's absolute row cap; past it the render keeps the
/// section's head and the clipped diff tail waits for the cumulative,
/// file-backed diff slice (mirrors the composer cap's role — the band never
/// grows without bound over an unbounded `write_file` content).
pub const APPROVAL_MAX_ROWS: u16 = 12;

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
    /// The approval dialog's row count (`App::wrap_section` wrapping the
    /// head dialog's cached lines).
    pub approval_rows: u16,
    /// The composer's desired row count ([`crate::composer::Composer::desired_rows`]).
    pub composer_rows: u16,
}

/// The split: the band's total height plus each widget's visible rows,
/// top to bottom: stream, approval, composer, status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandLayout {
    /// The desired band height (screen-capped) — the height function's
    /// output, fed to `InlineShell::set_height`.
    pub band_height: u16,
    /// Rows the stream tail may show (bottom-anchored).
    pub stream_rows: u16,
    /// Rows the approval section may show (head-clipped to its slice).
    pub approval_rows: u16,
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
        approval_rows,
        composer_rows,
    } = input;
    if screen_rows == 0 {
        return BandLayout {
            band_height: 0,
            stream_rows: 0,
            approval_rows: 0,
            composer_rows: 0,
            status_rows: 0,
        };
    }
    // The short-window corner: below two rows the status line goes.
    let status_rows = if screen_rows >= 2 { STATUS_ROWS } else { 0 };
    // The composer cap, both absolute and half-screen (rounded up).
    let half_screen = screen_rows.div_ceil(2);
    let composer_cap = COMPOSER_MAX_ROWS.min(half_screen).max(1);
    let composer_rows = composer_rows.min(composer_cap);
    // The approval section's cap mirrors the composer's.
    let approval_cap = APPROVAL_MAX_ROWS.min(half_screen).max(1);
    let approval_rows = approval_rows.min(approval_cap);
    let band_height = (status_rows + composer_rows + approval_rows + stream_rows).min(screen_rows);
    // The re-split, in widget priority: the pending dialog first (a blocked
    // run cannot proceed without it), then the composer (the documented
    // corner keeps it a row), the status line the first casualty. The sum
    // can never exceed the band this way, whatever the screen height.
    let approval_rows = approval_rows.min(band_height);
    let composer_rows = composer_rows.min(band_height - approval_rows);
    let status_rows = status_rows.min(band_height - approval_rows - composer_rows);
    let stream_visible = band_height - approval_rows - composer_rows - status_rows;
    BandLayout {
        band_height,
        stream_rows: stream_visible,
        approval_rows,
        composer_rows,
        status_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(
        screen_rows: u16,
        stream_rows: u16,
        approval_rows: u16,
        composer_rows: u16,
    ) -> BandLayout {
        layout(&LayoutInput {
            screen_rows,
            stream_rows,
            approval_rows,
            composer_rows,
        })
    }

    #[test]
    fn an_idle_band_is_composer_plus_status() {
        let layout = split(24, 0, 0, 1);
        assert_eq!(layout.band_height, 2);
        assert_eq!(layout.stream_rows, 0);
        assert_eq!(layout.approval_rows, 0);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
    }

    #[test]
    fn the_band_grows_with_content_and_caps_at_the_screen() {
        let layout = split(24, 30, 0, 1);
        assert_eq!(layout.band_height, 24);
        assert_eq!(layout.stream_rows, 22);
    }

    #[test]
    fn a_growing_composer_borrows_rows_from_the_stream() {
        let layout = split(24, 20, 0, 5);
        assert_eq!(layout.band_height, 24);
        assert_eq!(layout.composer_rows, 5);
        assert_eq!(layout.stream_rows, 18);
    }

    #[test]
    fn the_approval_section_grows_the_band_the_way_the_composer_does() {
        let layout = split(24, 20, 3, 1);
        assert_eq!(layout.band_height, 24);
        assert_eq!(layout.approval_rows, 3);
        assert_eq!(layout.composer_rows, 1);
        // The section borrows from the stream share, never past the screen.
        assert_eq!(layout.stream_rows, 19);
    }

    #[test]
    fn the_approval_section_caps_at_the_absolute_maximum() {
        let layout = split(24, 0, 20, 1);
        assert_eq!(layout.approval_rows, APPROVAL_MAX_ROWS);
        assert_eq!(layout.band_height, 1 + APPROVAL_MAX_ROWS + 1);
    }

    #[test]
    fn the_approval_section_shares_the_short_window() {
        let layout = split(10, 2, 4, 2);
        assert_eq!(layout.approval_rows, 4);
        assert_eq!(layout.composer_rows, 2);
        assert_eq!(layout.stream_rows, 2);
        assert_eq!(layout.band_height, 9);
    }

    #[test]
    fn the_status_line_yields_to_the_dialog_on_a_two_row_screen() {
        // The widgets' raw sum exceeds the band; the status line is the
        // casualty, the dialog and the composer keep their rows.
        let layout = split(2, 10, 1, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.stream_rows, 0);
        assert_eq!(layout.band_height, 2);
    }

    #[test]
    fn the_dialog_outranks_the_composer_on_a_one_row_screen() {
        // The last row of a one-row screen belongs to the pending dialog —
        // y/n answers it without the composer; steering waits a row.
        let layout = split(1, 10, 1, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 0);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 1);
    }

    #[test]
    fn the_composer_caps_at_the_absolute_maximum() {
        let layout = split(24, 0, 0, 20);
        assert_eq!(layout.composer_rows, COMPOSER_MAX_ROWS);
        assert_eq!(layout.band_height, 1 + COMPOSER_MAX_ROWS);
    }

    #[test]
    fn the_composer_never_takes_more_than_half_the_screen() {
        let layout = split(10, 5, 0, 12);
        assert_eq!(layout.composer_rows, 5);
        assert_eq!(layout.stream_rows, 4);
        assert_eq!(layout.band_height, 10);
    }

    #[test]
    fn the_short_window_corner_drops_status_first() {
        let layout = split(2, 10, 0, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
        assert_eq!(layout.stream_rows, 0);
        assert_eq!(layout.band_height, 2);
        let layout = split(1, 10, 0, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 1);
    }

    #[test]
    fn a_zero_row_screen_lays_out_nothing() {
        assert_eq!(
            split(0, 10, 3, 3),
            BandLayout {
                band_height: 0,
                stream_rows: 0,
                approval_rows: 0,
                composer_rows: 0,
                status_rows: 0,
            }
        );
    }
}
