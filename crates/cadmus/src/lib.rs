//! Core library of `cadmus`.
//!
//! The binary (`src/main.rs`) is a thin CLI shell over this library: keep
//! logic here so it stays testable without spawning a process.

mod chat;
mod error;
mod telemetry;
mod tools;

pub use chat::{ChatConfig, ChatResult, run_chat};
pub use error::{Error, Result};
pub use tools::coding_tools;
