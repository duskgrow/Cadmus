use cadmus_contract::{CacheSupport, Capabilities, ReasoningCaps, SoSupport, Support};
use genai::resolver::Endpoint;

use crate::Dialect;

/// Kimi (Moonshot AI) — the phase-0 primary provider. OpenAI-compatible at
/// `https://api.moonshot.cn/v1`; reasoning tiers are a top-level
/// `reasoning_effort` (`low`/`high`/`max`, vendor default `max`).
pub struct KimiDialect {
    model: String,
    capabilities: Capabilities,
}

impl KimiDialect {
    /// `kimi-k3`: 1M context, always-on thinking with effort tiers.
    /// `max_output` is a conservative registry value — verify against the
    /// first real recording.
    #[must_use]
    pub fn k3() -> Self {
        Self {
            model: "kimi-k3".to_string(),
            capabilities: Capabilities {
                tools: true,
                parallel_tools: Support::Yes,
                structured_output: SoSupport::JsonMode,
                reasoning: Some(ReasoningCaps {
                    off_capable: false,
                    efforts: vec![
                        cadmus_contract::EffortLevel::Low,
                        cadmus_contract::EffortLevel::High,
                        cadmus_contract::EffortLevel::Max,
                    ],
                    budget_capable: false,
                    always_on: true,
                }),
                prompt_cache: CacheSupport::Automatic,
                logprobs: false,
                max_context: 1_000_000,
                max_output: 32_000,
                opaque_echo: vec![],
            },
        }
    }
}

impl Dialect for KimiDialect {
    fn id(&self) -> &str {
        &self.model
    }

    fn endpoint(&self) -> Endpoint {
        // Trailing slash: genai URL-joins `chat/completions` (RFC 3986), which
        // would drop a slash-less base's last segment (`/v1`).
        Endpoint::from_static("https://api.moonshot.cn/v1/")
    }

    fn api_key_env(&self) -> &'static str {
        "MOONSHOT_API_KEY"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    /// Moonshot's documented temperature range is `[0, 1]`.
    fn clamp_temperature(&self, temperature: f32) -> f32 {
        temperature.clamp(0.0, 1.0)
    }
}
