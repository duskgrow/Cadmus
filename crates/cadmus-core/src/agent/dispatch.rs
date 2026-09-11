//! Tool-call dispatch (ADR-0008 items 2/4): batch approval first, then
//! serial or cooperative-parallel execution, with messages and events
//! always landing in call order so the trajectory reads the same either way.

use std::collections::HashMap;

use cadmus_contract::{EventError, EventKind, Message, ToolCall, error_kinds};
use serde_json::Value;
use tokio_stream::StreamExt;

use super::fold::ResultTrack;
use super::{AgentError, AgentLoop, Concurrency, ToolError};

impl AgentLoop {
    /// Executes one assistant turn's tool calls, appending call/result event
    /// pairs, and pushing the tool messages onto the history. A batch runs
    /// concurrently only when EVERY call in it is parallel-safe; a single
    /// undeclared call serializes the whole batch in call order (ADR-0008
    /// item 2). Messages and events always land in call order, so the
    /// trajectory reads the same either way.
    ///
    /// Batch approval precedes any execution (ADR-0008 item 4 amendment):
    /// the turn's gated (mutation) calls are presented together, each
    /// approved or rejected independently, so a decision can depend on the
    /// batch's contents but never on another gated call's result. A
    /// rejected call never invokes; its rejection lands as an `is_error`
    /// tool result in call order.
    ///
    /// All-or-nothing, not segment mixing: the model emits a turn's calls as
    /// one unordered batch — it cannot know which tools are parallel-safe,
    /// and scheduling is not its job — so a finer-grained schedule buys no
    /// real ordering information, while a sloppy batch (a write followed by
    /// its own run command) is still rescued by in-order serial execution.
    /// Safety is a property of the tool's side effects (a same-file
    /// read-modify-write race, an external session's state), so the
    /// declaration lives on the tool with a fail-safe serial default. What
    /// the scheduler protects against is silent corruption, not errors —
    /// errors are already recoverable model feedback.
    pub(super) async fn dispatch_tools(
        &self,
        calls: Vec<ToolCall>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let denied = self.gate(&calls, root_span, turn).await?;
        let all_safe = calls.iter().all(|call| self.is_parallel_safe(call));
        if all_safe {
            self.dispatch_parallel(&calls, &denied, messages, root_span, turn)
                .await?;
        } else {
            for (position, call) in calls.iter().enumerate() {
                self.dispatch_one(call, denied.get(&position), messages, root_span, turn)
                    .await?;
            }
        }
        Ok(())
    }

    fn is_parallel_safe(&self, call: &ToolCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.concurrency() == Concurrency::ParallelSafe)
    }

    async fn dispatch_one(
        &self,
        call: &ToolCall,
        denied: Option<&Option<String>>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let tool_span = self.next_span();
        // The call event opens the span whether or not the gate lets the
        // invocation through — a rejected call is a paired, closed span,
        // never a dangling one.
        self.emit(&self.turn_event(
            &tool_span,
            root_span,
            turn,
            EventKind::ToolCall { call: call.clone() },
        ))?;
        let outcome = match denied {
            Some(comment) => rejection(call, comment.as_deref()),
            None => self.execute(call).await,
        };
        self.push_result(call, &tool_span, outcome, messages, root_span, turn)
    }

    /// One batch of parallel-safe calls (ADR-0008 item 2): call events land
    /// in order, results land in call order, and the invocations are driven
    /// cooperatively (one pinned single-item branch per call; `StreamMap`
    /// polls every branch — core stays runtime-free per ADR-0002).
    /// Cooperative means wall-time overlap exists only for futures that
    /// yield (MCP wrappers, approval waits): the built-in fs tools are
    /// blocking and never yield, so their batches currently serialize in
    /// wall time while keeping the same contract. Tool errors stay
    /// per-call; a panicking tool aborts the run, exactly as in serial
    /// execution.
    async fn dispatch_parallel(
        &self,
        calls: &[ToolCall],
        denied: &HashMap<usize, Option<String>>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let mut spans = Vec::with_capacity(calls.len());
        for call in calls {
            let tool_span = self.next_span();
            self.emit(&self.turn_event(
                &tool_span,
                root_span,
                turn,
                EventKind::ToolCall { call: call.clone() },
            ))?;
            spans.push(tool_span);
        }

        // One single-item stream per call; StreamMap polls every branch,
        // so the invocations are driven concurrently on this task. The
        // iter+then pair is load-bearing: `tokio_stream::once` would yield
        // the future OBJECT un-driven (its Item is the value itself).
        let mut branches = tokio_stream::StreamMap::new();
        let mut outcomes: Vec<Option<(Value, Option<EventError>)>> =
            (0..calls.len()).map(|_| None).collect();
        for (offset, call) in calls.iter().enumerate() {
            // A rejected call never becomes a branch: its denial is the
            // settled outcome, pre-filled in call order.
            if let Some(comment) = denied.get(&offset) {
                outcomes[offset] = Some(rejection(call, comment.as_deref()));
                continue;
            }
            let tool = self
                .tools
                .get(&call.name)
                .cloned()
                .expect("parallel-safe calls resolve to a registered tool");
            let arguments = call.arguments.clone();
            // Pinned to the heap for `Unpin`: an in-flight async block is
            // not `Unpin`, which `StreamMap` requires of its branches.
            let branch = Box::pin(
                tokio_stream::iter(std::iter::once(async move { tool.invoke(arguments).await }))
                    .then(std::convert::identity),
            );
            branches.insert(offset, branch);
        }
        while let Some((offset, result)) = branches.next().await {
            outcomes[offset] = Some(match result {
                Ok(content) => (content, None),
                Err(err) => tool_failure(&err),
            });
        }

        for (call, (tool_span, outcome)) in calls.iter().zip(spans.iter().zip(outcomes)) {
            // Invariant, not a runtime condition: every branch yields exactly
            // one item, so every slot is filled. No AgentError variant would
            // help a caller — a miss means this code changed shape — so the
            // assertion panics instead of propagating.
            let outcome = outcome.expect("every call in the batch settled");
            self.push_result(call, tool_span, outcome, messages, root_span, turn)?;
        }
        Ok(())
    }

    /// Appends the result event (errored on failure) and the tool message
    /// (`is_error`-marked on failure) for one settled call.
    fn push_result(
        &self,
        call: &ToolCall,
        tool_span: &str,
        outcome: (Value, Option<EventError>),
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let (result, error) = outcome;
        self.note_outcome(call, error.as_ref());
        let is_error = error.is_some();
        let mut result_event = self.turn_event(
            tool_span,
            root_span,
            turn,
            EventKind::ToolResult {
                call_id: call.id.clone(),
                result: result.clone(),
            },
        );
        if let Some(error) = error {
            result_event = result_event.errored(error);
        }
        self.emit(&result_event)?;
        // Track the result's coordinates for the fold machinery: the history
        // position it is about to occupy, its turn and its event id.
        self.tool_result_tracks
            .lock()
            .expect("tracks poisoned")
            .push(ResultTrack {
                msg_index: messages.len(),
                turn,
                event_id: result_event.id.clone(),
                call_id: call.id.clone(),
            });
        messages.push(if is_error {
            Message::tool_error(call.id.clone(), result)
        } else {
            Message::tool_result(call.id.clone(), result)
        });
        Ok(())
    }

    /// Returns the result value and, for failures, the structured error: the
    /// message stream collapses both into text for the model, the trajectory
    /// keeps them apart. A hallucinated tool name is feedback, not a fatal
    /// error.
    async fn execute(&self, call: &ToolCall) -> (Value, Option<EventError>) {
        let Some(tool) = self.tools.get(&call.name) else {
            let message = format!("unknown tool: {}", call.name);
            return (
                Value::String(message.clone()),
                Some(EventError {
                    kind: error_kinds::UNKNOWN_TOOL.into(),
                    message,
                }),
            );
        };
        match tool.invoke(call.arguments.clone()).await {
            Ok(content) => (content, None),
            Err(err) => tool_failure(&err),
        }
    }
}

fn tool_failure(err: &ToolError) -> (Value, Option<EventError>) {
    (
        Value::String(err.to_string()),
        Some(EventError {
            kind: error_kinds::TOOL.into(),
            message: err.to_string(),
        }),
    )
}

/// A rejected call's outcome: the denial is model feedback (ADR-0008 item 4
/// — a rejection is returned as a tool result so it enters the trajectory),
/// phrased so the model cannot read the effect as having happened; the
/// event's error kind lets reflection tell a reviewer's no from a tool
/// failure.
fn rejection(call: &ToolCall, comment: Option<&str>) -> (Value, Option<EventError>) {
    let reason = comment.unwrap_or("no reason given");
    let message = format!(
        "{} call rejected ({reason}); it was not executed — do not assume its effect \
         happened. Adjust the approach or continue without it.",
        call.name
    );
    (
        Value::String(message.clone()),
        Some(EventError {
            kind: error_kinds::APPROVAL_REJECTED.into(),
            message,
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cadmus_contract::ToolSpec;
    use serde_json::json;

    use super::*;
    use crate::ReplayProvider;
    use crate::agent::fixtures::{
        EchoTool, StepTool, YieldTool, call, test_capabilities, test_loop,
    };
    use crate::agent::{AgentTool, ToolError};

    struct FailTool;

    #[async_trait]
    impl AgentTool for FailTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "fail".into(),
                description: "always fails".into(),
                parameters: json!({"type": "object"}),
            }
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Err(ToolError {
                tool: "fail".into(),
                message: "boom".into(),
            })
        }
    }

    /// A parallel-safe fake that waits for a notification before ending.
    struct WaitTool {
        name: &'static str,
        notify: Arc<tokio::sync::Notify>,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentTool for WaitTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "wait".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            self.log.lock().await.push(format!("start {}", self.name));
            self.notify.notified().await;
            self.log.lock().await.push(format!("end {}", self.name));
            Ok(Value::String(self.name.into()))
        }
    }

    /// A parallel-safe fake that ends and then releases the waiter — its
    /// completion strictly precedes the waiter's, under any poll order
    /// (`notify_one` stores the permit when nobody waits yet).
    struct ReleaseTool {
        name: &'static str,
        notify: Arc<tokio::sync::Notify>,
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentTool for ReleaseTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "release".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            self.log.lock().await.push(format!("start {}", self.name));
            self.log.lock().await.push(format!("end {}", self.name));
            self.notify.notify_one();
            Ok(Value::String(self.name.into()))
        }
    }

    struct ParallelFailTool;

    #[async_trait]
    impl AgentTool for ParallelFailTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "pfail".into(),
                description: "parallel-safe, always fails".into(),
                parameters: json!({"type": "object"}),
            }
        }

        fn concurrency(&self) -> Concurrency {
            Concurrency::ParallelSafe
        }

        async fn invoke(&self, _arguments: Value) -> Result<Value, ToolError> {
            Err(ToolError {
                tool: "pfail".into(),
                message: "boom".into(),
            })
        }
    }

    fn dispatch_harness(tools: Vec<Arc<dyn AgentTool>>) -> AgentLoop {
        let provider = ReplayProvider::new([]).with_capabilities(test_capabilities());
        let (agent, _sink) = test_loop(provider, tools, 8);
        agent
    }

    #[tokio::test]
    async fn parallel_safe_calls_overlap_and_land_in_call_order() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = dispatch_harness(vec![
            Arc::new(WaitTool {
                name: "p1",
                notify: notify.clone(),
                log: log.clone(),
            }),
            Arc::new(ReleaseTool {
                name: "p2",
                notify,
                log: log.clone(),
            }),
        ]);
        let mut messages = Vec::new();

        // A serialized batch deadlocks on the notification; the timeout turns
        // a concurrency regression into a fast, legible failure.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            agent.dispatch_tools(
                vec![call("c1", "p1"), call("c2", "p2")],
                &mut messages,
                "root",
                1,
            ),
        )
        .await
        .expect("parallel batch must not serialize into a deadlock")
        .expect("dispatch");

        // Overlap proven and completion deterministically staggered: p2 ran
        // to completion and released p1, so "end p2" precedes "end p1" under
        // any branch poll order.
        let log = log.lock().await;
        let end_p2 = log.iter().position(|entry| entry == "end p2");
        let end_p1 = log.iter().position(|entry| entry == "end p1");
        assert!(end_p2 < end_p1, "got: {log:?}");
        drop(log);

        // Results land in call order despite the staggered completion.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(!messages[0].is_error && !messages[1].is_error);
    }

    #[tokio::test]
    async fn a_batch_with_an_undeclared_call_serializes_entirely() {
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let agent = dispatch_harness(vec![
            Arc::new(YieldTool {
                name: "p1",
                log: log.clone(),
            }),
            Arc::new(YieldTool {
                name: "p2",
                log: log.clone(),
            }),
            Arc::new(StepTool {
                name: "s1",
                log: log.clone(),
            }),
        ]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "p1"), call("c2", "p2"), call("c3", "s1")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // One undeclared call serializes the whole batch, in call order.
        let log = log.lock().await;
        assert_eq!(
            log.as_slice(),
            [
                "start p1", "end p1", "start p2", "end p2", "start s1", "end s1"
            ],
            "got: {log:?}"
        );
        drop(log);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("c3"));
    }

    #[tokio::test]
    async fn tool_errors_are_marked_is_error_in_the_history() {
        let agent = dispatch_harness(vec![Arc::new(FailTool), Arc::new(EchoTool)]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "fail"), call("c2", "echo")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        assert!(messages[0].is_error);
        assert!(
            matches!(&messages[0].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("boom")),
            "got: {:?}",
            messages[0].content
        );
        assert!(!messages[1].is_error);
    }

    #[tokio::test]
    async fn a_failure_inside_a_parallel_batch_stays_per_call() {
        let agent = dispatch_harness(vec![
            Arc::new(ParallelFailTool),
            Arc::new(YieldTool {
                name: "ok",
                log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            }),
        ]);
        let mut messages = Vec::new();

        agent
            .dispatch_tools(
                vec![call("c1", "pfail"), call("c2", "ok")],
                &mut messages,
                "root",
                1,
            )
            .await
            .expect("dispatch");

        // The failure is marked and its sibling is unaffected (ADR-0008:
        // a failure cascades only within its own batch).
        assert!(messages[0].is_error);
        assert!(
            matches!(&messages[0].content[0], cadmus_contract::ContentPart::Text { text } if text.contains("boom")),
            "got: {:?}",
            messages[0].content
        );
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("c2"));
        assert!(!messages[1].is_error);
    }
}
