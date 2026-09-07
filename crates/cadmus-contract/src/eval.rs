//! Eval set v1 schema (ADR-0005 §7): the scenario-case file format and the
//! aggregate score-file format. Cases are version-controlled data under
//! `evals/` — the root trust anchor of the evolution loop (report §2):
//! evolution artifacts never enter the set, and holdout traces never enter
//! reflection input (ADR-0010 §4 — the separation is mechanical: every score
//! event and every eval run's start-run command carries the
//! `selfevol.eval_split` attribute, see [`attrs::EVAL_SPLIT`]).
//!
//! Schema evolution follows the same additive-only discipline as the event
//! log: new optional fields and new [`Expectation`] variants may be added;
//! existing fields never change meaning.

use serde::{Deserialize, Serialize};

use crate::ScoreEvent;

/// One scenario case: a prompt run against a fixture workspace, scored by
/// its expectations. v1 cases are read-only codebase QA; edit-verify cases
/// arrive with the write tools (ADR-0008).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    /// Unique across the set, kebab-case; carried onto every score event's
    /// `case_id`.
    pub id: String,
    pub split: EvalSplit,
    /// Directory name under the fixtures root (`evals/fixtures/<fixture>`).
    pub fixture: String,
    /// The user prompt the run starts from.
    pub prompt: String,
    /// Why the expectations hold (where the answer lives in the fixture) —
    /// so a later fixture edit can re-derive them instead of guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// At least one; every expectation scores independently.
    pub expect: Vec<Expectation>,
}

/// The isolation label (ADR-0010 §3/§4). `Holdout` cases measure the
/// model+harness composite only at gate time; their traces are excluded from
/// reflection by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalSplit {
    Search,
    Holdout,
}

impl EvalSplit {
    /// The `selfevol.eval_split` attribute value.
    #[must_use]
    pub fn as_attr(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Holdout => "holdout",
        }
    }
}

/// One scored expectation. The `metric` tag value is the stable machine name
/// recorded as [`ScoreEvent::metric`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "metric", rename_all = "snake_case")]
pub enum Expectation {
    /// The run finished with status ok (a `run_finished` event without an
    /// error). The baseline every case should carry.
    RunCompleted,
    /// The final assistant message's text contains the needle — verbatim and
    /// case-sensitive, so needles are short distinctive tokens (a path, a
    /// value, a name), never full sentences.
    AnswerContains { needle: String },
    /// The named tool was invoked at least once during the run.
    ToolCalled { name: String },
    /// Completed turns stayed within the budget. An efficiency bound, never
    /// a success signal on its own: a run that crashed early has few turns.
    TurnsWithin { max: u32 },
}

impl Expectation {
    /// The stable [`ScoreEvent::metric`] value.
    #[must_use]
    pub fn metric_name(&self) -> &'static str {
        match self {
            Self::RunCompleted => "run_completed",
            Self::AnswerContains { .. } => "answer_contains",
            Self::ToolCalled { .. } => "tool_called",
            Self::TurnsWithin { .. } => "turns_within",
        }
    }
}

/// One case's outcome in a full-set run: the score file's per-case record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: String,
    pub split: EvalSplit,
    /// The run's trace — the full trajectory behind these scores.
    pub trace_id: String,
    /// One entry per expectation, in the case's order.
    pub scores: Vec<ScoreEvent>,
    /// Every expectation scored 1.0.
    pub passed: bool,
}

/// The aggregate score file written by `cadmus eval` — ADR-0005 §7's
/// acceptance artifact and the phase-2 gate's comparison input. Gates
/// compare reports only within one pinned model+harness composite
/// (ADR-0010 §2), so the composite identity is recorded here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub cadmus_version: String,
    /// Provider id as wired (`kimi`, …).
    pub provider: String,
    /// Model id as sent on the wire.
    pub model: String,
    /// FNV-1a hex digest of the corpus that produced these scores (sorted
    /// canonical case JSONs plus referenced fixture file bytes): the ruler's
    /// identity. Two reports are comparable only within one digest — and one
    /// composite; an uncommitted corpus edit between runs is visible here.
    pub set_digest: String,
    /// In case-id order (the set's deterministic run order).
    pub results: Vec<CaseResult>,
    pub passed: u32,
    pub total: u32,
}
