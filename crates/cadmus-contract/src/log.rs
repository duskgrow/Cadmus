//! The trajectory log-writer port (ADR-0005) and its error type.
//!
//! Sync by design: appends happen at turn boundaries (a few KB after
//! multi-second model/tool work), so a blocking append keeps the agent loop
//! simple; durability (flush policy) is the adapter's concern. Reading a log
//! back is the adapter's plain API, not a port — nothing in the core
//! consumes logs through a seam yet.

use crate::Event;

/// Append-only trajectory sink. One call appends one event of one trace;
/// implementations map `trace_id` to storage (the JSONL adapter date-shards
/// one file per trace) and never reorder or rewrite.
pub trait EventSink: Send + Sync {
    fn append(&self, event: &Event) -> Result<(), LogError>;
}

/// The spill-artifact port (ADR-0007 items 2/3): a folded tool result's
/// full text spills to a per-trace artifact file, and the fold directive
/// references the returned reference. Implementations must return a
/// log-stable reference (shard-relative, so artifacts stay relocatable
/// across tiering roots) — never a bare absolute path. A spill failure is
/// fatal to the run: a silently unspilled fold would orphan the
/// placeholder's re-obtaining pointer (the never-silent rule).
pub trait ArtifactSink: Send + Sync {
    fn spill(&self, name: &str, content: &str) -> Result<String, LogError>;
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("event serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("event log write failed: {0}")]
    Io(#[from] std::io::Error),
    /// The JSONL adapter computes a trace's shard path from its id; an id
    /// without a parseable date is rejected instead of silently misplaced.
    #[error("trace id `{0}` carries no shard date (expected `tr-YYYYMMDD-…`)")]
    InvalidTraceId(String),
}
