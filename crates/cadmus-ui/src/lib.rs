//! The design system and the content pipelines (ADR-0017, ADR-0018 item 2).
//!
//! Two jobs, one renderer-agnostic crate: own the design tokens, palettes,
//! theme loader, icon registry and motion scales; and turn content
//! (streaming markdown, code fences, diffs) into a **semantic-style IR** —
//! spans tagged with ADR-0017's slot roles, never renderer types.
//! `cadmus-tui` maps the IR onto ratatui lines; the future GUI maps it onto
//! its rich-text equivalent. The hard problems (incremental parse, highlight
//! state, diff computation) are solved once, here.
