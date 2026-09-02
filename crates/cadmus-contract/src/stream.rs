use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Normalized increments; aggregation is the caller's job (cadmus-core).
/// Unknown vendor events are warn-and-skip at the adapter, and malformed
/// frames surface as in-stream `Err` items rather than killing the stream
/// (pitfall #9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamChunk {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart {
        index: u32,
        id: String,
        name: String,
    },
    ToolArgsDelta {
        index: u32,
        fragment: String,
    },
    /// Parallel tool-call fragments are routed by `index`; adapters normalize
    /// Anthropic content-block indexes and `OpenAI` tool-call indexes into the
    /// same space (pitfall #1).
    ToolCallEnd {
        index: u32,
    },
    /// Vendor-opaque deltas (thought signatures, …); folded into
    /// `Message::opaque`.
    OpaqueDelta(Value),
    Usage(Usage),
    Done {
        finish: FinishReason,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    /// Three separate buckets — without them billing cannot be reconciled:
    /// Anthropic reports 5m/1h write tiers separately, `OpenAI` charges for
    /// cache writes since GPT-5.6, `DeepSeek`'s disk prefix cache is ~1/10 price.
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
    /// Verbatim vendor usage object (field names differ everywhere,
    /// pitfall #12).
    #[serde(default)]
    pub raw: Value,
}

/// Metadata only — the core branches on actual content, not on the finish
/// reason, which compatible gateways routinely get wrong (pitfall #4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other(String),
}
