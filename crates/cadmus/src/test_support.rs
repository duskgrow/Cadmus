//! Shared support for the library's unit tests. Integration tests under
//! `tests/` compile against the public API only, so they keep their own
//! local copies of anything similar.

use std::path::PathBuf;

/// A scratch tree under the OS temp dir, unique per test name and process,
/// removed on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("cadmus-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create scratch");
        Self(root)
    }

    /// Writes `contents` at the scratch-relative `path`, creating parents.
    pub fn write(&self, path: &str, contents: &str) {
        self.write_bytes(path, contents.as_bytes());
    }

    pub fn write_bytes(&self, path: &str, contents: &[u8]) {
        let full = self.0.join(path);
        std::fs::create_dir_all(full.parent().expect("parent")).expect("mkdirs");
        std::fs::write(full, contents).expect("write");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
