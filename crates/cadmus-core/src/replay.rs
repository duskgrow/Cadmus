use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use cadmus_contract::{
    CacheSupport, Capabilities, ChatRequest, ChunkStream, ContractSubject, ModelError, Provider,
    QueuedResponse, SoSupport, StreamChunk, Support,
};

/// One queued response: a chunk script or a call-level failure.
enum ReplayScript {
    Stream(Vec<Result<StreamChunk, ModelError>>),
    Fail(ModelError),
}

/// Recorded-replay fake provider (ADR-0003): scripts are fixed chunk
/// sequences and each `chat_stream` call pops the next one. Contract tests
/// and snapshot tests never make live calls — real provider traffic is
/// recorded into fixtures offline.
pub struct ReplayProvider {
    capabilities: Capabilities,
    scripts: Mutex<VecDeque<ReplayScript>>,
}

impl ReplayProvider {
    pub fn new<I>(scripts: I) -> Self
    where
        I: IntoIterator<Item = Vec<Result<StreamChunk, ModelError>>>,
    {
        Self {
            capabilities: Self::default_capabilities(),
            scripts: Mutex::new(scripts.into_iter().map(ReplayScript::Stream).collect()),
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

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChunkStream, ModelError> {
        // Fakes provide behavior: the declared capabilities are enforced like
        // a real adapter would, failing fast before the scripted wire.
        if !self.capabilities.tools && !request.tools.is_empty() {
            return Err(ModelError::CapabilityMismatch(
                "replay profile declares no tool support".into(),
            ));
        }
        match self
            .scripts
            .lock()
            .expect("replay scripts poisoned")
            .pop_front()
        {
            Some(ReplayScript::Stream(script)) => Ok(Box::pin(tokio_stream::iter(script))),
            Some(ReplayScript::Fail(error)) => Err(error),
            None => Err(ModelError::Protocol("replay script exhausted".into())),
        }
    }
}

impl ContractSubject for ReplayProvider {
    fn queue(&self, response: QueuedResponse) {
        let script = match response {
            QueuedResponse::Chunks(chunks) => ReplayScript::Stream(Self::script(chunks)),
            QueuedResponse::CallError(error) => ReplayScript::Fail(error),
            QueuedResponse::StreamError { chunks, error } => ReplayScript::Stream(
                chunks
                    .into_iter()
                    .map(Ok)
                    .chain(std::iter::once(Err(error)))
                    .collect(),
            ),
        };
        self.scripts
            .lock()
            .expect("replay scripts poisoned")
            .push_back(script);
    }
}
