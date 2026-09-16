//! Unit-test helpers shared across this crate's `#[cfg(test)]` modules.
//! Integration tests under `tests/` keep their own local copies — they
//! compile against the public API only.

use std::sync::OnceLock;

use cadmus_ui::highlight::Highlighter;
use ratatui::text::Line;

/// One shared highlighter for every render test.
pub fn highlighter() -> &'static Highlighter {
    static HIGHLIGHTER: OnceLock<Highlighter> = OnceLock::new();
    HIGHLIGHTER.get_or_init(Highlighter::new)
}

/// Rendered rows as plain text (styles dropped) for text-level assertions.
pub fn texts(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}
