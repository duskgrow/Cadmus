//! Boundary contract of Cadmus: port traits, wire types, [`Capabilities`],
//! [`ModelProfile`], the trajectory event schema (ADR-0005) and the eval set
//! schema ([`EvalCase`], ADR-0005 §7 / ADR-0010 §4).
//!
//! This is the only crate where serializable boundary types live (ADR-0002):
//! the core never `use`s an external capability directly, and adapters never
//! invent their own wire types. Everything here is plain data plus the
//! [`Provider`] port — logic lives in `cadmus-core`, IO in the adapters.

mod approval;
mod capabilities;
mod context;
mod error;
mod eval;
mod event;
mod live;
mod log;
mod message;
mod profile;
mod provider;
mod request;
mod state;
mod stream;
pub mod testing;

pub use approval::Approval;
pub use capabilities::{CacheSupport, Capabilities, ReasoningCaps, SoSupport, Support};
pub use context::{InstructionFile, PrefixRecord, SkillSummary, TodoItem, TodoStatus};
pub use error::ModelError;
pub use eval::{CaseResult, EvalCase, EvalReport, EvalSplit, Expectation};
pub use event::{
    Clock, Command, EstimateSource, Event, EventError, EventKind, FoldedRef, IdSequence,
    ScoreEvent, Status, SteerMode, TurnOutcome, attrs, error_kinds,
};
pub use live::{
    Attachment, CallSnapshot, CommandSource, InFlight, LiveItem, LiveKind, LiveSink, LiveUpdate,
    OpenTurn, PendingApproval, Sync, TurnSnapshot,
};
pub use log::{ArtifactSink, EventSink, LogError};
pub use message::{ContentPart, Message, Role, ToolCall};
pub use profile::{CacheHints, FewShotFormat, ModelProfile, ToolDescriptionStyle};
pub use provider::{ChunkStream, Provider};
pub use request::{
    CacheDirective, ChatRequest, EffortLevel, OutputMode, Reasoning, Sampling, ToolChoice, ToolSpec,
};
pub use state::{FinishRecord, RunState};
pub use stream::{FinishReason, StreamChunk, Usage};
#[doc(inline)]
pub use testing::{ContractSubject, QueuedResponse};
