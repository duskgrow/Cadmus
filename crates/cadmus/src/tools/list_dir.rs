use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, Effect, ToolError};
use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{error, resolve};

const MAX_LIST_ENTRIES: usize = 200;

/// `list_dir`: one level of a workspace directory, sorted, `[dir]`/`[file]`
/// prefixes, hidden entries filtered with grep's policy, capped at 200.
pub(super) struct ListDir {
    pub(super) root: PathBuf,
}

#[async_trait]
impl AgentTool for ListDir {
    /// Read-only and stateless: parallel-safe like every perception tool
    /// (ADR-0008 item 2).
    fn concurrency(&self) -> Concurrency {
        Concurrency::ParallelSafe
    }

    /// Reads only: never gated (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Perception
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List one level of a workspace directory (default: workspace root), \
                          sorted, with [dir]/[file] prefixes. Use this to orient when you don't \
                          know what a directory contains; to find files by content, use grep \
                          instead of listing recursively. Hidden entries are skipped by \
                          default (same per-platform policy as grep); pass `include_hidden` to \
                          include them, like `ls -a`. Naming a hidden directory explicitly \
                          lists it. A symlink shows its target's kind. At most 200 entries — \
                          when capped, the footer says so."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "directory to list (default: workspace root)"},
                    "include_hidden": {"type": "boolean", "description": "include hidden entries (default false, like ls; true is like ls -a)"},
                },
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let base = arguments["path"].as_str().unwrap_or(".");
        let canonical = resolve(&self.root, base).map_err(|message| error("list_dir", message))?;
        if !canonical.is_dir() {
            return Err(error("list_dir", format!("`{base}` is not a directory")));
        }

        let show_hidden = match arguments.get("include_hidden") {
            None => false,
            Some(value) => value.as_bool().ok_or_else(|| {
                error(
                    "list_dir",
                    format!("include_hidden must be a boolean, got {value}"),
                )
            })?,
        };
        let mut entries = visible_children(&canonical, show_hidden)
            .map_err(|err| error("list_dir", format!("cannot list `{base}`: {err}")))?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let total = entries.len();
        let lines: Vec<String> = entries
            .into_iter()
            .take(MAX_LIST_ENTRIES)
            .map(|(name, is_dir)| format!("{} {name}", if is_dir { "[dir]" } else { "[file]" }))
            .collect();
        let mut output = lines.join("\n");
        if total > MAX_LIST_ENTRIES {
            let _ = write!(
                output,
                "\n… [{total} entries, capped at {MAX_LIST_ENTRIES}]"
            );
        }
        Ok(Value::String(output))
    }
}

/// One level of `dir` as (name, `is_dir`) pairs, filtered by the same
/// hidden-entry policy grep walks with — the `ignore` crate owns the
/// per-platform semantics (dot-prefix on Unix, the hidden attribute on
/// Windows), so the two tools cannot drift apart. The walk root is exempt:
/// naming a hidden directory explicitly lists it, the same bypass grep gives
/// an explicitly named file.
fn visible_children(dir: &Path, show_hidden: bool) -> std::io::Result<Vec<(String, bool)>> {
    let mut walker = WalkBuilder::new(dir);
    // Everything off except the hidden filter: gitignore is a search-time
    // policy, not a listing one — orientation may legitimately want target/
    // or other ignored paths. The hidden filter itself is the caller's knob:
    // default `ls` behavior, `include_hidden` for `ls -a`.
    walker
        .hidden(!show_hidden)
        .follow_links(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .ignore(false)
        .parents(false)
        .max_depth(Some(1));
    let mut entries = Vec::new();
    for entry in walker.build() {
        // An unreadable directory is a tool error, never an empty listing —
        // the model must not read "permission denied" as "nothing here".
        let entry = entry.map_err(std::io::Error::other)?;
        if entry.depth() == 0 {
            continue;
        }
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path().is_dir(),
        ));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::tool;
    use crate::test_support::Scratch;

    #[tokio::test]
    async fn list_dir_marks_kinds() {
        let scratch = Scratch::new("list");
        scratch.write("file.txt", "x");
        scratch.write("dir/nested.txt", "y");
        let list_dir = tool(&scratch.0, "list_dir");

        let result = list_dir.invoke(json!({})).await.expect("list");
        assert_eq!(result, json!("[dir] dir\n[file] file.txt"));
    }

    #[tokio::test]
    async fn list_dir_skips_hidden_entries() {
        let scratch = Scratch::new("list-hidden");
        scratch.write(".secret", "x");
        scratch.write(".git/config", "x");
        scratch.write("visible.txt", "x");
        let list_dir = tool(&scratch.0, "list_dir");

        let result = list_dir.invoke(json!({})).await.expect("list");
        assert_eq!(result, json!("[file] visible.txt"));
    }

    #[tokio::test]
    async fn list_dir_rejects_a_non_boolean_hidden() {
        let scratch = Scratch::new("list-hidden-invalid");
        scratch.write("f.txt", "x\n");
        let list_dir = tool(&scratch.0, "list_dir");

        let err = list_dir
            .invoke(json!({"include_hidden": "yes"}))
            .await
            .expect_err("mistyped hidden must be a tool error");
        assert!(err.message.contains("include_hidden"), "got: {err}");
    }

    #[tokio::test]
    async fn list_dir_hidden_true_shows_hidden_entries() {
        let scratch = Scratch::new("list-hidden-opt-in");
        scratch.write(".secret", "x");
        scratch.write(".git/config", "x");
        scratch.write("visible.txt", "x");
        let list_dir = tool(&scratch.0, "list_dir");

        let result = list_dir
            .invoke(json!({"include_hidden": true}))
            .await
            .expect("list");
        assert_eq!(
            result,
            json!("[dir] .git\n[file] .secret\n[file] visible.txt")
        );
    }

    #[tokio::test]
    async fn list_dir_lists_an_explicitly_named_hidden_directory() {
        let scratch = Scratch::new("list-hidden-explicit");
        scratch.write(".config/settings.toml", "x");
        let list_dir = tool(&scratch.0, "list_dir");

        let result = list_dir
            .invoke(json!({"path": ".config"}))
            .await
            .expect("list");
        assert_eq!(result, json!("[file] settings.toml"));
    }
}
