//! The diff pipeline (ADR-0018 item 8): a `similar` line diff with inline
//! word-level spans, emitted as IR lines tagged with ADR-0017's `diff-*`
//! slots — renderer-agnostic, shared by the approval prompt and the `/diff`
//! Old→new content pairs come from the client (tool-call arguments
//! for `edit_file`, a workspace read for `write_file` against an existing
//! file); this module stays pure.
//!
//! Two recorded degradations. Inline granularity is `similar`'s
//! tokenization: without the `unicode` feature, a CJK run tokenizes as one
//! word, so a CJK word edit tints the whole line, not the word (line-level
//! diffing is unaffected; enabling the feature is the evidence-gated
//! upgrade, `unicode-segmentation` is already in-tree). The hunk divider
//! rides as plain span text with no way to flag it as decorative —
//! glyph-tier degradation (ADR-0017 items 5 and 9) is the render layer's
//! job and lands with the icon registry.

use similar::{ChangeTag, TextDiff};

use crate::ir::{Color, Line, Slot, Span, Style};

/// Context lines shown around a change — the unified-diff default (the
/// tool-result feedback path uses the same radius).
const CONTEXT_RADIUS: usize = 3;

/// Separator emitted between change groups farther apart than twice the
/// context radius.
const DIVIDER: &str = "⋯";

/// Diff `old_text` → `new_text` into IR lines, one per changed or context
/// line: the gutter is `- ` (removed), `+ ` (added) or two spaces (context,
/// subtle); intra-line word changes carry the paired `-bg` tint over the
/// line's `diff-*` foreground (the delta/bat convention — at 16 colors the
/// `*-bg` slots resolve to the plain tone, so tinting degrades to the
/// foreground alone). Returns no lines for identical texts: a net-zero
/// change reports itself, never a diff (the tool-result path's rule).
/// Consumers own viewporting; the context radius bounds the output.
#[must_use]
pub fn diff_lines(old_text: &str, new_text: &str) -> Vec<Line> {
    if old_text == new_text {
        return Vec::new();
    }
    let diff = TextDiff::from_lines(old_text, new_text);
    let groups = diff.grouped_ops(CONTEXT_RADIUS);
    let mut lines = Vec::new();
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from_spans(vec![Span::slotted(
                DIVIDER,
                Slot::TextSubtle,
            )]));
        }
        for op in group {
            for change in diff.iter_inline_changes(op) {
                lines.push(change_line(&change));
            }
        }
    }
    lines
}

/// Style one diff line: gutter plus content spans. The line tag selects the
/// slot pair; the inline pass flags the changed words for the background
/// tint. Line values keep their terminator from `from_lines` — strip it,
/// carriage return included (CRLF files diff against their own bytes).
fn change_line(change: &similar::InlineChange<'_, str>) -> Line {
    let (fg, bg) = match change.tag() {
        ChangeTag::Delete => (Slot::DiffRemoved, Slot::DiffRemovedBg),
        ChangeTag::Insert => (Slot::DiffAdded, Slot::DiffAddedBg),
        ChangeTag::Equal => (Slot::TextSubtle, Slot::TextSubtle),
    };
    let fg = Color::Slot(fg);
    let bg = Color::Slot(bg);
    let gutter = match change.tag() {
        ChangeTag::Delete => "- ",
        ChangeTag::Insert => "+ ",
        ChangeTag::Equal => "  ",
    };
    // The gutter shows the change kind by geometry and the content by color
    // (double-coded per ADR-0017); it never carries the line's own tint.
    let mut spans = vec![Span::slotted(gutter, Slot::TextSubtle)];
    for &(emphasized, value) in change.values() {
        let text = value.strip_suffix('\n').unwrap_or(value);
        let text = text.strip_suffix('\r').unwrap_or(text);
        if text.is_empty() {
            continue;
        }
        let style = if emphasized {
            Style {
                fg: Some(fg),
                bg: Some(bg),
                ..Style::default()
            }
        } else {
            Style {
                fg: Some(fg),
                ..Style::default()
            }
        };
        spans.push(Span {
            text: text.to_string(),
            style,
        });
    }
    Line::from_spans(spans)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn numbered_lines(n: usize) -> String {
        let mut text = String::new();
        for i in 1..=n {
            let _ = writeln!(text, "line {i}");
        }
        text
    }

    fn diff(old: &str, new: &str) -> Vec<Line> {
        diff_lines(old, new)
    }

    fn tinted(line: &Line) -> Vec<&str> {
        line.spans
            .iter()
            .filter(|span| span.style.bg.is_some())
            .map(|span| span.text.as_str())
            .collect()
    }

    #[test]
    fn identical_texts_render_no_lines() {
        assert!(diff("same\n", "same\n").is_empty());
        assert!(diff("", "").is_empty());
    }

    #[test]
    fn an_added_line_reads_in_the_added_slot() {
        let lines = diff("a\n", "a\nb\n");
        assert_eq!(lines.len(), 2);
        let added = &lines[1];
        assert_eq!(added.text(), "+ b");
        assert_eq!(
            added.spans[0].style.fg,
            Some(Color::Slot(Slot::TextSubtle)),
            "the gutter is chrome, never the line's own tint"
        );
        let content = &added.spans[1];
        assert_eq!(content.style.fg, Some(Color::Slot(Slot::DiffAdded)));
        assert_eq!(content.style.bg, None, "whole added lines need no tint");
    }

    #[test]
    fn a_removed_line_reads_in_the_removed_slot() {
        let lines = diff("a\nb\n", "a\n");
        assert_eq!(lines.len(), 2);
        let removed = &lines[1];
        assert_eq!(removed.text(), "- b");
        assert_eq!(
            removed.spans[0].style.fg,
            Some(Color::Slot(Slot::TextSubtle)),
            "the gutter is chrome, never the line's own tint"
        );
        assert_eq!(
            removed.spans[1].style.fg,
            Some(Color::Slot(Slot::DiffRemoved))
        );
    }

    #[test]
    fn an_empty_removed_line_renders_its_gutter_only() {
        let lines = diff("a\n\nb\n", "a\nb\n");
        assert_eq!(lines[1].text(), "- ");
        assert_eq!(
            lines[1].spans.len(),
            1,
            "the stripped line leaves the gutter as the only span"
        );
    }

    #[test]
    fn an_inline_word_change_tints_only_the_changed_words() {
        let lines = diff("the quick brown\n", "the quick fox\n");
        assert_eq!(lines.len(), 2, "one removed line paired with one added");
        let removed = &lines[0];
        assert_eq!(removed.text(), "- the quick brown");
        assert_eq!(tinted(removed), vec!["brown"]);
        let added = &lines[1];
        assert_eq!(added.text(), "+ the quick fox");
        assert_eq!(tinted(added), vec!["fox"]);
        // The equal head of the line carries the line's foreground, no tint
        // (the inline pass merges adjacent equal words into one span).
        assert!(
            removed
                .spans
                .iter()
                .any(|span| span.text == "the quick " && span.style.bg.is_none())
        );
    }

    #[test]
    fn context_lines_are_subtle_and_radius_bounded() {
        let old = numbered_lines(100);
        let new = old.replace("line 50", "line fifty");
        let lines = diff(&old, &new);
        assert!(
            lines.len() <= 2 * CONTEXT_RADIUS + 2,
            "one hunk with its context, not the file: {}",
            lines.len()
        );
        for line in &lines {
            if line.text().starts_with('-') || line.text().starts_with('+') {
                continue;
            }
            let content = &line.spans[1];
            assert_eq!(
                content.style.fg,
                Some(Color::Slot(Slot::TextSubtle)),
                "context recedes: {:?}",
                line.text()
            );
        }
    }

    #[test]
    fn far_apart_hunks_are_divided() {
        let old = numbered_lines(100);
        let new = old
            .replace("line 2", "line two")
            .replace("line 98", "line ninety-eight");
        let lines = diff(&old, &new);
        assert!(
            lines.len() > 2 * CONTEXT_RADIUS + 4,
            "two hunks stay separate, not one merged run: {}",
            lines.len()
        );
        assert!(
            lines.iter().any(|line| line.text() == DIVIDER),
            "the hunks separate with a divider: {lines:?}"
        );
    }

    #[test]
    fn a_new_file_is_all_added() {
        let lines = diff("", "x\ny\n");
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(line.text().starts_with('+'));
        }
    }

    #[test]
    fn crlf_lines_diff_without_stray_carriage_returns() {
        let lines = diff("a\r\nb\r\n", "a\r\nc\r\n");
        assert_eq!(lines[1].text(), "- b");
        assert_eq!(lines[2].text(), "+ c");
    }
}
