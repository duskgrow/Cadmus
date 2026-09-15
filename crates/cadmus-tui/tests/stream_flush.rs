//! The stream widget's flush channel, locked with vt100 (ADR-0018 item 4:
//! "the vt100 suite locks each channel at implementation"). The widget sits
//! on the real pipeline and the real shell: deltas stream in, completed
//! content leaves the band into scrollback *continuously* (never one batch
//! at turn end), held blocks (tables) block the flush prefix, an open
//! fence's body streams, the band's height follows the layout function, and
//! a width shrink re-materializes the visible tail from the source SSOT.
//!
//! The strongest assertion form is the world's full non-blank row sequence
//! (scrollback + screen, oldest first): any lost, duplicated or stale row
//! breaks it. The composer appears as a one-row placeholder — its own
//! rendering is unit-tested in `composer.rs`.

mod common;

use std::sync::OnceLock;

use cadmus_tui::layout::{self, LayoutInput};
use cadmus_tui::shell::InlineShell;
use cadmus_tui::stream::Stream;
use cadmus_ui::highlight::Highlighter;
use cadmus_ui::theme::{ColorDepth, Theme};
use common::{GuardSink, SCREEN_ROWS, VtBackend, World};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(Highlighter::new)
}

fn texts(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// The placeholder band rows below the stream area (composer + status).
const PROMPT_ROW: &str = "› prompt";
const STATUS_ROW: &str = "status";

/// The test app: the shell owns the terminal, the stream widget owns the
/// pipeline and the flush contract, and the layout function owns the band
/// height — event-driven (pumped per delta batch, never per frame).
struct StreamApp {
    shell: InlineShell<VtBackend, GuardSink>,
    stream: Stream,
    theme: Theme,
    depth: ColorDepth,
}

impl StreamApp {
    fn boot(world: &World) -> Self {
        let guard = GuardSink::default();
        let shell = InlineShell::new(world.backend.clone(), guard, 2).expect("boot shell");
        let mut app = Self {
            shell,
            stream: Stream::new(),
            theme: Theme::ansi(),
            depth: ColorDepth::Truecolor,
        };
        app.pump();
        app
    }

    /// Materialize-then-draw: the closure captures owned rows, never a
    /// borrow of the app, so it runs inside shell ops that hold the
    /// terminal mutably.
    fn band_render(&mut self) -> impl FnOnce(&mut Frame<'_>) + use<> {
        let width = self.shell.width();
        let rows = self
            .stream
            .live_rows(width, highlighter(), &self.theme, self.depth);
        let split = split(u16::try_from(rows.len()).unwrap_or(u16::MAX));
        let stream_rows = Stream::visible(&rows, split.stream_rows);
        let composer_rows = split.composer_rows;
        let status_rows = split.status_rows;
        move |frame: &mut Frame<'_>| {
            let area = frame.area();
            let stream_height = u16::try_from(stream_rows.len()).unwrap_or(u16::MAX);
            let (mut y, width) = (area.y, area.width);
            frame.render_widget(
                Paragraph::new(stream_rows),
                Rect::new(area.x, y, width, stream_height),
            );
            y += stream_height;
            frame.render_widget(
                Paragraph::new(PROMPT_ROW),
                Rect::new(area.x, y, width, composer_rows),
            );
            y += composer_rows;
            if status_rows > 0 {
                frame.render_widget(
                    Paragraph::new(STATUS_ROW),
                    Rect::new(area.x, y, width, status_rows),
                );
            }
        }
    }

    /// One event batch: flush what may leave, then re-fit the band height
    /// and repaint — the amendments' event-driven height discipline.
    fn pump(&mut self) {
        let width = self.shell.width();
        let (logical, rows) =
            self.stream
                .flushable_rows(width, highlighter(), &self.theme, self.depth);
        if logical > 0 {
            let render = self.band_render();
            self.shell.flush(&rows, render).expect("flush");
            self.stream.ack_flushed(logical);
        }
        let render = self.band_render();
        let desired = {
            let width = self.shell.width();
            let count = self.stream.live_row_count(width, highlighter());
            split(count).band_height
        };
        self.shell.set_height(desired, render).expect("set height");
        let render = self.band_render();
        self.shell.draw(render);
    }

    /// The band's expected visible rows, bottom to the placeholder status.
    fn expected_band(&mut self) -> Vec<String> {
        let width = self.shell.width();
        let rows = self
            .stream
            .live_rows(width, highlighter(), &self.theme, self.depth);
        let split = split(u16::try_from(rows.len()).unwrap_or(u16::MAX));
        let mut band = texts(&Stream::visible(&rows, split.stream_rows));
        band.push(PROMPT_ROW.to_string());
        if split.status_rows > 0 {
            band.push(STATUS_ROW.to_string());
        }
        band.retain(|row| !row.is_empty());
        band
    }

    /// The full-sequence assertion: scrollback + screen, oldest first, is
    /// exactly the emitted history followed by the current band.
    fn assert_world(&mut self, world: &World, expected_history: &[String]) {
        let mut expected = expected_history.to_vec();
        expected.extend(self.expected_band());
        assert_eq!(
            world.nonblank_rows(),
            expected,
            "scrollback+screen sequence (visible: {:?}, scrollback: {:?})",
            world.visible_rows(),
            world.scrollback_rows()
        );
    }
}

/// The layout split for the current content — the height function's
/// output, fed to `set_height` (which no-ops on equality).
fn split(stream_rows: u16) -> layout::BandLayout {
    layout::layout(&LayoutInput {
        screen_rows: SCREEN_ROWS,
        stream_rows,
        composer_rows: 1,
    })
}

/// Boot a band over a short pre-existing shell session; returns the app and
/// the expected history seeded with those lines.
fn boot_anchored(world: &World) -> (StreamApp, Vec<String>) {
    let expected: Vec<String> = (0..4).map(|i| format!("sh$·cmd·{i}")).collect();
    world.print_lines(&expected);
    (StreamApp::boot(world), expected)
}

/// A multi-paragraph turn flushes paragraph by paragraph *while streaming* —
/// the stable/tail two-region model — never in one batch at turn end.
#[test]
fn completed_turns_flush_continuously_not_in_one_batch() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream.push_delta("first answer paragraph\n");
    app.pump();
    // The open paragraph holds: it renders only inside the band.
    let rows = world.nonblank_rows();
    assert_eq!(
        rows[rows.len() - 3..],
        ["first answer paragraph", PROMPT_ROW, STATUS_ROW],
        "the open paragraph holds in the band: {rows:?}"
    );

    app.stream.push_delta("\nsecond answer paragraph\n");
    app.pump();
    // Mid-stream: the completed first paragraph has already left the band
    // (it sits above it) — flush is continuous, not end-batched. The
    // sequence assertion proves it: history first, then the band.
    expected.push("first answer paragraph".to_string());
    app.assert_world(&world, &expected);

    // Item completion is authoritative; finalize closes the open tail.
    app.stream
        .finalize("first answer paragraph\n\nsecond answer paragraph\n\nfinal words\n");
    app.pump();
    expected.extend([
        "second answer paragraph".to_string(),
        "final words".to_string(),
    ]);
    app.assert_world(&world, &expected);
    // Nothing flushable remains: the band holds only the placeholder rows.
    assert_eq!(app.shell.band_height(), 2);
}

/// Tables hold the flush prefix from their header until they settle (item 4:
/// column widths depend on all rows); a following block settles the table.
#[test]
fn an_open_table_holds_then_flushes_on_settle() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream
        .push_delta("| name | value |\n| --- | --- |\n| alpha | 1 |\n");
    app.pump();
    // The open table renders live in the band; nothing has left it.
    app.assert_world(&world, &expected);
    assert!(
        app.expected_band().iter().any(|row| row.contains("alpha")),
        "the held table renders live in the band"
    );

    app.stream.push_delta("\nafter the table\n");
    app.pump();
    // Settled by the following paragraph, the whole table has flushed above
    // the band (the flat form at 80 columns: padded columns, ` | ` joins) —
    // proven by the sequence assertion.
    expected.push("name  | value".to_string());
    expected.push("alpha | 1".to_string());
    app.assert_world(&world, &expected);
}

/// An open fence's body lines are literal: they flush while the fence is
/// still open (item 4), and the closer never renders a row of its own.
#[test]
fn an_open_fence_streams_its_body_lines() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream.push_delta("```rust\nlet a = 1;\n");
    app.pump();
    app.stream.push_delta("let b = 2;\n");
    app.pump();
    // Both body lines already left the band, fence still open.
    expected.extend(["let a = 1;".to_string(), "let b = 2;".to_string()]);
    app.assert_world(&world, &expected);

    app.stream.push_delta("```\nafter the fence\n");
    app.pump();
    // The closer rendered zero rows; the trailing paragraph holds open.
    app.assert_world(&world, &expected);
    assert!(
        world
            .nonblank_rows()
            .iter()
            .all(|row| !row.starts_with("```")),
        "fence markers never render"
    );
}

/// A width shrink clears the screen (stock ratatui); the shell replays the
/// still-visible *flushed* tail, re-rendered from the stream's source SSOT
/// at the new width — while the live tail stays with the band's repaint and
/// is never duplicated above it.
#[test]
fn width_shrink_replays_the_stream_tail_from_source() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    // One row at 80 columns, two rows once re-wrapped at 40.
    let long_paragraph = "one two three four five six seven eight nine ten eleven twelve";
    app.stream.push_delta(long_paragraph);
    app.stream.push_delta("\n\n");
    app.stream.push_delta("eta theta iota kappa lambda mu\n");
    app.pump();
    expected.push(long_paragraph.to_string());
    app.assert_world(&world, &expected);
    let pre_scrollback = world.scrollback_rows();

    world.resize(SCREEN_ROWS, 40);
    let render = app.band_render();
    let stream = &mut app.stream;
    let theme = &app.theme;
    let depth = app.depth;
    app.shell
        .on_resize(
            40,
            SCREEN_ROWS,
            |max_rows, width| stream.replay_tail(max_rows, width, highlighter(), theme, depth),
            render,
        )
        .expect("resize");
    assert_eq!(
        world.scrollback_rows(),
        pre_scrollback,
        "width shrink must not touch scrollback"
    );
    // The replayed rows are the flushed paragraph re-wrapped at 40 columns;
    // the live tail ("eta theta ...") appears only inside the band.
    let rows = world.nonblank_rows();
    let first_row = "one two three four five six seven eight";
    assert!(
        rows.windows(2)
            .any(|pair| pair == [first_row, "nine ten eleven twelve"]),
        "re-wrapped replay above the band: {rows:?}"
    );
    let eta_sightings = rows
        .iter()
        .filter(|row| row.as_str() == "eta theta iota kappa lambda mu")
        .count();
    assert_eq!(eta_sightings, 1, "the live tail is never duplicated");
    let band = app.expected_band();
    assert_eq!(&rows[rows.len() - band.len()..], band, "band intact");

    // Streaming continues undisturbed at the new width.
    app.stream.push_delta("nu xi omicron pi rho\n");
    app.pump();
    let rows = world.nonblank_rows();
    let band = app.expected_band();
    assert_eq!(&rows[rows.len() - band.len()..], band);
}

/// The band's height is event-driven over the layout function (the second
/// 2026-09-14 amendment): content arrival grows it, the flush shrinks it
/// back, and idle returns to composer + status.
#[test]
fn the_band_height_follows_content_across_the_flush() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);
    assert_eq!(app.shell.band_height(), 2, "idle: composer + status");

    app.stream.push_delta("one\ntwo\nthree\nfour\n");
    app.pump();
    // One open paragraph (soft-joined into one logical line): one live row.
    assert_eq!(app.shell.band_height(), 3);

    app.stream.push_delta("\nfive six seven eight nine ten\n");
    app.pump();
    // The completed first paragraph (with its separator) flushed; the band
    // is back to one live row.
    expected.push("one two three four".to_string());
    app.assert_world(&world, &expected);
    assert_eq!(app.shell.band_height(), 3);

    app.stream.push_delta("\nseven\n");
    app.pump();
    expected.push("five six seven eight nine ten".to_string());
    app.assert_world(&world, &expected);

    app.stream
        .finalize("one two three four\n\nfive six seven eight nine ten\n\nseven\n");
    app.pump();
    expected.push("seven".to_string());
    app.assert_world(&world, &expected);
    assert_eq!(app.shell.band_height(), 2, "all flushed: back to idle");
}
