//! Approval vocabulary (ADR-0008 item 4, amended 2026-09-09): the decisions
//! exchanged over the approval seam. Who is asked and how the dialog is
//! drawn is the client's; these types are the normative payloads
//! (ADR-0013 item 10).

use serde::{Deserialize, Serialize};

/// One decision on a gated tool call.
///
/// Batch approval: a turn's gated calls are presented together and each is
/// decided independently; approved calls execute immediately. A decision
/// may depend on the batch's contents but never on another gated call's
/// *result* — results postdate the approval moment (the 2026-09-09
/// amendment's accepted trade-off).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Approval {
    /// The call executes.
    Approved,
    /// The call never executes. The loop returns the rejection to the model
    /// as an `is_error` tool result, so the judgment enters the trajectory
    /// and reflection can learn from it (ADR-0005); `comment` carries the
    /// reviewer's optional reason (ADR-0011 item 3) — user-supplied, or a
    /// policy's static explanation.
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_serializes_as_a_tagged_decision() {
        let approved = serde_json::to_value(Approval::Approved).expect("serialize");
        assert_eq!(approved, serde_json::json!({"decision": "approved"}));

        let rejected =
            serde_json::to_value(Approval::Rejected { comment: None }).expect("serialize");
        assert_eq!(rejected, serde_json::json!({"decision": "rejected"}));

        let with_comment = serde_json::to_value(Approval::Rejected {
            comment: Some("no".into()),
        })
        .expect("serialize");
        assert_eq!(
            with_comment,
            serde_json::json!({"decision": "rejected", "comment": "no"})
        );
        // The comment-less form round-trips (additive tolerance).
        let back: Approval = serde_json::from_value(rejected).expect("deserialize");
        assert_eq!(back, Approval::Rejected { comment: None });
    }
}
