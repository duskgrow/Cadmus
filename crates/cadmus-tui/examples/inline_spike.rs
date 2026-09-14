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
//! Dynamic-height probe (2026-09-14): `g`/`s` grow/shrink the band via the
//! recreation protocol (blank-insert, park cursor, recreate the `Terminal` —
//! the deterministic row bookkeeping is locked by
//! `tests/dynamic_height_spike.rs`). What the manual matrix judges here is
//! what vt100 cannot: compositing. Judging a negative (no flicker, no
//! residue) needs a reference for what the failure looks like, so the probe
//! ships its own positive controls: `G` grows naively to produce the
//! stale-band residue the protocol prevents, `F` shrinks slowly without the
//! 2026h guard to produce the flicker it prevents, and `f` replays `F`
//! guarded — pass means `g` looks nothing like `G` and `f` nothing like `F`.
//!
//! Controls: `c` completes the active turn early, `q` quits and prints a
//! diagnostic summary (terminal identity env vars + counters) — paste it
//! into the ADR-0018 amendment. The startup legend above the band lists the
//! per-key expectations.
//!
//! Machine-verifiable evidence (2026-09-14): eyewitness reports are a lossy
//! channel, so every run tees the raw output stream to
//! `target/inline-spike/capture-<ts>.bin` plus a `.txt` sidecar (initial
//! size, terminal identity, resize events with byte offsets, the full key
//! log, and the expected final row sequence). `inline_spike_replay` then
//! replays the capture through vt100 and diffs it against the sidecar model
//! — the matrix verdict is computed, not described. The sentinel protocol
//! (tap `x` around each height key) makes swallowed-input races (upstream
//! #2640) visible in the key log. Keep the window size fixed during a
//! dynamic-height leg: the sidecar model wraps at the final width, so
//! mid-leg resizes make the model unreliable (resize reflow is the earlier
//! matrix's topic, recorded in ADR-0018).

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    BeginSynchronizedUpdate, EndSynchronizedUpdate, disable_raw_mode, enable_raw_mode,
};
use crossterm::{event, execute};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Initial active-area height — see spike fact F1 in the module docs. The
/// grow/shrink keys change it at runtime via Terminal recreation.
const VIEWPORT_HEIGHT: u16 = 8;
/// Grow/shrink step of the dynamic-height probe.
const HEIGHT_STEP: u16 = 4;
/// Smallest band the shrink key allows (status + one content row).
const MIN_BAND_HEIGHT: u16 = 2;

/// The probe legend: printed into scrollback above the band at startup and
/// embedded in the sidecar (the replay model starts with it). Every line
/// must stay ≤ 72 columns — a wrapped legend line desyncs the replay model
/// (the model never wraps it; learned from the first Zed capture).
const LEGEND: &[&str] = &[
    "inline_spike — viewport + dynamic-height probe; rows numbered Txx·Cyy",
    "  g grow +4 (protocol)   expect: one atomic jump, numbers continuous",
    "  G grow +4 (naive)      expect: GARBAGE above band (residue reference)",
    "  s shrink −4 (protocol) expect: ≤4 blank rows, eaten by later output",
    "  f shrink −4 slow+2026h expect: still one atomic jump (guard holds)",
    "  F shrink −4 slow naked expect: visible blank flash (flicker reference)",
    "  sentinel: tap x before and after each height key (swallow check)",
    "  keep the window size fixed in this leg; ≥80 columns wide",
    "  c complete turn · q quit + diagnostics",
];

/// Duplicates the raw output stream into a capture file while passing it
/// through to the real terminal — the tee is what makes a matrix run
/// replayable and therefore machine-verifiable.
#[derive(Clone)]
struct Tee {
    inner: Rc<RefCell<TeeInner>>,
}

struct TeeInner {
    file: fs::File,
    offset: u64,
}

impl Tee {
    fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: Rc::new(RefCell::new(TeeInner {
                file: fs::File::create(path)?,
                offset: 0,
            })),
        })
    }

    /// Bytes written so far — resize events are logged against this offset so
    /// the replay can apply them at the exact stream position.
    fn offset(&self) -> u64 {
        self.inner.borrow().offset
    }

    fn write_legend_line(&mut self, line: &str) -> io::Result<()> {
        // `\r\n` so the capture replays correctly (bare `\n` staircases under
        // vt100) — harmless on the cooked-mode terminal this prints to.
        self.write_all(format!("{line}\r\n").as_bytes())
    }
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write_all(buf)?;
        let mut inner = self.inner.borrow_mut();
        inner.file.write_all(buf)?;
        inner.offset += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()?;
        self.inner.borrow_mut().file.flush()
    }
}
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
    grows: usize,
    naive_grows: usize,
    shrinks: usize,
    slow_guarded_shrinks: usize,
    slow_unguarded_shrinks: usize,
}

struct Harness {
    terminal: Terminal<CrosstermBackend<Tee>>,
    tee: Tee,
    /// Capture stem (`target/inline-spike/capture-<ts>`); the sidecar is the
    /// same stem with `.txt`.
    capture_stem: PathBuf,
    identity: String,
    initial_rows: u16,
    initial_cols: u16,
    /// Every key press in order — the sentinel protocol's evidence.
    keys: Vec<String>,
    /// (stream offset, rows, cols) per resize event, for the replay.
    resize_log: Vec<(u64, u16, u16)>,
    /// Logical (unwrapped) lines of completed turns — the stand-in for the
    /// event stream: reflow re-materializes from here, never from cells.
    transcript: Vec<String>,
    /// Streamed chunks of the open turn.
    active: Vec<String>,
    turn_no: usize,
    width: u16,
    band_height: u16,
    stats: Stats,
}

fn main() -> io::Result<()> {
    let identity = terminal_identity();
    let dir = Path::new("target/inline-spike");
    fs::create_dir_all(dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let stem = dir.join(format!("capture-{stamp}"));
    // The probe legend lives in scrollback above the band: expectations stay
    // visible (and reviewable by scrolling) for the whole run.
    let mut tee = Tee::create(&stem.with_extension("bin"))?;
    for line in LEGEND {
        tee.write_legend_line(line)?;
    }
    let Some(mut harness) = Harness::enter(tee, identity.clone(), stem)? else {
        println!("inline_spike needs a real terminal (raw mode + size query failed)");
        return Ok(());
    };
    let outcome = harness.run();
    harness.write_sidecar()?;
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
        "  band height changes: {} grows ({} naive), {} shrinks ({} slow-guarded, {} slow-unguarded)",
        stats.grows,
        stats.naive_grows,
        stats.shrinks,
        stats.slow_guarded_shrinks,
        stats.slow_unguarded_shrinks
    );
    println!(
        "  capture: {}.bin (+ .txt sidecar)",
        harness.capture_stem_display()
    );
    println!(
        "  replay: cargo run -p cadmus-tui --example inline_spike_replay -- {}",
        harness.capture_stem_display()
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
    fn enter(tee: Tee, identity: String, capture_stem: PathBuf) -> io::Result<Option<Self>> {
        if enable_raw_mode().is_err() {
            return Ok(None);
        }
        let Ok((width, height)) = crossterm::terminal::size() else {
            disable_raw_mode()?;
            return Ok(None);
        };
        let backend = CrosstermBackend::new(tee.clone());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(VIEWPORT_HEIGHT),
            },
        )?;
        terminal.clear()?;
        Ok(Some(Self {
            terminal,
            tee,
            capture_stem,
            identity,
            initial_rows: height,
            initial_cols: width,
            keys: Vec::new(),
            resize_log: Vec::new(),
            transcript: Vec::new(),
            active: Vec::new(),
            turn_no: 1,
            width,
            band_height: VIEWPORT_HEIGHT,
            stats: Stats::default(),
        }))
    }

    fn run(&mut self) -> io::Result<()> {
        loop {
            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.keys.push(format!("{:?}", key.code));
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('c') => self.complete_turn()?,
                            KeyCode::Char('g') => self.grow(false)?,
                            KeyCode::Char('G') => self.grow(true)?,
                            KeyCode::Char('s') => self.shrink(false, true)?,
                            KeyCode::Char('f') => self.shrink(true, true)?,
                            KeyCode::Char('F') => self.shrink(true, false)?,
                            _ => {}
                        }
                    }
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
        self.resize_log.push((self.tee.offset(), height, width));
        let shrunk = width < self.width;
        self.stats.resizes += 1;
        self.width = width;
        let mut out = self.tee.clone();
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
        // Guard sequences go through the tee like everything else — routing
        // them to raw stdout once punched a hole in the capture (first Zed
        // run showed 0 guards in the stream).
        let mut out = self.tee.clone();
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

    /// Dynamic-height probe: grow the band via Terminal recreation (spike
    /// fact F1's escape hatch). The protocol — blank-insert delta rows so
    /// the taller band covers only blanks, park the cursor at the future
    /// band top so the re-anchor's append scrolls nothing, then recreate —
    /// is the one `tests/dynamic_height_spike.rs` locks deterministically;
    /// the `naive` flag skips it to demonstrate the stale-band residue.
    fn grow(&mut self, naive: bool) -> io::Result<()> {
        let screen_height = crossterm::terminal::size()?.1;
        let new_height = (self.band_height + HEIGHT_STEP).min(screen_height);
        if new_height == self.band_height {
            return Ok(());
        }
        let delta = new_height - self.band_height;
        let mut out = self.tee.clone();
        execute!(out, BeginSynchronizedUpdate)?;
        if naive {
            self.stats.naive_grows += 1;
        } else {
            self.terminal.insert_before(delta, |_buf| {})?;
            let new_top = self.terminal.get_frame().area().y - delta;
            self.terminal
                .set_cursor_position(Position::new(0, new_top))?;
            self.stats.grows += 1;
        }
        self.recreate(new_height)?;
        execute!(out, EndSynchronizedUpdate)?;
        Ok(())
    }

    /// Shrink side of the probe: clear the old band (the vacated rows stay
    /// as bounded blank residue), park the cursor delta rows lower,
    /// recreate. The blank gap is consumed by later turn flushes. `slow`
    /// parks the cleared intermediate state for 150 ms so a compositing
    /// failure becomes unmissable; `guarded` toggles the 2026h wrapper —
    /// `f` (slow+guarded) against `F` (slow+unguarded) is the guard's A/B.
    fn shrink(&mut self, slow: bool, guarded: bool) -> io::Result<()> {
        let new_height = self
            .band_height
            .saturating_sub(HEIGHT_STEP)
            .max(MIN_BAND_HEIGHT);
        if new_height == self.band_height {
            return Ok(());
        }
        let delta = self.band_height - new_height;
        let mut out = self.tee.clone();
        if guarded {
            execute!(out, BeginSynchronizedUpdate)?;
        }
        self.terminal.clear()?;
        if slow {
            // The pause window defaults to 150 ms for human eyes; scripted
            // terminals (tmux capture-pane) override it via SPIKE_SLOW_MS.
            let slow_ms = std::env::var("SPIKE_SLOW_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(150);
            std::thread::sleep(Duration::from_millis(slow_ms));
        }
        let new_top = self.terminal.get_frame().area().y + delta;
        self.terminal
            .set_cursor_position(Position::new(0, new_top))?;
        match (slow, guarded) {
            (false, _) => self.stats.shrinks += 1,
            (true, true) => self.stats.slow_guarded_shrinks += 1,
            (true, false) => self.stats.slow_unguarded_shrinks += 1,
        }
        self.recreate(new_height)?;
        if guarded {
            execute!(out, EndSynchronizedUpdate)?;
        }
        Ok(())
    }

    fn recreate(&mut self, new_height: u16) -> io::Result<()> {
        self.terminal = Terminal::with_options(
            CrosstermBackend::new(self.tee.clone()),
            TerminalOptions {
                viewport: Viewport::Inline(new_height),
            },
        )?;
        self.band_height = new_height;
        self.draw_active();
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

    /// The band's content, shared by the live draw and the sidecar model —
    /// one composer, so the replay model can't drift from what was drawn.
    fn band_lines(&self) -> Vec<Line<'static>> {
        let content_rows = usize::from(self.band_height).saturating_sub(2);
        let wrapped = wrap_lines(
            &self.active,
            usize::from(self.width).saturating_sub(WRAP_SLACK),
        );
        let scroll = wrapped.len().saturating_sub(content_rows);
        let mut lines = vec![
            Line::from("inline_spike — c · g/G · s/f/F · q (legend above)"),
            Line::from(format!(
                "turns {} · inserts {} · resizes {}",
                self.stats.turns, self.stats.inserts, self.stats.resizes
            )),
        ];
        lines.extend(wrapped.into_iter().skip(scroll));
        lines
    }

    fn try_draw_active(&mut self) -> io::Result<()> {
        let lines = self.band_lines();
        self.terminal.draw(|frame| {
            let area = frame.area();
            frame.render_widget(Paragraph::new(lines), area);
        })?;
        Ok(())
    }

    fn capture_stem_display(&self) -> String {
        self.capture_stem.display().to_string()
    }

    /// The sidecar: everything the replay analyzer needs to verify the run
    /// without an eyewitness — metadata, resize events, the key log, and the
    /// expected final non-blank row sequence (legend + flushed history +
    /// current band, all wrapped at the final width).
    fn write_sidecar(&self) -> io::Result<()> {
        let s = &self.stats;
        let mut lines = vec![
            "cadmus-inline-spike-capture v1".to_string(),
            format!("identity: {}", self.identity),
            format!("size: {} {}", self.initial_rows, self.initial_cols),
        ];
        lines.extend(
            self.resize_log
                .iter()
                .map(|(offset, rows, cols)| format!("resize: {offset} {rows} {cols}")),
        );
        lines.push(format!(
            "stats: turns {} inserts {} rows {} resizes {} shrink_replays {} grows {} naive_grows {} shrinks {} slow_guarded {} slow_unguarded {} resize_errors {} draw_errors {}",
            s.turns, s.inserts, s.inserted_rows, s.resizes, s.shrink_replays, s.grows,
            s.naive_grows, s.shrinks, s.slow_guarded_shrinks, s.slow_unguarded_shrinks,
            s.resize_errors, s.draw_errors
        ));
        lines.push(format!("keys: {}", self.keys.join(" ")));
        lines.push("expected:".to_string());
        lines.extend(self.model_rows());
        fs::write(self.capture_stem.with_extension("txt"), lines.join("\n"))
    }

    fn model_rows(&self) -> Vec<String> {
        let width = usize::from(self.width).saturating_sub(WRAP_SLACK);
        LEGEND
            .iter()
            .map(|line| (*line).to_string())
            .chain(
                self.transcript
                    .iter()
                    .flat_map(|line| wrap_line(line, width)),
            )
            .chain(self.band_lines().into_iter().map(|line| line.to_string()))
            .filter(|row| !row.trim_end().is_empty())
            .collect()
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
