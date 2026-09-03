//! Agent loop, stream aggregation, trajectory replay and skill
//! orchestration — pure logic, no IO.
//!
//! The core owns the *aggregation semantics* of normalized provider streams
//! (the thin client only solves transport and frame parsing, ADR-0003), plus
//! the agent loop itself and the deterministic trajectory replayer
//! (ADR-0005). Time, randomness and IO are always injected as constructor
//! parameters — there is no hidden `now()`/`rand()`/global state here, which
//! is what keeps every test deterministic.

mod agent;
mod assembler;
mod replay;
mod trajectory;

pub use agent::{AgentError, AgentLoop, AgentTool, RunOutcome, ToolError};
pub use assembler::{AssembledTurn, MessageAssembler};
// The trajectory's llm_response events carry the outcome (ADR-0005), so the
// enum moved to the contract crate; re-exported here for continuity.
pub use cadmus_contract::TurnOutcome;
pub use replay::ReplayProvider;
pub use trajectory::{FinishRecord, RunState, replay_trace};
