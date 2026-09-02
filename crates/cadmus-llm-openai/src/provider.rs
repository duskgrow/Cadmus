use async_trait::async_trait;
use cadmus_contract::{Capabilities, ChatRequest, ChunkStream, ModelError, Provider};
use genai::adapter::AdapterKind;
use genai::resolver::AuthData;
use genai::{Client, ModelIden, ServiceTarget};
use tokio_stream::StreamExt;

use crate::dialect::{Dialect, api_key_from_env};
use crate::error::map_genai_error;
use crate::map::build_genai_request;
use crate::stream_map::MappedStream;

/// The OpenAI-compatible provider adapter: one thin-client-backed
/// [`Provider`] per (vendor, model) dialect. Adapters carry no logic of their
/// own beyond mapping — replacing the thin client must never touch the core
/// (ADR-0002's boundary-leak signal).
pub struct OpenAiProvider {
    client: Client,
    target: ServiceTarget,
    capabilities: Capabilities,
    dialect: Box<dyn Dialect>,
}

impl OpenAiProvider {
    /// Builds a provider, reading the API key from the dialect's environment
    /// variable.
    pub fn from_env(dialect: Box<dyn Dialect>) -> Result<Self, ModelError> {
        let auth = api_key_from_env(dialect.as_ref())?;
        Ok(Self::with_auth(dialect, auth))
    }

    /// Builds a provider with an explicit key (tests, managed secret stores).
    #[must_use]
    pub fn with_auth(dialect: Box<dyn Dialect>, auth: AuthData) -> Self {
        let target = ServiceTarget {
            endpoint: dialect.endpoint(),
            auth,
            model: ModelIden::new(AdapterKind::OpenAI, dialect.model_name()),
        };
        let capabilities = dialect.capabilities();
        Self {
            client: Client::default(),
            target,
            capabilities,
            dialect,
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChunkStream, ModelError> {
        let genai_request = build_genai_request(request, self.dialect.echo_reasoning())?;
        let options = self.dialect.build_options(request)?;
        let response = self
            .client
            .exec_chat_stream(self.target.clone(), genai_request, Some(&options))
            .await
            .map_err(map_genai_error)?;

        // genai surfaces pre-content HTTP failures (4xx/5xx) as the first
        // stream item; the port contract wants call-level failures as
        // call-level errors (pitfall #11), so peek one item. A failure after
        // content starts stays an in-stream item (pitfall #9).
        let mut stream = MappedStream::new(response.stream);
        match stream.next().await {
            Some(Err(error)) => Err(error),
            Some(Ok(first)) => Ok(Box::pin(tokio_stream::iter(vec![Ok(first)]).chain(stream))),
            None => Ok(Box::pin(stream)),
        }
    }
}
