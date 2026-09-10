use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, Effect, ToolError};
use serde_json::{Value, json};

use super::error;
use crate::skills::LoadedSkill;

/// `skill` (ADR-0006's progressive disclosure, level 2): activates a skill
/// by name — the body arrives as the tool result. A pure lookup over the
/// run-start scan; no filesystem access at invoke time.
pub(super) struct Skill {
    skills: Vec<LoadedSkill>,
    /// The workspace root: a skill root inside it has model-readable bundled
    /// resources; a user-level root outside it does not (the file tools'
    /// confinement) — the activation result says which.
    workspace: std::path::PathBuf,
}

impl Skill {
    pub(super) fn new(skills: Vec<LoadedSkill>, workspace: std::path::PathBuf) -> Self {
        Self { skills, workspace }
    }
}

#[async_trait]
impl AgentTool for Skill {
    /// A pure lookup over the in-memory scan: parallel-safe like every
    /// perception tool (ADR-0008 item 2).
    fn concurrency(&self) -> Concurrency {
        Concurrency::ParallelSafe
    }

    /// Reads only harness-held text: never gated (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Perception
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description:
                "Load a skill's full instructions by name. The Skills section of the system \
                 message lists each available skill with its activation condition — when the \
                 task matches one, call this BEFORE starting the work so the procedure shapes \
                 the approach; the skill body arrives as this tool's result. Do NOT activate a \
                 skill whose condition does not match the task, and never re-activate one \
                 already loaded in this conversation — its instructions are already in front of \
                 you. The name must match the listing exactly; an unknown name returns the \
                 available list."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "the skill name, exactly as listed in the Skills section (lowercase, hyphenated)"},
                },
                "required": ["name"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let name = arguments["name"].as_str().unwrap_or_default();
        if name.is_empty() {
            return Err(error(
                "skill",
                "missing required `name` — pass the skill name exactly as listed".into(),
            ));
        }
        let Some(skill) = self.skills.iter().find(|skill| skill.summary.name == name) else {
            // An unknown name is a correction, not a failure (docs/tools.md):
            // the error returns the available list so the model can retry.
            let guidance = if self.skills.is_empty() {
                "no skills are available in this run".to_string()
            } else {
                let names = self
                    .skills
                    .iter()
                    .map(|skill| skill.summary.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("available skills: {names}")
            };
            return Err(error(
                "skill",
                format!("unknown skill `{name}` — {guidance}"),
            ));
        };
        // The root line names where the skill lives; whether its bundled
        // resources are reachable depends on the file tools' confinement —
        // say which instead of over-promising for a user-level skill.
        let reach = if skill.root.starts_with(&self.workspace) {
            " — bundled resources (scripts/, references/, assets/) resolve against it"
        } else {
            " (outside the workspace — bundled resources are not reachable via the file tools)"
        };
        Ok(Value::String(format!(
            "{}\n\nSkill root: {}{reach}.",
            skill.body.trim(),
            skill.root.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cadmus_contract::SkillSummary;

    use super::*;

    fn skill(name: &str, body: &str) -> LoadedSkill {
        LoadedSkill {
            summary: SkillSummary {
                name: name.into(),
                description: format!("{name} description"),
            },
            body: body.into(),
            root: PathBuf::from(format!("/repo/.agents/skills/{name}")),
        }
    }

    fn tool(skills: Vec<LoadedSkill>) -> Skill {
        Skill::new(skills, PathBuf::from("/repo"))
    }

    #[tokio::test]
    async fn a_hit_injects_the_body_and_the_root_line() {
        let tool = tool(vec![skill("pr-preflight", "body text\n")]);
        let result = tool
            .invoke(json!({"name": "pr-preflight"}))
            .await
            .expect("a listed name hits");
        let Value::String(text) = result else {
            panic!("text result");
        };
        assert!(text.starts_with("body text\n\n"), "body first, trimmed");
        assert!(text.contains("Skill root: /repo/.agents/skills/pr-preflight"));
        assert!(text.contains("resolve against it"), "inside the workspace");
    }

    #[tokio::test]
    async fn a_user_level_root_says_its_resources_are_not_reachable() {
        let mut outside = skill("global-skill", "body");
        outside.root = PathBuf::from("/home/u/.agents/skills/global-skill");
        let tool = tool(vec![outside]);
        let result = tool
            .invoke(json!({"name": "global-skill"}))
            .await
            .expect("a listed name hits");
        let Value::String(text) = result else {
            panic!("text result");
        };
        assert!(text.contains("not reachable via the file tools"));
    }

    #[tokio::test]
    async fn an_unknown_name_is_a_correction_listing_the_available() {
        let tool = tool(vec![skill("alpha", "a"), skill("beta", "b")]);
        let err = tool
            .invoke(json!({"name": "Alpha"}))
            .await
            .expect_err("names are case-sensitive and exact");
        assert!(err.message.contains("unknown skill `Alpha`"));
        assert!(err.message.contains("available skills: alpha, beta"));
    }

    #[tokio::test]
    async fn an_empty_catalog_says_so() {
        let tool = tool(Vec::new());
        let err = tool
            .invoke(json!({"name": "anything"}))
            .await
            .expect_err("no skills to activate");
        assert!(err.message.contains("no skills are available in this run"));
    }

    #[tokio::test]
    async fn a_missing_name_is_a_correction() {
        let tool = tool(vec![skill("alpha", "a")]);
        let err = tool.invoke(json!({})).await.expect_err("name is required");
        assert!(err.message.contains("missing required `name`"));
    }

    #[test]
    fn the_declarations_match_a_pure_lookup() {
        let tool = tool(Vec::new());
        assert_eq!(tool.effect(), Effect::Perception);
        assert!(matches!(tool.concurrency(), Concurrency::ParallelSafe));
    }
}
