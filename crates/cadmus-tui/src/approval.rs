//! The interactive approval surface (ADR-0018 item 8 / ADR-0011 item 3):
//! while the run's gate awaits a decision, the band hosts the pending
//! request between the stream tail and the composer — a header naming it,
//! one marker line per gated call, and each call's proposed change as diff
//! lines (the `diff-*` slots, ADR-0017).
//!
//! The call → diff-lines mapping is pure: it reads [`ToolCall::arguments`]
//! only and never the workspace. A cumulative, file-backed diff (the
//! proposed change rendered against the file's current content) is a
//! deliberate follow-up: it needs workspace reads, and this crate keeps IO
//! injected at the app boundary (AGENTS.md) — the checkpoint/diff slice's
//! already-injected file access is its natural owner.
//!
//! Generation is budgeted and cached, not per-frame: each call's diff
//! contribution is capped at the section's row cap (`DIFF_LINES_PER_CALL` —
//! a clipped tail names itself and waits for the file-backed slice), and
//! the app materializes [`section_lines`] once per request — the render
//! re-wraps the cached lines at frame rate, where work proportional to a
//! huge `write_file` would otherwise burn per pump.

use cadmus_contract::{PendingApproval, ToolCall};
use cadmus_ui::diff::diff_lines;
use cadmus_ui::ir::{self, Slot};
use cadmus_ui::theme::{ColorDepth, Theme};
use ratatui::text::Line;

use crate::layout::APPROVAL_MAX_ROWS;
use crate::transcript::{subtle_line, tool_marker};
use crate::wrap::wrap_rows;

/// One call's diff contribution is capped at the section's absolute row cap:
/// the render head-clips the section to that many rows anyway, so generating
/// past it would be pure waste — and the materialized lines are re-wrapped
/// at frame rate, where work proportional to a huge `write_file` would burn
/// per pump. The clipped tail says so and waits for the cumulative,
/// file-backed diff slice.
const DIFF_LINES_PER_CALL: usize = APPROVAL_MAX_ROWS as usize;

/// The pending request's display rows at `width` — what the band renders
/// while a request waits. `None` (nothing pending) renders no rows. The app
/// wraps once per pump and feeds the count to the height function and the
/// rows to the band render, so the two can never disagree (the transcript
/// snapshot contract's shape).
#[must_use]
pub fn section_rows(
    pending: Option<&PendingApproval>,
    width: u16,
    theme: &Theme,
    depth: ColorDepth,
) -> Vec<Line<'static>> {
    let Some(pending) = pending else {
        return Vec::new();
    };
    wrap_rows(&section_lines(pending), width, theme, depth)
}

/// The section's logical lines: the header, then per call the marker line
/// (the transcript's shape — one quiet line, name plus primary target)
/// followed by its proposed change.
///
/// The app materializes this ONCE per pending request (at enqueue and at
/// sync-reseed), not per frame: the diff is `O(content)`, and the render
/// re-wraps at frame rate against a cached copy — only width changes (or a
/// new request) recompute. The per-call budget bounds the re-wrap work.
#[must_use]
pub fn section_lines(pending: &PendingApproval) -> Vec<ir::Line> {
    let mut lines = vec![header(pending.calls.len())];
    for call in &pending.calls {
        lines.push(subtle_line(tool_marker(call)));
        lines.extend(change_lines(call));
    }
    lines
}

/// The header names the request and the keys that answer it; the name rides
/// the accent slot, the key hint stays subtle (accent restraint, ADR-0017 —
/// the accent marks the request, nothing else).
fn header(call_count: usize) -> ir::Line {
    ir::Line::from_spans(vec![
        ir::Span::slotted(format!("approve {call_count} call(s)"), Slot::Accent),
        ir::Span::slotted("  y: approve · n: reject", Slot::TextSubtle),
    ])
}

/// The call's proposed change as diff lines (module docs for the purity
/// boundary): `write_file` renders its `content` against the empty file
/// (all-added); `edit_file` renders one small diff per `{old_string,
/// new_string}` pair; every other tool contributes none — its marker line
/// stands alone.
fn change_lines(call: &ToolCall) -> Vec<ir::Line> {
    let Some(args) = call.arguments.as_object() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    match call.name.as_str() {
        "write_file" => {
            let Some(content) = args.get("content").and_then(|value| value.as_str()) else {
                return Vec::new();
            };
            push_capped(&mut lines, "", content);
        }
        "edit_file" => {
            let Some(edits) = args.get("edits").and_then(|value| value.as_array()) else {
                return Vec::new();
            };
            for edit in edits {
                let old = edit.get("old_string").and_then(|value| value.as_str());
                let new = edit.get("new_string").and_then(|value| value.as_str());
                let (Some(old), Some(new)) = (old, new) else {
                    continue;
                };
                push_capped(&mut lines, old, new);
            }
        }
        _ => {}
    }
    lines
}

/// Append one pair's diff under the per-call budget: past the cap the kept
/// head stops and a quiet marker names what was clipped (the render
/// head-clips to the same cap, so the marker is the last visible row of a
/// truncated contribution).
fn push_capped(lines: &mut Vec<ir::Line>, old: &str, new: &str) {
    let diff = diff_lines(old, new);
    if diff.len() <= DIFF_LINES_PER_CALL {
        lines.extend(diff);
        return;
    }
    let remaining = diff.len() - DIFF_LINES_PER_CALL;
    lines.extend(diff.into_iter().take(DIFF_LINES_PER_CALL));
    lines.push(ir::Line::from_spans(vec![ir::Span::slotted(
        format!("⋯ {remaining} more line(s)"),
        Slot::TextSubtle,
    )]));
}

#[cfg(test)]
mod tests {
    use cadmus_contract::ToolCall;
    use serde_json::json;

    use super::*;
    use crate::test_util::texts;

    fn pending(calls: Vec<ToolCall>) -> PendingApproval {
        PendingApproval {
            request_id: "ap1".into(),
            turn: 1,
            calls,
        }
    }

    fn section(pending: &PendingApproval) -> Vec<String> {
        texts(&section_rows(
            Some(pending),
            80,
            &Theme::ansi(),
            ColorDepth::Truecolor,
        ))
    }

    #[test]
    fn no_pending_request_renders_no_rows() {
        assert!(section_rows(None, 80, &Theme::ansi(), ColorDepth::Truecolor).is_empty());
    }

    #[test]
    fn the_header_names_the_request_and_the_answer_keys() {
        let rows = section(&pending(vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({}),
        }]));
        assert_eq!(rows[0], "approve 1 call(s)  y: approve · n: reject");
    }

    #[test]
    fn a_write_file_call_renders_its_content_all_added() {
        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "src/main.rs", "content": "fn main() {\n    run();\n}\n"}),
        };
        let rows = section(&pending(vec![call]));
        assert_eq!(
            rows,
            vec![
                "approve 1 call(s)  y: approve · n: reject",
                "→ write_file src/main.rs",
                "+ fn main() {",
                "+     run();",
                "+ }",
            ]
        );
        // The proposed change rides the added slot.
        let wrapped = section_rows(
            Some(&pending(vec![ToolCall {
                id: "c1".into(),
                name: "write_file".into(),
                arguments: json!({"path": "src/main.rs", "content": "new line\n"}),
            }])),
            80,
            &Theme::ansi(),
            ColorDepth::Truecolor,
        );
        let added = &wrapped[2];
        assert_eq!(added.spans[1].style.fg, Some(ratatui::style::Color::Green));
    }

    #[test]
    fn an_edit_file_call_renders_one_diff_per_edit_pair() {
        let call = ToolCall {
            id: "c1".into(),
            name: "edit_file".into(),
            arguments: json!({
                "path": "src/main.rs",
                "edits": [
                    {"old_string": "let a = 1;", "new_string": "let a = 2;"},
                    {"old_string": "fn old() {}", "new_string": "fn new() {}"},
                ],
            }),
        };
        let rows = section(&pending(vec![call]));
        assert_eq!(
            rows,
            vec![
                "approve 1 call(s)  y: approve · n: reject",
                "→ edit_file src/main.rs",
                "- let a = 1;",
                "+ let a = 2;",
                "- fn old() {}",
                "+ fn new() {}",
            ]
        );
    }

    #[test]
    fn a_huge_write_is_capped_with_a_truncation_marker() {
        use std::fmt::Write as _;

        let mut content = String::new();
        for n in 1..=30 {
            let _ = writeln!(content, "line {n}");
        }
        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "big.rs", "content": content}),
        };
        let lines = section_lines(&pending(vec![call]));
        // Header, the marker line, the capped head, and the truncation note.
        assert_eq!(lines.len(), 2 + DIFF_LINES_PER_CALL + 1);
        assert_eq!(
            lines.last().expect("the truncation note").text(),
            format!("⋯ {} more line(s)", 30 - DIFF_LINES_PER_CALL)
        );
    }

    #[test]
    fn a_call_without_a_renderable_shape_contributes_no_diff() {
        // A non-write tool's marker line stands alone.
        let rows = section(&pending(vec![ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments: json!({"command": "cargo test"}),
        }]));
        assert_eq!(
            rows,
            vec![
                "approve 1 call(s)  y: approve · n: reject",
                "→ bash cargo test"
            ]
        );
        // Malformed write arguments contribute nothing either.
        let rows = section(&pending(vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "src/main.rs"}),
        }]));
        assert_eq!(
            rows,
            vec![
                "approve 1 call(s)  y: approve · n: reject",
                "→ write_file src/main.rs"
            ]
        );
    }
}
