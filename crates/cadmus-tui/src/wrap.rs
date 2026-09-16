//! The one wrap implementation (ADR-0018 item 1: ratatui's own word wrapper;
//! the `textwrap` crate stays deferred). Flush rows, band rows and height
//! math all come from [`Paragraph`]-wrapping the same mapped lines, so they
//! can never disagree; the scratch render only reads back what ratatui
//! itself would draw. `Wrap { trim: false }` keeps continuation-line
//! indentation — fenced code must not lose leading whitespace; the prose
//! cost (a leading space on continuation rows) is accepted until the
//! textwrap trigger fires.
//!
//! Wrapping is per-logical-line independent (ratatui wraps each `Line`
//! separately, no cross-line flow), so a wrapped prefix is always a row
//! prefix of the wrapped whole — the flush split and the stream suite rely
//! on this.

use cadmus_ui::ir;
use cadmus_ui::theme::{ColorDepth, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use crate::style::ir_style;

/// Map logical IR lines onto ratatui lines under the theme and depth.
fn map_lines(lines: &[ir::Line], theme: &Theme, depth: ColorDepth) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            Line::from(
                line.spans
                    .iter()
                    .map(|span| {
                        Span::styled(span.text.clone(), ir_style(&span.style, theme, depth))
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Word-wrap logical lines into display rows through ratatui's own wrapper,
/// reading the scratch render back into lines (module docs for the why).
/// Trailing whitespace is trimmed from each row — invisible on screen, and
/// keeping it would only pollute scrollback diffs.
#[must_use]
pub fn wrap_rows(
    logical: &[ir::Line],
    width: u16,
    theme: &Theme,
    depth: ColorDepth,
) -> Vec<Line<'static>> {
    if logical.is_empty() {
        return Vec::new();
    }
    let width = width.max(1);
    let paragraph = Paragraph::new(map_lines(logical, theme, depth)).wrap(Wrap { trim: false });
    let height = paragraph.line_count(width);
    let height = u16::try_from(height).unwrap_or(u16::MAX);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    paragraph.render(area, &mut buf);
    (0..height).map(|y| extract_row(&buf, y, width)).collect()
}

/// One row of the scratch buffer as a line: consecutive same-style cells
/// merge, wide-grapheme continuation cells (which `Buffer` resets to a
/// blank symbol) are skipped by width math, and trailing whitespace drops —
/// invisible on screen, and keeping it would only pollute scrollback diffs.
fn extract_row(buf: &Buffer, y: u16, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut skip = 0u16;
    let mut x = 0;
    while x < width {
        let Some(cell) = buf.cell((x, y)) else { break };
        x += 1;
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let symbol = cell.symbol();
        skip = unicode_width::UnicodeWidthStr::width(symbol)
            .saturating_sub(1)
            .try_into()
            .unwrap_or(u16::MAX);
        let style = ratatui::style::Style::default()
            .fg(cell.fg)
            .bg(cell.bg)
            .add_modifier(cell.modifier);
        if symbol.is_empty() {
            continue; // zero-width symbols carry no ink
        }
        if let Some(last) = spans.last_mut()
            && last.style == style
        {
            last.content.to_mut().push_str(symbol);
        } else {
            spans.push(Span::styled(symbol.to_string(), style));
        }
    }
    while spans
        .last()
        .is_some_and(|span| span.content.trim().is_empty())
    {
        spans.pop();
    }
    if let Some(last) = spans.last_mut() {
        let trimmed = last.content.trim_end().to_string();
        last.content = trimmed.into();
    }
    Line::from(spans)
}
