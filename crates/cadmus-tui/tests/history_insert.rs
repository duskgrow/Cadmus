//! History insertion uses raw terminal writes, so assertions cover both the
//! byte stream and the emulated screen. `FullScreen` additionally proves the
//! complete scrollback + screen sequence, including blank rows. vt100 0.16
//! drops rows leaving partial scroll regions; `Standard` assertions deliberately
//! cover only visible contents and the emitted protocol.

mod common;

use std::cell::Cell;
use std::io::{self, Write};

use cadmus_tui::cursor::CursorTracker;
use cadmus_tui::shell::{InlineShell, ScrollbackStrategy};
use common::{BSU, ESU, GuardSink, VtBackend, World, hard_wrap};
use ratatui::Frame;
use ratatui::backend::Backend;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

type Shell<B = VtBackend> = InlineShell<B, GuardSink>;

fn band_rows(height: u16) -> Vec<String> {
    (0..height)
        .map(|row| {
            if row == 0 {
                "PROMPT"
            } else if row == height - 1 {
                "STATUS"
            } else {
                ""
            }
            .to_string()
        })
        .collect()
}

fn render_hidden_band(frame: &mut Frame<'_>) {
    let area = frame.area();
    let rows: Vec<_> = band_rows(area.height).into_iter().map(Line::from).collect();
    frame.render_widget(Paragraph::new(rows), area);
}

fn render_band(frame: &mut Frame<'_>) {
    render_hidden_band(frame);
    frame.set_cursor_position(Position::new(2, frame.area().bottom() - 1));
}

fn boot(world: &World, height: u16, strategy: ScrollbackStrategy) -> (Shell, GuardSink) {
    let guard = GuardSink::default();
    let mut shell = InlineShell::new(world.backend.clone(), guard.clone(), height, strategy)
        .expect("boot shell");
    shell.draw(render_band);
    guard.take();
    world.backend.take_raw_bytes();
    (shell, guard)
}

fn flush<B: Backend<Error = io::Error> + Clone + Write>(shell: &mut Shell<B>, rows: &[String]) {
    let lines: Vec<_> = rows.iter().cloned().map(Line::from).collect();
    shell.flush(&lines, render_band).expect("flush history");
}

fn numbered_rows(start: usize, count: usize) -> Vec<String> {
    (start..start + count)
        .map(|i| format!("row-{i:04}"))
        .collect()
}

fn assert_band<B: Backend<Error = io::Error> + Clone + Write>(world: &World, shell: &mut Shell<B>) {
    let area = shell.band_area();
    let visible = world.visible_rows();
    assert_eq!(
        visible[usize::from(area.y)..usize::from(area.bottom())],
        band_rows(area.height),
        "band at {area:?} in {visible:?}"
    );
    assert!(
        visible[usize::from(area.bottom())..]
            .iter()
            .all(String::is_empty),
        "unused rows below the band stay blank: {visible:?}"
    );
    assert_eq!(world.cursor(), (area.bottom() - 1, 2), "composer cursor");
}

fn assert_full_world(world: &World, shell: &mut Shell, history: &[String]) {
    let mut actual = world.scrollback_rows();
    actual.extend(world.visible_rows());
    let area = shell.band_area();
    let mut expected = history.to_vec();
    expected.extend(band_rows(area.height));
    // Only the unused screen below the band contributes trailing blanks.
    expected.extend((area.bottom()..shell.screen_rows()).map(|_| String::new()));
    assert_eq!(actual, expected, "full scrollback + screen sequence");
    assert_band(world, shell);
}

fn assert_visible_tail<B: Backend<Error = io::Error> + Clone + Write>(
    world: &World,
    shell: &mut Shell<B>,
    history: &[String],
) {
    let top = usize::from(shell.band_area().y);
    let visible = world.visible_rows();
    assert!(history.len() >= top, "history must fill the visible region");
    assert_eq!(
        visible[..top],
        history[history.len() - top..],
        "visible history tail"
    );
    assert_band(world, shell);
}

fn contains_bytes(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|part| part == needle)
}

fn has_csi_scroll_up(bytes: &[u8]) -> bool {
    bytes.windows(2).enumerate().any(|(index, pair)| {
        pair == b"\x1b["
            && bytes[index + 2..]
                .iter()
                .find(|byte| (0x40..=0x7e).contains(*byte))
                == Some(&b'S')
    })
}

#[test]
fn fullscreen_cjk_at_exact_width_preserves_every_grapheme() {
    for width in [8, 9, 16, 17] {
        let world = World::new();
        world.resize(8, width);
        let (mut shell, _) = boot(&world, 3, ScrollbackStrategy::FullScreen);
        let exact = format!(
            "{}{}",
            "界".repeat(usize::from(width / 2)),
            if width % 2 == 0 { "" } else { "!" }
        );
        let ending_wide = format!("{}举", "x".repeat(usize::from(width - 2)));
        assert_eq!(exact.width(), usize::from(width));
        assert_eq!(ending_wide.width(), usize::from(width));
        let mut expected = Vec::new();
        for turn in 0..12 {
            let rows = vec![exact.clone(), ending_wide.clone(), format!("tail-{turn}")];
            flush(&mut shell, &rows);
            expected.extend(rows);
            assert_full_world(&world, &mut shell, &expected);
        }
        assert!(world.scrollback_rows().len() > 8, "exercise paged history");
    }
}

#[test]
fn standard_cjk_at_exact_width_keeps_the_visible_tail() {
    for width in [8, 9, 16, 17] {
        let world = World::new();
        world.resize(8, width);
        let (mut shell, _) = boot(&world, 2, ScrollbackStrategy::Standard);
        let exact = format!("{}界", "a".repeat(usize::from(width - 2)));
        let rows: Vec<_> = (0..20)
            .map(|i| {
                if i % 2 == 0 {
                    exact.clone()
                } else {
                    "后续".into()
                }
            })
            .collect();
        flush(&mut shell, &rows);
        assert_eq!(shell.band_area().y, 6);
        assert_visible_tail(&world, &mut shell, &rows);
    }
}

#[test]
fn styled_cjk_and_combining_spans_keep_text_and_attributes() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 12);
        let (mut shell, _) = boot(&world, 2, strategy);
        let rows = [
            Line::from(vec![
                Span::styled(
                    "你好",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw("e\u{301}"),
                Span::styled("界", Style::default().fg(Color::Blue)),
                Span::raw("abcde"),
            ]),
            Line::from(vec![Span::raw("next "), Span::raw("界e\u{301}")]).style(
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::ITALIC),
            ),
        ];
        assert_eq!(rows[0].width(), 12, "style boundaries reach the right edge");
        shell.flush(&rows, render_band).expect("flush styled rows");
        let expected = vec!["你好e\u{301}界abcde".into(), "next 界e\u{301}".into()];
        assert_eq!(shell.band_area().y, 2);
        assert_visible_tail(&world, &mut shell, &expected);
        if strategy == ScrollbackStrategy::FullScreen {
            assert_full_world(&world, &mut shell, &expected);
        }

        assert_eq!(world.cell(0, 0).contents(), "你");
        assert_eq!(world.cell(0, 0).fgcolor(), vt100::Color::Idx(1));
        assert!(world.cell(0, 0).bold());
        assert!(world.cell(0, 1).is_wide_continuation());
        assert_eq!(world.cell(0, 2).contents(), "好");
        assert_eq!(world.cell(0, 4).contents(), "e\u{301}");
        assert_eq!(world.cell(0, 4).fgcolor(), vt100::Color::Default);
        assert!(!world.cell(0, 4).bold());
        assert_eq!(world.cell(0, 5).fgcolor(), vt100::Color::Idx(4));
        assert_eq!(world.cell(0, 11).contents(), "e");
        assert_eq!(world.cell(1, 0).fgcolor(), vt100::Color::Idx(2));
        assert!(world.cell(1, 0).italic());
        assert!(!world.cell(1, 0).bold());
        assert_eq!(world.cell(2, 0).fgcolor(), vt100::Color::Default);
        assert!(
            !world.cell(2, 0).italic(),
            "history styling cannot leak into the band"
        );

        let more = numbered_rows(0, 16);
        flush(&mut shell, &more);
        if strategy == ScrollbackStrategy::FullScreen {
            let mut expected = expected;
            expected.extend(more);
            assert_full_world(&world, &mut shell, &expected);
        } else {
            assert_visible_tail(&world, &mut shell, &more);
        }
    }
}

#[test]
fn span_style_overrides_line_style_without_removing_dim_with_bold() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 16);
        let (mut shell, _) = boot(&world, 2, strategy);
        let rows = [
            Line::from(vec![
                Span::raw("A"),
                Span::styled(
                    "界e\u{301}",
                    Style::default()
                        .fg(Color::Blue)
                        .remove_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::DIM),
                ),
                Span::raw("Z"),
            ])
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Line::from("plain"),
        ];
        shell
            .flush(&rows, render_band)
            .expect("flush style override");
        assert_eq!(shell.band_area().y, 2);
        assert_visible_tail(&world, &mut shell, &["A界e\u{301}Z".into(), "plain".into()]);
        for col in [0, 4] {
            let cell = world.cell(0, col);
            assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
            assert!(cell.bold(), "line style returns after the override");
            assert!(!cell.dim());
        }
        for col in [1, 3] {
            let cell = world.cell(0, col);
            assert_eq!(cell.fgcolor(), vt100::Color::Idx(4));
            assert!(!cell.bold());
            assert!(cell.dim(), "remove-bold must not clear dim intensity");
        }
        let plain = world.cell(1, 0);
        assert_eq!(plain.fgcolor(), vt100::Color::Default);
        assert!(!plain.bold() && !plain.dim());
    }
}

#[test]
fn fullscreen_preserves_blanks_across_batches_taller_than_the_screen() {
    let world = World::new();
    world.resize(7, 16);
    let (mut shell, _) = boot(&world, 3, ScrollbackStrategy::FullScreen);
    let mut expected = Vec::new();
    for batch in 0..3 {
        let mut rows = vec![String::new()];
        for row in 0..15 {
            rows.push(format!("b{batch}-{row:02}"));
            if row % 3 == 0 {
                rows.extend([String::new(), String::new()]);
            }
        }
        rows.push(String::new());
        assert!(rows.len() > 7);
        flush(&mut shell, &rows);
        expected.extend(rows);
        assert_full_world(&world, &mut shell, &expected);
    }
    assert!(world.scrollback_rows().len() > 14);
}

#[test]
fn standard_preserves_blank_rows_in_the_visible_tail_of_a_large_batch() {
    let world = World::new();
    world.resize(8, 16);
    let (mut shell, _) = boot(&world, 2, ScrollbackStrategy::Standard);
    let mut rows = numbered_rows(0, 20);
    rows.extend([
        "tail-a".into(),
        String::new(),
        String::new(),
        "tail-b".into(),
        String::new(),
        String::new(),
    ]);
    flush(&mut shell, &rows);
    assert_eq!(shell.band_area().y, 6);
    assert_visible_tail(&world, &mut shell, &rows);
}

#[test]
fn a_band_at_screen_top_moves_down_without_losing_the_first_rows() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 16);
        let (mut shell, _) = boot(&world, 3, strategy);
        assert_eq!(shell.band_area().y, 0);
        let mut rows = vec!["first".into()];
        flush(&mut shell, &rows);
        assert_eq!(shell.band_area().y, 1);
        assert_visible_tail(&world, &mut shell, &rows);
        flush(&mut shell, &[String::new(), "third".into()]);
        rows.extend([String::new(), "third".into()]);
        assert_eq!(shell.band_area().y, 3);
        assert_visible_tail(&world, &mut shell, &rows);
        if strategy == ScrollbackStrategy::FullScreen {
            assert_full_world(&world, &mut shell, &rows);
        }
    }
}

#[test]
fn full_height_and_one_row_bands_accept_more_than_a_screen_of_history() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for (screen_height, band_height) in [(8, 8), (8, 1), (1, 1)] {
            let world = World::new();
            world.resize(screen_height, 16);
            let (mut shell, _) = boot(&world, band_height, strategy);
            let mut expected = Vec::new();
            for batch in 0..3 {
                let rows = numbered_rows(batch * 13, 13);
                flush(&mut shell, &rows);
                expected.extend(rows);
                assert_eq!(shell.band_area().y, screen_height - band_height);
                assert_visible_tail(&world, &mut shell, &expected);
                if strategy == ScrollbackStrategy::FullScreen {
                    assert_full_world(&world, &mut shell, &expected);
                }
            }
        }
    }
}

#[test]
fn fullscreen_grow_shrink_and_flush_preserve_history_and_blank_buffers() {
    let world = World::new();
    world.resize(10, 16);
    let mut expected = numbered_rows(0, 8);
    world.print_lines(&expected);
    let (mut shell, _) = boot(&world, 3, ScrollbackStrategy::FullScreen);
    assert_full_world(&world, &mut shell, &expected);

    for (step, height) in [7, 2, 4, 10, 1, 6, 3].into_iter().enumerate() {
        let old = shell.band_area();
        shell
            .set_height(height, render_band)
            .expect("change height");
        if height < old.height {
            assert_eq!(shell.band_area().y, old.y, "shrink keeps the top edge");
        }
        assert_full_world(&world, &mut shell, &expected);
        let rows = vec![format!("step-{step}"), String::new(), "界e\u{301}".into()];
        flush(&mut shell, &rows);
        expected.extend(rows);
        assert_full_world(&world, &mut shell, &expected);
        shell.draw(render_band);
        assert_full_world(&world, &mut shell, &expected);
    }
}

#[test]
fn standard_grow_shrink_then_flush_restores_the_visible_tail() {
    let world = World::new();
    world.resize(10, 16);
    let (mut shell, _) = boot(&world, 3, ScrollbackStrategy::Standard);
    for (step, height) in [7, 2, 4, 10, 1, 6, 3].into_iter().enumerate() {
        let old = shell.band_area();
        shell
            .set_height(height, render_band)
            .expect("change height");
        if height < old.height {
            assert_eq!(shell.band_area().y, old.y, "shrink keeps the top edge");
        }
        assert_band(&world, &mut shell);
        let rows = numbered_rows(step * 13, 13);
        flush(&mut shell, &rows);
        assert_eq!(shell.band_area().y, 10 - height);
        assert_visible_tail(&world, &mut shell, &rows);
    }
}

/// The Standard-strategy leg of the 2026-09-22 save-the-window-first
/// contract (the `FullScreen` leg lives in `dynamic_height_spike.rs` and owns
/// the full-world assertion): the shrink must scroll the visible window
/// into scrollback before the clear. vt100 cannot track rows leaving a
/// partial region, so this leg judges the emitted protocol and the visible
/// rows, per this suite's header note.
#[test]
fn standard_width_shrink_scrolls_the_window_into_scrollback_first() {
    let world = World::new();
    let (mut shell, _guard) = boot(&world, 8, ScrollbackStrategy::Standard);
    // 96 cells per logical row: 2 display rows at 80 columns, 3 at 40.
    let source: Vec<String> = (0..12)
        .map(|i| format!("row-{i:02}·{}", "x".repeat(89)))
        .collect();
    flush(&mut shell, &source);
    assert_eq!(shell.band_area().y, 16);
    world.backend.take_raw_bytes();

    world.resize(24, 40);
    let replay_source = source.clone();
    shell
        .on_resize(
            40,
            24,
            move |max_rows, width| {
                let rows: Vec<String> = replay_source
                    .iter()
                    .flat_map(|row| hard_wrap(row, width))
                    .collect();
                let skip = rows.len().saturating_sub(usize::from(max_rows));
                rows.into_iter().skip(skip).map(Line::from).collect()
            },
            render_band,
        )
        .expect("debounced width shrink");

    // The save: DECSTBM over the window (its top IS the screen top, so the
    // departures land in native scrollback on a real terminal), the cursor
    // parked at the region's bottom, one CRLF per visible row.
    let raw = world.backend.take_raw_bytes();
    let mut needle = b"\x1b[1;16r\x1b[16;1H".to_vec();
    needle.extend(b"\r\n".repeat(16));
    assert!(
        contains_bytes(&raw, &needle),
        "the window scrolls into scrollback before the clear: {raw:?}"
    );
    // The replayed tail fills the window at the new width.
    let tail: Vec<String> = source
        .iter()
        .flat_map(|row| hard_wrap(row, 40))
        .skip(20)
        .collect();
    assert_visible_tail(&world, &mut shell, &tail);
}

fn stale_width_shrink_replay<B: Backend<Error = io::Error> + Clone + Write>(
    world: &World,
    shell: &mut Shell<B>,
    logical: &str,
    wrapped: &[&str],
) {
    let old_width = shell.width();
    world.resize(10, 12);
    let pending = vec![logical.to_string(), String::new(), "last界".into()];
    flush(shell, &pending);
    let mut replay: Vec<String> = wrapped.iter().map(|row| (*row).to_string()).collect();
    replay.extend([String::new(), "last界".into()]);
    assert_eq!(
        shell.width(),
        old_width,
        "the debounce still owns the width change"
    );
    assert_eq!(
        shell.band_area().width,
        12,
        "flush uses the actual terminal width"
    );
    let visible = world.visible_rows();
    let top = usize::from(shell.band_area().y);
    assert_eq!(
        &visible[top - replay.len()..top],
        replay,
        "stale rows re-wrap before replay"
    );
    assert_band(world, shell);
    let replay_calls = Cell::new(0);
    let replays_before = shell.stats().shrink_replays;
    let shell_height_for_replay = shell.band_height();
    shell
        .on_resize(
            12,
            10,
            |max_rows, width| {
                replay_calls.set(replay_calls.get() + 1);
                assert_eq!(max_rows, 10 - shell_height_for_replay);
                assert_eq!(width, 12);
                replay.iter().cloned().map(Line::from).collect()
            },
            render_band,
        )
        .expect("debounced width shrink");
    assert_eq!(replay_calls.get(), 1);
    assert_eq!(shell.width(), 12);
    assert_eq!(shell.stats().shrink_replays, replays_before + 1);
    // Pre-existing visible shell rows leave into scrollback on a width
    // shrink (vt100 drops the partial-region departures); this caller owns
    // only the replay tail and on screen it must appear exactly once.
    let visible = world.visible_rows();
    let top = usize::from(shell.band_area().y);
    assert_eq!(
        visible[..top],
        replay,
        "replay replaces the visible tail without duplication"
    );
    assert_visible_tail(world, shell, &replay);
    shell.draw(render_band);
    assert_visible_tail(world, shell, &replay);
    flush(shell, &["after".into()]);
    replay.push("after".into());
    assert_visible_tail(world, shell, &replay);
}

fn short_tail_after_stale_width_shrink(strategy: ScrollbackStrategy) {
    for initial_history in [0, 14] {
        let world = World::new();
        world.resize(10, 48);
        world.print_lines(&numbered_rows(0, initial_history));
        let (mut shell, _) = boot(&world, 4, strategy);
        stale_width_shrink_replay(
            &world,
            &mut shell,
            "ab界cd界ef界gh界",
            &["ab界cd界ef界", "gh界"],
        );
    }
}

#[test]
fn standard_stale_width_shrink_replays_a_short_tail_once() {
    short_tail_after_stale_width_shrink(ScrollbackStrategy::Standard);
}

#[test]
fn fullscreen_stale_width_shrink_replays_a_short_tail_once() {
    short_tail_after_stale_width_shrink(ScrollbackStrategy::FullScreen);
}

#[test]
fn stale_width_shrink_replays_a_tail_filling_the_history_region() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for initial_history in [0, 14] {
            let world = World::new();
            world.resize(10, 48);
            world.print_lines(&numbered_rows(0, initial_history));
            let (mut shell, _) = boot(&world, 4, strategy);
            stale_width_shrink_replay(
                &world,
                &mut shell,
                "abcdefghijklmnopqrstuvwxABCDEFGHIJKLMNOPQRSTUVWX",
                &[
                    "abcdefghijkl",
                    "mnopqrstuvwx",
                    "ABCDEFGHIJKL",
                    "MNOPQRSTUVWX",
                ],
            );
        }
    }
}

#[test]
fn hidden_cursor_width_growth_preserves_the_last_flushed_row() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(10, 24);
        let tracker = CursorTracker::new(world.backend.clone()).expect("seed cursor tracker");
        let guard = GuardSink::default();
        let mut shell = InlineShell::new(tracker, guard.clone(), 2, strategy).expect("boot shell");
        shell.draw(render_hidden_band);
        world.fail_queries(true);
        // No renderer parks the cursor: geometry must not depend on a
        // frame cursor that differs from the tracker's last explicit park.
        for row in ["a", "b", "c"] {
            shell
                .flush(&[Line::from(row)], render_hidden_band)
                .expect("flush row");
        }
        let before = world.visible_rows();
        assert_eq!(&before[..3], ["a", "b", "c"]);
        guard.take();
        world.resize(10, 30);
        shell
            .on_resize(
                30,
                10,
                |_, _| panic!("width growth cannot replay"),
                render_hidden_band,
            )
            .expect("widen hidden-cursor band");
        assert_eq!(world.visible_rows(), before, "width growth erased history");
        assert_eq!(shell.band_area(), Rect::new(0, 3, 30, 2));
        assert_eq!(world.cursor_queries(), 1);
        assert_eq!(guard.take(), [BSU, ESU].concat());
        shell.draw(render_hidden_band);
        assert_eq!(
            world.visible_rows(),
            before,
            "unchanged hidden-cursor redraw"
        );
    }
}

#[test]
fn physical_width_shrink_during_raw_flush_preserves_the_inserted_mark() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(10, 24);
        let mut rows = numbered_rows(0, 6);
        world.print_lines(&rows);
        let (mut shell, guard) = boot(&world, 4, strategy);
        // A bottom-anchored insert does not move the band: the old Inline
        // path skipped recreation and its subsequent autoresize erased MARK.
        assert_eq!(shell.band_area().y, 6);
        world.backend.resize_on_next_raw_flush(10, 12);
        flush(&mut shell, &["MARK".into()]);
        rows.push("MARK".into());
        assert_eq!(
            world.backend.size().expect("physical size").width,
            12,
            "resize hook fired"
        );
        assert_eq!(shell.band_area().y, 6);
        assert_visible_tail(&world, &mut shell, &rows);
        assert_eq!(guard.take(), [BSU, ESU].concat());
        shell.draw(render_band);
        assert_visible_tail(&world, &mut shell, &rows);
    }
}

#[test]
fn physical_resize_during_band_growth_reconciles_the_new_rectangle() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for screen_rows in [6, 8, 12] {
            let world = World::new();
            world.resize(10, 24);
            let rows = numbered_rows(0, 6);
            world.print_lines(&rows);
            let (mut shell, guard) = boot(&world, 4, strategy);
            world.backend.resize_on_next_raw_flush(screen_rows, 24);
            shell
                .set_height(8, render_band)
                .expect("grow while resizing");
            let area = shell.band_area();
            assert_eq!(area.height, screen_rows.min(8));
            assert!(area.bottom() <= screen_rows);
            assert_band(&world, &mut shell);
            if strategy == ScrollbackStrategy::FullScreen {
                if screen_rows == 12 {
                    // The grow leg: the band follows its content. vt100
                    // top-anchors on grow, so the move leaves a blank gap
                    // between the history and the band — the accepted
                    // residue; a restoring terminal shifts the band image
                    // down with the content and the band lands on it
                    // gap-free.
                    let mut expected = rows.clone();
                    expected.extend([String::new(), String::new()]);
                    expected.extend(band_rows(area.height));
                    let mut actual = world.scrollback_rows();
                    actual.extend(world.visible_rows());
                    assert_eq!(actual, expected, "grow-leg world with the follow gap");
                } else {
                    assert_full_world(&world, &mut shell, &rows);
                }
            }
            assert_eq!(guard.take(), [BSU, ESU].concat());
            // The grow leg owes the debounced refill (the raced fit set the
            // marker); the shrink legs fit clean and must not replay.
            let replay = rows.clone();
            shell
                .on_resize(
                    24,
                    screen_rows,
                    move |max_rows, _width| {
                        assert_eq!(screen_rows, 12, "only the grow leg replays");
                        replay[replay.len().saturating_sub(usize::from(max_rows))..]
                            .iter()
                            .cloned()
                            .map(Line::from)
                            .collect()
                    },
                    render_band,
                )
                .expect("notify settled resize");
            assert_band(&world, &mut shell);
            let inserted = numbered_rows(100, 16);
            flush(&mut shell, &inserted);
            assert_visible_tail(&world, &mut shell, &inserted);
        }
    }
}

#[test]
fn physical_height_grow_during_raw_flush_reglues_and_refills() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(10, 24);
        let rows = numbered_rows(0, 6);
        world.print_lines(&rows);
        let (mut shell, guard) = boot(&world, 4, strategy);
        // The grow (10 → 16) races the flush: it lands mid-insert, before
        // the shell's bookkeeping, and the mid-op `sync_size` must re-glue
        // the bottom-glued band without erasing or misplacing rows — the
        // restoring class follows its shifted image, the top-anchoring
        // class erases its own vacated image first (ADR-0018, 2026-09-22
        // 4th amendment; the insert's own clear already removed that image
        // here, so both classes land on the same geometry).
        world.backend.resize_on_next_raw_flush(16, 24);
        flush(&mut shell, &["MARK".into()]);
        let area = shell.band_area();
        assert_eq!(area, Rect::new(0, 12, 24, 4), "band re-glued to the bottom");
        assert!(area.bottom() <= 16, "band on-screen: {area:?}");
        assert_band(&world, &mut shell);
        assert_eq!(guard.take(), [BSU, ESU].concat());
        let world_rows = world.nonblank_rows();
        // vt100 drops the insert's own row departing the partial region
        // (the suite header's standing artifact), so Standard asserts from
        // the second row; the grow itself scrolls nothing here.
        let check_from = usize::from(strategy == ScrollbackStrategy::Standard);
        for row in rows[check_from..].iter().chain(["MARK".to_string()].iter()) {
            assert!(
                world_rows.iter().any(|actual| actual.contains(row)),
                "lost {row}: {world_rows:?}"
            );
        }
        // The settled notification owes the grow's refill: the transcript
        // is shorter than the new window, so every row re-materializes and
        // the band hugs the content — the drag-window geometry heals away.
        let mut source = rows[check_from..].to_vec();
        source.push("MARK".to_string());
        let replay = source.clone();
        shell
            .on_resize(
                24,
                16,
                move |max_rows, _width| {
                    replay
                        .iter()
                        .skip(replay.len().saturating_sub(usize::from(max_rows)))
                        .cloned()
                        .map(Line::from)
                        .collect()
                },
                render_band,
            )
            .expect("notify settled resize");
        assert_eq!(
            usize::from(shell.band_area().y),
            source.len(),
            "band hugs the refilled content"
        );
        let visible = world.visible_rows();
        assert_eq!(visible[..source.len()], source[..], "the refilled world");
        assert!(
            visible[shell.band_area().bottom() as usize..]
                .iter()
                .all(String::is_empty),
            "nothing below the hugging band"
        );
        assert_band(&world, &mut shell);
    }
}

#[test]
fn physical_height_shrink_during_raw_flush_preserves_displaced_history() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for screen_rows in [8, 6] {
            let world = World::new();
            world.resize(10, 24);
            let mut rows = numbered_rows(0, 6);
            world.print_lines(&rows);
            let (mut shell, guard) = boot(&world, 4, strategy);
            world.backend.resize_on_next_raw_flush(screen_rows, 24);
            flush(&mut shell, &["MARK".into()]);
            rows.push("MARK".into());
            assert_visible_tail(&world, &mut shell, &rows);
            if strategy == ScrollbackStrategy::FullScreen {
                assert_full_world(&world, &mut shell, &rows);
            }
            assert_eq!(guard.take(), [BSU, ESU].concat());
            shell
                .on_resize(
                    24,
                    screen_rows,
                    |_, _| panic!("no replay needed for height"),
                    render_band,
                )
                .expect("settled height");
            assert_visible_tail(&world, &mut shell, &rows);
        }
    }
}

#[test]
fn height_change_cannot_consume_a_pending_width_shrink_replay() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(10, 24);
        let (mut shell, guard) = boot(&world, 4, strategy);
        world.resize(10, 12);
        let rows = vec!["first界".into(), "last界".into()];
        flush(&mut shell, &rows);
        shell
            .set_height(2, render_band)
            .expect("shrink band during debounce");
        guard.take();
        let replayed = Cell::new(false);
        shell
            .on_resize(
                12,
                10,
                |max_rows, width| {
                    replayed.set(true);
                    assert_eq!((max_rows, width), (8, 12));
                    rows.iter().cloned().map(Line::from).collect()
                },
                render_band,
            )
            .expect("debounced width shrink");
        assert!(
            replayed.get(),
            "set_height consumed the pending width change"
        );
        assert_eq!(shell.band_area(), Rect::new(0, 2, 12, 2));
        assert_visible_tail(&world, &mut shell, &rows);
        assert_eq!(guard.take(), [BSU, ESU].concat());
    }
}

#[test]
fn one_column_stale_rows_escape_wide_glyphs_without_changing_the_source() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(16, 24);
        let mut shell = InlineShell::new(world.backend.clone(), GuardSink::default(), 1, strategy)
            .expect("boot shell");
        shell.draw(render_hidden_band);
        let source = Line::from("A界B");
        let original = source.clone();
        world.resize(16, 1);
        shell
            .flush(std::slice::from_ref(&source), render_hidden_band)
            .expect("flush stale wide glyph");
        assert_eq!(
            source, original,
            "escaping is display-only; source stays intact"
        );
        // A two-cell glyph cannot fit at all. Its ASCII spelling occupies
        // eight physical rows at one column, not a silently discarded glyph.
        let mut expected: Vec<String> = r"A\u{754c}B".chars().map(|ch| ch.to_string()).collect();
        assert_eq!(shell.band_area(), Rect::new(0, 10, 1, 1));
        expected.push("P".into());
        expected.resize(16, String::new());
        assert_eq!(world.visible_rows(), expected);
        shell
            .flush(&[Line::from("Z")], render_hidden_band)
            .expect("flush after escaped rows");
        assert_eq!(
            &world.visible_rows()[10..12],
            ["Z", "P"],
            "escape rows counted for later inserts"
        );
        assert_eq!(source, original);
    }
}

fn flush_on_a_short_screen<B: Backend<Error = io::Error> + Clone + Write>(
    world: &World,
    shell: &mut Shell<B>,
    strategy: ScrollbackStrategy,
    notify_before_flush: bool,
) {
    let requested = shell.band_height();
    let width = shell.width();
    world.resize(4, width);
    if notify_before_flush {
        shell
            .on_resize(
                width,
                4,
                |_, _| panic!("height-only shrink cannot replay"),
                render_band,
            )
            .expect("height-only shrink");
    }
    let rows = numbered_rows(100, 9);
    flush(shell, &rows);
    assert_eq!(
        shell.band_height(),
        requested,
        "flush preserves the requested height"
    );
    assert_eq!(shell.band_area(), Rect::new(0, 0, width, 4));
    assert_band(world, shell);
    // Both strategies fall back to full-screen writes when the band fills
    // the terminal, so these newly flushed rows are observable in vt100.
    assert!(
        world.scrollback_rows().ends_with(&rows),
        "short-screen flush lost rows"
    );
    shell
        .on_resize(
            width,
            4,
            |_, _| panic!("height-only shrink cannot replay"),
            render_band,
        )
        .expect("same-height notification");
    assert_eq!(shell.band_height(), requested);
    assert_band(world, shell);

    world.resize(10, width);
    // The regrow owes the debounced refill: the newest rows that fit the
    // taller window re-materialize from source above the re-expanded band.
    let replay = rows.clone();
    shell
        .on_resize(
            width,
            10,
            move |max_rows, _width| {
                replay[replay.len().saturating_sub(usize::from(max_rows))..]
                    .iter()
                    .cloned()
                    .map(Line::from)
                    .collect()
            },
            render_band,
        )
        .expect("restore screen height");
    assert_eq!(shell.band_height(), requested);
    assert_eq!(
        shell.band_area().height,
        requested,
        "the requested band re-expands"
    );
    assert_band(world, shell);
    let rows = numbered_rows(200, 12);
    flush(shell, &rows);
    assert_eq!(shell.band_area().y, 10 - requested);
    assert_visible_tail(world, shell, &rows);
    if strategy == ScrollbackStrategy::FullScreen {
        let mut all = world.scrollback_rows();
        all.extend(world.visible_rows());
        let mut suffix = rows;
        suffix.extend(band_rows(requested));
        assert!(
            all.ends_with(&suffix),
            "post-growth flush preserves the complete batch"
        );
    }
}

#[test]
fn flush_after_height_only_shrink_preserves_the_requested_band_height() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for notify_before_flush in [false, true] {
            let world = World::new();
            world.resize(10, 24);
            world.print_lines(&numbered_rows(0, 12));
            let (mut shell, _) = boot(&world, 7, strategy);
            flush_on_a_short_screen(&world, &mut shell, strategy, notify_before_flush);
        }
    }
}

#[test]
fn tracked_flush_grow_and_resize_never_query_the_backend_after_boot() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for notify_before_flush in [false, true] {
            let world = World::new();
            world.resize(10, 48);
            world.print_lines(&numbered_rows(0, 12));
            let tracker = CursorTracker::new(world.backend.clone()).expect("seed cursor tracker");
            let mut shell = InlineShell::new(tracker, GuardSink::default(), 3, strategy)
                .expect("boot tracked shell");
            shell.draw(render_band);
            assert_eq!(world.cursor_queries(), 1, "only the initial seed uses CPR");
            world.fail_queries(true);
            flush(&mut shell, &numbered_rows(20, 12));
            assert_band(&world, &mut shell);
            shell.set_height(4, render_band).expect("grow tracked band");
            assert_band(&world, &mut shell);
            stale_width_shrink_replay(
                &world,
                &mut shell,
                "abcdefghijklmnopqrstuvwxABCDEFGHIJKLMNOPQRSTUVWX",
                &[
                    "abcdefghijkl",
                    "mnopqrstuvwx",
                    "ABCDEFGHIJKL",
                    "MNOPQRSTUVWX",
                ],
            );
            shell
                .set_height(7, render_band)
                .expect("grow before the short screen");
            flush_on_a_short_screen(&world, &mut shell, strategy, notify_before_flush);
            assert_eq!(
                world.cursor_queries(),
                1,
                "no post-boot CPR, even if errors are tolerated"
            );
            assert_eq!(shell.stats().tolerated_draw_errors, 0);
            assert_eq!(shell.stats().tolerated_resize_errors, 0);
        }
    }
}

#[test]
fn standard_long_session_keeps_the_visible_tail_and_band_exact() {
    let world = World::new();
    world.resize(8, 16);
    let (mut shell, guard) = boot(&world, 3, ScrollbackStrategy::Standard);
    let mut history = Vec::new();
    // Exceeds even the rig's scrollback capacity, but the oracle only uses
    // visible rows: departed partial-region rows are unavailable in vt100.
    for batch in 0..240 {
        let rows = vec![
            format!("turn-{batch:03}界"),
            String::new(),
            "tail e\u{301}".into(),
        ];
        flush(&mut shell, &rows);
        history.extend(rows);
        assert_visible_tail(&world, &mut shell, &history);
        assert_eq!(guard.take(), [BSU, ESU].concat());
        shell.draw(render_band);
        assert_visible_tail(&world, &mut shell, &history);
        assert_eq!(guard.take(), [BSU, ESU].concat());
    }
    assert_eq!(shell.band_area().y, 5);
}

#[test]
fn raw_history_uses_crlf_and_standard_restores_the_scroll_margins() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 16);
        world.print_lines(&numbered_rows(0, 6));
        let (mut shell, guard) = boot(&world, 3, strategy);
        assert_eq!(shell.band_area().y, 5);
        let rows = numbered_rows(6, 14);
        flush(&mut shell, &rows);
        let bytes = world.backend.take_raw_bytes();
        assert!(contains_bytes(&bytes, b"\r\n"), "raw CRLF advances history");
        assert!(
            !has_csi_scroll_up(&bytes),
            "CSI S discards rows on some terminals"
        );
        if strategy == ScrollbackStrategy::Standard {
            let region = b"\x1b[1;5r";
            let set = bytes
                .windows(region.len())
                .position(|part| part == region)
                .expect("partial history scroll region");
            let reset = bytes
                .windows(3)
                .rposition(|part| part == b"\x1b[r")
                .expect("DECSTBM reset");
            assert!(reset > set, "reset follows the partial-region write");
        }
        assert_eq!(guard.take(), [BSU, ESU].concat());
        assert_visible_tail(&world, &mut shell, &rows);

        // A normal newline at the physical bottom must scroll the entire
        // screen after the insertion has finished, not a leftover region.
        let visible = world.visible_rows();
        let mut backend = world.backend.clone();
        backend
            .write_all(b"\x1b[8;1H\r\n")
            .expect("ordinary bottom newline");
        let mut expected = visible[1..].to_vec();
        expected.push(String::new());
        assert_eq!(world.visible_rows(), expected, "full margins restored");
    }
}

#[test]
fn untrusted_line_controls_match_the_filtered_raw_output() {
    let cases = [
        ("\x1b[2Jhello", "[2Jhello"),
        ("left\rright\nnext\tend", "leftrightnextend"),
        ("\x1b]52;c;Y2xpcA==\x07", "]52;c;Y2xpcA=="),
        ("a\0b\x08c\x7fd\u{009b}e\u{009d}f\u{009c}g", "abcdefg"),
    ];
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        for (untrusted, filtered) in cases {
            let unsafe_world = World::new();
            let safe_world = World::new();
            unsafe_world.resize(8, 40);
            safe_world.resize(8, 40);
            let (mut unsafe_shell, _) = boot(&unsafe_world, 2, strategy);
            let (mut safe_shell, _) = boot(&safe_world, 2, strategy);
            let style = Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD);
            let unsafe_line =
                Line::from(vec![Span::raw("prefix "), Span::styled(untrusted, style)]);
            let safe_line = Line::from(vec![Span::raw("prefix "), Span::styled(filtered, style)]);
            let original = unsafe_line.clone();
            unsafe_shell
                .flush(std::slice::from_ref(&unsafe_line), render_band)
                .expect("flush untrusted line");
            safe_shell
                .flush(&[safe_line], render_band)
                .expect("flush filtered line");
            assert_eq!(unsafe_line, original, "filtering does not mutate source");
            assert_eq!(
                unsafe_world.backend.take_raw_bytes(),
                safe_world.backend.take_raw_bytes(),
                "untrusted controls must never reach the terminal: {untrusted:?}"
            );
            assert_visible_tail(
                &unsafe_world,
                &mut unsafe_shell,
                &[format!("prefix {filtered}")],
            );
        }
    }
}

#[test]
fn unchanged_band_redraw_restores_blank_cells_after_history_insertion() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 16);
        let (mut shell, _) = boot(&world, 3, strategy);
        let mut history = numbered_rows(0, 12);
        flush(&mut shell, &history);
        assert_eq!(shell.band_area().y, 5);
        let before_area = shell.band_area();
        let rows = vec!["X".repeat(16); 2];
        flush(&mut shell, &rows);
        history.extend(rows);
        assert_eq!(shell.band_area(), before_area, "the viewport did not move");
        assert_visible_tail(&world, &mut shell, &history);
        let before = world.visible_rows();
        shell.draw(render_band);
        assert_eq!(
            world.visible_rows(),
            before,
            "unchanged frame has no stale cells"
        );
        let area = shell.band_area();
        for col in 0..area.width {
            assert!(world.cell(area.y + 1, col).contents().trim().is_empty());
        }
        for col in 6..area.width {
            assert!(world.cell(area.y, col).contents().trim().is_empty());
            assert!(
                world
                    .cell(area.bottom() - 1, col)
                    .contents()
                    .trim()
                    .is_empty()
            );
        }
        if strategy == ScrollbackStrategy::FullScreen {
            assert_full_world(&world, &mut shell, &history);
        }
    }
}

#[test]
fn raw_write_failure_propagates_closes_update_and_attempts_margin_reset() {
    for strategy in [ScrollbackStrategy::Standard, ScrollbackStrategy::FullScreen] {
        let world = World::new();
        world.resize(8, 16);
        world.print_lines(&numbered_rows(0, 6));
        let (mut shell, guard) = boot(&world, 3, strategy);
        world.backend.fail_next_raw_write_containing(b'F');
        let rendered = Cell::new(false);
        let error = shell
            .flush(&[Line::from("FAIL")], |_| rendered.set(true))
            .expect_err("raw history writes are structural failures");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(error.to_string(), "injected raw write failure");
        assert!(
            !rendered.get(),
            "failed insertion does not report a completed draw"
        );
        assert_eq!(
            guard.take(),
            [BSU, ESU].concat(),
            "failed op closes its update"
        );
        let bytes = world.backend.take_raw_bytes();
        let failed = bytes
            .iter()
            .position(|byte| *byte == b'F')
            .expect("payload write was attempted");
        if strategy == ScrollbackStrategy::Standard {
            assert!(contains_bytes(&bytes[..failed], b"\x1b[1;5r"));
        }
        assert!(
            contains_bytes(&bytes[world.backend.failed_raw_write_end()..], b"\x1b[r"),
            "margin reset is attempted after the failed write"
        );
    }
}
