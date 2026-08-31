//! Core library of `cadmus`.
//!
//! The binary (`src/main.rs`) is a thin CLI shell over this library: keep
//! logic here so it stays testable without spawning a process.

mod error;

pub use error::{Error, Result};

/// Builds a greeting for `name`.
///
/// # Errors
///
/// Returns [`Error::EmptyName`] when `name` is empty or all whitespace.
pub fn greet(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::EmptyName);
    }
    Ok(format!("Hello, {name}!"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets_by_name() {
        assert_eq!(greet("ferris").expect("valid name"), "Hello, ferris!");
    }

    #[test]
    fn rejects_empty_name() {
        assert!(matches!(greet("   "), Err(Error::EmptyName)));
    }
}
