use async_trait::async_trait;
use cadmus_contract::{TodoItem, ToolSpec};
use cadmus_core::{AgentTool, Effect, ToolError};
use serde_json::{Value, json};

use super::error;

/// `todo_write` (ADR-0007 item 1(c)'s exception): the one model-authored
/// value code stores — the task plan, replaced whole on every call and
/// rendered verbatim into the status trailer by the loop. Stateless here:
/// the list lives in the loop's folded state (rebuilt from the trajectory
/// on replay), so the tool only validates and confirms.
pub(super) struct TodoWrite;

#[async_trait]
impl AgentTool for TodoWrite {
    /// Deliberately the fail-safe serial default: two whole-list replaces
    /// must never race — the later one wins, in call order.
    ///
    /// Harness-internal state only, no workspace effect: never gated
    /// (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Perception
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: cadmus_core::context::TODO_WRITE.into(),
            description:
                "Replace the task plan with a new whole list. Use this to plan multi-step \
                          work and to keep the plan current as steps complete — the list is \
                          maintained by code and rendered back to you in the [cadmus status] block \
                          of every request, so the plan of record is always in front of you. Do \
                          NOT use it for scratch notes or working data; it is the plan of record \
                          only. Never make it the sole call of a turn: update the plan alongside \
                          the action it tracks, not instead of acting."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "the complete new plan; replaces the previous list wholesale",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string", "description": "the task, in imperative wording"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                                "activeForm": {"type": "string", "description": "optional present-continuous wording shown while the task is in_progress"},
                            },
                            "required": ["content", "status"],
                        },
                    },
                },
                "required": ["items"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let items_value = arguments.get("items").cloned().unwrap_or(Value::Null);
        let items: Vec<TodoItem> = serde_json::from_value(items_value).map_err(|err| {
            error(
                cadmus_core::context::TODO_WRITE,
                format!(
                    "`items` must be a list of {{content, status, activeForm?}} with status one of \
                     pending/in_progress/completed: {err}"
                ),
            )
        })?;
        for (index, item) in items.iter().enumerate() {
            if item.content.trim().is_empty() {
                return Err(error(
                    cadmus_core::context::TODO_WRITE,
                    format!("item {} has empty content", index + 1),
                ));
            }
        }

        let count = |status| items.iter().filter(|item| item.status == status).count();
        let (pending, in_progress, completed) = (
            count(cadmus_contract::TodoStatus::Pending),
            count(cadmus_contract::TodoStatus::InProgress),
            count(cadmus_contract::TodoStatus::Completed),
        );
        Ok(Value::String(format!(
            "plan recorded: {} item(s) — {pending} pending, {in_progress} in progress, \
             {completed} completed. The [cadmus status] block of the next request shows it.",
            items.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_valid_replace_is_confirmed_with_counts() {
        let result = TodoWrite
            .invoke(json!({"items": [
                {"content": "done", "status": "completed"},
                {"content": "doing", "status": "in_progress", "activeForm": "doing it"},
                {"content": "later", "status": "pending"},
            ]}))
            .await
            .expect("valid list");
        let Value::String(text) = result else {
            panic!("text result");
        };
        assert!(text.contains("3 item(s)"));
        assert!(text.contains("1 pending"));
    }

    #[tokio::test]
    async fn malformed_items_are_corrections_not_failures() {
        let err = TodoWrite
            .invoke(json!({"items": [{"content": "x", "status": "doing"}]}))
            .await
            .expect_err("bad status is rejected");
        assert!(err.message.contains("pending/in_progress/completed"));

        let err = TodoWrite
            .invoke(json!({"items": [{"content": "  ", "status": "pending"}]}))
            .await
            .expect_err("empty content is rejected");
        assert!(err.message.contains("empty content"));

        let err = TodoWrite
            .invoke(json!({}))
            .await
            .expect_err("missing items is rejected");
        assert!(err.message.contains("items"));
    }

    #[tokio::test]
    async fn an_empty_list_is_a_legal_clear() {
        TodoWrite
            .invoke(json!({"items": []}))
            .await
            .expect("clearing the plan is valid");
    }
}
