//! Smoke check for agent-facing docs ("can the host even load it?"):
//!
//! - every `.agents/skills/<name>/SKILL.md` has spec-valid frontmatter (the
//!   shared validator in `cadmus-core/src/skills/frontmatter.rs`: required
//!   `name`/`description`, the spec's name charset and length windows, name
//!   == directory name — the same rules the runtime loader applies, so the
//!   repo can never ship a skill its own agent refuses), whose keys stay
//!   within [`ALLOWED_KEYS`], and whose body fits the
//!   progressive-disclosure budget;
//! - `.claude/skills` points at `.agents/skills` (single source of truth —
//!   tolerates git's text-file fallback on platforms without symlink support);
//! - `AGENTS.md` exists, stays within the always-on line budget, and carries
//!   its freshness note;
//! - `CLAUDE.md` is only a reference to AGENTS.md, never a second copy.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

// The frontmatter parser is shared with the runtime skill loader: xtask's
// zero-dependency policy (crates/xtask/src/arch.rs) forbids a Cargo edge to
// cadmus-core, so the same source file compiles into this crate via #[path]
// — std-only and free of crate:: references by construction (see the file's
// module doc). Issues come back structured; this checker prepends the
// repo-relative path, keeping its failure messages byte-identical.
#[path = "../../cadmus-core/src/skills/frontmatter.rs"]
mod frontmatter;

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
    let (frontmatter_text, body) = match frontmatter::split_frontmatter(&text) {
        Ok(parts) => parts,
        Err(reason) => {
            failures.push(format!("{rel}: {reason}"));
            return;
        }
    };

    let (frontmatter, issues) = frontmatter::parse_frontmatter(&frontmatter_text);
    failures.extend(issues.iter().map(|issue| format!("{rel}: {issue}")));

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
    if let Err(reason) = frontmatter::validate(&frontmatter, &dir_name) {
        failures.push(format!("{rel}: {reason}"));
    }

    if body.chars().count() > SKILL_BODY_BUDGET_CHARS {
        failures.push(format!(
            "{rel}: body is {} chars, exceeding the {SKILL_BODY_BUDGET_CHARS}-char budget — move detail into reference files inside the skill directory",
            body.chars().count()
        ));
    }
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
