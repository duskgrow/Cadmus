//! The app: the ADR-0018 event loop driving the inline shell (item 5) over
//! the view-model materialization (item 10, [`crate::transcript`]). One
//! `select!` over five sources — terminal input, the run's live feed, the
//! run's outcome, the resize debounce and the frame scheduler's draw ticks —
//! with input never blocking and frames demand-driven.
//!
//! The draw pump is the one place rows materialize: per tick it takes one
//! [`Transcript::snapshot`] and the flush, band render and layout all read
//! off it (the per-accessor re-render open item's consumer). Stream draining
//! is the two-regime policy of item 5 in its cheap form: items apply one
//! select-iteration at a time (typewriter), and every branch drains whatever
//! the feed already queued before the next wait (catch-up) — a draw always
//! sees the fully drained state. The pure-function hysteresis over queue
//! depth and age stays deferred: it gates *materialization*, which only the
//! snapshot's measurement can justify.
//!
//! Key handling is a minimal fixed map (chars, editing ops, Enter submits,
//! Esc interrupts, Ctrl-C quits, Tab selects a call and y/n answer it) —
//! ADR-0018 item 6's mode × key → command layer with keymap-as-data is its
//! own slice, and the default bindings are decided in its binding-design
//! task.

use std::collections::VecDeque;
use std::io::{self, Write};

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
use crate::composer::Composer;
use crate::cursor::CursorTracker;
use crate::debounce::ResizeDebounce;
use crate::frame::{Draw, FrameRequester, frame_scheduler};
use crate::input::{EventSource, InputBroker};
use crate::layout::{self, BandLayout, LayoutInput};
use crate::shell::InlineShell;
use crate::stream::Stream;
use crate::style::ir_style;
use crate::transcript::{Light, Transcript};
use crate::wrap::wrap_rows;

/// The drainer's bounded forwarding depth: a stalled app lags the
/// broadcaster and is told to re-sync — never awaited (ADR-0013 item 5).
const FEED_DEPTH: usize = 1_024;

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

/// The status line's right side — the state-truthfulness rule (ADR-0011's
/// 2026-09-11 amendment item 6): derived from events, never guessed.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Status {
    Idle,
    Streaming,
    Tool(String),
    Failed,
}

impl Status {
    fn text(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Streaming => "streaming".into(),
            Self::Tool(name) => format!("→ {name}"),
            Self::Failed => "failed".into(),
        }
    }

    fn note(&mut self, light: Light) {
        match light {
            Light::None => {}
            Light::Streaming => *self = Self::Streaming,
            Light::Tool(name) => *self = Self::Tool(name),
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

/// The app. See the module docs.
pub struct App<B: Backend<Error = io::Error> + Clone, W: Write, I: EventSource> {
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
    /// The session's conversation: each run's outcome replaces it.
    history: Vec<Message>,
    run: Option<ActiveRun>,
    feed: Option<mpsc::Receiver<FeedMsg>>,
    status: Status,
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

impl<B: Backend<Error = io::Error> + Clone, W: Write, I: EventSource> App<B, W, I> {
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
            history: Vec::new(),
            run: None,
            feed: None,
            status: Status::Idle,
            command_seq: 0,
            approvals: VecDeque::new(),
            quit: false,
        }
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

    /// The draw pump: one snapshot per tick drives the flush, the band
    /// render and the layout input (module docs).
    fn pump(&mut self) -> io::Result<()> {
        let Self {
            shell,
            transcript,
            composer,
            highlighter,
            theme,
            depth,
            label,
            status,
            approvals,
            ..
        } = &mut *self;
        let width = shell.width();
        let snapshot = transcript.snapshot(width, highlighter, theme, *depth);
        // The approval section wraps its cached lines at this width: the row
        // count feeds the height function and the rows themselves feed the
        // band render, so flush math and pixels can never disagree (the
        // snapshot contract's shape).
        let approval_rows = wrap_section(approvals, width, theme, *depth);
        let layout = band_layout(
            shell,
            composer,
            snapshot.live_rows.len(),
            approval_rows.len(),
        );
        let mut band = BandCtx {
            composer,
            label,
            status,
            layout,
            theme,
            depth: *depth,
        };
        if !snapshot.flush_rows.is_empty() {
            let render = band_render(snapshot.live_rows.clone(), approval_rows.clone(), &mut band);
            // Flush first, then confirm: the ack contract is a *successful*
            // shell flush, so a structural failure must die un-acked.
            shell.flush(&snapshot.flush_rows, render)?;
        }
        transcript.apply_flush(&snapshot.acks);
        if shell.needs_height_change(layout.band_height) {
            // The recreation seam: the cursor tracker answers the anchor
            // query without a CPR round-trip (cursor.rs), so no quiesce
            // window — the event stream stays live across recreation.
            let render = band_render(snapshot.live_rows.clone(), approval_rows.clone(), &mut band);
            shell.set_height(layout.band_height, render)?;
        }
        let render = band_render(snapshot.live_rows, approval_rows, &mut band);
        shell.draw(render);
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
        self.composer.clear();
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

    fn on_feed(&mut self, msg: FeedMsg) -> io::Result<()> {
        match msg {
            FeedMsg::Item(item) => {
                // The one client rule (ADR-0013 item 4) guards the dialog
                // queue too: a stale request must not reopen it. The
                // baseline is read before the item applies.
                let fresh = item.seq > self.transcript.as_of_seq();
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
                self.requester.schedule_frame();
            }
            FeedMsg::Sync { sync, resync } => {
                // Pump first: the transfer counts on everything flushable
                // already being in scrollback (transcript docs).
                self.pump()?;
                let width = self.shell.width();
                self.transcript
                    .apply_sync(&sync, width, &self.highlighter, resync);
                // The attach baseline is authoritative for the dialog queue
                // as well: an attach mid-wait replays the pending request(s)
                // (ADR-0013 item 3), a lag re-attach drops what settled in
                // the hole.
                self.sync_approvals(&sync.in_flight.pending_approvals);
                if resync {
                    // The hole marker: flushed directly, outside the block
                    // model, so replays never reorder it (transcript docs).
                    let marker = wrap_rows(
                        &[ir::Line::from_spans(vec![ir::Span::slotted(
                            "… resynced …",
                            Slot::TextSubtle,
                        )])],
                        width,
                        &self.theme,
                        self.depth,
                    );
                    if !marker.is_empty() {
                        let snapshot = self.transcript.snapshot(
                            width,
                            &self.highlighter,
                            &self.theme,
                            self.depth,
                        );
                        let approval_rows =
                            wrap_section(&self.approvals, width, &self.theme, self.depth);
                        let layout = band_layout(
                            &mut self.shell,
                            &self.composer,
                            snapshot.live_rows.len(),
                            approval_rows.len(),
                        );
                        let mut band = BandCtx {
                            composer: &mut self.composer,
                            label: &self.label,
                            status: &self.status,
                            layout,
                            theme: &self.theme,
                            depth: self.depth,
                        };
                        let render = band_render(snapshot.live_rows, approval_rows, &mut band);
                        self.shell.flush(&marker, render)?;
                    }
                }
                self.requester.schedule_frame();
            }
            // The feed's last message: the tail drained, then the report —
            // teardown in stream order (the drainer docs).
            FeedMsg::Outcome(outcome) => {
                self.feed = None;
                self.run = None;
                // A dead run owns no dialog: its gate already denied every
                // pending request (the channel-drop rule).
                self.approvals.clear();
                match outcome {
                    Ok(messages) => {
                        self.history = messages;
                        self.status = Status::Idle;
                    }
                    Err(text) => self.fail_run(&text),
                }
                self.requester.schedule_frame();
            }
        }
        Ok(())
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
        // Materialize at the new width first: the replay closure and the
        // band render both read from source, never from the old frame.
        let band_rows =
            self.transcript
                .unflushed_rows(cols, &self.highlighter, &self.theme, self.depth);
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
            ..
        } = &mut *self;
        let layout = band_layout(shell, composer, band_rows.len(), approval_rows.len());
        let mut band = BandCtx {
            composer,
            label,
            status,
            layout,
            theme,
            depth: *depth,
        };
        let render = band_render(band_rows, approval_rows, &mut band);
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
fn band_layout<B: Backend<Error = io::Error> + Clone, W: Write>(
    shell: &mut InlineShell<B, W>,
    composer: &Composer,
    live_rows: usize,
    approval_rows: usize,
) -> BandLayout {
    layout::layout(&LayoutInput {
        screen_rows: shell.screen_rows(),
        stream_rows: u16::try_from(live_rows).unwrap_or(u16::MAX),
        approval_rows: u16::try_from(approval_rows).unwrap_or(u16::MAX),
        composer_rows: composer.desired_rows(shell.width()),
    })
}

/// The invariant half of a band render across one pump: every call differs
/// only in which rows it draws, so the context packs once and each call
/// site names just its rows. `composer` is a `&mut`: the render closure
/// scrolls it.
struct BandCtx<'a> {
    composer: &'a mut Composer,
    label: &'a str,
    status: &'a Status,
    layout: BandLayout,
    theme: &'a Theme,
    depth: ColorDepth,
}

/// The band render closure: owned rows in, widgets drawn top to bottom —
/// stream tail (bottom-anchored in its slice), the approval section
/// (head-clipped to its slice), composer, status line. Every slice is
/// intersected with the frame area: during the stale-frame window around a
/// resize, the split math may exceed the band, and clipping beats
/// panicking.
fn band_render<'a>(
    rows: Vec<Line<'static>>,
    approval: Vec<Line<'static>>,
    ctx: &'a mut BandCtx<'_>,
) -> impl FnOnce(&mut Frame<'_>) + 'a {
    let subtle = ir_style(
        &ir::Style {
            fg: Some(ir::Color::Slot(Slot::TextSubtle)),
            ..ir::Style::default()
        },
        ctx.theme,
        ctx.depth,
    );
    let selection = ir_style(
        &ir::Style {
            bg: Some(ir::Color::Slot(Slot::Selection)),
            ..ir::Style::default()
        },
        ctx.theme,
        ctx.depth,
    );
    // The input zone reads as a surface (BgSubtle) — the one break in the
    // transcript's flatness, double-coded with the accent prompt marker
    // (ADR-0017 slot wiring).
    let composer_base = ir_style(
        &ir::Style {
            bg: Some(ir::Color::Slot(Slot::BgSubtle)),
            ..ir::Style::default()
        },
        ctx.theme,
        ctx.depth,
    );
    let state_style = status_style(ctx.status, ctx.theme, ctx.depth);
    let status_left = ctx.label.to_string();
    let status_right = ctx.status.text();
    let layout = ctx.layout;
    let composer = &mut *ctx.composer;
    move |frame: &mut Frame<'_>| {
        let area = frame.area();
        let clip = |rect: Rect| rect.intersection(area);
        let shown = Stream::visible(&rows, layout.stream_rows);
        let shown_len = u16::try_from(shown.len()).unwrap_or(u16::MAX);
        let stream_y = area.y + layout.stream_rows.saturating_sub(shown_len);
        frame.render_widget(
            Paragraph::new(shown),
            clip(Rect::new(area.x, stream_y, area.width, shown_len)),
        );
        if layout.approval_rows > 0 {
            // Head-clipped: the header and call lines are the
            // decision-relevant part; a clipped diff tail waits for the
            // cumulative, file-backed diff slice (layout docs).
            let take = usize::from(layout.approval_rows);
            let shown: Vec<Line> = approval.iter().take(take).cloned().collect();
            let approval_y = area.y + layout.stream_rows;
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
        if layout.composer_rows > 0 {
            let composer_y = area.y + layout.stream_rows + layout.approval_rows;
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
            );
        }
        if layout.status_rows > 0 {
            let status_y = area.bottom().saturating_sub(1);
            let status = status_line(&status_left, &status_right, area.width, subtle, state_style);
            frame.render_widget(
                Paragraph::new(status),
                clip(Rect::new(area.x, status_y, area.width, 1)),
            );
        }
    }
}

/// The one-row status line: label left, run state right (ADR-0011 floor;
/// the composable status surface of the 2026-09-11 amendment item 5 is its
/// own slice). The label and padding stay subtle; the state carries its
/// status color (Info while active, Error on failure) — double-coded with
/// the text per ADR-0017.
fn status_line(
    label: &str,
    state: &str,
    width: u16,
    style: Style,
    state_style: Style,
) -> Line<'static> {
    let width = usize::from(width);
    let state_width = UnicodeWidthStr::width(state);
    let budget = width.saturating_sub(state_width + 2);
    let mut kept = String::new();
    let mut kept_width = 0;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(label, true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if kept_width + grapheme_width > budget {
            break;
        }
        kept.push_str(grapheme);
        kept_width += grapheme_width;
    }
    let pad = width.saturating_sub(kept_width + state_width);
    Line::from(vec![
        Span::styled(kept, style),
        Span::styled(" ".repeat(pad), style),
        Span::styled(state.to_string(), state_style),
    ])
}

/// The status state's color, derived from the event-driven state itself
/// (the state-truthfulness rule): active states read in `Info`, a failure
/// in `Error`, idle stays subtle.
fn status_style(status: &Status, theme: &Theme, depth: ColorDepth) -> Style {
    let slot = match status {
        Status::Idle => Slot::TextSubtle,
        Status::Streaming | Status::Tool(_) => Slot::Info,
        Status::Failed => Slot::Error,
    };
    ir_style(
        &ir::Style {
            fg: Some(ir::Color::Slot(slot)),
            ..ir::Style::default()
        },
        theme,
        depth,
    )
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
        stream_rows: 0,
        approval_rows: 0,
        composer_rows: 1,
    })
    .band_height;
    // The shell is built BEFORE the input broker exists: the tracker's
    // seed query above needs crossterm's global event reader uncontended,
    // and an EventStream's create-drop cycle would leave a stale wake edge
    // that a query could read as an instant timeout (input.rs).
    let shell = InlineShell::new(backend, Stdout, band)?;
    let input = InputBroker::new();
    let mut app = App::new(shell, input, driver, config);
    app.run_loop().await
}
