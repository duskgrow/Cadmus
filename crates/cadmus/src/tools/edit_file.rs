use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use cadmus_contract::ToolSpec;
use cadmus_core::{AgentTool, Concurrency, Effect, ToolError};
use serde_json::{Value, json};

use super::diff::unified_diff;
use super::{error, resolve};

/// `edit_file`: exact-match replacements on one UTF-8 text file (ADR-0008
/// item 3 + the 2026-09-09 amendment): an array of `old_string`→`new_string`
/// pairs per call, applied in array order, each matching exactly once at
/// its step, all-or-nothing — a file is never left half-edited.
pub(super) struct EditFile {
    pub(super) root: PathBuf,
}

#[async_trait]
impl AgentTool for EditFile {
    /// Serial: read-modify-write races on one file would corrupt it.
    /// Declared, not defaulted (ADR-0008 item 2 amendment).
    fn concurrency(&self) -> Concurrency {
        Concurrency::Serial
    }

    /// Rewrites the file in place: a mutation, gated by the client policy
    /// (ADR-0008 item 4).
    fn effect(&self) -> Effect {
        Effect::Mutation
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Apply exact string replacements to one UTF-8 text file in the workspace. \
                          Each entry in `edits` replaces `old_string` with `new_string` and succeeds \
                          only when `old_string` matches EXACTLY once at its step; entries apply in \
                          array order, and the call writes all of them or none — the file is never \
                          left half-edited. Use this for partial changes; to create a file or \
                          rewrite it wholesale, use write_file. Read the file with read_file first \
                          and copy `old_string` verbatim, indentation included; matching is exact \
                          against the raw bytes (never fuzzy). A repeat of a successful call fails \
                          safely because the old text is gone — edits are near-idempotent, so never \
                          retry blindly: on a failed edit, re-read the file and adjust. The result \
                          carries a capped unified diff of what actually changed — check placement \
                          there instead of re-reading the file."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "path relative to the workspace root; absolute paths work only inside the workspace"},
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "the replacements to apply in order, all or none",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {"type": "string", "description": "the exact text to find; must match exactly once at its step"},
                                "new_string": {"type": "string", "description": "the replacement text"},
                            },
                            "required": ["old_string", "new_string"],
                        },
                    },
                },
                "required": ["path", "edits"],
            }),
        }
    }

    async fn invoke(&self, arguments: Value) -> Result<Value, ToolError> {
        let path = arguments["path"].as_str().unwrap_or_default();
        if path.is_empty() {
            return Err(error("edit_file", "path must be a non-empty string".into()));
        }
        let edits = arguments["edits"]
            .as_array()
            .filter(|edits| !edits.is_empty());
        let Some(edits) = edits else {
            return Err(error(
                "edit_file",
                "edits must be a non-empty array of {old_string, new_string}".into(),
            ));
        };
        let canonical = resolve(&self.root, path).map_err(|message| error("edit_file", message))?;
        let bytes = fs::read(&canonical).map_err(|err| {
            let extra = if err.kind() == std::io::ErrorKind::NotFound {
                "; to create it, use write_file"
            } else {
                ""
            };
            error("edit_file", format!("cannot read `{path}`: {err}{extra}"))
        })?;
        let original = String::from_utf8(bytes).map_err(|_| {
            error(
                "edit_file",
                format!("`{path}` is not UTF-8 text; edit_file edits text files only"),
            )
        })?;
        let content = apply_edits(&original, edits, path)?;

        fs::write(&canonical, content.as_bytes()).map_err(|err| {
            error(
                "edit_file",
                // Non-atomic by decision (module docs): say so, so the model
                // never trusts a maybe-torn file.
                format!(
                    "cannot write `{path}`: {err} — the file may be partially written; \
                     re-read it with read_file before assuming its state"
                ),
            )
        })?;
        let mut result = format!(
            "applied {} edit(s) to `{path}` ({} bytes, {} lines)",
            edits.len(),
            content.len(),
            content.lines().count()
        );
        // The verification feedback (ADR-0008 item 5): what the file
        // transition actually was — placement, surviving context. A net-zero
        // batch (A→B then B→A) reports itself, never an empty diff.
        match unified_diff(path, &original, &content) {
            Some(diff) => {
                result.push('\n');
                result.push_str(&diff);
            }
            None => result.push_str(" (no net change)"),
        }
        Ok(Value::String(result))
    }
}

/// Applies the batch to `original` in array order, each edit matching
/// exactly once at its step. Pure in-memory: the caller writes only on
/// success — a failed batch never touches the file (all-or-nothing).
fn apply_edits(original: &str, edits: &[Value], path: &str) -> Result<String, ToolError> {
    let mut content = original.to_string();
    for (index, edit) in edits.iter().enumerate() {
        let step = index + 1;
        let old = edit["old_string"].as_str().ok_or_else(|| {
            error(
                "edit_file",
                format!("edit {step}: old_string must be a string"),
            )
        })?;
        let new = edit["new_string"].as_str().ok_or_else(|| {
            error(
                "edit_file",
                format!("edit {step}: new_string must be a string"),
            )
        })?;
        if old.is_empty() {
            return Err(error(
                "edit_file",
                format!("edit {step}: old_string must not be empty. Nothing was written."),
            ));
        }
        // A no-op edit must fail, not report success: otherwise a
        // blind-retry loop gets perpetual success feedback while the
        // file never changes (and the near-idempotency signal dies).
        if old == new {
            return Err(error(
                "edit_file",
                format!(
                    "edit {step}: old_string and new_string are identical — no change. \
                     Nothing was written."
                ),
            ));
        }
        match content.matches(old).count() {
            0 => {
                // read_file displays CRLF normalized, so a verbatim copy
                // of what the model saw can miss the raw bytes — name
                // that trap instead of leaving a mysterious mismatch
                // (ADR-0008's parameter-fidelity lesson); matching stays
                // exact, never silently normalized.
                let hint = if content.contains("\r\n") {
                    " The file uses CRLF line endings; read_file displays them normalized, so \
                     match the raw bytes or rewrite the file with write_file."
                } else {
                    ""
                };
                return Err(error(
                    "edit_file",
                    format!(
                        "edit {step}: old_string not found in `{path}` (0 exact matches).{hint} \
                         Re-read the file with read_file and retry with verbatim text. \
                         Nothing was written."
                    ),
                ));
            }
            1 => content = content.replacen(old, new, 1),
            count => {
                return Err(error(
                    "edit_file",
                    format!(
                        "edit {step}: old_string matches {count} places in `{path}` — \
                         ambiguous. Include more surrounding context so it matches exactly \
                         once. Nothing was written."
                    ),
                ));
            }
        }
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tests::{Scratch, tool};

    #[tokio::test]
    async fn edit_file_applies_an_ordered_batch_all_or_nothing() {
        let scratch = Scratch::new("edit-batch");
        scratch.write("a.txt", "alpha\nbeta\ngamma\n");
        let edit_file = tool(&scratch.0, "edit_file");

        // The second edit matches text the first edit introduced — entries
        // apply in array order to the running content.
        let result = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [
                    {"old_string": "beta", "new_string": "BETA\ndelta"},
                    {"old_string": "delta", "new_string": "DELTA"},
                ],
            }))
            .await
            .expect("edit");

        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).expect("read back"),
            "alpha\nBETA\nDELTA\ngamma\n"
        );
        assert!(
            result
                .as_str()
                .expect("string")
                .starts_with("applied 2 edit(s) to `a.txt`"),
            "got: {result}"
        );
        // The result carries the unified diff of the file transition:
        // placement via the hunk header, the lost line, the landed lines.
        let text = result.as_str().expect("string");
        assert!(text.contains("@@"), "got: {text}");
        assert!(text.contains("-beta"), "got: {text}");
        assert!(text.contains("+BETA"), "got: {text}");
        assert!(text.contains("+DELTA"), "got: {text}");
    }

    #[tokio::test]
    async fn edit_file_reports_a_net_zero_batch_without_a_diff() {
        let scratch = Scratch::new("edit-netzero");
        scratch.write("a.txt", "alpha\n");
        let edit_file = tool(&scratch.0, "edit_file");

        // A→B then B→A: each edit is legitimate (old != new), the file
        // transition is nil.
        let result = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [
                    {"old_string": "alpha", "new_string": "beta"},
                    {"old_string": "beta", "new_string": "alpha"},
                ],
            }))
            .await
            .expect("edit");

        let text = result.as_str().expect("string");
        assert!(text.contains("no net change"), "got: {text}");
        assert!(!text.contains("@@"), "got: {text}");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).expect("read back"),
            "alpha\n"
        );
    }

    #[tokio::test]
    async fn edit_file_writes_nothing_when_any_edit_fails() {
        let scratch = Scratch::new("edit-atomic");
        scratch.write("a.txt", "alpha\nbeta\n");
        let edit_file = tool(&scratch.0, "edit_file");

        let err = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [
                    {"old_string": "alpha", "new_string": "ALPHA"},
                    {"old_string": "missing", "new_string": "x"},
                ],
            }))
            .await
            .expect_err("edit 2 cannot match");

        assert!(err.message.contains("edit 2"), "got: {err}");
        assert!(err.message.contains("Nothing was written"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).expect("read back"),
            "alpha\nbeta\n",
            "a failed batch leaves the file untouched"
        );
    }

    #[tokio::test]
    async fn edit_file_requires_exactly_one_match() {
        let scratch = Scratch::new("edit-unique");
        scratch.write("a.txt", "x = 1;\ny = 1;\n");
        let edit_file = tool(&scratch.0, "edit_file");

        let err = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [{"old_string": "1", "new_string": "2"}],
            }))
            .await
            .expect_err("ambiguous");
        assert!(err.message.contains("matches 2 places"), "got: {err}");

        let err = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [{"old_string": "", "new_string": "2"}],
            }))
            .await
            .expect_err("empty old_string");
        assert!(err.message.contains("must not be empty"), "got: {err}");
    }

    #[tokio::test]
    async fn edit_file_reapplication_fails_safely() {
        let scratch = Scratch::new("edit-idempotent");
        scratch.write("a.txt", "alpha\n");
        let edit_file = tool(&scratch.0, "edit_file");
        let call = json!({
            "path": "a.txt",
            "edits": [{"old_string": "alpha", "new_string": "beta"}],
        });

        edit_file.invoke(call.clone()).await.expect("first edit");
        let err = edit_file
            .invoke(call)
            .await
            .expect_err("the old text is gone after a successful edit");
        assert!(err.message.contains("0 exact matches"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).expect("read back"),
            "beta\n"
        );
    }

    #[tokio::test]
    async fn edit_file_names_the_crlf_trap_on_a_miss() {
        let scratch = Scratch::new("edit-crlf");
        // read_file shows this file's line endings normalized, so a verbatim
        // copy of the displayed text cannot match the raw bytes.
        scratch.write_bytes("a.txt", b"alpha\r\nbeta\r\n");
        let edit_file = tool(&scratch.0, "edit_file");

        let err = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [{"old_string": "alpha\nbeta", "new_string": "x"}],
            }))
            .await
            .expect_err("LF cannot match CRLF bytes");
        assert!(err.message.contains("CRLF"), "got: {err}");
        assert_eq!(
            std::fs::read(scratch.0.join("a.txt")).expect("read back"),
            b"alpha\r\nbeta\r\n"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_an_empty_or_noop_batch() {
        let scratch = Scratch::new("edit-empty");
        scratch.write("a.txt", "alpha\n");
        let edit_file = tool(&scratch.0, "edit_file");

        let err = edit_file
            .invoke(json!({"path": "a.txt", "edits": []}))
            .await
            .expect_err("empty batch");
        assert!(err.message.contains("non-empty array"), "got: {err}");

        let err = edit_file
            .invoke(json!({
                "path": "a.txt",
                "edits": [{"old_string": "alpha", "new_string": "alpha"}],
            }))
            .await
            .expect_err("no-op edit");
        assert!(err.message.contains("identical"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("a.txt")).expect("read back"),
            "alpha\n"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_non_utf8_and_missing_files() {
        let scratch = Scratch::new("edit-rejects");
        scratch.write_bytes("bin.dat", b"\xff\xfe\x00");
        let edit_file = tool(&scratch.0, "edit_file");

        let err = edit_file
            .invoke(json!({
                "path": "bin.dat",
                "edits": [{"old_string": "x", "new_string": "y"}],
            }))
            .await
            .expect_err("binary");
        assert!(err.message.contains("not UTF-8 text"), "got: {err}");

        let err = edit_file
            .invoke(json!({
                "path": "missing.txt",
                "edits": [{"old_string": "x", "new_string": "y"}],
            }))
            .await
            .expect_err("missing file");
        assert!(err.message.contains("write_file"), "got: {err}");
    }
}
