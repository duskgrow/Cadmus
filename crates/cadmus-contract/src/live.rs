//! The client protocol's ephemeral half (ADR-0013): the live-stream
//! vocabulary, the attach handshake and the two ports the loop talks
//! through. This module is the normative wire spec of item 10 — the ADR
//! pins the invariants; the field lists live here and nowhere else.
//!
//! Two downstream channels with different durability (item 2): the durable
//! JSONL log (message granularity, ADR-0005) and this ephemeral live stream.
//! Deltas are never appended to the log. Semantics in core, presentation in
//! clients (item 7): every payload here is semantic (paths, edit strings,
//! tool names), never a rendered string.
//!
//! The one client rule (item 4): positions are a per-run total order
//! ([`Event::seq`] and [`LiveItem::seq`] draw from the same sequence); an
//! attaching client's baseline is `Sync.history + Sync.in_flight`, and it
//! then drops every live item with `seq ≤ Sync.as_of_seq` and applies the
//! rest — idempotent and retry-safe over lossy transports.

use serde::{Deserialize, Serialize};

use crate::{Command, Event, RunState, StreamChunk, ToolCall, Usage};

/// One item on a run's live stream: a monotonic `seq` (the same sequence
/// durable events draw from) plus the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveItem {
    pub seq: u64,
    pub trace_id: String,
    /// Flattened under the `kind` tag — the same envelope pattern as
    /// [`Event`], so payload field names must never collide with `seq` /
    /// `trace_id`.
    #[serde(flatten)]
    pub kind: LiveKind,
}

/// The live payload vocabulary. Durable events republished right after their
/// successful append ride [`LiveKind::Recorded`], so one subscription gives
/// a client both channels; the ephemeral kinds below are never logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveKind {
    /// A durable event, republished live. Turn/span boundaries (request,
    /// response, tool call/result, command, terminal record) all arrive
    /// through this variant — the live view of the log, item 2's "the log
    /// writer is one subscriber" made observable to every client.
    Recorded { event: Box<Event> },
    /// One normalized provider-stream increment of the open assistant turn
    /// (text/reasoning deltas, tool-call starts and argument fragments). The
    /// chunk vocabulary is [`StreamChunk`]'s own — one aggregation dialect,
    /// no parallel one. Never logged: the completed turn lands as the
    /// `llm_response` event, which reconciles exactly with the buffered
    /// deltas (item 5).
    AssistantDelta { turn: u32, chunk: StreamChunk },
    /// A gated tool batch awaits resolution (ADR-0008 item 4): clients draw
    /// the approval dialog from this. The resolution arrives as a
    /// `Recorded` `resolve_approval` command — the request itself is never
    /// logged (the decision is the durable fact), so an attach during the
    /// wait reconstructs the dialog from [`InFlight::pending_approvals`].
    ApprovalRequested {
        request_id: String,
        turn: u32,
        calls: Vec<ToolCall>,
    },
}

/// What a subscription yields. `Lagged` (item 5) means the client fell
/// behind and the stream has a gap: drop everything, re-attach for a fresh
/// [`Sync`]. The subscription ends after it — no item following a gap is
/// usable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveUpdate {
    Item { item: Box<LiveItem> },
    Lagged,
}

/// The attach handshake's reply (item 3): the render baseline plus the
/// position it is current as of. `attach = replay(log) + sync(live) +
/// tail(deltas)` — session picker, dashboard, GUI, remote attach and
/// headless `--json` (the degenerate attach at position 0) share this one
/// path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sync {
    /// The log fold.
    pub history: RunState,
    /// The live aggregator's current state.
    pub in_flight: InFlight,
    /// The position `history + in_flight` is current as of: apply only live
    /// items with `seq` greater than this.
    pub as_of_seq: u64,
}

/// The live aggregator's state at attach time — the part of the run's state
/// that has not landed in the log yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InFlight {
    /// The open assistant turn and its partial fold, while a provider call
    /// is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_turn: Option<OpenTurn>,
    /// Gated batches awaiting a resolve command; an attach during an
    /// approval wait renders the dialog immediately from this (item 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_approvals: Vec<PendingApproval>,
}

/// One in-flight assistant turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTurn {
    /// The 1-based assistant-turn index (the `selfevol.turn` attribute's
    /// live counterpart).
    pub turn: u32,
    /// What the aggregator has folded so far.
    pub partial: TurnSnapshot,
}

/// The mid-stream fold of one assistant turn — the same aggregation
/// semantics as the completed `llm_response`, exposed while open (item 3:
/// Sync adds no new aggregation logic, it exposes the loop's own).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnSnapshot {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    /// Sealed calls plus still-open partials, ordered by stream index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<CallSnapshot>,
    /// The latest usage record the stream carried, when one arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// One tool call inside an in-flight turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CallSnapshot {
    /// Aggregation finished: arguments parsed.
    Sealed { index: u32, call: Box<ToolCall> },
    /// Still streaming: `arguments` is the raw fragment so far — possibly
    /// incomplete JSON, never parse it as final.
    Open {
        index: u32,
        id: String,
        name: String,
        arguments: String,
    },
}

/// One gated batch presented and not yet resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub turn: u32,
    pub calls: Vec<ToolCall>,
}

/// One attach's yield: the handshake plus the live tail. `tail` ends when
/// the run's publisher goes away; a [`LiveUpdate::Lagged`] item is always
/// the last meaningful one — re-attach for a fresh `Sync`.
pub struct Attachment {
    pub sync: Sync,
    pub tail: Box<dyn Iterator<Item = LiveUpdate> + Send>,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("sync", &self.sync)
            .finish_non_exhaustive()
    }
}

/// The ephemeral downstream port (ADR-0013 item 2): the loop publishes, the
/// in-process broadcaster and future transports implement, frontends
/// subscribe. Best-effort and synchronous — a slow subscriber lags (and is
/// told so), it never slows the loop.
///
/// (The [`Sync`] handshake type shadows the marker in this module; the
/// supertraits are fully qualified.)
pub trait LiveSink: Send + ::std::marker::Sync {
    fn publish(&self, item: &LiveItem);
}

/// The upstream port (ADR-0013 item 6): commands are the only upstream a
/// client has. The run-owning process — the server side, where the loop
/// lives; clients only produce — receives at defined points: turn
/// boundaries, per chunk mid-stream, and while awaiting an approval
/// resolve. Each command applies in receipt order. (In phase 1 the two
/// sides share one process; the split is the phase-5 transport's.)
#[async_trait::async_trait]
pub trait CommandSource: Send + ::std::marker::Sync {
    /// Awaits the next command; `None` means every client is gone. A gate
    /// that learns this mid-wait treats the request as unanswered — deny
    /// (ADR-0008 item 4's conservative default).
    async fn recv(&self) -> Option<Command>;
    /// Non-blocking drain for the mid-stream and boundary seams. The default
    /// suits sources with nothing to poll (scripts, closed channels).
    fn poll(&self) -> Option<Command> {
        None
    }
}
