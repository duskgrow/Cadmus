//! Client-command intake (ADR-0013 item 6): classification on receipt,
//! idempotent retry by client-minted command id, and the defined application
//! points (turn boundary, mid-stream between chunks, gate wait, finish line).
//! Commands are recorded at application — never at receipt — so the replayed
//! history equals the live one (ADR-0005's fold invariant).

use std::collections::{HashSet, VecDeque};

use cadmus_contract::{Command, Message, SteerMode};

use super::events::interrupted_detail;
use super::{AgentError, AgentLoop};

/// Commands received ahead of their application point, classified on
/// receipt (ADR-0013 item 6: the loop applies each command in receipt
/// order at its defined point). The kinds split because their application
/// points and consumption rules differ — a resolve is matched to its gate
/// by request id, steers partition by mode and apply in receipt order, an
/// interrupt is first-wins state — one shared queue would make every
/// consumer re-scan and re-order. Client command ids are deduped at the
/// gate of every path — a retried submission applies exactly once.
#[derive(Default)]
pub(super) struct Inbox {
    /// Resolves awaiting their gate (a resolve can only name an already
    /// published request, so a stale one sits harmlessly until the run
    /// ends — late answers and retries are safe by construction).
    resolves: VecDeque<Command>,
    /// Steers not yet applied; they land as user messages at the next
    /// application point, in receipt order.
    steers: Vec<Command>,
    /// The first pending interrupt (a second one changes nothing).
    interrupt: Option<Command>,
    /// Client command ids already seen — the idempotent-retry seam.
    seen: HashSet<String>,
}

impl AgentLoop {
    /// Classifies one received command into the inbox, deduped by the
    /// client-minted command id (ADR-0002's idempotent-retry seam). A
    /// `start_run` mid-run is a protocol anomaly: ignored.
    pub(super) fn note(&self, command: Command) {
        let mut inbox = self.inbox.lock().expect("inbox poisoned");
        if let Some(id) = command.command_id()
            && !inbox.seen.insert(id.to_string())
        {
            return;
        }
        match command {
            Command::ResolveApproval { .. } => inbox.resolves.push_back(command),
            Command::Steer { .. } => inbox.steers.push(command),
            Command::Interrupt { .. } => {
                if inbox.interrupt.is_none() {
                    inbox.interrupt = Some(command);
                }
            }
            Command::StartRun { .. } => {}
        }
    }

    /// Non-blocking drain of the command source into the inbox — the
    /// mid-stream and boundary seam.
    pub(super) fn poll_incoming(&self) {
        while let Some(command) = self.protocol.commands.poll() {
            self.note(command);
        }
    }

    /// The pending interrupt, taken once (the caller applies it).
    pub(super) fn take_interrupt(&self) -> Option<Command> {
        self.inbox.lock().expect("inbox poisoned").interrupt.take()
    }

    /// The resolve naming `request_id`, taken out of the inbox once.
    pub(super) fn take_resolve(&self, request_id: &str) -> Option<Command> {
        let mut inbox = self.inbox.lock().expect("inbox poisoned");
        let position = inbox.resolves.iter().position(|command| {
            matches!(
                command,
                Command::ResolveApproval { request_id: rid, .. } if rid == request_id
            )
        })?;
        inbox.resolves.remove(position)
    }

    /// Turn-boundary application point: a pending interrupt ends the run
    /// (command recorded, then the terminal record); otherwise every
    /// buffered inject-mode steer lands as a user message ahead of this
    /// turn's request. Queue-mode steers stay buffered — they apply only
    /// when the run would otherwise finish.
    pub(super) fn drain_boundary(
        &self,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        self.poll_incoming();
        if let Some(command) = self.take_interrupt() {
            self.record_command(command, root_span, turn)?;
            self.finish_with(root_span, turn - 1, Some(interrupted_detail()))?;
            return Err(AgentError::Interrupted);
        }
        self.apply_steers(messages, root_span, turn, false)?;
        Ok(())
    }

    /// Appends buffered steers as user messages, recording each command at
    /// its application point (the fold-invariant recording rule). Inject
    /// steers apply at any boundary; queue steers only when
    /// `include_queued` (the run's would-be finish line). Returns how many
    /// steers were applied.
    pub(super) fn apply_steers(
        &self,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
        include_queued: bool,
    ) -> Result<usize, AgentError> {
        let steers: Vec<Command> = {
            let mut inbox = self.inbox.lock().expect("inbox poisoned");
            let (apply, keep) = std::mem::take(&mut inbox.steers).into_iter().partition(
                |command| {
                    include_queued
                        || matches!(command, Command::Steer { mode, .. } if *mode == SteerMode::Inject)
                },
            );
            inbox.steers = keep;
            apply
        };
        let count = steers.len();
        for command in steers {
            let Command::Steer { text, .. } = &command else {
                continue;
            };
            messages.push(Message::user(text.clone()));
            self.record_command(command, root_span, turn)?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cadmus_contract::{
        Approval, ChatRequest, EventKind, FinishReason, LiveItem, LiveKind, Status, StreamChunk,
        ToolCall, ToolSpec, error_kinds,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{
        EchoTool, SendOnMatch, kind_name, test_capabilities, text_script, tool_call_script,
    };
    use crate::agent::{AgentTool, ClientProtocol, ToolError};
    use crate::testing::test_telemetry;

    /// A resolve naming an unknown request is dropped — late answers and
    /// retries can never resurrect a settled (or never-open) gate.
    #[tokio::test]
    async fn a_resolve_for_an_unknown_request_is_dropped() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"ping\"}"),
            text_script("done"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        // A bogus resolve sits in the channel before the real request exists.
        sender
            .send(Command::ResolveApproval {
                command_id: "cmd-bogus".into(),
                request_id: "ap-bogus".into(),
                decisions: vec![Approval::Rejected {
                    comment: Some("stray".into()),
                }],
            })
            .expect("channel open");
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
            .expect("the stray resolve must not poison the run");

        assert_eq!(outcome.turns, 2);
        let resolves: Vec<_> = sink
            .events()
            .into_iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    EventKind::Command(Command::ResolveApproval { .. })
                )
            })
            .collect();
        assert_eq!(
            resolves.len(),
            1,
            "only the real request's resolve is recorded"
        );
        assert!(!outcome.messages[2].is_error, "the call was approved");
    }

    /// ADR-0002's idempotent-retry seam: a retried submission (same client
    /// command id) applies exactly once — one message, one command event.
    #[tokio::test]
    async fn a_retried_command_applies_exactly_once() {
        let provider = ReplayProvider::new([text_script("one"), text_script("two")])
            .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        let steer = || Command::Steer {
            command_id: "cmd-dup".into(),
            text: "one more thing".into(),
            mode: SteerMode::Queue,
        };
        sender.send(steer()).expect("first send");
        sender.send(steer()).expect("the retry");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the queued steer extends the run");

        let steered: Vec<_> = outcome
            .messages
            .iter()
            .filter(|message| {
                matches!(&message.content[0], cadmus_contract::ContentPart::Text { text } if text == "one more thing")
            })
            .collect();
        assert_eq!(steered.len(), 1, "the retry deduped on the command id");
        let steers = sink
            .events()
            .iter()
            .filter(|event| matches!(&event.kind, EventKind::Command(Command::Steer { .. })))
            .count();
        assert_eq!(steers, 1, "one command event, not two");
    }

    /// An inject steer sent while turn 1 streams lands at the next request
    /// boundary — never in the in-flight request — recorded at application
    /// so the fold matches the live history.
    #[tokio::test]
    async fn an_injected_steer_lands_at_the_next_request_boundary() {
        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"ping\"}"),
            text_script("pong received"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let recording = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        // React to turn 1's recorded response exactly like a watching
        // client: the steer enters the channel while the tools dispatch.
        // The auto-resolver sits behind it so the gate still resolves.
        let reactive = SendOnMatch::new(
            recording,
            sender.clone(),
            |item: &LiveItem| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event }
                        if matches!(event.kind, EventKind::LlmResponse { .. })
                )
            },
            Command::Steer {
                command_id: "cmd-steer".into(),
                text: "also say thanks".into(),
                mode: SteerMode::Inject,
            },
        );
        let resolver = crate::testing::AutoResolver::new(
            std::sync::Arc::new(reactive),
            sender,
            |calls: &[ToolCall]| calls.iter().map(|_| Approval::Approved).collect(),
        );
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(resolver),
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
            .run(&ChatRequest::user_text("say ping", 1_024))
            .await
            .expect("run");

        // user → assistant(call) → tool(result) → user(steer) → assistant(text)
        let roles: Vec<_> = outcome
            .messages
            .iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(
            roles,
            [
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
                cadmus_contract::Role::Tool,
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
            ]
        );
        assert!(
            matches!(&outcome.messages[3].content[0], cadmus_contract::ContentPart::Text { text } if text == "also say thanks")
        );
        // The command is recorded at application — between the tool result
        // and turn 2's request — so the replayed history equals the live one.
        let events = sink.events();
        let state = crate::replay_trace(&events);
        assert_eq!(state.messages, outcome.messages);
    }

    /// A queue steer holds past mid-task boundaries and fires only at the
    /// would-be finish line: the run continues instead of ending.
    #[tokio::test]
    async fn a_queued_steer_continues_a_run_that_would_finish() {
        let provider = ReplayProvider::new([text_script("first"), text_script("second")])
            .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        sender
            .send(Command::Steer {
                command_id: "cmd-queue".into(),
                text: "one more thing".into(),
                mode: SteerMode::Queue,
            })
            .expect("send");
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the queued steer extends the run");

        assert_eq!(outcome.turns, 2);
        // user → assistant("first") → user(steer) → assistant("second")
        assert!(
            matches!(&outcome.messages[1].content[0], cadmus_contract::ContentPart::Text { text } if text == "first")
        );
        assert!(
            matches!(&outcome.messages[2].content[0], cadmus_contract::ContentPart::Text { text } if text == "one more thing")
        );
        let state = crate::replay_trace(&sink.events());
        assert_eq!(state.messages, outcome.messages);
    }

    /// Mid-stream interrupt (chunk granularity): the stream is dropped, the
    /// partial turn is recorded errored, and the terminal record carries the
    /// interruption — completed work is preserved.
    #[tokio::test]
    async fn an_interrupt_mid_stream_truncates_the_turn() {
        let provider = ReplayProvider::new([ReplayProvider::script(vec![
            StreamChunk::TextDelta("par".into()),
            StreamChunk::TextDelta("tial".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ])])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let live = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        let live = std::sync::Arc::new(SendOnMatch::new(
            live,
            sender,
            |item: &LiveItem| matches!(item.kind, LiveKind::AssistantDelta { .. }),
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        ));
        let protocol = ClientProtocol {
            live,
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("the run is interrupted");
        assert!(matches!(err, AgentError::Interrupted));

        let events = sink.events();
        let kinds: Vec<&str> = events.iter().map(kind_name).collect();
        assert_eq!(
            kinds,
            [
                "start_run",
                "llm_request",
                "interrupt",
                "llm_response",
                "run_finished"
            ],
            "the command lands before its consequences"
        );
        let response = &events[3];
        assert_eq!(response.status, Status::Error);
        assert_eq!(
            response.error.as_ref().map(|error| error.kind.as_str()),
            Some(error_kinds::INTERRUPTED)
        );
        let EventKind::LlmResponse { message, .. } = &response.kind else {
            panic!("expected llm_response");
        };
        assert!(matches!(
            message.content.first(),
            Some(cadmus_contract::ContentPart::Text { text }) if text == "par"
        ));
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Error);
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 0 }));
    }
    /// An interrupt planted during turn 1's tool dispatch lands at the next
    /// boundary: dispatches in flight settle first (their results are
    /// recorded), then the run ends.
    #[tokio::test]
    async fn an_interrupt_at_the_boundary_lets_dispatch_settle() {
        /// Sends the interrupt when invoked — a mid-run client action at a
        /// deterministic point.
        struct InterruptTool(std::sync::mpsc::Sender<Command>);

        #[async_trait]
        impl AgentTool for InterruptTool {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "echo".into(),
                    description: "echoes and interrupts".into(),
                    parameters: json!({"type": "object"}),
                }
            }

            async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
                let _ = self.0.send(Command::Interrupt {
                    command_id: "cmd-esc".into(),
                });
                Ok(arguments)
            }
        }

        let provider = ReplayProvider::new([
            tool_call_script("c1", "{\"text\":\"x\"}"),
            text_script("never reached"),
        ])
        .with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let (protocol, _live, sender) = crate::testing::protocol_with(|calls| {
            calls.iter().map(|_| Approval::Approved).collect()
        });
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![Arc::new(InterruptTool(sender))],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let err = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect_err("interrupted at the boundary");
        assert!(matches!(err, AgentError::Interrupted));
        let events = sink.events();
        // The tool result landed before the run ended.
        assert!(events.iter().any(
            |event| matches!(&event.kind, EventKind::ToolResult { call_id, .. } if call_id == "c1")
        ));
        let finished = events.last().expect("terminal record");
        assert!(matches!(finished.kind, EventKind::RunFinished { turns: 1 }));
        assert_eq!(finished.status, Status::Error);
    }
    /// The finish-line race: a run that completed before the interrupt was
    /// applied finishes clean — the moot interrupt is dropped unrecorded,
    /// and a queued steer it arrived with is canceled with it (the user
    /// stopped the run; "one more thing" dies with it).
    #[tokio::test]
    async fn an_interrupt_at_the_finish_line_is_moot() {
        let provider =
            ReplayProvider::new([text_script("one")]).with_capabilities(test_capabilities());
        let (telemetry, sink) = test_telemetry("tr-test");
        let recording = std::sync::Arc::new(crate::testing::RecordingLive::default());
        let (commands, sender) = crate::testing::ChannelCommands::new();
        sender
            .send(Command::Steer {
                command_id: "cmd-queue".into(),
                text: "one more thing".into(),
                mode: SteerMode::Queue,
            })
            .expect("the queued steer");
        // The interrupt lands when turn 1's response is recorded — after
        // the stream, before the finish check: the exact finish-line window.
        let reactive = SendOnMatch::new(
            recording,
            sender,
            |item: &LiveItem| {
                matches!(
                    &item.kind,
                    LiveKind::Recorded { event }
                        if matches!(event.kind, EventKind::LlmResponse { .. })
                )
            },
            Command::Interrupt {
                command_id: "cmd-esc".into(),
            },
        );
        let protocol = ClientProtocol {
            live: std::sync::Arc::new(reactive),
            commands: std::sync::Arc::new(commands),
        };
        let agent = AgentLoop::new(
            Arc::new(provider),
            vec![],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );

        let outcome = agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("the run finished before the interrupt applied");

        assert_eq!(outcome.turns, 1);
        assert_eq!(
            outcome.messages.len(),
            2,
            "the queued steer was canceled with the interrupt"
        );
        let events = sink.events();
        assert!(
            events.iter().all(|event| !matches!(
                event.kind,
                EventKind::Command(Command::Steer { .. } | Command::Interrupt { .. })
            )),
            "moot commands are never recorded"
        );
        let finished = events.last().expect("terminal record");
        assert_eq!(finished.status, Status::Ok);
    }
}
