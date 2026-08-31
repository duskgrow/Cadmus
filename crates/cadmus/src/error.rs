//! The library's public error boundary.
//!
//! Decision rule: callers that need to *react* to a failure mode get a
//! `thiserror` enum variant; callers that only give up and report get an
//! opaque `miette::Report` (see the top level in `main.rs`).

use miette::Diagnostic;
use thiserror::Error;

/// All failure modes of the `cadmus` library.
///
/// Errors are user-facing documentation: `code` is searchable and `help`
/// gives an actionable fix. Once published, an error code is part of the
/// public interface — removing or renaming one is a `SemVer`-breaking change.
#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    /// The name was empty or whitespace-only.
    #[error("name must not be empty")]
    #[diagnostic(
        code(cadmus::empty_name),
        help("pass a non-empty name, e.g. `--name ferris`")
    )]
    EmptyName,
}

/// Convenience alias for library results.
pub type Result<T> = std::result::Result<T, Error>;
