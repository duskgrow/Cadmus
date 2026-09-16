//! Interactive chat: the ADR-0011 TUI driving the same two protocol ports
//! headless chat drives (ADR-0013 item 6: commands are the only upstream).
//! The session is a client-side concept: one run per prompt, the app's
//! history carried across runs and handed back as each run's base.
//!
//! The interaction surface never renders logs (open-items): in TUI mode
//! tracing writes to `{trace_root}/cadmus.log`, never to the terminal.

use std::path::PathBuf;
use std::sync::Arc;

use cadmus_contract::{ChatRequest, Command, Message, Provider};
use cadmus_core::{AgentLoop, ClientProtocol, ContextBundle, Telemetry};
use cadmus_memory::JsonlLog;
use cadmus_transport::{Broadcaster, command_channel};
use cadmus_tui::app::{AppConfig, RunDriver, RunHandle};

use crate::telemetry::{SeqIds, SystemClock, default_trace_root, mint_trace_id};
use crate::tools::coding_tools;
use crate::{ChatConfig, Error, approval, context, provider};

/// Runs the interactive session until the user quits.
pub async fn run_tui(config: &ChatConfig) -> Result<(), Error> {
    let (provider, wire_model) = provider::build(
        &config.provider,
        config.model.as_deref(),
        config.base_url.as_deref(),
    )?;
    // Same resolution as one-shot chat (chat.rs): the model's registry value
    // unless the operator overrode it.
    let max_tokens = config
        .max_tokens
        .unwrap_or(provider.capabilities().max_output);
    let root = match &config.trace_root {
        Some(root) => root.clone(),
        None => default_trace_root().ok_or(Error::TraceRoot)?,
    };
    init_file_tracing(&root)?;
    let log = Arc::new(JsonlLog::new(root).map_err(Error::TraceLog)?);
    let cwd = std::env::current_dir().map_err(Error::Workdir)?;
    let cwd = cwd.canonicalize().unwrap_or(cwd);

    let driver = TuiDriver {
        provider: Arc::new(provider),
        run_attributes: provider::run_attributes(&config.provider, &wire_model),
        max_tokens,
        max_turns: config.max_turns,
        approve_writes: config.approve_writes,
        log,
        root: cwd.clone(),
    };
    let config = AppConfig {
        label: format!("{}·{wire_model}  {}", config.provider, cwd.display()),
        theme: cadmus_ui::theme::Theme::ansi(),
        depth: cadmus_tui::style::detect_depth(),
    };
    cadmus_tui::app::run(Box::new(driver), config)
        .await
        .map_err(Error::Tui)
}

/// The session boundary: every prompt spawns one run (trajectory, live
/// stream, command channel), the same wiring as one-shot chat's, with the
/// loop task spawned onto the caller's runtime.
struct TuiDriver {
    provider: Arc<dyn Provider>,
    run_attributes: std::collections::BTreeMap<String, serde_json::Value>,
    max_tokens: u32,
    max_turns: usize,
    approve_writes: bool,
    log: Arc<JsonlLog>,
    root: PathBuf,
}

impl RunDriver for TuiDriver {
    fn start(&self, messages: Vec<Message>) -> RunHandle {
        let trace_id = mint_trace_id();
        let telemetry = Telemetry {
            sink: self.log.clone(),
            clock: Arc::new(SystemClock),
            ids: Arc::new(SeqIds::default()),
            trace_id: trace_id.clone(),
            run_attributes: self.run_attributes.clone(),
        };
        // The context pipeline (ADR-0007), rebuilt per run like one-shot
        // chat's: skills and instructions are cheap rediscovery, and the
        // nested-file tracker's state is per-run by design.
        let skills = crate::skills::discover(&self.root, &context::Scope::UserAndWorkspace);
        let catalog: Vec<_> = skills.iter().map(|skill| skill.summary.clone()).collect();
        let tools = coding_tools(self.root.clone(), skills);
        let specs: Vec<_> = tools.iter().map(|tool| tool.spec()).collect();
        let instructions =
            context::instruction_chain(&self.root, &context::Scope::UserAndWorkspace);
        let pipeline = ContextBundle {
            prefix: cadmus_core::FrozenPrefix::assemble(
                cadmus_core::context::SYSTEM_PROMPT,
                &instructions,
                &catalog,
                &specs,
            ),
            probe: Arc::new(context::GitProbe::new(self.root.clone())),
            tracker: Arc::new(context::NestedInstructions::new(self.root.clone())),
            cwd: self.root.display().to_string(),
            artifacts: self
                .log
                .artifacts(&trace_id)
                .map(|sink| Arc::new(sink) as Arc<dyn cadmus_contract::ArtifactSink>)
                .expect("a minted trace id resolves its artifact dir"),
            fold_policy: cadmus_core::context::FoldPolicy::default(),
        };
        // The attach happens before the loop exists, so position 0 holds by
        // construction (one-shot chat's own lesson, chat.rs).
        let broadcaster = Arc::new(Broadcaster::new());
        let first = broadcaster.attach();
        let (sender, commands) = command_channel();
        let app_commands = sender.clone();
        let resolver = approval::AutoResolver::new(
            broadcaster.clone(),
            sender,
            approval::unattended(self.approve_writes),
        );
        let agent = AgentLoop::new(
            self.provider.clone(),
            tools,
            pipeline,
            ClientProtocol {
                live: Arc::new(resolver),
                commands: Arc::new(commands),
            },
            self.max_turns,
            telemetry,
        );
        let (teardown_tx, teardown_rx) = std::sync::mpsc::channel();
        let run_broadcaster = broadcaster.clone();
        let max_tokens = self.max_tokens;
        tokio::spawn(async move {
            let base = ChatRequest::user_text("", max_tokens).with_messages(messages);
            let outcome = agent.run(&base).await;
            // Close the tails even on the paths without a terminal record
            // (chat.rs's tail discipline), then report — the drainer
            // forwards the report last, so the client tears the run down in
            // stream order.
            run_broadcaster.close();
            let _ = teardown_tx.send(
                outcome
                    .map(|outcome| outcome.messages)
                    .map_err(|error| error.to_string()),
            );
        });
        RunHandle {
            attachment: first,
            reattach: Box::new(move || broadcaster.attach()),
            commands: Box::new(move |command: Command| {
                // A closed channel means the run is gone (the gate treats it
                // as unanswered-deny), so dropping is safe.
                let _ = app_commands.send(command);
            }),
            teardown: teardown_rx,
        }
    }
}

/// TUI-mode tracing: same filter discipline as the CLI's (`RUST_LOG`,
/// default `warn`), one append-only file — capped at [`LOG_CAP_BYTES`] by
/// truncation at session start (the file is a debug channel, not an
/// archive). The interaction view stays clean.
fn init_file_tracing(trace_root: &std::path::Path) -> Result<(), Error> {
    use tracing_subscriber::EnvFilter;
    /// The cap past which a new session starts the log fresh.
    const LOG_CAP_BYTES: u64 = 8 * 1024 * 1024;
    let path = trace_root.join("cadmus.log");
    let oversized = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_CAP_BYTES);
    let file = Arc::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(!oversized)
            .truncate(oversized)
            .open(&path)
            .map_err(Error::TraceLog)?,
    );
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(move || file.clone())
        .with_ansi(false)
        .init();
    tracing::info!(path = %path.display(), "TUI session: logs go to this file");
    Ok(())
}
