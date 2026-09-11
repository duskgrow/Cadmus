//! The tool contract (ADR-0008): what a tool declares about itself (spec,
//! concurrency, effect) and how a call fails.

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use serde_json::Value;

/// A tool the agent may call. rmcp servers are wrapped into this trait at the
/// wiring layer; tests use hand-rolled fakes.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Concurrency declaration (ADR-0008 item 2): whether `invoke` may run
    /// concurrently with other calls in the same turn. The default is
    /// fail-safe — undeclared tools serialize. That includes third-party
    /// MCP wrappers, whose semantics we do not control.
    fn concurrency(&self) -> Concurrency {
        Concurrency::Serial
    }

    /// Effect declaration (ADR-0008 item 4): whether a call can mutate the
    /// workspace. Mutation calls pass the client policy's approval gate —
    /// presented as one batch per turn; perception calls are never gated.
    /// The default is fail-safe: an undeclared read is merely gated, an
    /// undeclared mutation would execute ungated.
    fn effect(&self) -> Effect {
        Effect::Mutation
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError>;
}

/// Whether a tool's `invoke` may overlap with other calls in the same turn
/// (ADR-0008 item 2: declared concurrency safety, defaulting to non-parallel
/// (fail-safe)). Built-in tools are designed for parallel safety on purpose
/// — the declaration records the analysis; the serial default protects what
/// we cannot vouch for (third-party wrappers, unanalyzed additions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concurrency {
    Serial,
    ParallelSafe,
}

/// The gate-relevant effect of a call (ADR-0008 item 4). Mutation calls are
/// presented to the client policy before executing; perception is never
/// gated. The fail-safe default is `Mutation` — same protective direction
/// as [`Concurrency`]'s serial default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Perception,
    Mutation,
}

#[derive(Debug, thiserror::Error)]
#[error("tool `{tool}` failed: {message}")]
pub struct ToolError {
    pub tool: String,
    pub message: String,
}
