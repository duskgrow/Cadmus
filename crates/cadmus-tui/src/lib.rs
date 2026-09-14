//! The terminal frontend (ADR-0011 floor v2, ADR-0018).
//!
//! A client of the ADR-0013 client protocol: protocol events flow into a
//! view-model materialization and widgets read materialized state only — no
//! god-enum of UI-internal events (the Codex lesson, ADR-0018 item 10).
//! Rendering maps `cadmus-ui`'s semantic-style IR onto ratatui; the crate
//! owns the event loop (actor pair, capped frame rate, input never blocks),
//! the mode × key input layer (keymap as data) and the self-built composer.
//! It sees the contract and the IR — never core internals.
