//! The stream widget: one assistant block's markdown pipeline and the
//! flush contract with the shell (ADR-0018 items 2 and 4). The pipeline's
//! stable prefix is the emission queue's source — completed content leaves
//! into real scrollback through the app's paced drain, never in one batch
//! at turn end — and the unstable tail is never rendered (the 2026-09-20
//! second amendment). Resize replay re-derives everything from the source
//! SSOT.
//!
//! Wrapping discipline: [`crate::wrap`] is the single wrap implementation —
//! flush rows, band rows and height math all come from it, so they can
//! never disagree.

use cadmus_ui::highlight::Highlighter;
use cadmus_ui::markdown::{MarkdownStream, Render, render_document};
use cadmus_ui::theme::{ColorDepth, Theme};
use ratatui::text::Line;

use crate::wrap::wrap_rows;

/// The streaming transcript's markdown pipeline. See the module docs for
/// the wrapping and flush contracts.
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

    /// Confirm `lines` logical lines left for scrollback (a successful
    /// shell insert, confirmed by the drain). They never reappear in the
    /// live lines.
    pub fn ack_flushed(&mut self, lines: usize) {
        self.pipeline.ack_flushed(lines);
    }

    /// A display-window helper: the rows bottom-anchored into `height`
    /// rows (newest content wins when the rows are taller). The scrollback
    /// replay's window; the band itself has no content window (the paced
    /// drain's inserts are the visible stream).
    #[must_use]
    pub fn visible(rows: &[Line<'static>], height: u16) -> Vec<Line<'static>> {
        let skip = rows.len().saturating_sub(usize::from(height));
        rows.iter().skip(skip).cloned().collect()
    }

    /// Resize replay (the shell's `on_resize` closure): the still-visible
    /// *flushed* history tail re-materialized from the source SSOT at the
    /// new width — up to `max_rows` display rows, newest last. The unstable
    /// tail is excluded by construction: it is never rendered, and the
    /// queued-but-undrained stable rows are still live in the pipeline
    /// (the drain owns the ack contract), so replaying only the committed
    /// source can never paint them into scrollback early.
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
        // The completed paragraph flushes with its separator (item 4).
        let flushable = stream.render(80, highlighter()).flushable_len();
        assert_eq!(flushable, 2);
        stream.ack_flushed(flushable);
        let render = stream.render(80, highlighter());
        assert_eq!(render.live_lines().len(), 1);
        assert_eq!(render.live_lines()[0].text(), "next");
        assert_eq!(render.flushable_len(), 0);
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
        let flushable = stream.render(80, highlighter()).flushable_len();
        stream.ack_flushed(flushable);
        // Only the flushed paragraph replays; the open one stays out (the
        // unstable tail is never rendered).
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
    fn an_empty_stream_has_no_lines() {
        let mut stream = Stream::new();
        let render = stream.render(80, highlighter());
        assert!(render.live_lines().is_empty());
        assert_eq!(render.flushable_len(), 0);
    }
}
