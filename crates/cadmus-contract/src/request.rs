use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Message;

/// A provider call. Only the portable intersection lives here — anything
/// vendor-private goes through [`ChatRequest::extra`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub tool_choice: ToolChoice,
    /// `None` = vendor default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tools: Option<bool>,
    #[serde(default)]
    pub output: OutputMode,
    /// Tiers the adapter does not support are clamped to the nearest tier and
    /// the actually applied value is reported — never silently passed through.
    #[serde(default)]
    pub reasoning: Reasoning,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub sampling: Sampling,
    #[serde(default)]
    pub cache: CacheDirective,
    /// Escape hatch: whitelisted vendor passthrough. Fields the adapter does
    /// not recognize are warned about, never silently swallowed
    /// (counter-example: new-api dropping unknown fields with 200 OK).
    #[serde(default)]
    pub extra: Value,
}

impl ChatRequest {
    /// A plain single-turn text request.
    pub fn user_text(text: impl Into<String>, max_output_tokens: u32) -> Self {
        Self {
            messages: vec![Message::user(text)],
            tools: Vec::new(),
            tool_choice: ToolChoice::default(),
            parallel_tools: None,
            output: OutputMode::default(),
            reasoning: Reasoning::default(),
            max_output_tokens,
            sampling: Sampling::default(),
            cache: CacheDirective::default(),
            extra: Value::default(),
        }
    }

    /// The same request with a different history (the agent loop rebuilds the
    /// request each turn, keeping every other parameter byte-stable for
    /// prompt-cache hits).
    #[must_use]
    pub fn with_messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Named(String),
}

/// Structured-output mode; the adapter walks the four-step degradation
/// ladder (native strict → native loose/JSON mode → prompt-injected +
/// validation retry → plain text) by capability, and a validation failure
/// surfaces as a routing signal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Text,
    Json,
    Schema {
        schema: Value,
        strict: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    Off,
    #[default]
    Adaptive,
    Effort(EffortLevel),
    Budget(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

/// Vendor ranges differ (Moonshot clamps temperature to [0, 1]); clamping is
/// the adapter's job.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sampling {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDirective {
    #[default]
    None,
    Auto,
    /// Indexes into `ChatRequest::messages` that carry a cache breakpoint.
    Breakpoints(Vec<usize>),
}
