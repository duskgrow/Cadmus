//! Boundary contract of Cadmus: port traits, wire types, [`Capabilities`],
//! [`ModelProfile`] and the trajectory event schema (ADR-0005).
//!
//! This is the only crate where serializable boundary types live (ADR-0002):
//! the core never `use`s an external capability directly, and adapters never
//! invent their own wire types. Everything here is plain data plus the
//! [`Provider`] port — logic lives in `cadmus-core`, IO in the adapters.

mod capabilities;
mod error;
mod event;
mod log;
mod message;
mod profile;
mod provider;
mod request;
mod stream;
pub mod testing;

pub use capabilities::{CacheSupport, Capabilities, ReasoningCaps, SoSupport, Support};
pub use error::ModelError;
pub use event::{
    Clock, Command, Event, EventError, EventKind, IdSequence, ScoreEvent, Status, TurnOutcome,
    attrs,
};
pub use log::{EventSink, LogError};
pub use message::{ContentPart, Message, Role, ToolCall};
pub use profile::{CacheHints, FewShotFormat, ModelProfile, ToolDescriptionStyle};
pub use provider::{ChunkStream, Provider};
pub use request::{
    CacheDirective, ChatRequest, EffortLevel, OutputMode, Reasoning, Sampling, ToolChoice, ToolSpec,
};
pub use stream::{FinishReason, StreamChunk, Usage};
#[doc(inline)]
pub use testing::{ContractSubject, QueuedResponse};
