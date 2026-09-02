//! Shared test support: a recorded-replay SSE stub and the contract-script →
//! OpenAI-wire translator. No live calls, ever (ADR-0003).

pub mod stub;
pub mod wire;
