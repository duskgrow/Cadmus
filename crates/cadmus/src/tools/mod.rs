//! Read-only coding tools, confined to a workspace root (phase 0 scope; the
//! Landlock sandbox is phase 3, report §7). Paths resolving outside the root
//! are tool errors — feedback the model can recover from, never a fatal error.
//!
//! One module per tool. The confinement seam (`resolve`) and the tool-error
//! constructor stay here: the security floor has exactly one home, and every
//! tool — present and future — goes through it.

mod grep;
mod list_dir;
mod read_file;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadmus_core::{AgentTool, ToolError};

use grep::Grep;
use list_dir::ListDir;
use read_file::ReadFile;

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
    // `has_root`, not `is_absolute`: on Windows `/foo` has a root but no
    // drive prefix, and `is_absolute` says false — which would silently
    // re-root it into the workspace. Root-relative input gets the same
    // confinement check on every platform.
    let candidate = if Path::new(path).has_root() {
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use cadmus_core::AgentTool;
    use serde_json::json;

    use super::coding_tools;

    /// A scratch workspace under the OS temp dir, unique per test name and
    /// process, removed on drop. Shared by every tool's test module.
    pub(super) struct Scratch(pub(super) PathBuf);

    impl Scratch {
        pub(super) fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("cadmus-tools-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create scratch");
            Self(root)
        }

        pub(super) fn write(&self, path: &str, contents: &str) {
            self.write_bytes(path, contents.as_bytes());
        }

        pub(super) fn write_bytes(&self, path: &str, contents: &[u8]) {
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

    pub(super) fn tool(root: &Path, name: &str) -> Arc<dyn AgentTool> {
        coding_tools(root.to_path_buf())
            .into_iter()
            .find(|tool| tool.spec().name == name)
            .expect("tool exists")
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
}
