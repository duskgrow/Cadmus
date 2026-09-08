use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, ToolError};
use serde_json::{Value, json};

use super::{error, resolve};

const MAX_GREP_FILE_BYTES: u64 = 1024 * 1024;
const MAX_GREP_MATCHES: usize = 50;
/// Never descended into, even when visible: build output and VCS internals
/// dwarf any useful payload.
const SKIP_DIRS: [&str; 2] = ["target", ".git"];

/// `grep`: literal substring search, recursive, skipping hidden entries plus
/// `target/`/`.git/`, capped at 50 matches.
pub(super) struct Grep {
    pub(super) root: PathBuf,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::{Scratch, tool};

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
}
