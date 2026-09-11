//! Eval scoring (ADR-0005 §7): pure functions from a folded [`RunState`] to
//! one score per expectation. No IO — the harness in the binary owns files,
//! providers and the log; this module only reads the trajectory's fold, so
//! scoring is identically defined for a live run, a replayed trace and a
//! test fake.
//!
//! Every metric is binary (0.0 / 1.0 with `passed` set): at n≈50 the gate
//! statistics (ADR-0010 §1) work on per-item pass/fail, and partial credit
//! would only blur the pairing.

use cadmus_contract::{EvalCase, Expectation, Role, ScoreEvent, Status};

use crate::RunState;

/// Scores every expectation of `case` against the folded run, in the case's
/// order. Runs that errored mid-flight still score: `run_completed` records
/// the failure and the content metrics grade whatever the partial trace
/// holds — a crash is a 0, not an exception.
#[must_use]
pub fn score_case(case: &EvalCase, state: &RunState) -> Vec<ScoreEvent> {
    case.expect
        .iter()
        .map(|expectation| {
            let passed = match expectation {
                Expectation::RunCompleted => run_completed(state),
                Expectation::AnswerContains { needle } => answer_contains(state, needle),
                Expectation::ToolCalled { name } => tool_called(state, name),
                Expectation::TurnsWithin { max } => state.turns <= *max,
            };
            ScoreEvent {
                case_id: case.id.clone(),
                metric: expectation.metric_name().to_string(),
                score: if passed { 1.0 } else { 0.0 },
                passed: Some(passed),
            }
        })
        .collect()
}

/// Status-ok terminal record — the loop's own error paths (turn limit,
/// empty turn, provider failure) all finish the trace with an error, so a
/// missing or errored `run_finished` both mean "did not complete".
fn run_completed(state: &RunState) -> bool {
    matches!(&state.finished, Some(finish) if finish.status == Status::Ok && finish.error.is_none())
}

/// Verbatim, case-sensitive containment in the final assistant text.
fn answer_contains(state: &RunState, needle: &str) -> bool {
    final_answer_text(state).contains(needle)
}

/// The last assistant message's text parts joined — the loop ends on the
/// tool-call-free assistant turn, so the last assistant message is the
/// answer. No assistant message (a run that died before turn one) yields
/// the empty string, which contains nothing.
fn final_answer_text(state: &RunState) -> String {
    state
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(cadmus_contract::Message::text_body)
        .unwrap_or_default()
}

fn tool_called(state: &RunState, name: &str) -> bool {
    state
        .messages
        .iter()
        .flat_map(cadmus_contract::Message::tool_calls)
        .any(|call| call.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FinishRecord;
    use cadmus_contract::{ContentPart, EvalSplit, Message, ToolCall};

    fn case(expect: Vec<Expectation>) -> EvalCase {
        EvalCase {
            id: "c-1".into(),
            split: EvalSplit::Search,
            fixture: "f".into(),
            prompt: "p".into(),
            note: None,
            expect,
        }
    }

    fn finished_state(messages: Vec<Message>, turns: u32) -> RunState {
        RunState {
            trace_id: "tr-1".into(),
            provider: None,
            model: None,
            messages,
            turns,
            warnings: Vec::new(),
            scores: Vec::new(),
            dangling_tool_calls: Vec::new(),
            finished: Some(FinishRecord {
                turns,
                status: Status::Ok,
                error: None,
            }),
        }
    }

    fn crashed_state() -> RunState {
        RunState {
            trace_id: "tr-1".into(),
            provider: None,
            model: None,
            messages: Vec::new(),
            turns: 0,
            warnings: Vec::new(),
            scores: Vec::new(),
            dangling_tool_calls: Vec::new(),
            finished: None,
        }
    }

    fn scores_of(case: &EvalCase, state: &RunState) -> Vec<bool> {
        score_case(case, state)
            .iter()
            .map(|score| score.passed.expect("passed is set"))
            .collect()
    }

    #[test]
    fn every_expectation_scores_one_event_in_order() {
        let state = finished_state(
            vec![Message::text(
                cadmus_contract::Role::Assistant,
                "the answer is 42",
            )],
            2,
        );
        let case = case(vec![
            Expectation::RunCompleted,
            Expectation::AnswerContains {
                needle: "42".into(),
            },
            Expectation::AnswerContains {
                needle: "absent".into(),
            },
            Expectation::TurnsWithin { max: 3 },
        ]);

        let scores = score_case(&case, &state);
        let metrics: Vec<&str> = scores.iter().map(|s| s.metric.as_str()).collect();
        assert_eq!(
            metrics,
            [
                "run_completed",
                "answer_contains",
                "answer_contains",
                "turns_within"
            ]
        );
        assert_eq!(
            scores.iter().map(|s| s.passed).collect::<Vec<_>>(),
            [Some(true), Some(true), Some(false), Some(true)]
        );
        assert!(scores.iter().all(|s| s.case_id == "c-1"));
    }

    #[test]
    fn a_crash_scores_run_completed_zero_but_still_scores() {
        let state = crashed_state();
        let case = case(vec![
            Expectation::RunCompleted,
            Expectation::TurnsWithin { max: 4 },
        ]);
        // turns_within passes vacuously on zero turns — it is an efficiency
        // bound, and only meaningful next to run_completed.
        assert_eq!(scores_of(&case, &state), [false, true]);
    }

    #[test]
    fn answer_contains_reads_only_the_final_assistant_text() {
        let messages = vec![
            Message::user("q"),
            Message::text(Role::Assistant, "first draft mentions needle"),
            Message::text(Role::Assistant, "final answer without it"),
        ];
        let state = finished_state(messages, 2);
        let case = case(vec![Expectation::AnswerContains {
            needle: "needle".into(),
        }]);
        assert_eq!(scores_of(&case, &state), [false]);
    }

    #[test]
    fn tool_called_matches_any_assistant_turn() {
        let mut with_call = Message::text(Role::Assistant, "looking it up");
        with_call.content.push(ContentPart::ToolCall {
            call: ToolCall {
                id: "c1".into(),
                name: "grep".into(),
                arguments: serde_json::json!({"pattern": "x"}),
            },
        });
        let state = finished_state(vec![with_call, Message::text(Role::Assistant, "done")], 2);
        let case = case(vec![
            Expectation::ToolCalled {
                name: "grep".into(),
            },
            Expectation::ToolCalled {
                name: "read_file".into(),
            },
        ]);
        assert_eq!(scores_of(&case, &state), [true, false]);
    }

    #[test]
    fn an_errored_finish_is_not_completed() {
        let mut state = finished_state(Vec::new(), 1);
        state.finished = state.finished.map(|finish| FinishRecord {
            status: Status::Error,
            ..finish
        });
        let case = case(vec![Expectation::RunCompleted]);
        assert_eq!(scores_of(&case, &state), [false]);
    }
}
