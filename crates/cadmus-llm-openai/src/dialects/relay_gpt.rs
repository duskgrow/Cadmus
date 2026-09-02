use cadmus_contract::{CacheSupport, Capabilities, EffortLevel, ReasoningCaps, SoSupport, Support};
use genai::resolver::Endpoint;

use crate::Dialect;

/// A relay-station GPT — a generic OpenAI-compatible endpoint reached through
/// a relay, so the base URL is always explicit configuration (relays vary).
/// Serves as the "no vendor quirks" control case for the dialect seam.
pub struct RelayGptDialect {
    model: String,
    base_url: String,
    capabilities: Capabilities,
}

impl RelayGptDialect {
    /// The capability declaration describes a GPT-5-class model; override it
    /// when the relay fronts something else (the config level of the
    /// capability resolution stack).
    #[must_use]
    pub fn new(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            base_url: base_url.into(),
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

impl Dialect for RelayGptDialect {
    fn id(&self) -> &str {
        &self.model
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::from_owned(self.base_url.clone())
    }

    fn api_key_env(&self) -> &'static str {
        "RELAY_GPT_API_KEY"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }
}
