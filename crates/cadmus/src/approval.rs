//! The client-side approval policies (ADR-0008 item 4 / ADR-0013 item 6):
//! the gate publishes a request on the live stream, and every client answers
//! with a `resolve_approval` command through the command channel — the one
//! path local and future remote clients share. The policies here are that
//! path's auto-answerers: eval approves (every case runs against a
//! disposable scratch copy of its fixture); headless chat denies mutations
//! unless the operator passed `--yes`, and the denial text is model feedback
//! that names the way out instead of reading as a tool failure.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cadmus_contract::{Approval, Command, LiveItem, LiveKind, LiveSink, ToolCall};
use cadmus_transport::CommandSender;

/// eval's policy: approve unconditionally (a disposable scratch workspace
/// per case).
pub fn approve_all(calls: &[ToolCall]) -> Vec<Approval> {
    calls.iter().map(|_| Approval::Approved).collect()
}

/// Headless chat's policy (ADR-0011 item 3: unattended runs default to
/// deny).
pub fn unattended(yes: bool) -> impl Fn(&[ToolCall]) -> Vec<Approval> {
    move |calls| {
        calls
            .iter()
            .map(|_| {
                if yes {
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

/// The auto-answering client (ADR-0013 item 6): reacts to approval requests
/// on the live stream by sending the resolve command through the command
/// channel — the same path the TUI's user will take. Wraps the run's real
/// sink, so every item still reaches it. Mirrors
/// `cadmus_core::testing::AutoResolver`; the test double deliberately stays
/// out of the binary (same rule as the duplicated `SeqIds`).
pub struct AutoResolver<P> {
    inner: Arc<dyn LiveSink>,
    commands: CommandSender,
    policy: P,
    command_seq: AtomicU64,
}

impl<P> AutoResolver<P> {
    pub fn new(inner: Arc<dyn LiveSink>, commands: CommandSender, policy: P) -> Self {
        Self {
            inner,
            commands,
            policy,
            command_seq: AtomicU64::new(0),
        }
    }
}

impl<P> LiveSink for AutoResolver<P>
where
    P: Fn(&[ToolCall]) -> Vec<Approval> + Send + Sync,
{
    fn publish(&self, item: &LiveItem) {
        if let LiveKind::ApprovalRequested {
            request_id, calls, ..
        } = &item.kind
        {
            let command = Command::ResolveApproval {
                command_id: format!(
                    "cmd-auto-{}",
                    self.command_seq.fetch_add(1, Ordering::Relaxed)
                ),
                request_id: request_id.clone(),
                decisions: (self.policy)(calls),
            };
            // A closed channel means the run is gone; the gate treats a
            // closed channel as unanswered-deny, so dropping is safe.
            let _ = self.commands.send(command);
        }
        self.inner.publish(item);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use cadmus_transport::{Blackhole, command_channel};

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

    #[test]
    fn approve_all_approves_every_call() {
        assert_eq!(
            approve_all(&calls()),
            vec![Approval::Approved, Approval::Approved]
        );
    }

    #[test]
    fn headless_denies_without_yes_and_approves_with_it() {
        // The deny default is the security posture of unattended runs: one
        // decision per call, each a rejection that names the way out.
        for decision in unattended(false)(&calls()) {
            match decision {
                Approval::Rejected { comment } => {
                    assert!(comment.expect("a reason").contains("--yes"));
                }
                Approval::Approved => panic!("unattended run approved a mutation"),
            }
        }
        assert_eq!(
            unattended(true)(&calls()),
            vec![Approval::Approved, Approval::Approved]
        );
    }

    /// The auto-resolver answers through the command channel: the request's
    /// id round-trips, and the policy's decisions ride verbatim.
    #[tokio::test]
    async fn auto_resolver_answers_over_the_command_channel() {
        use cadmus_contract::CommandSource;

        let (sender, receiver) = command_channel();
        let resolver = AutoResolver::new(Arc::new(Blackhole), sender, approve_all);
        resolver.publish(&LiveItem {
            seq: 1,
            trace_id: "tr-test".into(),
            kind: LiveKind::ApprovalRequested {
                request_id: "ap7".into(),
                turn: 1,
                calls: calls(),
            },
        });

        let command = receiver.recv().await.expect("a command");
        let Command::ResolveApproval {
            command_id,
            request_id,
            decisions,
        } = command
        else {
            panic!("expected a resolve command");
        };
        assert_eq!(request_id, "ap7");
        assert_eq!(command_id, "cmd-auto-0");
        assert_eq!(decisions, vec![Approval::Approved, Approval::Approved]);
    }
}
