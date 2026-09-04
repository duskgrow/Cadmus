//! One-shot chat: prompt in, final answer out, with the coding tools wired.
//! Streaming deltas are assembled in `cadmus-core`; phase 0 prints the final
//! turn (incremental terminal rendering is a later polish).
//!
//! Every run appends its trajectory to the JSONL event log (ADR-0005): one
//! file per trace under the trace root, recorded as `ChatResult::trace_path`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use cadmus_contract::{ChatRequest, ContentPart, Message, Usage, attrs};
use cadmus_core::{AgentLoop, RunOutcome, Telemetry};
use cadmus_llm_openai::{CustomDialect, Dialect, OpenAiProvider, dialect_by_name};
use cadmus_memory::JsonlLog;

use crate::Error;
use crate::telemetry::{SeqIds, SystemClock, default_trace_root, mint_trace_id};
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
    /// Trajectory root; `None` resolves the env/default chain at run time.
    pub trace_root: Option<PathBuf>,
}

/// The completed run, shaped for output.
pub struct ChatResult {
    pub final_text: String,
    pub messages: Vec<Message>,
    pub turns: usize,
    pub warnings: Vec<String>,
    pub usage: Option<Usage>,
    /// The trace id and the JSONL file the run's trajectory was appended to.
    pub trace_id: String,
    pub trace_path: PathBuf,
}

/// Runs one prompt through the agent loop with the coding tools confined to
/// the current working directory.
pub async fn run_chat(prompt: &str, config: &ChatConfig) -> Result<ChatResult, Error> {
    let dialect = build_dialect(config)?;
    // Captured before the dialect moves into the provider: the model name as
    // sent on the wire is trajectory provenance.
    let wire_model = dialect.model_name().to_string();
    // Fail fast with an actionable error instead of a wire 401.
    if std::env::var(dialect.api_key_env()).is_err() {
        return Err(Error::MissingApiKey {
            env: dialect.api_key_env(),
        });
    }
    let provider = OpenAiProvider::from_env(dialect).map_err(Error::Provider)?;

    let root = match &config.trace_root {
        Some(root) => root.clone(),
        None => default_trace_root().ok_or(Error::TraceRoot)?,
    };
    let clock = Arc::new(SystemClock);
    let log = Arc::new(JsonlLog::new(root).map_err(Error::TraceLog)?);
    let trace_id = mint_trace_id();
    let telemetry = Telemetry {
        sink: log.clone(),
        clock,
        ids: Arc::new(SeqIds::default()),
        trace_id: trace_id.clone(),
        run_attributes: run_attributes(config, &wire_model),
    };

    // Pointed out before the run: a failed run's partial trajectory is
    // exactly the one worth inspecting. The path is a pure function of the
    // minted id, so it is computed once here.
    let trace_path = log
        .trace_path(&trace_id)
        .expect("minted id resolves to a shard path");
    tracing::info!(trace_id, path = %trace_path.display(), "recording trajectory");
    let root = std::env::current_dir().map_err(Error::Workdir)?;
    let agent = AgentLoop::new(
        Arc::new(provider),
        coding_tools(root),
        config.max_turns,
        telemetry,
    );
    let outcome = agent
        .run(&ChatRequest::user_text(prompt, config.max_tokens))
        .await?;
    Ok(into_result(outcome, trace_id, trace_path))
}

/// Run-level provenance recorded on the start-run event (ADR-0005 §3): the
/// wired provider, the model name as sent on the wire, and the binary
/// version — the attributes every later projection groups by.
fn run_attributes(config: &ChatConfig, wire_model: &str) -> BTreeMap<String, serde_json::Value> {
    BTreeMap::from([
        (attrs::PROVIDER.to_string(), config.provider.clone().into()),
        (attrs::MODEL.to_string(), wire_model.into()),
        (
            attrs::CADMUS_VERSION.to_string(),
            env!("CARGO_PKG_VERSION").into(),
        ),
    ])
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

fn into_result(outcome: RunOutcome, trace_id: String, trace_path: PathBuf) -> ChatResult {
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
        trace_id,
        trace_path,
    }
}
