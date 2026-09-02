//! Contract-script → `OpenAI`-wire translation plus the stub-backed provider
//! newtype. The translator emits the common `OpenAI`-compatible SSE shape
//! (one tool-call delta per event, usage riding the finishing chunk,
//! `data: [DONE]` terminator), so the semantic contract suite can drive any
//! dialect through the real genai parsing path.
//!
//! Shared across integration-test targets, each of which compiles this module
//! independently and uses a different subset — hence the dead-code allowance.
#![allow(dead_code)]

use async_trait::async_trait;
use cadmus_contract::{
    Capabilities, ChatRequest, ChunkStream, ContractSubject, FinishReason, ModelError, OutputMode,
    Provider, QueuedResponse, Reasoning, StreamChunk, Usage,
};
use cadmus_llm_openai::{Dialect, OpenAiProvider};
use genai::chat::{ChatOptions, ChatResponseFormat, ReasoningEffort};
use genai::resolver::{AuthData, Endpoint};
use serde_json::{Value, json};

use crate::support::stub::{SseStub, StubReply};

/// Translates a well-formed chunk script into an SSE body. `ToolCallEnd` has
/// no wire event (calls end implicitly); `Usage` and `Done` fold into the
/// terminating events.
pub fn to_sse_body(chunks: &[StreamChunk]) -> String {
    let mut events: Vec<Value> = chunks.iter().filter_map(chunk_event).collect();

    let usage = chunks.iter().find_map(|chunk| match chunk {
        StreamChunk::Usage(usage) => Some(usage),
        _ => None,
    });
    let finish = chunks.iter().find_map(|chunk| match chunk {
        StreamChunk::Done { finish } => Some(finish),
        _ => None,
    });
    match (finish, usage) {
        (Some(finish), usage) => {
            let mut event =
                json!({"choices": [{"delta": {}, "finish_reason": wire_finish(finish)}]});
            if let Some(usage) = usage {
                event["usage"] = wire_usage(usage);
            }
            events.push(event);
        }
        // The second common shape: a trailing usage-only chunk.
        (None, Some(usage)) => events.push(json!({"choices": [], "usage": wire_usage(usage)})),
        (None, None) => {}
    }

    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

/// The in-stream error shape: scripted chunks, then a provider error event,
/// then termination (pitfall #9 — the error must surface as a stream item).
pub fn to_sse_error_body(chunks: &[StreamChunk], error: &ModelError) -> String {
    let mut body = String::new();
    for event in chunks.iter().filter_map(chunk_event) {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: ");
    body.push_str(
        &json!({"error": {"message": error.to_string(), "type": "server_error"}}).to_string(),
    );
    body.push_str("\n\ndata: [DONE]\n\n");
    body
}

/// The call-level error reply matching a [`ModelError`] variant.
pub fn call_error_reply(error: &ModelError) -> StubReply {
    let body = |message: &str, kind: &str| {
        json!({"error": {"message": message, "type": kind}}).to_string()
    };
    match error {
        ModelError::RateLimited { .. } => {
            StubReply::json(429, body("rate limited", "rate_limit_error"))
        }
        ModelError::Auth(message) => StubReply::json(401, body(message, "authentication_error")),
        ModelError::Server { status, .. } => {
            StubReply::json(*status, body("server error", "server_error"))
        }
        ModelError::ContextLength => StubReply::json(
            400,
            body("maximum context length exceeded", "invalid_request_error"),
        ),
        ModelError::Protocol(message)
        | ModelError::InvalidRequest(message)
        | ModelError::CapabilityMismatch(message) => {
            StubReply::json(400, body(message, "invalid_request_error"))
        }
        ModelError::Network(message) => StubReply::json(500, body(message, "server_error")),
    }
}

fn chunk_event(chunk: &StreamChunk) -> Option<Value> {
    match chunk {
        StreamChunk::TextDelta(text) => Some(json!({"choices": [{"delta": {"content": text}}]})),
        StreamChunk::ReasoningDelta(text) => {
            Some(json!({"choices": [{"delta": {"reasoning_content": text}}]}))
        }
        StreamChunk::ToolCallStart { index, id, name } => {
            Some(json!({"choices": [{"delta": {"tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": ""},
            }]}}]}))
        }
        StreamChunk::ToolArgsDelta { index, fragment } => {
            Some(json!({"choices": [{"delta": {"tool_calls": [{
                "index": index,
                "function": {"arguments": fragment},
            }]}}]}))
        }
        // Implicit on the wire; the adapter synthesizes ends at stream end.
        StreamChunk::ToolCallEnd { .. } | StreamChunk::Usage(_) | StreamChunk::Done { .. } => None,
        StreamChunk::OpaqueDelta(_) => {
            panic!("opaque deltas have no OpenAI-wire translation")
        }
    }
}

fn wire_finish(finish: &FinishReason) -> &str {
    match finish {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Other(raw) => raw.as_str(),
    }
}

fn wire_usage(usage: &Usage) -> Value {
    json!({
        "prompt_tokens": usage.input + usage.cache_read,
        "prompt_tokens_details": {"cached_tokens": usage.cache_read},
        "completion_tokens": usage.output,
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning},
        "total_tokens": usage.input + usage.cache_read + usage.output,
    })
}

/// A dialect wrapper overriding only the endpoint, so the real dialect code
/// (capabilities, reasoning mapping, echo obligations) runs against the stub.
pub struct StubbedEndpoint {
    inner: Box<dyn Dialect>,
    endpoint: String,
}

impl Dialect for StubbedEndpoint {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::from_owned(self.endpoint.clone())
    }

    fn api_key_env(&self) -> &'static str {
        self.inner.api_key_env()
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn echo_reasoning(&self) -> bool {
        self.inner.echo_reasoning()
    }

    fn extra_body(&self, reasoning: &Reasoning) -> Option<Value> {
        self.inner.extra_body(reasoning)
    }

    fn map_reasoning(&self, reasoning: &Reasoning) -> Result<Option<ReasoningEffort>, ModelError> {
        self.inner.map_reasoning(reasoning)
    }

    fn clamp_temperature(&self, temperature: f32) -> f32 {
        self.inner.clamp_temperature(temperature)
    }

    fn map_output(&self, output: &OutputMode) -> Result<Option<ChatResponseFormat>, ModelError> {
        self.inner.map_output(output)
    }

    fn build_options(&self, request: &ChatRequest) -> Result<ChatOptions, ModelError> {
        self.inner.build_options(request)
    }
}

/// The contract subject for `OpenAI`-compatible dialects: the real adapter
/// pointed at a local replay stub.
pub struct StubProvider {
    provider: OpenAiProvider,
    stub: SseStub,
}

impl StubProvider {
    pub fn new(dialect: Box<dyn Dialect>) -> Self {
        let stub = SseStub::start();
        let provider = OpenAiProvider::with_auth(
            Box::new(StubbedEndpoint {
                inner: dialect,
                endpoint: stub.endpoint().to_string(),
            }),
            AuthData::from_single("stub-key"),
        );
        Self { provider, stub }
    }

    /// The requests served so far, for dialect assertions on the wire shape.
    pub fn requests(&self) -> Vec<super::stub::RecordedRequest> {
        self.stub.requests()
    }

    /// Queues a raw SSE body (a recorded fixture) as the next reply.
    pub fn queue_raw_sse(&self, body: &str) {
        self.stub.push(StubReply::sse(body.to_string()));
    }
}

#[async_trait]
impl Provider for StubProvider {
    fn capabilities(&self) -> &Capabilities {
        self.provider.capabilities()
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChunkStream, ModelError> {
        self.provider.chat_stream(request).await
    }
}

impl ContractSubject for StubProvider {
    fn queue(&self, response: QueuedResponse) {
        match response {
            QueuedResponse::Chunks(chunks) => self.stub.push(StubReply::sse(to_sse_body(&chunks))),
            QueuedResponse::CallError(error) => self.stub.push(call_error_reply(&error)),
            QueuedResponse::StreamError { chunks, error } => self
                .stub
                .push(StubReply::sse(to_sse_error_body(&chunks, &error))),
        }
    }
}
