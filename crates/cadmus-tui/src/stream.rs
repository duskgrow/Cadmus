//! The stream widget: the band's live tail over `cadmus-ui`'s markdown
//! pipeline (ADR-0018 items 2 and 4). The widget owns the pipeline's flush
//! contract with the shell — completed content leaves the band into real
//! scrollback continuously, never in one batch at turn end — and re-derives
//! everything (resize replay included) from the source SSOT.
//!
//! Wrapping discipline: [`crate::wrap`] is the single wrap implementation —
//! flush rows, band rows and height math all come from it, so they can
//! never disagree.

use cadmus_ui::highlight::Highlighter;
use cadmus_ui::markdown::{MarkdownStream, Render, render_document};
use cadmus_ui::theme::{ColorDepth, Theme};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::wrap::wrap_rows;

/// The streaming transcript's live tail. See the module docs for the
/// wrapping and flush contracts.
pub struct Stream {
    pipeline: MarkdownStream,
}

impl Stream {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pipeline: MarkdownStream::new(),
        }
    }

    /// Append one output delta. Nothing renders until [`Stream::render`].
    pub fn push_delta(&mut self, text: &str) {
        self.pipeline.push_delta(text);
    }

    /// Replace the buffered source with the authoritative complete item
    /// (item 4: a saturated transport cannot truncate the transcript).
    pub fn finalize(&mut self, full_source: &str) {
        self.pipeline.finalize(full_source);
    }

    /// The buffered source (the SSOT the resize replay re-renders from).
    #[must_use]
    pub fn source(&self) -> &str {
        self.pipeline.source()
    }

    /// Advance the pipeline's incremental render at `width`.
    pub fn render(&mut self, width: u16, highlighter: &Highlighter) -> &Render {
        self.pipeline.render(width, highlighter)
    }

    /// The flushable prefix, wrapped: the count of *logical* lines covered
    /// (for [`Stream::ack_flushed`]) plus the display rows to hand
    /// [`crate::shell::InlineShell::flush`]. Empty when nothing may flush.
    pub fn flushable_rows(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> (usize, Vec<Line<'static>>) {
        let render = self.pipeline.render(width, highlighter);
        let flushable = render.flushable_len();
        if flushable == 0 {
            return (0, Vec::new());
        }
        let rows = wrap_rows(&render.live_lines()[..flushable], width, theme, depth);
        (flushable, rows)
    }

    /// Confirm `lines` logical lines left the band (a successful shell
    /// flush). They never reappear in the live tail.
    pub fn ack_flushed(&mut self, lines: usize) {
        self.pipeline.ack_flushed(lines);
    }

    /// Every live (unflushed) row, wrapped — the band's stream-tail content.
    pub fn live_rows(
        &mut self,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Vec<Line<'static>> {
        let render = self.pipeline.render(width, highlighter);
        wrap_rows(render.live_lines(), width, theme, depth)
    }

    /// The live tail's wrapped row count — the layout function's stream
    /// input. Cheap: measured through the same wrapper, without a scratch
    /// render, and styles never affect wrapping (they are zero-width), so
    /// plain text is enough.
    pub fn live_row_count(&mut self, width: u16, highlighter: &Highlighter) -> u16 {
        let render = self.pipeline.render(width, highlighter);
        if render.live_lines().is_empty() {
            return 0;
        }
        let lines: Vec<Line<'static>> = render
            .live_lines()
            .iter()
            .map(|line| Line::from(line.text()))
            .collect();
        let count = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .line_count(width.max(1));
        u16::try_from(count).unwrap_or(u16::MAX)
    }

    /// The band's stream-tail slice: the live rows bottom-anchored into
    /// `height` rows (newest content wins when the tail is taller).
    #[must_use]
    pub fn visible(rows: &[Line<'static>], height: u16) -> Vec<Line<'static>> {
        let skip = rows.len().saturating_sub(usize::from(height));
        rows.iter().skip(skip).cloned().collect()
    }

    /// Resize replay (the shell's `on_resize` closure): the still-visible
    /// *flushed* history tail re-materialized from the source SSOT at the
    /// new width — up to `max_rows` display rows, newest last. The live tail
    /// is excluded: it comes back with the band's own repaint, and replaying
    /// it here would duplicate it. Only the committed source replays (the
    /// newline gate's partial trailing line has never been visible).
    pub fn replay_tail(
        &mut self,
        max_rows: u16,
        width: u16,
        highlighter: &Highlighter,
        theme: &Theme,
        depth: ColorDepth,
    ) -> Vec<Line<'static>> {
        let live = self
            .pipeline
            .render(width, highlighter)
            .live_lines()
            .to_vec();
        let committed = self.pipeline.committed_source().to_string();
        let logical = render_document(&committed, width, highlighter);
        // The document and the pipeline share one renderer, so the flushed
        // prefix is the document minus the still-live logical lines.
        let split = logical.len().saturating_sub(live.len());
        debug_assert_eq!(
            &logical[split..],
            live.as_slice(),
            "the document's tail must equal the pipeline's live lines"
        );
        let rows = wrap_rows(&logical[..split], width, theme, depth);
        Self::visible(&rows, max_rows)
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use cadmus_ui::ir;

    use super::*;
    use crate::test_util::{highlighter, texts};

    #[test]
    fn a_paragraph_flushes_once_it_completes() {
        let mut stream = Stream::new();
        stream.push_delta("hello world\n\nnext\n");
        let (logical, rows) =
            stream.flushable_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        // The completed paragraph flushes with its separator (item 4).
        assert_eq!(logical, 2);
        assert_eq!(texts(&rows), vec!["hello world", ""]);
        stream.ack_flushed(logical);
        let live = stream.live_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(texts(&live), vec!["next"]);
        let (logical, _) =
            stream.flushable_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(logical, 0);
    }

    #[test]
    fn wrap_rows_and_live_row_count_agree() {
        let mut stream = Stream::new();
        stream.push_delta("alpha beta gamma delta epsilon zeta\n\n");
        let rows = stream.live_rows(10, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        let count = stream.live_row_count(10, highlighter());
        assert_eq!(usize::from(count), rows.len());
        assert!(rows.len() > 1, "the long line wraps at width 10");
    }

    #[test]
    fn wide_content_wraps_below_the_width() {
        let logical = [ir::Line::plain("你好世界你好")]; // 12 columns of CJK
        let rows = wrap_rows(&logical, 8, &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!(texts(&rows), vec!["你好世界", "你好"]);
    }

    #[test]
    fn continuation_whitespace_survives_the_wrap() {
        // trim: false — whitespace past the wrap boundary carries to the
        // continuation row, so indented code keeps its indent.
        let logical = [ir::Line::plain("aaaa    bb")];
        let rows = wrap_rows(&logical, 6, &Theme::ansi(), ColorDepth::Truecolor);
        // With trim: true the continuation row would lose its leading space.
        assert_eq!(texts(&rows), vec!["aaaa", " bb"]);
    }

    #[test]
    fn the_replay_tail_renders_from_source_at_the_new_width() {
        let mut stream = Stream::new();
        stream.push_delta("alpha beta gamma delta\n\nsecond part here\n");
        let (logical, _rows) =
            stream.flushable_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        stream.ack_flushed(logical);
        // Only the flushed paragraph replays; the open one stays with the band.
        let rows = stream.replay_tail(10, 8, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        let texts = texts(&rows);
        assert!(texts.contains(&"alpha".to_string()), "{texts:?}");
        assert!(!texts.iter().any(|row| row.contains("here")), "{texts:?}");
    }

    #[test]
    fn visible_bottom_anchors_the_tail() {
        let rows: Vec<Line<'static>> = (0..5).map(|i| Line::from(format!("row {i}"))).collect();
        let window = Stream::visible(&rows, 2);
        assert_eq!(texts(&window), vec!["row 3", "row 4"]);
    }

    #[test]
    fn an_empty_stream_has_no_rows() {
        let mut stream = Stream::new();
        assert!(
            stream
                .live_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor)
                .is_empty()
        );
        assert_eq!(stream.live_row_count(80, highlighter()), 0);
        let (logical, rows) =
            stream.flushable_rows(80, highlighter(), &Theme::ansi(), ColorDepth::Truecolor);
        assert_eq!((logical, rows.len()), (0, 0));
    }
}
