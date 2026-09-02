//! The library's public error boundary.
//!
//! Decision rule: callers that need to *react* to a failure mode get a
//! `thiserror` enum variant; callers that only give up and report get an
//! opaque `miette::Report` (see the top level in `main.rs`).

use miette::Diagnostic;
use thiserror::Error;

/// All failure modes of the `cadmus` library.
///
/// Errors are user-facing documentation: `code` is searchable and `help`
/// gives an actionable fix. Once published, an error code is part of the
/// public interface — removing or renaming one is a `SemVer`-breaking change.
#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    /// The chat API key's environment variable was not set.
    #[error("API key environment variable {env} is not set")]
    #[diagnostic(
        code(cadmus::missing_api_key),
        help(
            "export the key first, e.g. `export {env}=sk-…` — keys travel through the environment only"
        )
    )]
    MissingApiKey { env: &'static str },

    /// An unknown `--provider` value.
    #[error(
        "--provider {provider} is not one of: {valid}",
        provider = .0,
        valid = cadmus_llm_openai::dialect_names().join(", ")
    )]
    #[diagnostic(
        code(cadmus::unknown_provider),
        help("registry vendors pin their model; `custom` takes --model and --base-url")
    )]
    UnknownProvider(String),

    /// `custom` was chosen without the endpoint coordinates.
    #[error("--provider custom requires --model and --base-url")]
    #[diagnostic(
        code(cadmus::custom_config_missing),
        help(
            "custom endpoints vary, so both are explicit: `--provider custom --model gpt-5.2 --base-url https://relay.example.com`, key via CADMUS_CUSTOM_API_KEY"
        )
    )]
    CustomConfigMissing,

    /// No prompt on the command line and nothing on stdin.
    #[error("a prompt is required")]
    #[diagnostic(
        code(cadmus::prompt_required),
        help(
            "pass the prompt as arguments or pipe it on stdin, e.g. `cadmus chat 'explain main.rs'`"
        )
    )]
    PromptRequired,

    /// Reading the prompt from stdin failed.
    #[error("cannot read the prompt from stdin: {0}")]
    #[diagnostic(
        code(cadmus::read_prompt),
        help("pass the prompt as arguments instead, e.g. `cadmus chat 'explain main.rs'`")
    )]
    ReadPrompt(std::io::Error),

    /// The provider call itself failed (auth, quota, network, wire).
    #[error("provider call failed: {0}")]
    #[diagnostic(
        code(cadmus::provider),
        help(
            "check the API key, account quota and network; rerun with RUST_LOG=debug for wire detail"
        )
    )]
    Provider(#[from] cadmus_contract::ModelError),

    /// The agent loop failed (turn limit, empty turn).
    #[error("agent run failed: {0}")]
    #[diagnostic(
        code(cadmus::agent),
        help(
            "an empty turn with finish=length means the model spent its whole budget on hidden \
              thinking — raise --max-tokens; otherwise rerun with RUST_LOG=debug"
        )
    )]
    Agent(#[from] cadmus_core::AgentError),

    /// The working directory could not be determined; the coding tools are
    /// confined to it, so running without one is impossible.
    #[error("cannot determine the working directory: {0}")]
    #[diagnostic(
        code(cadmus::workdir),
        help("run cadmus from inside the workspace the tools should see")
    )]
    Workdir(std::io::Error),
}

/// Convenience alias for library results.
pub type Result<T> = std::result::Result<T, Error>;
