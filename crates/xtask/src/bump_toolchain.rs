//! `bump-toolchain`: point rust-toolchain.toml's `channel` at the latest
//! stable Rust release. That file is the toolchain-version SSOT (flake.nix,
//! rustup and CI all consume it), so a bump is exactly this one rewrite; the
//! weekly toolchain-update workflow wraps it in a validated PR.
//!
//! Std-only like the rest of xtask; the HTTPS fetch shells out to `curl`
//! (std has no TLS stack, and curl is present on every GitHub-hosted runner).
//! When `$GITHUB_OUTPUT` is set, `changed=<true|false>` and (on change)
//! `version=<X.Y.Z>` are appended for the calling workflow step.

use std::path::PathBuf;
use std::process::ExitCode;

const CHANNEL_URL: &str = "https://static.rust-lang.org/dist/channel-rust-stable.toml";

/// Entry point: `bump-toolchain` (no arguments).
pub fn run(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!("usage: bump-toolchain  (no arguments)");
        return ExitCode::from(2);
    }
    match bump() {
        Ok(Outcome::UpToDate(current)) => {
            println!("rust toolchain is up to date ({current})");
            set_output("changed", "false");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Bumped { from, to }) => {
            println!("bumped rust toolchain: {from} -> {to}");
            set_output("changed", "true");
            set_output("version", &to);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

enum Outcome {
    UpToDate(String),
    Bumped { from: String, to: String },
}

fn bump() -> Result<Outcome, String> {
    let path = toolchain_path();
    let content = std::fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let current = parse_channel(&content)
        .ok_or_else(|| format!("no `channel = \"...\"` line in {}", path.display()))?;
    if parse_version(&current).is_none() {
        return Err(format!(
            "channel {current:?} is not a pinned X.Y.Z version; refusing to guess"
        ));
    }
    let latest = latest_stable()?;
    if parse_version(&latest) <= parse_version(&current) {
        return Ok(Outcome::UpToDate(current));
    }
    let updated = bumped_content(&content, &latest)
        .ok_or_else(|| format!("failed to rewrite the channel line in {}", path.display()))?;
    std::fs::write(&path, updated)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(Outcome::Bumped {
        from: current,
        to: latest,
    })
}

/// rust-toolchain.toml at the workspace root (crates/xtask -> ../..).
fn toolchain_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("rust-toolchain.toml")
}

/// Query the latest stable release from the official channel manifest.
fn latest_stable() -> Result<String, String> {
    let output = std::process::Command::new("curl")
        .args(["-sSfL", "--max-time", "60", CHANNEL_URL])
        .output()
        .map_err(|err| format!("cannot run curl (present on every GitHub-hosted runner): {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl {CHANNEL_URL} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let content = String::from_utf8(output.stdout)
        .map_err(|err| format!("channel manifest is not UTF-8: {err}"))?;
    parse_latest_stable(&content)
        .ok_or_else(|| "no [pkg.rust] version in the channel manifest".to_string())
}

/// The `channel` value of a rust-toolchain.toml (`channel = "X.Y.Z"`).
fn parse_channel(content: &str) -> Option<String> {
    content
        .lines()
        .find(|line| is_channel_line(line))
        .and_then(quoted_value)
}

/// The latest stable version in a channel-rust-stable.toml: `[pkg.rust]`'s
/// `version = "X.Y.Z (hash date)"`, reduced to the bare `X.Y.Z`.
fn parse_latest_stable(content: &str) -> Option<String> {
    let mut in_rust_pkg = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_rust_pkg = line == "[pkg.rust]";
            continue;
        }
        if in_rust_pkg && line.starts_with("version") {
            let value = quoted_value(line)?;
            return value.split_whitespace().next().map(str::to_owned);
        }
    }
    None
}

/// A `channel = "..."` line, with any leading indentation.
fn is_channel_line(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("channel")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// The first `"..."`-quoted string on a line.
fn quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}

/// `X.Y.Z` as a comparable tuple; `None` for anything else (`nightly`,
/// `1.98`, `1.98.0.1`, ...).
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// `content` with only the channel line rewritten, preserving comments,
/// indentation and the trailing-newline convention.
fn bumped_content(content: &str, new_version: &str) -> Option<String> {
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in content.lines() {
        if replaced || !is_channel_line(line) {
            lines.push(line.to_owned());
            continue;
        }
        let indent = &line[..line.len() - line.trim_start().len()];
        lines.push(format!("{indent}channel = \"{new_version}\""));
        replaced = true;
    }
    if !replaced {
        return None;
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// Append `key=value` for the calling GitHub Actions step; a no-op locally.
fn set_output(key: &str, value: &str) {
    use std::io::Write as _;
    let Ok(path) = std::env::var("GITHUB_OUTPUT") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{key}={value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLCHAIN: &str =
        "# a comment\n[toolchain]\nchannel = \"1.94.0\"\ncomponents = [\"rustfmt\", \"clippy\"]\n";
    const MANIFEST: &str = "manifest-version = \"2\"\ndate = \"2026-08-18\"\n\n[pkg.rust]\nversion = \"1.98.0 (88d9e12ae 2026-08-18)\"\ngit_commit_hash = \"88d9e12ae\"\n\n[pkg.cargo]\nversion = \"0.99.0 (abc 2026-08-18)\"\n";

    #[test]
    fn parses_channel_and_manifest_version() {
        assert_eq!(parse_channel(TOOLCHAIN).as_deref(), Some("1.94.0"));
        assert_eq!(parse_latest_stable(MANIFEST).as_deref(), Some("1.98.0"));
    }

    #[test]
    fn compares_only_three_part_versions() {
        assert!(parse_version("1.98.0") < parse_version("1.98.1"));
        assert!(parse_version("1.98.0") > parse_version("1.97.1"));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.98"), None);
    }

    #[test]
    fn rewrites_only_the_channel_line() {
        let updated = bumped_content(TOOLCHAIN, "1.98.0").unwrap();
        assert!(updated.contains("channel = \"1.98.0\""));
        assert!(updated.contains("# a comment"));
        assert!(updated.ends_with('\n'));
        assert!(!updated.contains("1.94.0"));
    }
}
