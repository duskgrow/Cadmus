//! Context-pipeline wire types (ADR-0007): the frozen-prefix record that
//! keeps a trace self-sufficient for exact request reconstruction, and the
//! todo-list vocabulary the status trailer renders. Pure data — the assembly
//! and rendering logic lives in `cadmus-core`.

use serde::{Deserialize, Serialize};

/// One workspace-instruction file (`AGENTS.md`): part of the frozen prefix
/// chain, or injected mid-run when the agent first touches its subtree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstructionFile {
    /// Absolute path — provenance only; the content travels inline (a trace
    /// never depends on the filesystem state it ran against, ADR-0005).
    pub path: String,
    pub content: String,
}

/// The frozen prefix as recorded on the start-run command: everything the
/// per-turn request render prepends, so replay reconstructs the exact
/// context the model saw (ADR-0007 item 3) without re-reading the
/// filesystem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefixRecord {
    /// The change-detection key over the assembled bytes plus the tool
    /// specs — the ADR-0010 eval-pairing comparability key, also carried as
    /// the [`PREFIX_HASH`](crate::attrs::PREFIX_HASH) attribute.
    pub hash: String,
    /// The assembled system-message text (system prompt + instruction
    /// chain + skill catalog rendered in), exactly as sent.
    pub system: String,
    /// The structured provenance of `system` (phase 2's reflector consumes
    /// it); empty when no instruction files applied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<InstructionFile>,
    /// The skill catalog's structured provenance (ADR-0006); empty when no
    /// skills were discovered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillSummary>,
}

/// One skill's level-1 catalog entry (ADR-0006's progressive disclosure):
/// name+description is the always-loaded discovery unit; the body stays on
/// disk until the `skill` tool activates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

/// One todo-list item (ADR-0007 item 1(c)'s model-authored exception):
/// written by `todo_write`, stored and rendered verbatim by code — never
/// recomputed or summarized by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// The task, imperative wording.
    pub content: String,
    pub status: TodoStatus,
    /// Present-continuous wording for the in-progress display (Claude Code
    /// TaskCreate-compatible). The TUI spinner is its consumer; the trailer
    /// render ignores it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "activeForm"
    )]
    pub active_form: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_item_wire_shape_matches_the_tool_schema() {
        // The tool-call arguments are the wire: camelCase `activeForm`,
        // absent when unset; `status` in snake_case.
        let item: TodoItem = serde_json::from_str(r#"{"content":"do x","status":"in_progress"}"#)
            .expect("parses without activeForm");
        assert_eq!(item.active_form, None);
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::json!({"content":"do x","status":"in_progress"}),
            "no activeForm key when unset"
        );

        let full: TodoItem =
            serde_json::from_str(r#"{"content":"do x","status":"pending","activeForm":"doing x"}"#)
                .expect("parses with activeForm");
        assert_eq!(full.active_form.as_deref(), Some("doing x"));
        assert_eq!(
            serde_json::to_value(&full).unwrap()["activeForm"],
            serde_json::json!("doing x")
        );
    }

    #[test]
    fn prefix_record_skills_are_additive() {
        // Empty catalogs serialize to the pre-skills wire shape, and traces
        // recorded before the field existed still parse (ADR-0005's
        // additive-event rule).
        let record = PrefixRecord {
            hash: "h".into(),
            system: "s".into(),
            instructions: Vec::new(),
            skills: Vec::new(),
        };
        let wire = serde_json::to_value(&record).unwrap();
        assert!(wire.get("skills").is_none(), "empty catalog stays absent");
        let parsed: PrefixRecord =
            serde_json::from_value(serde_json::json!({"hash": "h", "system": "s"}))
                .expect("a pre-skills record still parses");
        assert!(parsed.skills.is_empty());
    }
}
