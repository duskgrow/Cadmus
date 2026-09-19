//! Per-call gate/dispatch interleavings. Clients and tools rendezvous on
//! channels, so progress assertions never depend on a sleep or poll order.
//!
//! Its own file rather than a `mod tests` inside `gate.rs` / `dispatch.rs`:
//! the interleavings belong to neither module alone (they drive the gate,
//! dispatch, inbox and published events together), and the harness below —
//! injected clock, command source, live sink and a releasing probe tool — is
//! most of the length, so inlining would bury the code under its tests.
//! Crate-internal rather than `tests/`: the assertions read `AgentLoop`
//! internals and drive the loop through its injected ports, which only a
//! `#[cfg(test)]` module can reach. End-to-end run/replay coverage lives in
//! `crates/cadmus-core/tests/` and `crates/cadmus/tests/approvals.rs`.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use cadmus_contract::{
    Approval, Clock, Command, CommandSource, Event, EventKind, LiveItem, LiveKind, LiveSink,
    Message, TimedRecv, ToolCall, ToolCompletion, ToolResultProjection, ToolSpec, attrs,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::fixtures::{EchoTool, call, test_capabilities, text_script, tool_call_script};
use super::{AgentError, AgentLoop, AgentTool, ClientProtocol, Concurrency, Effect, ToolError};
use crate::ReplayProvider;
use crate::testing::{RecordingSink, test_context, test_telemetry};

#[derive(Default)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

struct Commands {
    rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Command>>,
    waits: mpsc::UnboundedSender<Duration>,
    timeout: Arc<Notify>,
}

#[async_trait]
impl CommandSource for Commands {
    async fn recv(&self) -> Option<Command> {
        self.rx.lock().await.recv().await
    }

    async fn recv_timeout(&self, duration: Duration) -> TimedRecv {
        let _ = self.waits.send(duration);
        tokio::select! {
            command = self.recv() => command.map_or(TimedRecv::Closed, TimedRecv::Command),
            () = self.timeout.notified() => TimedRecv::TimedOut,
        }
    }

    fn poll(&self) -> Option<Command> {
        self.rx.try_lock().ok()?.try_recv().ok()
    }
}

struct Live {
    sender: mpsc::UnboundedSender<LiveItem>,
    recording: Arc<crate::testing::RecordingLive>,
}

impl LiveSink for Live {
    fn publish(&self, item: &LiveItem) {
        self.recording.publish(item);
        let _ = self.sender.send(item.clone());
    }
}

struct Probe {
    index: usize,
    name: &'static str,
    concurrency: Concurrency,
    effect: Effect,
    starts: mpsc::UnboundedSender<usize>,
    ends: mpsc::UnboundedSender<usize>,
    release: Arc<Notify>,
    completed: Arc<Mutex<Vec<usize>>>,
}

#[async_trait]
impl AgentTool for Probe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "controlled invocation".into(),
            parameters: json!({"type": "object"}),
        }
    }

    fn concurrency(&self) -> Concurrency {
        self.concurrency
    }

    fn effect(&self) -> Effect {
        self.effect
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let _ = self.starts.send(self.index);
        self.release.notified().await;
        self.completed.lock().expect("completed").push(self.index);
        let _ = self.ends.send(self.index);
        if arguments.get("fail").and_then(Value::as_bool) == Some(true) {
            Err(ToolError {
                tool: self.name.into(),
                message: "probe failed".into(),
            })
        } else {
            Ok(json!(self.name))
        }
    }
}

struct Client {
    commands: Option<mpsc::UnboundedSender<Command>>,
    live: mpsc::UnboundedReceiver<LiveItem>,
    waits: mpsc::UnboundedReceiver<Duration>,
    starts: mpsc::UnboundedReceiver<usize>,
    ends: mpsc::UnboundedReceiver<usize>,
    releases: Vec<Arc<Notify>>,
    timeout: Arc<Notify>,
    clock: Arc<TestClock>,
    sink: Arc<RecordingSink>,
}

impl Client {
    fn send(&self, command: Command) {
        self.commands
            .as_ref()
            .expect("connected")
            .send(command)
            .expect("command");
    }

    async fn request(&mut self) -> (String, Vec<ToolCall>) {
        loop {
            if let LiveKind::ApprovalRequested {
                request_id, calls, ..
            } = self.live.recv().await.expect("request").kind
            {
                return (request_id, calls);
            }
        }
    }

    async fn completion(&mut self) -> ToolCompletion {
        loop {
            if let LiveKind::ToolCompleted { completion } =
                self.live.recv().await.expect("completion").kind
            {
                return completion;
            }
        }
    }

    async fn resolution(&mut self) -> Command {
        loop {
            if let LiveKind::Recorded { event } = self.live.recv().await.expect("resolution").kind
                && let EventKind::Command(
                    command @ (Command::ResolveApproval { .. }
                    | Command::ResolveApprovalCall { .. }),
                ) = event.kind
            {
                return command;
            }
        }
    }
}

struct Dispatched {
    result: Result<(), AgentError>,
    messages: Vec<Message>,
    events: Vec<Event>,
    live: Vec<LiveItem>,
    completed: Vec<usize>,
}

async fn dispatch<F, Fut>(
    specs: &[(&'static str, Concurrency, Effect)],
    calls: Vec<ToolCall>,
    script: F,
) -> Dispatched
where
    F: FnOnce(Client) -> Fut,
    Fut: Future<Output = ()>,
{
    let (commands, rx) = mpsc::unbounded_channel();
    let (live, live_rx) = mpsc::unbounded_channel();
    let recording = Arc::new(crate::testing::RecordingLive::default());
    let (waits, waits_rx) = mpsc::unbounded_channel();
    let (starts, starts_rx) = mpsc::unbounded_channel();
    let (ends, ends_rx) = mpsc::unbounded_channel();
    let timeout = Arc::new(Notify::new());
    let completed = Arc::new(Mutex::new(Vec::new()));
    let releases: Vec<_> = specs.iter().map(|_| Arc::new(Notify::new())).collect();
    let tools = specs
        .iter()
        .enumerate()
        .map(|(index, &(name, concurrency, effect))| {
            Arc::new(Probe {
                index,
                name,
                concurrency,
                effect,
                starts: starts.clone(),
                ends: ends.clone(),
                release: releases[index].clone(),
                completed: completed.clone(),
            }) as Arc<dyn AgentTool>
        })
        .collect();
    let (mut telemetry, sink) = test_telemetry("tr-approval");
    let clock = Arc::new(TestClock::default());
    telemetry.clock = clock.clone();
    let agent = AgentLoop::new(
        Arc::new(ReplayProvider::new([]).with_capabilities(test_capabilities())),
        tools,
        test_context(),
        ClientProtocol {
            live: Arc::new(Live {
                sender: live,
                recording: recording.clone(),
            }),
            commands: Arc::new(Commands {
                rx: tokio::sync::Mutex::new(rx),
                waits,
                timeout: timeout.clone(),
            }),
        },
        8,
        telemetry,
    );
    let client = Client {
        commands: Some(commands),
        live: live_rx,
        waits: waits_rx,
        starts: starts_rx,
        ends: ends_rx,
        releases,
        timeout,
        clock,
        sink: sink.clone(),
    };
    let mut messages = Vec::new();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            agent.dispatch_tools(calls, &mut messages, "root", 1),
            script(client)
        )
    })
    .await
    .expect("dispatch and client must make progress");
    let completed = completed.lock().expect("completed").clone();
    Dispatched {
        result,
        messages,
        events: sink.events(),
        live: recording.items(),
        completed,
    }
}

fn answer(request: &str, id: &str, call_index: usize, decision: Approval) -> Command {
    Command::ResolveApprovalCall {
        command_id: id.into(),
        request_id: request.into(),
        call_index,
        decision,
    }
}

fn recorded_resolutions(events: &[Event]) -> Vec<&Command> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Command(
                command @ (Command::ResolveApproval { .. } | Command::ResolveApprovalCall { .. }),
            ) => Some(command),
            _ => None,
        })
        .collect()
}

/// A synchronous per-call policy exercises the run spine without a timer.
struct PerCallResolver(std::sync::mpsc::Sender<Command>);

impl LiveSink for PerCallResolver {
    fn publish(&self, item: &LiveItem) {
        if let LiveKind::ApprovalRequested {
            request_id, calls, ..
        } = &item.kind
        {
            for index in (0..calls.len()).rev() {
                self.0
                    .send(answer(
                        request_id,
                        &format!("call-{index}"),
                        index,
                        Approval::Approved,
                    ))
                    .expect("command");
            }
        }
    }
}

#[tokio::test]
async fn per_call_resolution_round_trips_through_the_run_and_replay() {
    let (commands, sender) = crate::testing::ChannelCommands::new();
    let (telemetry, sink) = test_telemetry("tr-per-call");
    let agent = AgentLoop::new(
        Arc::new(ReplayProvider::new([
            tool_call_script("call", "{\"text\":\"done\"}"),
            text_script("finished"),
        ])),
        vec![Arc::new(EchoTool)],
        test_context(),
        ClientProtocol {
            live: Arc::new(PerCallResolver(sender)),
            commands: Arc::new(commands),
        },
        8,
        telemetry,
    );
    let outcome = agent
        .run(&cadmus_contract::ChatRequest::user_text("test", 1_024))
        .await
        .expect("run");
    let events = sink.events();
    assert!(matches!(
        recorded_resolutions(&events).as_slice(),
        [Command::ResolveApprovalCall { .. }]
    ));
    assert_eq!(outcome.turns, 2);
    let replay = crate::replay_trace(&events);
    assert_eq!(replay.messages, outcome.messages);
    assert!(replay.dangling_tool_calls.is_empty());
}

#[tokio::test]
async fn a_failed_per_call_record_never_permits_execution() {
    struct FailApproval;
    impl cadmus_contract::EventSink for FailApproval {
        fn append(&self, event: &Event) -> Result<(), cadmus_contract::LogError> {
            assert!(
                matches!(
                    event.kind,
                    EventKind::Command(Command::ResolveApprovalCall { .. })
                ),
                "no tool call may precede a durable approval"
            );
            Err(cadmus_contract::LogError::Io(std::io::Error::other(
                "approval append failed",
            )))
        }
    }
    let (commands, sender) = crate::testing::ChannelCommands::new();
    let (mut telemetry, _) = test_telemetry("tr-log-failure");
    telemetry.sink = Arc::new(FailApproval);
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let agent = AgentLoop::new(
        Arc::new(ReplayProvider::new([])),
        vec![Arc::new(super::fixtures::StepTool {
            name: "step",
            log: log.clone(),
        })],
        test_context(),
        ClientProtocol {
            live: Arc::new(PerCallResolver(sender)),
            commands: Arc::new(commands),
        },
        8,
        telemetry,
    );
    let mut messages = Vec::new();
    let result = agent
        .dispatch_tools(vec![call("call", "step")], &mut messages, "root", 1)
        .await;
    assert!(matches!(result, Err(AgentError::Log(_))));
    assert!(log.lock().await.is_empty());
    assert!(messages.is_empty());
}

const PARALLEL: Concurrency = Concurrency::ParallelSafe;
const MUTATION: Effect = Effect::Mutation;

#[tokio::test]
async fn out_of_order_approval_executes_before_siblings_with_gated_indices_and_duplicate_ids() {
    let result = dispatch(
        &[
            ("read", PARALLEL, Effect::Perception),
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
        ],
        vec![
            call("read-id", "read"),
            call("same-id", "first"),
            call("same-id", "second"),
        ],
        |mut client| async move {
            let (request, calls) = client.request().await;
            assert_eq!(
                calls
                    .iter()
                    .map(|call| call.name.as_str())
                    .collect::<Vec<_>>(),
                ["first", "second"]
            );
            client.send(answer(&request, "second", 1, Approval::Approved));
            let mut starts = vec![
                client.starts.recv().await.unwrap(),
                client.starts.recv().await.unwrap(),
            ];
            starts.sort_unstable();
            assert_eq!(starts, [0, 2], "gated index 1 is original call 2");
            client.releases[2].notify_one();
            assert_eq!(client.ends.recv().await, Some(2));
            assert!(
                client.starts.try_recv().is_err(),
                "the unanswered sibling never runs"
            );
            assert!(
                !client
                    .sink
                    .events()
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::ToolResult { .. })),
                "later results wait for the original prefix"
            );
            client.releases[0].notify_one();
            assert_eq!(client.ends.recv().await, Some(0));
            client.send(answer(&request, "first", 0, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [2, 0, 1]);
    assert_eq!(
        result
            .messages
            .iter()
            .map(Message::text_body)
            .collect::<Vec<_>>(),
        ["read", "first", "second"]
    );
    let replay = crate::replay_trace(&result.events);
    assert_eq!(replay.messages, result.messages);
    assert!(replay.dangling_tool_calls.is_empty());
    let Command::ResolveApprovalCall { request_id, .. } = recorded_resolutions(&result.events)[0]
    else {
        panic!("per-call resolution");
    };
    let results: Vec<_> = result
        .events
        .iter()
        .filter(|event| matches!(event.kind, EventKind::ToolResult { .. }))
        .collect();
    assert!(
        !results[0]
            .attributes
            .contains_key(attrs::APPROVAL_REQUEST_ID)
    );
    assert!(
        !results[0]
            .attributes
            .contains_key(attrs::APPROVAL_CALL_INDEX)
    );
    assert_eq!(
        replay.tool_results,
        vec![
            ToolResultProjection {
                message_index: 1,
                request_id: request_id.clone(),
                call_index: 0
            },
            ToolResultProjection {
                message_index: 2,
                request_id: request_id.clone(),
                call_index: 1
            },
        ]
    );
}

#[tokio::test]
async fn interrupt_preserves_the_later_results_approval_address_with_duplicate_ids() {
    let result = dispatch(
        &[("A", PARALLEL, MUTATION), ("B", PARALLEL, MUTATION)],
        vec![call("duplicate", "A"), call("duplicate", "B")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "approve-b", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            client.releases[1].notify_one();
            assert_eq!(client.completion().await.name, "B");
            client.send(Command::Interrupt {
                command_id: "stop".into(),
            });
        },
    )
    .await;
    assert!(matches!(result.result, Err(AgentError::Interrupted)));
    assert_eq!(result.completed, [1]);
    assert_eq!(
        result.messages,
        [Message::tool_result("duplicate", json!("B"))]
    );
    let Command::ResolveApprovalCall { request_id, .. } = recorded_resolutions(&result.events)[0]
    else {
        panic!("per-call resolution");
    };
    let event = result
        .events
        .iter()
        .find(|event| matches!(event.kind, EventKind::ToolResult { .. }))
        .expect("durable B result");
    assert_eq!(event.attributes[attrs::APPROVAL_REQUEST_ID], *request_id);
    assert_eq!(event.attributes[attrs::APPROVAL_CALL_INDEX], 1);
    // This is the attach window after B drains but before the terminal record
    // retires the partial request. The result is first, its gated index is 1.
    let before_terminal: Vec<_> = result
        .events
        .iter()
        .take_while(|event| !matches!(event.kind, EventKind::RunFinished { .. }))
        .cloned()
        .collect();
    let replay = crate::replay_trace(&before_terminal);
    assert!(replay.finished.is_none());
    assert_eq!(replay.messages, result.messages);
    assert_eq!(
        replay.tool_results,
        [ToolResultProjection {
            message_index: 0,
            request_id: request_id.clone(),
            call_index: 1,
        }]
    );
}

#[tokio::test]
async fn retries_conflicts_invalid_indices_and_late_answers_have_no_effect() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
        ],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer("unknown", "unknown", 0, Approval::Approved));
            client.send(answer(&request, "invalid", usize::MAX, Approval::Approved));
            let first = answer(&request, "first", 1, Approval::Approved);
            client.send(first.clone());
            client.send(first);
            client.send(answer(
                &request,
                "conflict",
                1,
                Approval::Rejected { comment: None },
            ));
            assert_eq!(client.starts.recv().await, Some(1));
            client.send(answer(&request, "other", 0, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(0));
            client.send(answer(
                &request,
                "late",
                0,
                Approval::Rejected { comment: None },
            ));
            client.send(Command::ResolveApproval {
                command_id: "late-batch".into(),
                request_id: request,
                decisions: Vec::new(),
            });
            for release in &client.releases {
                release.notify_one();
            }
        },
    )
    .await;
    result.result.expect("dispatch");
    assert!(result.messages.iter().all(|message| !message.is_error));
    assert_eq!(
        recorded_resolutions(&result.events)
            .iter()
            .map(|command| command.command_id())
            .collect::<Vec<_>>(),
        [Some("first"), Some("other")]
    );
}

#[tokio::test]
async fn mixed_serial_batches_skip_undecided_and_completed_calls_without_overlap() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
            ("serial", Concurrency::Serial, MUTATION),
        ],
        vec![
            call("c1", "first"),
            call("c2", "second"),
            call("c3", "serial"),
        ],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "third", 2, Approval::Approved));
            assert_eq!(
                client.starts.recv().await,
                Some(2),
                "a ready serial call skips undecided siblings"
            );
            client.send(answer(&request, "second", 1, Approval::Approved));
            client.resolution().await;
            client.resolution().await;
            assert!(
                client.starts.try_recv().is_err(),
                "a mixed batch has only one invocation at a time"
            );
            client.releases[2].notify_one();
            assert_eq!(client.starts.recv().await, Some(1));
            assert_eq!(client.ends.recv().await, Some(2));
            client.releases[1].notify_one();
            assert_eq!(client.ends.recv().await, Some(1));
            // Both later slots are completed, not blockers or restart candidates.
            assert_eq!(client.completion().await.message_call_index, 2);
            assert_eq!(client.completion().await.message_call_index, 1);
            client.send(answer(&request, "first", 0, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(0));
            client.releases[0].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [2, 1, 0]);
    assert_eq!(
        result
            .messages
            .iter()
            .map(Message::text_body)
            .collect::<Vec<_>>(),
        ["first", "second", "serial"]
    );
    assert_eq!(
        crate::replay_trace(&result.events).messages,
        result.messages
    );
}

async fn completion_before_sibling_answer(fail: bool, concurrency: Concurrency) {
    let mut second = call("duplicate", "second");
    second.arguments = json!({"fail": fail});
    let result = dispatch(
        &[
            ("read", concurrency, Effect::Perception),
            ("first", concurrency, MUTATION),
            ("second", concurrency, MUTATION),
        ],
        vec![call("read", "read"), call("duplicate", "first"), second],
        |mut client| async move {
            let (request, _) = client.request().await;
            assert_eq!(client.starts.recv().await, Some(0));
            client.releases[0].notify_one();
            assert_eq!(client.ends.recv().await, Some(0));
            client.send(answer(&request, "second", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(2));
            client.releases[2].notify_one();
            let completion = client.completion().await;
            assert_eq!(
                completion.message_call_index, 2,
                "completion uses the full batch index"
            );
            assert_eq!(completion.call_id, "duplicate");
            assert_eq!(completion.name, "second");
            assert_eq!(completion.turn, 1);
            assert_eq!(completion.error.is_some(), fail);
            if fail {
                assert_eq!(
                    completion.error.as_ref().unwrap().kind,
                    cadmus_contract::error_kinds::TOOL
                );
                assert!(completion.result.as_str().unwrap().contains("probe failed"));
            } else {
                assert_eq!(completion.result, json!("second"));
            }
            let events = client.sink.events();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event.kind, EventKind::ToolResult { .. }))
                    .count(),
                1,
                "only the ungated prefix is durable while the sibling remains unanswered"
            );
            assert_eq!(recorded_resolutions(&events).len(), 1);
            assert!(
                events
                    .iter()
                    .any(|event| event.span_id == completion.span_id
                        && matches!(
                            &event.kind, EventKind::ToolCall { call } if call.name == "second"
                        ))
            );
            client.send(answer(&request, "first", 0, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    let completions: Vec<_> = result
        .live
        .iter()
        .filter_map(|item| match &item.kind {
            LiveKind::ToolCompleted { completion } => Some((item.seq, completion)),
            _ => None,
        })
        .collect();
    assert_eq!(
        completions.len(),
        1,
        "immediately flushable outcomes need no provisional event"
    );
    let (seq, completion) = completions[0];
    let durable: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::ToolResult { result, .. } => Some((event, result)),
            _ => None,
        })
        .collect();
    assert_eq!(durable.len(), 3);
    assert_eq!(durable[0].1, &json!("read"));
    assert_eq!(durable[1].1, &json!("first"));
    assert_eq!(durable[2].0.span_id, completion.span_id);
    assert_eq!(durable[2].1, &completion.result);
    assert_eq!(durable[2].0.error, completion.error);
    assert!(seq < durable[1].0.seq && durable[1].0.seq < durable[2].0.seq);
    assert_eq!(result.messages[2].is_error, fail);
    assert_eq!(
        crate::replay_trace(&result.events).messages,
        result.messages
    );
}

#[tokio::test]
async fn early_success_is_visible_before_sibling_answer_and_reconciles_by_span() {
    for concurrency in [PARALLEL, Concurrency::Serial] {
        completion_before_sibling_answer(false, concurrency).await;
    }
}

#[tokio::test]
async fn early_failure_is_visible_before_sibling_answer_and_reconciles_by_span() {
    for concurrency in [PARALLEL, Concurrency::Serial] {
        completion_before_sibling_answer(true, concurrency).await;
    }
}

#[tokio::test]
async fn serial_batch_approved_together_keeps_original_order_without_provisional_results() {
    let result = dispatch(
        &[
            ("first", Concurrency::Serial, MUTATION),
            ("second", Concurrency::Serial, MUTATION),
        ],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(Command::ResolveApproval {
                command_id: "batch".into(),
                request_id: request,
                decisions: vec![Approval::Approved, Approval::Approved],
            });
            assert_eq!(client.starts.recv().await, Some(0));
            client.releases[0].notify_one();
            assert_eq!(client.starts.recv().await, Some(1));
            assert_eq!(client.ends.recv().await, Some(0));
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [0, 1]);
    assert!(
        result
            .live
            .iter()
            .all(|item| !matches!(item.kind, LiveKind::ToolCompleted { .. }))
    );
}

#[tokio::test]
async fn legacy_short_batch_uses_original_indices_and_preserves_partial_decisions() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
            ("last", PARALLEL, MUTATION),
        ],
        vec![
            call("c1", "first"),
            call("c2", "second"),
            call("c3", "last"),
        ],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(
                &request,
                "reject-first",
                0,
                Approval::Rejected { comment: None },
            ));
            client.resolution().await;
            client.send(Command::ResolveApproval {
                command_id: "batch".into(),
                request_id: request,
                decisions: vec![Approval::Approved, Approval::Approved],
            });
            assert_eq!(client.starts.recv().await, Some(1));
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [1]);
    assert_eq!(
        result
            .messages
            .iter()
            .map(|message| message.is_error)
            .collect::<Vec<_>>(),
        [true, false, true]
    );
    assert_eq!(recorded_resolutions(&result.events).len(), 2);
}

#[tokio::test]
async fn timeout_after_partial_approval_denies_only_unanswered_and_drains_execution() {
    let result = dispatch(
        &[("first", PARALLEL, MUTATION), ("second", PARALLEL, MUTATION)],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "second", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            client.resolution().await;
            client.timeout.notify_one();
            let Command::ResolveApproval { decisions, .. } = client.resolution().await else {
                panic!("timeout resolves the remainder");
            };
            assert!(matches!(&decisions[0], Approval::Rejected { comment: Some(comment) } if comment.contains("timed out")));
            assert_eq!(decisions[1], Approval::Approved);
            client.send(answer(&request, "too-late", 0, Approval::Approved));
            client.releases[1].notify_one();
        },
    ).await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [1]);
    assert!(result.messages[0].is_error);
    assert!(!result.messages[1].is_error);
    assert_eq!(recorded_resolutions(&result.events).len(), 2);
}

#[tokio::test]
async fn closed_channel_after_partial_approval_preserves_the_approved_call() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
        ],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "second", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            client.resolution().await;
            client.commands.take();
            let Command::ResolveApproval { decisions, .. } = client.resolution().await else {
                panic!("closure resolves the remainder");
            };
            assert_eq!(
                decisions,
                [Approval::Rejected { comment: None }, Approval::Approved]
            );
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [1]);
    assert!(result.messages[0].is_error);
    assert!(!result.messages[1].is_error);
    assert_eq!(
        crate::replay_trace(&result.events).messages,
        result.messages
    );
}

#[tokio::test]
async fn partial_answers_strays_and_invalid_commands_do_not_restart_the_wait_budget() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
        ],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            assert_eq!(client.waits.recv().await, Some(Duration::from_secs(300)));
            client.clock.0.store(60_000, Ordering::Relaxed);
            client.send(answer(&request, "second", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(1));
            assert_eq!(client.waits.recv().await, Some(Duration::from_secs(240)));
            client.clock.0.store(120_000, Ordering::Relaxed);
            client.send(answer("unknown", "stray", 0, Approval::Approved));
            assert_eq!(client.waits.recv().await, Some(Duration::from_secs(180)));
            client.clock.0.store(299_000, Ordering::Relaxed);
            client.send(answer(&request, "invalid", 2, Approval::Approved));
            assert_eq!(client.waits.recv().await, Some(Duration::from_secs(1)));
            client.clock.0.store(300_000, Ordering::Relaxed);
            client.send(answer(&request, "at-deadline", 0, Approval::Approved));
            client.resolution().await;
            assert!(matches!(
                client.resolution().await,
                Command::ResolveApproval { .. }
            ));
            client.releases[1].notify_one();
        },
    )
    .await;
    result.result.expect("dispatch");
    assert_eq!(result.completed, [1]);
    assert!(result.messages[0].is_error);
    assert_eq!(recorded_resolutions(&result.events).len(), 2);
}

#[tokio::test]
async fn interrupt_drains_in_flight_calls_and_records_completed_work_before_terminal() {
    let result = dispatch(
        &[
            ("first", PARALLEL, MUTATION),
            ("second", PARALLEL, MUTATION),
            ("last", PARALLEL, MUTATION),
        ],
        vec![
            call("c1", "first"),
            call("c2", "second"),
            call("c3", "last"),
        ],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "second", 1, Approval::Approved));
            client.send(answer(&request, "last", 2, Approval::Approved));
            let mut starts = vec![
                client.starts.recv().await.unwrap(),
                client.starts.recv().await.unwrap(),
            ];
            starts.sort_unstable();
            assert_eq!(starts, [1, 2]);
            client.send(Command::Interrupt {
                command_id: "stop".into(),
            });
            client.releases[2].notify_one();
            assert_eq!(client.ends.recv().await, Some(2));
            assert!(
                !client
                    .sink
                    .events()
                    .iter()
                    .any(|event| matches!(event.kind, EventKind::RunFinished { .. }))
            );
            client.releases[1].notify_one();
        },
    )
    .await;
    assert!(matches!(result.result, Err(AgentError::Interrupted)));
    assert_eq!(result.completed, [2, 1]);
    assert_eq!(
        result
            .messages
            .iter()
            .map(|message| message.tool_call_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("c2"), Some("c3")]
    );
    assert!(matches!(
        result.events.last().unwrap().kind,
        EventKind::RunFinished { .. }
    ));
    let replay = crate::replay_trace(&result.events);
    assert!(replay.dangling_tool_calls.is_empty());
    assert_eq!(replay.messages, result.messages);
    assert_eq!(
        replay.finished.unwrap().error.unwrap().kind,
        cadmus_contract::error_kinds::INTERRUPTED
    );
}

#[tokio::test]
async fn parked_receiver_interrupt_beats_serial_admission_on_tool_completion() {
    let result = dispatch(
        &[
            ("first", Concurrency::Serial, MUTATION),
            ("second", Concurrency::Serial, MUTATION),
            ("unanswered", Concurrency::Serial, MUTATION),
        ],
        vec![
            call("c1", "first"),
            call("c2", "second"),
            call("c3", "unanswered"),
        ],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(answer(&request, "first", 0, Approval::Approved));
            client.send(answer(&request, "second", 1, Approval::Approved));
            assert_eq!(client.starts.recv().await, Some(0));
            client.resolution().await;
            client.resolution().await;
            while client.waits.try_recv().is_ok() {}
            // A fresh wait after this harmless command proves the retained
            // receiver is parked with C unanswered and A still in flight.
            client.send(answer(&request, "park", usize::MAX, Approval::Approved));
            client.waits.recv().await.expect("parked receiver");
            client.send(Command::Interrupt {
                command_id: "stop".into(),
            });
            // Both the command and A's completion are ready at the next poll.
            // Release every probe so an erroneous B start fails, not hangs.
            for release in &client.releases {
                release.notify_one();
            }
        },
    )
    .await;
    assert!(matches!(result.result, Err(AgentError::Interrupted)));
    assert_eq!(
        result.completed,
        [0],
        "the queued interrupt must prevent B from starting"
    );
    assert_eq!(result.messages.len(), 1);
    assert!(!result.events.iter().any(|event| matches!(
        &event.kind, EventKind::ToolCall { call } if call.id == "c2"
    )));
}

#[tokio::test]
async fn interrupt_stops_unstarted_serial_calls_even_after_all_approvals() {
    let result = dispatch(
        &[
            ("first", Concurrency::Serial, MUTATION),
            ("second", Concurrency::Serial, MUTATION),
        ],
        vec![call("c1", "first"), call("c2", "second")],
        |mut client| async move {
            let (request, _) = client.request().await;
            client.send(Command::ResolveApproval {
                command_id: "all".into(),
                request_id: request,
                decisions: vec![Approval::Approved, Approval::Approved],
            });
            assert_eq!(client.starts.recv().await, Some(0));
            client.send(Command::Interrupt {
                command_id: "stop".into(),
            });
            client.releases[0].notify_one();
        },
    )
    .await;
    assert!(matches!(result.result, Err(AgentError::Interrupted)));
    assert_eq!(result.completed, [0]);
    assert_eq!(result.messages.len(), 1);
}
