use cadmus_contract::{
    CacheSupport, Capabilities, EffortLevel, ModelError, Reasoning, ReasoningCaps, SoSupport,
    Support,
};
use genai::chat::ReasoningEffort;
use genai::resolver::Endpoint;
use serde_json::{Value, json};

use crate::Dialect;

/// `DeepSeek` — fast and cheap, the phase-0 second provider for validating
/// the dialect seam. `OpenAI`-compatible at `https://api.deepseek.com`;
/// thinking is opt-in via `{"thinking": {"type": "enabled"}}` plus
/// `reasoning_effort`, and thinking turns carrying tool calls must have their
/// `reasoning_content` echoed back verbatim or the wire answers 400.
pub struct DeepSeekDialect {
    model: String,
    capabilities: Capabilities,
}

impl DeepSeekDialect {
    /// `deepseek-v4-flash`: the lightweight verification target.
    /// `max_output` is a conservative registry value — verify against the
    /// first real recording.
    #[must_use]
    pub fn v4_flash() -> Self {
        Self {
            model: "deepseek-v4-flash".to_string(),
            capabilities: Capabilities {
                tools: true,
                parallel_tools: Support::Yes,
                structured_output: SoSupport::JsonMode,
                reasoning: Some(ReasoningCaps {
                    off_capable: true,
                    // The vendor documents `high`; `low` is the presumed
                    // sibling tier — verify at the first recording.
                    efforts: vec![EffortLevel::Low, EffortLevel::High],
                    budget_capable: false,
                    always_on: false,
                }),
                prompt_cache: CacheSupport::DiskAuto,
                logprobs: false,
                max_context: 128_000,
                max_output: 8_000,
                opaque_echo: vec!["reasoning_content".to_string()],
            },
        }
    }
}

impl Dialect for DeepSeekDialect {
    fn id(&self) -> &str {
        &self.model
    }

    fn endpoint(&self) -> Endpoint {
        // Trailing slash per the RFC-3986 join rule (see `KimiDialect`).
        Endpoint::from_static("https://api.deepseek.com/")
    }

    fn api_key_env(&self) -> &'static str {
        "DEEPSEEK_API_KEY"
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities.clone()
    }

    /// The echo obligation: past reasoning goes back on the wire.
    fn echo_reasoning(&self) -> bool {
        true
    }

    fn extra_body(&self, reasoning: &Reasoning) -> Option<Value> {
        match reasoning {
            Reasoning::Effort(_) => Some(json!({"thinking": {"type": "enabled"}})),
            Reasoning::Off | Reasoning::Adaptive | Reasoning::Budget(_) => None,
        }
    }

    fn map_reasoning(&self, reasoning: &Reasoning) -> Result<Option<ReasoningEffort>, ModelError> {
        match reasoning {
            // Thinking is strictly opt-in on this vendor; both "off" and
            // "vendor default" mean no parameter at all.
            Reasoning::Off | Reasoning::Adaptive => Ok(None),
            Reasoning::Effort(level) => {
                let mapped = match level {
                    EffortLevel::Low => ReasoningEffort::Low,
                    EffortLevel::Medium | EffortLevel::High => {
                        if *level == EffortLevel::Medium {
                            tracing::warn!(requested = ?level, applied = ?EffortLevel::High, "reasoning effort clamped to nearest supported tier");
                        }
                        ReasoningEffort::High
                    }
                    EffortLevel::Max => {
                        tracing::warn!(requested = ?level, applied = ?EffortLevel::High, "reasoning effort clamped to nearest supported tier");
                        ReasoningEffort::High
                    }
                };
                Ok(Some(mapped))
            }
            Reasoning::Budget(_) => Err(ModelError::CapabilityMismatch(
                "deepseek does not support token-budget reasoning".into(),
            )),
        }
    }
}
