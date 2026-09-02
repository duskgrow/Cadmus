//! `OpenAI`-compatible provider adapter + per-vendor wire dialects (ADR-0003).
//!
//! Provider customization splits into two layers with different change
//! mechanisms: the **dialect modules** in this crate are code (endpoint
//! configuration, tool-call delta-aggregation quirks, reasoning-field naming,
//! structured-output degradation path); **`ModelProfile`** prompt affinity is
//! versioned config data living in `cadmus-contract`. A new provider means a
//! new dialect module plus passing the contract test suite — recorded-replay,
//! never live calls in CI.
//!
//! Transport and SSE frame parsing come from the `genai` thin client; the
//! aggregation semantics stay in `cadmus-core` (report §4.1.3).

mod dialect;
mod dialects;
mod error;
mod map;
mod provider;
mod stream_map;

pub use dialect::Dialect;
pub use dialects::{DeepSeekDialect, KimiDialect, RelayGptDialect};
pub use provider::OpenAiProvider;
