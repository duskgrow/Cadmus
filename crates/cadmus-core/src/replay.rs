use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use cadmus_contract::{
    CacheSupport, Capabilities, ChatRequest, ChunkStream, ModelError, Provider, SoSupport,
    StreamChunk, Support,
};

/// Recorded-replay fake provider (ADR-0003): scripts are fixed chunk
/// sequences and each `chat_stream` call pops the next one. Contract tests
/// and snapshot tests never make live calls — real provider traffic is
/// recorded into fixtures offline.
pub struct ReplayProvider {
    capabilities: Capabilities,
    scripts: Mutex<VecDeque<Vec<Result<StreamChunk, ModelError>>>>,
}

impl ReplayProvider {
    pub fn new<I>(scripts: I) -> Self
    where
        I: IntoIterator<Item = Vec<Result<StreamChunk, ModelError>>>,
    {
        Self {
            capabilities: Self::default_capabilities(),
            scripts: Mutex::new(scripts.into_iter().collect()),
        }
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// A script of all-Ok chunks.
    pub fn script(chunks: Vec<StreamChunk>) -> Vec<Result<StreamChunk, ModelError>> {
        chunks.into_iter().map(Ok).collect()
    }

    fn default_capabilities() -> Capabilities {
        Capabilities {
            tools: true,
            parallel_tools: Support::Yes,
            structured_output: SoSupport::NativeStrict,
            reasoning: None,
            prompt_cache: CacheSupport::Automatic,
            logprobs: false,
            max_context: 128_000,
            max_output: 8_000,
            opaque_echo: vec![],
        }
    }
}

#[async_trait]
impl Provider for ReplayProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn chat_stream(&self, _request: &ChatRequest) -> Result<ChunkStream, ModelError> {
        let script = self
            .scripts
            .lock()
            .expect("replay scripts poisoned")
            .pop_front()
            .ok_or_else(|| ModelError::Protocol("replay script exhausted".into()))?;
        Ok(Box::pin(tokio_stream::iter(script)))
    }
}
