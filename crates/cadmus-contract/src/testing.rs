//! The provider contract test suite (report §9.2.1): the single authoritative
//! implementation of the [`Provider`] port's semantics. Every adapter wires in
//! with a one-line [`provider_contract_tests!`](crate::provider_contract_tests)
//! invocation; new adapters get the full port coverage for free, and the
//! suite's authoritative location stays unique.
//!
//! The suite never makes live calls — subjects are scripted: the replay fake
//! natively, network adapters through a local recorded-replay stub. Every
//! assertion is a port semantic; no adapter-private API appears here.

use std::collections::HashSet;

use tokio_stream::StreamExt;

use crate::{ChatRequest, FinishReason, ModelError, Provider, StreamChunk, ToolSpec};

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

/// Instantiates the suite as one `#[test]` per case, each driving the factory
/// on its own current-thread runtime (with a 30-second watchdog so a broken
/// adapter fails instead of hanging CI).
///
/// The consuming crate must have `tokio` with the `rt` and `time` features as
/// a dev-dependency.
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
