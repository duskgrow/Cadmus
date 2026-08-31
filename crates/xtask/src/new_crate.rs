//! `new-crate <name>` — add an internal workspace crate (unpublished by
//! default; rules in CONTRIBUTING.md "Adding a crate").

use std::path::Path;
use std::process::ExitCode;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

const CARGO_TOML_TEMPLATE: &str = r#"[package]
name = "NAME_PLACEHOLDER"
description = "TODO: one-line description of this crate"
# Internal crates carry no semver burden. To publish later, switch to
# version.workspace = true, drop publish = false, and register the package
# in release-plz.toml — see CONTRIBUTING.md.
version = "0.0.0"
publish = false
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
"#;

const LIB_RS_TEMPLATE: &str =
    "//! TODO: crate documentation (the contract for consumers, not an implementation recap)\n";

/// Entry point: `new-crate <name>` (kebab-case).
pub fn run(args: &[String]) -> ExitCode {
    let [name] = args else {
        eprintln!("usage: new-crate <name>  (kebab-case: lowercase letters, digits, hyphens)");
        return ExitCode::from(2);
    };
    if !is_kebab_case(name) {
        eprintln!("error: crate name must be kebab-case (lowercase letters, digits, hyphens)");
        return ExitCode::FAILURE;
    }
    let crate_dir = Path::new(ROOT).join("crates").join(name);
    if crate_dir.exists() {
        eprintln!("error: crates/{name} already exists");
        return ExitCode::FAILURE;
    }
    if let Err(err) = scaffold(&crate_dir, name) {
        eprintln!("error: {err}");
        return ExitCode::FAILURE;
    }
    println!("created crates/{name}");
    println!(
        "to depend on it from another crate: register {{ path = \"crates/{name}\" }} in the root [workspace.dependencies] and inherit with workspace = true."
    );
    ExitCode::SUCCESS
}

fn scaffold(crate_dir: &Path, name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(crate_dir.join("src"))?;
    let manifest = CARGO_TOML_TEMPLATE.replace("NAME_PLACEHOLDER", name);
    std::fs::write(crate_dir.join("Cargo.toml"), manifest)?;
    std::fs::write(crate_dir.join("src/lib.rs"), LIB_RS_TEMPLATE)?;
    Ok(())
}

/// `^[a-z][a-z0-9]*(-[a-z0-9]+)*$` — hand-rolled so `xtask` stays std-only.
#[must_use]
pub fn is_kebab_case(name: &str) -> bool {
    let mut segments = name.split('-');
    let Some(first) = segments.next() else {
        return false;
    };
    let valid_first = first.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    valid_first
        && segments.all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case_validation() {
        assert!(is_kebab_case("a"));
        assert!(is_kebab_case("my-cli"));
        assert!(is_kebab_case("x1-y2"));
        assert!(!is_kebab_case("1abc"));
        assert!(!is_kebab_case("My-Cli"));
        assert!(!is_kebab_case("my_cli"));
        assert!(!is_kebab_case("my--cli"));
        assert!(!is_kebab_case("-cli"));
        assert!(!is_kebab_case("cli-"));
        assert!(!is_kebab_case(""));
    }
}
