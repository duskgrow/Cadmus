//! Trajectory event types (ADR-0005): the per-trace append-only JSONL event
//! log is the trajectory SSOT. One self-describing event per line, full
//! request/response text inline at message granularity (the start-run base
//! plus response/result events; per-turn request snapshots are deliberately
//! not duplicated), references by id — never by file path.
//!
//! Schema evolution is additive-only (self-describing events plus rebuildable
//! projections downgrade the report's §11.1 irreversibility risk): new
//! optional fields and new [`EventKind`]/[`Command`] variants may be added,
//! existing fields never change meaning. The tolerance contract is precise:
//! unknown *fields* are ignored by serde default, but an unknown `kind` or
//! `command` value fails loudly — a replay/projection engine must never
//! silently drop an event kind it does not understand. A torn trailing line
//! after a crash is dropped by the log reader (`cadmus-memory`), not parsed
//! here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ChatRequest, FinishReason, Message, ToolCall, Usage};

/// One event in a trajectory log: a fixed envelope with the per-kind payload
/// flattened under the `kind` tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Unique within the trace (`e7`); minted from the injected
    /// [`IdSequence`].
    pub id: String,
    pub trace_id: String,
    /// The span this event belongs to (`s3`). An event is a point in time;
    /// a span is an interval — never itself a line in the log — delimited by
    /// the events sharing its id: request opens it, response closes it;
    /// tool call/result likewise. Duration is the pair's timestamp delta.
    pub span_id: String,
    /// Spans form a tree: turn and tool spans hang off the run's root span
    /// (which `start_run` opens and `run_finished` closes). Phase 1's tree
    /// is two levels deep; the link exists so deeper trees (cascade retries,
    /// control-plane commands) need no format change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// Milliseconds since the Unix epoch, from the injected [`Clock`] — the
    /// core never reads the system clock itself.
    pub time_unix_ms: u64,
    /// Flattened into the same JSON object as the envelope — payload field
    /// names must therefore never collide with envelope field names (the
    /// disjoint-key invariant; a collision would silently misparse).
    #[serde(flatten)]
    pub kind: EventKind,
    #[serde(default)]
    pub status: Status,
    /// Structured failure detail; present exactly when `status` is
    /// [`Status::Error`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EventError>,
    /// Free attribute bag. Long-lived keys live in the `selfevol.*`
    /// namespace (see [`attrs`]); `OTel` `gen_ai.*` is a vocabulary reference
    /// only — the upstream namespace is still all-Development.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, Value>,
}

impl Event {
    #[must_use]
    pub fn new(
        id: String,
        trace_id: String,
        span_id: String,
        parent_span_id: Option<String>,
        time_unix_ms: u64,
        kind: EventKind,
    ) -> Self {
        Self {
            id,
            trace_id,
            span_id,
            parent_span_id,
            time_unix_ms,
            kind,
            status: Status::Ok,
            error: None,
            attributes: BTreeMap::new(),
        }
    }

    /// Marks the event failed and attaches the structured detail.
    #[must_use]
    pub fn errored(mut self, error: EventError) -> Self {
        self.status = Status::Error;
        self.error = Some(error);
        self
    }

    #[must_use]
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// The per-kind payload; variant names are the wire `kind` values
/// (`llm_request`, …). Adding a variant is the standard evolution step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// Span-open marker for one provider call (its close is the matching
    /// `llm_response` on the same span): the pair gives duration, and an
    /// unclosed span is the "call never returned" crash signal. Deliberately
    /// no request snapshot — the fold reconstructs the exact per-turn
    /// history from `start_run` + message-level events, so duplicating it per
    /// turn would grow the log quadratically in turns. Per-call parameters
    /// arrive as attributes when cascade routing (phase 3) makes them vary
    /// within a run.
    LlmRequest,
    /// The assembled assistant turn. `status == error` with a partial
    /// `message` records a stream that died mid-flight.
    LlmResponse {
        message: Message,
        /// `None` = the provider never reported usage (pitfall #8: a missing
        /// terminal record means truncation, not a zero-usage success).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        /// May be the assembler's default (`stop`) when the stream died
        /// before a terminal record — `outcome` and the envelope `status`
        /// carry the truncation truth, not this field.
        finish: FinishReason,
        outcome: TurnOutcome,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    /// A tool invocation starts. Kept separate from the assistant message so
    /// a crash between call and result leaves an honest dangling span — with
    /// side-effecting tools that is the "effect unknown" signal.
    ToolCall { call: ToolCall },
    /// The tool's answer. `status == error` marks a tool failure the loop
    /// fed back to the model as text — the distinction survives here even
    /// where the message stream collapses it.
    ToolResult { call_id: String, result: Value },
    /// One (case, metric) score for this run (eval set v1, ADR-0005 §7).
    EvalScore(ScoreEvent),
    /// A client command (ADR-0002): validated, ordered and appended by the
    /// owning node; retries apply idempotently via the envelope id.
    Command(Command),
    /// Clean terminal record of the run. A trace without it ended in a crash
    /// window; the failure detail rides the envelope.
    RunFinished { turns: u32 },
}

/// One (case, metric) eval score recorded against a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreEvent {
    pub case_id: String,
    pub metric: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<bool>,
}

/// A validated client operation (ADR-0002's command seam): the only event
/// kind a client may ever produce — the control plane's trust boundary is
/// this type. Phase 1 knows only the run-opening command; approvals,
/// messages and steering arrive with the control plane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Opens a run: the base request the loop started from. Replaying a log
    /// re-seeds the message history from here, so a trace is self-sufficient.
    StartRun { base: Box<ChatRequest> },
}

/// Outcome classification of a completed event (span-level, OTel-shaped).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Ok,
    Error,
}

/// Stable, machine-readable failure detail (the `kind` string is the routing
/// signal, e.g. `rate_limited`, `turn_limit`; the message is for humans).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventError {
    pub kind: String,
    pub message: String,
}

/// How an assembled assistant turn ended. Lives in the contract because the
/// trajectory carries it; the core's assembler produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// Text and/or tool calls present.
    Content,
    /// The stream ended with open tool calls or without a terminal record —
    /// truncated, not a zero-usage success (pitfall #8).
    Truncated,
    /// No text, no reasoning, no tool calls. Legitimate result or error is
    /// decided by the finish reason (pitfall #5): a reasoning model that
    /// spends its whole budget on hidden thinking returns empty content with
    /// `Length`, which is retryable/escalatable, while `Stop` is a protocol
    /// anomaly.
    Empty,
}

/// The injected wall clock (ADR-0002's no-hidden-time rule): every event
/// timestamp comes from here, so tests run on a fixed timeline.
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

/// Trace-unique sequence numbers. The core formats kind-prefixed ids (`e7`
/// for events, `s3` for spans) from one sequence, so the sequence's only job
/// is monotonic uniqueness within a run.
pub trait IdSequence: Send + Sync {
    fn next(&self) -> u64;
}

/// Stable machine strings for [`EventError::kind`] — the failure vocabulary
/// that projections and (phase 3) cascade routing branch on.
pub mod error_kinds {
    // ModelError mappings:
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const SERVER: &str = "server";
    pub const NETWORK: &str = "network";
    pub const PROTOCOL: &str = "protocol";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const CAPABILITY_MISMATCH: &str = "capability_mismatch";
    pub const AUTH: &str = "auth";
    pub const CONTEXT_LENGTH: &str = "context_length";
    /// Assistant turn with no text, reasoning or tool calls.
    pub const EMPTY_TURN: &str = "empty_turn";
    /// Assistant turn limit exceeded.
    pub const TURN_LIMIT: &str = "turn_limit";
    /// A wired tool's invocation returned an error.
    pub const TOOL: &str = "tool";
    /// The model named a tool that is not wired in.
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
}

/// Well-known attribute-bag keys — the SSOT of the long-lived `selfevol.*`
/// vocabulary (`OTel` `gen_ai.*` naming as reference).
pub mod attrs {
    /// Provider id as wired (`kimi`, `deepseek`, `custom`, …) — recorded on
    /// the start-run command.
    pub const PROVIDER: &str = "selfevol.provider";
    /// Model id as sent on the wire.
    pub const MODEL: &str = "selfevol.model";
    /// Binary version that wrote the trace.
    pub const CADMUS_VERSION: &str = "selfevol.cadmus.version";
    /// 1-based assistant-turn index within the run — on llm request/response
    /// and tool call/result events.
    pub const TURN: &str = "selfevol.turn";
}
