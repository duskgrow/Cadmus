//! Skill discovery (ADR-0006): the filesystem side of the Agent Skills
//! format — scanning the standard directories, validating frontmatter and
//! loading bodies eagerly. Everything here is wiring: parsing and
//! validation are `cadmus-core`'s pure logic; this module only reads the
//! world and hands over values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cadmus_contract::SkillSummary;
use cadmus_core::skills;

use crate::context::{Scope, read_capped};

/// One discovered skill with its body eagerly loaded: the run freezes its
/// prefix at start, and the same discipline applies here — skill files are
/// small, and a mid-run file change must never mutate a frozen run.
pub struct LoadedSkill {
    /// The level-1 catalog entry (`name` + `description`): rendered into
    /// the frozen prefix and recorded in the run's `PrefixRecord`.
    pub summary: SkillSummary,
    /// The SKILL.md body — the level-2 payload the `skill` tool injects on
    /// activation.
    pub body: String,
    /// The skill's directory; the activation result names it so bundled
    /// resources (scripts/, references/, assets/) stay reachable.
    pub root: PathBuf,
}

/// Discovers the run's skills per the scope (ADR-0006 + the
/// universal-standards-first rule): interactive runs scan the user-level
/// `~/.agents/skills` and the workspace's `.agents/skills`; eval runs scan
/// the workspace alone — the operator's machine must never leak into
/// scores. On a name conflict the workspace skill wins (proximity
/// precedence, same as the AGENTS.md chain). The catalog comes back sorted
/// by name so the prefix bytes are deterministic.
#[must_use]
pub fn discover(root: &Path, scope: &Scope) -> Vec<LoadedSkill> {
    discover_in(root, user_dir_for(scope).as_deref())
}

/// The scope's user-level root: interactive runs get the operator's
/// `~/.agents/skills`; eval forfeits it unconditionally. The seal is a
/// property of this mapping — pinned by the tests — never of the machine
/// the run happens on.
fn user_dir_for(scope: &Scope) -> Option<PathBuf> {
    match scope {
        Scope::UserAndWorkspace => user_skills_dir(),
        Scope::WorkspaceOnly => None,
    }
}

/// The scan with the user-level directory explicit, so tests never touch
/// the real home directory.
fn discover_in(root: &Path, user_dir: Option<&Path>) -> Vec<LoadedSkill> {
    let mut by_name = BTreeMap::new();
    if let Some(dir) = user_dir {
        scan(dir, &mut by_name);
    }
    // Scanned second: the map insert makes the workspace win name conflicts
    // (proximity precedence).
    scan(&root.join(".agents/skills"), &mut by_name);
    by_name.into_values().collect()
}

/// The user-level skills root: `~/.agents/skills` (the Agent Skills
/// convention, verified 2026-09-10 — deliberately NOT the XDG config path
/// the user-global AGENTS.md uses; the standard owns this one). Hand-rolled
/// under the zero-new-dependency policy, mirroring `user_global_instructions`.
fn user_skills_dir() -> Option<PathBuf> {
    fn env(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    if let Some(home) = env("HOME") {
        return Some(home.join(".agents/skills"));
    }
    env("USERPROFILE").map(|profile| profile.join(".agents/skills"))
}

/// Scans one skills root: every immediate subdirectory carrying a SKILL.md
/// is a candidate. Non-directory entries are not skills and skip silently;
/// a directory without SKILL.md is a broken skill and warns; an invalid one
/// warns and skips. Sorted entries keep the warn order deterministic.
fn scan(dir: &Path, by_name: &mut BTreeMap<String, LoadedSkill>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No skills root is the normal case, not a finding; any other
        // read failure warns (never silent).
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::warn!(path = %dir.display(), %err, "skipping unreadable skills root");
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            tracing::warn!(path = %path.display(), "skipping skill directory without SKILL.md");
            continue;
        }
        if let Some(skill) = load(&path, &skill_md) {
            by_name.insert(skill.summary.name.clone(), skill);
        }
    }
}

/// Loads one candidate: cap check, frontmatter split and parse, spec
/// validation. Every failure warns and skips — a broken skill must never
/// block the run, but the skip is never silent.
fn load(dir: &Path, skill_md: &Path) -> Option<LoadedSkill> {
    let text = read_capped(skill_md, "SKILL.md")?;
    let (frontmatter_text, body) = match skills::split_frontmatter(&text) {
        Ok(parts) => parts,
        Err(reason) => {
            tracing::warn!(path = %skill_md.display(), "skipping skill: {reason}");
            return None;
        }
    };
    let (frontmatter, issues) = skills::parse_frontmatter(&frontmatter_text);
    if !issues.is_empty() {
        for issue in &issues {
            tracing::warn!(path = %skill_md.display(), %issue, "skipping skill with invalid frontmatter");
        }
        return None;
    }
    let dir_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let summary = match skills::validate(&frontmatter, &dir_name) {
        Ok(summary) => summary,
        Err(reason) => {
            tracing::warn!(path = %skill_md.display(), "skipping skill: {reason}");
            return None;
        }
    };
    Some(LoadedSkill {
        summary,
        body,
        root: dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree under the OS temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cadmus-skills-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create scratch");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes `<base>/.agents/skills/<name>/SKILL.md`.
    fn write_skill(base: &Path, name: &str, skill_md: &str) {
        let dir = base.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).expect("mkdirs");
        std::fs::write(dir.join("SKILL.md"), skill_md).expect("write");
    }

    fn valid_skill(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\nbody of {name}\n")
    }

    fn names(skills: &[LoadedSkill]) -> Vec<&str> {
        skills
            .iter()
            .map(|skill| skill.summary.name.as_str())
            .collect()
    }

    #[test]
    fn the_workspace_wins_name_conflicts_and_the_catalog_is_sorted() {
        let scratch = Scratch::new("precedence");
        let user = scratch.0.join("user");
        let repo = scratch.0.join("repo");
        // The user root's entries stand alone (no .agents/skills segment) —
        // discover_in takes the skills root directly.
        std::fs::create_dir_all(user.join("shared")).expect("mkdirs");
        std::fs::write(
            user.join("shared/SKILL.md"),
            valid_skill("shared", "user version"),
        )
        .expect("write");
        std::fs::create_dir_all(user.join("user-only")).expect("mkdirs");
        std::fs::write(
            user.join("user-only/SKILL.md"),
            valid_skill("user-only", "u"),
        )
        .expect("write");
        write_skill(&repo, "shared", &valid_skill("shared", "project version"));
        write_skill(&repo, "project-only", &valid_skill("project-only", "p"));

        let skills = discover_in(&repo, Some(&user));
        assert_eq!(names(&skills), vec!["project-only", "shared", "user-only"]);
        let shared = &skills[1];
        assert_eq!(shared.summary.description, "project version");
        assert!(shared.root.starts_with(&repo), "the workspace copy won");
        assert_eq!(shared.body, "body of shared\n");
    }

    #[test]
    fn workspace_only_scope_never_reads_the_user_level() {
        // The eval seal: whatever the operator's machine holds at
        // ~/.agents/skills, WorkspaceOnly must not see it — pinned at the
        // mapping so a regression fails on ANY machine, not only on one
        // with a populated user level.
        assert!(user_dir_for(&Scope::WorkspaceOnly).is_none());

        let scratch = Scratch::new("sealed");
        let repo = scratch.0.join("fixture");
        write_skill(&repo, "fixture-skill", &valid_skill("fixture-skill", "f"));

        let skills = discover(&repo, &Scope::WorkspaceOnly);
        assert_eq!(names(&skills), vec!["fixture-skill"]);
    }

    #[test]
    fn invalid_skills_warn_and_skip() {
        let scratch = Scratch::new("invalid");
        let repo = scratch.0.join("repo");
        // name != directory name
        write_skill(&repo, "dir-name", &valid_skill("other-name", "d"));
        // missing description
        write_skill(&repo, "no-desc", "---\nname: no-desc\n---\nbody\n");
        // charset violation
        write_skill(&repo, "bad-case", &valid_skill("Bad_Case", "d"));
        // oversized description
        write_skill(
            &repo,
            "long-desc",
            &format!(
                "---\nname: long-desc\ndescription: {}\n---\nbody\n",
                "d".repeat(1025)
            ),
        );
        // unparseable frontmatter (unquoted colon-space)
        write_skill(
            &repo,
            "bad-yaml",
            "---\nname: bad-yaml\ndescription: a: b\n---\nbody\n",
        );
        // no frontmatter at all
        write_skill(&repo, "no-frontmatter", "just a body\n");
        // one valid skill survives the cull
        write_skill(&repo, "good", &valid_skill("good", "g"));

        let skills = discover_in(&repo, None);
        assert_eq!(names(&skills), vec!["good"]);
    }

    #[test]
    fn an_oversized_skill_file_is_skipped() {
        let scratch = Scratch::new("oversized");
        let repo = scratch.0.join("repo");
        let big_body = "x".repeat(300 * 1024);
        write_skill(
            &repo,
            "huge",
            &format!("---\nname: huge\ndescription: d\n---\n{big_body}"),
        );

        let skills = discover_in(&repo, None);
        assert!(skills.is_empty());
    }

    #[test]
    fn non_skill_entries_skip_without_loading() {
        let scratch = Scratch::new("entries");
        let repo = scratch.0.join("repo");
        let skills_dir = repo.join(".agents/skills");
        std::fs::create_dir_all(skills_dir.join("empty-dir")).expect("mkdirs");
        std::fs::write(skills_dir.join("README.md"), "not a skill").expect("write");
        write_skill(&repo, "real", &valid_skill("real", "r"));

        let skills = discover_in(&repo, None);
        assert_eq!(names(&skills), vec!["real"]);
    }

    #[test]
    fn a_crlf_skill_file_loads() {
        let scratch = Scratch::new("crlf");
        let repo = scratch.0.join("repo");
        write_skill(
            &repo,
            "crlf",
            "---\r\nname: crlf\r\ndescription: d\r\n---\r\nbody\r\n",
        );

        let skills = discover_in(&repo, None);
        assert_eq!(names(&skills), vec!["crlf"]);
        assert_eq!(skills[0].body, "body\n");
    }
}
