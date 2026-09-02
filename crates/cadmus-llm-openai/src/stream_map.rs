//! Stateful genai-event → contract-chunk mapping, one instance per stream.
//!
//! genai emits a `ToolCallChunk` per wire delta carrying the *accumulated* call
//! (id/name stable, arguments merged so far); this mapper re-derives honest
//! increments by suffix-diffing, so cadmus-core's assembler is exercised in
//! production exactly as in the replay tests (aggregation semantics stay in
//! the core, report §4.1.3).

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use cadmus_contract::{FinishReason, ModelError, StreamChunk};
use genai::chat::ChatStreamEvent;
use serde_json::json;
use tokio_stream::Stream;

use crate::error::map_genai_error;
use crate::map::{map_stop_reason, map_usage};

struct OpenCall {
    id: String,
    sent_args_len: usize,
}

#[derive(Default)]
pub struct StreamMapper {
    /// Synthesized index space: position in this vec is the contract `index`,
    /// assigned in order of first appearance (the adapter normalizes wire
    /// indexes/ids into one space, per the contract).
    calls: Vec<OpenCall>,
}

impl StreamMapper {
    pub fn map_event(&mut self, event: ChatStreamEvent) -> Vec<StreamChunk> {
        match event {
            ChatStreamEvent::Start => vec![],
            ChatStreamEvent::Chunk(chunk) => vec![StreamChunk::TextDelta(chunk.content)],
            ChatStreamEvent::ReasoningChunk(chunk) => {
                vec![StreamChunk::ReasoningDelta(chunk.content)]
            }
            ChatStreamEvent::ThoughtSignatureChunk(chunk) => vec![StreamChunk::OpaqueDelta(
                json!({ "thought_signature": chunk.content }),
            )],
            ChatStreamEvent::ToolCallChunk(chunk) => self.map_tool_call(&chunk.tool_call),
            ChatStreamEvent::End(end) => self.map_end(&end),
        }
    }

    fn map_tool_call(&mut self, call: &genai::chat::ToolCall) -> Vec<StreamChunk> {
        let args = accumulated_args(call);
        if let Some(index) = self.calls.iter().position(|open| open.id == call.call_id) {
            // Nothing new accumulated is a valid no-op delta.
            suffix_diff(&args, &mut self.calls[index].sent_args_len).map_or_else(
                Vec::new,
                |fragment| {
                    vec![StreamChunk::ToolArgsDelta {
                        index: u32::try_from(index).expect("tool call index"),
                        fragment,
                    }]
                },
            )
        } else {
            self.calls.push(OpenCall {
                id: call.call_id.clone(),
                sent_args_len: args.len(),
            });
            let index = u32::try_from(self.calls.len() - 1).expect("tool call index");
            let mut chunks = vec![StreamChunk::ToolCallStart {
                index,
                id: call.call_id.clone(),
                name: call.fn_name.clone(),
            }];
            if !args.is_empty() {
                chunks.push(StreamChunk::ToolArgsDelta {
                    index,
                    fragment: args,
                });
            }
            chunks
        }
    }

    fn map_end(&mut self, end: &genai::chat::StreamEnd) -> Vec<StreamChunk> {
        let mut chunks: Vec<StreamChunk> = Vec::new();
        for index in 0..self.calls.len() {
            chunks.push(StreamChunk::ToolCallEnd {
                index: u32::try_from(index).expect("tool call index"),
            });
        }
        self.calls.clear();
        if let Some(usage) = &end.captured_usage {
            chunks.push(StreamChunk::Usage(map_usage(usage)));
        }
        chunks.push(StreamChunk::Done {
            finish: end
                .captured_stop_reason
                .as_ref()
                .map_or(FinishReason::Other("unspecified".into()), map_stop_reason),
        });
        chunks
    }
}

fn accumulated_args(call: &genai::chat::ToolCall) -> String {
    match &call.fn_arguments {
        serde_json::Value::String(args) => args.clone(),
        other => other.to_string(),
    }
}

/// The accumulated argument string only ever grows; a shrink means the
/// provider violated accumulation semantics — stay lossless but loud.
fn suffix_diff(args: &str, sent: &mut usize) -> Option<String> {
    if args.len() < *sent {
        tracing::warn!("tool call arguments shrank between deltas; resending nothing");
        return None;
    }
    if args.len() == *sent {
        return None;
    }
    let fragment = args[*sent..].to_string();
    *sent = args.len();
    Some(fragment)
}

/// The genai stream mapped onto contract chunks, buffering multi-chunk
/// events (a single `End` fans out to `ToolCallEnd`/`Usage`/`Done`). Hand-rolled
/// instead of `flat_map` so the crate stays on tokio-stream only.
pub struct MappedStream {
    inner: genai::chat::ChatStream,
    mapper: StreamMapper,
    pending: VecDeque<Result<StreamChunk, ModelError>>,
}

impl MappedStream {
    pub fn new(inner: genai::chat::ChatStream) -> Self {
        Self {
            inner,
            mapper: StreamMapper::default(),
            pending: VecDeque::new(),
        }
    }
}

impl Stream for MappedStream {
    type Item = Result<StreamChunk, ModelError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        loop {
            if let Some(item) = this.pending.pop_front() {
                return Poll::Ready(Some(item));
            }
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    let mapped = this.mapper.map_event(event);
                    this.pending.extend(mapped.into_iter().map(Ok));
                }
                // A wire error becomes an in-stream `Err` item, not a dropped
                // stream (pitfall #9).
                Poll::Ready(Some(Err(error))) => {
                    return Poll::Ready(Some(Err(map_genai_error(error))));
                }
                Poll::Ready(None) => return Poll::Ready(this.pending.pop_front()),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadmus_contract::Usage;
    use genai::chat::{ChatStreamEvent, StreamChunk as GenaiChunk, StreamEnd, ToolCall, ToolChunk};

    fn tool_call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            call_id: id.into(),
            fn_name: name.into(),
            fn_arguments: serde_json::Value::String(args.into()),
            thought_signatures: None,
        }
    }

    fn chunk_event(id: &str, name: &str, args: &str) -> ChatStreamEvent {
        ChatStreamEvent::ToolCallChunk(ToolChunk {
            tool_call: tool_call(id, name, args),
        })
    }

    // Pitfall #1 at the dialect seam: two parallel calls interleave as
    // accumulated snapshots; the mapper must emit honest per-call increments.
    #[test]
    fn maps_interleaved_accumulated_tool_calls() {
        let mut mapper = StreamMapper::default();
        let mut chunks = Vec::new();
        for event in [
            chunk_event("call_a", "read_file", "{\"path\":\"/a"),
            chunk_event("call_b", "grep", "{\"pattern\":\"fn"),
            chunk_event("call_a", "read_file", "{\"path\":\"/a\"}"),
            chunk_event("call_b", "grep", "{\"pattern\":\"fn main\"}"),
            ChatStreamEvent::End(StreamEnd::default()),
        ] {
            chunks.extend(mapper.map_event(event));
        }
        assert_eq!(
            chunks,
            vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "call_a".into(),
                    name: "read_file".into(),
                },
                StreamChunk::ToolArgsDelta {
                    index: 0,
                    fragment: "{\"path\":\"/a".into(),
                },
                StreamChunk::ToolCallStart {
                    index: 1,
                    id: "call_b".into(),
                    name: "grep".into(),
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
                    finish: FinishReason::Other("unspecified".into()),
                },
            ]
        );
    }

    // Dual mode: a one-shot call (llama.cpp style) is start + full args + end.
    #[test]
    fn maps_one_shot_tool_call() {
        let mut mapper = StreamMapper::default();
        let mut chunks = mapper.map_event(chunk_event("call_0", "noop", "{\"x\":1}"));
        chunks.extend(mapper.map_event(ChatStreamEvent::End(StreamEnd::default())));
        assert_eq!(
            chunks,
            vec![
                StreamChunk::ToolCallStart {
                    index: 0,
                    id: "call_0".into(),
                    name: "noop".into(),
                },
                StreamChunk::ToolArgsDelta {
                    index: 0,
                    fragment: "{\"x\":1}".into(),
                },
                StreamChunk::ToolCallEnd { index: 0 },
                StreamChunk::Done {
                    finish: FinishReason::Other("unspecified".into()),
                },
            ]
        );
    }

    #[test]
    fn maps_usage_and_stop_reason_at_end() {
        let mut mapper = StreamMapper::default();
        let end = StreamEnd {
            captured_usage: Some(genai::chat::Usage {
                prompt_tokens: Some(10),
                completion_tokens: Some(4),
                ..genai::chat::Usage::default()
            }),
            captured_stop_reason: Some(genai::chat::StopReason::MaxTokens("length".into())),
            ..StreamEnd::default()
        };
        let chunks = mapper.map_event(ChatStreamEvent::End(end));
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            &chunks[0],
            StreamChunk::Usage(Usage {
                input: 10,
                output: 4,
                ..
            })
        ));
        assert_eq!(
            chunks[1],
            StreamChunk::Done {
                finish: FinishReason::Length
            }
        );
    }

    #[test]
    fn maps_text_reasoning_and_thought_signature() {
        let mut mapper = StreamMapper::default();
        assert_eq!(
            mapper.map_event(ChatStreamEvent::Chunk(GenaiChunk {
                content: "hi".into()
            })),
            vec![StreamChunk::TextDelta("hi".into())]
        );
        assert_eq!(
            mapper.map_event(ChatStreamEvent::ReasoningChunk(GenaiChunk {
                content: "hmm".into()
            })),
            vec![StreamChunk::ReasoningDelta("hmm".into())]
        );
        assert_eq!(
            mapper.map_event(ChatStreamEvent::ThoughtSignatureChunk(GenaiChunk {
                content: "sig".into()
            })),
            vec![StreamChunk::OpaqueDelta(
                json!({ "thought_signature": "sig" })
            )]
        );
    }
}
