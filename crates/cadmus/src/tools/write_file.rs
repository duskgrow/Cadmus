use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, Effect, ToolError};
use serde_json::{Value, json};

use super::diff::unified_diff;
use super::{error, resolve};

/// `write_file`: create or overwrite a UTF-8 text file, confined to the
/// workspace root. `content` is written byte-for-byte (ADR-0008 item 2:
/// parameter fidelity — no newline appended, no line endings converted).
pub(super) struct WriteFile {
    pub(super) root: PathBuf,
}

#[async_trait]
impl AgentTool for WriteFile {
    /// Serial: two concurrent writes to one file would tear it. Declared,
    /// not defaulted (ADR-0008 item 2 amendment: the declaration records
    /// the analysis).
    fn concurrency(&self) -> Concurrency {
        Concurrency::Serial
    }

    /// Creates and overwrites: a mutation, gated by the client policy
    /// (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Mutation
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create a new file or overwrite an existing one inside the workspace, \
                          writing `content` byte-for-byte (no trailing newline is added and no \
                          line endings are converted); missing parent directories are created. \
                          Use this to create a file or rewrite a small file wholesale. For a \
                          partial change to an existing file, use edit_file instead — overwriting \
                          a large file wastes context and silently loses any part you did not \
                          reproduce. Never use this to read a file (use read_file). `path` is \
                          relative to the workspace root; absolute paths work only inside the \
                          workspace. An overwrite's result carries a capped unified diff against \
                          the previous content — check it for unintended loss instead of \
                          re-reading."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "path relative to the workspace root; absolute paths work only inside the workspace"},
                    "content": {"type": "string", "description": "the file's full new content, written byte-for-byte"},
                },
                "required": ["path", "content"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let path = arguments["path"].as_str().unwrap_or_default();
        if path.is_empty() {
            return Err(error(
                "write_file",
                "path must be a non-empty string".into(),
            ));
        }
        let Some(content) = arguments.get("content").and_then(Value::as_str) else {
            return Err(error(
                "write_file",
                format!(
                    "content must be a string, got {}",
                    arguments.get("content").unwrap_or(&Value::Null)
                ),
            ));
        };
        let canonical =
            resolve(&self.root, path).map_err(|message| error("write_file", message))?;
        let existed = canonical.exists();
        // Overwrite verification (ADR-0008 item 5): the diff against the
        // previous bytes is the only place silent loss (content the model
        // forgot to carry over) becomes visible. Read before writing; a
        // non-UTF-8 or unreadable previous file yields no diff.
        let previous = if existed {
            fs::read(&canonical)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        } else {
            None
        };
        if let Some(parent) = canonical.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                error(
                    "write_file",
                    format!("cannot create the parent directories of `{path}`: {err}"),
                )
            })?;
        }
        fs::write(&canonical, content).map_err(|err| {
            error(
                "write_file",
                // Non-atomic by decision (module docs): say so, so the model
                // never trusts a maybe-torn file.
                format!(
                    "cannot write `{path}`: {err} — the file may be partially written; \
                     re-read it with read_file before assuming its state"
                ),
            )
        })?;
        let verb = if existed { "overwrote" } else { "created" };
        let mut result = format!(
            "{verb} `{path}` ({} bytes, {} lines)",
            content.len(),
            content.lines().count()
        );
        if existed {
            match previous.as_deref() {
                Some(old) => match unified_diff(path, old, content) {
                    Some(diff) => {
                        result.push('\n');
                        result.push_str(&diff);
                    }
                    None => result.push_str(" (content unchanged)"),
                },
                None => result.push_str(" (no diff: previous content was not UTF-8)"),
            }
        }
        Ok(Value::String(result))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::tool;
    use crate::test_support::Scratch;

    #[tokio::test]
    async fn write_file_creates_a_new_file_with_parent_dirs() {
        let scratch = Scratch::new("write-create");
        let write_file = tool(&scratch.0, "write_file");

        let result = write_file
            .invoke(json!({"path": "src/new/main.rs", "content": "fn main() {}\n"}))
            .await
            .expect("write");

        assert_eq!(
            std::fs::read_to_string(scratch.0.join("src/new/main.rs")).expect("read back"),
            "fn main() {}\n"
        );
        assert_eq!(
            result,
            json!("created `src/new/main.rs` (13 bytes, 1 lines)")
        );
        // A create's content is entirely the model's own input — no diff.
    }

    #[tokio::test]
    async fn write_file_overwrite_result_carries_a_unified_diff() {
        let scratch = Scratch::new("write-diff");
        scratch.write("a.txt", "keep me\nlose me\n");
        let write_file = tool(&scratch.0, "write_file");

        let result = write_file
            .invoke(json!({"path": "a.txt", "content": "keep me\nnew line\n"}))
            .await
            .expect("write")
            .as_str()
            .expect("string")
            .to_string();

        assert!(result.contains("@@"), "got: {result}");
        // The lost line is the feedback's whole point.
        assert!(result.contains("-lose me"), "got: {result}");
        assert!(result.contains("+new line"), "got: {result}");
    }

    #[tokio::test]
    async fn write_file_overwrite_of_a_non_utf8_file_names_the_missing_diff() {
        let scratch = Scratch::new("write-binary");
        scratch.write_bytes("bin.dat", b"\xff\xfe\x00");
        let write_file = tool(&scratch.0, "write_file");

        let result = write_file
            .invoke(json!({"path": "bin.dat", "content": "text now\n"}))
            .await
            .expect("write");

        assert!(
            result.as_str().expect("string").contains("no diff"),
            "got: {result}"
        );
    }

    #[tokio::test]
    async fn write_file_overwrites_byte_for_byte() {
        let scratch = Scratch::new("write-overwrite");
        // CRLF and a missing trailing newline must survive: parameter
        // fidelity forbids silent content transforms (ADR-0008 item 2).
        scratch.write_bytes("a.txt", b"old\r\ncontent\r\n");
        let write_file = tool(&scratch.0, "write_file");

        let result = write_file
            .invoke(json!({"path": "a.txt", "content": "new\r\nbody"}))
            .await
            .expect("write");

        assert_eq!(
            std::fs::read(scratch.0.join("a.txt")).expect("read back"),
            b"new\r\nbody"
        );
        assert!(
            result
                .as_str()
                .expect("string")
                .starts_with("overwrote `a.txt`")
        );
        // Byte-exactness was asserted above; the diff channel is covered by
        // its own test (CRLF bytes make exact diff assertions brittle).
    }

    #[tokio::test]
    async fn write_file_requires_a_string_content() {
        let scratch = Scratch::new("write-content");
        let write_file = tool(&scratch.0, "write_file");

        let err = write_file
            .invoke(json!({"path": "a.txt"}))
            .await
            .expect_err("content is required");
        assert!(
            err.message.contains("content must be a string"),
            "got: {err}"
        );

        let err = write_file
            .invoke(json!({"content": "x"}))
            .await
            .expect_err("path is required");
        assert!(
            err.message.contains("path must be a non-empty string"),
            "got: {err}"
        );
    }
}
