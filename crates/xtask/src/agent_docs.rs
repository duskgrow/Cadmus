//! Smoke check for agent-facing docs ("can the host even load it?"):
//!
//! - every `.agents/skills/<name>/SKILL.md` has frontmatter whose `name` equals
//!   the directory name, whose `description` is 1..=1024 chars, whose keys stay
//!   within [`ALLOWED_KEYS`], and whose body fits the progressive-disclosure budget;
//! - `.claude/skills` points at `.agents/skills` (single source of truth —
//!   tolerates git's text-file fallback on platforms without symlink support);
//! - `AGENTS.md` exists, stays within the always-on line budget, and carries
//!   its freshness note;
//! - `CLAUDE.md` is only a reference to AGENTS.md, never a second copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The repository root is baked in at compile time, so the check works from
/// any working directory (hooks, CI jobs and the bootstrap app alike).
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

// ~5k tokens of body text per skill: level-2 progressive-disclosure budget
const SKILL_BODY_BUDGET_CHARS: usize = 20_000;
const AGENTS_LINE_BUDGET: usize = 250;
// The SKILL.md standard guarantees only `name` and `description` are loaded at
// level 1; anything else would be metadata no host reads (see ADR-0005).
const ALLOWED_KEYS: [&str; 5] = [
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
];

/// Entry point: `agent-check` (no arguments).
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("usage: agent-check  (takes no arguments)");
        return ExitCode::from(2);
    }
    let root = Path::new(ROOT);
    let mut failures: Vec<String> = Vec::new();

    let mut skill_files: Vec<PathBuf> = Vec::new();
    let skills_dir = root.join(".agents/skills");
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("SKILL.md");
            if candidate.is_file() {
                skill_files.push(candidate);
            }
        }
    }
    skill_files.sort();
    if skill_files.is_empty() {
        failures
            .push(".agents/skills: no skills found (expected at least one */SKILL.md)".to_string());
    }
    for skill_file in &skill_files {
        check_skill(skill_file, root, &mut failures);
    }

    check_claude_skills_link(root, &mut failures);
    check_agents_md(root, &mut failures);
    check_claude_md(root, &mut failures);

    if failures.is_empty() {
        println!(
            "agent-doc smoke check ok: {} skill(s), AGENTS.md, CLAUDE.md, .claude/skills",
            skill_files.len()
        );
        return ExitCode::SUCCESS;
    }
    eprintln!("agent-doc smoke check FAILED:");
    for failure in &failures {
        eprintln!("  - {failure}");
    }
    ExitCode::FAILURE
}

fn check_skill(path: &Path, root: &Path, failures: &mut Vec<String>) {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let Ok(text) = std::fs::read_to_string(path) else {
        failures.push(format!("{rel}: unreadable"));
        return;
    };
    let (frontmatter_text, body) = match split_frontmatter(&text) {
        Ok(parts) => parts,
        Err(reason) => {
            failures.push(format!("{rel}: {reason}"));
            return;
        }
    };

    let frontmatter = parse_frontmatter(&frontmatter_text, &rel, failures);

    let unknown: Vec<&String> = frontmatter
        .keys()
        .filter(|key| !ALLOWED_KEYS.contains(&key.as_str()))
        .collect();
    if !unknown.is_empty() {
        failures.push(format!(
            "{rel}: unsupported frontmatter keys {unknown:?} — only {ALLOWED_KEYS:?} are allowed; activation conditions belong in `description`, not custom fields"
        ));
    }

    let dir_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match frontmatter.get("name") {
        None => failures.push(format!("{rel}: frontmatter requires `name`")),
        Some(name) if *name != dir_name => failures.push(format!(
            "{rel}: name {name:?} must equal directory name {dir_name:?}"
        )),
        Some(_) => {}
    }

    match frontmatter.get("description") {
        None => failures.push(format!("{rel}: frontmatter requires `description`")),
        Some(description) if !(1..=1024).contains(&description.chars().count()) => {
            failures.push(format!(
                "{rel}: description must be 1..1024 chars (got {})",
                description.chars().count()
            ));
        }
        Some(_) => {}
    }

    if body.chars().count() > SKILL_BODY_BUDGET_CHARS {
        failures.push(format!(
            "{rel}: body is {} chars, exceeding the {SKILL_BODY_BUDGET_CHARS}-char budget — move detail into reference files inside the skill directory",
            body.chars().count()
        ));
    }
}

/// Split raw file text into (frontmatter, body), keeping the two failure
/// modes apart for actionable messages. Windows checkouts may carry CRLF
/// line endings (git autocrlf); YAML hosts accept both, so normalize before
/// the line-oriented split.
fn split_frontmatter(text: &str) -> Result<(String, String), &'static str> {
    let text = text.replace("\r\n", "\n");
    let Some(rest) = text.strip_prefix("---\n") else {
        return Err("missing YAML frontmatter (must start with ---)");
    };
    let Some(end) = rest.find("\n---\n") else {
        return Err("frontmatter is not closed with ---");
    };
    Ok((rest[..end].to_string(), rest[end + 5..].to_string()))
}

/// Parse the single-line `key: value` subset this checker supports. There is
/// no YAML parser in std, so validate against what strict YAML hosts reject:
/// an unquoted `: ` (or trailing `:`) ends the scalar there, and a leading
/// indicator char starts a construct this line-based format forbids.
fn parse_frontmatter(text: &str, rel: &str, failures: &mut Vec<String>) -> HashMap<String, String> {
    let mut frontmatter = HashMap::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            failures.push(format!("{rel}: unparseable frontmatter line: {line:?}"));
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        if value.starts_with('"') || value.starts_with('\'') {
            let quote = value.as_bytes()[0];
            if value.len() < 2 || !value.ends_with(char::from(quote)) {
                failures.push(format!("{rel}: unterminated quoted scalar for {key:?}"));
                continue;
            }
        } else if !value.is_empty() {
            if value.contains(": ") || value.ends_with(':') {
                failures.push(format!(
                    "{rel}: plain scalar for {key:?} contains ':' — strict YAML hosts reject this; wrap the value in double quotes"
                ));
                continue;
            }
            if ">|&*!%@`[{".contains(value.as_bytes()[0] as char) {
                failures.push(format!(
                    "{rel}: value for {key:?} starts with a YAML indicator — only single-line plain or quoted scalars are supported in SKILL.md frontmatter"
                ));
                continue;
            }
        }
        frontmatter.insert(key, value.trim_matches(['"', '\'']).to_string());
    }
    frontmatter
}

fn check_claude_skills_link(root: &Path, failures: &mut Vec<String>) {
    let link = root.join(".claude/skills");
    let expected = Path::new("../.agents/skills");
    match std::fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.file_type().is_symlink() => match std::fs::read_link(&link) {
            Ok(target) if target == expected => {}
            Ok(target) => failures.push(format!(
                ".claude/skills: symlink must point to ../.agents/skills (got {})",
                target.display()
            )),
            Err(err) => failures.push(format!(".claude/skills: unreadable symlink: {err}")),
        },
        Ok(metadata) if metadata.is_file() => {
            // git on platforms without symlink support materializes a text
            // file containing the link target; accept that degradation.
            match std::fs::read_to_string(&link) {
                Ok(content) if content.trim() == "../.agents/skills" => {}
                Ok(content) => failures.push(format!(
                    ".claude/skills: unexpected pointer content {content:?}"
                )),
                Err(err) => failures.push(format!(".claude/skills: unreadable: {err}")),
            }
        }
        _ => failures
            .push(".claude/skills: missing — it must symlink to ../.agents/skills".to_string()),
    }
}

fn check_agents_md(root: &Path, failures: &mut Vec<String>) {
    let path = root.join("AGENTS.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        failures.push("AGENTS.md: missing".to_string());
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > AGENTS_LINE_BUDGET {
        failures.push(format!(
            "AGENTS.md: {} lines exceeds the {AGENTS_LINE_BUDGET}-line budget",
            lines.len()
        ));
    }
    if !lines
        .iter()
        .take(5)
        .any(|line| line.contains("Last reviewed"))
    {
        failures.push(
            "AGENTS.md: missing the freshness note (Last reviewed) in the header".to_string(),
        );
    }
}

fn check_claude_md(root: &Path, failures: &mut Vec<String>) {
    let path = root.join("CLAUDE.md");
    let Ok(text) = std::fs::read_to_string(&path) else {
        failures.push("CLAUDE.md: missing (must be a one-line reference to AGENTS.md)".to_string());
        return;
    };
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("<!--"))
        .collect();
    if lines != ["@AGENTS.md"] {
        failures.push(
            "CLAUDE.md: must contain only the @AGENTS.md reference (SSOT — never a copy)"
                .to_string(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (HashMap<String, String>, Vec<String>) {
        let mut failures = Vec::new();
        let frontmatter = parse_frontmatter(text, "test/SKILL.md", &mut failures);
        (frontmatter, failures)
    }

    #[test]
    fn split_tolerates_crlf_checkouts() {
        // git materializes CRLF on Windows checkouts (autocrlf); YAML hosts
        // accept both endings, so the checker must too.
        let (frontmatter, body) =
            split_frontmatter("---\r\nname: x\r\n---\r\nbody\r\n").expect("valid frontmatter");
        assert_eq!(frontmatter, "name: x");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn split_distinguishes_open_and_close_failures() {
        assert_eq!(
            split_frontmatter("name: x\n").unwrap_err(),
            "missing YAML frontmatter (must start with ---)"
        );
        assert_eq!(
            split_frontmatter("---\nname: x\n").unwrap_err(),
            "frontmatter is not closed with ---"
        );
    }

    #[test]
    fn plain_scalars_parse() {
        let (frontmatter, failures) = parse("name: pr-preflight\ndescription: short\n");
        assert!(failures.is_empty());
        assert_eq!(frontmatter["description"], "short");
    }

    #[test]
    fn unquoted_colon_space_is_rejected() {
        let (_, failures) = parse("description: layer on top: diff self-review\n");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("wrap the value in double quotes"));
    }

    #[test]
    fn quoted_colon_space_is_accepted() {
        let (frontmatter, failures) = parse("description: \"layer on top: diff\"\n");
        assert!(failures.is_empty());
        assert_eq!(frontmatter["description"], "layer on top: diff");
    }

    #[test]
    fn unterminated_quote_and_indicators_are_rejected() {
        let (_, failures) = parse("description: \"never closed\n");
        assert_eq!(failures.len(), 1);
        let (_, failures) = parse("description: >- folded\n");
        assert_eq!(failures.len(), 1);
    }
}
