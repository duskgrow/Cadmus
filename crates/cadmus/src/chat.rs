//! One-shot chat: prompt in, final answer out, with the coding tools wired.
//! Streaming deltas are assembled in `cadmus-core`; phase 0 prints the final
//! turn (incremental terminal rendering is a later polish).

use std::sync::Arc;

use cadmus_contract::{ChatRequest, ContentPart, Message, Usage};
use cadmus_core::{AgentLoop, RunOutcome};
use cadmus_llm_openai::{CustomDialect, Dialect, OpenAiProvider, dialect_by_name};

use crate::Error;
use crate::tools::coding_tools;

/// Everything a chat run needs, resolved from CLI arguments. The provider
/// name is passed through verbatim — the vendor registry lives in
/// `cadmus-llm-openai` (SSOT); this struct carries no vendor knowledge.
pub struct ChatConfig {
    pub provider: String,
    /// Required for `--provider custom`; the registry dialects pin their
    /// model.
    pub model: Option<String>,
    /// Required for `--provider custom`.
    pub base_url: Option<String>,
    pub max_tokens: u32,
    pub max_turns: usize,
}

/// The completed run, shaped for output.
pub struct ChatResult {
    pub final_text: String,
    pub messages: Vec<Message>,
    pub turns: usize,
    pub warnings: Vec<String>,
    pub usage: Option<Usage>,
}

/// Runs one prompt through the agent loop with the coding tools confined to
/// the current working directory.
pub async fn run_chat(prompt: &str, config: &ChatConfig) -> Result<ChatResult, Error> {
    let dialect = build_dialect(config)?;
    // Fail fast with an actionable error instead of a wire 401.
    if std::env::var(dialect.api_key_env()).is_err() {
        return Err(Error::MissingApiKey {
            env: dialect.api_key_env(),
        });
    }
    let provider = OpenAiProvider::from_env(dialect).map_err(Error::Provider)?;

    let root = std::env::current_dir().map_err(Error::Workdir)?;
    let agent = AgentLoop::new(Arc::new(provider), coding_tools(root), config.max_turns);
    let outcome = agent
        .run(&ChatRequest::user_text(prompt, config.max_tokens))
        .await?;
    Ok(into_result(outcome))
}

fn build_dialect(config: &ChatConfig) -> Result<Box<dyn Dialect>, Error> {
    if config.provider == "custom" {
        let (Some(model), Some(base_url)) = (&config.model, &config.base_url) else {
            return Err(Error::CustomConfigMissing);
        };
        return Ok(Box::new(CustomDialect::new(model, base_url)));
    }
    dialect_by_name(&config.provider).ok_or_else(|| Error::UnknownProvider(config.provider.clone()))
}

fn into_result(outcome: RunOutcome) -> ChatResult {
    let final_text = outcome
        .final_turn
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    ChatResult {
        final_text,
        messages: outcome.messages,
        turns: outcome.turns,
        warnings: outcome.final_turn.warnings,
        usage: outcome.final_turn.usage,
    }
}
