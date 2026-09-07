//! One-shot chat: prompt in, final answer out, with the coding tools wired.
//! Streaming deltas are assembled in `cadmus-core`; phase 0 prints the final
//! turn (incremental terminal rendering is a later polish).
//!
//! Every run appends its trajectory to the JSONL event log (ADR-0005): one
//! file per trace under the trace root, recorded as `ChatResult::trace_path`.

use std::path::PathBuf;
use std::sync::Arc;

use cadmus_contract::{ChatRequest, ContentPart, Message, Usage};
use cadmus_core::{AgentLoop, RunOutcome, Telemetry};
use cadmus_memory::JsonlLog;

use crate::telemetry::{SeqIds, SystemClock, default_trace_root, mint_trace_id};
use crate::tools::coding_tools;
use crate::{Error, provider};

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
    let (provider, wire_model) = provider::build(
        &config.provider,
        config.model.as_deref(),
        config.base_url.as_deref(),
    )?;

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
        run_attributes: provider::run_attributes(&config.provider, &wire_model),
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
