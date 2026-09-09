//! The client-side approval policies (ADR-0008 item 4 / ADR-0013 item 6):
//! core's gate asks, each client answers. Eval auto-approves — every case
//! runs against a disposable scratch copy of its fixture. Headless chat
//! denies mutations unless the operator passed `--yes`: an unanswered
//! approval denies, and the denial text is model feedback that names the
//! way out instead of reading as a tool failure.

use async_trait::async_trait;
use cadmus_contract::{Approval, ToolCall};
use cadmus_core::Approver;

/// eval's policy: mutations are safe to approve unconditionally inside the
/// per-case scratch workspaces (a disposable copy per case).
pub struct ApproveAll;

#[async_trait]
impl Approver for ApproveAll {
    async fn approve(&self, calls: &[ToolCall]) -> Vec<Approval> {
        calls.iter().map(|_| Approval::Approved).collect()
    }
}

/// Headless chat's policy: no user is attending, so gated (mutation) calls
/// are denied unless `--yes` was passed (ADR-0011 item 3: unattended runs
/// default to deny).
pub struct Headless {
    pub yes: bool,
}

#[async_trait]
impl Approver for Headless {
    async fn approve(&self, calls: &[ToolCall]) -> Vec<Approval> {
        calls
            .iter()
            .map(|_| {
                if self.yes {
                    Approval::Approved
                } else {
                    Approval::Rejected {
                        comment: Some(
                            "unattended run: workspace mutations need `--yes` or an interactive \
                             client"
                                .into(),
                        ),
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn calls() -> Vec<ToolCall> {
        vec![
            ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "edit_file".into(),
                arguments: json!({}),
            },
        ]
    }

    #[tokio::test]
    async fn approve_all_approves_every_call() {
        let decisions = ApproveAll.approve(&calls()).await;
        assert_eq!(decisions, vec![Approval::Approved, Approval::Approved]);
    }

    #[tokio::test]
    async fn headless_denies_without_yes_and_approves_with_it() {
        // The deny default is the security posture of unattended runs: one
        // decision per call, each a rejection that names the way out.
        let denied = Headless { yes: false }.approve(&calls()).await;
        assert_eq!(denied.len(), 2);
        for decision in denied {
            match decision {
                Approval::Rejected { comment } => {
                    assert!(comment.expect("a reason").contains("--yes"));
                }
                Approval::Approved => panic!("unattended run approved a mutation"),
            }
        }

        let approved = Headless { yes: true }.approve(&calls()).await;
        assert_eq!(approved, vec![Approval::Approved, Approval::Approved]);
    }
}
