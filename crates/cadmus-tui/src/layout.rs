//! The band's height function and widget layout rules (ADR-0018, both
//! 2026-09-14 amendments and both 2026-09-20 amendments): the desired band
//! height over content, capped at the screen (Codex's `desired_height`
//! precedent), and the per-frame re-split among the widgets inside the
//! band — the `receiving…` row on top, the approval section (the pending
//! request's dialog), the run-status row (the live task state and run
//! clock), the composer, status line at the bottom.
//!
//! The rules, pinned here per the amendments' delegation ("the layout rules
//! already scoped in amendment item 1 (composer cap, short-window corner)"):
//!
//! - The composer is capped at [`COMPOSER_MAX_ROWS`] and never takes more
//!   than half the screen (rounded up): past the cap it scrolls internally
//!   instead of growing at all (the composer cap; the short-window corner,
//!   where interaction still wins). Widget priority while the window is
//!   short: the pending dialog first (it blocks the run until answered),
//!   then the composer (it keeps its row), then the run-status row, then
//!   the floor status line, the `receiving…` row the first casualty.
//! - The approval section is capped at [`APPROVAL_MAX_ROWS`] (past it the
//!   render keeps the section's head — internal scrolling is the
//!   cumulative-diff slice's) and shares the short window by the same
//!   rules.
//! - The run-status row is one row while a run is active (or the status is
//!   Failed), none while idle — event-driven like every other height input.
//!   It yields to the dialog and the composer on short windows; only the
//!   floor status line and the `receiving…` row rank lower.
//! - The `receiving…` row is one row while a run is active with output
//!   pending (the emission queue or the unstable tail non-empty), none
//!   otherwise — the paced-emission model's liveness signal in place of
//!   the hidden unstable tail (the 2026-09-20 second amendment), never a
//!   content slice.
//! - While a run is active the band's desired height never shrinks (the run
//!   high-water hold, ADR-0018's 2026-09-20 amendment): the app holds the
//!   run's floor and [`BandLayout::held_at`] pads the slack above the
//!   band's content slices, so a settled block, a dismissed dialog or a
//!   cleared type-ahead composer leaves no vacated-row residue mid-stream;
//!   the one collapse lands at the run's outcome.
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

/// The run-status row's height while visible (a single line — the live
/// task state and the run clock; codex's status-row-above-the-composer
/// precedent). The app drives the 0/1 (run active or Failed vs. idle).
pub const RUN_STATUS_ROWS: u16 = 1;

/// The `receiving…` row's height while visible (a single line — the
/// liveness signal that output is pending). The app drives the 0/1 (run
/// active with the queue or the unstable tail non-empty).
pub const RECEIVING_ROWS: u16 = 1;

/// Content metrics at the current width, measured by the widgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayoutInput {
    /// Terminal rows.
    pub screen_rows: u16,
    /// The `receiving…` row: 1 while a run is active with output pending,
    /// 0 otherwise (the app's call — [`RECEIVING_ROWS`] when shown).
    pub receiving_rows: u16,
    /// The approval dialog's row count (`App::wrap_section` wrapping the
    /// head dialog's cached lines).
    pub approval_rows: u16,
    /// The run-status row: 1 while a run is active or the status is Failed,
    /// 0 while idle (the app's call — [`RUN_STATUS_ROWS`] when shown).
    pub run_status_rows: u16,
    /// The composer's desired row count ([`crate::composer::Composer::desired_rows`]).
    pub composer_rows: u16,
}

/// The split: the band's total height plus each widget's visible rows,
/// top to bottom: slack padding, receiving row, approval, run-status,
/// composer, status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandLayout {
    /// The desired band height (screen-capped) — the height function's
    /// output, fed to `InlineShell::set_height`.
    pub band_height: u16,
    /// Blank slack padding the band's TOP ([`BandLayout::held_at`]'s
    /// high-water output — the content slices anchor below it).
    pub slack_rows: u16,
    /// The `receiving…` row's rows (0 or 1, the input capped).
    pub receiving_rows: u16,
    /// Rows the approval section may show (head-clipped to its slice).
    pub approval_rows: u16,
    /// Rows the run-status row gets (0 or 1, the input capped).
    pub run_status_rows: u16,
    /// Rows the composer may show (it scrolls internally past this).
    pub composer_rows: u16,
    /// Rows the status line gets.
    pub status_rows: u16,
}

impl BandLayout {
    /// The run high-water hold (ADR-0018's 2026-09-20 amendment): the split
    /// with the band held at `floor` rows. The slack over the content's own
    /// want pads the band's top with blank rows, so every content slice
    /// keeps its place at the bottom. `floor` is a desired-height concept:
    /// the shell screen-clamps it like any other desired height, and a
    /// floor at or below the content's want changes nothing.
    #[must_use]
    pub fn held_at(mut self, floor: u16) -> Self {
        if floor > self.band_height {
            self.slack_rows = floor - self.band_height;
            self.band_height = floor;
        }
        self
    }
}

/// The height function and the re-split, one pure computation.
#[must_use]
pub fn layout(input: &LayoutInput) -> BandLayout {
    let &LayoutInput {
        screen_rows,
        receiving_rows,
        approval_rows,
        run_status_rows,
        composer_rows,
    } = input;
    if screen_rows == 0 {
        return BandLayout {
            band_height: 0,
            slack_rows: 0,
            receiving_rows: 0,
            approval_rows: 0,
            run_status_rows: 0,
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
    // The run-status and receiving rows are single lines by construction.
    let run_status_rows = run_status_rows.min(RUN_STATUS_ROWS);
    let receiving_rows = receiving_rows.min(RECEIVING_ROWS);
    let band_height =
        (status_rows + run_status_rows + composer_rows + approval_rows + receiving_rows)
            .min(screen_rows);
    // The re-split, in widget priority: the pending dialog first (a blocked
    // run cannot proceed without it), then the composer (the documented
    // corner keeps it a row), then the run-status row, then the floor
    // status line, and the `receiving…` row the first casualty. The sum
    // can never exceed the band this way, whatever the screen height.
    let approval_rows = approval_rows.min(band_height);
    let composer_rows = composer_rows.min(band_height - approval_rows);
    let run_status_rows = run_status_rows.min(band_height - approval_rows - composer_rows);
    let status_rows =
        status_rows.min(band_height - approval_rows - composer_rows - run_status_rows);
    let receiving_rows = receiving_rows
        .min(band_height - approval_rows - composer_rows - run_status_rows - status_rows);
    BandLayout {
        band_height,
        slack_rows: 0,
        receiving_rows,
        approval_rows,
        run_status_rows,
        composer_rows,
        status_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(
        screen_rows: u16,
        receiving_rows: u16,
        approval_rows: u16,
        run_status_rows: u16,
        composer_rows: u16,
    ) -> BandLayout {
        layout(&LayoutInput {
            screen_rows,
            receiving_rows,
            approval_rows,
            run_status_rows,
            composer_rows,
        })
    }

    #[test]
    fn an_idle_band_is_composer_plus_status() {
        let layout = split(24, 0, 0, 0, 1);
        assert_eq!(layout.band_height, 2);
        assert_eq!(layout.slack_rows, 0);
        assert_eq!(layout.receiving_rows, 0);
        assert_eq!(layout.approval_rows, 0);
        assert_eq!(layout.run_status_rows, 0);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
    }

    #[test]
    fn the_run_status_row_grows_and_shrinks_the_band_with_the_run() {
        // Run active: the row adds its one row to the idle band.
        let layout = split(24, 0, 0, 1, 1);
        assert_eq!(layout.run_status_rows, 1);
        assert_eq!(layout.band_height, 3);
        // Run over: back to composer + status.
        let layout = split(24, 0, 0, 0, 1);
        assert_eq!(layout.band_height, 2);
        assert_eq!(layout.run_status_rows, 0);
        // The input is a single row by construction.
        let layout = split(24, 0, 0, 5, 1);
        assert_eq!(layout.run_status_rows, 1);
        assert_eq!(layout.band_height, 3);
    }

    #[test]
    fn the_run_status_row_yields_to_the_dialog_and_the_composer() {
        // Three rows with everything pending: the floor line goes, the row
        // keeps its place under the dialog.
        let layout = split(3, 0, 1, 1, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.run_status_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 3);
        // Two rows: the dialog and the composer outrank the row.
        let layout = split(2, 0, 1, 1, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.run_status_rows, 0);
        assert_eq!(layout.band_height, 2);
    }

    #[test]
    fn the_run_status_row_outranks_the_floor_status_line() {
        let layout = split(2, 0, 0, 1, 1);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.run_status_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 2);
    }

    #[test]
    fn the_receiving_row_grows_the_band_while_output_is_pending() {
        let layout = split(24, 1, 0, 1, 1);
        assert_eq!(layout.receiving_rows, 1);
        assert_eq!(layout.band_height, 4);
        // The input is a single row by construction.
        let layout = split(24, 5, 0, 1, 1);
        assert_eq!(layout.receiving_rows, 1);
        assert_eq!(layout.band_height, 4);
    }

    #[test]
    fn the_receiving_row_is_the_first_casualty_on_a_short_window() {
        // Five rows fits everyone; four has the row yielding to the floor
        // line, and below that it is simply gone.
        let layout = split(5, 1, 1, 1, 1);
        assert_eq!(layout.receiving_rows, 1);
        assert_eq!(layout.band_height, 5);
        let layout = split(4, 1, 1, 1, 1);
        assert_eq!(layout.receiving_rows, 0);
        assert_eq!(layout.status_rows, 1);
        assert_eq!(layout.band_height, 4);
        let layout = split(2, 1, 0, 1, 1);
        assert_eq!(layout.receiving_rows, 0);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.run_status_rows, 1);
        assert_eq!(layout.band_height, 2);
    }

    #[test]
    fn the_band_grows_with_content_and_caps_at_the_screen() {
        let layout = split(24, 1, 3, 1, 1);
        assert_eq!(layout.band_height, 7);
        // On a short screen below the content's want, it caps at the screen.
        let layout = split(4, 1, 3, 1, 1);
        assert_eq!(layout.band_height, 4);
        assert_eq!(layout.receiving_rows, 0);
        assert_eq!(layout.status_rows, 0);
    }

    #[test]
    fn the_approval_section_grows_the_band_the_way_the_composer_does() {
        let layout = split(12, 0, 3, 0, 1);
        assert_eq!(layout.band_height, 5);
        assert_eq!(layout.approval_rows, 3);
        assert_eq!(layout.composer_rows, 1);
    }

    #[test]
    fn the_approval_section_caps_at_the_absolute_maximum() {
        let layout = split(24, 0, 20, 0, 1);
        assert_eq!(layout.approval_rows, APPROVAL_MAX_ROWS);
        assert_eq!(layout.band_height, 1 + APPROVAL_MAX_ROWS + 1);
    }

    #[test]
    fn the_status_line_yields_to_the_dialog_on_a_two_row_screen() {
        // The widgets' raw sum exceeds the band; the status line is the
        // casualty, the dialog and the composer keep their rows.
        let layout = split(2, 0, 1, 0, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.receiving_rows, 0);
        assert_eq!(layout.band_height, 2);
    }

    #[test]
    fn the_dialog_outranks_the_composer_on_a_one_row_screen() {
        // The last row of a one-row screen belongs to the pending dialog —
        // y/n answers it without the composer; steering waits a row.
        let layout = split(1, 0, 1, 0, 1);
        assert_eq!(layout.approval_rows, 1);
        assert_eq!(layout.composer_rows, 0);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 1);
    }

    #[test]
    fn the_composer_caps_at_the_absolute_maximum() {
        let layout = split(24, 0, 0, 0, 20);
        assert_eq!(layout.composer_rows, COMPOSER_MAX_ROWS);
        assert_eq!(layout.band_height, 1 + COMPOSER_MAX_ROWS);
    }

    #[test]
    fn the_composer_never_takes_more_than_half_the_screen() {
        let layout = split(10, 0, 0, 0, 12);
        assert_eq!(layout.composer_rows, 5);
        assert_eq!(layout.band_height, 6);
    }

    #[test]
    fn the_short_window_corner_drops_status_first() {
        let layout = split(2, 0, 0, 0, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 1);
        assert_eq!(layout.band_height, 2);
        let layout = split(1, 0, 0, 0, 3);
        assert_eq!(layout.composer_rows, 1);
        assert_eq!(layout.status_rows, 0);
        assert_eq!(layout.band_height, 1);
    }

    #[test]
    fn the_hold_pads_the_bands_top_with_the_slack() {
        let layout = split(24, 1, 0, 1, 1);
        assert_eq!(layout.band_height, 4);
        let held = layout.held_at(9);
        assert_eq!(held.band_height, 9);
        // The 5 held rows pad the band's top; every content slice keeps
        // its rows.
        assert_eq!(held.slack_rows, 5);
        assert_eq!(held.receiving_rows, 1);
        assert_eq!(held.approval_rows, 0);
        assert_eq!(held.run_status_rows, 1);
        assert_eq!(held.composer_rows, 1);
        assert_eq!(held.status_rows, 1);
    }

    #[test]
    fn a_floor_below_the_contents_want_holds_nothing() {
        let layout = split(24, 1, 0, 1, 1);
        assert_eq!(layout.held_at(2), layout);
        assert_eq!(layout.held_at(layout.band_height), layout);
    }

    #[test]
    fn a_zero_row_screen_lays_out_nothing() {
        assert_eq!(
            split(0, 1, 3, 1, 3),
            BandLayout {
                band_height: 0,
                slack_rows: 0,
                receiving_rows: 0,
                approval_rows: 0,
                run_status_rows: 0,
                composer_rows: 0,
                status_rows: 0,
            }
        );
    }
}
