//! Test doubles for the telemetry and client-protocol ports — the
//! determinism seam for loop and trajectory tests. These are fakes
//! (behavior, not mocks): the recording sinks keep everything so tests
//! assert on the trajectory and the live stream themselves.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cadmus_contract::{
    Approval, ArtifactSink, Clock, Command, CommandSource, Event, EventSink, IdSequence, LiveItem,
    LiveKind, LiveSink, LogError, ToolCall,
};

use crate::context::{FoldPolicy, FrozenPrefix, GitStatus, NoInstructions, StatusProbe};
use crate::{ClientProtocol, ContextBundle, Telemetry};

/// An in-memory [`EventSink`] keeping every appended event, in order.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    /// A snapshot of everything appended so far.
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("recording sink poisoned").clone()
    }
}

impl EventSink for RecordingSink {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.events
            .lock()
            .expect("recording sink poisoned")
            .push(event.clone());
        Ok(())
    }
}

/// An in-memory [`LiveSink`] keeping every published item, in order.
#[derive(Default)]
pub struct RecordingLive {
    items: Mutex<Vec<LiveItem>>,
}

impl RecordingLive {
    /// A snapshot of everything published so far.
    pub fn items(&self) -> Vec<LiveItem> {
        self.items.lock().expect("recording live poisoned").clone()
    }
}

impl LiveSink for RecordingLive {
    fn publish(&self, item: &LiveItem) {
        self.items
            .lock()
            .expect("recording live poisoned")
            .push(item.clone());
    }
}

/// An in-memory [`CommandSource`] over a std channel: scripts pre-seed it,
/// or a fake tool pushes mid-run through the sender handle (a tool firing
/// during dispatch is how tests plant a command at a precise loop point).
///
/// `recv` blocks the calling thread on an empty queue: scripts must queue
/// every resolve before the loop's gate can await it — a missing script
/// hangs the test, which is honest: a real run would wait for its client
/// too.
pub struct ChannelCommands {
    rx: Mutex<std::sync::mpsc::Receiver<Command>>,
}

impl ChannelCommands {
    /// The source plus its send half (cloneable, for injection fakes).
    #[must_use]
    pub fn new() -> (Self, std::sync::mpsc::Sender<Command>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { rx: Mutex::new(rx) }, tx)
    }

    /// A source pre-loaded with commands, in order.
    #[must_use]
    pub fn scripted(commands: impl IntoIterator<Item = Command>) -> Self {
        let (source, tx) = Self::new();
        for command in commands {
            tx.send(command).expect("scripted channel open");
        }
        source
    }
}

#[async_trait::async_trait]
impl CommandSource for ChannelCommands {
    async fn recv(&self) -> Option<Command> {
        self.rx.lock().expect("commands poisoned").recv().ok()
    }

    fn poll(&self) -> Option<Command> {
        self.rx.lock().expect("commands poisoned").try_recv().ok()
    }
}

/// The test-double approval client (ADR-0013 item 6): reacts to approval
/// requests on the live stream by sending the resolve command through the
/// command channel — the one path every client shares. Wraps an inner sink
/// so published items still reach it. Deliberately duplicated in spirit by
/// the binary's wiring (`cadmus::approval`); the test double must not leak
/// into the binary through this module.
pub struct AutoResolver<P> {
    inner: Arc<dyn LiveSink>,
    commands: std::sync::mpsc::Sender<Command>,
    policy: P,
    command_seq: AtomicU64,
}

impl<P> AutoResolver<P> {
    #[must_use]
    pub fn new(
        inner: Arc<dyn LiveSink>,
        commands: std::sync::mpsc::Sender<Command>,
        policy: P,
    ) -> Self {
        Self {
            inner,
            commands,
            policy,
            command_seq: AtomicU64::new(0),
        }
    }
}

impl<P> LiveSink for AutoResolver<P>
where
    P: Fn(&[ToolCall]) -> Vec<Approval> + Send + Sync,
{
    fn publish(&self, item: &LiveItem) {
        if let LiveKind::ApprovalRequested {
            request_id, calls, ..
        } = &item.kind
        {
            let command = Command::ResolveApproval {
                command_id: format!(
                    "cmd-test-{}",
                    self.command_seq.fetch_add(1, Ordering::Relaxed)
                ),
                request_id: request_id.clone(),
                decisions: (self.policy)(calls),
            };
            // A closed channel means the run is ending; the gate denies on
            // a closed channel, so dropping the send is safe.
            let _ = self.commands.send(command);
        }
        self.inner.publish(item);
    }
}

/// A client protocol over test doubles: approval requests resolve through
/// the command channel per `policy`, and every published item lands in the
/// returned recording sink. The sender half comes back for tests that
/// inject commands mid-run.
#[must_use]
pub fn protocol_with<P>(
    policy: P,
) -> (
    ClientProtocol,
    Arc<RecordingLive>,
    std::sync::mpsc::Sender<Command>,
)
where
    P: Fn(&[ToolCall]) -> Vec<Approval> + Send + Sync + 'static,
{
    let live = Arc::new(RecordingLive::default());
    let (commands, sender) = ChannelCommands::new();
    let resolver = AutoResolver::new(live.clone(), sender.clone(), policy);
    (
        ClientProtocol {
            live: Arc::new(resolver),
            commands: Arc::new(commands),
        },
        live,
        sender,
    )
}

/// The common case: every gated call approved, nothing else scripted.
#[must_use]
pub fn auto_approving() -> (ClientProtocol, Arc<RecordingLive>) {
    let (protocol, live, _sender) =
        protocol_with(|calls| calls.iter().map(|_| Approval::Approved).collect());
    (protocol, live)
}

/// A stopped clock: every timestamp is the same fixed instant.
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
}

/// Sequential ids: 1, 2, 3, …
///
/// Deliberately duplicated from `cadmus::telemetry::SeqIds` (the wiring
/// layer's real impl); keep the two in sync (five lines each).
#[derive(Default)]
pub struct SeqIds(AtomicU64);

impl IdSequence for SeqIds {
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// An in-memory [`ArtifactSink`] keeping every spill by name — the fold
/// tests assert on the spilled content itself.
#[derive(Default)]
pub struct RecordingArtifacts {
    spills: Mutex<BTreeMap<String, String>>,
}

impl RecordingArtifacts {
    /// A snapshot of everything spilled so far (name → content).
    pub fn spills(&self) -> BTreeMap<String, String> {
        self.spills.lock().expect("artifacts poisoned").clone()
    }
}

impl ArtifactSink for RecordingArtifacts {
    fn spill(&self, name: &str, content: &str) -> Result<String, LogError> {
        self.spills
            .lock()
            .expect("artifacts poisoned")
            .insert(name.to_string(), content.to_string());
        Ok(format!("test-artifacts/{name}"))
    }
}

/// A [`StatusProbe`] with a scripted answer (ADR-0002's injected-IO rule).
pub struct FixedProbe(pub Option<GitStatus>);

impl StatusProbe for FixedProbe {
    fn snapshot(&self) -> Option<GitStatus> {
        self.0.clone()
    }
}

/// A minimal context bundle for loop tests: a one-word prompt, no
/// instruction files, `/test` cwd, no git, no nested tracking, recording
/// artifacts and the default fold policy. Tests that exercise the pipeline
/// assemble their own.
#[must_use]
pub fn test_context() -> ContextBundle {
    ContextBundle {
        prefix: FrozenPrefix::assemble("test prompt", &[], &[]),
        probe: Arc::new(FixedProbe(None)),
        tracker: Arc::new(NoInstructions),
        cwd: "/test".into(),
        artifacts: Arc::new(RecordingArtifacts::default()),
        fold_policy: FoldPolicy::default(),
    }
}

/// A telemetry bundle over a recording sink; the sink handle comes back for
/// assertions on the emitted trajectory.
#[must_use]
pub fn test_telemetry(trace_id: &str) -> (Telemetry, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let telemetry = Telemetry {
        sink: sink.clone(),
        clock: Arc::new(FixedClock(1_788_393_600_000)),
        ids: Arc::new(SeqIds::default()),
        trace_id: trace_id.into(),
        run_attributes: BTreeMap::new(),
    };
    (telemetry, sink)
}
