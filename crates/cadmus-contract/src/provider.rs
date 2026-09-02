use std::pin::Pin;

use async_trait::async_trait;
use tokio_stream::Stream;

use crate::{Capabilities, ChatRequest, ModelError, StreamChunk};

pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, ModelError>> + Send>>;

/// The model-invocation port. A single call shape: everything is a stream —
/// a non-streaming answer is the degenerate case the caller assembles.
/// (Low-frequency IO port, hence `async_trait` per ADR-0002's dispatch rules.)
#[async_trait]
pub trait Provider: Send + Sync {
    /// Static declaration of what this model can do; adapters resolve it from
    /// the registry + config overrides and must not guess at call time.
    fn capabilities(&self) -> &Capabilities;

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChunkStream, ModelError>;
}
