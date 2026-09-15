//! Syntax highlighting for fenced code, part of the streaming-markdown
//! pipeline (ADR-0018 item 4): syntect over two-face's bat corpus, emitting
//! the semantic-style IR (ADR-0017), never renderer types.
//!
//! Design choices:
//! - The theme is two-face's embedded `Ansi` theme: its palette is built
//!   from the terminal's own named colors, so code stays terminal-relative
//!   and readable on dark and light terminals alike, following the user's
//!   palette instead of fighting it (the ADR-0017 item 5 principle applied
//!   to syntax scopes).
//! - Token backgrounds are dropped: only foreground and font style map onto
//!   the IR. Per-token background rectangles look patchy in a terminal where
//!   the emulator's own background shows through, so `bg` stays `None` and
//!   the terminal background wins.
//! - [`Highlighter`] construction deserializes the full syntax set and theme
//!   — slow by design; callers hold it in a `OnceLock` or a long-lived
//!   struct (sanctioned by ADR-0018 item 4).
//! - Degradation guards (the Codex precedent): a single line over 64 KiB or
//!   a snippet over 1 MiB renders plain — sublime-syntax regex matching is
//!   superlinear on pathological inputs, and the transcript must stay
//!   responsive on minified junk. An over-limit line bypasses the parser
//!   entirely, so the highlight state skips it; later lines may be
//!   minimally miscolored, an accepted cost for junk input.

use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as SyntectHighlighter, Theme,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

use crate::ir::{Color, Line, Modifiers, Span, Style};

/// Lines longer than this render plain (module docs).
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Snippets larger than this render plain (module docs). Shared with the
/// markdown pipeline's open-fence fallback, which re-renders whole bodies.
pub(crate) const MAX_SNIPPET_BYTES: usize = 1024 * 1024;

/// The shared highlighting context: syntax corpus plus theme. Cheap to
/// share by reference, expensive to build.
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    /// Load two-face's newline-aware syntax corpus and the embedded `Ansi`
    /// theme (module docs for the theme choice).
    #[must_use]
    pub fn new() -> Self {
        Self {
            syntax_set: two_face::syntax::extra_newlines(),
            theme: two_face::theme::extra()
                .get(two_face::theme::EmbeddedThemeName::Ansi)
                .clone(),
        }
    }

    /// Open an incremental highlighter for a fence tagged `lang_token`.
    /// `None` for an empty or unknown token — the caller then renders body
    /// lines plain.
    #[must_use]
    pub fn open_fence(&self, lang_token: &str) -> Option<FenceHighlighter> {
        let token = lang_token.trim();
        if token.is_empty() {
            return None;
        }
        let syntax = self.syntax_set.find_syntax_by_token(token)?;
        Some(FenceHighlighter {
            parse_state: ParseState::new(syntax),
            highlight_state: HighlightState::new(
                &SyntectHighlighter::new(&self.theme),
                ScopeStack::new(),
            ),
        })
    }

    /// One-shot full highlight of a complete snippet: closed fences and the
    /// open-fence fallback path. Unknown languages and over-limit snippets
    /// render plain, one [`Line`] per source line.
    #[must_use]
    pub fn highlight_snippet(&self, lang_token: &str, code: &str) -> Vec<Line> {
        if code.len() > MAX_SNIPPET_BYTES {
            return plain_lines(code);
        }
        match self.open_fence(lang_token) {
            Some(mut fence) => code
                .lines()
                .map(|line| fence.push_line(line, self))
                .collect(),
            None => plain_lines(code),
        }
    }
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

/// Incremental per-line highlighter for one fence: carries syntect's
/// [`ParseState`] (the scope parser) and [`HighlightState`] (the style
/// resolver) across calls, so a streaming fence highlights in O(new lines)
/// instead of re-rendering its whole body per delta (ADR-0018 item 4).
pub struct FenceHighlighter {
    parse_state: ParseState,
    highlight_state: HighlightState,
}

impl FenceHighlighter {
    /// Highlight one complete body line (without its trailing newline),
    /// continuing the parser and highlight state. Over-long lines and
    /// syntect parse failures degrade to a plain line.
    pub fn push_line(&mut self, line: &str, highlighter: &Highlighter) -> Line {
        if line.len() > MAX_LINE_BYTES {
            return Line::plain(line);
        }
        let Ok(ops) = self.parse_state.parse_line(line, &highlighter.syntax_set) else {
            return Line::plain(line);
        };
        // `SyntectHighlighter` is a cheap per-call wrapper over the theme;
        // the resumable state lives in `self.highlight_state`.
        let theme_highlighter = SyntectHighlighter::new(&highlighter.theme);
        let tokens =
            HighlightIterator::new(&mut self.highlight_state, &ops, line, &theme_highlighter);
        let mut spans: Vec<Span> = Vec::new();
        for (style, text) in tokens {
            if text.is_empty() {
                continue;
            }
            let style = map_style(style);
            if let Some(last) = spans.last_mut()
                && last.style == style
            {
                last.text.push_str(text);
            } else {
                spans.push(Span {
                    text: text.to_string(),
                    style,
                });
            }
        }
        Line { spans }
    }
}

/// syntect → IR style mapping: foreground becomes raw sRGB, font style
/// becomes modifiers, and the token background is dropped (module docs).
fn map_style(style: syntect::highlighting::Style) -> Style {
    let fg = style.foreground;
    Style {
        fg: Some(Color::Rgb(fg.r, fg.g, fg.b)),
        bg: None,
        mods: Modifiers {
            bold: style.font_style.contains(FontStyle::BOLD),
            italic: style.font_style.contains(FontStyle::ITALIC),
            underline: style.font_style.contains(FontStyle::UNDERLINE),
            ..Modifiers::default()
        },
    }
}

/// One plain literal [`Line`] per source line.
fn plain_lines(code: &str) -> Vec<Line> {
    code.lines().map(Line::plain).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    fn highlighter() -> &'static Highlighter {
        static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
        HIGHLIGHTER.get_or_init(Highlighter::new)
    }

    #[test]
    fn an_unknown_language_opens_no_fence() {
        assert!(highlighter().open_fence("not-a-language").is_none());
    }

    #[test]
    fn an_empty_language_token_opens_no_fence() {
        assert!(highlighter().open_fence("").is_none());
        assert!(highlighter().open_fence("   ").is_none());
    }

    #[test]
    fn a_known_language_highlights_into_rgb_spans() {
        let lines = highlighter().highlight_snippet("rust", "fn main() {}\n");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
        );
    }

    #[test]
    fn highlighted_text_roundtrips_the_source() {
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        let lines = highlighter().highlight_snippet("rust", code);
        let text: String = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
        assert_eq!(text, code.trim_end_matches('\n'));
    }

    #[test]
    fn the_incremental_fence_matches_the_one_shot_snippet() {
        // A block comment spanning lines forces the state to carry.
        let code = "fn main() {\n    /* start\n       still comment */\n    let x = 1;\n}\n";
        let mut fence = highlighter().open_fence("rust").unwrap();
        let incremental: Vec<Line> = code
            .lines()
            .map(|line| fence.push_line(line, highlighter()))
            .collect();
        assert_eq!(incremental, highlighter().highlight_snippet("rust", code));
    }

    #[test]
    fn adjacent_runs_with_equal_styles_merge() {
        let lines = highlighter().highlight_snippet("rust", "let abc = def + ghi;\n");
        for line in &lines {
            assert!(
                line.spans
                    .windows(2)
                    .all(|pair| pair[0].style != pair[1].style),
                "adjacent same-style spans should have merged: {:?}",
                line.spans
            );
        }
    }

    #[test]
    fn a_line_over_64_kib_renders_plain() {
        let long_line = "x".repeat(MAX_LINE_BYTES + 1);
        let mut fence = highlighter().open_fence("rust").unwrap();
        let line = fence.push_line(&long_line, highlighter());
        assert_eq!(line, Line::plain(&long_line));
    }

    #[test]
    fn a_snippet_over_1_mib_renders_plain() {
        let code = "let x = 1;\n".repeat(MAX_SNIPPET_BYTES / 11 + 1);
        let lines = highlighter().highlight_snippet("rust", &code);
        assert!(
            lines
                .iter()
                .all(|line| { line.spans.iter().all(|span| span.style == Style::default()) })
        );
    }

    #[test]
    fn token_backgrounds_are_dropped() {
        let lines = highlighter().highlight_snippet("rust", "fn main() {}\n");
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg.is_none())
        );
    }

    #[test]
    fn an_empty_snippet_renders_no_lines() {
        assert!(highlighter().highlight_snippet("rust", "").is_empty());
    }
}
