//! Spike for ADR-0018 item 3: does stock ratatui 0.30 `Viewport::Inline` +
//! `Terminal::insert_before` carry the inline-rendering mechanism, or does
//! the combination Codex needed (dynamic height, escape-sequence history
//! writes, per-terminal scroll strategies) force a thin derived `Terminal`?
//!
//! Facts already established by reading ratatui-core 0.1.2 (no run needed):
//!
//! - F1: the inline viewport's height is fixed at creation — there is no
//!   `set_viewport`/height-mutation API. `resize` recomputes the anchor but
//!   keeps the height. Layouts must be fixed-height (or recreate the
//!   `Terminal` on height change — untested here).
//! - F2: `insert_before`'s default path (without the `scrolling-regions`
//!   feature) avoids DEC scroll regions entirely: direct cell draws +
//!   `append_lines` + viewport clear, repainted by the next `draw`. That is
//!   the maximally portable path — Windows Terminal's known quirk is dropping
//!   lines under *partial DEC scroll regions*, which this path never emits.
//! - F3: `resize` on an inline viewport re-anchors via a DA cursor-position
//!   query (stdin round-trip) — a latency and failure surface under quirky
//!   terminals and resize storms. The event loop owes it debounce (Codex:
//!   75 ms) and error tolerance on *every* path that can issue the query —
//!   including `draw`, whose built-in `autoresize` re-anchors too (observed:
//!   a storm killed this harness through the draw path). Both are event-loop
//!   policy, not a reason to fork.
//! - F4: re-anchoring scrolls the terminal to keep the viewport fully
//!   visible, so each processed resize leaves the previous frame as residue
//!   in scrollback. Debounce bounds it to ≤1 stale frame per drag gesture;
//!   eliminating it is one of the fork's real advantages (Codex owns
//!   `viewport_area` instead of re-deriving it from the cursor).
//!
//! What remains is behavioral, and needs a real terminal — run this example
//! and walk the acceptance matrix (the verdict amends ADR-0018 item 3):
//!
//! 1. **Streaming while history inserts above** — the active area grows a
//!    chunk every 80 ms; every 12 chunks (or `c`) the turn completes and its
//!    rows insert above. Pass: no duplicated, dropped or garbled rows, the
//!    viewport stays anchored, inserts are flicker-free under the 2026h
//!    synchronized-update guard.
//! 2. **Resize reflow at narrow widths** — shrink the window narrower
//!    mid-stream. Stock ratatui clears the *visible* screen on horizontal
//!    shrink; this harness then re-materializes the on-screen history tail
//!    from its own transcript (the event stream in the real TUI). Pass: the
//!    visible view is correct afterwards. Note, not a failure: scrollback
//!    duplication across shrinks — stock ratatui cannot delete its own
//!    scrollback rows (Codex's DEC-row-delete is the unportable trick).
//! 3. **Terminal quirks** — run in Windows Terminal, Zellij, tmux and one
//!    plain xterm-class terminal; record behavior per terminal.
//!
//! Controls: `c` completes the active turn early, `q` quits and prints a
//! diagnostic summary (terminal identity env vars + counters) — paste it
//! into the ADR-0018 amendment.

use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
};
use crossterm::{event, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Fixed active-area height — see spike fact F1 in the module docs.
const VIEWPORT_HEIGHT: u16 = 8;
/// Typewriter cadence of the simulated stream.
const TICK: Duration = Duration::from_millis(80);
/// Auto-complete the active turn after this many streamed chunks.
const TURN_CHUNKS: usize = 12;
/// Wrap one cell short of the full width so no row can hit the terminal's
/// auto-wrap and desync the row bookkeeping (Codex pre-wraps history lines
/// for the same reason).
const WRAP_SLACK: usize = 1;

#[derive(Default)]
struct Stats {
    turns: usize,
    inserts: usize,
    inserted_rows: usize,
    resizes: usize,
    resize_errors: usize,
    draw_errors: usize,
    shrink_replays: usize,
}

struct Harness {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    /// Logical (unwrapped) lines of completed turns — the stand-in for the
    /// event stream: reflow re-materializes from here, never from cells.
    transcript: Vec<String>,
    /// Streamed chunks of the open turn.
    active: Vec<String>,
    turn_no: usize,
    width: u16,
    stats: Stats,
}

fn main() -> io::Result<()> {
    let identity = terminal_identity();
    let Some(mut harness) = Harness::enter()? else {
        println!("inline_spike needs a real terminal (raw mode + size query failed)");
        return Ok(());
    };
    let outcome = harness.run();
    let stats = harness.exit();
    println!("inline_spike diagnostics");
    println!("  terminal: {identity}");
    println!(
        "  turns: {}, insert_before calls: {}, rows inserted: {}",
        stats.turns, stats.inserts, stats.inserted_rows
    );
    println!(
        "  resizes: {} (shrink replays: {}, resize errors tolerated: {}, draw errors tolerated: {})",
        stats.resizes, stats.shrink_replays, stats.resize_errors, stats.draw_errors
    );
    println!(
        "verdict checklist: [ ] streaming clean  [ ] shrink reflow correct  [ ] scrollback state noted"
    );
    outcome
}

/// Identifies the terminal for the verdict record: Windows Terminal sets
/// `WT_SESSION`, Zellij sets `ZELLIJ`, tmux sets `TMUX`.
fn terminal_identity() -> String {
    [
        "TERM_PROGRAM",
        "TERM",
        "COLORTERM",
        "WT_SESSION",
        "ZELLIJ",
        "TMUX",
        "WEZTERM_PANE",
    ]
    .into_iter()
    .filter_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| format!("{name}={value}"))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

impl Harness {
    fn enter() -> io::Result<Option<Self>> {
        if enable_raw_mode().is_err() {
            return Ok(None);
        }
        let Ok((width, _height)) = crossterm::terminal::size() else {
            disable_raw_mode()?;
            return Ok(None);
        };
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT_HEIGHT),
            },
        )?;
        terminal.clear()?;
        Ok(Some(Self {
            terminal,
            transcript: Vec::new(),
            active: Vec::new(),
            turn_no: 1,
            width,
            stats: Stats::default(),
        }))
    }

    fn run(&mut self) -> io::Result<()> {
        loop {
            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') => self.complete_turn()?,
                        _ => {}
                    },
                    Event::Resize(width, height) => self.on_resize(width, height)?,
                    _ => {}
                }
            } else {
                self.stream_tick()?;
            }
        }
        Ok(())
    }

    fn exit(&mut self) -> &Stats {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
        &self.stats
    }

    /// One typewriter step: append a chunk to the open turn and repaint the
    /// active area. Turns auto-complete at `TURN_CHUNKS` chunks.
    fn stream_tick(&mut self) -> io::Result<()> {
        let chunk = self.active.len() + 1;
        self.active.push(format!(
            "T{:02}·C{chunk:02} — the quick brown fox jumps over the lazy dog {}",
            self.turn_no,
            "x".repeat(self.turn_no * 4),
        ));
        if self.active.len() >= TURN_CHUNKS {
            self.complete_turn()
        } else {
            self.draw_active();
            Ok(())
        }
    }

    /// The mechanism under test: completed-turn rows leave the viewport into
    /// real scrollback via `insert_before`, inside a 2026h guard so the
    /// portable path's clear+repaint never becomes visible.
    fn complete_turn(&mut self) -> io::Result<()> {
        if self.active.is_empty() {
            return Ok(());
        }
        let rows = wrap_lines(
            &self.active,
            usize::from(self.width).saturating_sub(WRAP_SLACK),
        );
        self.transcript.append(&mut self.active);
        self.insert_rows(&rows)?;
        self.stats.turns += 1;
        self.turn_no += 1;
        Ok(())
    }

    fn on_resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        let shrunk = width < self.width;
        self.stats.resizes += 1;
        self.width = width;
        let mut out = io::stdout();
        execute!(out, BeginSynchronizedUpdate)?;
        // For inline viewports `resize` takes the new terminal size and
        // recomputes the anchor from the cursor row. The re-anchor is a DA
        // cursor-position round-trip (spike fact F3): tolerate its failure —
        // the next resize or draw re-anchors — instead of dying mid-storm.
        if self
            .terminal
            .resize(Rect::new(0, 0, width, height))
            .is_err()
        {
            self.stats.resize_errors += 1;
            execute!(out, EndSynchronizedUpdate)?;
            return Ok(());
        }
        if shrunk {
            // Stock ratatui cleared the visible screen (horizontal shrink);
            // re-materialize the on-screen history tail from the transcript.
            let visible_history = usize::from(height.saturating_sub(VIEWPORT_HEIGHT));
            let wrapped = wrap_lines(
                &self.transcript,
                usize::from(width).saturating_sub(WRAP_SLACK),
            );
            let skip = wrapped.len().saturating_sub(visible_history);
            let tail: Vec<Line<'_>> = wrapped.into_iter().skip(skip).collect();
            if !tail.is_empty() {
                self.insert_rows(&tail)?;
                self.stats.shrink_replays += 1;
            }
        }
        self.draw_active();
        execute!(out, EndSynchronizedUpdate)?;
        Ok(())
    }

    /// Insert pre-wrapped rows above the viewport and repaint it (the
    /// portable `insert_before` path clears the viewport on its way out).
    fn insert_rows(&mut self, rows: &[Line<'_>]) -> io::Result<()> {
        let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
        let mut out = io::stdout();
        execute!(out, BeginSynchronizedUpdate)?;
        self.terminal.insert_before(height, |buf| {
            Paragraph::new(rows.to_vec()).render(buf.area, buf);
        })?;
        self.stats.inserts += 1;
        self.stats.inserted_rows += rows.len();
        self.draw_active();
        execute!(out, EndSynchronizedUpdate)?;
        Ok(())
    }

    /// Repaint the active area. `draw` re-anchors inline viewports on size
    /// change via a cursor-position query (spike fact F3) — tolerate its
    /// failure like the explicit `resize` path: the next tick repaints.
    fn draw_active(&mut self) {
        if self.try_draw_active().is_err() {
            self.stats.draw_errors += 1;
        }
    }

    fn try_draw_active(&mut self) -> io::Result<()> {
        let active = &self.active;
        let stats = &self.stats;
        let width = self.width;
        self.terminal.draw(|frame| {
            let area = frame.area();
            let content_rows = usize::from(area.height).saturating_sub(2);
            let wrapped = wrap_lines(active, usize::from(width).saturating_sub(WRAP_SLACK));
            let scroll = wrapped.len().saturating_sub(content_rows);
            let mut lines = vec![
                Line::from("inline_spike — c: complete turn · q: quit"),
                Line::from(format!(
                    "turns {} · inserts {} · resizes {}",
                    stats.turns, stats.inserts, stats.resizes
                )),
            ];
            lines.extend(wrapped.into_iter().skip(scroll));
            frame.render_widget(Paragraph::new(lines), area);
        })?;
        Ok(())
    }
}

/// Hard wrap by display width (grapheme-correct, word boundaries ignored):
/// the same wrapper feeds the insert path and the live area, so the row
/// math matches what the renderer produces 1:1.
fn wrap_lines(lines: &[String], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    lines
        .iter()
        .flat_map(|line| wrap_line(line, width))
        .map(Line::from)
        .collect()
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for grapheme in UnicodeSegmentation::graphemes(line, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if current_width + grapheme_width > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(grapheme);
        current_width += grapheme_width;
    }
    rows.push(current);
    rows
}
