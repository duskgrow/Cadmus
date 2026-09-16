//! The terminal frontend (ADR-0011 floor v2, ADR-0018).
//!
//! A client of the ADR-0013 client protocol: protocol events flow into a
//! view-model materialization and widgets read materialized state only — no
//! god-enum of UI-internal events (the Codex lesson, ADR-0018 item 10).
//! Rendering maps `cadmus-ui`'s semantic-style IR onto ratatui; the crate
//! owns the event loop (actor pair, capped frame rate, input never blocks),
//! the mode × key input layer (keymap as data) and the self-built composer.
//! It sees the contract and the IR — never core internals.
//!
//! - [`shell`] — the inline shell: owns the raw terminal and the band's
//!   lifecycle (anchor, dynamic height via `Terminal` recreation, resize
//!   reflow, guarded draws — ADR-0018's 2026-09-14 amendments).
//! - [`composer`] — the self-built multiline prompt editor: grapheme-correct
//!   cursor/word ops, selection, bounded snapshot undo, hard-wrap layout
//!   with cursor placement, and the paste-burst heuristic (ADR-0018 item 6,
//!   ADR-0012's editor-grade-input floor).
//! - [`stream`] — the stream widget: the band's live tail over `cadmus-ui`'s
//!   markdown pipeline, owning the flush contract with the shell (ADR-0018
//!   items 2 and 4).
//! - [`style`] — the IR → ratatui style mapping, incl. color-depth
//!   degradation (ADR-0017 item 5).
//! - [`layout`] — the band's height function and widget split rules (the
//!   2026-09-14 amendments' composer cap and short-window corner), pure.
//! - [`frame`] — the event loop's redraw half: the [`frame::FrameRequester`]/
//!   [`frame::FrameScheduler`] actor pair coalescing and capping frames at
//!   120 FPS, demand-driven (ADR-0018 item 5).
//! - [`input`] — the input broker: owns the crossterm event stream; the
//!   quiesce seam for shell (re)construction and `$EDITOR` handoff (ADR-0018
//!   item 5).
//! - [`debounce`] — resize-burst coalescing cadence (inline-spike
//!   discipline 2).
//! - [`transcript`] — the view-model materialization (item 10): protocol
//!   events in, widget-readable rows out; one snapshot per pump batch
//!   drives flush, band render and layout.
//! - [`wrap`] — the one wrap implementation (ratatui's own word wrapper):
//!   flush rows, band rows and height math can never disagree.
//! - [`app`] — the event loop driving it all: one `select!` over input,
//!   live feed, run outcome, resize debounce and draw ticks; the draw pump
//!   and the real-terminal boot.

pub mod app;
pub mod composer;
pub mod debounce;
pub mod frame;
pub mod input;
pub mod layout;
pub mod shell;
pub mod stream;
pub mod style;
pub mod transcript;
pub mod wrap;
