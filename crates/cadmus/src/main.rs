//! Command-line shell for `cadmus`.
//!
//! Follows <https://clig.dev>: data goes to stdout, diagnostics to stderr,
//! `--json` provides machine-readable output, and `--help` is generated from
//! the clap definitions (help text is documentation with a single source).

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use cadmus::{ChatConfig, Error};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Chat with the agent (one shot: prompt in, final answer out), with
    /// read-only coding tools confined to the current directory
    Chat {
        /// Provider to use (registry dialect, or `custom` for an explicit
        /// OpenAI-compatible endpoint)
        #[arg(long, default_value = "kimi")]
        provider: String,
        /// Model name (required for --provider custom; registry dialects pin
        /// their model)
        #[arg(long)]
        model: Option<String>,
        /// Endpoint base URL (required for --provider custom; key comes from
        /// `CADMUS_CUSTOM_API_KEY`)
        #[arg(long)]
        base_url: Option<String>,
        /// Maximum output tokens per assistant turn
        #[arg(long, default_value_t = 4_096)]
        max_tokens: u32,
        /// Maximum assistant turns before the run fails
        #[arg(long, default_value_t = 16)]
        max_turns: usize,
        /// Directory the trajectory JSONL log is written under (default: the
        /// `CADMUS_TRACE_ROOT` env var, else the platform data dir)
        #[arg(long)]
        trace_root: Option<std::path::PathBuf>,
        /// Emit the full message sequence as JSON on stdout
        #[arg(long)]
        json: bool,
        /// The prompt; read from stdin when omitted
        prompt: Vec<String>,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> miette::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Commands::Chat {
            provider,
            model,
            base_url,
            max_tokens,
            max_turns,
            trace_root,
            json,
            prompt,
        } => {
            // Validate the provider name before any prompt resolution — a
            // bare `cadmus chat --provider bogus` must fail fast instead of
            // blocking on stdin.
            if !cadmus_llm_openai::dialect_names().contains(&provider.as_str()) {
                return Err(Error::UnknownProvider(provider).into());
            }
            let config = ChatConfig {
                provider,
                model,
                base_url,
                max_tokens,
                max_turns,
                trace_root,
            };
            let prompt = resolve_prompt(&prompt)?;
            let result = cadmus::run_chat(&prompt, &config).await?;

            for warning in &result.warnings {
                tracing::warn!(%warning, "turn warning");
            }
            if let Some(usage) = &result.usage {
                tracing::info!(
                    input = usage.input,
                    output = usage.output,
                    reasoning = usage.reasoning,
                    turns = result.turns,
                    "run finished"
                );
            }

            if json {
                let out = serde_json::json!({
                    "messages": result.messages,
                    "turns": result.turns,
                    "usage": result.usage,
                    "warnings": result.warnings,
                    "trace_id": result.trace_id,
                    "trace_path": result.trace_path,
                });
                println!("{out}");
            } else {
                println!("{}", result.final_text);
            }
            Ok(())
        }
    }
}

/// Trailing arguments joined with spaces; when empty, stdin is the prompt.
fn resolve_prompt(arguments: &[String]) -> cadmus::Result<String> {
    let prompt = if arguments.is_empty() {
        std::io::read_to_string(std::io::stdin()).map_err(cadmus::Error::ReadPrompt)?
    } else {
        arguments.join(" ")
    };
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(cadmus::Error::PromptRequired);
    }
    Ok(prompt)
}

/// Logs always go to stderr; default level is `warn`, override with `RUST_LOG`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
