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
