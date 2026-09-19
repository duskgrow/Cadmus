//! The folded state of one trace (ADR-0005 §4). `cadmus-core`'s replayer
//! produces it from the event log; ADR-0013's attach handshake carries it as
//! the `Sync` payload's `history` — which is why the type lives here with
//! the other wire types instead of in the core.

use serde::{Deserialize, Serialize};

use crate::{EventError, Message, ScoreEvent, Status};

/// The folded state of one trace: what replaying its event log reconstructs,
/// and what an attaching client adopts as its render baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub trace_id: String,
    /// `selfevol.provider` / `selfevol.model` from the start-run command,
    /// when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reconstructed history: the start-run seed plus every assistant,
    /// tool and steered user message, in log order.
    pub messages: Vec<Message>,
    /// Durable tool results with authoritative approval addresses. Legacy
    /// results without those event attributes have no entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResultProjection>,
    /// Completed assistant turns (ok `llm_response` events).
    pub turns: u32,
    /// Every turn warning, in log order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<ScoreEvent>,
    /// Tool calls never closed by a result — the crash window's dangling
    /// spans, in the order they opened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dangling_tool_calls: Vec<String>,
    /// The terminal record; `None` means the trace ended mid-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<FinishRecord>,
}

/// A durable result's approval address, kept outside provider-facing messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultProjection {
    /// Index of the result in `RunState::messages`, including seeded history.
    pub message_index: usize,
    pub request_id: String,
    /// Zero-based position in the request's gated batch.
    pub call_index: usize,
}

/// How the run ended, from its `run_finished` event's envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishRecord {
    pub turns: u32,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EventError>,
}
