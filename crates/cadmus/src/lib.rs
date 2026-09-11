//! Core library of `cadmus`.
//!
//! The binary (`src/main.rs`) is a thin CLI shell over this library: keep
//! logic here so it stays testable without spawning a process.

mod approval;
mod chat;
mod context;
mod error;
mod eval;
pub mod provider;
mod render;
mod skills;
mod telemetry;
#[cfg(test)]
mod test_support;
mod tools;

pub use chat::{ChatConfig, ChatResult, run_chat};
pub use error::{Error, Result};
pub use eval::{EvalConfig, corpus_digest, load_cases, run_eval};
pub use tools::coding_tools;

/// A non-empty environment variable as a path — an empty value counts as
/// unset (an empty XDG variable means "use the default", not the root).
/// Shared by the hand-rolled path resolutions (trace root, user-global
/// instructions, user skills dir), which deliberately differ above this
/// primitive: XDG data vs XDG config vs the Agent-Skills convention.
pub(crate) fn env_path(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}
