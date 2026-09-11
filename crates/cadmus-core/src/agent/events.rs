//! Event plumbing: minting (one sequence for event ids and live-seq,
//! ADR-0013 item 4), append-then-publish ordering, and the structured error
//! details shared by the truncated-turn and terminal records.

use cadmus_contract::{
    Command, Event, EventError, EventKind, LiveItem, LiveKind, ModelError, attrs, error_kinds,
};

use super::{AgentError, AgentLoop};
use crate::AssembledTurn;

impl AgentLoop {
    pub(super) fn emit(&self, event: &Event) -> Result<(), AgentError> {
        // Append first, publish second: a log failure aborts the run before
        // any subscriber sees the event — clients never observe state the
        // trajectory does not have. The republished copy is what makes the
        // log's contents observable on the live stream (ADR-0013 item 2).
        self.telemetry.sink.append(event)?;
        self.protocol.live.publish(&LiveItem {
            seq: event.seq,
            trace_id: event.trace_id.clone(),
            kind: LiveKind::Recorded {
                event: Box::new(event.clone()),
            },
        });
        Ok(())
    }

    /// Publishes one ephemeral live item, stamped from the same sequence
    /// the durable events draw from (one total order across both channels,
    /// ADR-0013 item 4). Best-effort: a lagging subscriber is told to
    /// re-sync, never awaited.
    pub(super) fn publish(&self, kind: LiveKind) {
        self.protocol.live.publish(&LiveItem {
            seq: self.telemetry.ids.next(),
            trace_id: self.telemetry.trace_id.clone(),
            kind,
        });
    }

    /// Appends a client command to the trace — off the run root, stamped
    /// with the turn it takes effect in — and publishes it live through
    /// [`Self::emit`].
    pub(super) fn record_command(
        &self,
        command: Command,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let span = self.next_span();
        self.emit(&self.turn_event(&span, root_span, turn, EventKind::Command(command)))
    }

    pub(super) fn envelope(&self, span: &str, parent: Option<&str>, kind: EventKind) -> Event {
        // The id and the seq mint from one counter (`e7` ↔ seq 7), so the
        // log identity and the client-protocol position never disagree
        // (ADR-0013 item 4's total order, one sequence for both channels).
        let seq = self.telemetry.ids.next();
        Event::new(
            seq,
            format!("e{seq}"),
            self.telemetry.trace_id.clone(),
            span.to_string(),
            parent.map(str::to_string),
            self.telemetry.clock.now_unix_ms(),
            kind,
        )
    }

    /// One span per turn / tool execution, hanging off the run root, with
    /// the 1-based turn index as an attribute.
    pub(super) fn turn_event(&self, span: &str, root: &str, turn: usize, kind: EventKind) -> Event {
        self.envelope(span, Some(root), kind)
            .with_attribute(attrs::TURN, u64::try_from(turn).unwrap_or(u64::MAX))
    }

    pub(super) fn next_span(&self) -> String {
        format!("s{}", self.telemetry.ids.next())
    }

    /// The terminal record: `turns` counts *completed* assistant turns.
    pub(super) fn finish_with(
        &self,
        root: &str,
        completed_turns: usize,
        error: Option<EventError>,
    ) -> Result<(), AgentError> {
        let event = self.envelope(
            root,
            None,
            EventKind::RunFinished {
                turns: u32::try_from(completed_turns).unwrap_or(u32::MAX),
            },
        );
        let event = match error {
            Some(error) => event.errored(error),
            None => event,
        };
        self.emit(&event)
    }
}

/// The structured detail of an interrupted run, carried by the truncated
/// turn and the terminal record alike.
pub(super) fn interrupted_detail() -> EventError {
    EventError {
        kind: error_kinds::INTERRUPTED.into(),
        message: AgentError::Interrupted.to_string(),
    }
}

pub(super) fn response_kind(turn: &AssembledTurn) -> EventKind {
    EventKind::LlmResponse {
        message: turn.message.clone(),
        usage: turn.usage.clone(),
        finish: turn.finish.clone(),
        outcome: turn.outcome,
        warnings: turn.warnings.clone(),
    }
}

pub(super) fn model_event_error(error: &ModelError) -> EventError {
    let kind = match error {
        ModelError::RateLimited { .. } => error_kinds::RATE_LIMITED,
        ModelError::Server { .. } => error_kinds::SERVER,
        ModelError::Network(_) => error_kinds::NETWORK,
        ModelError::Protocol(_) => error_kinds::PROTOCOL,
        ModelError::InvalidRequest(_) => error_kinds::INVALID_REQUEST,
        ModelError::CapabilityMismatch(_) => error_kinds::CAPABILITY_MISMATCH,
        ModelError::Auth(_) => error_kinds::AUTH,
        ModelError::ContextLength => error_kinds::CONTEXT_LENGTH,
    };
    EventError {
        kind: kind.into(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use cadmus_contract::{ChatRequest, EventSink, LogError};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::Telemetry;
    use crate::agent::fixtures::{test_capabilities, text_script};
    use crate::testing::{FixedClock, RecordingSink, SeqIds, auto_approving, test_context};

    /// A sink that records until the `fail_at`-th append (0-based), which
    /// fails — the mid-run log-failure seam.
    struct FailAtSink {
        inner: RecordingSink,
        countdown: Mutex<usize>,
    }

    impl FailAtSink {
        fn new(fail_at: usize) -> Self {
            Self {
                inner: RecordingSink::default(),
                countdown: Mutex::new(fail_at),
            }
        }

        fn events(&self) -> Vec<Event> {
            self.inner.events()
        }
    }

    impl EventSink for FailAtSink {
        fn append(&self, event: &Event) -> Result<(), LogError> {
            let mut countdown = self.countdown.lock().expect("countdown poisoned");
            if *countdown == 0 {
                return Err(LogError::Io(std::io::Error::other("sink boom")));
            }
            *countdown -= 1;
            self.inner.append(event)
        }
    }

    /// Append-before-publish: with the trajectory log failing mid-run, the
    /// failed event never reaches the live stream — clients never observe
    /// state the trajectory does not have (ADR-0013 item 2).
    #[tokio::test]
    async fn a_failed_append_never_reaches_the_live_stream() {
        let provider = Arc::new(
            ReplayProvider::new([text_script("done")]).with_capabilities(test_capabilities()),
        );
        // start_run lands; the llm_request append fails.
        let sink = Arc::new(FailAtSink::new(1));
        let telemetry = Telemetry {
            sink: sink.clone(),
            clock: Arc::new(FixedClock(1_788_393_600_000)),
            ids: Arc::new(SeqIds::default()),
            trace_id: "tr-emit".into(),
            run_attributes: BTreeMap::new(),
        };
        let (protocol, live) = auto_approving();
        let agent = AgentLoop::new(provider, vec![], test_context(), protocol, 8, telemetry);

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("the log failure aborts the run");
        assert!(matches!(err, AgentError::Log(_)));

        let appended = sink.events();
        assert_eq!(appended.len(), 1, "only start_run landed in the log");
        let items = live.items();
        assert_eq!(
            items.len(),
            appended.len(),
            "the live stream observes exactly what the trajectory holds"
        );
        assert!(matches!(
            &items[0].kind,
            LiveKind::Recorded { event }
                if matches!(event.kind, EventKind::Command(Command::StartRun { .. }))
        ));
    }
}
