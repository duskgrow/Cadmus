//! The Agent Skills format (ADR-0006), pure-logic half: SKILL.md frontmatter
//! parsing and validation. Filesystem discovery lives in the `cadmus` crate
//! (the wiring side); the catalog render lives in [`crate::context`].

mod frontmatter;

use std::collections::HashMap;

use cadmus_contract::SkillSummary;

pub use frontmatter::{FrontmatterIssue, parse_frontmatter, split_frontmatter};

/// Validates one parsed SKILL.md frontmatter against the Agent Skills spec
/// and returns the level-1 catalog entry. The rules live in the shared
/// frontmatter file (`validate` there) — shared verbatim with the
/// agent-check smoke gate; this wrapper only rehomes the result into the
/// wire type. The failure text feeds the loader's warn-skip: an invalid
/// skill must never block a run, but the skip is never silent.
pub fn validate<S: ::std::hash::BuildHasher>(
    frontmatter: &HashMap<String, String, S>,
    dir_name: &str,
) -> Result<SkillSummary, String> {
    let (name, description) = frontmatter::validate(frontmatter, dir_name)?;
    Ok(SkillSummary { name, description })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_rehomes_the_validated_pair() {
        let frontmatter = HashMap::from([
            ("name".to_string(), "pr-preflight".to_string()),
            ("description".to_string(), "review a PR".to_string()),
        ]);
        let summary = validate(&frontmatter, "pr-preflight").expect("valid");
        assert_eq!(
            summary,
            SkillSummary {
                name: "pr-preflight".into(),
                description: "review a PR".into(),
            }
        );
        // The shared validator's verdict passes through unchanged.
        validate(&frontmatter, "other-dir").unwrap_err();
    }
}
