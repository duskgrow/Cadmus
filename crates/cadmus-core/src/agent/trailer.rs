//! The loop's ADR-0007 item-1 machinery: the trailer state (per-tool
//! counters and the todo list — code-folded from settled calls, never
//! model-recomputed), the per-turn trailer render, and nested-instruction
//! injection.

use cadmus_contract::{
    EventError, EventKind, InstructionFile, Message, TodoItem, ToolCall, error_kinds,
};

use super::{AgentError, AgentLoop};
use crate::context::{TODO_WRITE, TrailerView, format_injected, render_trailer};

impl AgentLoop {
    /// Folds one settled call into the trailer state (ADR-0007 item 1(c)):
    /// the per-tool counter counts executions — gate rejections and
    /// unknown-tool answers never executed, so neither is counted — and a
    /// successful `todo_write` replaces the todo list verbatim (the one
    /// model-authored value code may store, per the item's own exception).
    pub(super) fn note_outcome(&self, call: &ToolCall, error: Option<&EventError>) {
        let never_executed = matches!(
            error,
            Some(e) if e.kind == error_kinds::APPROVAL_REJECTED || e.kind == error_kinds::UNKNOWN_TOOL
        );
        if !never_executed {
            *self
                .tool_counts
                .lock()
                .expect("tool counts poisoned")
                .entry(call.name.clone())
                .or_insert(0) += 1;
        }
        if error.is_none()
            && call.name == TODO_WRITE
            && let Some(items) = call.arguments.get("items")
            && let Ok(items) = serde_json::from_value::<Vec<TodoItem>>(items.clone())
        {
            *self.todos.lock().expect("todos poisoned") = items;
        }
    }

    /// Renders this turn's trailer from the probe's fresh snapshot, the
    /// injected clock and the folded state. The probe runs before any lock
    /// is taken — it may block on a subprocess, and the folded state does
    /// not depend on it.
    pub(super) fn render_trailer(&self, run_start_ms: u64) -> String {
        let git = self.context.probe.snapshot();
        let counts = self.tool_counts.lock().expect("tool counts poisoned");
        let todos = self.todos.lock().expect("todos poisoned");
        render_trailer(&TrailerView {
            cwd: &self.context.cwd,
            git,
            now_ms: self.telemetry.clock.now_unix_ms(),
            run_start_ms,
            // Minutes cross the contract boundary as a primitive (its dep
            // set is closed); the conversion is range-checked here.
            offset: time::UtcOffset::from_whole_seconds(
                self.telemetry.clock.utc_offset_minutes().saturating_mul(60),
            )
            .unwrap_or(time::UtcOffset::UTC),
            tool_counts: &counts,
            todos: &todos,
        })
    }

    /// Appends newly-entered subtrees' instruction files as standalone user
    /// messages (ADR-0007 item 1(a)): never folded into a tool result (that
    /// would pollute the errors-are-corrections channel), each recorded as
    /// an `instruction_injected` event so the fold rebuilds identical bytes.
    pub(super) fn inject_nested(
        &self,
        files: Vec<InstructionFile>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        for file in files {
            let text = format_injected(&file);
            let span = self.next_span();
            self.emit(&self.turn_event(
                &span,
                root_span,
                turn,
                EventKind::InstructionInjected {
                    path: file.path,
                    content: file.content,
                },
            ))?;
            messages.push(Message::user(text));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cadmus_contract::{Approval, ChatRequest, Clock, FinishReason, StreamChunk, ToolSpec};
    use serde_json::{Value, json};

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{
        EchoTool, StepTool, test_capabilities, text_of, text_script, tool_call_script,
    };
    use crate::agent::{AgentTool, ContextBundle, Telemetry, ToolError};
    use crate::context::InstructionTracker;
    use crate::testing::test_telemetry;

    struct TodoTool;

    #[async_trait]
    impl AgentTool for TodoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: TODO_WRITE.into(),
                description: "test todo tool".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Ok(Value::String("recorded".into()))
        }
    }

    /// A tracker that yields its files on the first call batch, then nothing.
    struct OneShotTracker(std::sync::Mutex<Option<Vec<InstructionFile>>>);

    impl InstructionTracker for OneShotTracker {
        fn on_calls(&self, calls: &[ToolCall]) -> Vec<InstructionFile> {
            if calls.is_empty() {
                return Vec::new();
            }
            self.0
                .lock()
                .expect("tracker poisoned")
                .take()
                .unwrap_or_default()
        }
    }
    /// A clock with a configurable UTC offset: pins the Clock → conversion →
    /// trailer wiring (without it, hardcoding UTC in `render_trailer` would
    /// keep every test green while the production feature died).
    struct OffsetClock(u64, i32);

    impl Clock for OffsetClock {
        fn now_unix_ms(&self) -> u64 {
            self.0
        }
        fn utc_offset_minutes(&self) -> i32 {
            self.1
        }
    }

    /// Runs one turn on a fixed clock (2026-09-03T00:00:00Z) with the given
    /// offset and returns the rendered trailer.
    async fn trailer_on_offset(offset_minutes: i32) -> String {
        let provider = Arc::new(
            ReplayProvider::new([text_script("done")]).with_capabilities(test_capabilities()),
        );
        let sink = Arc::new(crate::testing::RecordingSink::default());
        let telemetry = Telemetry {
            sink,
            clock: Arc::new(OffsetClock(1_788_393_600_000, offset_minutes)),
            ids: Arc::new(crate::testing::SeqIds::default()),
            trace_id: "tr-offset".into(),
            run_attributes: std::collections::BTreeMap::new(),
        };
        let agent = AgentLoop::new(
            provider.clone(),
            vec![],
            crate::testing::test_context(),
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("hi", 1_024))
            .await
            .expect("run");
        let requests = provider.requests();
        text_of(requests[0].messages.last().expect("trailer message"))
    }

    #[tokio::test]
    async fn the_trailer_clock_renders_at_the_clock_offset() {
        let trailer = trailer_on_offset(480).await;
        assert!(
            trailer.contains("time: 2026-09-03T08:00:00+08:00 (run elapsed 0s)"),
            "{trailer}"
        );
    }

    #[tokio::test]
    async fn a_garbage_offset_falls_back_to_utc() {
        let trailer = trailer_on_offset(i32::MAX).await;
        assert!(
            trailer.contains("time: 2026-09-03T00:00:00Z (run elapsed 0s)"),
            "{trailer}"
        );
    }
    #[tokio::test]
    async fn todo_write_folds_into_the_next_trailer() {
        let provider = Arc::new(ReplayProvider::new([
            ReplayProvider::script(vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: TODO_WRITE.into(),
                },
                StreamChunk::ToolArgsDelta {
                    index: 0,
                    fragment: "{\"items\":[{\"content\":\"write tests\",\"status\":\"in_progress\"},{\"content\":\"ship it\",\"status\":\"pending\"}]}".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::ToolCalls,
                },
            ]),
            text_script("done"),
        ]));
        let (telemetry, _sink) = test_telemetry("tr-todo");
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(TodoTool)],
            crate::testing::test_context(),
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("plan it", 1_024))
            .await
            .expect("run");

        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let trailer = text_of(requests[1].messages.last().expect("trailer message"));
        assert!(
            trailer.contains("tools: todo_write: 1"),
            "counter: {trailer}"
        );
        assert!(
            trailer.contains("[>] write tests"),
            "in-progress: {trailer}"
        );
        assert!(trailer.contains("[ ] ship it"), "pending: {trailer}");
    }

    #[tokio::test]
    async fn rejected_calls_stay_out_of_the_tool_counters() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let provider = Arc::new(ReplayProvider::new([
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
            text_script("done"),
        ]));
        let (telemetry, _sink) = test_telemetry("tr-rejected");
        let (protocol, _live, _sender) = crate::testing::protocol_with(|calls| {
            calls
                .iter()
                .map(|_| Approval::Rejected { comment: None })
                .collect()
        });
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(StepTool { name: "step", log })],
            crate::testing::test_context(),
            protocol,
            8,
            telemetry,
        );
        agent
            .run(&ChatRequest::user_text("try", 1_024))
            .await
            .expect("run");

        let requests = provider.requests();
        let trailer = text_of(requests[1].messages.last().expect("trailer message"));
        assert!(
            !trailer.contains("tools:"),
            "a rejected call never executed, so no counter line: {trailer}"
        );
    }

    #[tokio::test]
    async fn nested_instructions_land_in_history_event_and_fold() {
        let file = InstructionFile {
            path: "/repo/crates/x/AGENTS.md".into(),
            content: "crate rules\n".into(),
        };
        let provider = Arc::new(
            ReplayProvider::new([
                tool_call_script("c1", "{\"text\":\"ping\"}"),
                text_script("done"),
            ])
            .with_capabilities(test_capabilities()),
        );
        let (telemetry, sink) = test_telemetry("tr-nested");
        let context = ContextBundle {
            tracker: Arc::new(OneShotTracker(std::sync::Mutex::new(Some(vec![
                file.clone(),
            ])))),
            ..crate::testing::test_context()
        };
        let agent = AgentLoop::new(
            provider.clone(),
            vec![Arc::new(EchoTool)],
            context,
            crate::testing::auto_approving().0,
            8,
            telemetry,
        );
        let outcome = agent
            .run(&ChatRequest::user_text("go", 1_024))
            .await
            .expect("run");

        let injected_text = format_injected(&file);
        // The position is load-bearing, not just the presence: the injected
        // user message must land AFTER the tool results — between the
        // assistant's tool-call turn and its results it would be an invalid
        // sequence for strict providers. Pin the whole role sequence.
        let roles: Vec<cadmus_contract::Role> = outcome
            .messages
            .iter()
            .map(|message| message.role)
            .collect();
        assert_eq!(
            roles,
            vec![
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant,
                cadmus_contract::Role::Tool,
                cadmus_contract::Role::User,
                cadmus_contract::Role::Assistant
            ]
        );
        assert_eq!(text_of(&outcome.messages[3]), injected_text);
        let events = sink.events();
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            EventKind::InstructionInjected { path, content }
                if path == &file.path && content == &file.content
        )));
        // The fold invariant end to end: replaying the log reproduces the
        // live history byte for byte, injected messages included.
        let folded = crate::replay_trace(&events);
        assert_eq!(folded.messages, outcome.messages);
    }
}
