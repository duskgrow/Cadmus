//! Agent loop, stream aggregation and skill orchestration — pure logic, no IO.
//!
//! The core owns the *aggregation semantics* of normalized provider streams
//! (the thin client only solves transport and frame parsing, ADR-0003), plus
//! the agent loop itself. Time, randomness and IO are always injected as
//! constructor parameters — there is no hidden `now()`/`rand()`/global state
//! here, which is what keeps every test deterministic.

mod agent;
mod assembler;
mod replay;

pub use agent::{AgentError, AgentLoop, AgentTool, RunOutcome, ToolError};
pub use assembler::{AssembledTurn, MessageAssembler, TurnOutcome};
pub use replay::ReplayProvider;
