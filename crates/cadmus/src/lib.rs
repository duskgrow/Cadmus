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
mod tools;

pub use chat::{ChatConfig, ChatResult, run_chat};
pub use error::{Error, Result};
pub use eval::{EvalConfig, corpus_digest, load_cases, run_eval};
pub use tools::coding_tools;
