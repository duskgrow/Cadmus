//! The app loop, locked end to end with vt100 (ADR-0018 items 5 and 10):
//! keys drive the composer, Enter spawns a run through the scripted driver
//! (mid-run it sends an inject steer, Tab a queued one — the 2026-09-21
//! binding amendment), stable rows type out of the emission queue into
//! scrollback at the paced budget (the 2026-09-20 second amendment), the
//! unstable tail is never
//! rendered (the `receiving…` row carries the liveness signal), Esc sends
//! the interrupt command and dumps the queue whole, and the outcome folds
//! back into the session history. The rig shares `tests/common`'s vt100
//! world; the strongest assertion is again the full non-blank row sequence
//! (scrollback + screen, oldest first) — and for the run high-water hold
//! (ADR-0018's 2026-09-20 amendment) its blank-preserving sibling, which
//! pins exactly where every blank row is allowed to be.
//!
//! Time is tokio's paused clock (frame gate, debounce, the pacing horizon);
//! the drainer thread is real-time and only *scheduled*, never awaited, so
//! nothing here may assume when its delivery lands: the waits hop in real time
//! until the world shows the effect (`flush_feed_until`). Every real-time wait
//! goes through `real_wait`, which inhibits the paused clock's auto-advance, so
//! the clock moves only where this suite advances it — the invariant the
//! tick-counting assertions rest on (and the one the Windows job caught).

mod common;

use std::sync::{Arc, Mutex};

use cadmus_contract::{
    Approval, Attachment, Command, Event as TraceEvent, EventKind, InFlight, LiveItem, LiveKind,
    LiveUpdate, Message, PendingApproval, RunState, SettledApproval, SteerMode, StreamChunk, Sync,
    ToolCall, ToolCompletion,
};
use cadmus_tui::app::{App, AppConfig, RunDriver, RunHandle};
use cadmus_tui::config::Motion;
use cadmus_tui::shell::{InlineShell, ScrollbackStrategy};
use cadmus_ui::theme::{ColorDepth, Theme};
use common::{GuardSink, ScriptedInput, World};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tokio::task::yield_now;
use tokio::time::{Duration, advance};

/// One scripted run's test-side handles: the live feed to publish into and
/// the report to send (the drainer forwards it last, as in production).
struct RunSlots {
    live: std::sync::mpsc::Sender<LiveUpdate>,
    outcome: std::sync::mpsc::Sender<Result<Vec<Message>, String>>,
}

impl RunSlots {
    fn record(&self, seq: u64, kind: EventKind) {
        self.live.send(recorded(seq, 1, kind)).expect("feed");
    }
}

fn has_row(rows: &[String], text: &str) -> bool {
    rows.iter().any(|row| row.contains(text))
}

const COMPOSER_PLACEHOLDER: &str = "❯ Ask anything";

/// The composer's placeholder while a run is active: the keys that work
/// mid-run (the steer pair and Esc — the 2026-09-21 binding amendment).
const BUSY_PLACEHOLDER: &str = "❯ Enter to steer · Tab to queue · Esc to interrupt";

/// The busy placeholder while the approval dialog is open: the dialog owns
/// Tab, so the composer names only the keys still its own.
const DIALOG_PLACEHOLDER: &str = "❯ Enter to steer · Esc to interrupt";

fn call_decision(
    command_id: &str,
    request_id: &str,
    call_index: usize,
    decision: Approval,
) -> Command {
    Command::ResolveApprovalCall {
        command_id: command_id.into(),
        request_id: request_id.into(),
        call_index,
        decision,
    }
}

/// The scripted session boundary: `start` records the submitted messages
/// and hands back a run whose feed and outcome the test drives.
struct ScriptDriver {
    submitted: Arc<Mutex<Vec<Vec<Message>>>>,
    commands: Arc<Mutex<Vec<Command>>>,
    runs: Arc<Mutex<Vec<RunSlots>>>,
    /// The attach baseline's pending approvals: every started run's sync
    /// carries them, scripting an attach mid-wait (ADR-0013 item 3).
    pending: Arc<Mutex<Vec<PendingApproval>>>,
    /// The attach baseline's settled approvals: every started run's sync
    /// carries them, scripting an attach after the settle (the rebuilt
    /// transcript's explicit record).
    settled: Arc<Mutex<Vec<SettledApproval>>>,
    /// A full sync override for the started runs (a history-carrying
    /// baseline for the replay tests); falls back to `sync_with_pending`.
    baseline: Arc<Mutex<Option<Sync>>>,
    reattachment: Arc<Mutex<Option<Attachment>>>,
}

impl ScriptDriver {
    fn new() -> Self {
        Self {
            submitted: Arc::new(Mutex::new(Vec::new())),
            commands: Arc::new(Mutex::new(Vec::new())),
            runs: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(Vec::new())),
            settled: Arc::new(Mutex::new(Vec::new())),
            baseline: Arc::new(Mutex::new(None)),
            reattachment: Arc::new(Mutex::new(None)),
        }
    }

    fn take_run(&self) -> RunSlots {
        self.runs
            .lock()
            .expect("runs")
            .pop()
            .expect("a submitted run waits for its slots")
    }

    fn submitted(&self) -> Vec<Vec<Message>> {
        self.submitted.lock().expect("submitted").clone()
    }

    fn commands(&self) -> Vec<Command> {
        self.commands.lock().expect("commands").clone()
    }
}

impl RunDriver for ScriptDriver {
    fn start(&self, messages: Vec<Message>) -> RunHandle {
        self.submitted.lock().expect("submitted").push(messages);
        let (live_tx, live_rx) = std::sync::mpsc::channel();
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        self.runs.lock().expect("runs").push(RunSlots {
            live: live_tx,
            outcome: outcome_tx,
        });
        let commands = Arc::clone(&self.commands);
        let pending = self.pending.lock().expect("pending").clone();
        let settled = self.settled.lock().expect("settled").clone();
        let baseline = self.baseline.lock().expect("baseline").clone();
        let reattachment = Arc::clone(&self.reattachment);
        RunHandle {
            attachment: Attachment {
                sync: baseline.unwrap_or_else(|| sync_with_pending(pending, settled)),
                tail: Box::new(live_rx.into_iter()),
            },
            reattach: Box::new(move || {
                reattachment
                    .lock()
                    .expect("reattachment")
                    .take()
                    .expect("scripted re-attach")
            }),
            commands: Box::new(move |command| {
                commands.lock().expect("commands").push(command);
            }),
            teardown: outcome_rx,
        }
    }
}

fn sync_with_pending(pending: Vec<PendingApproval>, settled: Vec<SettledApproval>) -> Sync {
    Sync {
        history: RunState {
            trace_id: "tr-test".into(),
            provider: None,
            model: None,
            messages: Vec::new(),
            tool_results: Vec::new(),
            turns: 0,
            warnings: Vec::new(),
            scores: Vec::new(),
            dangling_tool_calls: Vec::new(),
            finished: None,
        },
        in_flight: InFlight {
            open_turn: None,
            pending_approvals: pending,
            completed_tools: Vec::new(),
        },
        settled_approvals: settled,
        as_of_seq: 0,
    }
}

struct Rig {
    driver: ScriptDriver,
    input: tokio::sync::mpsc::UnboundedSender<Event>,
}

fn boot(world: &World) -> (App<common::VtBackend, GuardSink, ScriptedInput>, Rig) {
    boot_with_pending(world, Vec::new())
}

fn boot_unpaced(world: &World) -> (App<common::VtBackend, GuardSink, ScriptedInput>, Rig) {
    // The TERM=dumb motion profile: the paced typewriter is off.
    boot_with_config(world, Vec::new(), Vec::new(), Motion::None)
}

fn boot_with_pending(
    world: &World,
    pending: Vec<PendingApproval>,
) -> (App<common::VtBackend, GuardSink, ScriptedInput>, Rig) {
    boot_with_baseline(world, pending, Vec::new())
}

fn boot_with_baseline(
    world: &World,
    pending: Vec<PendingApproval>,
    settled: Vec<SettledApproval>,
) -> (App<common::VtBackend, GuardSink, ScriptedInput>, Rig) {
    boot_with_config(world, pending, settled, Motion::Full)
}

fn boot_with_config(
    world: &World,
    pending: Vec<PendingApproval>,
    settled: Vec<SettledApproval>,
    motion: Motion,
) -> (App<common::VtBackend, GuardSink, ScriptedInput>, Rig) {
    let (input_tx, input) = ScriptedInput::channel();
    let guard = GuardSink::default();
    let shell = InlineShell::new(
        world.backend.clone(),
        guard,
        2,
        ScrollbackStrategy::FullScreen,
    )
    .expect("boot shell");
    let driver = ScriptDriver::new();
    *driver.pending.lock().expect("pending") = pending;
    *driver.settled.lock().expect("settled") = settled;
    let app = App::new(
        shell,
        input,
        Box::new(driver.clone_handles()),
        AppConfig {
            label: "kimi·k2".into(),
            context_window: 128_000,
            refresh_label: None,
            motion,
            theme: Theme::ansi(),
            depth: ColorDepth::Truecolor,
        },
    );
    (
        app,
        Rig {
            driver,
            input: input_tx,
        },
    )
}

impl ScriptDriver {
    /// The driver handed to the app shares these handles (the rig keeps the
    /// read side).
    fn clone_handles(&self) -> Self {
        Self {
            submitted: Arc::clone(&self.submitted),
            commands: Arc::clone(&self.commands),
            runs: Arc::clone(&self.runs),
            pending: Arc::clone(&self.pending),
            settled: Arc::clone(&self.settled),
            baseline: Arc::clone(&self.baseline),
            reattachment: Arc::clone(&self.reattachment),
        }
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn type_text(rig: &Rig, text: &str) {
    for c in text.chars() {
        rig.input
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )))
            .expect("input");
    }
}

fn delta(seq: u64, turn: u32, text: &str) -> LiveUpdate {
    LiveUpdate::Item {
        item: Box::new(LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::AssistantDelta {
                turn,
                chunk: StreamChunk::TextDelta(text.into()),
            },
        }),
    }
}

fn recorded(seq: u64, turn: u32, kind: EventKind) -> LiveUpdate {
    LiveUpdate::Item {
        item: Box::new(LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::Recorded {
                event: Box::new(
                    TraceEvent::new(
                        seq,
                        format!("ev-{seq}"),
                        "tr-test".into(),
                        "sp-1".into(),
                        None,
                        0,
                        kind,
                    )
                    .with_attribute(cadmus_contract::attrs::TURN, u64::from(turn)),
                ),
            },
        }),
    }
}

fn llm_response(seq: u64, turn: u32, text: &str) -> LiveUpdate {
    recorded(
        seq,
        turn,
        EventKind::LlmResponse {
            message: Message::text(cadmus_contract::Role::Assistant, text),
            usage: None,
            finish: cadmus_contract::FinishReason::Stop,
            outcome: cadmus_contract::TurnOutcome::Content,
            warnings: Vec::new(),
        },
    )
}

/// Pump the loop until every queued effect lands: frame gate + a few
/// cooperative yields for the real-time drainer thread.
async fn settle() {
    for _ in 0..4 {
        yield_now().await;
        advance(Duration::from_millis(20)).await;
        yield_now().await;
    }
}

/// The drainer thread runs on real time while the test clock is paused, so
/// feed/outcome effects can't be awaited by a fixed pump count: pump until
/// the world shows them, with a real-time backstop for slow CI machines.
async fn settle_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..500 {
        yield_now().await;
        advance(Duration::from_millis(20)).await;
        yield_now().await;
        if condition() {
            // One more pump so a trailing frame renders the new state.
            yield_now().await;
            advance(Duration::from_millis(20)).await;
            yield_now().await;
            return;
        }
        real_wait().await;
    }
    panic!("the world never reached the expected state");
}

/// The delivery window's hop budget (`FEED_HOPS` ≈ 1 ms each). The predicate
/// form is budgeted in hops and one frame step, never in tries: only that
/// generous real-time backstop — and never a tuned window — is a timing
/// assumption about the machine.
const FEED_HOPS: usize = 32;

/// One real-time hop that leaves the paused clock alone, and the only way the
/// suite waits in real time. While a blocking task runs, tokio inhibits the
/// paused clock's auto-advance (`time::pause`'s documented seam; `start_paused`
/// guarantees the current-thread runtime the inhibit needs), so the park this
/// wait puts the loop in wakes on real deliveries — a bare park instead jumps
/// the clock to the next timer, firing the 33 ms pacing horizon or the 1 Hz
/// clock tick mid-window. Virtual time then moves only where this suite
/// advances it, which every tick-counting assertion depends on.
async fn real_wait() {
    tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(1)))
        .await
        .expect("the real-time wait joins");
}

/// One delivery hop: the drainer is only ever *scheduled*, never awaited, so
/// the loop is pumped on real time — no virtual advance, so the frame gate
/// alone decides whether a pump fires.
async fn feed_hop() {
    yield_now().await;
    real_wait().await;
    yield_now().await;
}

/// The hops half of a wait: real time until `condition` holds, no virtual
/// advance. A pump the open gate already unblocked lands here, and a slow
/// deliverer is absorbed in real time — which is the whole point: the paced
/// suites would otherwise read a late delivery as a missing budget.
async fn hop_until(condition: &mut impl FnMut() -> bool) -> bool {
    for _ in 0..FEED_HOPS {
        feed_hop().await;
        if condition() {
            return true;
        }
    }
    false
}

/// One 10 ms frame step: a demand frame blocked by the rate gate fires here,
/// and the queue's first budget drains with it. The 10 ms is the harness's one
/// piece of arithmetic, and it is deliberate: wider than the 120 FPS frame gate
/// (`frame::MIN_FRAME_INTERVAL`, 8.33 ms) so a gated frame does fire, narrower
/// than the 33 ms pacing tick so no horizon can — the "one budget per step" the
/// tick-counting assertions rest on (the Esc-dump pin included: a dump that
/// waited for the next tick instead of the next pump fails here).
async fn arrival_frame() {
    advance(Duration::from_millis(10)).await;
    yield_now().await;
    yield_now().await;
}

/// Pump the loop on real time until `condition` reports the world caught up,
/// then stop at that pump.
///
/// Hops first, and the one frame step after them is the fallback for a gate the
/// app's own frames left hot; the trailing hops let that step's pump land. The
/// clock moves at most those 10 ms, so the wait cannot hand its caller a second
/// budget: no horizon is reachable, and whatever frame the step fires is the
/// one the delivery was waiting on.
async fn flush_feed_until(mut condition: impl FnMut() -> bool) {
    if hop_until(&mut condition).await {
        return;
    }
    arrival_frame().await;
    if condition() || hop_until(&mut condition).await {
        return;
    }
    panic!(
        "the feed never landed: ~{} real-time hops and one 10 ms frame step without the condition holding",
        FEED_HOPS * 2
    );
}

/// One pacing tick: the 33 ms horizon fires exactly one pump (one budget).
async fn tick33() {
    advance(Duration::from_millis(33)).await;
    yield_now().await;
    yield_now().await;
}

/// Whether the `receiving…` liveness row is up.
fn receiving_row(world: &World) -> bool {
    has_row(&world.visible_rows(), "receiving…")
}

/// Scrollback + screen, oldest first (the insertion-side view —
/// `nonblank_rows` minus the blank filter).
fn all_rows(world: &World) -> Vec<String> {
    let mut rows = world.scrollback_rows();
    rows.extend(world.visible_rows());
    rows
}

/// The world's visible rows with the never-written tail (and the collapse
/// buffer below the band) trimmed — band-suffix assertions read this.
fn visible_content(world: &World) -> Vec<String> {
    let mut rows = world.visible_rows();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

/// The visible floor status row (the band anchors at the cursor, so find
/// it by content): the session label, left-aligned.
fn status_row(world: &World) -> String {
    world
        .visible_rows()
        .into_iter()
        .find(|row| row.starts_with("kimi·k2"))
        .unwrap_or_default()
}

/// The run-status row above the composer (left-aligned, found by its state
/// word) — empty while idle, when the row is absent. The transcript's
/// `Failed after …` note is filtered out by the `·`.
fn run_status_row(world: &World) -> String {
    world
        .visible_rows()
        .into_iter()
        .find(|row| {
            row.starts_with("Working ·")
                || row.starts_with("Waiting for approval ·")
                || row.starts_with("Failed ·")
        })
        .unwrap_or_default()
}

/// The suite's shared epilogue: Ctrl-C quits, the loop joins cleanly.
async fn quit_and_join(
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    input: &tokio::sync::mpsc::UnboundedSender<Event>,
) {
    input.send(ctrl('c')).expect("input");
    for _ in 0..8 {
        yield_now().await;
        if task.is_finished() {
            break;
        }
    }
    task.await.expect("the loop joins").expect("a clean exit");
}

/// The residue-sensitive assertion form: scrollback + screen, oldest first,
/// WITH the blank rows (`nonblank_rows` filters the very rows the high-water
/// hold is judged on), trimmed only of the never-written screen tail below
/// the band. The allowed blanks: the markdown's own single separators, the
/// band's slack padding at the band's TOP (the hold's design), and the ONE
/// per-run collapse is top-anchored: its buffer hangs BELOW the band, so it
/// is trimmed here with the never-written screen tail.
fn rows_with_blanks(world: &World) -> Vec<String> {
    let mut rows = world.scrollback_rows();
    rows.extend(world.visible_rows());
    let end = rows
        .iter()
        .rposition(|row| !row.is_empty())
        .map_or(0, |index| index + 1);
    rows.truncate(end);
    rows
}

/// The completion row's clock is the virtual clock's walk through the
/// test's own advances (the RunFinished-gap probe crosses whole seconds);
/// the sequence asserts normalize it to a fixed marker.
fn normalize_worked_for(rows: Vec<String>) -> Vec<String> {
    rows.into_iter()
        .map(|row| {
            if row.starts_with("Worked for ") {
                "Worked for …".to_string()
            } else {
                row
            }
        })
        .collect()
}

/// The world's tokenNNNN words, in order (the wrap-agnostic
/// no-loss/no-duplication pin).
fn token_words(world: &World) -> Vec<String> {
    all_rows(world)
        .iter()
        .flat_map(|row| row.split_whitespace().map(str::to_string))
        .filter(|word| word.starts_with("token"))
        .collect()
}

/// Fixed-width tokens: nine characters each, so an 80-column row wraps
/// after exactly 8 and a 100-column row after exactly 10 — the wrapped
/// rows the height math sees are computable in the test.
fn tokens(from: usize, to: usize) -> String {
    (from..=to)
        .map(|i| format!("token{i:04}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The harness's own seam, pinned: a real-time wait must leave the paused
/// clock alone even with a timer pending. Without the blocking task's
/// auto-advance inhibit, the park jumps the clock to that timer — the failure
/// the Windows job showed (its doubled arrival budget, itself only a symptom)
/// — and every tick-counting assertion in this file is downstream of the clock
/// staying put.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_real_time_wait_leaves_the_paused_clock_alone() {
    // A timer pending across the waits: a park would advance to it.
    let armed = tokio::spawn(tokio::time::sleep(Duration::from_secs(60)));
    let before = tokio::time::Instant::now();
    for _ in 0..4 {
        real_wait().await;
    }
    assert_eq!(
        tokio::time::Instant::now(),
        before,
        "the clock moved on its own"
    );
    armed.abort();
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
// The full session narrative is the point of the test; splitting it would
// only scatter the timeline.
#[allow(clippy::too_many_lines)]
async fn the_session_loop_end_to_end() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            world.print_lines(&["$ cadmus chat".to_string()]);
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });

            // The boot frame: an idle band is composer + status (the band
            // anchors at the cursor, so find the status row by content).
            settle().await;
            assert_eq!(status_row(&world), "kimi·k2");

            // Type and submit the first prompt.
            type_text(&rig, "fix the test");
            settle().await;
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            let submitted = driver.submitted();
            assert_eq!(submitted.len(), 1);
            assert_eq!(submitted[0][0].text_body(), "fix the test");
            // The prompt block left the band (it sits above it — rows only
            // reach vt100 "scrollback" once the screen fills, so assert on
            // the full sequence). A first attach is NOT a resync: no marker.
            assert!(
                world
                    .nonblank_rows()
                    .contains(&"❯ fix the test".to_string()),
                "world: {:?}",
                world.nonblank_rows()
            );
            assert!(
                !world.nonblank_rows().contains(&"… resynced …".to_string()),
                "a spurious resync marker: {:?}",
                world.nonblank_rows()
            );

            // The run streams: text lands in the band, completed content
            // flushes continuously. The run-status row works above the
            // composer, its clock at 0s on the paused test clock.
            run.live
                .send(delta(1, 1, "reading main.rs\n\n"))
                .expect("feed");
            settle_until(|| {
                world
                    .nonblank_rows()
                    .contains(&"reading main.rs".to_string())
            })
            .await;
            assert!(run_status_row(&world).starts_with("Working · "));

            // Esc interrupts; the command rides the run's sink.
            rig.input.send(key(KeyCode::Esc)).expect("input");
            settle().await;
            assert!(
                matches!(driver.commands().as_slice(), [Command::Interrupt { .. }]),
                "commands: {:?}",
                driver.commands()
            );

            // The turn seals, a tool runs, the run finishes; the outcome
            // folds into the session history.
            run.live
                .send(llm_response(2, 1, "reading main.rs\n\n"))
                .expect("feed");
            run.live
                .send(recorded(
                    3,
                    1,
                    EventKind::ToolCall {
                        call: cadmus_contract::ToolCall {
                            id: "c1".into(),
                            name: "read_file".into(),
                            arguments: serde_json::json!({}),
                        },
                    },
                ))
                .expect("feed");
            run.live
                .send(recorded(4, 1, EventKind::RunFinished { turns: 1 }))
                .expect("feed");
            let history = vec![
                Message::user("fix the test"),
                Message::text(cadmus_contract::Role::Assistant, "reading main.rs\n\n"),
            ];
            // The tail ends (production: the run task closes the
            // broadcaster) and the report follows — teardown in stream
            // order, the drainer's contract.
            drop(run.live);
            run.outcome.send(Ok(history.clone())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            // The strongest assertion form: the exact full row sequence —
            // any lost, duplicated or marker-polluted row breaks it. The
            // completion row (Codex's FinalMessageSeparator precedent)
            // records the run's cost; the run-status row is gone (idle).
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "$ cadmus chat",
                    "❯ fix the test",
                    "reading main.rs",
                    "Worked for 0s",
                    COMPOSER_PLACEHOLDER,
                    "kimi·k2",
                ],
                "scrollback+screen sequence (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );
            assert_eq!(status_row(&world), "kimi·k2");
            assert!(run_status_row(&world).is_empty());

            // The second run carries the first's history.
            type_text(&rig, "now fix it");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let _run2 = driver.take_run();
            let submitted = driver.submitted();
            assert_eq!(submitted.len(), 2);
            assert_eq!(submitted[1].len(), 3);
            assert_eq!(submitted[1][2].text_body(), "now fix it");

            // Ctrl-C quits.
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The gate's two-call request: a write and an edit of the same file.
fn gated_batch() -> Vec<ToolCall> {
    vec![
        ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "content": "fn main() {}\n",
            }),
        },
        ToolCall {
            id: "c2".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": "src/main.rs",
                "edits": [{"old_string": "let a = 1;", "new_string": "let a = 2;"}],
            }),
        },
    ]
}

fn approval_request(seq: u64, request_id: &str, calls: Vec<ToolCall>) -> LiveUpdate {
    LiveUpdate::Item {
        item: Box::new(LiveItem {
            seq,
            trace_id: "tr-test".into(),
            kind: LiveKind::ApprovalRequested {
                request_id: request_id.into(),
                turn: 1,
                calls,
                wait_timeout: std::time::Duration::from_secs(300),
            },
        }),
    }
}

/// The band shows the selected call's change and the other original slots.
const APPROVAL_SECTION: [&str; 5] = [
    "approve 2 call(s)  ·  unanswered denies after 5 min",
    "Tab: arm selected call · y/n: answer it · Esc: interrupt",
    "> 1/2 ▸ write_file src/main.rs",
    "+ fn main() {}",
    "  2/2 ▸ edit_file src/main.rs (pending)",
];

/// A call can be answered ahead of its sibling, even with duplicated wire
/// ids. Only the recorded answer settles it; focus advances independently.
#[tokio::test(start_paused = true, flavor = "current_thread")]
// The full dialog narrative is the point of the test; splitting it would
// only scatter the timeline.
#[allow(clippy::too_many_lines)]
async fn a_second_call_can_be_approved_before_the_first() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            let mut calls = gated_batch();
            calls[1].id = calls[0].id.clone();
            run.live
                .send(approval_request(1, "ap1", calls))
                .expect("feed");
            settle_until(|| has_row(&world.visible_rows(), "approve 2 call(s)")).await;
            // Band order: prompt, section, the (paused) run-status row, the
            // busy composer, the floor line.
            assert_eq!(
                world.nonblank_rows(),
                [
                    vec!["❯ change main".to_string()],
                    APPROVAL_SECTION.map(String::from).to_vec(),
                    vec![
                        "Waiting for approval · 0s".to_string(),
                        DIALOG_PLACEHOLDER.to_string(),
                        "kimi·k2".to_string(),
                    ],
                ]
                .concat()
            );

            rig.input.send(key(KeyCode::Tab)).expect("arm first");
            rig.input.send(key(KeyCode::Tab)).expect("select second");
            settle().await;
            assert!(has_row(&world.visible_rows(), "> 2/2 ▸ edit_file"));
            assert!(has_row(&world.visible_rows(), "- let a = 1;"));
            rig.input.send(key(KeyCode::Char('y'))).expect("input");
            // Unix auto-repeat is indistinguishable from ordinary Press.
            rig.input.send(key(KeyCode::Char('y'))).expect("repeat");
            settle().await;
            assert_eq!(
                driver.commands(),
                vec![call_decision("tui-0", "ap1", 1, Approval::Approved)]
            );
            assert!(has_row(&world.visible_rows(), "> 1/2 ▸ write_file"));
            assert!(has_row(
                &world.visible_rows(),
                "edit_file src/main.rs (sent)"
            ));
            assert!(!has_row(&world.nonblank_rows(), "✓ approved"));

            run.record(2, EventKind::Command(driver.commands()[0].clone()));
            settle_until(|| has_row(&world.nonblank_rows(), "✓ approved: edit_file")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("repeat after resolve");
            settle().await;
            assert_eq!(driver.commands().len(), 1);
            assert!(has_row(&world.visible_rows(), "> 1/2 ▸ write_file"));
            // Focus cannot return to the settled second slot.
            rig.input.send(key(KeyCode::BackTab)).expect("input");
            rig.input.send(key(KeyCode::Char('n'))).expect("input");
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("extra answer");
            settle().await;
            assert_eq!(
                driver.commands()[1],
                call_decision("tui-1", "ap1", 0, Approval::Rejected { comment: None })
            );
            assert_eq!(
                driver.commands().len(),
                2,
                "submitted slots are not answerable again"
            );
            run.record(3, EventKind::Command(driver.commands()[1].clone()));
            run.record(
                4,
                EventKind::ToolCall {
                    call: gated_batch().remove(1),
                },
            );
            run.record(5, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "❯ change main",
                    "✓ approved: edit_file",
                    "✗ rejected: write_file",
                    "▸ edit_file src/main.rs",
                    "Worked for 0s",
                    COMPOSER_PLACEHOLDER,
                    "kimi·k2",
                ]
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_held_answer_cannot_cross_resync_or_a_new_prompt() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| has_row(&world.visible_rows(), "Tab: arm")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("held before prompt");
            settle().await;
            assert!(driver.commands().is_empty());
            rig.input.send(key(KeyCode::Tab)).expect("arm first");
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("approve first");
            settle().await;
            let mut sync = sync_with_pending(
                vec![PendingApproval {
                    request_id: "ap1".into(),
                    turn: 1,
                    message_index: None,
                    calls: gated_batch(),
                    decisions: Vec::new(),
                    wait_timeout: Duration::from_secs(300),
                }],
                Vec::new(),
            );
            sync.as_of_seq = 1;
            let (tail, receiver) = std::sync::mpsc::channel();
            *driver.reattachment.lock().expect("reattachment") = Some(Attachment {
                sync,
                tail: Box::new(receiver.into_iter()),
            });
            run.live.send(LiveUpdate::Lagged).expect("lag");
            settle_until(|| has_row(&world.nonblank_rows(), "resynced")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("held across resync");
            settle().await;
            assert_eq!(driver.commands().len(), 1);
            assert!(has_row(
                &world.visible_rows(),
                "write_file src/main.rs (sent)"
            ));
            rig.input.send(key(KeyCode::Tab)).expect("rearm sibling");
            rig.input
                .send(key(KeyCode::Char('n')))
                .expect("reject sibling");
            settle().await;
            assert_eq!(
                driver.commands()[1],
                call_decision("tui-1", "ap1", 1, Approval::Rejected { comment: None })
            );
            for (index, command) in driver.commands().into_iter().enumerate() {
                tail.send(recorded(index as u64 + 2, 1, EventKind::Command(command)))
                    .expect("resolve");
            }
            tail.send(approval_request(4, "ap2", gated_batch()))
                .expect("next prompt");
            settle_until(|| has_row(&world.visible_rows(), "> 1/2 ▸ write_file")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("held across new prompt");
            settle().await;
            assert_eq!(driver.commands().len(), 2);
            drop(tail);
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_early_completion_is_visible_while_a_sibling_waits() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            run.record(
                2,
                EventKind::Command(call_decision("remote", "ap1", 1, Approval::Approved)),
            );
            let completion = ToolCompletion {
                span_id: "sp-1".into(),
                turn: 1,
                message_call_index: 1,
                call_id: "c2".into(),
                name: "edit_file".into(),
                result: serde_json::json!("changed main.rs"),
                error: None,
            };
            run.live
                .send(LiveUpdate::Item {
                    item: Box::new(LiveItem {
                        seq: 3,
                        trace_id: "tr-test".into(),
                        kind: LiveKind::ToolCompleted {
                            completion: completion.clone(),
                        },
                    }),
                })
                .expect("completion");
            settle_until(|| has_row(&world.nonblank_rows(), "changed main.rs")).await;
            assert!(driver.commands().is_empty());
            assert!(has_row(&world.visible_rows(), "> 1/2 ▸ write_file"));
            assert!(has_row(&world.nonblank_rows(), "✓ edit_file"));
            rig.input.send(key(KeyCode::Tab)).expect("arm sibling");
            rig.input
                .send(key(KeyCode::Char('n')))
                .expect("reject sibling");
            settle().await;
            run.record(4, EventKind::Command(driver.commands()[0].clone()));
            run.record(
                5,
                EventKind::ToolResult {
                    call_id: completion.call_id,
                    result: completion.result,
                },
            );
            run.record(6, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                world
                    .nonblank_rows()
                    .iter()
                    .filter(|row| row.contains("changed main.rs"))
                    .count(),
                1
            );
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_in_order_result_is_visible_while_a_sibling_waits() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            run.record(
                2,
                EventKind::Command(call_decision("remote", "ap1", 0, Approval::Approved)),
            );
            run.record(
                3,
                EventKind::ToolCall {
                    call: gated_batch().remove(0),
                },
            );
            run.record(
                4,
                EventKind::ToolResult {
                    call_id: "c1".into(),
                    result: serde_json::json!("created main.rs"),
                },
            );
            settle_until(|| has_row(&world.nonblank_rows(), "created main.rs")).await;
            assert!(driver.commands().is_empty());
            assert!(has_row(&world.visible_rows(), "> 2/2 ▸ edit_file"));
            assert!(has_row(&world.nonblank_rows(), "✓ write_file"));
            rig.input.send(key(KeyCode::Tab)).expect("arm sibling");
            rig.input
                .send(key(KeyCode::Char('n')))
                .expect("reject sibling");
            settle().await;
            run.record(5, EventKind::Command(driver.commands()[0].clone()));
            run.record(6, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                world
                    .nonblank_rows()
                    .iter()
                    .filter(|row| row.contains("created main.rs"))
                    .count(),
                1
            );
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// n rejects only the focused call; a later legacy batch reply settles the
/// remaining slot without rendering the first decision twice.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn n_rejects_the_pending_request() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .any(|row| row.contains("approve 2 call(s)"))
            })
            .await;

            rig.input.send(key(KeyCode::Tab)).expect("arm first");
            rig.input.send(key(KeyCode::Char('n'))).expect("input");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::ResolveApprovalCall { command_id, request_id, call_index: 0, decision: Approval::Rejected { comment: None } }]
                    if command_id == "tui-0" && request_id == "ap1"
                ),
                "commands: {:?}",
                driver.commands()
            );

            run.live.send(recorded(2, 1, EventKind::Command(driver.commands()[0].clone()))).expect("feed");
            run.live
                .send(recorded(
                    3,
                    1,
                    EventKind::Command(Command::ResolveApproval {
                        command_id: "remote-batch".into(),
                        request_id: "ap1".into(),
                        decisions: vec![Approval::Approved],
                    }),
                ))
                .expect("feed");
            run.live
                .send(recorded(4, 1, EventKind::RunFinished { turns: 1 }))
                .expect("feed");
            drop(run.live);
            run.outcome
                .send(Ok(vec![Message::user("change main")]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "❯ change main",
                    "✗ rejected: write_file",
                    "✗ rejected: edit_file",
                    "Worked for 0s",
                    COMPOSER_PLACEHOLDER,
                    "kimi·k2"
                ]
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// While a request is pending, Esc still interrupts the run and ordinary
/// keys still edit the composer — the modal capture takes y/n only.
/// The gate's deny timeout settles a request the dialog still holds: the
/// recorded resolution drops the prompt without a keystroke — a settled
/// request must not linger answerable.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_recorded_timeout_resolution_clears_the_dialog() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .any(|row| row.contains("approve 2 call(s)"))
            })
            .await;

            // A remote client settles only the first slot. The second stays
            // answerable and the eventual timeout cannot overwrite the first.
            run.live.send(recorded(2, 1, EventKind::Command(Command::ResolveApprovalCall {
                command_id: "remote".into(), request_id: "ap1".into(), call_index: 0, decision: Approval::Approved,
            }))).expect("feed");
            settle_until(|| world.visible_rows().iter().any(|row| row.contains("> 2/2 ▸ edit_file"))).await;
            run.live.send(recorded(3, 1, EventKind::Command(Command::ResolveApprovalCall {
                command_id: "duplicate".into(), request_id: "ap1".into(), call_index: 0, decision: Approval::Rejected { comment: None },
            }))).expect("feed");
            let timeout = "approval request timed out unanswered (5 minutes) — denied by the conservative default";
            run.live
                .send(recorded(
                    4,
                    1,
                    EventKind::Command(Command::ResolveApproval {
                        command_id: "ap-timeout-0".into(),
                        request_id: "ap1".into(),
                        decisions: vec![
                            Approval::Rejected {
                                comment: Some(timeout.into()),
                            },
                            Approval::Rejected {
                                comment: Some(timeout.into()),
                            },
                        ],
                    }),
                ))
                .expect("feed");
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .all(|row| !row.contains("approve 2 call(s)"))
            })
            .await;
            assert!(
                driver.commands().is_empty(),
                "no command was sent: {:?}",
                driver.commands()
            );
            let rows = world.nonblank_rows();
            assert_eq!(rows.iter().filter(|row| row.contains("✓ approved: write_file")).count(), 1);
            assert!(rows.iter().any(|row| row.contains("✗ rejected: edit_file")));
            assert!(!rows.iter().any(|row| row.contains("✗ rejected: write_file")));

            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn remote_answers_advance_focus_but_stale_or_invalid_records_do_not() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(10, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| has_row(&world.visible_rows(), "> 1/2 ▸ write_file")).await;
            run.record(
                11,
                EventKind::Command(Command::ResolveApprovalCall {
                    command_id: "remote".into(),
                    request_id: "ap1".into(),
                    call_index: 0,
                    decision: Approval::Approved,
                }),
            );
            settle_until(|| has_row(&world.visible_rows(), "> 2/2 ▸ edit_file")).await;
            for (seq, request_id, call_index) in
                [(10, "ap1", 1), (12, "ap1", usize::MAX), (13, "unknown", 1)]
            {
                run.record(
                    seq,
                    EventKind::Command(Command::ResolveApprovalCall {
                        command_id: format!("invalid-{seq}"),
                        request_id: request_id.into(),
                        call_index,
                        decision: Approval::Approved,
                    }),
                );
            }
            settle().await;
            assert!(has_row(&world.visible_rows(), "> 2/2 ▸ edit_file"));
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("unarmed press");
            settle().await;
            assert!(driver.commands().is_empty());
            rig.input
                .send(key(KeyCode::Tab))
                .expect("rearm after remote answer");
            rig.input.send(key(KeyCode::Char('n'))).expect("input");
            settle().await;
            assert_eq!(
                driver.commands(),
                vec![Command::ResolveApprovalCall {
                    command_id: "tui-0".into(),
                    request_id: "ap1".into(),
                    call_index: 1,
                    decision: Approval::Rejected { comment: None },
                }]
            );
            // The terminal record closes the dialog even before teardown arrives.
            run.record(14, EventKind::RunFinished { turns: 1 });
            settle_until(|| !has_row(&world.visible_rows(), "approve 2 call(s)")).await;
            rig.input.send(key(KeyCode::Char('y'))).expect("input");
            settle().await;
            assert_eq!(driver.commands().len(), 1);
            assert!(!has_row(&world.nonblank_rows(), "✓ approved: edit_file"));
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle().await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// A partial attach renders the recorded decision and focuses only the
/// remaining call, without ever needing the original live request.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_attach_mid_wait_seeds_the_dialog_from_the_sync() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot_with_pending(
                &world,
                vec![PendingApproval {
                    request_id: "ap-sync".into(),
                    turn: 3,
                    message_index: None,
                    calls: gated_batch(),
                    decisions: vec![Some(Approval::Rejected { comment: None }), None],
                    wait_timeout: std::time::Duration::from_secs(300),
                }],
            );
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            // No approval item is ever published: the section renders from
            // the sync's pending list alone.
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .any(|row| row.contains("approve 2 call(s)"))
            })
            .await;

            rig.input.send(key(KeyCode::Char('y'))).expect("unarmed press");
            settle().await;
            assert!(driver.commands().is_empty());
            rig.input.send(key(KeyCode::Tab)).expect("arm after attach");
            rig.input.send(key(KeyCode::Char('y'))).expect("input");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::ResolveApprovalCall { command_id, request_id, call_index: 1, decision: Approval::Approved }]
                    if command_id == "tui-0" && request_id == "ap-sync"
                ),
                "commands: {:?}",
                driver.commands()
            );

            // The run settles and the loop quits cleanly.
            let run = driver.take_run();
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// An attach after the settle: the baseline sync carries the settled
/// batch, so the rebuilt transcript shows the explicit approve/reject
/// record — not only the consequence the fold replays.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_attach_after_a_settle_renders_the_resolution_record() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot_with_baseline(
                &world,
                Vec::new(),
                vec![SettledApproval {
                    request_id: "ap-settled".into(),
                    message_index: None,
                    calls: gated_batch(),
                    decisions: vec![Approval::Approved, Approval::Rejected { comment: None }],
                }],
            );
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            // No live item is ever published: the record renders from the
            // sync's settled list alone.
            settle_until(|| {
                world
                    .nonblank_rows()
                    .iter()
                    .any(|row| row.contains("✓ approved: write_file"))
            })
            .await;
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .any(|row| row.contains("✗ rejected: edit_file")),
                "rows: {:?}",
                world.nonblank_rows()
            );

            let run = driver.take_run();
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The modal capture answers y/n only: Esc still interrupts (the command
/// rides the sink), other keys edit the composer, and nothing leaks into
/// the transcript.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_modal_leaves_esc_and_the_composer_untouched() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .any(|row| row.contains("approve 2 call(s)"))
            })
            .await;

            // Esc interrupts: the interrupt rides the sink and the dialog
            // stays — an interrupt during the wait ends the run without a
            // resolution, and teardown clears the queue.
            rig.input.send(key(KeyCode::Esc)).expect("input");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::Interrupt { command_id }] if command_id == "tui-0"
                ),
                "commands: {:?}",
                driver.commands()
            );
            // The composer still takes text (keys other than the captured
            // y/n — those answer the dialog, that is what modal means).
            type_text(&rig, "abc");
            settle().await;
            assert!(
                world.visible_rows().iter().any(|row| row.contains("abc")),
                "the composer stays editable while the dialog waits: {:?}",
                world.visible_rows()
            );

            drop(run.live);
            run.outcome
                .send(Ok(vec![Message::user("change main")]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            // The dialog died with the run.
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .all(|row| !row.contains("approve 2 call(s)")),
                "world: {:?}",
                world.nonblank_rows()
            );
            // The interrupt path lands the same completion row (its report
            // is Ok — completed work is preserved).
            assert!(
                has_row(&world.nonblank_rows(), "Worked for 0s"),
                "the interrupt lands the completion row too: {:?}",
                world.nonblank_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_failed_run_marks_the_transcript() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "do a thing");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            drop(run.live); // the feed ends without a terminal record
            run.outcome
                .send(Err("provider call failed: auth".to_string()))
                .expect("outcome");
            // The failure marker and the note queue behind the drain — wait
            // for the note (the queue's last row) before counting.
            settle_until(|| has_row(&world.nonblank_rows(), "Failed after 0s")).await;

            let rows = world.nonblank_rows();
            // Exactly one failure marker — the outcome and the feed must
            // never double-print (teardown is serialized, drainer docs).
            let markers = rows
                .iter()
                .filter(|row| row.as_str() == "run failed: provider call failed: auth")
                .count();
            assert_eq!(markers, 1, "rows: {rows:?}");
            // The failure rides the run-status row on its frozen clock,
            // and the completion note lands once, in the error slot.
            assert_eq!(run_status_row(&world), "Failed · 0s");
            assert_eq!(
                rows.iter()
                    .filter(|row| row.as_str() == "Failed after 0s")
                    .count(),
                1,
                "rows: {rows:?}"
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The run high-water hold, characterized under paced emission (ADR-0018's
/// 2026-09-20 amendment + second amendment): a multi-block turn — two
/// wrapped paragraphs and a tight list, settling at different times —
/// streams through several queue+drain cycles. The unstable tail is NEVER
/// rendered (the `receiving…` row stands in), the stable rows type out at
/// the paced budget, and the blank-preserving sequence keeps EXACTLY the
/// source's blank structure: the final scrollback is byte-identical to the
/// pre-queue model's (emission changes WHEN rows appear, never WHAT
/// appears).
#[tokio::test(start_paused = true, flavor = "current_thread")]
// The full session narrative is the point of the test; splitting it would
// only scatter the timeline.
#[allow(clippy::too_many_lines)]
async fn the_run_high_water_hold_leaves_no_mid_stream_blanks() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "explain the parser");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // 33 tokens wrap to 5 rows at 80 columns. A single trailing
            // newline keeps the paragraph the open tail: UNSTABLE, so it
            // renders nowhere — the receiving row carries the liveness
            // signal, and the band holds at the run's floor (2 → 4).
            let p1 = tokens(1, 33);
            run.live
                .send(delta(1, 1, &format!("{p1}\n")))
                .expect("feed");
            settle_until(|| receiving_row(&world)).await;
            assert_eq!(run_status_row(&world), "Working · 0s");
            assert!(
                !world
                    .nonblank_rows()
                    .iter()
                    .any(|row| row.contains("token")),
                "the unstable tail is never rendered: {:?}",
                world.nonblank_rows()
            );

            // The blank line closes p1 — it and the new separator queue
            // (6 rows) while the 2-row p2 holds open, unstable. The rows
            // type out one per tick; the world then is exactly the flushed
            // prefix plus the band (receiving on: p2 is live).
            let p2 = tokens(41, 50);
            run.live
                .send(delta(2, 1, &format!("\n{p2}\n")))
                .expect("feed");
            settle_until(|| {
                rows_with_blanks(&world)
                    == vec![
                        "❯ explain the parser".to_string(),
                        String::new(),
                        tokens(1, 8),
                        tokens(9, 16),
                        tokens(17, 24),
                        tokens(25, 32),
                        tokens(33, 33),
                        String::new(), // the markdown separator, flushed with p1
                        "receiving…".to_string(),
                        "Working · 0s".to_string(),
                        BUSY_PLACEHOLDER.to_string(),
                        "kimi·k2".to_string(),
                    ]
            })
            .await;
            assert!(
                !has_row(&world.nonblank_rows(), &tokens(41, 48)),
                "p2 is still unstable — nowhere: {:?}",
                world.nonblank_rows()
            );

            // Two more queue cycles: the list's items arrive in batches
            // (the field's failing shape); completed items type out while
            // the open item stays unrendered.
            run.live
                .send(delta(3, 1, "\n- one\n- two\n"))
                .expect("feed");
            settle_until(|| {
                let rows = all_rows(&world);
                has_row(&rows, "- one") && !has_row(&rows, "- two")
            })
            .await;
            run.live.send(delta(4, 1, "- three\n")).expect("feed");
            settle_until(|| {
                let rows = all_rows(&world);
                has_row(&rows, "- two") && !has_row(&rows, "- three")
            })
            .await;
            let source = format!("{p1}\n\n{p2}\n\n- one\n- two\n- three\n");
            run.live.send(llm_response(5, 1, &source)).expect("feed");
            settle_until(|| has_row(&all_rows(&world), "- three")).await;

            // The settling-gap wart: the terminal record's Idle light while
            // the run is still active must read Working across the 1 Hz
            // tick, never a blank row (the state-truthfulness rule).
            run.live
                .send(recorded(6, 1, EventKind::RunFinished { turns: 1 }))
                .expect("feed");
            settle_until(|| run_status_row(&world) == "Working · 1s").await;

            // The outcome: the note queues LAST and types out at the
            // accelerated floor, then the run-status row releases and the
            // band's last collapse is exactly its one row. The final
            // scrollback is byte-identical to the pre-queue model's.
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("explain the parser"),
                    Message::text(cadmus_contract::Role::Assistant, &source),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                normalize_worked_for(rows_with_blanks(&world)),
                vec![
                    "❯ explain the parser".to_string(),
                    String::new(),
                    tokens(1, 8),
                    tokens(9, 16),
                    tokens(17, 24),
                    tokens(25, 32),
                    tokens(33, 33),
                    String::new(),
                    tokens(41, 48),
                    tokens(49, 50),
                    String::new(),
                    "- one".to_string(),
                    "- two".to_string(),
                    "- three".to_string(),
                    String::new(), // the completion note's own separator
                    "Worked for …".to_string(),
                    COMPOSER_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ],
                "the outcome: exactly the source's blanks, byte-identical to \
                 the pre-queue model (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );

            // Back to back: the second run seeds a fresh floor at the
            // collapsed height (2), grows with its liveness row, and
            // collapses the same way — the accepted cost is per-run, never
            // per-block.
            type_text(&rig, "and the config");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run2 = driver.take_run();
            run2.live.send(delta(1, 1, "done\n")).expect("feed");
            settle_until(|| receiving_row(&world)).await;
            run2.live.send(llm_response(2, 1, "done\n")).expect("feed");
            run2.live
                .send(recorded(3, 1, EventKind::RunFinished { turns: 1 }))
                .expect("feed");
            // The sealed row types out at the held height; the band shows
            // one slack row atop it (the floor held at the receiving era's
            // 4) while the note still waits on the outcome.
            settle_until(|| {
                rows_with_blanks(&world).ends_with(&[
                    "done".to_string(),
                    String::new(),
                    "Working · 0s".to_string(),
                    BUSY_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ])
            })
            .await;
            drop(run2.live);
            run2.outcome
                .send(Ok(vec![
                    Message::user("explain the parser"),
                    Message::text(cadmus_contract::Role::Assistant, &source),
                    Message::user("and the config"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n"),
                ]))
                .expect("outcome");
            settle_until(|| {
                world
                    .nonblank_rows()
                    .iter()
                    .filter(|row| row.starts_with("Worked for"))
                    .count()
                    == 2
            })
            .await;
            let mut expected = vec![
                "❯ explain the parser".to_string(),
                String::new(),
                tokens(1, 8),
                tokens(9, 16),
                tokens(17, 24),
                tokens(25, 32),
                tokens(33, 33),
                String::new(),
                tokens(41, 48),
                tokens(49, 50),
                String::new(),
                "- one".to_string(),
                "- two".to_string(),
                "- three".to_string(),
            ];
            expected.extend([
                String::new(), // run 1's note separator
                "Worked for …".to_string(),
                String::new(), // the second prompt's leading separator
                "❯ and the config".to_string(),
                String::new(),
                "done".to_string(),
                String::new(), // run 2's note separator
                "Worked for …".to_string(),
                COMPOSER_PLACEHOLDER.to_string(),
                "kimi·k2".to_string(),
            ]);
            assert_eq!(
                normalize_worked_for(rows_with_blanks(&world)),
                expected,
                "back to back: zero mid-stream blanks, each run's final \
                 scrollback byte-identical (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The hold's two mid-run shrink traps (ADR-0018's 2026-09-20 amendment):
/// the approval dialog's appear/disappear and a type-ahead composer's
/// grow/clear. Both raise the content's own want — the floor follows
/// honestly — and neither may shrink the band when they unwind: the rows
/// they claimed stay as slack padding inside the band until the outcome's
/// one collapse, whose Δ accounts for the peak.
#[tokio::test(start_paused = true, flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn a_dismissed_dialog_and_a_cleared_type_ahead_never_shrink_the_band() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // The held paragraph (NO trailing blank, so it stays the open
            // tail) renders nowhere — the receiving row stands in, and the
            // floor's peak will account for the liveness row.
            run.live
                .send(delta(1, 1, "reading main.rs\n"))
                .expect("feed");
            settle_until(|| receiving_row(&world)).await;

            // The dialog appears (+its section rows), then the user types
            // ahead two extra composer rows: the floor rises with both.
            run.live
                .send(approval_request(
                    2,
                    "ap1",
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "src/main.rs",
                            "content": "fn main() {}\n",
                        }),
                    }],
                ))
                .expect("feed");
            settle_until(|| has_row(&world.visible_rows(), "approve 1 call(s)")).await;
            rig.input.send(ctrl('j')).expect("newline");
            rig.input.send(ctrl('j')).expect("newline");
            type_text(&rig, "draft");
            settle().await;
            // …and unwinds both: the type-ahead is deleted, the decision is
            // answered and recorded. Neither shrinks the band.
            for _ in 0..7 {
                rig.input.send(key(KeyCode::Backspace)).expect("erase");
            }
            settle().await;
            rig.input.send(key(KeyCode::Tab)).expect("arm");
            rig.input.send(key(KeyCode::Char('y'))).expect("answer");
            settle().await;
            assert_eq!(
                driver.commands(),
                vec![call_decision("tui-0", "ap1", 0, Approval::Approved)]
            );
            run.record(3, EventKind::Command(driver.commands()[0].clone()));
            settle_until(|| has_row(&world.nonblank_rows(), "✓ approved: write_file")).await;

            // Mid-run: the dialog's and the composer's claimed rows are
            // slack padding INSIDE the band (above the run-status row) —
            // not one blank landed above the flushed content.
            let rows = rows_with_blanks(&world);
            let content_end = rows
                .iter()
                .position(|row| row.starts_with("✓ approved"))
                .expect("the resolution marker");
            assert!(
                rows[..=content_end]
                    .iter()
                    .filter(|row| row.is_empty())
                    .count()
                    <= 2,
                "only the markdown separators may be blank above the band: {rows:?}"
            );
            let working = rows
                .iter()
                .position(|row| row == "Working · 0s")
                .expect("the run-status row");
            assert!(
                rows[content_end + 1..working].iter().all(String::is_empty),
                "the held slack is one blank run inside the band: {rows:?}"
            );
            assert!(
                working - content_end > 3,
                "the floor kept the peak: {rows:?}"
            );

            // The outcome: the floor releases, the band falls from the peak
            // (10) to the run's resting height (3); the note types out and
            // the last collapse is the run-status row's one row (3 → 2).
            run.record(4, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "reading main.rs\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            let mut expected = vec![
                "❯ change main".to_string(),
                String::new(),
                "reading main.rs".to_string(),
                // The resolution marker attaches to the run's tail with no
                // separator of its own.
                "✓ approved: write_file".to_string(),
            ];
            expected.extend([
                String::new(), // the note's own separator
                "Worked for …".to_string(),
                // No residue: the peak's buffer sits below the band.
                COMPOSER_PLACEHOLDER.to_string(),
                "kimi·k2".to_string(),
            ]);
            assert_eq!(
                normalize_worked_for(rows_with_blanks(&world)),
                expected,
                "one collapse over the dialog+type-ahead peak, no mid-run \
                 blanks (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// A width grow mid-run: the queue's pending rows fold back and re-wrap at
/// the new width while the typewriter keeps pace — and across the whole
/// resize+drain, no row is lost and none duplicated (the wrap-agnostic
/// pin; the exact keep-the-front re-wrap semantics are the transcript unit
/// tests'). The band holds its height and collapses as ever.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_width_grow_mid_run_loses_no_rows() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "refactor the wrap");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // Content streaming: one closed paragraph typing out, a second
            // closed behind it, a third held open. Each arrival stops at the
            // next budget (its whole effect here: the queue runs on), so the
            // queue is still mid-drain when the resize lands below.
            let p1 = tokens(1, 33);
            let p2 = tokens(41, 57);
            let p3 = tokens(61, 77);
            run.live
                .send(delta(1, 1, &format!("{p1}\n\n")))
                .expect("feed");
            flush_feed_until(|| !token_words(&world).is_empty()).await;
            let typed = token_words(&world).len();
            run.live
                .send(delta(2, 1, &format!("{p2}\n\n{p3}\n")))
                .expect("feed");
            flush_feed_until(|| token_words(&world).len() > typed).await;

            // The resize lands mid-drain: the queue's pending rows fold
            // back and re-wrap at 100 columns as they type out.
            world.resize(24, 100);
            rig.input
                .send(Event::Resize(100, 24))
                .expect("resize event");
            settle().await;

            let source = format!("{p1}\n\n{p2}\n\n{p3}\n");
            run.live.send(llm_response(3, 1, &source)).expect("feed");
            run.record(4, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("refactor the wrap"),
                    Message::text(cadmus_contract::Role::Assistant, &source),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;

            // The wrap-agnostic pin: every token landed exactly once, in
            // order, whatever width its row was wrapped at.
            let expected: Vec<String> = (1..=33)
                .chain(41..=57)
                .chain(61..=77)
                .map(|i| format!("token{i:04}"))
                .collect();
            assert_eq!(
                token_words(&world),
                expected,
                "no row lost, none duplicated across the resize"
            );
            // The note types last; the band collapsed to idle.
            let rows = world.nonblank_rows();
            let note = rows
                .iter()
                .rposition(|row| row.starts_with("Worked for"))
                .unwrap();
            let last_token = rows
                .iter()
                .rposition(|row| row.contains("token0077"))
                .unwrap();
            assert!(note > last_token, "the note typed last: {rows:?}");
            let visible = visible_content(&world);
            assert_eq!(
                visible[visible.len() - 2..],
                [COMPOSER_PLACEHOLDER.to_string(), "kimi·k2".to_string()],
                "collapsed to idle: {visible:?}"
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The floor engages before the composer clears: a multi-line prompt's
/// composer collapse at submit produces no residue either — the band's
/// claimed rows become the run's floor and the run's slices re-fill them.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_multi_line_prompts_composer_collapse_leaves_no_residue() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            // Three composer lines while typing (the idle grow is
            // unchanged): the band claims 3 + 1 = 4 rows before Enter.
            type_text(&rig, "line one");
            rig.input.send(ctrl('j')).expect("newline");
            type_text(&rig, "line two");
            rig.input.send(ctrl('j')).expect("newline");
            type_text(&rig, "line three");
            settle().await;
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // The submit sequence: the composer collapsed back to one row
            // but the band held its 4; the prompt block types out at the
            // paced budget, then the band rests with one slack row atop it.
            settle_until(|| {
                rows_with_blanks(&world)
                    == vec![
                        "❯ line one".to_string(),
                        "❯ line two".to_string(),
                        "❯ line three".to_string(),
                        String::new(), // the prompt block's own separator
                        String::new(), // the held slack
                        "Working · 0s".to_string(),
                        BUSY_PLACEHOLDER.to_string(),
                        "kimi·k2".to_string(),
                    ]
            })
            .await;

            // The held paragraph (NO trailing blank) stays the open tail:
            // the receiving row stands in until the completion note's own
            // push seals it (a clean RunFinished does not seal), then tail
            // + note type out and the band collapses.
            run.live.send(delta(1, 1, "ok\n")).expect("feed");
            settle_until(|| receiving_row(&world)).await;
            run.record(2, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("line one\nline two\nline three"),
                    Message::text(cadmus_contract::Role::Assistant, "ok\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                normalize_worked_for(rows_with_blanks(&world)),
                vec![
                    "❯ line one".to_string(),
                    "❯ line two".to_string(),
                    "❯ line three".to_string(),
                    String::new(),
                    "ok".to_string(),
                    String::new(), // the note's own separator
                    "Worked for …".to_string(),
                    // No residue: each collapse top-anchors, its buffer
                    // sits below the band (trimmed with the screen tail).
                    COMPOSER_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ],
                "the composer-peak floor's collapses leave no residue \
                 (visible: {:?})",
                world.visible_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// Esc releases the floor through the same outcome path: the interrupt's
/// report is `Ok`, so the band collapses once at the report — no special
/// case, no extra residue.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_interrupt_collapses_the_band_through_the_same_outcome_path() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "stop it");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            // The held paragraph (NO trailing blank) stays the open tail:
            // it renders nowhere — the receiving row stands in.
            run.live.send(delta(1, 1, "working hard\n")).expect("feed");
            settle_until(|| receiving_row(&world)).await;
            assert!(
                !has_row(&world.nonblank_rows(), "working hard"),
                "the unstable tail is never rendered: {:?}",
                world.nonblank_rows()
            );

            rig.input.send(key(KeyCode::Esc)).expect("interrupt");
            settle().await;
            assert!(
                matches!(driver.commands().as_slice(), [Command::Interrupt { .. }]),
                "commands: {:?}",
                driver.commands()
            );

            run.record(2, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("stop it"),
                    Message::text(cadmus_contract::Role::Assistant, "working hard\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            // The tail stays live until the completion note's own push
            // seals it (a clean RunFinished does not seal), so the
            // outcome's pump flushes tail + note together; the Δ = 2 buffer
            // then forms below the band.
            assert_eq!(
                normalize_worked_for(rows_with_blanks(&world)),
                vec![
                    "❯ stop it".to_string(),
                    String::new(),
                    "working hard".to_string(),
                    String::new(), // the note's own separator
                    "Worked for …".to_string(),
                    // No residue: the Δ = 2 buffer sits below the band.
                    COMPOSER_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ],
                "the interrupt collapses once, through the outcome path \
                 (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// Enter mid-run sends an inject steer (the default granularity): the
/// composer clears into the pending acknowledgment, the recorded command
/// renders the block exactly once, and the acknowledgment retires.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn enter_mid_run_sends_an_inject_steer_and_the_record_renders_it_once() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "fix the bug");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            assert!(has_row(&world.nonblank_rows(), BUSY_PLACEHOLDER));
            run.live.send(delta(1, 1, "working\n")).expect("feed");
            settle().await;

            type_text(&rig, "also check the tests");
            rig.input.send(key(KeyCode::Enter)).expect("steer");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::Steer { command_id, text, mode: SteerMode::Inject }]
                        if command_id == "tui-0" && text == "also check the tests"
                ),
                "commands: {:?}",
                driver.commands()
            );
            // The send's acknowledgment: the composer names the pending
            // steer while it awaits application core-side.
            assert!(has_row(&world.nonblank_rows(), "1 steer pending"));

            // The core applies the steer at the next boundary: the recorded
            // command renders the block, the count retires.
            run.record(
                2,
                EventKind::Command(Command::Steer {
                    command_id: "tui-0".into(),
                    text: "also check the tests".into(),
                    mode: SteerMode::Inject,
                }),
            );
            settle_until(|| has_row(&world.nonblank_rows(), "❯ also check the tests")).await;
            assert!(has_row(&world.nonblank_rows(), BUSY_PLACEHOLDER));

            run.record(3, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("fix the bug"),
                    Message::user("also check the tests"),
                    Message::text(cadmus_contract::Role::Assistant, "working\n"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert_eq!(
                world
                    .nonblank_rows()
                    .iter()
                    .filter(|row| row.contains("also check the tests"))
                    .count(),
                1,
                "the steer renders exactly once: {:?}",
                world.nonblank_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// Tab mid-run sends a queue steer — the held granularity. The pending
/// count accumulates over sends and resets with the run when the buffered
/// steers die unapplied (never logged, never rendered).
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn tab_mid_run_queues_until_the_run_resets_the_acknowledgment() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "fix the bug");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            type_text(&rig, "one more thing");
            rig.input.send(key(KeyCode::Tab)).expect("queue");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::Steer { text, mode: SteerMode::Queue, .. }] if text == "one more thing"
                ),
                "commands: {:?}",
                driver.commands()
            );
            assert!(has_row(&world.nonblank_rows(), "1 steer pending"));

            type_text(&rig, "and another");
            rig.input.send(key(KeyCode::Tab)).expect("queue");
            settle().await;
            assert!(has_row(&world.nonblank_rows(), "2 steers pending"));
            assert_eq!(driver.commands().len(), 2);

            // The run ends with both steers still buffered core-side: the
            // acknowledgment resets with the run, the composer returns to
            // the idle placeholder.
            run.record(1, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("fix the bug"),
                    Message::text(cadmus_contract::Role::Assistant, "done"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert!(has_row(&world.nonblank_rows(), COMPOSER_PLACEHOLDER));
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .all(|row| !row.contains("steer pending")),
                "no acknowledgment outlives the run: {:?}",
                world.nonblank_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// Nothing to steer: idle Tab keeps the draft (there is no run), and an
/// empty composer mid-run sends neither an inject nor a queue.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn tab_or_enter_with_nothing_to_steer_sends_nothing() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "draft");
            rig.input.send(key(KeyCode::Tab)).expect("tab");
            settle().await;
            assert!(
                driver.commands().is_empty() && driver.submitted().is_empty(),
                "idle Tab is a no-op: {:?} / {:?}",
                driver.commands(),
                driver.submitted()
            );
            assert!(has_row(&world.nonblank_rows(), "❯ draft"));

            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            rig.input.send(key(KeyCode::Enter)).expect("enter");
            rig.input.send(key(KeyCode::Tab)).expect("tab");
            settle().await;
            assert!(
                driver.commands().is_empty(),
                "an empty composer steers nothing: {:?}",
                driver.commands()
            );

            run.record(1, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("draft"),
                    Message::text(cadmus_contract::Role::Assistant, "ok"),
                ]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The modal's capture boundary: while the approval dialog is open Tab
/// moves its focus (no steer leaks) and the composer placeholder names
/// only the keys still its own, but Enter is not the dialog's — it
/// steers, and the pending acknowledgment rides the composer while the
/// dialog waits.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_dialog_owns_tab_but_not_enter() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "change main");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            run.live
                .send(approval_request(1, "ap1", gated_batch()))
                .expect("feed");
            settle_until(|| {
                world
                    .visible_rows()
                    .iter()
                    .any(|row| row.contains("approve 2 call(s)"))
            })
            .await;
            // The truthfulness rule: while the dialog owns Tab, the
            // composer stops claiming it (the dialog's hint row names it).
            assert!(has_row(&world.visible_rows(), DIALOG_PLACEHOLDER));
            assert!(
                world.visible_rows().iter().all(|row| !row.contains("Tab to queue")),
                "no Tab claim while the dialog owns it: {:?}",
                world.visible_rows()
            );

            type_text(&rig, "steer attempt");
            rig.input.send(key(KeyCode::Tab)).expect("tab");
            settle().await;
            assert!(
                driver.commands().is_empty(),
                "the dialog's Tab steers nothing: {:?}",
                driver.commands()
            );
            assert!(has_row(&world.visible_rows(), "❯ steer attempt"));

            rig.input.send(key(KeyCode::Enter)).expect("steer");
            settle().await;
            assert!(
                matches!(
                    driver.commands().as_slice(),
                    [Command::Steer { text, mode: SteerMode::Inject, .. }] if text == "steer attempt"
                ),
                "commands: {:?}",
                driver.commands()
            );
            assert!(has_row(
                &world.nonblank_rows(),
                "❯ 1 steer pending · Enter to steer · Esc to interrupt"
            ));

            // The dialog settles: the placeholder reclaims the full keymap.
            run.record(
                2,
                EventKind::Command(Command::ResolveApproval {
                    command_id: "cmd-remote".into(),
                    request_id: "ap1".into(),
                    decisions: vec![Approval::Approved, Approval::Approved],
                }),
            );
            settle_until(|| {
                has_row(
                    &world.nonblank_rows(),
                    "❯ 1 steer pending · Enter to steer · Tab to queue · Esc to interrupt",
                )
            })
            .await;

            // The run ends with the steer still buffered: the
            // acknowledgment dies with it.
            drop(run.live);
            run.outcome
                .send(Ok(vec![Message::user("change main")]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert!(has_row(&world.nonblank_rows(), COMPOSER_PLACEHOLDER));
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .all(|row| !row.contains("steer pending")),
                "no acknowledgment outlives the run: {:?}",
                world.nonblank_rows()
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// A lag re-attach zeroes the pending acknowledgment: the hole's steer
/// applications landed in the fold and the still-buffered ones are
/// unknowable, so the count can only over-report from there.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn a_pending_steer_zeroes_at_a_resync() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "fix the bug");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            type_text(&rig, "one more thing");
            rig.input.send(key(KeyCode::Tab)).expect("queue");
            settle().await;
            assert!(has_row(&world.nonblank_rows(), "1 steer pending"));

            let (tail, receiver) = std::sync::mpsc::channel();
            *driver.reattachment.lock().expect("reattachment") = Some(Attachment {
                sync: sync_with_pending(Vec::new(), Vec::new()),
                tail: Box::new(receiver.into_iter()),
            });
            run.live.send(LiveUpdate::Lagged).expect("lag");
            settle_until(|| has_row(&world.nonblank_rows(), "resynced")).await;
            assert!(has_row(&world.nonblank_rows(), BUSY_PLACEHOLDER));
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .all(|row| !row.contains("steer pending")),
                "the acknowledgment zeroes at a resync: {:?}",
                world.nonblank_rows()
            );

            drop(tail);
            drop(run.live);
            run.outcome
                .send(Ok(vec![Message::user("fix the bug")]))
                .expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The ADR-0011 floor row: the context-usage ratio lands right-aligned
/// once the first response's usage is known (before that the right side
/// stays empty), and the binary's label refresh fires at the run outcome —
/// the cadence the agent's edits land on, never mid-run.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_floor_shows_context_usage_and_refreshes_the_git_label_at_the_outcome() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            // Boot by hand: the refresh closure is the seam under test. The
            // dirty marker `*` is what the refresh adds.
            let (input_tx, input) = ScriptedInput::channel();
            let shell = InlineShell::new(
                world.backend.clone(),
                GuardSink::default(),
                2,
                ScrollbackStrategy::FullScreen,
            )
            .expect("boot shell");
            let driver = ScriptDriver::new();
            let refreshes = Arc::new(Mutex::new(0usize));
            let refresh = {
                let refreshes = Arc::clone(&refreshes);
                move || {
                    *refreshes.lock().expect("refreshes") += 1;
                    "kimi·k2 (git:main)*".to_string()
                }
            };
            let mut app = App::new(
                shell,
                input,
                Box::new(driver.clone_handles()),
                AppConfig {
                    label: "kimi·k2 (git:main)".into(),
                    context_window: 128_000,
                    refresh_label: Some(Box::new(refresh)),
                    motion: Motion::Full,
                    theme: Theme::ansi(),
                    depth: ColorDepth::Truecolor,
                },
            );
            let rig = Rig {
                driver,
                input: input_tx,
            };
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;
            // Nothing known yet: the right side stays empty.
            assert_eq!(status_row(&world), "kimi·k2 (git:main)");

            type_text(&rig, "hi");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            // 45_000 input + 200 cache-read tokens of a 128k window.
            run.live
                .send(recorded(
                    1,
                    1,
                    EventKind::LlmResponse {
                        message: Message::text(cadmus_contract::Role::Assistant, "hi\n"),
                        usage: Some(cadmus_contract::Usage {
                            input: 45_000,
                            cache_read: 200,
                            ..Default::default()
                        }),
                        finish: cadmus_contract::FinishReason::Stop,
                        outcome: cadmus_contract::TurnOutcome::Content,
                        warnings: Vec::new(),
                    },
                ))
                .expect("feed");
            settle_until(|| status_row(&world).ends_with("45.2k/128k (35%)")).await;
            let row = status_row(&world);
            assert!(
                row.starts_with("kimi·k2 (git:main)"),
                "the label is unrefreshed mid-run: {row}"
            );
            assert_eq!(
                *refreshes.lock().expect("refreshes"),
                0,
                "no refresh before the outcome"
            );

            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("hi"),
                    Message::text(cadmus_contract::Role::Assistant, "hi\n"),
                ]))
                .expect("outcome");
            settle_until(|| status_row(&world).starts_with("kimi·k2 (git:main)*")).await;
            let row = status_row(&world);
            assert!(row.ends_with("45.2k/128k (35%)"), "usage survives: {row}");
            assert_eq!(*refreshes.lock().expect("refreshes"), 1);

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The paced typewriter's policy, tick by tick under the paused clock: a
/// queued block emits at the depth-tiered budget — 4 rows/tick in a
/// backlog, 2 mid-range, 1 at a trickle — and nothing between ticks.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_typewriter_emits_at_the_paced_rate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "go");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // An open fence's 30 body lines are stable rows: they queue
            // whole and drain at the policy's budget. The arrival pump
            // takes the depth-30 slice (4); each 33 ms tick after takes the
            // tier's budget — 4, then 2s down to the shallow tier, then 1s.
            let fence = (1..=30).fold(String::new(), |acc, i| acc + &format!("line {i:02}\n"));
            run.live
                .send(delta(1, 1, &format!("```\n{fence}")))
                .expect("feed");
            let body_rows = || {
                all_rows(&world)
                    .iter()
                    .filter(|row| row.starts_with("line "))
                    .count()
            };
            flush_feed_until(|| body_rows() > 0).await;
            let schedule = [4, 8, 10, 12, 14, 16, 18, 20, 22, 24, 25, 26, 27, 28, 29, 30];
            assert_eq!(
                body_rows(),
                schedule[0],
                "the arrival pump takes the depth-30 budget"
            );
            for &expected in &schedule[1..] {
                tick33().await;
                assert_eq!(body_rows(), expected, "one tick at the tier's budget");
            }
            // The queue is empty: another tick emits nothing.
            tick33().await;
            assert_eq!(body_rows(), 30, "drained: the typewriter rests");

            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle().await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The unstable tail is NEVER rendered: mid-paragraph (no closing blank
/// yet) the scrollback and the band hold only previously stable rows and
/// the `receiving…` row — no partial paragraph anywhere, on any tick.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_unstable_tail_is_never_rendered() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "explain");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // A single trailing newline: the paragraph is the open tail.
            run.live
                .send(delta(1, 1, "half formed thought\n"))
                .expect("feed");
            flush_feed_until(|| receiving_row(&world)).await;
            let rows = world.nonblank_rows();
            assert!(
                !has_row(&rows, "half formed"),
                "the unstable tail is never rendered: {rows:?}"
            );
            assert!(
                receiving_row(&world),
                "the liveness row stands in: {:?}",
                world.visible_rows()
            );
            assert!(run_status_row(&world).starts_with("Working · "));
            // Ticks pass: nothing becomes stable, nothing appears.
            for _ in 0..3 {
                tick33().await;
            }
            assert!(
                !has_row(&world.nonblank_rows(), "half formed"),
                "still unstable, still nowhere: {:?}",
                world.nonblank_rows()
            );

            // The closing blank makes it stable: it types out at the pace.
            run.live.send(delta(2, 1, "\n")).expect("feed");
            flush_feed_until(|| has_row(&world.nonblank_rows(), "half formed thought")).await;
            assert!(
                has_row(&world.nonblank_rows(), "half formed thought"),
                "stable now, typed out: {:?}",
                world.nonblank_rows()
            );

            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle().await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The `receiving…` row's lifecycle: off while a run has nothing pending,
/// on with the first unstable content, held across the drain (queue
/// non-empty), gone with the last row.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_receiving_row_lives_and_dies_with_the_drain() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "go");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();
            // Run active, nothing pending: the row is off.
            assert!(
                !receiving_row(&world),
                "nothing pending: {:?}",
                world.visible_rows()
            );

            // An unstable tail turns it on.
            run.live.send(delta(1, 1, "forming\n")).expect("feed");
            flush_feed_until(|| receiving_row(&world)).await;
            assert!(receiving_row(&world));

            // The closing blank queues the paragraph (its separator is
            // lazy — it comes with the NEXT block); the first budget
            // drains it — the row holds while the queue is non-empty.
            run.live.send(delta(2, 1, "\nsecond\n\n")).expect("feed");
            flush_feed_until(|| has_row(&all_rows(&world), "forming")).await;
            assert!(
                has_row(&all_rows(&world), "forming"),
                "the stable row typed out"
            );
            assert!(receiving_row(&world), "the separator is still queued");

            // The separator drains: queue empty, but "second" is the open
            // tail — the row holds on the unstable tail alone.
            tick33().await;
            assert!(
                receiving_row(&world),
                "the open tail still holds it: {:?}",
                world.visible_rows()
            );

            // Closing "second" queues it; the last drain empties the queue
            // and no tail remains — the row is gone.
            run.live.send(delta(3, 1, "\n")).expect("feed");
            flush_feed_until(|| !receiving_row(&world)).await;
            assert!(
                !receiving_row(&world),
                "drained: the row is gone: {:?}",
                world.visible_rows()
            );

            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle().await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// End-of-run sequencing, pinned: the run's remaining stable rows type out
/// at the accelerated floor, THEN `Worked for Ns` (the note queues LAST),
/// and only then does the band collapse — by exactly the run-status row's
/// one row (3 → 2: the end-of-run composer jump is dead).
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_note_types_last_and_the_band_collapses_by_one_row() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "work it");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // 20 tokens: 3 rows at 80 columns, held as the unstable tail
            // (a single trailing newline). The arrival pump heats the frame
            // gate, so the outcome below lands before any drain.
            let text = tokens(1, 20);
            run.live
                .send(delta(1, 1, &format!("{text}\n")))
                .expect("feed");
            flush_feed_until(|| receiving_row(&world)).await;
            assert!(receiving_row(&world));

            // The seal, the terminal record and the report land together on
            // real time — the frame gate is still hot from the arrival pump,
            // so the delivery alone fires nothing; the 10 ms frame step that
            // follows is the one accelerated budget (4): the 3 content rows
            // typed and the note's separator, with the note's TEXT still
            // queued behind them.
            run.live
                .send(llm_response(2, 1, &format!("{text}\n")))
                .expect("feed");
            run.record(3, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("work it"),
                    Message::text(cadmus_contract::Role::Assistant, format!("{text}\n")),
                ]))
                .expect("outcome");
            flush_feed_until(|| has_row(&world.nonblank_rows(), &tokens(17, 20))).await;

            // The accelerated budget (4): the 3 content rows typed, the
            // note's separator too — but the note's TEXT is still queued.
            let rows = world.nonblank_rows();
            assert!(has_row(&rows, &tokens(17, 20)), "content typed: {rows:?}");
            assert!(
                !has_row(&rows, "Worked for"),
                "the note types LAST: {rows:?}"
            );
            assert!(
                !receiving_row(&world),
                "the liveness row is gone with the run"
            );
            assert_eq!(run_status_row(&world), "Working · 0s");
            let visible = visible_content(&world);
            assert_eq!(
                visible[visible.len() - 3..],
                [
                    "Working · 0s".to_string(),
                    COMPOSER_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string()
                ],
                "the band holds the run's resting height (3): {visible:?}"
            );

            // The last tick: the note lands, and ONLY THEN the band
            // collapses — by exactly the run-status row's one row (3 → 2).
            tick33().await;
            let rows = world.nonblank_rows();
            assert!(has_row(&rows, "Worked for 0s"), "the note landed: {rows:?}");
            assert_eq!(run_status_row(&world), "", "the row released");
            let visible = visible_content(&world);
            assert_eq!(
                visible[visible.len() - 2..],
                [COMPOSER_PLACEHOLDER.to_string(), "kimi·k2".to_string()],
                "the collapse was exactly one row (3 → 2): {visible:?}"
            );
            assert_eq!(
                rows_with_blanks(&world),
                vec![
                    "❯ work it".to_string(),
                    String::new(),
                    tokens(1, 8),
                    tokens(9, 16),
                    tokens(17, 20),
                    String::new(), // the note's own separator
                    "Worked for 0s".to_string(),
                    COMPOSER_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ],
                "byte-identical to the pre-queue model: {:?}",
                rows_with_blanks(&world)
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// Esc's impatient path: the queued backlog dumps whole at the next pump —
/// no 33 ms tick waits — then the note lands right behind it and the band
/// collapses.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_interrupt_dumps_the_queue_instantly() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "stop it");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // 12 stable rows queue (an open fence's body); the arrival
            // pump takes 2 (the depth-12 budget), 10 wait on the cadence.
            let fence = (1..=12).fold(String::new(), |acc, i| acc + &format!("line {i:02}\n"));
            run.live
                .send(delta(1, 1, &format!("```\n{fence}")))
                .expect("feed");
            let body_rows = || {
                all_rows(&world)
                    .iter()
                    .filter(|row| row.starts_with("line "))
                    .count()
            };
            flush_feed_until(|| body_rows() > 0).await;
            assert_eq!(body_rows(), 2, "the paced arrival budget");

            // Esc: everything dumps whole — a 10 ms demand frame, NOT a
            // 33 ms tick, puts all 12 rows out.
            rig.input.send(key(KeyCode::Esc)).expect("interrupt");
            flush_feed_until(|| body_rows() > 2).await;
            assert_eq!(body_rows(), 12, "the backlog dumped instantly");
            assert!(
                matches!(driver.commands().as_slice(), [Command::Interrupt { .. }]),
                "commands: {:?}",
                driver.commands()
            );

            // The note lands right behind it (the dump rides the run_end
            // too), then the collapse — again with no 33 ms tick waited.
            run.record(2, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome
                .send(Ok(vec![Message::user("stop it")]))
                .expect("outcome");
            flush_feed_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            assert!(
                has_row(&world.nonblank_rows(), "Worked for 0s"),
                "the note landed instantly: {:?}",
                world.nonblank_rows()
            );
            let visible = visible_content(&world);
            assert_eq!(
                visible[visible.len() - 2..],
                [COMPOSER_PLACEHOLDER.to_string(), "kimi·k2".to_string()],
                "collapsed: {visible:?}"
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// A first attach's replayed history bypasses the pacing entirely: the
/// rebuild's rows insert whole in the sync handler — zero virtual time
/// passes, so the paced drain never got a tick.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn an_attach_replay_bypasses_the_pacing() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot(&world);
            let driver = rig.driver.clone_handles();
            // The attach baseline carries a 3-row assistant message (20
            // tokens at 80 columns) from a previous session.
            let mut sync = sync_with_pending(Vec::new(), Vec::new());
            sync.history.messages = vec![Message::text(
                cadmus_contract::Role::Assistant,
                tokens(1, 20),
            )];
            *driver.baseline.lock().expect("baseline") = Some(sync);
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "replay me");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            // Real-time delivery only — NOT a virtual millisecond: the whole
            // replay is already there (paced emission would have put out
            // one budget at most).
            flush_feed_until(|| has_row(&world.nonblank_rows(), &tokens(1, 8))).await;
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "❯ replay me".to_string(),
                    tokens(1, 8),
                    tokens(9, 16),
                    tokens(17, 20),
                    "Working · 0s".to_string(),
                    BUSY_PLACEHOLDER.to_string(),
                    "kimi·k2".to_string(),
                ],
                "the replay inserted whole, no pacing tick: {:?}",
                world.nonblank_rows()
            );

            let run = driver.take_run();
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle().await;
            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// `Motion::Reduced` boots into the instant-emission behavior (its
/// differentiated calmer drain is the pacing-refinement item's consumer —
/// until then it must not pace, pinned here so the alias cannot rot).
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_reduced_profile_emits_instantly() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot_with_config(&world, Vec::new(), Vec::new(), Motion::Reduced);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "go");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            let fence = (1..=10).fold(String::new(), |acc, i| acc + &format!("line {i:02}\n"));
            run.live
                .send(delta(1, 1, &format!("```\n{fence}")))
                .expect("feed");
            let body_rows = || {
                all_rows(&world)
                    .iter()
                    .filter(|row| row.starts_with("line "))
                    .count()
            };
            flush_feed_until(|| body_rows() > 0).await;
            assert_eq!(body_rows(), 10, "reduced: instant emission");

            run.live
                .send(llm_response(2, 1, &format!("```\n{fence}```\n")))
                .expect("feed");
            run.record(3, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;

            quit_and_join(task, &rig.input).await;
        })
        .await;
}

/// The TERM=dumb motion profile (`Motion::None`): every pump drains the
/// whole queue — the typewriter is off, emission is instant.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn the_dumb_terminal_profile_emits_instantly() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let world = World::new();
            let (mut app, rig) = boot_unpaced(&world);
            let driver = rig.driver.clone_handles();
            let task = tokio::task::spawn_local(async move { app.run_loop().await });
            settle().await;

            type_text(&rig, "go");
            rig.input.send(key(KeyCode::Enter)).expect("input");
            settle().await;
            let run = driver.take_run();

            // 10 stable rows queue; the arrival pump drains them ALL — no
            // 33 ms tick involved.
            let fence = (1..=10).fold(String::new(), |acc, i| acc + &format!("line {i:02}\n"));
            run.live
                .send(delta(1, 1, &format!("```\n{fence}")))
                .expect("feed");
            let body_rows = || {
                all_rows(&world)
                    .iter()
                    .filter(|row| row.starts_with("line "))
                    .count()
            };
            flush_feed_until(|| body_rows() > 0).await;
            assert_eq!(body_rows(), 10, "unpaced: instant emission");

            run.live
                .send(llm_response(2, 1, &format!("```\n{fence}```\n")))
                .expect("feed");
            run.record(3, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| has_row(&world.nonblank_rows(), "Worked for")).await;
            let visible = visible_content(&world);
            assert_eq!(
                visible[visible.len() - 2..],
                [COMPOSER_PLACEHOLDER.to_string(), "kimi·k2".to_string()],
                "collapsed: {visible:?}"
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}
