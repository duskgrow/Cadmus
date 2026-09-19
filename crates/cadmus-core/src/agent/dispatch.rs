//! Tool-call dispatch (ADR-0008 items 2/4): per-call approval and execution
//! advance together, while result events and messages retain original order —
//! except on interrupt, which stops new starts, skips untouched calls without
//! inventing rejections for them, and records the results that did land in
//! their relative order.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;

use cadmus_contract::{
    Approval, EventError, EventKind, LiveKind, Message, TimedRecv, ToolCall, ToolCompletion, attrs,
    error_kinds,
};
use serde_json::Value;
use tokio_stream::{Stream, StreamExt};

use super::events::interrupted_detail;
use super::fold::ResultTrack;
use super::gate::Gate;
use super::{AgentError, AgentLoop, Concurrency, ToolError};

type ToolOutcome = (Value, Option<EventError>);
type CommandWait<'a> = Pin<Box<dyn Future<Output = TimedRecv> + Send + 'a>>;

enum DispatchTick {
    Tool(usize, ToolOutcome),
    Command(TimedRecv),
}

impl AgentLoop {
    /// All-parallel-safe batches launch every decided call without waiting
    /// for siblings. Any serial declaration limits the batch to one invocation
    /// at a time, choosing the first ready, unstarted call in original order.
    /// Undecided siblings do not block execution; only durable result events
    /// and messages wait for original positions.
    ///
    /// The cooperative poll loop needs no runtime. Interrupt stops new starts,
    /// drains in-flight work, and records its results before the terminal event.
    // Keep start/flush/interrupt ordering together: these transitions share
    // the same cursors, and splitting them obscures which calls may still run.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn dispatch_tools(
        &self,
        calls: Vec<ToolCall>,
        messages: &mut Vec<Message>,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        let mut gate = self.open_gate(&calls, turn);
        let all_safe = calls.iter().all(|call| self.is_parallel_safe(call));
        let mut branches = tokio_stream::StreamMap::new();
        let mut spans = vec![None; calls.len()];
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        let mut flushed = 0;
        let mut interrupt = None;
        let mut waiting: Option<CommandWait<'_>> = None;
        let mut completed = Vec::new();
        loop {
            // A parked receive owns the source's mutex, so poll_incoming alone
            // can miss a queued interrupt when a tool completes at the same time.
            // Poll that receiver before admitting any more work.
            if let Some(Poll::Ready(command)) =
                poll_fn(|cx| Poll::Ready(waiting.as_mut().map(|wait| wait.as_mut().poll(cx)))).await
            {
                waiting = None;
                self.receive_during_dispatch(command, &mut gate, root_span, turn)?;
            }
            self.poll_incoming();
            if interrupt.is_none() {
                interrupt = self.take_interrupt();
            }
            let mut admitted = false;
            if interrupt.is_none() {
                gate.drain(self, root_span, turn)?;
                for (position, call) in calls.iter().enumerate().skip(flushed) {
                    if !all_safe && !branches.is_empty() {
                        break;
                    }
                    if spans[position].is_some() {
                        continue;
                    }
                    let Some(decision) = &gate.decisions[position] else {
                        continue;
                    };
                    let span = self.next_span();
                    self.emit(&self.turn_event(
                        &span,
                        root_span,
                        turn,
                        EventKind::ToolCall { call: call.clone() },
                    ))?;
                    spans[position] = Some(span);
                    admitted = true;
                    match decision {
                        Approval::Rejected { comment } => {
                            outcomes[position] = Some(rejection(call, comment.as_deref()));
                            completed.push(position);
                        }
                        Approval::Approved => {
                            // One pinned single-item stream per call, driven on
                            // this task. Blocking tools still run synchronously.
                            branches.insert(
                                position,
                                Box::pin(
                                    tokio_stream::iter(std::iter::once(self.execute(call)))
                                        .then(std::convert::identity),
                                ),
                            );
                        }
                    }
                    if !all_safe {
                        break;
                    }
                }
            }
            while flushed < calls.len() {
                if let Some(outcome) = outcomes[flushed].take() {
                    self.push_result(
                        &calls[flushed],
                        spans[flushed].as_deref().expect("started call"),
                        outcome,
                        gate.approval_address(flushed),
                        messages,
                        root_span,
                        turn,
                    )?;
                } else if interrupt.is_none() || spans[flushed].is_some() {
                    break;
                }
                // On interrupt, untouched slots are not fabricated as denials;
                // completed later calls still reach the log, in relative order.
                flushed += 1;
            }
            // The ordered cursor consumed everything immediately recordable.
            // Publish only newly completed outcomes still blocked behind it;
            // their durable result later reconciles by this exact span id.
            for position in completed.drain(..) {
                if let Some((result, error)) = &outcomes[position] {
                    self.publish(LiveKind::ToolCompleted {
                        completion: ToolCompletion {
                            span_id: spans[position].clone().expect("started call"),
                            turn: u32::try_from(turn).unwrap_or(u32::MAX),
                            message_call_index: position,
                            call_id: calls[position].id.clone(),
                            name: calls[position].name.clone(),
                            result: result.clone(),
                            error: error.clone(),
                        },
                    });
                }
            }
            if flushed == calls.len() {
                break;
            }
            // A newly admitted rejection may leave more ready serial work.
            // Buffered later outcomes alone must never cause a busy loop.
            if admitted && branches.is_empty() {
                continue;
            }
            if !gate.pending() || interrupt.is_some() {
                waiting = None;
            } else if waiting.is_none() {
                waiting = Some(self.protocol.commands.recv_timeout(gate.remaining(self)));
            }
            // Retain the receive future across tool completions: neither a
            // tool wake nor a stray command resets the gate's fixed deadline.
            let tick = poll_fn(|cx| {
                if let Poll::Ready(Some((position, outcome))) =
                    Pin::new(&mut branches).poll_next(cx)
                {
                    return Poll::Ready(DispatchTick::Tool(position, outcome));
                }
                if let Some(wait) = &mut waiting
                    && let Poll::Ready(command) = wait.as_mut().poll(cx)
                {
                    return Poll::Ready(DispatchTick::Command(command));
                }
                Poll::Pending
            })
            .await;
            match tick {
                DispatchTick::Tool(position, outcome) => {
                    branches.remove(&position);
                    outcomes[position] = Some(outcome);
                    completed.push(position);
                }
                DispatchTick::Command(command) => {
                    waiting = None;
                    self.receive_during_dispatch(command, &mut gate, root_span, turn)?;
                }
            }
        }
        if let Some(command) = interrupt {
            // Recorded after the drained results: those commands were applied
            // when they arrived (record-on-effect), and the interrupt's
            // application point is here, where it stopped new starts.
            self.record_command(command, root_span, turn)?;
            self.finish_with(root_span, turn, Some(interrupted_detail()))?;
            return Err(AgentError::Interrupted);
        }
        Ok(())
    }

    fn receive_during_dispatch(
        &self,
        command: TimedRecv,
        gate: &mut Gate,
        root_span: &str,
        turn: usize,
    ) -> Result<(), AgentError> {
        match command {
            TimedRecv::Command(command) => self.note(command),
            TimedRecv::Closed => gate.deny_unanswered(self, root_span, turn, false)?,
            TimedRecv::TimedOut => gate.deny_unanswered(self, root_span, turn, true)?,
        }
        Ok(())
    }

    fn is_parallel_safe(&self, call: &ToolCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.concurrency() == Concurrency::ParallelSafe)
    }

    /// Appends the result event (errored on failure) and the tool message
    /// (`is_error`-marked on failure) for one settled call.
    #[allow(clippy::too_many_arguments)] // Event coordinates plus the optional approval address.
    fn push_result(
        &self,
        call: &ToolCall,
        tool_span: &str,
        outcome: (Value, Option<EventError>),
        approval: Option<(&str, usize)>,
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
        if let Some((request_id, call_index)) = approval {
            result_event = result_event
                .with_attribute(attrs::APPROVAL_REQUEST_ID, request_id)
                .with_attribute(attrs::APPROVAL_CALL_INDEX, call_index);
        }
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
