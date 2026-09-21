//! The app: the ADR-0018 event loop driving the inline shell (item 5) over
//! the view-model materialization (item 10, [`crate::transcript`]). One
//! `select!` over five sources — terminal input, the run's live feed, the
//! run's outcome, the resize debounce and the frame scheduler's draw ticks —
//! with input never blocking and frames demand-driven.
//!
//! The draw pump is the one place rows materialize: per tick one
//! [`Transcript::snapshot`] appends the newly stable rows to the emission
//! queue and the paced drain emits a budgeted slice into scrollback — the
//! 2026-09-20 second amendment's typewriter (codex's commit-tick model
//! adapted to Cadmus's flush contract). Rows appear only once stable, the
//! band sliding down one row per insert; the unstable tail is never
//! rendered — the band's `receiving…` row carries the liveness signal
//! instead. While a run is active the pump holds the band at the run's
//! high-water floor — the desired height never shrinks mid-run, so block
//! settles and dialog or composer appear/disappear leave no vacated-row
//! residue in the page; the floor releases at the outcome (the 2026-09-20
//! amendment's mechanism, top-anchored collapse included). End-of-run is an
//! explicit state (`run_end`): the completion note queues LAST (ordering is
//! automatic), the run-status row rides its frozen clock until the queue
//! empties, and the band's final collapse is then exactly that one row.
//!
//! Key handling is a minimal fixed map (chars, editing ops, Enter submits,
//! Esc interrupts, Ctrl-C quits, Tab selects a call and y/n answer it) —
//! ADR-0018 item 6's mode × key → command layer with keymap-as-data is its
//! own slice, and the default bindings are decided in its binding-design
//! task.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::Duration;

use cadmus_contract::{
    Approval, Attachment, Command, EventKind, LiveItem, LiveKind, LiveUpdate, Message,
    PendingApproval, Sync,
};
use cadmus_ui::highlight::Highlighter;
use cadmus_ui::ir::{self, Slot};
use cadmus_ui::theme::{ColorDepth, Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use ratatui::Frame;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use tokio::time::Instant;
use unicode_width::UnicodeWidthStr;

use crate::approval;
use crate::clock::RunClock;
use crate::composer::{Composer, DEFAULT_PLACEHOLDER, RUNNING_PLACEHOLDER};
use crate::cursor::CursorTracker;
use crate::debounce::ResizeDebounce;
use crate::frame::{Draw, FrameRequester, frame_scheduler};
use crate::input::{EventSource, InputBroker};
use crate::layout::{self, BandLayout, LayoutInput};
use crate::shell::InlineShell;
use crate::style::ir_style;
use crate::transcript::{Light, Transcript, failed_after_line, truncate_cells, worked_for_line};
use crate::wrap::wrap_rows;

/// The drainer's bounded forwarding depth: a stalled app lags the
/// broadcaster and is told to re-sync — never awaited (ADR-0013 item 5).
const FEED_DEPTH: usize = 1_024;

/// The typewriter's tick: the paced drain's self-reschedule horizon (~30
/// inserts/second — the codex commit-tick rhythm adapted to Cadmus's flush
/// contract: stable rows queue transcript-side and drain into scrollback at
/// this pace, the band sliding down one row per insert).
const DRAIN_TICK: Duration = Duration::from_millis(33);

/// The budget's depth tiers: below `DRAIN_DEEP` rows one per tick, below
/// `DRAIN_FLOOD` two, at or past it four — smooth at a trickle, catching up
/// under backlog (the pacing constants live here, tunable in one place).
const DRAIN_DEEP: usize = 8;
const DRAIN_FLOOD: usize = 24;

/// A finishing run's budget floor: the tail's last stable rows and the
/// completion note never dawdle (the end-of-run sequencing).
const DRAIN_RUN_END: usize = 4;

/// The liveness row's text — the stand-in for the hidden unstable tail:
/// one subtle row at the band's top while a run has output pending.
const RECEIVING_TEXT: &str = "receiving…";

/// One run's wires, handed over at submit (the binary builds them from the
/// freshly spawned loop). Contract types only — the frontend never links
/// core or transport (ADR-0018 item 10), so the re-attach capability and
/// the command sink are plain closures.
pub struct RunHandle {
    pub attachment: Attachment,
    /// Re-attach after a lag (ADR-0013 item 5): a fresh handshake.
    pub reattach: Box<dyn Fn() -> Attachment + Send>,
    /// The upstream command sink for this run (steer/interrupt/resolve).
    pub commands: Box<dyn Fn(Command) + Send + ::std::marker::Sync>,
    /// The run's report, sent once the tail is fully drained — the drainer
    /// forwards it last, so the app tears the run down in stream order
    /// (an outcome must never race the tail's trailing items).
    pub teardown: std::sync::mpsc::Receiver<Result<Vec<Message>, String>>,
}

/// How the app starts runs — the session boundary, implemented by the
/// binary's wiring.
pub trait RunDriver {
    fn start(&self, messages: Vec<Message>) -> RunHandle;
}

/// Injected app configuration (AGENTS.md: no hidden environment reads).
pub struct AppConfig {
    /// The status line's left label (provider·model).
    pub label: String,
    /// The model's context window — the denominator of the floor's
    /// context-usage ratio. The provider's registry declaration
    /// (`Capabilities::max_context`), handed over as plain data: the
    /// frontend never links core (ADR-0018 item 10).
    pub context_window: u64,
    /// Re-derives the floor's left label: the binary's git re-probe lives
    /// behind this closure, keeping process IO out of the TUI crate. The
    /// app calls it at each run outcome — the cadence at which the agent's
    /// edits land. `None` keeps the boot label for the session.
    pub refresh_label: Option<Box<dyn Fn() -> String + Send + ::std::marker::Sync>>,
    pub theme: Theme,
    pub depth: ColorDepth,
}

/// What the drainer forwards to the loop.
enum FeedMsg {
    /// A (re-)attach baseline; `resync` distinguishes a lag re-attach from
    /// the run's first attach — the drainer is the authoritative source of
    /// that bit (never re-derive it from view state).
    Sync {
        sync: Box<Sync>,
        resync: bool,
    },
    Item(Box<LiveItem>),
    /// The tail ended and the run reported — always the last message of a
    /// run's feed, so teardown applies in stream order.
    Outcome(Result<Vec<Message>, String>),
}

/// The drainer thread: the attachment's tail is a blocking iterator, so one
/// thread per run forwards it into the loop's channel, re-attaching on lag
/// (the same shape as headless chat's renderer). The run's report rides the
/// same thread — forwarded once the tail ends — which serializes teardown
/// after every trailing item by construction.
fn spawn_drainer(
    attachment: Attachment,
    reattach: Box<dyn Fn() -> Attachment + Send>,
    teardown: std::sync::mpsc::Receiver<Result<Vec<Message>, String>>,
    tx: mpsc::Sender<FeedMsg>,
) {
    std::thread::spawn(move || {
        let mut attachment = attachment;
        let mut resync = false;
        'attach: loop {
            if tx
                .blocking_send(FeedMsg::Sync {
                    sync: Box::new(attachment.sync),
                    resync,
                })
                .is_err()
            {
                return;
            }
            for update in &mut attachment.tail {
                let msg = match update {
                    LiveUpdate::Item { item } => FeedMsg::Item(item),
                    LiveUpdate::Lagged => {
                        attachment = reattach();
                        resync = true;
                        continue 'attach;
                    }
                };
                if tx.blocking_send(msg).is_err() {
                    return;
                }
            }
            // The tail ended (the publisher is gone); the report is next —
            // a dead run task reports through the channel's hang-up.
            let outcome = teardown
                .recv()
                .unwrap_or_else(|_| Err("the run task died without a report".into()));
            let _ = tx.blocking_send(FeedMsg::Outcome(outcome));
            return;
        }
    });
}

/// The run-status row's state — the state-truthfulness rule (ADR-0011's
/// 2026-09-11 amendment item 6): derived from events, never guessed.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Status {
    Idle,
    Streaming,
    Tool {
        name: String,
        target: Option<String>,
    },
    Failed,
}

impl Status {
    fn note(&mut self, light: Light) {
        match light {
            Light::None => {}
            Light::Streaming => *self = Self::Streaming,
            Light::Tool { name, target } => *self = Self::Tool { name, target },
            Light::Idle => *self = Self::Idle,
            Light::Failed => *self = Self::Failed,
        }
    }
}

/// A run in flight: its command sink. (Teardown arrives as the feed's last
/// message — `FeedMsg::Outcome`.)
struct ActiveRun {
    commands: Box<dyn Fn(Command) + Send + ::std::marker::Sync>,
}

/// The end-of-run sequencing state (see [`App::run_end`]): the outcome has
/// landed but the run's display is still typing out.
struct RunEnd;

/// The app. See the module docs.
pub struct App<B: Backend<Error = io::Error> + Clone + Write, W: Write, I: EventSource> {
    shell: InlineShell<B, W>,
    input: I,
    driver: Box<dyn RunDriver>,
    transcript: Transcript,
    composer: Composer,
    requester: FrameRequester,
    draws: mpsc::Receiver<Draw>,
    debounce: ResizeDebounce,
    highlighter: Highlighter,
    theme: Theme,
    depth: ColorDepth,
    label: String,
    /// The label's refresh closure (see [`AppConfig::refresh_label`]).
    refresh_label: Option<Box<dyn Fn() -> String + Send + ::std::marker::Sync>>,
    /// The model's context window (see [`AppConfig::context_window`]).
    context_window: u64,
    /// The session's conversation: each run's outcome replaces it.
    history: Vec<Message>,
    run: Option<ActiveRun>,
    feed: Option<mpsc::Receiver<FeedMsg>>,
    status: Status,
    /// The run's high-water floor (ADR-0018's 2026-09-20 amendment): while a
    /// run is active the band's desired height never shrinks — the floor is
    /// the running max of the content's own want, seeded at submit and
    /// released at the outcome for the one collapse per run. `None` idle:
    /// the composer's own grow/shrink while typing is unchanged.
    band_floor: Option<u16>,
    /// The run wall-clock (submit→outcome, paused across approval waits) —
    /// `Some` from the first submit on. It feeds the run-status row and the
    /// scrollback completion rows; only the event/render edges call
    /// `Instant::now()` (AGENTS.md's injected-time seam).
    clock: Option<RunClock>,
    /// The latest response's context size (input + cache-read tokens),
    /// shown on the run-status row once known.
    context_tokens: Option<u64>,
    command_seq: u64,
    /// Approval requests awaiting the user's decisions, FIFO — the band
    /// renders the head, Tab selects a call and y/n answer that call.
    /// The gate presents one batch per turn and awaits its decisions, so
    /// the queue holds a single request in practice; the FIFO covers an
    /// attach mid-wait (the sync baseline replays the pending list) and the
    /// protocol's general shape without a modal stack. Cleared when the run
    /// ends — a dead run's channel drop already denied the request, the app
    /// must not answer it.
    approvals: VecDeque<QueuedApproval>,
    /// The emission pacing switch (see [`App::with_pacing`]).
    paced: bool,
    /// Esc's impatient mode: set by [`App::interrupt`], the queue drains
    /// whole from there until the run's drain completes — an interrupt is
    /// no time for the typewriter. Cleared at the drain's completion and at
    /// a fresh submit (it belongs to the run that was interrupted).
    dump: bool,
    /// End-of-run sequencing: `Some` from the outcome's landing until the
    /// emission queue empties. The completion note queues LAST (behind the
    /// tail's stable rows, so ordering is automatic); the run-status row
    /// rides its frozen clock until then, and the band's final collapse is
    /// exactly that one row — the end-of-run composer jump dies.
    run_end: Option<RunEnd>,
    quit: bool,
}

/// One queued request plus its section's logical lines. The section is
/// materialized on focus/decision changes, not per frame. The dialog caches
/// each call's diff so navigation only copies already-budgeted lines.
struct QueuedApproval {
    dialog: approval::Dialog,
    section: Vec<ir::Line>,
}

/// The head request's display rows at `width`: the cached section lines
/// wrapped — wrapping is the only per-frame work ([`QueuedApproval`]'s
/// contract).
fn wrap_section(
    approvals: &VecDeque<QueuedApproval>,
    width: u16,
    theme: &Theme,
    depth: ColorDepth,
) -> Vec<Line<'static>> {
    wrap_rows(
        approvals
            .front()
            .map_or(&[][..], |queued| queued.section.as_slice()),
        width,
        theme,
        depth,
    )
}

impl<B: Backend<Error = io::Error> + Clone + Write, W: Write, I: EventSource> App<B, W, I> {
    /// Wire the parts and spawn the frame-scheduler actor — call within a
    /// tokio runtime (the actor is `tokio::spawn`ed). The boot frame is
    /// scheduled immediately: the band appears before the first keystroke.
    pub fn new(
        shell: InlineShell<B, W>,
        input: I,
        driver: Box<dyn RunDriver>,
        config: AppConfig,
    ) -> Self {
        let (requester, scheduler, draws) = frame_scheduler();
        // The scheduler actor is demand-driven and exits once every
        // requester is dropped (app teardown included).
        tokio::spawn(scheduler.run());
        // The boot frame: the band appears before the first keystroke.
        requester.schedule_frame();
        Self {
            shell,
            input,
            driver,
            transcript: Transcript::new(),
            composer: Composer::new(),
            requester,
            draws,
            debounce: ResizeDebounce::new(ResizeDebounce::DEFAULT_DELAY),
            highlighter: Highlighter::new(),
            theme: config.theme,
            depth: config.depth,
            label: config.label,
            refresh_label: config.refresh_label,
            context_window: config.context_window,
            history: Vec::new(),
            run: None,
            feed: None,
            status: Status::Idle,
            band_floor: None,
            clock: None,
            context_tokens: None,
            command_seq: 0,
            approvals: VecDeque::new(),
            paced: true,
            dump: false,
            run_end: None,
            quit: false,
        }
    }

    /// The motion-profile switch (the 2026-09-20 second amendment):
    /// `false` disables the paced typewriter (instant emission). The
    /// real-terminal boot reads `TERM` at the boundary ([`run`] via
    /// [`crate::style::detect_paced`]); tests inject the value directly —
    /// the full `full|reduced|none` profile lands with the item-7 TOML
    /// loader (docs/open-items.md).
    #[must_use]
    pub fn with_pacing(mut self, paced: bool) -> Self {
        self.paced = paced;
        self
    }

    /// The loop: select, handle, drain the feed, repeat until quit.
    /// Structural shell failures (insert/clear/recreate) propagate; draw
    /// failures are tolerated inside the shell.
    pub async fn run_loop(&mut self) -> io::Result<()> {
        while !self.quit {
            let tick = tokio::select! {
                event = self.input.next_event() => Tick::Input(event),
                msg = feed_next(&mut self.feed) => Tick::Feed(msg),
                () = resize_next(&self.debounce) => Tick::Resize,
                draw = self.draws.recv() => Tick::Draw(draw),
            };
            match tick {
                Tick::Input(event) => self.on_input(event)?,
                Tick::Feed(msg) => self.on_feed(msg)?,
                Tick::Resize => self.on_resize()?,
                Tick::Draw(draw) => {
                    if draw.is_none() {
                        // The scheduler is gone: no frames ever again — the
                        // loop's draw branch is dead, so the app is too.
                        return Ok(());
                    }
                    self.pump()?;
                }
            }
            // Catch-up: apply whatever the feed already queued before the
            // next wait — a draw always sees the fully drained state.
            while let Some(rx) = &mut self.feed {
                match rx.try_recv() {
                    Ok(msg) => self.on_feed(msg)?,
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    /// The run-status row's height input: 1 while a run is active, while
    /// the end-of-run drain is pending (the row rides its frozen clock
    /// until the note lands), or the status is Failed (the failure's row
    /// stays up), 0 while idle — event-driven per the layout contract.
    fn run_status_rows(&self) -> u16 {
        u16::from(self.run.is_some() || self.run_end.is_some() || self.status == Status::Failed)
    }

    /// The run clock's elapsed at `now` (ZERO before the first run).
    fn elapsed(&self, now: Instant) -> Duration {
        self.clock
            .as_ref()
            .map_or(Duration::ZERO, |clock| clock.elapsed(now))
    }

    /// The `receiving…` row's visibility input: a run is active and either
    /// the drain queue holds rows or the stream's unstable tail is non-empty
    /// (`snapshot.tail_live`) — the liveness signal that stands in for the
    /// never-rendered tail.
    fn receiving(&self, tail_live: bool) -> bool {
        self.run.is_some() && (self.transcript.queued_len() > 0 || tail_live)
    }

    /// Whether the display is still a run's: the run is active or its
    /// end-of-run drain is pending (the run-status row and its clock text
    /// ride until the note lands).
    fn running(&self) -> bool {
        self.run.is_some() || self.run_end.is_some()
    }

    /// The clock's pause seam: a gate wait is not work, so while the
    /// approvals queue is non-empty the timer holds (and the row reads
    /// "Waiting for approval"). `RunClock`'s pause/resume are idempotent,
    /// so this fires after every queue mutation site instead of tracking
    /// empty↔non-empty transitions by hand.
    fn sync_clock_pause(&mut self) {
        if self.run.is_none() {
            return;
        }
        let Some(clock) = &mut self.clock else {
            return;
        };
        if self.approvals.is_empty() {
            clock.resume(Instant::now());
        } else {
            clock.pause(Instant::now());
        }
    }

    /// One tick's drain budget by queue depth (the paced typewriter's
    /// policy): a trickle while the queue is shallow, a catch-up slope as
    /// it deepens, and the accelerated floor while a run is finishing. The
    /// instant modes bypass it: the `TERM=dumb` profile (`paced: false`)
    /// and Esc's dump both drain whole.
    fn drain_budget(&self, depth: usize) -> usize {
        if !self.paced || self.dump {
            return usize::MAX;
        }
        let budget = if depth < DRAIN_DEEP {
            1
        } else if depth < DRAIN_FLOOD {
            2
        } else {
            4
        };
        if self.run_end.is_some() {
            budget.max(DRAIN_RUN_END)
        } else {
            budget
        }
    }

    /// The draw pump: one snapshot per tick queues the newly stable rows,
    /// the paced drain emits a budgeted slice, and the band render and the
    /// layout read the post-drain state (module docs).
    fn pump(&mut self) -> io::Result<()> {
        self.pump_with(false)
    }

    /// The instant-drain pump: the attach transfer's pre/post passes — the
    /// resync transfer counts acked lines, and replayed history must not
    /// re-type. Same pump, the budget bypassed.
    fn pump_dump(&mut self) -> io::Result<()> {
        self.pump_with(true)
    }

    fn pump_with(&mut self, drain_all: bool) -> io::Result<()> {
        // The render edge's one `now`: the row's elapsed, the clock tick's
        // and the drain horizon's reads all share it.
        let now = Instant::now();
        let width = self.shell.width();
        let snapshot = self
            .transcript
            .snapshot(width, &self.highlighter, &self.theme, self.depth);
        // The drain: budget by the queue's depth, whole under the instant
        // modes. The `receiving…` row reads the POST-drain depth.
        let budget = if drain_all {
            usize::MAX
        } else {
            self.drain_budget(self.transcript.queued_len())
        };
        let drained = self.transcript.drain(budget);
        let receiving = self.receiving(snapshot.tail_live);
        let run_status_rows = self.run_status_rows();
        let running = self.running();
        let elapsed = self.elapsed(now);
        let waiting = !self.approvals.is_empty();
        let Self {
            shell,
            transcript,
            composer,
            theme,
            depth,
            label,
            run,
            status,
            band_floor,
            context_tokens,
            context_window,
            approvals,
            run_end,
            dump,
            ..
        } = &mut *self;
        let approval_rows = wrap_section(approvals, width, theme, *depth);
        let mut layout = band_layout(
            shell,
            composer,
            u16::from(receiving),
            approval_rows.len(),
            run_status_rows,
        );
        // The run high-water hold: while a run is active the desired height
        // only rises — the floor is the running max of the content's own
        // want — so a settled block, a dismissed dialog or a cleared
        // type-ahead composer never shrinks the band mid-run (the mechanism
        // behind the per-block blank-row residue the 2026-09-20 amendment
        // retires). The slack pads the band's top; the one collapse lands
        // at the outcome, when the floor is gone.
        if let Some(floor) = band_floor {
            *floor = (*floor).max(layout.band_height);
            layout = layout.held_at(*floor);
        }
        let run_status = run_status_text(status, running, elapsed, waiting, *context_tokens, width);
        // The floor's right side: the context-usage ratio, once the first
        // response's usage lands (before that it stays empty).
        let status_right =
            context_tokens.map(|tokens| format_context_usage(tokens, *context_window));
        let mut band = BandCtx {
            composer,
            label,
            status,
            running,
            run_status,
            status_right,
            layout,
            theme,
            depth: *depth,
        };
        if !drained.rows.is_empty() {
            let render = band_render(approval_rows.clone(), &mut band);
            // Flush first, then confirm: the ack contract is a *successful*
            // shell insert, so a structural failure must die un-acked.
            shell.flush(&drained.rows, render)?;
        }
        // Zero-row completion emissions still carry a logical-line ack.
        transcript.apply_flush(&drained.acks);
        // The end-of-run completion: the note's last row just landed. Esc's
        // dump mode ends here too, and the run-status row releases — the
        // collapse below is then exactly its one row (the outcome already
        // released the floor and dropped the receiving row).
        if run_end.is_some() && transcript.queued_len() == 0 {
            *run_end = None;
            *dump = false;
            let run_status_rows = u16::from(run.is_some() || *status == Status::Failed);
            layout = band_layout(
                shell,
                &*band.composer,
                u16::from(receiving),
                approval_rows.len(),
                run_status_rows,
            );
            if let Some(floor) = band_floor {
                *floor = (*floor).max(layout.band_height);
                layout = layout.held_at(*floor);
            }
            band.layout = layout;
        }
        // The collapse flushes BEFORE it shrinks — and the shrink itself is
        // top-anchored (shell.rs): the vacated Δ rows sit BELOW the band as
        // a blank buffer the next growth re-absorbs and the next inserts
        // descend into, so neither the run's final batch nor the transcript
        // ever sees a residue gap (the 2026-09-20 amendment's mechanism).
        if shell.needs_height_change(layout.band_height) {
            // Geometry is shell-owned; installing a fixed drawing surface
            // never queries the cursor or quiesces the input stream.
            let render = band_render(approval_rows.clone(), &mut band);
            shell.set_height(layout.band_height, render)?;
        }
        let render = band_render(approval_rows, &mut band);
        shell.draw(render);
        // The typewriter's horizon: while the queue holds rows the app
        // self-reschedules at the drain tick (frame.rs's pattern — an armed
        // frame never delays an interactive one).
        if self.transcript.queued_len() > 0 {
            self.requester.schedule_frame_at(now + DRAIN_TICK);
        }
        // The run clock's 1 Hz tick: a self-rescheduling horizon while a run
        // is active, silent when idle.
        if self.run.is_some() {
            self.requester
                .schedule_frame_at(now + Duration::from_secs(1));
        }
        Ok(())
    }

    fn on_input(&mut self, event: Option<io::Result<Event>>) -> io::Result<()> {
        let Some(event) = event else {
            // The input stream ended (stdin EOF): nowhere left to talk to.
            self.quit = true;
            return Ok(());
        };
        match event? {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Paste(text) => {
                // Bracketed paste: one insert, one undo unit (composer docs).
                self.composer.insert_str(&text);
                self.requester.schedule_frame();
            }
            Event::Resize(cols, rows) => {
                self.debounce.record(Instant::now(), cols, rows);
            }
            _ => {}
        }
        Ok(())
    }

    /// The minimal fixed keymap (module docs). Everything the composer can
    /// do is a plain method, so the item-6 keymap layer can rebind any of it.
    fn on_key(&mut self, key: KeyEvent) {
        // Bare y/n answer the focused call; Tab/Shift-Tab move focus without
        // stealing the composer's arrows. Esc and Ctrl-C still fall through.
        if !self.approvals.is_empty() {
            let backwards = match (key.code, key.modifiers) {
                (KeyCode::Tab, KeyModifiers::NONE) => Some(false),
                (KeyCode::BackTab, KeyModifiers::NONE | KeyModifiers::SHIFT)
                | (KeyCode::Tab, KeyModifiers::SHIFT) => Some(true),
                _ => None,
            };
            if let Some(backwards) = backwards {
                let queued = self.approvals.front_mut().expect("a pending dialog");
                queued.dialog.move_focus(backwards);
                queued.section = queued.dialog.lines();
            } else if key.modifiers.is_empty() && matches!(key.code, KeyCode::Char('y' | 'n')) {
                // No repeat filter here: `Dialog`'s arm-first rule is the
                // held-key guard (a repeat needs a fresh Tab), and submission
                // already disarms — so a terminal that reports the first
                // keydown as Repeat cannot lose a deliberate answer.
                self.resolve_approval(matches!(key.code, KeyCode::Char('y')));
            } else {
                self.edit_key(key);
                return;
            }
            self.requester.schedule_frame();
            return;
        }
        self.edit_key(key);
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let composer = &mut self.composer;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.quit = true,
            (KeyCode::Enter, KeyModifiers::NONE) => self.submit(),
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => composer.insert_newline(),
            (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                composer.undo();
            }
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => composer.delete_word_back(),
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => composer.move_home(false),
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => composer.move_end(false),
            (KeyCode::Backspace, _) => composer.backspace(),
            (KeyCode::Delete, _) => composer.delete_forward(),
            (KeyCode::Left, _) => composer.move_left(shift),
            (KeyCode::Right, _) => composer.move_right(shift),
            (KeyCode::Up, _) => composer.move_up(shift),
            (KeyCode::Down, _) => composer.move_down(shift),
            (KeyCode::Home, _) => composer.move_home(shift),
            (KeyCode::End, _) => composer.move_end(shift),
            (KeyCode::Esc, _) => self.interrupt(),
            (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                composer.insert_str(&c.to_string());
            }
            _ => return,
        }
        self.requester.schedule_frame();
    }

    /// Enter submits when idle (steering a live run is the steer slice's;
    /// until then the run's composer input stays editable but inert).
    fn submit(&mut self) {
        if self.run.is_some() {
            return;
        }
        let text = self.composer.text();
        if text.trim().is_empty() {
            return;
        }
        // The high-water floor engages BEFORE the composer clears, seeded
        // with the height the band actually claimed for the prompt: the
        // next pump's composer collapse and the prompt flush then shrink
        // nothing — the run-status row's +1 and the receiving row re-fill
        // the held rows within the same second.
        self.band_floor = Some(self.shell.band_height());
        // A fresh run starts paced: an Esc dump from the previous run does
        // not carry over (its note keeps typing at the run-end floor
        // behind the new prompt — the queue's FIFO orders them).
        self.dump = false;
        self.composer.clear();
        self.composer.set_placeholder(RUNNING_PLACEHOLDER);
        self.transcript.push_user(&text);
        let mut messages = self.history.clone();
        messages.push(Message::user(&text));
        let handle = self.driver.start(messages);
        let (tx, rx) = mpsc::channel(FEED_DEPTH);
        spawn_drainer(handle.attachment, handle.reattach, handle.teardown, tx);
        self.feed = Some(rx);
        self.run = Some(ActiveRun {
            commands: handle.commands,
        });
        self.clock = Some(RunClock::start(Instant::now()));
        self.status = Status::Streaming;
        self.requester.schedule_frame();
    }

    /// Esc: the ADR-0011 item 3 interrupt — completed work is preserved
    /// (the loop answers with the INTERRUPTED terminal record).
    fn interrupt(&mut self) {
        let Some(run) = &self.run else { return };
        (run.commands)(Command::Interrupt {
            command_id: format!("tui-{}", self.command_seq),
        });
        self.command_seq += 1;
        // The impatient path: the typewriter's pace is over — everything
        // drains whole from here to the run's collapse, note included.
        self.dump = true;
    }

    fn queue_approval(&mut self, pending: PendingApproval) {
        let dialog = approval::Dialog::new(pending);
        if dialog.complete() {
            return;
        }
        let section = dialog.lines();
        self.approvals.push_back(QueuedApproval { dialog, section });
    }

    /// Submission is not settlement: suppress retries locally, but keep the
    /// request until the recorded decisions arrive (including a racing deny).
    fn resolve_approval(&mut self, approved: bool) {
        let Some(run) = &self.run else { return };
        let Some(queued) = self.approvals.front_mut() else {
            return;
        };
        let Some(call_index) = queued.dialog.submit() else {
            return;
        };
        (run.commands)(Command::ResolveApprovalCall {
            command_id: format!("tui-{}", self.command_seq),
            request_id: queued.dialog.request.request_id.clone(),
            call_index,
            decision: if approved {
                Approval::Approved
            } else {
                Approval::Rejected { comment: None }
            },
        });
        self.command_seq += 1;
        queued.section = queued.dialog.lines();
    }

    fn sync_approvals(&mut self, pending: &[PendingApproval]) {
        let mut previous = std::mem::take(&mut self.approvals);
        for request in pending {
            if let Some(index) = previous
                .iter()
                .position(|queued| queued.dialog.request.request_id == request.request_id)
            {
                let mut queued = previous.remove(index).expect("matching request");
                queued.dialog.resync(request.clone());
                if !queued.dialog.complete() {
                    queued.section = queued.dialog.lines();
                    self.approvals.push_back(queued);
                }
            } else {
                self.queue_approval(request.clone());
            }
        }
    }

    fn settle_approval(&mut self, kind: &EventKind) {
        match kind {
            EventKind::Command(Command::ResolveApproval { request_id, .. }) => {
                self.approvals
                    .retain(|queued| queued.dialog.request.request_id != *request_id);
            }
            EventKind::Command(Command::ResolveApprovalCall {
                request_id,
                call_index,
                decision,
                ..
            }) => {
                if let Some(queued) = self
                    .approvals
                    .iter_mut()
                    .find(|queued| queued.dialog.request.request_id == *request_id)
                {
                    queued.dialog.settle(*call_index, decision);
                    queued.section = queued.dialog.lines();
                }
                self.approvals.retain(|queued| !queued.dialog.complete());
            }
            EventKind::RunFinished { .. } => self.approvals.clear(),
            _ => {}
        }
    }

    /// The context-size metric: the latest response's input + cache-read
    /// tokens, shown on the run-status row once known.
    fn note_tokens(&mut self, item: &LiveItem) {
        if let LiveKind::Recorded { event } = &item.kind
            && let EventKind::LlmResponse { usage: Some(u), .. } = &event.kind
        {
            self.context_tokens = Some(u.input + u.cache_read);
        }
    }

    fn on_feed(&mut self, msg: FeedMsg) -> io::Result<()> {
        match msg {
            FeedMsg::Item(item) => {
                // The one client rule (ADR-0013 item 4) guards the dialog
                // queue too: a stale request must not reopen it. The
                // baseline is read before the item applies.
                let fresh = item.seq > self.transcript.as_of_seq();
                self.note_tokens(&item);
                let light = self.transcript.apply_item(&item);
                if fresh
                    && let LiveKind::ApprovalRequested {
                        request_id,
                        turn,
                        calls,
                        wait_timeout,
                    } = &item.kind
                {
                    self.queue_approval(PendingApproval {
                        request_id: request_id.clone(),
                        turn: *turn,
                        message_index: None,
                        calls: calls.clone(),
                        decisions: vec![None; calls.len()],
                        wait_timeout: *wait_timeout,
                    });
                }
                // Recorded decisions, including remote answers and timeout,
                // are authoritative. A stale replay cannot clear a re-seeded
                // dialog; individual answers leave siblings available.
                if fresh && let LiveKind::Recorded { event } = &item.kind {
                    self.settle_approval(&event.kind);
                }
                self.status.note(light);
                self.sync_clock_pause();
                self.requester.schedule_frame();
            }
            FeedMsg::Sync { sync, resync } => {
                // Pump whole first: the transfer counts on everything
                // flushable already being in scrollback (transcript docs),
                // and replayed history must not re-type — the drain's
                // instant path.
                self.pump_dump()?;
                let width = self.shell.width();
                self.transcript
                    .apply_sync(&sync, width, &self.highlighter, resync);
                // The attach baseline is authoritative for the dialog queue
                // as well: an attach mid-wait replays the pending request(s)
                // (ADR-0013 item 3), a lag re-attach drops what settled in
                // the hole.
                self.sync_approvals(&sync.in_flight.pending_approvals);
                self.sync_clock_pause();
                // The rebuild's rows bypass pacing for the same reason.
                self.pump_dump()?;
                if resync {
                    // The hole marker: flushed directly, outside the block
                    // model, so replays never reorder it (transcript docs).
                    self.flush_resync_marker(width)?;
                }
                self.requester.schedule_frame();
            }
            // The feed's last message: the tail drained, then the report —
            // teardown in stream order (the drainer docs).
            FeedMsg::Outcome(outcome) => self.on_outcome(outcome),
        }
        Ok(())
    }

    /// The lag hole's marker row, flushed directly (outside the block
    /// model, so replays never reorder it — transcript docs).
    fn flush_resync_marker(&mut self, width: u16) -> io::Result<()> {
        let marker = wrap_rows(
            &[ir::Line::from_spans(vec![ir::Span::slotted(
                "… resynced …",
                Slot::TextSubtle,
            )])],
            width,
            &self.theme,
            self.depth,
        );
        if marker.is_empty() {
            return Ok(());
        }
        // The liveness query for the band's layout: the snapshot's append
        // pass is idempotent with nothing new — it only re-renders the open
        // tail.
        let snapshot = self
            .transcript
            .snapshot(width, &self.highlighter, &self.theme, self.depth);
        let receiving = self.receiving(snapshot.tail_live);
        let run_status_rows = self.run_status_rows();
        let running = self.running();
        let elapsed = self.elapsed(Instant::now());
        let waiting = !self.approvals.is_empty();
        let approval_rows = wrap_section(&self.approvals, width, &self.theme, self.depth);
        let layout = band_layout(
            &mut self.shell,
            &self.composer,
            u16::from(receiving),
            approval_rows.len(),
            run_status_rows,
        );
        // The marker's flush does not change the height, so the floor's
        // read side alone shapes the split — the pump alone raises the
        // high-water.
        let layout = floored_layout(self.band_floor, layout);
        let run_status = run_status_text(
            &self.status,
            running,
            elapsed,
            waiting,
            self.context_tokens,
            width,
        );
        let status_right = self
            .context_tokens
            .map(|tokens| format_context_usage(tokens, self.context_window));
        let mut band = BandCtx {
            composer: &mut self.composer,
            label: &self.label,
            status: &self.status,
            running,
            run_status,
            status_right,
            layout,
            theme: &self.theme,
            depth: self.depth,
        };
        let render = band_render(approval_rows, &mut band);
        self.shell.flush(&marker, render)
    }

    /// The run's teardown: the clock freezes here, at the run's true end
    /// (the report rides the drainer after the tail, so no approval wait
    /// or trailing item can follow), and the completion row lands in the
    /// transcript (Codex's `FinalMessageSeparator` precedent — the
    /// interrupt path's report is `Ok`, so it lands here too). The display
    /// is not done, though: the note queues LAST and the `run_end` state
    /// carries the drain to its completion (module docs).
    fn on_outcome(&mut self, outcome: Result<Vec<Message>, String>) {
        self.feed = None;
        self.run = None;
        // The git label's refresh cadence: the agent's edits land with the
        // run's outcome (an interrupt rides this path too — interrupted
        // edits are edits all the same). The probe itself is the binary's,
        // behind the injected closure.
        if let Some(refresh) = &self.refresh_label {
            self.label = refresh();
        }
        // The floor releases here, at the run's true end (an interrupt
        // rides this same path): the outcome's collapse lands at the run's
        // resting height (run-status row + composer + floor), NOT idle —
        // the row lingers for the drain, and the last Δ=1 collapse is the
        // drain's own.
        self.band_floor = None;
        let elapsed = self
            .clock
            .as_mut()
            .map(|clock| clock.freeze(Instant::now()));
        // A dead run owns no dialog: its gate already denied every
        // pending request (the channel-drop rule).
        self.approvals.clear();
        self.composer.set_placeholder(DEFAULT_PLACEHOLDER);
        match outcome {
            Ok(messages) => {
                self.history = messages;
                if let Some(elapsed) = elapsed {
                    self.transcript
                        .push_note(worked_for_line(&format_elapsed(elapsed)));
                }
                self.status = Status::Idle;
            }
            Err(text) => {
                self.fail_run(&text);
                if let Some(elapsed) = elapsed {
                    self.transcript
                        .push_note(failed_after_line(&format_elapsed(elapsed)));
                }
            }
        }
        // The drain's last leg: the tail's stable rows and the note type
        // out at the accelerated floor (or whole, under Esc's dump); the
        // run-status row and the final one-row collapse wait on the queue.
        self.run_end = Some(RunEnd);
        self.requester.schedule_frame();
    }

    fn fail_run(&mut self, text: &str) {
        // The RunFinished-with-error event already drew the marker on the
        // paths that record one — and teardown is serialized after every
        // trailing item, so that marker always landed first; paths without
        // a terminal record (a failed trajectory log aborts mid-run) land
        // here only.
        if self.status != Status::Failed {
            self.transcript.push_error(text);
        }
        self.status = Status::Failed;
    }

    /// The settled resize: re-anchor and replay from source at the new
    /// width, then the regular pump re-fits the height (the shell owns the
    /// mechanics; the cadence is the debounce's).
    fn on_resize(&mut self) -> io::Result<()> {
        let Some((cols, rows)) = self.debounce.take() else {
            return Ok(());
        };
        // The width-change rewind: queued rows carry the old width's wrap,
        // so the queue folds back into its blocks and the pump after this
        // re-queues at the new width (nothing queued was acked yet — the
        // drain owns the ack contract).
        if cols != self.shell.width() {
            self.transcript.rewind_queue();
        }
        // Materialize at the new width first: the replay closure and the
        // band render both read from source, never from the old frame.
        let now = Instant::now();
        let run_status_rows = self.run_status_rows();
        let running = self.running();
        let band_floor = self.band_floor;
        let elapsed = self.elapsed(now);
        let waiting = !self.approvals.is_empty();
        // The liveness inputs at the new width: the rewind reset the append
        // cursor, so this pass re-queues (the drain is the pump's, right
        // after); the rows themselves stay out of the band either way.
        let snapshot = self
            .transcript
            .snapshot(cols, &self.highlighter, &self.theme, self.depth);
        let receiving = self.receiving(snapshot.tail_live);
        let approval_rows = wrap_section(&self.approvals, cols, &self.theme, self.depth);
        let Self {
            shell,
            transcript,
            composer,
            highlighter,
            theme,
            depth,
            label,
            status,
            context_tokens,
            context_window,
            ..
        } = &mut *self;
        let layout = band_layout(
            shell,
            composer,
            u16::from(receiving),
            approval_rows.len(),
            run_status_rows,
        );
        // The resize repaint does not change the height (the pump right
        // after re-fits it), so the floor's read side alone shapes the
        // split.
        let layout = floored_layout(band_floor, layout);
        let run_status = run_status_text(status, running, elapsed, waiting, *context_tokens, cols);
        let status_right =
            context_tokens.map(|tokens| format_context_usage(tokens, *context_window));
        let mut band = BandCtx {
            composer,
            label,
            status,
            running,
            run_status,
            status_right,
            layout,
            theme,
            depth: *depth,
        };
        let render = band_render(approval_rows, &mut band);
        shell.on_resize(
            cols,
            rows,
            |max_rows, width| transcript.replay_tail(max_rows, width, highlighter, theme, *depth),
            render,
        )?;
        self.pump()
    }
}

/// One select-iteration's winner.
enum Tick {
    Input(Option<io::Result<Event>>),
    Feed(FeedMsg),
    Resize,
    Draw(Option<Draw>),
}

/// The feed branch: pends while no run is attached.
async fn feed_next(rx: &mut Option<mpsc::Receiver<FeedMsg>>) -> FeedMsg {
    match rx {
        Some(rx) => rx.recv().await.unwrap_or(FeedMsg::Outcome(Err(
            "the drainer died without a report".into(),
        ))),
        None => std::future::pending().await,
    }
}

/// The debounce branch: pends while no resize is pending.
async fn resize_next(debounce: &ResizeDebounce) {
    match debounce.fire_at() {
        Some(fire_at) => tokio::time::sleep_until(fire_at).await,
        None => std::future::pending().await,
    }
}

/// The band split for the current content (the height function's output).
fn band_layout<B: Backend<Error = io::Error> + Clone + Write, W: Write>(
    shell: &mut InlineShell<B, W>,
    composer: &Composer,
    receiving_rows: u16,
    approval_rows: usize,
    run_status_rows: u16,
) -> BandLayout {
    layout::layout(&LayoutInput {
        screen_rows: shell.screen_rows(),
        receiving_rows,
        approval_rows: u16::try_from(approval_rows).unwrap_or(u16::MAX),
        run_status_rows,
        composer_rows: composer.desired_rows(shell.width()),
    })
}

/// The floor's read side: the held split for a render that does not itself
/// change the height (the resize repaint, the resync marker's flush) — the
/// pump alone raises the high-water.
fn floored_layout(floor: Option<u16>, layout: BandLayout) -> BandLayout {
    match floor {
        Some(floor) => layout.held_at(floor),
        None => layout,
    }
}

/// The invariant half of a band render across one pump: every call differs
/// only in which rows it draws, so the context packs once and each call
/// site names just its rows. `composer` is a `&mut`: the render closure
/// scrolls it.
struct BandCtx<'a> {
    composer: &'a mut Composer,
    label: &'a str,
    status: &'a Status,
    /// Whether a run is active or its end-of-run drain is pending: an
    /// `Idle` light then reads as working (the state-truthfulness rule,
    /// see [`run_status_text`]) — a blank row would claim the run's
    /// display is over while it is not.
    running: bool,
    /// The run-status row's text for this pump (state word, detail),
    /// computed once at the render edge — the closure never re-derives it.
    run_status: (String, String),
    /// The floor row's right side for this pump (the context-usage ratio),
    /// computed at the render edge like the run status; `None` until the
    /// first usage lands, and the right side stays empty.
    status_right: Option<String>,
    layout: BandLayout,
    theme: &'a Theme,
    depth: ColorDepth,
}

/// A slot-only style resolved under the theme — the band's style literals
/// share the shape (ADR-0017 slot wiring).
fn slot_style(fg: Option<Slot>, bg: Option<Slot>, theme: &Theme, depth: ColorDepth) -> Style {
    ir_style(
        &ir::Style {
            fg: fg.map(ir::Color::Slot),
            bg: bg.map(ir::Color::Slot),
            ..ir::Style::default()
        },
        theme,
        depth,
    )
}

/// The band render closure: owned rows in, widgets drawn top to bottom —
/// the `receiving…` row (below the hold's slack), the approval section
/// (head-clipped to its slice), the run-status row, composer, floor status
/// line. Every slice is intersected with the frame area: during the
/// stale-frame window around a resize, the split math may exceed the band,
/// and clipping beats panicking.
fn band_render<'a>(
    approval: Vec<Line<'static>>,
    ctx: &'a mut BandCtx<'_>,
) -> impl FnOnce(&mut Frame<'_>) + 'a {
    let subtle = slot_style(Some(Slot::TextSubtle), None, ctx.theme, ctx.depth);
    let selection = slot_style(None, Some(Slot::Selection), ctx.theme, ctx.depth);
    // The input zone reads as a surface (BgSubtle) — the one break in the
    // transcript's flatness, double-coded with the accent prompt marker
    // (ADR-0017 slot wiring).
    let composer_base = slot_style(None, Some(Slot::BgSubtle), ctx.theme, ctx.depth);
    // The accent prompt marker, resolved here where the theme lives (the
    // composer owns the glyph, the theme owns the color).
    let prompt = slot_style(Some(Slot::Accent), None, ctx.theme, ctx.depth);
    let state_style = status_style(ctx.status, ctx.running, ctx.theme, ctx.depth);
    let status_left = ctx.label.to_string();
    let (status_word, status_detail) = ctx.run_status.clone();
    let status_right = ctx.status_right.clone();
    let layout = ctx.layout;
    let composer = &mut *ctx.composer;
    move |frame: &mut Frame<'_>| {
        let area = frame.area();
        let clip = |rect: Rect| rect.intersection(area);
        // The hold's slack pads the band's top; the content slices anchor
        // below it.
        let content_y = area.y + layout.slack_rows;
        if layout.receiving_rows > 0 {
            let line = Line::from(Span::styled(RECEIVING_TEXT, subtle));
            frame.render_widget(
                Paragraph::new(line),
                clip(Rect::new(
                    area.x,
                    content_y,
                    area.width,
                    layout.receiving_rows,
                )),
            );
        }
        if layout.approval_rows > 0 {
            // Head-clipped: the header and call lines are the
            // decision-relevant part; a clipped diff tail waits for the
            // cumulative, file-backed diff slice (layout docs).
            let take = usize::from(layout.approval_rows);
            let shown: Vec<Line> = approval.iter().take(take).cloned().collect();
            let approval_y = content_y + layout.receiving_rows;
            frame.render_widget(
                Paragraph::new(shown),
                clip(Rect::new(
                    area.x,
                    approval_y,
                    area.width,
                    layout.approval_rows,
                )),
            );
        }
        if layout.run_status_rows > 0 {
            let line = Line::from(vec![
                Span::styled(status_word, state_style),
                Span::styled(status_detail, subtle),
            ]);
            let run_status_y = content_y + layout.receiving_rows + layout.approval_rows;
            frame.render_widget(
                Paragraph::new(line),
                clip(Rect::new(
                    area.x,
                    run_status_y,
                    area.width,
                    layout.run_status_rows,
                )),
            );
        }
        if layout.composer_rows > 0 {
            let composer_y =
                content_y + layout.receiving_rows + layout.approval_rows + layout.run_status_rows;
            composer.render(
                clip(Rect::new(
                    area.x,
                    composer_y,
                    area.width,
                    layout.composer_rows,
                )),
                frame,
                composer_base,
                selection,
                prompt,
            );
        }
        if layout.status_rows > 0 {
            let status_y = area.bottom().saturating_sub(1);
            let status = status_line(&status_left, status_right.as_deref(), area.width, subtle);
            frame.render_widget(
                Paragraph::new(status),
                clip(Rect::new(area.x, status_y, area.width, 1)),
            );
        }
    }
}

/// The one-row floor status line (ADR-0011 floor: model, cwd+git,
/// context-usage %, session cost): the session label left-aligned, the
/// context-usage ratio right-aligned once known — both subtle, the label
/// yielding width to the usage. Session cost waits on a pricing source
/// (docs/open-items.md). The composable status surface of the 2026-09-11
/// amendment item 5 is its own slice. Padded to full width so a
/// once-longer frame never leaves stale cells behind.
fn status_line(label: &str, usage: Option<&str>, width: u16, style: Style) -> Line<'static> {
    let width = usize::from(width);
    let usage_width = usage.map_or(0, UnicodeWidthStr::width);
    // The label's budget excludes the usage and, when present, one
    // separating cell.
    let reserved = usage_width + usize::from(usage.is_some());
    let mut kept = String::new();
    let mut kept_width = 0;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(label, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if kept_width + grapheme_width + reserved > width {
            break;
        }
        kept.push_str(grapheme);
        kept_width += grapheme_width;
    }
    let pad = width.saturating_sub(kept_width + usage_width);
    let mut spans = vec![
        Span::styled(kept, style),
        Span::styled(" ".repeat(pad), style),
    ];
    if let Some(usage) = usage {
        spans.push(Span::styled(usage.to_string(), style));
    }
    Line::from(spans)
}

/// The run-status row's text at `width`, as (state word, detail): the word
/// renders in the status color (Info while active, Error on failed — the
/// state-truthfulness rule), the detail subtle. The word is "Working"
/// while the run works, "Waiting for approval" across a gate wait (the
/// clock is paused then) and "Failed" at the frozen end; the detail
/// carries the whole-second elapsed, the live tool's name + target (the
/// target is the width budget's first casualty) and the context size once
/// the first response's usage lands. `running` records whether a run is
/// active: between the terminal record and the outcome the light already
/// reads `Idle` but the run still is, so the row reads "Working" through
/// the gap — a blank row would claim the run is over while it is not.
fn run_status_text(
    status: &Status,
    running: bool,
    elapsed: Duration,
    waiting: bool,
    context_tokens: Option<u64>,
    width: u16,
) -> (String, String) {
    let elapsed = format_elapsed(elapsed);
    if waiting {
        return ("Waiting for approval".into(), format!(" · {elapsed}"));
    }
    let tokens = context_tokens.map_or(String::new(), |t| format!(" · {}", format_tokens(t)));
    match status {
        // The settling gap between the terminal record and the outcome:
        // the run is still active, so the truthful word is Working.
        Status::Idle if running => ("Working".into(), format!(" · {elapsed}{tokens}")),
        // The row's height is 0 while idle; this text never renders.
        Status::Idle => (String::new(), String::new()),
        Status::Failed => ("Failed".into(), format!(" · {elapsed}")),
        Status::Streaming => ("Working".into(), format!(" · {elapsed}{tokens}")),
        Status::Tool { name, target } => {
            let prefix = format!(" · {elapsed} · {name}");
            let Some(target) = target else {
                return ("Working".into(), format!("{prefix}{tokens}"));
            };
            // The target's budget: whatever the word, the fixed detail and
            // the tokens leave. `truncate_cells`' ellipsis can overshoot
            // its budget by one cell, so the budget gives up one — the
            // assembled line then never exceeds the row's width.
            let fixed = UnicodeWidthStr::width("Working")
                + UnicodeWidthStr::width(prefix.as_str())
                + UnicodeWidthStr::width(tokens.as_str())
                + 1; // the target's separating space
            let budget = usize::from(width).saturating_sub(fixed);
            if budget == 0 {
                ("Working".into(), format!("{prefix}{tokens}"))
            } else {
                (
                    "Working".into(),
                    format!(
                        "{prefix} {}{tokens}",
                        truncate_cells(target, budget.saturating_sub(1))
                    ),
                )
            }
        }
    }
}

/// The run clock's display: whole seconds — `12s`, `2m 05s`, `1h 02m 03s`
/// (the 1 Hz tick's resolution; sub-second precision is noise on a row
/// that updates once a second). The one home, shared by the run-status row
/// and the scrollback completion rows.
fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1000 {
        format!("{:.1}k tokens", tokens as f64 / 1000.0)
    } else {
        format!("{tokens} tokens")
    }
}

/// The floor's context-usage field: `45.2k/128k (35%)` — the latest
/// response's context size over the model's declared window
/// (`Capabilities::max_context`), the percent rounded to whole points.
fn format_context_usage(tokens: u64, window: u64) -> String {
    let pct = (u128::from(tokens) * 100 + u128::from(window) / 2) / u128::from(window.max(1));
    format!(
        "{}/{} ({pct}%)",
        compact_count(tokens),
        compact_count(window)
    )
}

/// The usage ratio's compact counts: one decimal under a million, trimmed
/// of a trailing `.0` (`45.2k`, `128k`); millions keep their decimal
/// (`1.0M`).
#[allow(clippy::cast_precision_loss)]
fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1e6)
    } else if value >= 1_000 {
        let text = format!("{:.1}k", value as f64 / 1e3);
        match text.strip_suffix(".0k") {
            Some(trimmed) => format!("{trimmed}k"),
            None => text,
        }
    } else {
        value.to_string()
    }
}

/// The status state's color, derived from the event-driven state itself
/// (the state-truthfulness rule): active states read in `Info`, a failure
/// in `Error`, idle stays subtle — except mid-run, where the `Idle` light
/// of the settling gap still reads Working and keeps the active color.
fn status_style(status: &Status, running: bool, theme: &Theme, depth: ColorDepth) -> Style {
    let slot = match status {
        Status::Idle if running => Slot::Info,
        Status::Idle => Slot::TextSubtle,
        Status::Streaming | Status::Tool { .. } => Slot::Info,
        Status::Failed => Slot::Error,
    };
    slot_style(Some(slot), None, theme, depth)
}

/// A cloneable handle onto the process's stdout: handles are cheap
/// references to the one global stream (each write locks it afresh), so
/// cloning is free and the shell's recreation gets a handle onto the same
/// terminal (the shell's `Clone` bound is about outliving the `Terminal`,
/// not about owning the stream).
#[derive(Clone, Copy, Default)]
struct Stdout;

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// RAII: raw mode and bracketed paste restore on every path.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        use crossterm::event::DisableBracketedPaste;
        use crossterm::terminal::disable_raw_mode;
        let _ = execute!(Stdout, DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

/// Boot the real terminal session (raw mode, bracketed paste, the inline
/// shell) and run the app until quit; the terminal is restored on every
/// exit path. App-boundary IO lives here and nowhere else in the crate.
pub async fn run(driver: Box<dyn RunDriver>, config: AppConfig) -> io::Result<()> {
    use crossterm::event::EnableBracketedPaste;
    use crossterm::terminal::enable_raw_mode;

    enable_raw_mode()?;
    execute!(Stdout, EnableBracketedPaste)?;
    let _restore = Restore;

    let backend = CrosstermBackend::new(Stdout);
    // The session's one cursor-position query: the tracker's seed must
    // precede the input broker — its parked reader thread holds crossterm's
    // global event lock, and any later query stalls out behind it (the
    // cursor.rs module docs). Every post-boot anchor query is answered from
    // tracked state instead.
    let backend = CursorTracker::new(backend)?;
    let screen_rows = backend.size()?.height;
    let band = layout::layout(&LayoutInput {
        screen_rows,
        receiving_rows: 0,
        approval_rows: 0,
        run_status_rows: 0,
        composer_rows: 1,
    })
    .band_height;
    // The shell is built BEFORE the input broker exists: the tracker's
    // seed query above needs crossterm's global event reader uncontended,
    // and an EventStream's create-drop cycle would leave a stale wake edge
    // that a query could read as an instant timeout (input.rs).
    let shell = InlineShell::new(
        backend,
        Stdout,
        band,
        crate::shell::ScrollbackStrategy::detect(),
    )?;
    let input = InputBroker::new();
    let mut app = App::new(shell, input, driver, config).with_pacing(crate::style::detect_paced());
    app.run_loop().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats_in_whole_seconds() {
        assert_eq!(format_elapsed(Duration::ZERO), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(12)), "12s");
        assert_eq!(format_elapsed(Duration::from_millis(59_900)), "59s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 05s");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "1h 02m 03s");
    }

    #[test]
    fn the_floor_usage_is_a_ratio_of_the_declared_window() {
        assert_eq!(format_context_usage(45_200, 128_000), "45.2k/128k (35%)");
        assert_eq!(format_context_usage(12_345, 1_000_000), "12.3k/1.0M (1%)");
        assert_eq!(format_context_usage(999, 8_000), "999/8k (12%)");
    }

    #[test]
    fn the_floor_line_right_aligns_usage_and_the_label_yields() {
        let text = |line: Line<'static>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        // Room for both: the usage right-aligns, padded between.
        let line = status_line("kimi·k2", Some("45.2k/128k (35%)"), 80, Style::default());
        assert_eq!(
            text(line),
            format!("kimi·k2{}45.2k/128k (35%)", " ".repeat(57))
        );
        // No usage yet: the label keeps the whole row (the empty right side).
        let line = status_line("kimi·k2", None, 10, Style::default());
        assert_eq!(text(line), "kimi·k2   ");
        // A tight row: the label yields to the usage, one separating cell
        // kept (23 + 1 + 16 = 40).
        let line = status_line(
            "kimi·k2 ~/proj (git:main)*",
            Some("45.2k/128k (35%)"),
            40,
            Style::default(),
        );
        assert_eq!(text(line), "kimi·k2 ~/proj (git:mai 45.2k/128k (35%)");
    }

    #[test]
    fn the_run_status_row_names_the_state_the_clock_and_the_tool() {
        // Streaming: the word and the clock; tokens join once known.
        let (word, detail) = run_status_text(
            &Status::Streaming,
            true,
            Duration::from_secs(12),
            false,
            None,
            80,
        );
        assert_eq!((word.as_str(), detail.as_str()), ("Working", " · 12s"));
        let (word, detail) = run_status_text(
            &Status::Streaming,
            true,
            Duration::from_secs(12),
            false,
            Some(45_200),
            80,
        );
        assert_eq!(
            (word.as_str(), detail.as_str()),
            ("Working", " · 12s · 45.2k tokens")
        );
        // A tool: name + target after the clock, tokens still last.
        let tool = Status::Tool {
            name: "read_file".into(),
            target: Some("src/main.rs".into()),
        };
        let (word, detail) = run_status_text(&tool, true, Duration::from_secs(12), false, None, 80);
        assert_eq!(
            (word.as_str(), detail.as_str()),
            ("Working", " · 12s · read_file src/main.rs")
        );
        // A gate wait overrides the state word (the clock is paused then).
        let (word, detail) =
            run_status_text(&tool, true, Duration::from_secs(12), true, Some(45_200), 80);
        assert_eq!(
            (word.as_str(), detail.as_str()),
            ("Waiting for approval", " · 12s")
        );
        // Failed: the frozen elapsed, no tokens.
        let (word, detail) = run_status_text(
            &Status::Failed,
            false,
            Duration::from_secs(12),
            false,
            Some(45_200),
            80,
        );
        assert_eq!((word.as_str(), detail.as_str()), ("Failed", " · 12s"));
        // The settling gap: the terminal record's Idle light while the run
        // is still active reads Working; only a run-less Idle blanks (and
        // the row's height is 0 then, so it never renders).
        let (word, detail) = run_status_text(
            &Status::Idle,
            true,
            Duration::from_secs(12),
            false,
            Some(45_200),
            80,
        );
        assert_eq!(
            (word.as_str(), detail.as_str()),
            ("Working", " · 12s · 45.2k tokens")
        );
        let (word, detail) = run_status_text(
            &Status::Idle,
            false,
            Duration::from_secs(12),
            false,
            None,
            80,
        );
        assert_eq!((word.as_str(), detail.as_str()), ("", ""));
    }

    #[test]
    fn the_run_status_row_truncates_the_target_to_fit() {
        let tool = Status::Tool {
            name: "read_file".into(),
            target: Some("a/very/long/path/that/keeps/going/and/going.rs".into()),
        };
        let (word, detail) = run_status_text(&tool, true, Duration::from_secs(12), false, None, 40);
        let line = format!("{word}{detail}");
        assert_eq!(line, "Working · 12s · read_file a/very/long/p…");
        assert!(
            UnicodeWidthStr::width(line.as_str()) <= 40,
            "line: {line:?}"
        );
        // No room at all: the target drops rather than overflowing.
        let (word, detail) = run_status_text(&tool, true, Duration::from_secs(12), false, None, 24);
        assert_eq!(format!("{word}{detail}"), "Working · 12s · read_file");
    }
}
