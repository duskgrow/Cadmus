//! Read-only coding tools, confined to a workspace root (phase 0 scope; the
//! Landlock sandbox is phase 3, report §7). Paths resolving outside the root
//! are tool errors — feedback the model can recover from, never a fatal error.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, ToolError};
use serde_json::{Value, json};

const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_GREP_FILE_BYTES: u64 = 1024 * 1024;
const MAX_GREP_MATCHES: usize = 50;
const MAX_LIST_ENTRIES: usize = 200;
/// Never descended into, even when visible: build output and VCS internals
/// dwarf any useful payload.
const SKIP_DIRS: [&str; 2] = ["target", ".git"];

/// The phase-0 coding toolset: `read_file`, `grep`, `list_dir`.
#[must_use]
pub fn coding_tools(root: PathBuf) -> Vec<Arc<dyn AgentTool>> {
    let root = canonical_root(root);
    vec![
        Arc::new(ReadFile { root: root.clone() }),
        Arc::new(Grep { root: root.clone() }),
        Arc::new(ListDir { root }),
    ]
}

fn canonical_root(root: PathBuf) -> PathBuf {
    root.canonicalize().unwrap_or(root)
}

/// Resolves `path` against `root` and confines it: the canonical result must
/// stay inside the root. Absolute paths are honored only if they still point
/// into the workspace.
fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    // Canonicalize the deepest existing ancestor and re-attach the missing
    // tail, so confinement also holds for not-yet-existing paths (write tools
    // arrive in later phases) instead of failing with a bare IO error.
    let mut probe = candidate.as_path();
    let mut missing = Vec::new();
    let base = loop {
        match probe.canonicalize() {
            Ok(base) => break base,
            Err(_) => match probe.file_name() {
                Some(name) => {
                    missing.push(name.to_owned());
                    probe = probe.parent().expect("parent exists above a file name");
                }
                None => return Err(format!("cannot resolve `{path}`")),
            },
        }
    };
    let resolved = missing.iter().rev().fold(base, |mut path, name| {
        path.push(name);
        path
    });
    if !resolved.starts_with(root) {
        return Err(format!("`{path}` resolves outside the workspace"));
    }
    Ok(resolved)
}

fn error(tool: &str, message: String) -> ToolError {
    ToolError {
        tool: tool.to_string(),
        message,
    }
}

/// `read_file`: UTF-8 text with optional 1-based line window, capped at
/// 512 KiB.
struct ReadFile {
    root: PathBuf,
}

#[async_trait]
impl AgentTool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from the workspace. Optionally restrict to a \
                          1-based line window with offset/limit. Files are capped at 512 KiB."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "path relative to the workspace root"},
                    "offset": {"type": "integer", "description": "first line to read (1-based, default 1)"},
                    "limit": {"type": "integer", "description": "maximum number of lines to read"},
                },
                "required": ["path"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let path = arguments["path"].as_str().unwrap_or_default();
        let canonical = resolve(&self.root, path).map_err(|message| error("read_file", message))?;
        let metadata = fs::metadata(&canonical)
            .map_err(|err| error("read_file", format!("cannot stat `{path}`: {err}")))?;
        if !metadata.is_file() {
            return Err(error("read_file", format!("`{path}` is not a file")));
        }

        let truncated = metadata.len() > MAX_FILE_BYTES;
        let text = read_head(&canonical, MAX_FILE_BYTES)
            .map_err(|err| error("read_file", format!("cannot read `{path}`: {err}")))?;

        let offset = arguments["offset"]
            .as_u64()
            .unwrap_or(1)
            .max(1)
            .try_into()
            .unwrap_or(usize::MAX);
        let limit: usize = arguments["limit"]
            .as_u64()
            .map_or(usize::MAX, |value| value.try_into().unwrap_or(usize::MAX));
        let lines: Vec<&str> = text.lines().collect();
        let total_lines = lines.len();
        let window: Vec<&str> = lines
            .into_iter()
            .skip(offset.saturating_sub(1))
            .take(limit)
            .collect();

        let mut output = window.join("\n");
        if truncated {
            output.push_str("\n… [truncated at 512 KiB]");
        }
        if offset > 1 || window.len() < total_lines {
            let _ = write!(
                output,
                "\n… [lines {offset}–{} of {total_lines}]",
                offset + window.len().saturating_sub(1)
            );
        }
        Ok(Value::String(output))
    }
}

/// `read_to_string` with a byte cap, so a multi-GB file cannot stall the loop.
fn read_head(path: &Path, cap: u64) -> std::io::Result<String> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut handle = file.take(cap + 1);
    let mut buffer = Vec::new();
    handle.read_to_end(&mut buffer)?;
    buffer.truncate(cap.try_into().unwrap_or(usize::MAX));
    String::from_utf8(buffer)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a UTF-8 text file"))
}

/// `grep`: literal substring search, recursive, skipping hidden entries plus
/// `target/`/`.git/`, capped at 50 matches.
struct Grep {
    root: PathBuf,
}

#[async_trait]
impl AgentTool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search workspace files for a literal substring (not a regex). Recursive \
                          from path (default: workspace root); hidden entries, target/ and .git/ \
                          are skipped. Returns `path:line: text`, at most 50 matches."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "literal substring to search for"},
                    "path": {"type": "string", "description": "directory to search (default: workspace root)"},
                },
                "required": ["pattern"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let pattern = arguments["pattern"].as_str().unwrap_or_default();
        if pattern.is_empty() {
            return Err(error("grep", "pattern must not be empty".into()));
        }
        let base = arguments["path"].as_str().unwrap_or(".");
        let canonical = resolve(&self.root, base).map_err(|message| error("grep", message))?;

        let mut matches = Vec::new();
        let mut files = Vec::new();
        collect_files(&canonical, &mut files);
        for file in files {
            if matches.len() >= MAX_GREP_MATCHES {
                break;
            }
            let Ok(metadata) = fs::metadata(&file) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_GREP_FILE_BYTES {
                continue;
            }
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            // Agent-facing paths use `/` on every OS (`Path::display` would
            // emit `\` on Windows).
            let display = file
                .strip_prefix(&self.root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            for (line_number, line) in text.lines().enumerate() {
                if line.contains(pattern) {
                    matches.push(format!("{display}:{}: {line}", line_number + 1));
                    if matches.len() >= MAX_GREP_MATCHES {
                        break;
                    }
                }
            }
        }

        if matches.is_empty() {
            return Ok(Value::String(format!("no matches for `{pattern}`")));
        }
        let mut output = matches.join("\n");
        if matches.len() >= MAX_GREP_MATCHES {
            let _ = write!(output, "\n… [stopped at {MAX_GREP_MATCHES} matches]");
        }
        Ok(Value::String(output))
    }
}

/// Depth-first recursive file collection, sorted for deterministic output.
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// `list_dir`: one level of a workspace directory, sorted, `[dir]`/`[file]`
/// prefixes, capped at 200 entries.
struct ListDir {
    root: PathBuf,
}

#[async_trait]
impl AgentTool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List one level of a workspace directory (default: workspace root), \
                          sorted, with [dir]/[file] prefixes. At most 200 entries."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "directory to list (default: workspace root)"},
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

        let mut entries: Vec<_> = fs::read_dir(&canonical)
            .map_err(|err| error("list_dir", format!("cannot list `{base}`: {err}")))?
            .flatten()
            .collect();
        entries.sort_by_key(fs::DirEntry::file_name);

        let total = entries.len();
        let mut lines = Vec::new();
        for entry in entries.into_iter().take(MAX_LIST_ENTRIES) {
            let kind = if entry.path().is_dir() {
                "[dir]"
            } else {
                "[file]"
            };
            lines.push(format!("{kind} {}", entry.file_name().to_string_lossy()));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch workspace under the OS temp dir, unique per test name and
    /// process, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cadmus-tools-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create scratch");
            Self(root)
        }

        fn write(&self, path: &str, contents: &str) {
            let full = self.0.join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdirs");
            fs::write(full, contents).expect("write");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tool(root: &Path, name: &str) -> Arc<dyn AgentTool> {
        coding_tools(root.to_path_buf())
            .into_iter()
            .find(|tool| tool.spec().name == name)
            .expect("tool exists")
    }

    #[tokio::test]
    async fn read_file_returns_contents_with_line_window() {
        let scratch = Scratch::new("read-window");
        scratch.write("src/main.rs", "fn main() {\n    // TODO\n}\n");
        let read_file = tool(&scratch.0, "read_file");

        let full = read_file
            .invoke(json!({"path": "src/main.rs"}))
            .await
            .expect("read");
        assert_eq!(full, json!("fn main() {\n    // TODO\n}"));

        let window = read_file
            .invoke(json!({"path": "src/main.rs", "offset": 2, "limit": 1}))
            .await
            .expect("read window");
        let text = window.as_str().expect("string");
        assert!(text.starts_with("    // TODO"), "got: {text}");
        assert!(text.contains("lines 2–2 of 3"), "got: {text}");
    }

    #[tokio::test]
    async fn tools_refuse_paths_outside_the_workspace() {
        let scratch = Scratch::new("confined");
        scratch.write("inside.txt", "safe");
        let read_file = tool(&scratch.0, "read_file");
        let grep = tool(&scratch.0, "grep");

        let err = read_file
            .invoke(json!({"path": "../escape.txt"}))
            .await
            .expect_err("must be confined");
        assert!(err.message.contains("outside the workspace"));

        let err = grep
            .invoke(json!({"pattern": "x", "path": "/"}))
            .await
            .expect_err("absolute escape must be confined");
        assert!(err.message.contains("outside the workspace"));
    }

    #[tokio::test]
    async fn grep_finds_sorted_matches_and_skips_target() {
        let scratch = Scratch::new("grep");
        scratch.write("a.rs", "let x = 1;\nlet y = 2;\n");
        scratch.write("sub/b.rs", "let x = 3;\n");
        scratch.write("target/c.rs", "let x = 4;\n");
        let grep = tool(&scratch.0, "grep");

        let result = grep
            .invoke(json!({"pattern": "let x"}))
            .await
            .expect("grep");
        let text = result.as_str().expect("string");
        assert_eq!(text, "a.rs:1: let x = 1;\nsub/b.rs:1: let x = 3;");
    }

    #[tokio::test]
    async fn list_dir_marks_kinds() {
        let scratch = Scratch::new("list");
        scratch.write("file.txt", "x");
        scratch.write("dir/nested.txt", "y");
        let list_dir = tool(&scratch.0, "list_dir");

        let result = list_dir.invoke(json!({})).await.expect("list");
        assert_eq!(result, json!("[dir] dir\n[file] file.txt"));
    }
}
