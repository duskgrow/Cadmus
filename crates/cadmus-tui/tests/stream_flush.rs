//! The stream widget's flush channel, locked with vt100 (ADR-0018 item 4:
//! "the vt100 suite locks each channel at implementation"). The widget sits
//! on the real pipeline and the real shell: deltas stream in, completed
//! content leaves into scrollback *continuously* (never one batch at turn
//! end), held blocks (tables) block the flush prefix, an open fence's body
//! streams, and a width shrink re-materializes the visible tail from the
//! source SSOT. The band itself carries no stream slice — the unstable
//! tail is never rendered (the 2026-09-20 second amendment), which these
//! tests pin alongside the flush contract: mid-stream the world shows
//! exactly the flushed prefix and the band's placeholder rows.
//!
//! The strongest assertion form is the world's full non-blank row sequence
//! (scrollback + screen, oldest first): any lost, duplicated or stale row
//! breaks it. The band appears as its two placeholder rows — pacing and the
//! band's own slices are the app loop's suite.

mod common;

use std::sync::OnceLock;

use cadmus_tui::shell::{InlineShell, ScrollbackStrategy};
use cadmus_tui::stream::Stream;
use cadmus_tui::wrap::wrap_rows;
use cadmus_ui::highlight::Highlighter;
use cadmus_ui::theme::{ColorDepth, Theme};
use common::{GuardSink, VtBackend, World};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(Highlighter::new)
}

/// The placeholder band rows (composer + status).
const PROMPT_ROW: &str = "› prompt";
const STATUS_ROW: &str = "status";

/// The test app: the shell owns the terminal, the stream widget owns the
/// pipeline and the flush contract. The band is a fixed two-row placeholder:
/// its slices are the app's, and stream content never renders in it.
struct StreamApp {
    shell: InlineShell<VtBackend, GuardSink>,
    stream: Stream,
    theme: Theme,
    depth: ColorDepth,
}

impl StreamApp {
    fn boot(world: &World) -> Self {
        let guard = GuardSink::default();
        let shell = InlineShell::new(
            world.backend.clone(),
            guard,
            2,
            ScrollbackStrategy::FullScreen,
        )
        .expect("boot shell");
        let mut app = Self {
            shell,
            stream: Stream::new(),
            theme: Theme::ansi(),
            depth: ColorDepth::Truecolor,
        };
        app.pump();
        app
    }

    /// The band render: two placeholder rows, no stream slice.
    fn band_render() -> impl FnOnce(&mut Frame<'_>) {
        move |frame: &mut Frame<'_>| {
            let area = frame.area();
            frame.render_widget(
                Paragraph::new(PROMPT_ROW),
                Rect::new(area.x, area.y, area.width, 1),
            );
            frame.render_widget(
                Paragraph::new(STATUS_ROW),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
    }

    /// One event batch: flush the stable prefix (the same oracle the
    /// transcript's emission queue reads), then repaint.
    fn pump(&mut self) {
        let width = self.shell.width();
        let (logical, rows) = {
            let render = self.stream.render(width, highlighter());
            let flushable = render.flushable_len();
            let rows = wrap_rows(
                &render.live_lines()[..flushable],
                width,
                &self.theme,
                self.depth,
            );
            (flushable, rows)
        };
        if logical > 0 {
            let render = Self::band_render();
            self.shell.flush(&rows, render).expect("flush");
            self.stream.ack_flushed(logical);
        }
        let render = Self::band_render();
        self.shell.draw(render);
    }
}

/// The full-sequence assertion: scrollback + screen, oldest first, is
/// exactly the emitted history followed by the placeholder band.
fn assert_world(world: &World, expected_history: &[String]) {
    let mut expected = expected_history.to_vec();
    expected.extend([PROMPT_ROW.to_string(), STATUS_ROW.to_string()]);
    assert_eq!(
        world.nonblank_rows(),
        expected,
        "scrollback+screen sequence (visible: {:?}, scrollback: {:?})",
        world.visible_rows(),
        world.scrollback_rows()
    );
}

/// Boot a band over a short pre-existing shell session; returns the app and
/// the expected history seeded with those lines.
fn boot_anchored(world: &World) -> (StreamApp, Vec<String>) {
    let expected: Vec<String> = (0..4).map(|i| format!("sh$·cmd·{i}")).collect();
    world.print_lines(&expected);
    (StreamApp::boot(world), expected)
}

/// A multi-paragraph turn flushes paragraph by paragraph *while streaming* —
/// the stable/tail two-region model — never in one batch at turn end, and
/// the still-unstable paragraph never renders anywhere.
#[test]
fn completed_turns_flush_continuously_not_in_one_batch() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream.push_delta("first answer paragraph\n");
    app.pump();
    // The open paragraph holds — and renders nowhere: the world is exactly
    // the boot history and the placeholder band.
    assert_world(&world, &expected);

    app.stream.push_delta("\nsecond answer paragraph\n");
    app.pump();
    // Mid-stream: the completed first paragraph has already left for
    // scrollback — flush is continuous, not end-batched. The second
    // paragraph, still unstable, renders nowhere.
    expected.push("first answer paragraph".to_string());
    assert_world(&world, &expected);

    // Item completion is authoritative; finalize closes the open tail.
    app.stream
        .finalize("first answer paragraph\n\nsecond answer paragraph\n\nfinal words\n");
    app.pump();
    expected.extend([
        "second answer paragraph".to_string(),
        "final words".to_string(),
    ]);
    assert_world(&world, &expected);
}

/// Tables hold the flush prefix from their header until they settle (item 4:
/// column widths depend on all rows); a following block settles the table.
/// Held, the table renders nowhere; settled, it flushes whole.
#[test]
fn an_open_table_holds_then_flushes_on_settle() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream
        .push_delta("| name | value |\n| --- | --- |\n| alpha | 1 |\n");
    app.pump();
    // The open table holds — and renders nowhere while held.
    assert_world(&world, &expected);

    app.stream.push_delta("\nafter the table\n");
    app.pump();
    // Settled by the following paragraph, the whole table has flushed above
    // the band (the flat form at 80 columns: padded columns, ` | ` joins) —
    // proven by the sequence assertion.
    expected.push("name  | value".to_string());
    expected.push("----- | -----".to_string());
    expected.push("alpha | 1".to_string());
    assert_world(&world, &expected);
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
    // Both body lines already left for scrollback, fence still open.
    expected.extend(["let a = 1;".to_string(), "let b = 2;".to_string()]);
    assert_world(&world, &expected);

    app.stream.push_delta("```\nafter the fence\n");
    app.pump();
    // The closer rendered zero rows; the trailing paragraph holds open (and
    // renders nowhere).
    assert_world(&world, &expected);
    assert!(
        world
            .nonblank_rows()
            .iter()
            .all(|row| !row.starts_with("```")),
        "fence markers never render"
    );
}

/// A width shrink replaces the visible history through source replay;
/// the shell replays the still-visible *flushed* tail, re-rendered from the stream's source SSOT
/// at the new width — while the unstable tail stays unrendered and is never
/// duplicated above the band.
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
    assert_world(&world, &expected);
    let pre_scrollback = world.scrollback_rows();

    world.resize(24, 40);
    let render = StreamApp::band_render();
    let stream = &mut app.stream;
    let theme = &app.theme;
    let depth = app.depth;
    app.shell
        .on_resize(
            40,
            24,
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
    // the unstable tail ("eta theta ...") appears nowhere.
    let rows = world.nonblank_rows();
    let first_row = "one two three four five six seven eight";
    assert!(
        rows.windows(2)
            .any(|pair| pair == [first_row, "nine ten eleven twelve"]),
        "re-wrapped replay above the band: {rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("eta theta")),
        "the unstable tail is never rendered: {rows:?}"
    );

    // Streaming continues undisturbed at the new width; closing the
    // paragraph flushes it exactly once, at the new wrap. (The shrink
    // cleared the on-screen boot lines — the shell's shrink clear; the
    // replay owns only the stream's tail — so the world is now the
    // replay, the new flush and the band.)
    app.stream.push_delta("nu xi omicron pi rho\n\n");
    app.pump();
    assert_eq!(
        world.nonblank_rows(),
        vec![
            first_row.to_string(),
            "nine ten eleven twelve".to_string(),
            "eta theta iota kappa lambda mu nu xi".to_string(),
            "omicron pi rho".to_string(),
            PROMPT_ROW.to_string(),
            STATUS_ROW.to_string(),
        ],
        "the closed paragraph flushes once at the new width"
    );
}

/// The flush channel keeps its continuity across settles: each completed
/// block leaves as it closes, the open tail holds, and the final
/// finalize flushes the rest — the sequence the emission queue paces out.
#[test]
fn the_flush_channel_stays_continuous_across_settles() {
    let world = World::new();
    let (mut app, mut expected) = boot_anchored(&world);

    app.stream.push_delta("one\ntwo\nthree\nfour\n");
    app.pump();
    // One open paragraph (soft-joined into one logical line): nothing
    // stable yet, nothing rendered.
    assert_world(&world, &expected);

    app.stream.push_delta("\nfive six seven eight nine ten\n");
    app.pump();
    // The completed first paragraph (with its separator) flushed.
    expected.push("one two three four".to_string());
    assert_world(&world, &expected);

    app.stream.push_delta("\nseven\n");
    app.pump();
    expected.push("five six seven eight nine ten".to_string());
    assert_world(&world, &expected);

    app.stream
        .finalize("one two three four\n\nfive six seven eight nine ten\n\nseven\n");
    app.pump();
    expected.push("seven".to_string());
    assert_world(&world, &expected);
}
