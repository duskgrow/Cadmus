//! The app loop, locked end to end with vt100 (ADR-0018 items 5 and 10):
//! keys drive the composer, Enter spawns a run through the scripted driver,
//! the live feed streams into the band and out to scrollback, Esc sends the
//! interrupt command, and the outcome folds back into the session history.
//! The rig shares `tests/common`'s vt100 world; the strongest assertion is
//! again the full non-blank row sequence (scrollback + screen, oldest
//! first).
//!
//! Time is tokio's paused clock (frame gate, debounce); the drainer thread
//! is real-time but only forwards into a channel, so a few settle pumps
//! always converge the world.

mod common;

use std::sync::{Arc, Mutex};

use cadmus_contract::{
    Approval, Attachment, Command, Event as TraceEvent, EventKind, InFlight, LiveItem, LiveKind,
    LiveUpdate, Message, PendingApproval, RunState, SettledApproval, StreamChunk, Sync, ToolCall,
    ToolCompletion,
};
use cadmus_tui::app::{App, AppConfig, RunDriver, RunHandle};
use cadmus_tui::shell::InlineShell;
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
        let reattachment = Arc::clone(&self.reattachment);
        RunHandle {
            attachment: Attachment {
                sync: sync_with_pending(pending, settled),
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
    let (input_tx, input) = ScriptedInput::channel();
    let guard = GuardSink::default();
    let shell = InlineShell::new(world.backend.clone(), guard, 2).expect("boot shell");
    let driver = ScriptDriver::new();
    *driver.pending.lock().expect("pending") = pending;
    *driver.settled.lock().expect("settled") = settled;
    let app = App::new(
        shell,
        input,
        Box::new(driver.clone_handles()),
        AppConfig {
            label: "kimi·k2".into(),
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
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the world never reached the expected state");
}

/// The visible status row (the band anchors at the cursor, so find it by
/// content): the label starts it, the run state rides the right edge.
fn status_row(world: &World) -> String {
    world
        .visible_rows()
        .into_iter()
        .find(|row| row.starts_with("kimi·k2"))
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
                    .contains(&"> fix the test".to_string()),
                "world: {:?}",
                world.nonblank_rows()
            );
            assert!(
                !world.nonblank_rows().contains(&"… resynced …".to_string()),
                "a spurious resync marker: {:?}",
                world.nonblank_rows()
            );

            // The run streams: text lands in the band, completed content
            // flushes continuously.
            run.live
                .send(delta(1, 1, "reading main.rs\n\n"))
                .expect("feed");
            settle_until(|| {
                world
                    .nonblank_rows()
                    .contains(&"reading main.rs".to_string())
            })
            .await;
            assert!(status_row(&world).ends_with("streaming"));

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
            settle_until(|| status_row(&world) == "kimi·k2").await;
            // The strongest assertion form: the exact full row sequence —
            // any lost, duplicated or marker-polluted row breaks it.
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "$ cadmus chat",
                    "> fix the test",
                    "reading main.rs",
                    "→ read_file",
                    "kimi·k2",
                ],
                "scrollback+screen sequence (visible: {:?}, scrollback: {:?})",
                world.visible_rows(),
                world.scrollback_rows()
            );
            assert_eq!(status_row(&world), "kimi·k2");

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

/// The status row with the streaming state right-aligned at 80 columns —
/// the exact full-sequence assertions pin padding, not just content.
fn streaming_status_row() -> String {
    format!("kimi·k2{:<64}streaming", "")
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
    "Tab: arm selected call · y/n: answer it",
    "> 1/2 → write_file src/main.rs",
    "+ fn main() {}",
    "  2/2 → edit_file src/main.rs (pending)",
];

/// A call can be answered ahead of its sibling, even with duplicated wire
/// ids. Only the recorded answer settles it; focus advances independently.
#[tokio::test(start_paused = true, flavor = "current_thread")]
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
            assert_eq!(
                world.nonblank_rows(),
                [
                    vec!["> change main".to_string()],
                    APPROVAL_SECTION.map(String::from).to_vec(),
                    vec![streaming_status_row()],
                ]
                .concat()
            );
            assert!(status_row(&world).ends_with("streaming"));

            rig.input.send(key(KeyCode::Tab)).expect("arm first");
            rig.input.send(key(KeyCode::Tab)).expect("select second");
            settle().await;
            assert!(has_row(&world.visible_rows(), "> 2/2 → edit_file"));
            assert!(has_row(&world.visible_rows(), "- let a = 1;"));
            rig.input.send(key(KeyCode::Char('y'))).expect("input");
            // Unix auto-repeat is indistinguishable from ordinary Press.
            rig.input.send(key(KeyCode::Char('y'))).expect("repeat");
            settle().await;
            assert_eq!(
                driver.commands(),
                vec![call_decision("tui-0", "ap1", 1, Approval::Approved)]
            );
            assert!(has_row(&world.visible_rows(), "> 1/2 → write_file"));
            assert!(has_row(
                &world.visible_rows(),
                "edit_file src/main.rs (sent)"
            ));
            assert!(!has_row(&world.nonblank_rows(), "✓ approved"));

            run.record(2, EventKind::Command(driver.commands()[0].clone()));
            settle_until(|| has_row(&world.nonblank_rows(), "✓ approved edit_file")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("repeat after resolve");
            settle().await;
            assert_eq!(driver.commands().len(), 1);
            assert!(has_row(&world.visible_rows(), "> 1/2 → write_file"));
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "> change main",
                    "✓ approved edit_file",
                    "✗ rejected write_file",
                    "→ edit_file src/main.rs",
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
            settle_until(|| has_row(&world.visible_rows(), "> 1/2 → write_file")).await;
            rig.input
                .send(key(KeyCode::Char('y')))
                .expect("held across new prompt");
            settle().await;
            assert_eq!(driver.commands().len(), 2);
            drop(tail);
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
            assert!(has_row(&world.visible_rows(), "> 1/2 → write_file"));
            assert!(has_row(
                &world.nonblank_rows(),
                "✓ completed edit_file (turn 1, tool call 2)"
            ));
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
            assert!(has_row(&world.visible_rows(), "> 2/2 → edit_file"));
            assert!(has_row(&world.nonblank_rows(), "✓ completed write_file"));
            rig.input.send(key(KeyCode::Tab)).expect("arm sibling");
            rig.input
                .send(key(KeyCode::Char('n')))
                .expect("reject sibling");
            settle().await;
            run.record(5, EventKind::Command(driver.commands()[0].clone()));
            run.record(6, EventKind::RunFinished { turns: 1 });
            drop(run.live);
            run.outcome.send(Ok(Vec::new())).expect("outcome");
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
            assert_eq!(
                world.nonblank_rows(),
                vec![
                    "> change main",
                    "✗ rejected write_file",
                    "✗ rejected edit_file",
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
            settle_until(|| world.visible_rows().iter().any(|row| row.contains("> 2/2 → edit_file"))).await;
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
            assert_eq!(rows.iter().filter(|row| row.contains("✓ approved write_file")).count(), 1);
            assert!(rows.iter().any(|row| row.contains("✗ rejected edit_file")));
            assert!(!rows.iter().any(|row| row.contains("✗ rejected write_file")));

            drop(run.live);
            run.outcome
                .send(Ok(vec![
                    Message::user("change main"),
                    Message::text(cadmus_contract::Role::Assistant, "done\n\n"),
                ]))
                .expect("outcome");
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
            settle_until(|| has_row(&world.visible_rows(), "> 1/2 → write_file")).await;
            run.record(
                11,
                EventKind::Command(Command::ResolveApprovalCall {
                    command_id: "remote".into(),
                    request_id: "ap1".into(),
                    call_index: 0,
                    decision: Approval::Approved,
                }),
            );
            settle_until(|| has_row(&world.visible_rows(), "> 2/2 → edit_file")).await;
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
            assert!(has_row(&world.visible_rows(), "> 2/2 → edit_file"));
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
            assert!(!has_row(&world.nonblank_rows(), "✓ approved edit_file"));
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
                    .any(|row| row.contains("✓ approved write_file"))
            })
            .await;
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .any(|row| row.contains("✗ rejected edit_file")),
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
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
            settle_until(|| status_row(&world) == "kimi·k2").await;
            // The dialog died with the run.
            assert!(
                world
                    .nonblank_rows()
                    .iter()
                    .all(|row| !row.contains("approve 2 call(s)")),
                "world: {:?}",
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
            settle_until(|| status_row(&world).ends_with("failed")).await;

            let rows = world.nonblank_rows();
            // Exactly one failure marker — the outcome and the feed must
            // never double-print (teardown is serialized, drainer docs).
            let markers = rows
                .iter()
                .filter(|row| row.as_str() == "run failed: provider call failed: auth")
                .count();
            assert_eq!(markers, 1, "rows: {rows:?}");
            // The state rides the right edge (padding is width-dependent).
            let status = status_row(&world);
            assert!(
                status.starts_with("kimi·k2") && status.ends_with("failed"),
                "status row: {status:?}"
            );

            quit_and_join(task, &rig.input).await;
        })
        .await;
}
