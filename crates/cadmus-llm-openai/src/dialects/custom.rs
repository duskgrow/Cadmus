use cadmus_contract::{CacheSupport, Capabilities, EffortLevel, ReasoningCaps, SoSupport, Support};
use genai::resolver::Endpoint;

use crate::Dialect;

/// A custom OpenAI-compatible endpoint — relay stations, self-hosted servers,
/// the phase-3 llama.cpp node. Because such endpoints vary, the base URL and
/// model are always explicit configuration. Serves as the "no vendor quirks"
/// control case for the dialect seam.
pub struct CustomDialect {
    model: String,
    base_url: String,
    capabilities: Capabilities,
}

impl CustomDialect {
    /// The capability declaration describes a GPT-5-class model; override it
    /// when the endpoint fronts something else (the config level of the
    /// capability resolution stack).
    #[must_use]
    pub fn new(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        // Normalize to a trailing slash: genai URL-joins `chat/completions`
        // (RFC 3986), which would otherwise drop the base's last segment.
        let mut base_url = base_url.into();
        if !base_url.ends_with('/') {
            base_url.push('/');
        }
        Self {
            model: model.into(),
            base_url,
            capabilities: Capabilities {
                tools: true,
                parallel_tools: Support::Yes,
                structured_output: SoSupport::NativeStrict,
                reasoning: Some(ReasoningCaps {
                    off_capable: true,
                    efforts: vec![EffortLevel::Low, EffortLevel::Medium, EffortLevel::High],
                    budget_capable: false,
                    always_on: false,
                }),
                prompt_cache: CacheSupport::Automatic,
                logprobs: false,
                max_context: 128_000,
                max_output: 16_000,
                opaque_echo: vec![],
            },
        }
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

impl Dialect for CustomDialect {
    fn id(&self) -> &str {
        &self.model
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::from_owned(self.base_url.clone())
    }

    fn api_key_env(&self) -> &'static str {
        "CADMUS_CUSTOM_API_KEY"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }
}
