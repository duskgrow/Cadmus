use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, ToolError};
use serde_json::{Value, json};

use super::{error, resolve};

const MAX_LIST_ENTRIES: usize = 200;

/// `list_dir`: one level of a workspace directory, sorted, `[dir]`/`[file]`
/// prefixes, capped at 200 entries.
pub(super) struct ListDir {
    pub(super) root: PathBuf,
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
    use serde_json::json;

    use super::super::tests::{Scratch, tool};

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
