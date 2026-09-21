// SPDX-License-Identifier: Apache-2.0
// OpenAI Codex, Copyright 2025 OpenAI.
// Derived portions also carry: Copyright (c) 2016-2022 Florian Dehau;
// Copyright (c) 2023-2025 The Ratatui Developers (MIT).
// Modified for Cadmus: pre-wrapped rows, stock Terminal integration,
// injected strategy, control filtering and error cleanup. See LICENSE-APACHE.
//
//! History insertion adapted from Codex's `insert_history.rs` and
//! `tui/scrollback.rs`: write text, not buffer continuation cells, and use
//! CRLF rather than CSI S to preserve native scrollback (ADR-0018).

use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Print, ResetColor, SetStyle};
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::IntoCrossterm;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::wrap::rewrap_rows;

/// Injected at terminal boot: partial margins lose departing rows on
/// Windows Terminal. Cadmus always pre-wraps, so Zellij uses the standard
/// path (Codex's Zellij exception is for terminal-managed wrapping).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollbackStrategy {
    Standard,
    FullScreen,
}

impl ScrollbackStrategy {
    #[must_use]
    pub fn for_terminal(zellij: bool, windows_terminal: bool) -> Self {
        if windows_terminal && !zellij {
            Self::FullScreen
        } else {
            Self::Standard
        }
    }

    /// Environment reads belong at the terminal boundary; tests inject the
    /// strategy instead of changing process-global environment variables.
    #[must_use]
    pub fn detect() -> Self {
        Self::for_terminal(
            std::env::var_os("ZELLIJ").is_some(),
            std::env::var_os("WT_SESSION").is_some()
                || std::env::var("TERM_PROGRAM").is_ok_and(|name| name == "Windows_Terminal"),
        )
    }
}

/// Returns the new band origin. The shell owns cursor synchronization and
/// invalidating ratatui's buffers; no raw cursor state escapes this operation.
pub(crate) fn insert(
    writer: &mut impl Write,
    rows: &[Line<'_>],
    mut area: Rect,
    screen: Size,
    strategy: ScrollbackStrategy,
) -> io::Result<Rect> {
    let rows = rewrap_rows(rows, screen.width.max(1));
    area.height = area.height.min(screen.height);
    area.y = area.y.min(screen.height.saturating_sub(area.height));
    let old_top = area.y;
    let available = screen.height.saturating_sub(area.bottom());
    let pushed = rows.len().min(usize::from(available));
    area.y += u16::try_from(pushed).unwrap_or(available);

    let result = (|| {
        // The old band will be repainted within the same synchronized update.
        // Clear it before scrolling so composer text cannot enter history.
        queue!(writer, ResetColor)?;
        clear_below(writer, old_top, screen.height)?;
        queue!(writer, MoveTo(0, old_top))?;
        if strategy == ScrollbackStrategy::FullScreen || area.y < 2 {
            // DECSTBM needs two distinct rows; a full-height band (or just
            // one row of history) therefore also uses full-screen scrolling.
            for (index, row) in rows.iter().enumerate() {
                if index > 0 {
                    writer.write_all(b"\r\n")?;
                }
                write_row(writer, row)?;
            }
            for _ in 0..area.height {
                queue!(writer, Print("\r\n"), Clear(ClearType::UntilNewLine))?;
            }
        } else {
            // Consume the blank buffer below the band first. The band is
            // cleared already, so direct writes replace Codex's Reverse Index
            // shift without copying stale composer cells or losing row zero.
            for (index, row) in rows[..pushed].iter().enumerate() {
                if index > 0 {
                    writer.write_all(b"\r\n")?;
                }
                write_row(writer, row)?;
            }
            if pushed < rows.len() {
                write!(writer, "\x1b[1;{}r", area.y)?;
                queue!(writer, MoveTo(0, area.y - 1))?;
                for row in &rows[pushed..] {
                    writer.write_all(b"\r\n")?;
                    write_row(writer, row)?;
                }
            }
        }
        Ok(())
    })();
    // Try cleanup even after a failed payload write. A leaked margin would
    // constrain subsequent shell output after this session has exited.
    let cleanup = reset(writer);
    result.and(cleanup)?;
    Ok(area)
}

/// Make room when a shorter physical screen moves the band upwards.
/// Scroll the history that would be covered, rather than clearing it. Rows
/// already removed by terminal reflow are outside the shell's control.
pub(crate) fn scroll_history(
    writer: &mut impl Write,
    history_bottom: u16,
    screen_height: u16,
    count: u16,
    strategy: ScrollbackStrategy,
) -> io::Result<()> {
    let result = (|| {
        queue!(writer, ResetColor)?;
        if strategy == ScrollbackStrategy::Standard && history_bottom > 1 {
            write!(writer, "\x1b[1;{history_bottom}r")?;
            queue!(writer, MoveTo(0, history_bottom - 1))?;
        } else {
            if history_bottom < screen_height {
                clear_below(writer, history_bottom, screen_height)?;
            }
            queue!(writer, MoveTo(0, screen_height.saturating_sub(1)))?;
        }
        for _ in 0..count {
            writer.write_all(b"\r\n")?;
        }
        Ok(())
    })();
    let cleanup = reset(writer);
    result.and(cleanup)
}

fn reset(writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(b"\x1b[r")?;
    queue!(writer, ResetColor)?;
    writer.flush()
}

/// tmux's default scroll-on-clear copies the visible screen into history
/// for an origin-position ED. Clear rows individually at the origin so a
/// transient composer never becomes permanent scrollback.
pub(crate) fn clear_below(writer: &mut impl Write, top: u16, height: u16) -> io::Result<()> {
    if top == 0 {
        for row in top..height {
            queue!(writer, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }
    } else {
        queue!(writer, MoveTo(0, top), Clear(ClearType::FromCursorDown))?;
    }
    Ok(())
}

fn write_row(writer: &mut impl Write, row: &Line<'_>) -> io::Result<()> {
    queue!(writer, Clear(ClearType::UntilNewLine))?;
    for span in &row.spans {
        // A reset before each span prevents modifiers (notably bold/dim)
        // leaking across style boundaries. Resolve removals before converting:
        // crossterm's NormalIntensity clears both bold and dim at once.
        let style = row.style.patch(span.style);
        let style = Style {
            sub_modifier: Modifier::empty(),
            ..style
        };
        queue!(
            writer,
            ResetColor,
            SetStyle(style.into_crossterm()),
            Print(&span.content)
        )?;
    }
    queue!(writer, ResetColor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_strategy_preserves_the_upstream_pre_wrapped_precedence() {
        assert_eq!(
            ScrollbackStrategy::for_terminal(false, false),
            ScrollbackStrategy::Standard
        );
        assert_eq!(
            ScrollbackStrategy::for_terminal(false, true),
            ScrollbackStrategy::FullScreen
        );
        assert_eq!(
            ScrollbackStrategy::for_terminal(true, false),
            ScrollbackStrategy::Standard
        );
        assert_eq!(
            ScrollbackStrategy::for_terminal(true, true),
            ScrollbackStrategy::Standard
        );
    }
}
