//! Command-line shell for `cadmus`.
//!
//! Follows <https://clig.dev>: data goes to stdout, diagnostics to stderr,
//! `--json` provides machine-readable output, and `--help` is generated from
//! the clap definitions (help text is documentation with a single source).

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print a greeting
    Greet {
        /// Who to greet
        #[arg(default_value = "world")]
        name: String,
        /// Emit machine-readable JSON on stdout
        #[arg(long)]
        json: bool,
    },
}

fn main() -> miette::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Commands::Greet { name, json } => {
            let greeting = cadmus::greet(&name)?;
            if json {
                let out = serde_json::json!({ "greeting": greeting });
                println!("{out}");
            } else {
                println!("{greeting}");
            }
            Ok(())
        }
    }
}

/// Logs always go to stderr; default level is `warn`, override with `RUST_LOG`.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
