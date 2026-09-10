//! One-shot chat: prompt in, final answer out, with the coding tools wired.
//! Streaming deltas are assembled in `cadmus-core`; the final turn prints
//! (incremental terminal rendering is the TUI's, ADR-0011 item 3), while
//! turn and tool activity render live to stderr from the live stream
//! (`render`) — headless chat is the client protocol's degenerate client
//! (attach at position 0, ADR-0013).
//!
//! Headless chat is unattended: mutation calls pass the approval gate and
//! are denied unless the operator passed `--yes` (ADR-0008 item 4, ADR-0011
//! item 3) — the denial comes back as a tool result, so the model can adapt.
//!
//! Every run appends its trajectory to the JSONL event log (ADR-0005): one
//! file per trace under the trace root, recorded as `ChatResult::trace_path`.

use std::path::PathBuf;
use std::sync::Arc;

use cadmus_contract::{ChatRequest, ContentPart, Message, Usage};
use cadmus_core::{AgentLoop, ClientProtocol, ContextBundle, RunOutcome, Telemetry};
use cadmus_memory::JsonlLog;
use cadmus_transport::{Broadcaster, command_channel};

use crate::telemetry::{SeqIds, SystemClock, default_trace_root, mint_trace_id};
use crate::tools::coding_tools;
use crate::{Error, approval, context, provider, render};

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
    /// Approve workspace-mutation tool calls without a prompt (the CLI's
    /// `-y/--yes`): headless chat cannot ask, so unattended runs deny them
    /// (ADR-0008 item 4, ADR-0011 item 3).
    pub approve_writes: bool,
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
    let root = root.canonicalize().unwrap_or(root);
    // The context pipeline (ADR-0007): frozen prefix (system prompt +
    // AGENTS.md chain + tool specs in the hash), git probe and nested-file
    // tracker — the loop renders prefix + history + fresh trailer per turn.
    let tools = coding_tools(root.clone());
    let specs: Vec<_> = tools.iter().map(|tool| tool.spec()).collect();
    let instructions =
        context::instruction_chain(&root, &context::InstructionScope::UserAndWorkspace);
    let pipeline = ContextBundle {
        prefix: cadmus_core::FrozenPrefix::assemble(
            cadmus_core::context::SYSTEM_PROMPT,
            &instructions,
            &specs,
        ),
        probe: Arc::new(context::GitProbe::new(root.clone())),
        tracker: Arc::new(context::NestedInstructions::new(root.clone())),
        cwd: root.display().to_string(),
        artifacts: log
            .artifacts(&trace_id)
            .map(|sink| Arc::new(sink) as Arc<dyn cadmus_contract::ArtifactSink>)
            .expect("a minted trace id resolves its artifact dir"),
        fold_policy: cadmus_core::context::FoldPolicy::default(),
    };
    // The client protocol (ADR-0013): the renderer subscribes to the live
    // stream; approvals auto-resolve through the command channel per the
    // `--yes` policy — the same two ports the TUI will drive. The attach
    // happens before the loop exists, so position 0 holds by construction
    // (an attach inside the renderer thread would race the run's first
    // events into the discarded baseline).
    let broadcaster = Arc::new(Broadcaster::new());
    let first = broadcaster.attach();
    let (sender, commands) = command_channel();
    let resolver = approval::AutoResolver::new(
        broadcaster.clone(),
        sender,
        approval::unattended(config.approve_writes),
    );
    let renderer = render::spawn(broadcaster.clone(), first);
    let agent = AgentLoop::new(
        Arc::new(provider),
        tools,
        pipeline,
        ClientProtocol {
            live: Arc::new(resolver),
            commands: Arc::new(commands),
        },
        config.max_turns,
        telemetry,
    );
    let result = agent
        .run(&ChatRequest::user_text(prompt, config.max_tokens))
        .await;
    // The tail must end even on the paths without a terminal record (a
    // failed trajectory log aborts mid-run): close, then join the renderer.
    broadcaster.close();
    let _ = renderer.join();
    let outcome = result?;
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
