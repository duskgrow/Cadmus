//! Trajectory store adapter (ADR-0005): append-only JSONL event logs, one
//! file per trace, date-sharded under a configurable root. Phase 2 adds the
//! SQL derived index here; the log stays the SSOT and the index stays
//! rebuildable from it.

mod jsonl;

pub use jsonl::{JsonlLog, ReadError, mint_trace_id};
