//! The provider contract test suite (report §9.2.1): the single authoritative
//! implementation of the [`Provider`] port's semantics. Every adapter wires in
//! with a one-line [`provider_contract_tests!`](crate::provider_contract_tests)
//! invocation; new adapters get the full port coverage for free, and the
//! suite's authoritative location stays unique.
//!
//! The suite never makes live calls — subjects are scripted: the replay fake
//! natively, network adapters through a local recorded-replay stub. Every
//! assertion is a port semantic; no adapter-private API appears here.
//!
//! The second half of this module is the client-protocol suite (ADR-0013
//! item 10): the executable semantics of attach — the Sync baseline, the
//! `drop positions ≤ as_of_seq` rule, pending-approval reconstruction and
//! lag recovery — instantiated by fakes, the in-process broadcaster and
//! (later) the stdio transport alike via
//! [`client_protocol_tests!`](crate::client_protocol_tests).

use std::collections::HashSet;

use tokio_stream::StreamExt;

use crate::{
    Attachment, ChatRequest, Command, Event, EventKind, FinishReason, InFlight, LiveItem, LiveKind,
    LiveUpdate, ModelError, Provider, StreamChunk, ToolSpec, attrs,
};

/// One scripted response for the subject under test.
#[derive(Debug)]
pub enum QueuedResponse {
    /// A chunk stream of all-`Ok` items.
    Chunks(Vec<StreamChunk>),
    /// `chat_stream` itself fails (HTTP error, auth failure, …).
    CallError(ModelError),
    /// The stream yields some `Ok` chunks, then an in-stream `Err` item —
    /// a broken frame or mid-flight provider error is an item, never a
    /// silently dropped stream (pitfall #9).
    StreamError {
        chunks: Vec<StreamChunk>,
        error: ModelError,
    },
}

/// A [`Provider`] the suite can script. The replay fake implements this
/// natively; stub-backed adapters implement it in their test harness on a
/// newtype wrapping the adapter plus its stub.
pub trait ContractSubject: Provider {
    /// Scripts the response of the next `chat_stream` call.
    fn queue(&self, response: QueuedResponse);
}

/// A completed stream must deliver the scripted chunks in order — the
/// degenerate non-streaming case is a one-chunk stream, so this pins both.
pub async fn text_answer_streams_verbatim(subject: &impl ContractSubject) {
    let script = vec![
        StreamChunk::TextDelta("Hello, ".into()),
        StreamChunk::TextDelta("world!".into()),
        StreamChunk::Done {
            finish: FinishReason::Stop,
        },
    ];
    subject.queue(QueuedResponse::Chunks(script.clone()));

    let chunks = collect_ok(subject, &ChatRequest::user_text("hi", 1_024)).await;
    assert_eq!(chunks, script, "streamed chunks must match the script");
}

/// Interleaved parallel tool calls are routed by `index` (pitfall #1): the
/// port-level invariant is index discipline — no fragment or end without an
/// open start, no duplicate start, and `Done` strictly terminal. Aggregation
/// itself is the core's job; the port guarantees a well-formed sequence.
pub async fn parallel_tool_stream_keeps_index_discipline(subject: &impl ContractSubject) {
    if !subject.capabilities().tools {
        // Meaningful only for tool-capable subjects; the mismatch branch is
        // covered by `capability_mismatch_fails_before_the_wire`.
        return;
    }
    subject.queue(QueuedResponse::Chunks(vec![
        StreamChunk::ToolCallStart {
            index: 0,
            id: "call_a".into(),
            name: "read_file".into(),
        },
        StreamChunk::ToolCallStart {
            index: 1,
            id: "call_b".into(),
            name: "grep".into(),
        },
        StreamChunk::ToolArgsDelta {
            index: 0,
            fragment: "{\"path\":\"/a".into(),
        },
        StreamChunk::ToolArgsDelta {
            index: 1,
            fragment: "{\"pattern\":\"fn".into(),
        },
        StreamChunk::ToolArgsDelta {
            index: 0,
            fragment: "\"}".into(),
        },
        StreamChunk::ToolArgsDelta {
            index: 1,
            fragment: " main\"}".into(),
        },
        StreamChunk::ToolCallEnd { index: 0 },
        StreamChunk::ToolCallEnd { index: 1 },
        StreamChunk::Done {
            finish: FinishReason::ToolCalls,
        },
    ]));

    let request = ChatRequest {
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        ..ChatRequest::user_text("read /a", 1_024)
    };
    let chunks = collect_ok(subject, &request).await;

    let mut started = HashSet::new();
    let mut ended = HashSet::new();
    for (position, chunk) in chunks.iter().enumerate() {
        match chunk {
            StreamChunk::ToolCallStart { index, .. } => {
                assert!(started.insert(*index), "duplicate start for index {index}");
                assert!(!ended.contains(index), "start after end for index {index}");
            }
            StreamChunk::ToolArgsDelta { index, .. } => {
                assert!(
                    started.contains(index) && !ended.contains(index),
                    "arguments fragment for non-open index {index}"
                );
            }
            StreamChunk::ToolCallEnd { index } => {
                assert!(
                    started.contains(index),
                    "end without start for index {index}"
                );
                assert!(ended.insert(*index), "duplicate end for index {index}");
            }
            StreamChunk::Done { finish } => {
                assert_eq!(
                    *finish,
                    FinishReason::ToolCalls,
                    "a tool-call turn ends with finish=tool_calls"
                );
                assert_eq!(
                    position,
                    chunks.len() - 1,
                    "Done must be the terminal chunk"
                );
            }
            _ => {}
        }
    }
    assert_eq!(started.len(), 2, "both scripted calls must appear");
}

/// A call-level failure surfaces as a typed [`ModelError`], and a failed call
/// does not poison the subject: the next scripted response streams normally.
pub async fn call_error_surfaces_then_recovers(subject: &impl ContractSubject) {
    subject.queue(QueuedResponse::CallError(ModelError::RateLimited {
        retry_after: None,
    }));
    let result = subject
        .chat_stream(&ChatRequest::user_text("hi", 1_024))
        .await;
    let Err(error) = result else {
        panic!("the scripted call error must surface");
    };
    assert!(
        matches!(error, ModelError::RateLimited { .. }),
        "expected RateLimited, got: {error}"
    );

    subject.queue(QueuedResponse::Chunks(vec![
        StreamChunk::TextDelta("recovered".into()),
        StreamChunk::Done {
            finish: FinishReason::Stop,
        },
    ]));
    let chunks = collect_ok(subject, &ChatRequest::user_text("again", 1_024)).await;
    assert_eq!(
        chunks.len(),
        2,
        "the subject must keep working after an error"
    );
}

/// A mid-stream failure is an `Err` *item* between `Ok` items — never a
/// silently truncated stream and never a hang (pitfall #9).
pub async fn in_stream_error_is_an_item_not_a_dropped_stream(subject: &impl ContractSubject) {
    subject.queue(QueuedResponse::StreamError {
        chunks: vec![StreamChunk::TextDelta("partial".into())],
        error: ModelError::Protocol("synthetic broken frame".into()),
    });

    let mut stream = subject
        .chat_stream(&ChatRequest::user_text("hi", 1_024))
        .await
        .expect("call succeeds; the failure is in-stream");
    let first = stream.next().await.expect("at least one Ok item");
    assert_eq!(
        first.expect("first item is Ok"),
        StreamChunk::TextDelta("partial".into())
    );
    let second = stream.next().await.expect("the error is an item");
    assert!(
        matches!(second, Err(ModelError::Protocol(_))),
        "expected an in-stream Protocol error, got: {second:?}"
    );
    // Drain: the stream must terminate (what follows the error is
    // adapter-specific — recovery or termination are both legal).
    while stream.next().await.is_some() {}
}

/// Dropping a stream mid-flight is the cancellation semantic: no panic, no
/// poisoned state — the next call streams its script unaffected.
pub async fn dropped_stream_does_not_poison_the_subject(subject: &impl ContractSubject) {
    let script = || {
        QueuedResponse::Chunks(vec![
            StreamChunk::TextDelta("one".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ])
    };
    subject.queue(script());
    subject.queue(script());

    let mut first = subject
        .chat_stream(&ChatRequest::user_text("hi", 1_024))
        .await
        .expect("first call");
    let _ = first.next().await;
    drop(first);

    let chunks = collect_ok(subject, &ChatRequest::user_text("hi", 1_024)).await;
    assert_eq!(chunks.len(), 2, "the queued response must stream in full");
}

/// Capability mismatches fail fast, before the wire: a request the declared
/// [`crate::Capabilities`] rule out must error with
/// [`ModelError::CapabilityMismatch`] without consuming the scripted response
/// of a subsequent legal request.
pub async fn capability_mismatch_fails_before_the_wire(subject: &impl ContractSubject) {
    let with_tools = ChatRequest {
        tools: vec![ToolSpec {
            name: "read_file".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type": "object"}),
        }],
        ..ChatRequest::user_text("hi", 1_024)
    };

    if subject.capabilities().tools {
        subject.queue(QueuedResponse::Chunks(vec![
            StreamChunk::TextDelta("ok".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ]));
        let chunks = collect_ok(subject, &with_tools).await;
        assert_eq!(
            chunks.first(),
            Some(&StreamChunk::TextDelta("ok".into())),
            "a tools-capable subject streams the scripted response"
        );
    } else {
        // The legal follow-up's script stays queued only if the failing call
        // never reached the wire.
        subject.queue(QueuedResponse::Chunks(vec![
            StreamChunk::TextDelta("untouched".into()),
            StreamChunk::Done {
                finish: FinishReason::Stop,
            },
        ]));
        let result = subject.chat_stream(&with_tools).await;
        let Err(error) = result else {
            panic!("tools request on a tools-less subject must fail");
        };
        assert!(
            matches!(error, ModelError::CapabilityMismatch(_)),
            "expected CapabilityMismatch, got: {error}"
        );
        let chunks = collect_ok(subject, &ChatRequest::user_text("hi", 1_024)).await;
        assert_eq!(
            chunks.first(),
            Some(&StreamChunk::TextDelta("untouched".into())),
            "the failed call must not have consumed the queued response"
        );
    }
}

/// Collects a stream that must be all-`Ok`, returning the chunks.
async fn collect_ok(subject: &impl ContractSubject, request: &ChatRequest) -> Vec<StreamChunk> {
    let stream = subject
        .chat_stream(request)
        .await
        .expect("scripted call must succeed");
    stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("scripted stream must be all-Ok")
}

// ============================================================================
// The client-protocol suite (ADR-0013 item 10): the attach semantics every
// protocol endpoint must honor, driven through a scriptable subject. Sync
// only — publish/attach are synchronous, the tail is a blocking iterator.
// ============================================================================

/// A client-protocol endpoint the suite scripts. One subject serves one run;
/// implementations are the in-process broadcaster, fakes, and later the
/// stdio transport.
pub trait ProtocolSubject {
    /// Feeds one live item, as the run's loop would publish it.
    fn publish(&self, item: &LiveItem);
    /// The attach handshake: a `Sync` baseline plus the live tail.
    fn attach(&self) -> Attachment;
    /// The per-subscriber queue bound — the lag case publishes past it.
    fn queue_capacity(&self) -> usize;
}

const SUITE_TRACE: &str = "tr-suite";

/// A recorded durable event as the loop publishes it (item seq == event seq).
fn recorded(seq: u64, kind: EventKind, turn: Option<u64>) -> LiveItem {
    let event = Event::new(
        seq,
        format!("e{seq}"),
        SUITE_TRACE.into(),
        format!("s{seq}"),
        Some("s0".into()),
        1_757_200_000_000 + seq,
        kind,
    );
    let event = match turn {
        Some(turn) => event.with_attribute(attrs::TURN, turn),
        None => event,
    };
    LiveItem {
        seq,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::Recorded {
            event: Box::new(event),
        },
    }
}

/// One assistant-stream delta of the open turn.
fn delta(seq: u64, turn: u32, chunk: StreamChunk) -> LiveItem {
    LiveItem {
        seq,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::AssistantDelta { turn, chunk },
    }
}

fn approval_request(
    seq: u64,
    request_id: &str,
    turn: u32,
    calls: Vec<crate::ToolCall>,
) -> LiveItem {
    LiveItem {
        seq,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::ApprovalRequested {
            request_id: request_id.into(),
            turn,
            calls,
            wait_timeout: std::time::Duration::from_secs(300),
        },
    }
}

fn start_run(seq: u64) -> LiveItem {
    recorded(
        seq,
        EventKind::Command(Command::StartRun {
            base: Box::new(ChatRequest::user_text("fix the typo", 4_096)),
            prefix: None,
        }),
        None,
    )
}

fn text_done(seq: u64, turn: u32, text: &str) -> LiveItem {
    response(
        seq,
        turn,
        crate::Message::text(crate::Role::Assistant, text),
        FinishReason::Stop,
    )
}

fn response(seq: u64, turn: u32, message: crate::Message, finish: FinishReason) -> LiveItem {
    recorded(
        seq,
        EventKind::LlmResponse {
            message,
            usage: None,
            finish,
            outcome: crate::TurnOutcome::Content,
            warnings: Vec::new(),
        },
        Some(u64::from(turn)),
    )
}

/// Drains `n` updates off the tail, asserting the drop-rule shape: every
/// item's seq strictly above `as_of_seq` and strictly increasing.
fn take_items(
    tail: &mut dyn Iterator<Item = LiveUpdate>,
    n: usize,
    as_of_seq: u64,
) -> Vec<LiveItem> {
    let mut items = Vec::new();
    let mut previous = as_of_seq;
    for update in tail.take(n) {
        let LiveUpdate::Item { item } = update else {
            panic!("expected an item, got the lag marker");
        };
        assert!(item.seq > as_of_seq, "the tail must not replay ≤ as_of_seq");
        assert!(item.seq > previous, "the tail is strictly ordered");
        previous = item.seq;
        items.push(*item);
    }
    assert_eq!(items.len(), n, "the tail ended early");
    items
}

/// The degenerate attach (item 3's position 0): an empty baseline, then
/// every published item arrives on the tail in order.
pub fn attach_at_zero_tails_every_item_in_order(subject: &impl ProtocolSubject) {
    let attachment = subject.attach();
    assert_eq!(attachment.sync.as_of_seq, 0);
    assert!(attachment.sync.history.messages.is_empty());
    assert_eq!(attachment.sync.in_flight, InFlight::default());

    let script = vec![
        start_run(1),
        recorded(2, EventKind::LlmRequest { trailer: None }, Some(1)),
        delta(3, 1, StreamChunk::TextDelta("hel".into())),
        delta(4, 1, StreamChunk::TextDelta("lo".into())),
        text_done(5, 1, "hello"),
    ];
    for item in &script {
        subject.publish(item);
    }
    let mut tail = attachment.tail;
    let received = take_items(&mut *tail, script.len(), attachment.sync.as_of_seq);
    assert_eq!(received, script, "the tail replays the script verbatim");
}

/// Mid-turn attach (the field pain of item 3): the baseline carries the log
/// fold AND the open turn's partial text, and the tail continues strictly
/// past `as_of_seq` — no gap, no duplicate, no jump when the response lands.
pub fn mid_run_attach_syncs_history_and_in_flight(subject: &impl ProtocolSubject) {
    subject.publish(&start_run(1));
    subject.publish(&recorded(
        2,
        EventKind::LlmRequest { trailer: None },
        Some(1),
    ));
    subject.publish(&delta(3, 1, StreamChunk::TextDelta("partial".into())));

    let attachment = subject.attach();
    assert_eq!(attachment.sync.as_of_seq, 3);
    assert_eq!(
        attachment.sync.history.messages.len(),
        1,
        "the start-run seed"
    );
    let open = attachment
        .sync
        .in_flight
        .open_turn
        .as_ref()
        .expect("a turn is in flight");
    assert_eq!(open.turn, 1);
    assert_eq!(open.partial.text, "partial");

    subject.publish(&delta(4, 1, StreamChunk::TextDelta(" tail".into())));
    subject.publish(&text_done(5, 1, "partial tail"));
    let mut tail = attachment.tail;
    let received = take_items(&mut *tail, 2, attachment.sync.as_of_seq);
    assert_eq!(received[0].seq, 4);
    assert_eq!(received[1].seq, 5);
}

/// An attach during an approval wait renders the dialog immediately (item
/// 3); the recorded resolve clears the pending request for later attaches.
pub fn attach_during_approval_wait_shows_the_pending_request(subject: &impl ProtocolSubject) {
    subject.publish(&start_run(1));
    subject.publish(&recorded(
        2,
        EventKind::LlmRequest { trailer: None },
        Some(1),
    ));
    subject.publish(&LiveItem {
        seq: 3,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::ApprovalRequested {
            request_id: "ap9".into(),
            turn: 1,
            calls: vec![crate::ToolCall {
                id: "call_1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            }],
            wait_timeout: std::time::Duration::from_secs(300),
        },
    });

    let waiting = subject.attach();
    let pending = &waiting.sync.in_flight.pending_approvals;
    assert_eq!(pending.len(), 1, "the open request reconstructs");
    assert_eq!(pending[0].request_id, "ap9");
    assert_eq!(pending[0].calls[0].name, "write_file");
    // The dialog's deadline rides the request: an attach mid-wait
    // reconstructs the same header the live path renders.
    assert_eq!(pending[0].wait_timeout, std::time::Duration::from_secs(300));

    subject.publish(&recorded(
        4,
        EventKind::Command(Command::ResolveApproval {
            command_id: "cmd-1".into(),
            request_id: "ap9".into(),
            decisions: vec![crate::Approval::Approved],
        }),
        Some(1),
    ));
    let mut tail = waiting.tail;
    let received = take_items(&mut *tail, 1, waiting.sync.as_of_seq);
    assert_eq!(received[0].seq, 4, "the resolve arrives on the live tail");

    let after = subject.attach();
    assert!(
        after.sync.in_flight.pending_approvals.is_empty(),
        "the recorded resolve settles the request"
    );
}

/// Per-call approval state remains attached to the original request while
/// siblings are answered: positions, rather than provider call ids, address
/// decisions, and the first recorded decision wins.
pub fn partial_approval_syncs_and_settles_in_original_order(subject: &impl ProtocolSubject) {
    use crate::Approval::{Approved, Rejected};

    let calls = ["one", "two", "three"]
        .map(|path| crate::ToolCall {
            id: "duplicate-call-id".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path": path}),
        })
        .to_vec();
    let resolve = |seq, command_id: &str, call_index, decision| {
        recorded(
            seq,
            EventKind::Command(Command::ResolveApprovalCall {
                command_id: command_id.into(),
                request_id: "ap-partial".into(),
                call_index,
                decision,
            }),
            Some(1),
        )
    };
    let rejected = Rejected {
        comment: Some("keep the second file".into()),
    };
    subject.publish(&start_run(1));
    subject.publish(&approval_request(2, "ap-partial", 1, calls.clone()));
    subject.publish(&resolve(3, "cmd-middle", 1, rejected.clone()));

    let partial = subject.attach();
    assert_eq!(partial.sync.as_of_seq, 3);
    assert!(partial.sync.settled_approvals.is_empty());
    let pending = &partial.sync.in_flight.pending_approvals;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, "ap-partial");
    assert_eq!(pending[0].turn, 1);
    assert_eq!(
        pending[0].message_index, None,
        "no observed response to anchor"
    );
    assert_eq!(pending[0].wait_timeout, std::time::Duration::from_secs(300));
    assert_eq!(
        pending[0].calls, calls,
        "the full original batch remains visible"
    );
    assert_eq!(
        pending[0].decisions,
        vec![None, Some(rejected.clone()), None]
    );

    let script = [
        resolve(4, "cmd-last", 2, Approved),
        resolve(5, "cmd-duplicate", 1, Approved),
        resolve(6, "cmd-first", 0, Approved),
        resolve(7, "cmd-first", 0, Approved),
        resolve(8, "cmd-late-duplicate", 0, Rejected { comment: None }),
    ];
    for item in &script[..2] {
        subject.publish(item);
    }
    let still_partial = subject.attach();
    assert_eq!(still_partial.sync.as_of_seq, 5);
    assert!(still_partial.sync.settled_approvals.is_empty());
    let pending = &still_partial.sync.in_flight.pending_approvals;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].calls, calls);
    assert_eq!(
        pending[0].decisions,
        vec![None, Some(rejected.clone()), Some(Approved)],
        "a different command id cannot replace a recorded decision"
    );
    for item in &script[2..] {
        subject.publish(item);
    }
    let mut tail = partial.tail;
    assert_eq!(
        take_items(&mut *tail, script.len(), partial.sync.as_of_seq),
        script
    );

    let settled = subject.attach();
    assert_eq!(settled.sync.as_of_seq, 8);
    assert!(settled.sync.in_flight.pending_approvals.is_empty());
    assert_eq!(
        settled.sync.settled_approvals.len(),
        1,
        "retries settle only once"
    );
    assert_eq!(settled.sync.settled_approvals[0].request_id, "ap-partial");
    assert_eq!(settled.sync.settled_approvals[0].message_index, None);
    assert_eq!(settled.sync.settled_approvals[0].calls, calls);
    assert_eq!(
        settled.sync.settled_approvals[0].decisions,
        vec![Approved, rejected, Approved],
        "settlement preserves full original call order"
    );
}

/// A batch that settled before the attach replays in the sync baseline
/// (item 3): the request was live-only, so the aggregator pairs the
/// recorded resolve with the calls it saw presented — the rebuilt
/// transcript renders the explicit approve/reject lines, not only their
/// consequences.
pub fn attach_after_a_settled_approval_replays_it(subject: &impl ProtocolSubject) {
    subject.publish(&start_run(1));
    subject.publish(&recorded(
        2,
        EventKind::LlmRequest { trailer: None },
        Some(1),
    ));
    subject.publish(&LiveItem {
        seq: 3,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::ApprovalRequested {
            request_id: "ap9".into(),
            turn: 1,
            calls: vec![crate::ToolCall {
                id: "call_1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            }],
            wait_timeout: std::time::Duration::from_secs(300),
        },
    });
    subject.publish(&recorded(
        4,
        EventKind::Command(Command::ResolveApproval {
            command_id: "cmd-1".into(),
            request_id: "ap9".into(),
            decisions: vec![crate::Approval::Approved],
        }),
        Some(1),
    ));

    let attached = subject.attach();
    assert!(
        attached.sync.in_flight.pending_approvals.is_empty(),
        "the settle clears the pending queue"
    );
    let settled = &attached.sync.settled_approvals;
    assert_eq!(settled.len(), 1, "the settled batch rides the baseline");
    assert_eq!(settled[0].request_id, "ap9");
    assert_eq!(settled[0].calls[0].name, "write_file");
    assert_eq!(settled[0].decisions, vec![crate::Approval::Approved]);
}

/// Approval anchors address the actual history, including seeded messages
/// and every message-producing event. Repeated calls cannot identify a turn.
pub fn approval_anchors_follow_seeded_history_across_turns(subject: &impl ProtocolSubject) {
    let call = crate::ToolCall {
        id: "repeated-id".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({"path": "same-file"}),
    };
    let mut message = crate::Message::text(crate::Role::Assistant, "same proposal");
    message
        .content
        .push(crate::ContentPart::ToolCall { call: call.clone() });
    let mut base = ChatRequest::user_text("try again", 4_096);
    base.messages.extend([
        message.clone(),
        crate::Message::tool_result(call.id.clone(), serde_json::json!("old result")),
    ]);
    subject.publish(&recorded(
        1,
        EventKind::Command(Command::StartRun {
            base: Box::new(base),
            prefix: None,
        }),
        None,
    ));
    for (turn, seq, index) in [(1, 2, 3), (2, 10, 7)] {
        subject.publish(&response(
            seq + 1,
            u32::try_from(turn).expect("small turn"),
            message.clone(),
            FinishReason::ToolCalls,
        ));
        let request_id = format!("ap-{turn}");
        subject.publish(&approval_request(
            seq + 2,
            &request_id,
            u32::try_from(turn).expect("small turn"),
            vec![call.clone()],
        ));
        let pending = subject.attach().sync;
        assert_eq!(pending.in_flight.pending_approvals.len(), 1);
        assert_eq!(
            pending.in_flight.pending_approvals[0].message_index,
            Some(index)
        );
        assert_eq!(pending.history.messages.len(), index + 1);
        assert_eq!(pending.history.messages[index], message);
        let command = if turn == 1 {
            Command::ResolveApproval {
                command_id: "cmd-batch".into(),
                request_id,
                decisions: vec![crate::Approval::Approved],
            }
        } else {
            Command::ResolveApprovalCall {
                command_id: "cmd-call".into(),
                request_id,
                call_index: 0,
                decision: crate::Approval::Approved,
            }
        };
        subject.publish(&recorded(seq + 3, EventKind::Command(command), Some(turn)));
        if turn == 1 {
            for item in [
                recorded(
                    6,
                    EventKind::InstructionInjected {
                        path: "nested/AGENTS.md".into(),
                        content: "instructions".into(),
                    },
                    Some(turn),
                ),
                recorded(7, EventKind::ToolCall { call: call.clone() }, Some(turn)),
                recorded(
                    8,
                    EventKind::ToolResult {
                        call_id: call.id.clone(),
                        result: serde_json::json!("done"),
                    },
                    Some(turn),
                ),
                recorded(
                    9,
                    EventKind::Command(Command::Steer {
                        command_id: "cmd-steer".into(),
                        text: "again".into(),
                        mode: crate::SteerMode::Inject,
                    }),
                    Some(turn),
                ),
            ] {
                subject.publish(&item);
            }
        }
    }
    let settled = subject.attach().sync;
    assert!(settled.in_flight.pending_approvals.is_empty());
    assert_eq!(settled.settled_approvals.len(), 2);
    assert_eq!(settled.settled_approvals[0].message_index, Some(3));
    assert_eq!(settled.settled_approvals[1].message_index, Some(7));
}

/// An out-of-order completion is provisional, not another history message.
/// Duplicate provider ids remain distinct until their own durable spans land.
pub fn buffered_tool_completions_sync_and_reconcile_by_span(subject: &impl ProtocolSubject) {
    subject.publish(&start_run(1));
    subject.publish(&text_done(2, 1, "tools requested"));
    let later = crate::ToolCompletion {
        span_id: "tool-later".into(),
        turn: 1,
        message_call_index: 2,
        call_id: "duplicate-id".into(),
        name: "write_file".into(),
        result: serde_json::json!({"written": true}),
        error: None,
    };
    let failed = crate::ToolCompletion {
        span_id: "tool-failed".into(),
        message_call_index: 1,
        result: serde_json::json!("write failed"),
        error: Some(crate::EventError {
            kind: crate::error_kinds::TOOL.into(),
            message: "write failed".into(),
        }),
        ..later.clone()
    };
    let completed = |seq, completion: &crate::ToolCompletion| LiveItem {
        seq,
        trace_id: SUITE_TRACE.into(),
        kind: LiveKind::ToolCompleted {
            completion: completion.clone(),
        },
    };
    let result = |seq, completion: &crate::ToolCompletion| {
        let mut item = recorded(
            seq,
            EventKind::ToolResult {
                call_id: completion.call_id.clone(),
                result: completion.result.clone(),
            },
            Some(u64::from(completion.turn)),
        );
        let LiveKind::Recorded { event } = &mut item.kind else {
            unreachable!()
        };
        event.span_id.clone_from(&completion.span_id);
        if let Some(error) = &completion.error {
            event.status = crate::Status::Error;
            event.error = Some(error.clone());
        }
        item
    };
    subject.publish(&completed(3, &later));
    subject.publish(&completed(4, &failed));
    let attached = subject.attach();
    assert_eq!(attached.sync.as_of_seq, 4);
    assert_eq!(attached.sync.history.messages.len(), 2);
    assert_eq!(
        attached.sync.in_flight.completed_tools,
        vec![later.clone(), failed.clone()]
    );
    let other = crate::ToolCompletion {
        span_id: "tool-first".into(),
        message_call_index: 0,
        ..later.clone()
    };
    let script = [
        completed(5, &later),
        result(6, &other),
        result(7, &later),
        result(8, &failed),
    ];
    for item in &script[..2] {
        subject.publish(item);
    }
    let unmatched = subject.attach().sync;
    assert_eq!(unmatched.history.messages.len(), 3);
    assert_eq!(
        unmatched.in_flight.completed_tools,
        vec![later, failed.clone()],
        "same provider id on another span does not retire an outcome"
    );
    subject.publish(&script[2]);
    let reconciled = subject.attach().sync;
    assert_eq!(reconciled.history.messages.len(), 4);
    assert_eq!(reconciled.in_flight.completed_tools, vec![failed]);
    subject.publish(&script[3]);
    let done = subject.attach().sync;
    assert!(done.in_flight.completed_tools.is_empty());
    assert_eq!(done.history.messages.len(), 5);
    assert!(done.history.messages[4].is_error);
    let mut tail = attached.tail;
    assert_eq!(
        take_items(&mut *tail, script.len(), attached.sync.as_of_seq),
        script
    );
}

/// Lag recovery (item 5): a subscriber that falls behind learns it via the
/// lag marker and re-attaches for a complete fresh baseline — the in-flight
/// replica folds deltas the lagging client never saw.
pub fn a_lagging_subscriber_is_told_to_resync(subject: &impl ProtocolSubject) {
    let attachment = subject.attach();
    let capacity = subject.queue_capacity();
    let total = capacity + 2;
    for index in 1..=total {
        subject.publish(&delta(
            u64::try_from(index).expect("small index"),
            1,
            StreamChunk::TextDelta("x".into()),
        ));
    }

    let mut tail = attachment.tail;
    let received = take_items(&mut *tail, capacity, 0);
    assert_eq!(received.len(), capacity, "the queue bound holds");
    assert_eq!(
        tail.next(),
        Some(LiveUpdate::Lagged),
        "a gap is signalled, never silent"
    );
    assert_eq!(tail.next(), None, "the subscription ends after the gap");

    let fresh = subject.attach();
    assert_eq!(fresh.sync.as_of_seq, u64::try_from(total).expect("small"));
    let open = fresh
        .sync
        .in_flight
        .open_turn
        .as_ref()
        .expect("the turn is still open");
    assert_eq!(open.partial.text, "x".repeat(total));
}

/// Turn close reconciles the delta buffer with the durable record (item 5):
/// after the response lands, a fresh attach sees no open turn and the
/// completed message in the fold.
pub fn turn_close_clears_the_in_flight_turn(subject: &impl ProtocolSubject) {
    subject.publish(&start_run(1));
    subject.publish(&recorded(
        2,
        EventKind::LlmRequest { trailer: None },
        Some(1),
    ));
    subject.publish(&delta(3, 1, StreamChunk::TextDelta("do".into())));
    subject.publish(&delta(4, 1, StreamChunk::TextDelta("ne".into())));
    subject.publish(&text_done(5, 1, "done"));

    let attachment = subject.attach();
    assert!(attachment.sync.in_flight.open_turn.is_none());
    assert_eq!(attachment.sync.history.turns, 1);
    assert_eq!(attachment.sync.history.messages.len(), 2);
}

/// The canonical scripted run behind the serialized-`Sync` snapshot locks
/// (ADR-0013's consequence: "insta mechanical gates extend to serialized
/// Sync payloads"). Subjects publish it verbatim; the snapshot asserts the
/// attach output, not the script.
#[must_use]
pub fn fixed_sync_script() -> Vec<LiveItem> {
    vec![
        start_run(1),
        recorded(2, EventKind::LlmRequest { trailer: None }, Some(1)),
        delta(3, 1, StreamChunk::TextDelta("partial".into())),
        delta(
            4,
            1,
            StreamChunk::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "read_file".into(),
            },
        ),
        delta(
            5,
            1,
            StreamChunk::ToolArgsDelta {
                index: 0,
                fragment: "{\"path\":\"src/".into(),
            },
        ),
    ]
}

/// Instantiates the client-protocol suite as one `#[test]` per case
/// (ADR-0013 item 10), the protocol-side mirror of
/// [`provider_contract_tests!`](crate::provider_contract_tests). The factory
/// produces one fresh subject per case; the subject trait implementation
/// lives with the consuming crate's tests, not its public surface.
///
/// ```ignore
/// cadmus_contract::client_protocol_tests!(Broadcaster::new);
/// ```
#[macro_export]
macro_rules! client_protocol_tests {
    ($factory:expr) => {
        mod client_protocol {
            use super::*;

            #[test]
            fn attach_at_zero_tails_every_item_in_order() {
                let subject = ($factory)();
                $crate::testing::attach_at_zero_tails_every_item_in_order(&subject);
            }

            #[test]
            fn mid_run_attach_syncs_history_and_in_flight() {
                let subject = ($factory)();
                $crate::testing::mid_run_attach_syncs_history_and_in_flight(&subject);
            }

            #[test]
            fn attach_during_approval_wait_shows_the_pending_request() {
                let subject = ($factory)();
                $crate::testing::attach_during_approval_wait_shows_the_pending_request(&subject);
            }

            #[test]
            fn partial_approval_syncs_and_settles_in_original_order() {
                let subject = ($factory)();
                $crate::testing::partial_approval_syncs_and_settles_in_original_order(&subject);
            }

            #[test]
            fn attach_after_a_settled_approval_replays_it() {
                let subject = ($factory)();
                $crate::testing::attach_after_a_settled_approval_replays_it(&subject);
            }

            #[test]
            fn approval_anchors_follow_seeded_history_across_turns() {
                let subject = ($factory)();
                $crate::testing::approval_anchors_follow_seeded_history_across_turns(&subject);
            }

            #[test]
            fn buffered_tool_completions_sync_and_reconcile_by_span() {
                let subject = ($factory)();
                $crate::testing::buffered_tool_completions_sync_and_reconcile_by_span(&subject);
            }

            #[test]
            fn a_lagging_subscriber_is_told_to_resync() {
                let subject = ($factory)();
                $crate::testing::a_lagging_subscriber_is_told_to_resync(&subject);
            }

            #[test]
            fn turn_close_clears_the_in_flight_turn() {
                let subject = ($factory)();
                $crate::testing::turn_close_clears_the_in_flight_turn(&subject);
            }
        }
    };
}
/// Instantiates the provider suite as one `#[test]` per case, each driving
/// the factory on its own current-thread runtime (with a 30-second watchdog
/// so a broken adapter fails instead of hanging CI).
///
/// The consuming crate must have `tokio` with the `rt` and `time` features
/// as a dev-dependency.
///
/// ```ignore
/// cadmus_contract::provider_contract_tests!(|| ReplayProvider::new(Vec::new()));
/// ```
#[macro_export]
macro_rules! provider_contract_tests {
    ($factory:expr) => {
        mod provider_contract {
            use super::*;

            fn block_on<F: ::core::future::Future<Output = ()>>(case: &str, future: F) {
                let runtime = ::tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("contract test runtime");
                runtime.block_on(async move {
                    ::tokio::time::timeout(::core::time::Duration::from_secs(30), future)
                        .await
                        .unwrap_or_else(|_| panic!("contract case `{case}` timed out"));
                });
            }

            #[test]
            fn text_answer_streams_verbatim() {
                block_on("text_answer_streams_verbatim", async {
                    let subject = ($factory)();
                    $crate::testing::text_answer_streams_verbatim(&subject).await;
                });
            }

            #[test]
            fn parallel_tool_stream_keeps_index_discipline() {
                block_on("parallel_tool_stream_keeps_index_discipline", async {
                    let subject = ($factory)();
                    $crate::testing::parallel_tool_stream_keeps_index_discipline(&subject).await;
                });
            }

            #[test]
            fn call_error_surfaces_then_recovers() {
                block_on("call_error_surfaces_then_recovers", async {
                    let subject = ($factory)();
                    $crate::testing::call_error_surfaces_then_recovers(&subject).await;
                });
            }

            #[test]
            fn in_stream_error_is_an_item_not_a_dropped_stream() {
                block_on("in_stream_error_is_an_item_not_a_dropped_stream", async {
                    let subject = ($factory)();
                    $crate::testing::in_stream_error_is_an_item_not_a_dropped_stream(&subject)
                        .await;
                });
            }

            #[test]
            fn dropped_stream_does_not_poison_the_subject() {
                block_on("dropped_stream_does_not_poison_the_subject", async {
                    let subject = ($factory)();
                    $crate::testing::dropped_stream_does_not_poison_the_subject(&subject).await;
                });
            }

            #[test]
            fn capability_mismatch_fails_before_the_wire() {
                block_on("capability_mismatch_fails_before_the_wire", async {
                    let subject = ($factory)();
                    $crate::testing::capability_mismatch_fails_before_the_wire(&subject).await;
                });
            }
        }
    };
}
