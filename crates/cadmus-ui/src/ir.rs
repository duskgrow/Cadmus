//! The semantic-style IR (ADR-0018 item 2): content pipelines emit spans
//! tagged with ADR-0017's slot roles, never renderer types. `cadmus-tui`
//! maps the IR onto ratatui lines; the future GUI maps it onto its rich-text
//! equivalent — the hard problems (incremental parse, highlight state, diff
//! computation) are solved once, here.

use unicode_width::UnicodeWidthStr;

/// The semantic slot set, fixed at 18 (ADR-0017 item 3). A new slot is
/// admitted only by naming the landed renderer that consumes it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Slot {
    Bg,
    BgSubtle,
    Text,
    TextSubtle,
    Accent,
    OnAccent,
    Success,
    Warning,
    Error,
    Info,
    Border,
    BorderActive,
    DiffAdded,
    DiffRemoved,
    DiffAddedBg,
    DiffRemovedBg,
    Mark,
    Selection,
}

/// A color reference: either a semantic slot (resolved by the theme) or raw
/// sRGB from the syntax highlighter — syntect themes carry their own
/// palette, and degrading it safely is the renderer's job (ADR-0017 item 5:
/// ratatui does not degrade `Rgb` on its own).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    Slot(Slot),
    Rgb(u8, u8, u8),
}

/// Text modifiers the IR may carry. ADR-0017 item 6: the TUI hierarchy maps
/// onto bold/dim/inverse; italic may decorate (quotes, asides) but never
/// carries sole semantics.
// A style-attribute bag is naturally bool-shaped; a hand-rolled bitfield
// would buy nothing here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modifiers {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikethrough: bool,
}

/// One styled run: colors by slot or RGB plus modifiers. `None` colors leave
/// the renderer's default in place (the terminal's own fg/bg).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub mods: Modifiers,
}

/// A styled text run within a [`Line`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    /// A span with the default style.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::default(),
        }
    }

    /// A span with one foreground slot and no modifiers.
    pub fn slotted(text: impl Into<String>, slot: Slot) -> Self {
        Self {
            text: text.into(),
            style: Style {
                fg: Some(Color::Slot(slot)),
                ..Style::default()
            },
        }
    }
}

/// One logical line: an ordered run of spans. Wrapping into display rows is
/// the renderer's concern at render width; the pipeline's flush invariants
/// (ADR-0018 item 4) are stated over these logical lines.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    #[must_use]
    pub fn from_spans(spans: Vec<Span>) -> Self {
        Self { spans }
    }

    /// A single-span, unstyled line.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            spans: vec![Span::plain(text)],
        }
    }

    /// The display width of the line's text (styles are zero-width).
    #[must_use]
    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.text.as_str()))
            .sum()
    }

    /// The concatenated text without styling (width math, plain fallbacks).
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}
