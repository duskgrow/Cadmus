//! The approval gate seam (ADR-0008 item 4): the loop asks, a client
//! answers. What is gated is core's policy (ADR-0013 item 7): mutation
//! calls ([`Effect::Mutation`](crate::Effect)) are gated, perception is
//! free. Who answers is the client's — the TUI asks the user, unattended
//! clients deny (ADR-0011 item 3), eval auto-approves inside its disposable
//! workspaces.

use async_trait::async_trait;
use cadmus_contract::{Approval, ToolCall};

/// The client-injected answerer for the approval gate. One turn's gated
/// calls arrive as a single batch, presented before any of them executes —
/// a decision may depend on the batch's contents but never on another gated
/// call's result (ADR-0008 item 4, 2026-09-09 amendment).
///
/// An implementation that waits on a human must pair the wait with a deny
/// timeout (ADR-0008 item 4: an unanswered request times out to deny). The
/// in-process policies answer immediately, so the timeout mechanism lands
/// with the first waiting client (the TUI) — together with the
/// `resolve_approval` command event that lets remote clients share this
/// path (ADR-0008 item 4 / ADR-0013 item 6; see docs/open-items.md).
#[async_trait]
pub trait Approver: Send + Sync {
    /// Decides each call in the batch — one decision per call, in call
    /// order. A short reply denies the remainder (the conservative
    /// default); extra decisions are ignored.
    async fn approve(&self, calls: &[ToolCall]) -> Vec<Approval>;
}
