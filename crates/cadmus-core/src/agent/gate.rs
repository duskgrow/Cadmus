//! The approval gate (ADR-0008 item 4 / ADR-0013 item 6): one batch request
//! per turn over the live stream, resolved by a `resolve_approval` command —
//! one path for local and future remote clients alike, with the resolution
//! itself entering the trajectory. Unanswered is deny.

use std::collections::HashMap;

use cadmus_contract::{Approval, Command, LiveKind, ToolCall};

use super::events::interrupted_detail;
use super::{AgentError, AgentLoop, Effect};

impl AgentLoop {
    /// Presents the batch's gated calls as one approval request on the live
    /// stream and awaits the matching `resolve_approval` command (ADR-0008
    /// item 4 / ADR-0013 item 6): one path for local and future remote
    /// clients alike, and the resolution itself enters the trajectory as a
    /// command event — the approval is recorded, not just its effects.
    ///
    /// Returns the rejections keyed by batch position. Position, not call
    /// id: ids are provider-supplied wire data with no uniqueness check,
    /// and a duplicated id must never let one call's rejection deny its
    /// same-id sibling. A short decision reply denies the remainder:
    /// unanswered is deny (ADR-0008 item 4); extra decisions are ignored;
    /// a resolve naming another request is dropped. A command channel that
    /// closes mid-wait denies the whole batch.
    ///
    /// An interrupt during the wait ends the run before any gated call
    /// executes: no call or result events land — the trajectory shows the
    /// model's intent (the recorded response), then the interrupt. Steers
    /// received during the wait classify into the inbox and apply at the
    /// next boundary.
    ///
    /// Timeout: a client that waits on a human pairs its wait with its own
    /// deny timeout (ADR-0008 item 4: human-in-the-loop always pairs a
    /// timeout with the conservative default). The in-process policies
    /// answer synchronously, so no core-side timeout mechanism exists yet
    /// — it lands with the first waiting client (the TUI).
    pub(super) async fn gate(
        &self,
        calls: &[ToolCall],
        root_span: &str,
        turn: usize,
    ) -> Result<HashMap<usize, Option<String>>, AgentError> {
        let mut positions = Vec::new();
        let mut batch = Vec::new();
        for (position, call) in calls.iter().enumerate() {
            if self.is_gated(call) {
                positions.push(position);
                batch.push(call.clone());
            }
        }
        let mut denied = HashMap::new();
        if batch.is_empty() {
            return Ok(denied);
        }

        let request_id = format!("ap{}", self.telemetry.ids.next());
        self.publish(LiveKind::ApprovalRequested {
            request_id: request_id.clone(),
            turn: u32::try_from(turn).unwrap_or(u32::MAX),
            calls: batch,
        });

        let command = loop {
            if let Some(command) = self.take_resolve(&request_id) {
                break command;
            }
            if let Some(command) = self.take_interrupt() {
                self.record_command(command, root_span, turn)?;
                self.finish_with(root_span, turn, Some(interrupted_detail()))?;
                return Err(AgentError::Interrupted);
            }
            match self.protocol.commands.recv().await {
                Some(command) => self.note(command),
                None => return Ok(deny_all(&positions)),
            }
        };
        let Command::ResolveApproval { decisions, .. } = command.clone() else {
            unreachable!("take_resolve returns only matching resolves");
        };
        self.record_command(command, root_span, turn)?;
        for (index, position) in positions.iter().enumerate() {
            match decisions.get(index) {
                Some(Approval::Approved) => {}
                Some(Approval::Rejected { comment }) => {
                    denied.insert(*position, comment.clone());
                }
                None => {
                    denied.insert(*position, None);
                }
            }
        }
        Ok(denied)
    }

    /// A call is gated when its tool declares a mutation effect (ADR-0008
    /// item 4). An unknown tool is never gated — it cannot mutate; dispatch
    /// turns it into `unknown_tool` feedback.
    fn is_gated(&self, call: &ToolCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.effect() == Effect::Mutation)
    }
}

/// Every gated position denied without a reason — the gate's conservative
/// default when the command channel closes mid-wait (unanswered is deny,
/// ADR-0008 item 4).
fn deny_all(positions: &[usize]) -> HashMap<usize, Option<String>> {
    positions.iter().map(|position| (*position, None)).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cadmus_contract::{
        ChatRequest, EventKind, FinishReason, LiveItem, Status, StreamChunk, ToolSpec, error_kinds,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{
        EchoTool, SendOnMatch, StepTool, YieldTool, call, kind_name, test_capabilities,
        text_script, tool_call_script,
    };
    use crate::agent::{AgentTool, ClientProtocol, ToolError};
    use crate::testing::test_telemetry;

    /// A gate harness whose client resolves every request through the
    /// command channel with `policy`'s fixed decisions — shorter or longer
    /// than the batch on purpose, to pin the conservative-mismatch
    /// behavior. The recording live sink comes back: the protocol's items
    /// are the ordering evidence (request before any execution).
    fn gate_harness(
        tools: Vec<Arc<dyn AgentTool>>,
        policy: impl Fn(&[ToolCall]) -> Vec<Approval> + Send + Sync + 'static,
    ) -> (
        AgentLoop,
        Arc<crate::testing::RecordingSink>,
        Arc<crate::testing::RecordingLive>,
    ) {
        let provider = ReplayProvider::new([]).with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, live, _sender) = crate::testing::protocol_with(policy);
        (
            AgentLoop::new(
                Arc::new(provider),
                tools,
                crate::testing::test_context(),
                protocol,
                8,
                telemetry,
            ),
            sink,
            live,
        )
    }
    /// The position of the first approval request and of the first recorded
    /// tool call in the live stream — the approve-before-execute evidence.
    fn request_and_call_positions(live: &crate::testing::RecordingLive) -> (usize, usize) {
        let items = live.items();
        let request = items
            .iter()
            .position(|item| matches!(item.kind, LiveKind::ApprovalRequested { .. }))
            .expect("an approval request was published");
        let call = items
            .iter()
            .position(|item| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event } if matches!(event.kind, EventKind::ToolCall { .. })
                )
            })
            .expect("a tool call was recorded");
        (request, call)
    }
    #[tokio::test]
    async fn a_rejected_mutation_never_executes_and_enters_the_trajectory() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let provider = ReplayProvider::new([
            ReplayProvider::script(vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: "step".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::ToolCalls,
                },
            ]),
            text_script("understood"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, live, _sender) = crate::testing::protocol_with(|_| {
            vec![Approval::Rejected {
                comment: Some("not today".into()),
            }]
        });
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(StepTool {
                name: "step",
                log: log.clone(),
            })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");

        // The tool never invoked.
        assert!(
            log.lock().await.is_empty(),
            "a rejected call must not execute"
        );
        // The request preceded any execution, and the resolution entered the
        // trajectory as a command event — the approval itself is recorded,
        // not just its effect (ADR-0008 item 4 / ADR-0013 item 6).
        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        let events = sink.events();
        let resolve = events.iter().find_map(|event| match &event.kind {
            EventKind::Command(Command::ResolveApproval {
                request_id,
                decisions,
                ..
            }) => Some((request_id.clone(), decisions.clone())),
            _ => None,
        });
        let Some((request_id, decisions)) = resolve else {
            panic!("the resolve command must be recorded");
        };
        assert!(
            live.items().iter().any(|item| matches!(
                &item.kind,
                LiveKind::ApprovalRequested { request_id: rid, .. } if *rid == request_id
            )),
            "the recorded resolve answers the published request"
        );
        assert_eq!(
            decisions,
            vec![Approval::Rejected {
                comment: Some("not today".into())
            }]
        );
        // The rejection is model feedback: an is_error tool result naming
        // the reason, never a silent skip.
        let message = &outcome.messages[2];
        assert!(message.is_error);
        assert!(
            matches!(&message.content[0], cadmus_contract::ContentPart::Text { text } if text.contains("rejected") && text.contains("not today")),
            "got: {:?}",
            message.content
        );
        // The trajectory carries the routing kind, so reflection can tell a
        // reviewer's no from a tool failure.
        let tool_result = events
            .iter()
            .find(|event| matches!(event.kind, EventKind::ToolResult { .. }))
            .expect("a tool_result event");
        assert_eq!(tool_result.status, Status::Error);
        assert_eq!(
            tool_result.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::APPROVAL_REJECTED)
        );
        // The fold mirrors the loop: replayed state matches live state, and
        // the rejected call's span stays paired (its tool_call event exists).
        assert!(
            events.iter().any(
                |event| matches!(&event.kind, EventKind::ToolCall { call } if call.id == "c1")
            )
        );
        let state = crate::replay_trace(&events);
        assert!(state.dangling_tool_calls.is_empty());
        assert_eq!(state.messages, outcome.messages);
    }

    #[tokio::test]
    async fn batch_approval_decides_each_call_independently_before_any_execution() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink, live) = gate_harness(
            vec![
                Arc::new(StepTool {
                    name: "w1",
                    log: log.clone(),
                }),
                Arc::new(StepTool {
                    name: "w2",
                    log: log.clone(),
                }),
            ],
            |_| {
                vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                ]
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "w1"), call("c2", "w2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // The batch was presented before either call executed.
        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        // Approved executed, rejected denied — and the results keep call
        // order.
        assert_eq!(
            log.lock().await.as_slice(),
            ["start w1", "end w1"],
            "the rejected call never runs"
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert!(!messages[0].is_error);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(messages[1].is_error);
    }

    #[tokio::test]
    async fn a_rejected_call_inside_a_parallel_batch_still_lands_in_call_order() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        // Both fakes are parallel-safe and (by the fail-safe default)
        // mutations, so the batch takes the parallel schedule with c1
        // pre-settled by the gate.
        let (agent, sink, live) = gate_harness(
            vec![
                Arc::new(YieldTool {
                    name: "m1",
                    log: log.clone(),
                }),
                Arc::new(YieldTool {
                    name: "m2",
                    log: log.clone(),
                }),
            ],
            |_| {
                vec![
                    Approval::Rejected {
                        comment: Some("no".into()),
                    },
                    Approval::Approved,
                ]
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "m1"), call("c2", "m2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        let (request, call) = request_and_call_positions(&live);
        assert!(request < call, "the gate opens before any execution");
        assert_eq!(log.lock().await.as_slice(), ["start m2", "end m2"]);
        assert_eq!(messages.len(), 2);
        assert!(messages[0].is_error);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert!(!messages[1].is_error);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        // The pre-settled rejection keeps the event pairing and the fold
        // identical to the live history.
        let events = sink.events();
        for id in ["c1", "c2"] {
            assert!(
                events.iter().any(
                    |event| matches!(&event.kind, EventKind::ToolCall { call } if call.id == id)
                )
            );
        }
        let state = crate::replay_trace(&events);
        assert!(state.dangling_tool_calls.is_empty());
        assert_eq!(state.messages, messages);
    }

    #[tokio::test]
    async fn a_short_decision_reply_denies_the_remainder() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink, _live) = gate_harness(
            vec![
                Arc::new(StepTool {
                    name: "w1",
                    log: log.clone(),
                }),
                Arc::new(StepTool {
                    name: "w2",
                    log: log.clone(),
                }),
            ],
            // One decision for two gated calls: the unanswered second call
            // must deny, not execute (ADR-0008 item 4).
            |_| vec![Approval::Approved],
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "w1"), call("c2", "w2")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        assert!(!messages[0].is_error);
        assert!(messages[1].is_error);
        assert!(
            matches!(&messages[1].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("no reason given")),
            "got: {:?}",
            messages[1].content
        );
    }

    /// A perception fake: `Effect::Perception` declared, so the gate never
    /// sees it (ADR-0008 item 4).
    struct PeekTool;

    #[async_trait]
    impl AgentTool for PeekTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "peek".into(),
                description: "peek".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn effect(&self) -> Effect {
            Effect::Perception
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("seen".into()))
        }
    }

    #[tokio::test]
    async fn perception_calls_never_reach_the_gate() {
        let (agent, _sink, live) = gate_harness(
            vec![Arc::new(PeekTool)],
            // Any answer would do — the gate must not ask at all.
            |_| panic!("the gate was shown a perception call"),
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "peek")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert!(
            !live
                .items()
                .iter()
                .any(|item| matches!(item.kind, LiveKind::ApprovalRequested { .. })),
            "a perception call must not open an approval request"
        );
        assert!(!messages[0].is_error);
    }

    #[tokio::test]
    async fn extra_decisions_are_ignored() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let (agent, _sink, _live) = gate_harness(
            vec![Arc::new(StepTool {
                name: "w1",
                log: log.clone(),
            })],
            // More decisions than gated calls: the stray rejection must not
            // leak into the batch.
            |_| {
                vec![
                    Approval::Approved,
                    Approval::Rejected {
                        comment: Some("stray".into()),
                    },
                ]
            },
        );
        let mut messages = Vec::new();

        agent
            .dispatch_tools(vec![call("c1", "w1")], &mut messages, "root", 1)
            .await
            .expect("dispatch");

        assert_eq!(
            log.lock().await.as_slice(),
            ["start w1", "end w1"],
            "the approved call runs"
        );
        assert!(!messages[0].is_error);
    }
    /// Interrupt during the approval wait: the gated calls never execute —
    /// no call or result events land — and the terminal record follows the
    /// interrupt command.
    #[tokio::test]
    async fn an_interrupt_during_the_approval_wait_ends_the_run() {
        let provider = ReplayProvider::new([ReplayProvider::script(vec![
            StreamChunk::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "step".into(),
            },
            StreamChunk::ToolCallEnd { index: 0 },
            StreamChunk::Done {
                finish: FinishReason::ToolCalls,
            },
        ])])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let live = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        // The interrupt arrives while the gate awaits its resolve: sent when
        // the approval request publishes.
        let live = std::sync::Arc::new(SendOnMatch::new(
            live,
            sender,
            |item: &LiveItem| matches!(item.kind, LiveKind::ApprovalRequested { .. }),
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        ));
        let protocol = ClientProtocol {
            live,
            commands: std::sync::Arc::new(commands),
        };
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(StepTool {
                name: "step",
                log: log.clone(),
            })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("interrupted at the gate");
        assert!(matches!(err, AgentError::Interrupted));
        assert!(log.lock().await.is_empty(), "the gated call never executed");
        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            [
                "start_run",
                "llm_request",
                "llm_response",
                "interrupt",
                "run_finished"
            ]
        );
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 1 }));
    }
    /// A command channel with no clients left (the sender is gone) mid-gate
    /// denies the batch — unanswered is deny (ADR-0008 item 4).
    #[tokio::test]
    async fn a_closed_command_channel_denies_the_gate() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"x\"}"),
            text_script("skipped"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (commands, sender) = crate::testing::ChannelCommands::new();
        drop(sender);
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(crate::testing::RecordingLive::default()),
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(EchoTool)],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the run adapts to the denial");
        assert!(outcome.messages[2].is_error);
        assert!(matches!(
            &outcome.messages[2].content[0],
            cadmus_contract::ContentPart::Text { text } if text.contains("no reason given")
        ));
        let result = sink
            .events()
            .into_iter()
            .find(|event| matches!(event.kind, EventKind::ToolResult { .. }))
            .expect("a tool_result event");
        assert_eq!(
            result.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::APPROVAL_REJECTED)
        );
    }
}
