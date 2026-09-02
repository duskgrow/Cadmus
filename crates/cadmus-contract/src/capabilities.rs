use serde::{Deserialize, Serialize};

use crate::EffortLevel;

/// What a model can actually do — a first-class declaration: capability
/// mismatches fail fast at the adapter instead of being silently downgraded
/// on the wire (`OpenRouter`'s `require_parameters` lesson). Resolution is a
/// three-level structure: static registry of known models, config override,
/// runtime probing as a last resort.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub tools: bool,
    pub parallel_tools: Support,
    pub structured_output: SoSupport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningCaps>,
    pub prompt_cache: CacheSupport,
    pub logprobs: bool,
    pub max_context: u32,
    pub max_output: u32,
    /// Opaque fields this model requires echoed back verbatim (e.g.
    /// `["reasoning_content"]` for `DeepSeek` thinking).
    #[serde(default)]
    pub opaque_echo: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Yes,
    /// The vendor accepts the flag but does not guarantee the behavior
    /// (vLLM's `parallel_tool_calls=true` semantics).
    AllowedNotGuaranteed,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoSupport {
    NativeStrict,
    NativeLoose,
    JsonMode,
    PromptOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningCaps {
    pub off_capable: bool,
    pub efforts: Vec<EffortLevel>,
    pub budget_capable: bool,
    pub always_on: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheSupport {
    ExplicitBreakpoints,
    Automatic,
    NamedObject,
    DiskAuto,
    None,
}
