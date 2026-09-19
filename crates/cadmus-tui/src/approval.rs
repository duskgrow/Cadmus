//! The interactive approval surface (ADR-0018 item 8 / ADR-0011 item 3):
//! while the run's gate awaits a decision, the band hosts the pending
//! request between the stream tail and the composer — a header naming it,
//! the wait's deadline, and the keys that answer it. Tab selects a call;
//! y/n answer only that call. Its proposed change uses the `diff-*` slots
//! (ADR-0017), and siblings retain their original batch positions.
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
//! the app caches those diffs once per request. Focus and decision changes
//! rebuild the section from that cache; the render only re-wraps its lines.

use std::time::Duration;

use cadmus_contract::{Approval, PendingApproval, ToolCall};
use cadmus_ui::diff::diff_lines;
use cadmus_ui::ir::{self, Slot};

use crate::layout::APPROVAL_MAX_ROWS;
use crate::transcript::{subtle_line, tool_marker};

/// One call's diff contribution is capped at the section's absolute row cap:
/// the render head-clips the section to that many rows anyway, so generating
/// past it would be pure waste — and the materialized lines are re-wrapped
/// at frame rate, where work proportional to a huge `write_file` would burn
/// per pump. The clipped tail says so and waits for the cumulative,
/// file-backed diff slice.
const DIFF_LINES_PER_CALL: usize = APPROVAL_MAX_ROWS as usize;

/// Client-local focus and submitted commands are separate from recorded
/// decisions: a racing client or timeout may win before our answer applies.
/// All addressing retains the original batch indices, including duplicate ids.
pub(crate) struct Dialog {
    pub(crate) request: PendingApproval,
    selected: Option<usize>,
    /// Unix terminals report held keys as repeated Press events. Only Tab
    /// arms an answer; submission, remote replacement and resync disarm it.
    armed: bool,
    submitted: Vec<bool>,
    changes: Vec<Vec<ir::Line>>,
}

impl Dialog {
    pub(crate) fn new(mut request: PendingApproval) -> Self {
        request.decisions.resize(request.calls.len(), None);
        let selected = request.decisions.iter().position(Option::is_none);
        let submitted = vec![false; request.calls.len()];
        let changes = request.calls.iter().map(change_lines).collect();
        Self {
            request,
            selected,
            armed: false,
            submitted,
            changes,
        }
    }

    pub(crate) fn resync(&mut self, request: PendingApproval) {
        let submitted = std::mem::take(&mut self.submitted);
        *self = Self::new(request);
        for (slot, was_submitted) in self.submitted.iter_mut().zip(submitted) {
            *slot = was_submitted;
        }
        self.selected = (0..self.request.calls.len()).find(|&index| self.answerable(index));
    }

    fn answerable(&self, index: usize) -> bool {
        self.request.decisions[index].is_none() && !self.submitted[index]
    }

    pub(crate) fn move_focus(&mut self, backwards: bool) {
        if !self.armed && self.selected.is_some_and(|index| self.answerable(index)) {
            self.armed = true;
            return;
        }
        let len = self.request.calls.len();
        let start = self.selected.unwrap_or(0);
        self.selected = (1..=len)
            .map(|offset| {
                if backwards {
                    (start + len - offset) % len
                } else {
                    (start + offset) % len
                }
            })
            .find(|&index| self.answerable(index));
        self.armed = self.selected.is_some();
    }

    pub(crate) fn submit(&mut self) -> Option<usize> {
        if !std::mem::take(&mut self.armed) {
            return None;
        }
        let index = self.selected?;
        self.submitted[index] = true;
        self.move_focus(false);
        self.armed = false;
        Some(index)
    }

    pub(crate) fn settle(&mut self, index: usize, decision: &Approval) {
        let Some(slot) = self.request.decisions.get_mut(index) else {
            return;
        };
        if slot.is_some() {
            return;
        }
        *slot = Some(decision.clone());
        if self.selected == Some(index) {
            self.move_focus(false);
            self.armed = false;
        }
    }

    pub(crate) fn complete(&self) -> bool {
        self.request.decisions.iter().all(Option::is_some)
    }

    pub(crate) fn lines(&self) -> Vec<ir::Line> {
        if self.complete() {
            return Vec::new();
        }
        let mut lines = vec![header(&self.request)];
        lines.push(subtle_line(match (self.selected, self.armed) {
            // Every slot is submitted or recorded and completion arrives as a
            // recorded command: naming an arm key here would advertise a key
            // that does nothing (and the answer is already in flight).
            (None, _) => "waiting for the recorded decision".to_string(),
            // The answer keys ride the hint, not the header: y/n need Tab
            // first (the held-key guard), and the prerequisite has to be read
            // in the same line or the header promises an inert key. Keeping it
            // here also holds the header under an 80-column wrap.
            (Some(_), true) => {
                "Tab/Shift-Tab: next/previous call · y: approve · n: reject".to_string()
            }
            (Some(_), false) => "Tab: arm selected call · y/n: answer it".to_string(),
        }));
        // Put the focused call ahead of its siblings: head clipping must
        // never leave y/n answering a call hidden behind another call's diff.
        if let Some(index) = self.selected {
            lines.push(ir::Line::from_spans(vec![ir::Span::slotted(
                format!(
                    "> {}/{} {}",
                    index + 1,
                    self.request.calls.len(),
                    tool_marker(&self.request.calls[index])
                ),
                Slot::Accent,
            )]));
            lines.extend(self.changes[index].iter().cloned());
        }
        for (index, call) in self.request.calls.iter().enumerate() {
            if self.selected == Some(index) {
                continue;
            }
            let state = match &self.request.decisions[index] {
                Some(Approval::Approved) => "approved",
                Some(Approval::Rejected { .. }) => "rejected",
                None if self.submitted[index] => "sent",
                None => "pending",
            };
            lines.push(subtle_line(format!(
                "  {}/{} {} ({state})",
                index + 1,
                self.request.calls.len(),
                tool_marker(call)
            )));
        }
        lines
    }
}

/// The header names the request and the wait's deadline — the name rides the
/// accent slot, the deadline stays subtle (accent restraint, ADR-0017 — the
/// accent marks the request, nothing else). The answer keys live on the hint
/// row below, where their Tab prerequisite fits in one readable line.
fn header(pending: &PendingApproval) -> ir::Line {
    ir::Line::from_spans(vec![
        ir::Span::slotted(
            format!("approve {} call(s)", pending.calls.len()),
            Slot::Accent,
        ),
        ir::Span::slotted(
            format!(
                "  ·  unanswered denies after {}",
                wait_name(pending.wait_timeout)
            ),
            Slot::TextSubtle,
        ),
    ])
}

/// The deadline's static name: whole minutes read as minutes, anything else
/// as seconds. The carried value is the budget the request opened with, not
/// a live countdown — that rides a later stream slice.
fn wait_name(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
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
                if lines.len() == DIFF_LINES_PER_CALL {
                    // The previous pair filled the budget exactly and a later
                    // pair still has content: free the last row for the
                    // marker instead of overwriting a rendered diff line.
                    lines.pop();
                    lines.push(subtle_line("⋯ more changes omitted"));
                    break;
                }
                let old = edit.get("old_string").and_then(|value| value.as_str());
                let new = edit.get("new_string").and_then(|value| value.as_str());
                let (Some(old), Some(new)) = (old, new) else {
                    continue;
                };
                if !push_capped(&mut lines, old, new) {
                    break;
                }
            }
        }
        _ => {}
    }
    lines
}

/// The budget is cumulative across edit pairs, including the omission
/// marker. Calling with no room left is a caller bug (`change_lines` frees
/// the marker's row first), so the borrow saturates rather than underflowing
/// in a render path. False stops the caller before it generates any
/// subsequent diffs.
fn push_capped(lines: &mut Vec<ir::Line>, old: &str, new: &str) -> bool {
    let remaining = DIFF_LINES_PER_CALL - lines.len();
    let diff = diff_lines(old, new);
    if diff.len() <= remaining {
        lines.extend(diff);
        return true;
    }
    lines.extend(diff.into_iter().take(remaining.saturating_sub(1)));
    lines.push(subtle_line("⋯ more changes omitted"));
    false
}

#[cfg(test)]
mod tests {
    use cadmus_contract::ToolCall;
    use cadmus_ui::theme::{ColorDepth, Theme};
    use ratatui::text::Line;
    use serde_json::json;

    use super::*;
    use crate::test_util::texts;
    use crate::wrap::wrap_rows;

    fn pending(calls: Vec<ToolCall>) -> PendingApproval {
        PendingApproval {
            request_id: "ap1".into(),
            turn: 1,
            message_index: None,
            calls,
            decisions: Vec::new(),
            // The gate's real budget (HUMAN_WAIT) — the header names it.
            wait_timeout: Duration::from_secs(300),
        }
    }

    /// The section's rows at the test width — the dialog's cached lines
    /// wrapped, the shape the app's `wrap_section` feeds the band.
    fn section(pending: &PendingApproval) -> Vec<String> {
        texts(&section_rows(pending))
    }

    fn section_rows(pending: &PendingApproval) -> Vec<Line<'static>> {
        let lines = Dialog::new(pending.clone()).lines();
        wrap_rows(&lines, 80, &Theme::ansi(), ColorDepth::Truecolor)
    }

    fn dialog() -> Dialog {
        Dialog::new(pending(
            ["write_file", "edit_file", "write_file"]
                .into_iter()
                .map(|name| ToolCall {
                    id: "duplicate".into(),
                    name: name.into(),
                    arguments: json!({}),
                })
                .collect(),
        ))
    }

    #[test]
    fn focus_keeps_original_indices_and_skips_submitted_or_settled_calls() {
        let mut dialog = dialog();
        assert_eq!(dialog.submit(), None, "a new dialog is unarmed");
        dialog.move_focus(true);
        dialog.move_focus(true);
        assert_eq!(dialog.selected, Some(2));
        assert_eq!(dialog.submit(), Some(2));
        assert_eq!(dialog.selected, Some(0));
        assert_eq!(
            dialog.submit(),
            None,
            "repeated Press cannot answer a sibling"
        );
        dialog.settle(2, &Approval::Approved);
        assert_eq!(dialog.submit(), None, "recording the answer cannot rearm");
        assert_eq!(
            dialog.request.decisions,
            vec![None, None, Some(Approval::Approved)],
            "only the recorded slot is settled"
        );
        dialog.settle(0, &Approval::Rejected { comment: None });
        assert_eq!(dialog.selected, Some(1));
        dialog.move_focus(false);
        assert_eq!(dialog.selected, Some(1));
        assert_eq!(dialog.submit(), Some(1));
        assert_eq!(dialog.submit(), None);
        assert!(
            !dialog.complete(),
            "sent answers await their recorded decisions"
        );
        dialog.settle(1, &Approval::Approved);
        dialog.settle(2, &Approval::Approved);
        assert!(dialog.complete());
        assert!(dialog.lines().is_empty());
    }

    #[test]
    fn the_first_recorded_decision_wins_and_bad_indices_are_ignored() {
        let mut dialog = dialog();
        dialog.settle(usize::MAX, &Approval::Approved);
        assert_eq!(dialog.selected, Some(0));
        dialog.settle(1, &Approval::Rejected { comment: None });
        dialog.settle(1, &Approval::Approved);
        assert_eq!(
            dialog.request.decisions[1],
            Some(Approval::Rejected { comment: None })
        );
        dialog.move_focus(false);
        dialog.move_focus(false);
        assert_eq!(dialog.selected, Some(2));
    }

    #[test]
    fn resync_preserves_submissions_but_never_rearms_a_decision() {
        let mut dialog = dialog();
        dialog.move_focus(false);
        assert_eq!(dialog.submit(), Some(0));
        let request = dialog.request.clone();
        dialog.move_focus(false);
        assert!(dialog.armed);
        dialog.resync(request);
        assert_eq!(dialog.submit(), None);
        dialog.move_focus(true);
        assert_eq!(
            dialog.submit(),
            Some(1),
            "an unrecorded submission must not become answerable again"
        );
        dialog.settle(0, &Approval::Approved);
        dialog.settle(1, &Approval::Approved);
        assert_eq!(dialog.submit(), None);
    }

    #[test]
    fn a_remote_decision_disarms_the_replacement_call() {
        let mut dialog = dialog();
        dialog.move_focus(false);
        dialog.settle(0, &Approval::Approved);
        assert_eq!(dialog.selected, Some(1));
        assert_eq!(dialog.submit(), None);
        dialog.move_focus(false);
        assert_eq!(dialog.submit(), Some(1));
    }

    #[test]
    fn a_partial_attach_only_focuses_undecided_slots() {
        let mut request = dialog().request;
        request.decisions = vec![
            Some(Approval::Approved),
            None,
            Some(Approval::Rejected { comment: None }),
        ];
        let mut dialog = Dialog::new(request);
        assert_eq!(dialog.selected, Some(1));
        dialog.move_focus(true);
        assert_eq!(dialog.submit(), Some(1));
        assert_eq!(dialog.submit(), None);
    }

    #[test]
    fn focusing_a_later_call_keeps_its_diff_ahead_of_a_long_sibling() {
        let mut dialog = Dialog::new(pending(vec![
            ToolCall {
                id: "dup".into(),
                name: "write_file".into(),
                arguments: json!({"path": "first.rs", "content": "first\n".repeat(100)}),
            },
            ToolCall {
                id: "dup".into(),
                name: "write_file".into(),
                arguments: json!({"path": "second.rs", "content": "second\n"}),
            },
        ]));
        dialog.move_focus(false);
        dialog.move_focus(false);
        let rows: Vec<_> = dialog.lines().iter().map(ir::Line::text).collect();
        assert_eq!(rows[2], "> 2/2 → write_file second.rs");
        assert_eq!(rows[3], "+ second");
        assert!(!rows.iter().any(|row| row == "+ first"));
    }

    #[test]
    fn an_empty_batch_has_nothing_to_select_or_render() {
        let mut dialog = Dialog::new(pending(Vec::new()));
        dialog.move_focus(true);
        dialog.move_focus(false);
        assert_eq!(dialog.submit(), None);
        assert!(dialog.lines().is_empty());
    }

    #[test]
    fn the_header_names_the_request_the_answer_keys_and_the_deadline() {
        let rows = section(&pending(vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({}),
        }]));
        assert_eq!(
            rows[0],
            "approve 1 call(s)  ·  unanswered denies after 5 min"
        );
    }

    #[test]
    fn the_deadline_names_seconds_when_the_budget_is_not_whole_minutes() {
        let mut pending = pending(vec![ToolCall {
            id: "c1".into(),
            name: "edit_file".into(),
            arguments: json!({}),
        }]);
        pending.wait_timeout = Duration::from_secs(90);
        let rows = section(&pending);
        assert_eq!(
            rows[0],
            "approve 1 call(s)  ·  unanswered denies after 90 s"
        );
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
                "approve 1 call(s)  ·  unanswered denies after 5 min",
                "Tab: arm selected call · y/n: answer it",
                "> 1/1 → write_file src/main.rs",
                "+ fn main() {",
                "+     run();",
                "+ }",
            ]
        );
        // The proposed change rides the added slot.
        let wrapped = section_rows(&pending(vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({"path": "src/main.rs", "content": "new line\n"}),
        }]));
        let added = &wrapped[3];
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
                "approve 1 call(s)  ·  unanswered denies after 5 min",
                "Tab: arm selected call · y/n: answer it",
                "> 1/1 → edit_file src/main.rs",
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
        let lines = Dialog::new(pending(vec![call])).lines();
        // Header, navigation, focused marker; the note is inside the cap.
        assert_eq!(lines.len(), 3 + DIFF_LINES_PER_CALL);
        assert_eq!(
            lines.last().expect("the truncation note").text(),
            "⋯ more changes omitted"
        );
    }

    #[test]
    fn many_edit_pairs_share_one_diff_budget() {
        let call = ToolCall {
            id: "c1".into(),
            name: "edit_file".into(),
            arguments: json!({"edits": (0..30).map(|index| json!({
                "old_string": format!("old {index}"), "new_string": format!("new {index}")
            })).collect::<Vec<_>>() }),
        };
        let lines = change_lines(&call);
        assert_eq!(lines.len(), DIFF_LINES_PER_CALL);
        assert_eq!(lines.last().unwrap().text(), "⋯ more changes omitted");
        assert!(!lines.iter().any(|line| line.text().contains("old 6")));
        let mut prefix = vec![subtle_line("kept"); DIFF_LINES_PER_CALL - 2];
        assert!(!push_capped(&mut prefix, "a\nb\nc\n", "x\ny\nz\n"));
        assert_eq!(prefix.len(), DIFF_LINES_PER_CALL);
    }

    #[test]
    fn a_fully_submitted_dialog_says_it_waits_instead_of_arming() {
        // The last answer is in flight: no slot is answerable, so the hint must
        // not advertise a Tab that would do nothing.
        let mut dialog = Dialog::new(pending(vec![ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            arguments: json!({}),
        }]));
        dialog.move_focus(false);
        assert_eq!(dialog.submit(), Some(0));
        assert_eq!(dialog.selected, None);
        assert!(!dialog.complete(), "the recorded decision has not arrived");
        let rows: Vec<_> = dialog.lines().iter().map(ir::Line::text).collect();
        assert_eq!(rows[1], "waiting for the recorded decision");
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
                "approve 1 call(s)  ·  unanswered denies after 5 min",
                "Tab: arm selected call · y/n: answer it",
                "> 1/1 → bash cargo test"
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
                "approve 1 call(s)  ·  unanswered denies after 5 min",
                "Tab: arm selected call · y/n: answer it",
                "> 1/1 → write_file src/main.rs"
            ]
        );
    }
}
