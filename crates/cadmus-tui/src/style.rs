//! Mapping `cadmus-ui`'s semantic-style IR onto ratatui styles (ADR-0018
//! item 2): slots resolve through the theme, and colors degrade along the
//! terminal's injected [`ColorDepth`] — ADR-0017 item 5: ratatui does not
//! degrade `Rgb` safely on its own, so the mapping is ours. Detection of the
//! depth (the item-5 chain) is app-boundary IO and lands with the app; these
//! functions are pure.

use cadmus_ui::ir;
use cadmus_ui::theme::{AnsiTone, ColorDepth, Theme, Tone};
use ratatui::style::{Color, Modifier, Style};

/// The color-depth detection chain (ADR-0017 item 5), app-boundary IO:
/// `NO_COLOR` set to anything drops color (no-color.org: presence is the
/// signal); `COLORTERM` truecolor/24bit grants the full palette; a
/// `256color` `TERM` the xterm cube; anything else the 16 named colors.
#[must_use]
pub fn detect_depth() -> ColorDepth {
    if std::env::var_os("NO_COLOR").is_some() {
        return ColorDepth::None;
    }
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    if matches!(colorterm.as_str(), "truecolor" | "24bit") {
        return ColorDepth::Truecolor;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term.contains("256color") {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
}

/// The motion-profile detection (the 2026-09-20 second amendment),
/// app-boundary IO like [`detect_depth`]: `TERM=dumb` disables the paced
/// typewriter (instant emission — a terminal that cannot move a cursor
/// gets no animation), everything else paces. The full `full|reduced|none`
/// profile lands with the item-7 TOML loader (docs/open-items.md).
#[must_use]
pub fn detect_paced() -> bool {
    std::env::var("TERM").is_ok_and(|term| term != "dumb")
}

/// Resolve an IR style to a ratatui style under the theme and color depth.
#[must_use]
pub fn ir_style(style: &ir::Style, theme: &Theme, depth: ColorDepth) -> Style {
    let mut out = Style::default();
    if let Some(fg) = style.fg {
        out = out.fg(ir_color(fg, theme, depth));
    }
    if let Some(bg) = style.bg {
        out = out.bg(ir_color(bg, theme, depth));
    }
    let mods = style.mods;
    let mut modifier = Modifier::empty();
    if mods.bold {
        modifier |= Modifier::BOLD;
    }
    if mods.dim {
        modifier |= Modifier::DIM;
    }
    if mods.italic {
        modifier |= Modifier::ITALIC;
    }
    if mods.underline {
        modifier |= Modifier::UNDERLINED;
    }
    if mods.inverse {
        modifier |= Modifier::REVERSED;
    }
    if mods.strikethrough {
        modifier |= Modifier::CROSSED_OUT;
    }
    out.add_modifier(modifier)
}

/// An IR color: slots resolve through the theme; RGB degrades.
fn ir_color(color: ir::Color, theme: &Theme, depth: ColorDepth) -> Color {
    match color {
        ir::Color::Slot(slot) => tone_color(theme.resolve(slot), depth),
        ir::Color::Rgb(r, g, b) => degrade_rgb(r, g, b, depth),
    }
}

/// A theme tone: named colors pass through unchanged at any depth (the
/// terminal's palette owns the hues); `Rgb` degrades.
fn tone_color(tone: Tone, depth: ColorDepth) -> Color {
    match tone {
        Tone::Reset => Color::Reset,
        Tone::Ansi(ansi) => ansi_color(ansi),
        Tone::Rgb(r, g, b) => degrade_rgb(r, g, b, depth),
    }
}

/// The 16 named terminal colors (ratatui's names for the same set).
fn ansi_color(tone: AnsiTone) -> Color {
    match tone {
        AnsiTone::Black => Color::Black,
        AnsiTone::Red => Color::Red,
        AnsiTone::Green => Color::Green,
        AnsiTone::Yellow => Color::Yellow,
        AnsiTone::Blue => Color::Blue,
        AnsiTone::Magenta => Color::Magenta,
        AnsiTone::Cyan => Color::Cyan,
        AnsiTone::White => Color::Gray,
        AnsiTone::BrightBlack => Color::DarkGray,
        AnsiTone::BrightRed => Color::LightRed,
        AnsiTone::BrightGreen => Color::LightGreen,
        AnsiTone::BrightYellow => Color::LightYellow,
        AnsiTone::BrightBlue => Color::LightBlue,
        AnsiTone::BrightMagenta => Color::LightMagenta,
        AnsiTone::BrightCyan => Color::LightCyan,
        AnsiTone::BrightWhite => Color::White,
    }
}

/// Degrade an sRGB color along the depth (ADR-0017 item 5): truecolor
/// passes, 256 colors get the xterm cube/ramp index, 16 colors the nearest
/// named VGA tone by Euclidean distance, and `None` drops color entirely
/// (modifiers still apply — `NO_COLOR` kills color, not emphasis).
#[must_use]
pub fn degrade_rgb(r: u8, g: u8, b: u8, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::Truecolor => Color::Rgb(r, g, b),
        ColorDepth::Ansi256 => Color::Indexed(rgb_to_256(r, g, b)),
        ColorDepth::Ansi16 => nearest_vga(r, g, b),
        ColorDepth::None => Color::Reset,
    }
}

/// The xterm 256-color mapping: the 6x6x6 color cube, with the grayscale
/// ramp for near-grays (the cube's own grays are too coarse).
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let cube = |v: u8| u8::try_from((u32::from(v) * 5 + 127) / 255).unwrap_or(5);
    let (cr, cg, cb) = (cube(r), cube(g), cube(b));
    // Near-gray detection: the channels agree within one cube step.
    if cr == cg && cg == cb {
        let gray_index = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
        if gray_index < 8 {
            return 16; // cube black
        }
        if gray_index > 238 {
            return 255; // ramp white
        }
        return u8::try_from(232 + (gray_index - 8) / 10).unwrap_or(255);
    }
    16 + 36 * cr + 6 * cg + cb
}

/// The VGA palette behind the 16 named colors, in [`AnsiTone`] order.
const VGA: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // black
    (170, 0, 0),     // red
    (0, 170, 0),     // green
    (170, 85, 0),    // yellow
    (0, 0, 170),     // blue
    (170, 0, 170),   // magenta
    (0, 170, 170),   // cyan
    (170, 170, 170), // white (gray)
    (85, 85, 85),    // bright black (dark gray)
    (255, 85, 85),   // bright red
    (85, 255, 85),   // bright green
    (255, 255, 85),  // bright yellow
    (85, 85, 255),   // bright blue
    (255, 85, 255),  // bright magenta
    (85, 255, 255),  // bright cyan
    (255, 255, 255), // bright white
];

/// The nearest named VGA tone by squared Euclidean distance.
fn nearest_vga(r: u8, g: u8, b: u8) -> Color {
    let dist = |i: usize| {
        let (vr, vg, vb) = VGA[i];
        let (dr, dg, db) = (
            i32::from(r) - i32::from(vr),
            i32::from(g) - i32::from(vg),
            i32::from(b) - i32::from(vb),
        );
        dr * dr + dg * dg + db * db
    };
    let nearest = (0..16).min_by_key(|&i| dist(i)).unwrap_or(7);
    ansi_color(match nearest {
        0 => AnsiTone::Black,
        1 => AnsiTone::Red,
        2 => AnsiTone::Green,
        3 => AnsiTone::Yellow,
        4 => AnsiTone::Blue,
        5 => AnsiTone::Magenta,
        6 => AnsiTone::Cyan,
        7 => AnsiTone::White,
        8 => AnsiTone::BrightBlack,
        9 => AnsiTone::BrightRed,
        10 => AnsiTone::BrightGreen,
        11 => AnsiTone::BrightYellow,
        12 => AnsiTone::BrightBlue,
        13 => AnsiTone::BrightMagenta,
        14 => AnsiTone::BrightCyan,
        _ => AnsiTone::BrightWhite,
    })
}

#[cfg(test)]
mod tests {
    use cadmus_ui::ir::{Modifiers, Slot, Span};

    use super::*;

    #[test]
    fn slots_resolve_through_the_theme() {
        let theme = Theme::ansi();
        let style = ir_style(
            &Span::slotted("x", Slot::Error).style,
            &theme,
            ColorDepth::Truecolor,
        );
        assert_eq!(style.fg, Some(Color::Red));
    }

    #[test]
    fn modifiers_map_across() {
        let theme = Theme::ansi();
        let ir = ir::Style {
            fg: None,
            bg: None,
            mods: Modifiers {
                bold: true,
                italic: true,
                strikethrough: true,
                ..Modifiers::default()
            },
        };
        let style = ir_style(&ir, &theme, ColorDepth::Truecolor);
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn rgb_degrades_along_the_depth() {
        let (r, g, b) = (0x5f, 0x5f, 0x87);
        assert_eq!(
            degrade_rgb(r, g, b, ColorDepth::Truecolor),
            Color::Rgb(r, g, b)
        );
        assert!(matches!(
            degrade_rgb(r, g, b, ColorDepth::Ansi256),
            Color::Indexed(_)
        ));
        assert!(matches!(
            degrade_rgb(r, g, b, ColorDepth::Ansi16),
            Color::Blue | Color::DarkGray | Color::LightBlue
        ));
        assert_eq!(degrade_rgb(r, g, b, ColorDepth::None), Color::Reset);
    }

    #[test]
    fn pure_black_and_white_land_on_the_ramp_ends() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 255);
    }

    #[test]
    fn nearest_vga_picks_the_expected_tone() {
        assert_eq!(nearest_vga(255, 90, 90), Color::LightRed);
        assert_eq!(nearest_vga(10, 10, 10), Color::Black);
        assert_eq!(nearest_vga(200, 200, 200), Color::Gray);
        assert_eq!(nearest_vga(240, 240, 240), Color::White);
    }
}
