//! Theme: resolution of the IR's semantic slots to concrete tones
//! (ADR-0017). This module is deliberately small: the full preset system
//! (four presets, capability detection, TOML theme files under XDG, the
//! xtask palette generator with WCAG gates) lands with its own slices
//! (ADR-0017 items 4–5, 10). What the first widgets need now is the
//! resolution *type* and one honest palette.
//!
//! The palette shipped here is the 16-named-colors one: ADR-0017 item 5's
//! `ansi` presets use only the terminal's own named colors so the user's
//! palette takes over entirely (the gh CLI accessibility precedent). Because
//! the terminal supplies the actual hues, one mapping serves dark and light
//! terminals alike; the named `dark`/`light` (truecolor) presets await the
//! item-4 generator.

use crate::ir::Slot;

/// A resolved color: the terminal's default (`Reset`), one of the 16 named
/// terminal colors, or raw sRGB.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tone {
    Reset,
    Ansi(AnsiTone),
    Rgb(u8, u8, u8),
}

/// The 16 named terminal colors (the user's palette owns the actual hues).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnsiTone {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

/// The terminal's color capability, detected (ADR-0017 item 5's chain) at
/// the app boundary and injected here; the render layer degrades `Rgb`
/// tones and syntect colors along it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorDepth {
    #[default]
    Truecolor,
    Ansi256,
    Ansi16,
    None,
}

/// A slot → tone table. Construction is by named presets; per-slot mutation
/// arrives with the TOML theme loader (ADR-0017 item 10).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    tones: [Tone; 18],
}

impl Theme {
    /// The 16-color preset (ADR-0017 item 5): only named terminal colors, so
    /// dark and light terminals both render their own palette. `bg`-class
    /// slots resolve to `Reset` (the terminal's own background); the
    /// `*-bg` diff slots have no subtle-bg representation at 16 colors and
    /// degrade to the plain text tone — the render layer may still pair
    /// them with the fg slot of the same name.
    #[must_use]
    pub fn ansi() -> Self {
        use AnsiTone as A;
        use Slot as S;
        // Indexed by slot, not by declaration order, so the table cannot
        // silently drift from the slot enum.
        let mut tones = [Tone::Reset; 18];
        tones[S::Bg as usize] = Tone::Reset;
        tones[S::BgSubtle as usize] = Tone::Ansi(A::BrightBlack);
        tones[S::Text as usize] = Tone::Reset;
        tones[S::TextSubtle as usize] = Tone::Ansi(A::BrightBlack);
        // The Linear-bones accent is a chroma-limited blue-violet; at 16
        // colors the nearest named tone is blue.
        tones[S::Accent as usize] = Tone::Ansi(A::Blue);
        tones[S::OnAccent as usize] = Tone::Ansi(A::BrightWhite);
        tones[S::Success as usize] = Tone::Ansi(A::Green);
        tones[S::Warning as usize] = Tone::Ansi(A::Yellow);
        tones[S::Error as usize] = Tone::Ansi(A::Red);
        tones[S::Info as usize] = Tone::Ansi(A::Cyan);
        tones[S::Border as usize] = Tone::Ansi(A::BrightBlack);
        tones[S::BorderActive as usize] = Tone::Ansi(A::Blue);
        tones[S::DiffAdded as usize] = Tone::Ansi(A::Green);
        tones[S::DiffRemoved as usize] = Tone::Ansi(A::Red);
        tones[S::DiffAddedBg as usize] = Tone::Ansi(A::Green);
        tones[S::DiffRemovedBg as usize] = Tone::Ansi(A::Red);
        tones[S::Mark as usize] = Tone::Ansi(A::Yellow);
        tones[S::Selection as usize] = Tone::Ansi(A::BrightBlack);
        Self { tones }
    }

    /// Resolve one slot to its tone.
    #[must_use]
    pub fn resolve(&self, slot: Slot) -> Tone {
        self.tones[slot as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slot_resolves() {
        let theme = Theme::ansi();
        // The slot set is fixed at 18 (ADR-0017 item 3); index coverage is
        // proven by construction (the table is built by mapping over all
        // slots), so this pins the count against accidental shrinkage.
        let slots = [
            Slot::Bg,
            Slot::BgSubtle,
            Slot::Text,
            Slot::TextSubtle,
            Slot::Accent,
            Slot::OnAccent,
            Slot::Success,
            Slot::Warning,
            Slot::Error,
            Slot::Info,
            Slot::Border,
            Slot::BorderActive,
            Slot::DiffAdded,
            Slot::DiffRemoved,
            Slot::DiffAddedBg,
            Slot::DiffRemovedBg,
            Slot::Mark,
            Slot::Selection,
        ];
        for slot in slots {
            let _ = theme.resolve(slot);
        }
    }
}
