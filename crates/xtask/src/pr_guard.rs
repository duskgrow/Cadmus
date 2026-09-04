//! Human gates for pull requests. Agents self-check via skills; humans get
//! red buttons. What cannot be machine-checked compiles into a mechanical
//! trigger plus a label the human applies as their conscious, timeline-logged
//! acknowledgment (applying the label re-runs this gate):
//!
//!   - `.github/` touched              ⇒ the `github-ok` label is required
//!   - dependency manifests touched    ⇒ `deps-ok` (`Cargo.toml` at any
//!     depth, `Cargo.lock`, `deny.toml`)
//!   - `*.snap` snapshots touched      ⇒ `snapshots-ok`
//!
//! On top of that sits a machine check with no label escape hatch: a secret
//! scan of the added diff lines (hard fail — a leaked secret needs rotation,
//! not a label). The one line-level escape hatch is the `pr-guard:allow`
//! marker, for documented *example* secrets (AWS's canonical docs key trips
//! the patterns by design); the marker stays visible in review, so it cannot
//! be smuggled.
//!
//! Two modes:
//!   `pr-guard [PR]`   full gate; PR labels come from the gh CLI (a PR
//!                     number/URL argument, else gh resolves the current
//!                     branch's PR); the changed-file list and the scanned
//!                     content both come from `gh pr diff`
//!   `pr-guard --staged`  secret-scan only, over `git diff --cached` — the
//!                     local pre-commit half of the gate (no gh needed)
//!
//! Std-only like the rest of xtask: the secret patterns below are hand-rolled
//! matchers, and gh's `--jq` output (one value per line) needs no JSON parser.

use std::collections::BTreeMap;
use std::process::{Command, ExitCode};

/// Entry point: `pr-guard [PR] | pr-guard --staged`.
pub fn run(args: &[String]) -> ExitCode {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--staged"] => staged(),
        [] => full(None),
        [pr] => full(Some(pr)),
        _ => {
            eprintln!("usage: pr-guard [PR]  ('--staged' scans the staged diff only)");
            ExitCode::from(2)
        }
    }
}

/// The local-hook half: secret-scan `git diff --cached`.
fn staged() -> ExitCode {
    let diff = match Command::new("git").args(["diff", "--cached"]).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            eprintln!(
                "error: git diff --cached failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return ExitCode::from(2);
        }
        Err(err) => {
            eprintln!("error: cannot run git: {err}");
            return ExitCode::from(2);
        }
    };
    let findings = scan_diff(&diff);
    if findings.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("pr-guard: possible secrets in the staged diff:");
    for finding in &findings {
        eprintln!("  - {finding}");
    }
    eprintln!("a leaked secret needs rotation, not just deletion");
    eprintln!("a documented *example* secret can carry the `{ALLOW_MARKER}` marker on its line");
    ExitCode::FAILURE
}

/// The CI half: labels via `gh pr view`, file list and content via `gh pr diff`.
fn full(pr: Option<&str>) -> ExitCode {
    let mut view: Vec<&str> = vec!["pr", "view"];
    view.extend(pr);
    let labels = match gh(&[&view[..], &["--json", "labels", "--jq", ".labels[].name"]].concat()) {
        Ok(out) => out,
        Err(code) => return code,
    };
    let mut diff_args: Vec<&str> = vec!["pr", "diff"];
    diff_args.extend(pr);
    let diff = match gh(&diff_args) {
        Ok(out) => out,
        Err(code) => return code,
    };
    let labels: Vec<String> = labels.lines().map(str::to_string).collect();
    let files = changed_files(&diff);

    let findings = scan_diff(&diff);
    let required = required_labels(&files);
    let missing: Vec<(&str, &Vec<String>)> = required
        .iter()
        .filter(|(label, _)| !labels.iter().any(|present| present == *label))
        .map(|(label, triggers)| (*label, triggers))
        .collect();

    if findings.is_empty() && missing.is_empty() {
        println!("pr-guard: clean (no secrets in the diff; no unacknowledged gated files)");
        return ExitCode::SUCCESS;
    }
    eprintln!("pr-guard: blocked");
    if !findings.is_empty() {
        eprintln!(
            "  secret scan (hard fail — rotate if real; documented examples carry `{ALLOW_MARKER}`):"
        );
        for finding in &findings {
            eprintln!("    - {finding}");
        }
    }
    for (label, triggers) in &missing {
        eprintln!("  missing label `{label}` — the human's logged acknowledgment for:");
        for trigger in triggers.iter().take(5) {
            eprintln!("    - {trigger}");
        }
        if triggers.len() > 5 {
            eprintln!("    - … and {} more", triggers.len() - 5);
        }
        eprintln!("    apply it once reviewed: gh pr edit --add-label {label}");
    }
    ExitCode::FAILURE
}

fn gh(args: &[&str]) -> Result<String, ExitCode> {
    match Command::new("gh").args(args).output() {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(output) => {
            eprintln!(
                "error: gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            Err(ExitCode::from(2))
        }
        Err(err) => {
            eprintln!("error: cannot run gh (needed for PR metadata): {err}");
            Err(ExitCode::from(2))
        }
    }
}

/// One possible secret in an added diff line. Display redacts the match —
/// CI logs must not echo the secret itself.
pub struct Finding {
    path: String,
    line: usize,
    pattern: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let whereabouts = if self.path.is_empty() {
            format!("diff line {}", self.line)
        } else {
            format!("{} (diff line {})", self.path, self.line)
        };
        write!(
            f,
            "{whereabouts}: matches secret pattern {:?}",
            self.pattern
        )
    }
}

/// Line-level escape hatch for the secret scan: an added line carrying this
/// marker is skipped. For documented *example* secrets only — the marker is
/// part of the reviewed diff, so misuse is visible.
pub const ALLOW_MARKER: &str = "pr-guard:allow";

/// Scan a unified diff for secrets in *added* lines only (a removal is the
/// fix, not the leak). Tracks the `+++ b/<path>` header for reporting.
#[must_use]
pub fn scan_diff(diff: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut path = String::new();
    for (index, line) in diff.lines().enumerate() {
        // headers are `+++ <path>` with a space — without it, added content
        // like `++i;` would be swallowed as a header and never scanned
        if let Some(rest) = line.strip_prefix("+++ ") {
            path = rest.strip_prefix("b/").unwrap_or("").to_string();
            continue;
        }
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if added.contains(ALLOW_MARKER) {
            continue;
        }
        if let Some(pattern) = SECRET_PATTERNS.iter().find(|p| (p.matches)(added)) {
            findings.push(Finding {
                path: path.clone(),
                line: index + 1,
                pattern: pattern.name,
            });
        }
    }
    findings
}

/// Changed file paths, derived from the diff itself — this avoids both the
/// 100-file cap of `gh pr view --json files` and a second API call. Union of
/// the `--- a/` and `+++ b/` headers: a deleted file shows up only on the
/// `---` side, and deleting `deny.toml` must still trip the gate. Two known
/// blind spots, both accepted: quoted paths (`+++ "b/weird name"`) don't
/// match the prefixes, and pure renames carry no `---`/`+++` headers at all —
/// the gated paths (.github/, manifests, snapshots) don't look like that in
/// practice, and a human still reviews the diff.
#[must_use]
pub fn changed_files(diff: &str) -> Vec<String> {
    let mut files = std::collections::BTreeSet::new();
    for line in diff.lines() {
        for prefix in ["--- a/", "+++ b/"] {
            if let Some(path) = line.strip_prefix(prefix) {
                files.insert(path.to_string());
            }
        }
    }
    files.into_iter().collect()
}

struct SecretPattern {
    name: &'static str,
    matches: fn(&str) -> bool,
}

// The pattern literals are deliberately assembled from fragments so this
// module's own source never trips the scan when it lands in a diff.
const KEY_BEGIN: &str = "-----BEGIN ";
const KEY_END: &str = " PRIVATE KEY-----";
const GITHUB_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];
const AWS_PREFIXES: [&str; 2] = ["AKIA", "ASIA"];
const SECRET_WORDS: [&str; 6] = ["SECRET", "TOKEN", "PASSWORD", "PASSWD", "API_KEY", "APIKEY"];

const SECRET_PATTERNS: [SecretPattern; 5] = [
    SecretPattern {
        name: "private key block",
        matches: |line| line.contains(KEY_BEGIN) && line.contains(KEY_END),
    },
    SecretPattern {
        name: "GitHub token",
        matches: |line| contains_prefixed_token(line, &GITHUB_PREFIXES, 20),
    },
    SecretPattern {
        name: "AWS access key ID",
        matches: |line| {
            AWS_PREFIXES.iter().any(|prefix| {
                line.match_indices(prefix).any(|(start, _)| {
                    line[start + prefix.len()..]
                        .bytes()
                        .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                        .count()
                        >= 16
                })
            })
        },
    },
    SecretPattern {
        name: "Slack token",
        matches: |line| {
            line.match_indices("xox").any(|(start, _)| {
                let rest = &line[start + 3..];
                let bytes = rest.as_bytes();
                bytes.len() >= 12
                    && matches!(bytes[0], b'b' | b'a' | b'p' | b'r' | b's')
                    && bytes[1] == b'-'
                    && rest[2..]
                        .bytes()
                        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
                        .count()
                        >= 10
            })
        },
    },
    SecretPattern {
        name: ".env-style secret assignment",
        matches: is_secret_assignment,
    },
];

/// `prefix` followed by a run of at least `min_tail` token characters.
fn contains_prefixed_token(line: &str, prefixes: &[&str], min_tail: usize) -> bool {
    prefixes.iter().any(|prefix| {
        line.match_indices(prefix).any(|(start, _)| {
            line[start + prefix.len()..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count()
                >= min_tail
        })
    })
}

/// A `.env`-style assignment: `KEY=value` with the key at line start (optional
/// `export ` prefix), no spaces around `=`, the key carrying a secret-ish
/// word, and a value that looks real (≥ 8 chars, not a documented placeholder
/// shape). Requiring a bare-identifier key is what keeps code and prose —
/// `let token = …`, `TOKEN: value` — out of the match.
fn is_secret_assignment(line: &str) -> bool {
    let line = line.strip_prefix("export ").unwrap_or(line);
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    if key.is_empty()
        || key.bytes().next().is_some_and(|b| b.is_ascii_digit())
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return false;
    }
    let upper = key.to_ascii_uppercase();
    if !SECRET_WORDS.iter().any(|word| upper.contains(word)) {
        return false;
    }
    let value = value.trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    if unquoted.len() < 8 {
        return false;
    }
    let lower = unquoted.to_ascii_lowercase();
    !(lower.starts_with('<')
        || lower.starts_with('$')
        || lower.starts_with('%')
        || lower.starts_with("your")
        || lower.starts_with("xxx")
        || lower.contains("example")
        || lower.contains("changeme")
        || lower.contains("change-me")
        || lower.contains("placeholder")
        || lower.contains("redacted"))
}

/// Changed-file → required-label mapping (the module doc is the SSOT for the
/// why). Returns each label with the files that triggered it, sorted for
/// stable output.
#[must_use]
pub fn required_labels(files: &[String]) -> BTreeMap<&'static str, Vec<String>> {
    let mut required: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for file in files {
        let basename = file.rsplit('/').next().unwrap_or(file);
        let label = if file.starts_with(".github/") {
            Some("github-ok")
        } else if basename == "Cargo.toml" || file == "Cargo.lock" || file == "deny.toml" {
            Some("deps-ok")
        } else if std::path::Path::new(file)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("snap"))
        {
            Some("snapshots-ok")
        } else {
            None
        };
        if let Some(label) = label {
            required.entry(label).or_default().push(file.clone());
        }
    }
    required
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.pattern).collect()
    }

    #[test]
    fn scans_added_lines_only() {
        let token = format!("ghp_{}", "a".repeat(30));
        let removed = format!("--- a/.env\n+++ b/.env\n-{token}\n");
        assert!(scan_diff(&removed).is_empty());
        let added = format!("--- a/.env\n+++ b/.env\n+{token}\n");
        let findings = scan_diff(&added);
        assert_eq!(paths(&findings), vec!["GitHub token"]);
        assert_eq!(findings[0].path, ".env");
    }

    #[test]
    fn finds_each_pattern_family() {
        let key = format!("+{KEY_BEGIN}RSA{KEY_END}");
        let github = format!("+gho_{}", "b".repeat(36));
        let pat = format!("+github_pat_{}", "c".repeat(30));
        let aws = format!("+AKIA{}", "D".repeat(16));
        let slack = format!("+xoxb-{}", "1234567890-abc");
        let env = format!("+OPENAI_API_KEY=sk-{}", "e".repeat(20));
        let diff = format!("{key}\n{github}\n{pat}\n{aws}\n{slack}\n{env}\n");
        assert_eq!(
            paths(&scan_diff(&diff)),
            vec![
                "private key block",
                "GitHub token",
                "GitHub token",
                "AWS access key ID",
                "Slack token",
                ".env-style secret assignment",
            ]
        );
    }

    #[test]
    fn ignores_lookalikes() {
        // docs and code mention token shapes without being secrets
        let diff = "\
+tokens look like ghp_ followed by base62
+footer (`TOKEN: value` / `TOKEN #value`)
+let token = parse(header);
+GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
+API_KEY=<your-key-here>
+PASSWORD=changeme-me
+DB_PASSWORD=short
+a key header starts with -----BEGIN and a type word
+a lone suffix PRIVATE KEY----- is not a header
";
        assert!(scan_diff(diff).is_empty());
    }

    #[test]
    fn env_assignment_edge_cases() {
        let real = format!("+SESSION_TOKEN={}", "f".repeat(24));
        assert_eq!(
            paths(&scan_diff(&real)),
            vec![".env-style secret assignment"]
        );
        let exported = format!("+export AWS_SECRET_ACCESS_KEY={}", "g".repeat(24));
        assert_eq!(
            paths(&scan_diff(&exported)),
            vec![".env-style secret assignment"]
        );
        // spaced assignments are code or prose, not .env lines
        assert!(scan_diff("+TOKEN = abcdefghijkl\n").is_empty());
        // quoted real values still count
        let quoted = format!("+API_KEY=\"{}\"", "h".repeat(16));
        assert_eq!(
            paths(&scan_diff(&quoted)),
            vec![".env-style secret assignment"]
        );
        // …but a placeholder hiding inside quotes does not (pins the unwrapping)
        assert!(scan_diff("+API_KEY=\"<123456>\"\n").is_empty());
        // every placeholder guard has a dedicated case (mutation pins)
        for line in [
            "+SECRET=${VAULT_REF}",
            "+SECRET=%VAULT_REF%",
            "+API_TOKEN=your-token-here",
            "+PASSWORD=xxxxxxxxxxxx",
            "+TOKEN=example-value-123",
            "+PASSWORD=changeme123",
            "+PASSWORD=change-me-123",
            "+TOKEN=placeholder-value",
            "+TOKEN=redacted-value-1",
        ] {
            assert!(scan_diff(line).is_empty(), "should not flag: {line}");
        }
    }

    #[test]
    fn allow_marker_skips_documented_examples() {
        // the marker is the one escape hatch: visible in review, line-scoped
        let marked = format!("+AWS_ACCESS_KEY_ID=AKIA{} # {ALLOW_MARKER}", "I".repeat(16));
        assert!(scan_diff(&marked).is_empty());
        // without the marker even AWS's canonical docs example is flagged —
        // placeholder heuristics deliberately do NOT apply to named tokens
        let canonical = format!("+AKIA{}", "IOSFODNN7EXAMPLE");
        assert_eq!(paths(&scan_diff(&canonical)), vec!["AWS access key ID"]);
        // digit and underscore arms of the token-character classes
        let mixed = format!("+ghp_{}", "A1_b2".repeat(6));
        assert_eq!(paths(&scan_diff(&mixed)), vec!["GitHub token"]);
    }

    #[test]
    fn derives_changed_files_from_the_diff() {
        let diff = "\
--- a/.github/workflows/ci.yml
+++ b/.github/workflows/ci.yml
@@ -1 +1 @@
--- a/deny.toml
+++ /dev/null
@@ -1 +0,0 @@
--- a/README.md
+++ b/README.md
";
        // deleted files still count (deny.toml), /dev/null never does,
        // and each path appears once
        assert_eq!(
            changed_files(diff),
            vec![".github/workflows/ci.yml", "README.md", "deny.toml"]
        );
    }

    #[test]
    fn usage_errors_exit_with_code_2() {
        assert_eq!(run(&["a".to_string(), "b".to_string()]), ExitCode::from(2));
        assert_eq!(
            run(&["--staged".to_string(), "extra".to_string()]),
            ExitCode::from(2)
        );
    }

    #[test]
    fn maps_gated_files_to_labels() {
        let files = vec![
            ".github/workflows/ci.yml".to_string(),
            "Cargo.toml".to_string(),
            "crates/xtask/Cargo.toml".to_string(),
            "Cargo.lock".to_string(),
            "deny.toml".to_string(),
            "crates/cadmus/tests/snapshots/cli__help.snap".to_string(),
            "crates/cadmus/tests/snapshots/LEGACY.SNAP".to_string(),
            "src/main.rs".to_string(),
            "docs/roadmap.md".to_string(),
        ];
        let required = required_labels(&files);
        assert_eq!(
            required.keys().copied().collect::<Vec<_>>(),
            vec!["deps-ok", "github-ok", "snapshots-ok"]
        );
        assert_eq!(required["github-ok"], vec![".github/workflows/ci.yml"]);
        assert_eq!(required["deps-ok"].len(), 4);
        assert_eq!(required["snapshots-ok"].len(), 2);
        assert!(required_labels(&["README.md".to_string()]).is_empty());
    }
}
