//! Terminal-quirk regression harness for the inline shell (ADR-0018): a thin
//! driver over `cadmus_tui::shell::InlineShell` — the band mechanism (2026h
//! guarded insert+draw, shell-owned geometry and shrink replay) lives in
//! the library and is locked by the vt100 integration suites. This harness
//! supplies what the suite cannot: a fake typewriter stream, key bindings,
//! the capture pipeline and diagnostics counters — so the terminal-quirk
//! matrix exercises the production path.
//!
//! What a matrix run judges on a real terminal is what vt100 cannot see:
//! compositing (flicker) and terminal quirks. The `G`/`f`/`F`
//! failure-reference keys retired with the 2026-09-14 verdict — the shell
//! owns the one-wrapper invariant by construction, so an unguarded or
//! protocol-less op is no longer expressible here; their evidence is frozen
//! in `docs/research/2026-09-14-terminal-recreation-spike.md` §7.5.
//!
//! Machine-verifiable evidence: every run tees the raw output stream to
//! `target/inline-spike/capture-<ts>.bin` plus a `.txt` sidecar (initial
//! size, terminal identity, resize events with byte offsets, the full key
//! log, and the expected final row sequence). For the full-screen fallback,
//! `inline_spike_replay` can diff the capture against the sidecar through
//! vt100. Standard partial regions need a native history capture: vt100
//! discards their departing rows. Sentinel keys still check swallowed input
//! around geometry changes. Keep the window size
//! fixed during a dynamic-height leg: the sidecar model wraps at the final
//! width, so mid-leg resizes make the model unreliable (resize reflow is a
//! separate matrix leg).

use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cadmus_tui::shell::{InlineShell, ShellStats};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{event, execute};
use ratatui::Frame;
use ratatui::backend::CrosstermBackend;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Initial band height. The grow/shrink keys change it at runtime through
/// the shell's geometry protocol.
const VIEWPORT_HEIGHT: u16 = 8;
/// Grow/shrink step of the dynamic-height probe.
const HEIGHT_STEP: u16 = 4;
/// Smallest band the shrink key allows (status + one content row).
const MIN_BAND_HEIGHT: u16 = 2;

/// The probe legend: printed into scrollback above the band at startup and
/// embedded in the sidecar (the replay model starts with it). Every line
/// must stay ≤ 72 columns — a wrapped legend line desyncs the replay model
/// (the model never wraps it; learned from the first Zed capture). The
/// startup assert below pins the rule.
const LEGEND: &[&str] = &[
    "inline_spike — inline-shell quirk probe driving cadmus_tui::shell",
    "  c complete turn (flush)   g grow +4   s shrink −4   q quit",
    "  sentinel: tap x before and after each g/s (CPR-race swallow check)",
    "  keep the window size fixed in height legs; ≥ 80 columns wide",
    "  failure-reference keys retired with the verdict — see module docs",
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

/// Probe-side counters; the mechanism counters live in the shell
/// (`ShellStats`), reported alongside at exit and in the sidecar.
#[derive(Clone, Copy, Default)]
struct Stats {
    turns: usize,
    resizes: usize,
}

struct Harness {
    shell: InlineShell<CrosstermBackend<Tee>, Tee>,
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
    assert!(
        LEGEND
            .iter()
            .all(|line| UnicodeWidthStr::width(*line) <= 72),
        "a wrapped legend line desyncs the replay model"
    );
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
    let (stats, shell_stats) = harness.exit();
    println!("inline_spike diagnostics");
    println!("  terminal: {identity}");
    println!(
        "  turns: {}, history inserts: {}, rows inserted: {}",
        stats.turns, shell_stats.inserts, shell_stats.inserted_rows
    );
    println!(
        "  resizes: {} (shrink replays: {}, resize errors tolerated: {}, draw errors tolerated: {})",
        stats.resizes,
        shell_stats.shrink_replays,
        shell_stats.tolerated_resize_errors,
        shell_stats.tolerated_draw_errors
    );
    println!(
        "  band height changes: {} grows, {} shrinks",
        shell_stats.grows, shell_stats.shrinks
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
        let shell = InlineShell::new(
            CrosstermBackend::new(tee.clone()),
            // Guard bytes take the same stream as everything else — routing
            // them to raw stdout once punched a hole in the capture (first
            // Zed run showed 0 guards in the stream).
            tee.clone(),
            VIEWPORT_HEIGHT,
            cadmus_tui::shell::ScrollbackStrategy::detect(),
        )?;
        Ok(Some(Self {
            shell,
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
                            KeyCode::Char('g') => self.grow()?,
                            KeyCode::Char('s') => self.shrink()?,
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
        // The sidecar's expected rows are computed from final state, so the
        // last frame must be a full repaint of final state — a flush's render
        // is materialized pre-op and would lag one counter step otherwise.
        self.repaint();
        Ok(())
    }

    fn exit(&mut self) -> (Stats, ShellStats) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
        (self.stats, self.shell.stats())
    }

    /// One typewriter step: append a chunk to the open turn and repaint the
    /// band. Turns auto-complete at `TURN_CHUNKS` chunks.
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
            self.repaint();
            Ok(())
        }
    }

    /// Completed-turn rows leave the band into real scrollback through the
    /// shell's guarded flush.
    fn complete_turn(&mut self) -> io::Result<()> {
        if self.active.is_empty() {
            return Ok(());
        }
        let rows = wrap_lines(&self.active, wrap_width(self.shell.width()));
        self.transcript.append(&mut self.active);
        let render = self.band_renderer();
        self.shell.flush(&rows, render)?;
        self.stats.turns += 1;
        self.turn_no += 1;
        Ok(())
    }

    fn grow(&mut self) -> io::Result<()> {
        let desired = self.shell.band_height() + HEIGHT_STEP;
        let render = self.band_renderer();
        self.shell.set_height(desired, render)
    }

    fn shrink(&mut self) -> io::Result<()> {
        let desired = self
            .shell
            .band_height()
            .saturating_sub(HEIGHT_STEP)
            .max(MIN_BAND_HEIGHT);
        let render = self.band_renderer();
        self.shell.set_height(desired, render)
    }

    fn on_resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.resize_log.push((self.tee.offset(), height, width));
        self.stats.resizes += 1;
        let render = self.band_renderer();
        let transcript = &self.transcript;
        self.shell.on_resize(
            width,
            height,
            |max_rows, width| {
                let wrapped = wrap_lines(transcript, wrap_width(width));
                let skip = wrapped.len().saturating_sub(usize::from(max_rows));
                wrapped.into_iter().skip(skip).collect()
            },
            render,
        )
    }

    fn repaint(&mut self) {
        let render = self.band_renderer();
        self.shell.draw(render);
    }

    /// Materialize-then-draw: the closure captures owned data, never a borrow
    /// of the harness, so it can run inside shell ops that hold the terminal
    /// mutably. Band geometry comes from the frame's live area, so the
    /// repaint after a recreation already shows the new height.
    fn band_renderer(&self) -> impl FnOnce(&mut Frame<'_>) + use<> {
        let active = self.active.clone();
        let width = self.shell.width();
        let turns = self.stats.turns;
        let inserts = self.shell.stats().inserts;
        let resizes = self.stats.resizes;
        move |frame: &mut Frame<'_>| {
            let area = frame.area();
            let lines = band_lines(&active, width, area.height, turns, inserts, resizes);
            frame.render_widget(Paragraph::new(lines), area);
        }
    }

    fn capture_stem_display(&self) -> String {
        self.capture_stem.display().to_string()
    }

    /// The sidecar: everything the replay analyzer needs to verify the run
    /// without an eyewitness — metadata, resize events, the key log, and the
    /// expected final non-blank row sequence (legend + flushed history +
    /// current band, all wrapped at the final width).
    fn write_sidecar(&self) -> io::Result<()> {
        let shell_stats = self.shell.stats();
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
            "stats: turns {} inserts {} rows {} resizes {} shrink_replays {} grows {} shrinks {} resize_errors {} draw_errors {}",
            self.stats.turns,
            shell_stats.inserts,
            shell_stats.inserted_rows,
            self.stats.resizes,
            shell_stats.shrink_replays,
            shell_stats.grows,
            shell_stats.shrinks,
            shell_stats.tolerated_resize_errors,
            shell_stats.tolerated_draw_errors,
        ));
        lines.push(format!("keys: {}", self.keys.join(" ")));
        lines.push("expected:".to_string());
        lines.extend(self.model_rows());
        fs::write(self.capture_stem.with_extension("txt"), lines.join("\n"))
    }

    fn model_rows(&self) -> Vec<String> {
        let width = wrap_width(self.shell.width());
        let shell_stats = self.shell.stats();
        LEGEND
            .iter()
            .map(|line| (*line).to_string())
            .chain(
                self.transcript
                    .iter()
                    .flat_map(|line| wrap_line(line, width)),
            )
            .chain(
                band_lines(
                    &self.active,
                    self.shell.width(),
                    self.shell.band_height(),
                    self.stats.turns,
                    shell_stats.inserts,
                    self.stats.resizes,
                )
                .into_iter()
                .map(|line| line.to_string()),
            )
            .filter(|row| !row.trim_end().is_empty())
            .collect()
    }
}

/// The band's content, shared by the live draw and the sidecar model — one
/// composer, so the replay model can't drift from what was drawn.
fn band_lines(
    active: &[String],
    width: u16,
    band_height: u16,
    turns: usize,
    inserts: usize,
    resizes: usize,
) -> Vec<Line<'static>> {
    let content_rows = usize::from(band_height).saturating_sub(2);
    let wrapped = wrap_lines(active, wrap_width(width));
    let scroll = wrapped.len().saturating_sub(content_rows);
    let mut lines = vec![
        Line::from("inline_spike — c · g · s · q (legend above)"),
        Line::from(format!(
            "turns {turns} · inserts {inserts} · resizes {resizes}"
        )),
    ];
    lines.extend(wrapped.into_iter().skip(scroll));
    lines
}

fn wrap_width(width: u16) -> usize {
    usize::from(width).saturating_sub(WRAP_SLACK)
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
