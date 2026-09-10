//! Wiring for the context pipeline (ADR-0007): the filesystem side core
//! never touches — loading the instruction-file chain, probing git state,
//! and tracking newly-entered subtrees for nested instruction files.
//! Assembly and rendering are `cadmus-core`'s pure logic; this module only
//! reads the world and hands over values.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cadmus_contract::{InstructionFile, ToolCall};
use cadmus_core::context::{GitStatus, InstructionTracker, StatusProbe};

/// Which files the chain may include (ADR-0007 item 1(a)).
pub enum InstructionScope {
    /// Interactive runs: the user-global file plus ancestors root→cwd.
    UserAndWorkspace,
    /// Eval runs: workspace files only — the operator's user-global file
    /// would make scores depend on the machine the eval runs on.
    WorkspaceOnly,
}

/// Loads the frozen-prefix instruction chain (ADR-0007 item 1(a) and the
/// 2026-09-10 amendment): only files named exactly `AGENTS.md` — no
/// vendor-specific fallbacks (the universal-standards-first rule).
/// `UserAndWorkspace` orders user-global first, then ancestors
/// filesystem-root → cwd, so precedence rises down the chain (a file nearer
/// the edited path wins); `WorkspaceOnly` reads the workspace root's own
/// file and nothing else — hermetic for eval. Unreadable or oversized files
/// are skipped with a warning: a broken instruction file must never block
/// the run, but the skip is never silent.
#[must_use]
pub fn instruction_chain(root: &Path, scope: &InstructionScope) -> Vec<InstructionFile> {
    let mut files = Vec::new();
    if let InstructionScope::UserAndWorkspace = scope {
        if let Some(path) = user_global_instructions() {
            load(&mut files, &path);
        }
        let ancestors: Vec<&Path> = root.ancestors().collect();
        for dir in ancestors.into_iter().rev() {
            load(&mut files, &dir.join("AGENTS.md"));
        }
    } else {
        load(&mut files, &root.join("AGENTS.md"));
    }
    files
}

/// One instruction file's size ceiling: these bytes ride every request's
/// system prompt for the whole run (and the trajectory), so the ADR-0007
/// layer-1 bounding discipline applies here too — 256 KiB is far past any
/// real instruction file, and the model can still read an oversized one
/// explicitly via the tools.
const MAX_INSTRUCTION_BYTES: u64 = 256 * 1024;

/// Reads one file into the chain, honoring the cap; `None`-equivalent
/// outcomes (absent, unreadable, oversized) never fail, but the abnormal
/// ones always warn.
fn load(files: &mut Vec<InstructionFile>, path: &Path) {
    if let Some(content) = read_capped(path) {
        files.push(InstructionFile {
            path: path.display().to_string(),
            content,
        });
    }
}

fn read_capped(path: &Path) -> Option<String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_INSTRUCTION_BYTES => {
            tracing::warn!(path = %path.display(), bytes = meta.len(), "skipping oversized instruction file");
            None
        }
        Ok(_) => match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "skipping unreadable instruction file");
                None
            }
        },
        Err(_) => None,
    }
}

/// The user-global instruction file: `$XDG_CONFIG_HOME/cadmus/AGENTS.md`,
/// then `~/.config/cadmus/AGENTS.md`, then
/// `%USERPROFILE%/AppData/Roaming/cadmus/AGENTS.md`. Hand-rolled under the
/// zero-new-dependency policy, mirroring `default_trace_root`.
fn user_global_instructions() -> Option<PathBuf> {
    fn env(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    if let Some(xdg) = env("XDG_CONFIG_HOME") {
        return Some(xdg.join("cadmus/AGENTS.md"));
    }
    if let Some(home) = env("HOME") {
        return Some(home.join(".config/cadmus/AGENTS.md"));
    }
    env("USERPROFILE").map(|profile| profile.join("AppData/Roaming/cadmus/AGENTS.md"))
}

/// The git freshness probe: one `git status` per request render, parsed
/// into the trailer's bounded scalars (branch + dirty count — never a file
/// list, ADR-0007's 2026-09-10 amendment). Any failure (not a work tree, no
/// git binary) degrades to `None`, and the trailer simply omits the line.
pub struct GitProbe {
    root: PathBuf,
}

impl GitProbe {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl StatusProbe for GitProbe {
    fn snapshot(&self) -> Option<GitStatus> {
        let output = std::process::Command::new("git")
            .args(["status", "--porcelain=v1", "--branch"])
            .current_dir(&self.root)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_status(&output.stdout)
    }
}

/// Parses `git status --porcelain=v1 --branch`: the `## branch[...]` header
/// (kept verbatim — detached HEAD reads `HEAD (no branch)`, which is honest)
/// plus one dirty entry per remaining line (untracked included).
fn parse_status(stdout: &[u8]) -> Option<GitStatus> {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines();
    // `..` is illegal in git ref names, so the upstream marker `...` can
    // never be part of the branch itself.
    let branch = lines
        .next()?
        .strip_prefix("## ")?
        .split("...")
        .next()?
        .to_string();
    let dirty_count = lines.filter(|line| !line.trim().is_empty()).count();
    Some(GitStatus {
        branch,
        dirty_count,
    })
}

/// The nested-instruction tracker (ADR-0007 item 1(a)): when a tool call
/// touches a subtree below the workspace root, every not-yet-injected
/// `AGENTS.md` between the root and the target is returned outer-first —
/// matching the chain's precedence convention. Only the root's own file is
/// excluded, because the prefix chain already carries it.
pub struct NestedInstructions {
    root: PathBuf,
    injected: Mutex<HashSet<PathBuf>>,
}

impl NestedInstructions {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            injected: Mutex::new(HashSet::new()),
        }
    }
}

impl InstructionTracker for NestedInstructions {
    fn on_calls(&self, calls: &[ToolCall]) -> Vec<InstructionFile> {
        let mut found = Vec::new();
        let mut injected = self.injected.lock().expect("injected set poisoned");
        for call in calls {
            // The built-in tools share one path-argument convention
            // (`path`); calls without one cannot enter a subtree.
            let Some(raw) = call
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let Ok(target) = crate::tools::resolve(&self.root, raw) else {
                continue;
            };
            let dir = if target.is_dir() {
                target
            } else {
                target
                    .parent()
                    .map_or_else(|| self.root.clone(), Path::to_path_buf)
            };
            let mut chain = Vec::new();
            let mut cursor = dir.as_path();
            while cursor != self.root && cursor.starts_with(&self.root) {
                let candidate = cursor.join("AGENTS.md");
                if candidate.is_file()
                    && let Ok(canonical) = candidate.canonicalize()
                    && !injected.contains(&canonical)
                {
                    chain.push(canonical);
                }
                let Some(parent) = cursor.parent() else { break };
                cursor = parent;
            }
            // Outer files first — the chain's precedence order.
            for path in chain.into_iter().rev() {
                // Insert only after a successful read: a failed read warns
                // (never silent) and stays retryable on the next call,
                // rather than skipping the file for the rest of the run.
                if let Some(content) = read_capped(&path) {
                    injected.insert(path.clone());
                    found.push(InstructionFile {
                        path: path.display().to_string(),
                        content,
                    });
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A scratch tree under the OS temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cadmus-context-{name}-{}", std::process::id()));
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

    #[test]
    fn chain_collects_ancestors_root_to_cwd() {
        let scratch = Scratch::new("chain");
        let root = scratch.0.join("repo/nested/deep");
        std::fs::create_dir_all(&root).expect("mkdirs");
        std::fs::write(scratch.0.join("repo/AGENTS.md"), "outer").expect("write");
        std::fs::write(scratch.0.join("repo/nested/AGENTS.md"), "inner").expect("write");

        let chain = instruction_chain(&root, &InstructionScope::UserAndWorkspace);
        let contents: Vec<&str> = chain.iter().map(|file| file.content.as_str()).collect();
        // Whatever the machine contributes (user-global, stray ancestors),
        // the workspace ancestors always close the chain, outer → cwd.
        assert_eq!(
            contents[contents.len() - 2..],
            vec!["outer", "inner"],
            "outer first, cwd last"
        );
    }

    #[test]
    fn workspace_only_scope_is_hermetic_to_ancestors() {
        let scratch = Scratch::new("hermetic");
        let root = scratch.0.join("eval-workspace");
        std::fs::create_dir_all(&root).expect("mkdirs");
        std::fs::write(scratch.0.join("AGENTS.md"), "ancestor rules").expect("write");
        std::fs::write(root.join("AGENTS.md"), "fixture rules").expect("write");

        let chain = instruction_chain(&root, &InstructionScope::WorkspaceOnly);
        let contents: Vec<&str> = chain.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["fixture rules"],
            "only the workspace root's own file — never an ancestor"
        );
    }

    #[test]
    fn git_probe_reads_branch_and_dirty_count() {
        let Some(()) = git_available() else {
            eprintln!("git unavailable; skipping probe test");
            return;
        };
        let scratch = Scratch::new("git");
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&scratch.0)
                .status()
                .expect("git runs");
            assert!(status.success());
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@cadmus"]);
        run(&["config", "user.name", "cadmus test"]);
        std::fs::write(scratch.0.join("tracked.txt"), "v1").expect("write");
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);

        let probe = GitProbe::new(scratch.0.clone());
        let clean = probe.snapshot().expect("a repo yields a snapshot");
        assert_eq!(
            clean,
            GitStatus {
                branch: "main".into(),
                dirty_count: 0
            }
        );

        std::fs::write(scratch.0.join("tracked.txt"), "v2").expect("edit");
        std::fs::write(scratch.0.join("untracked.txt"), "new").expect("write");
        let dirty = probe.snapshot().expect("snapshot");
        assert_eq!(dirty.dirty_count, 2, "modified + untracked");
    }

    #[test]
    fn git_probe_degrades_to_none_outside_a_repo() {
        let scratch = Scratch::new("nogit");
        let probe = GitProbe::new(scratch.0.clone());
        assert_eq!(probe.snapshot(), None);
    }

    fn git_available() -> Option<()> {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| ())
    }

    #[test]
    fn tracker_injects_subtree_files_once_outer_first() {
        let scratch = Scratch::new("nested");
        let root = scratch.0.canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("a/b")).expect("mkdirs");
        std::fs::write(root.join("AGENTS.md"), "root rules").expect("write");
        std::fs::write(root.join("a/AGENTS.md"), "a rules").expect("write");
        std::fs::write(root.join("a/b/AGENTS.md"), "b rules").expect("write");
        let tracker = NestedInstructions::new(root.clone());

        let call = |path: &str| ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": path}),
        };
        // The root's own file is already in the prefix chain — never injected.
        let first = tracker.on_calls(&[call("a/b/file.rs")]);
        let contents: Vec<&str> = first.iter().map(|file| file.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["a rules", "b rules"],
            "outer first, root skipped"
        );

        // Idempotent across calls and batches.
        assert!(tracker.on_calls(&[call("a/other.rs")]).is_empty());

        // A call without a path argument enters nothing.
        let no_path = ToolCall {
            id: "c2".into(),
            name: "todo_write".into(),
            arguments: json!({"items": []}),
        };
        assert!(tracker.on_calls(&[no_path]).is_empty());
    }

    #[test]
    fn parse_status_handles_detached_head() {
        let status = parse_status(b"## HEAD (no branch)\n M file.rs\n?? new.rs\n");
        assert_eq!(
            status,
            Some(GitStatus {
                branch: "HEAD (no branch)".into(),
                dirty_count: 2,
            })
        );
    }
}
