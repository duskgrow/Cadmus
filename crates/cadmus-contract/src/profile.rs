use serde::{Deserialize, Serialize};

/// Prompt-affinity data for one model — versioned config data, not code
/// (ADR-0003): loaded at runtime, edited like skill data, and from phase 2 on
/// itself an evolution target under the same gates as skill items. The
/// `version` chain is the rollback mechanism.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Matches the model name used in requests.
    pub id: String,
    pub version: u32,
    pub system_preamble: String,
    pub tool_description_style: ToolDescriptionStyle,
    pub few_shot_format: FewShotFormat,
    pub cache_hints: CacheHints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDescriptionStyle {
    Terse,
    Detailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FewShotFormat {
    ChatTurns,
    DelimitedBlocks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHints {
    /// Cache hits require a byte-stable prefix; any dynamic content injected
    /// into the prefix (timestamps, random ids, reordered tool lists) destroys
    /// it. When set, the harness must keep the prefix stable.
    pub stable_prefix_required: bool,
}
