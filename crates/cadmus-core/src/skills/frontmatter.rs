//! The minimal SKILL.md frontmatter parser (ADR-0006), shared by two
//! consumers: the runtime skill loader (the `cadmus` crate, via
//! `cadmus_core::skills`) and the `agent-check` smoke check (`xtask`, via a
//! `#[path]` include of this very file — xtask's zero-dependency policy
//! forbids a Cargo edge, so one source compiles in both crates).
//!
//! Constraints from that sharing: std-only, and no `crate::` references —
//! the file must compile identically under either crate root.
//!
//! There is no YAML parser in std, so this validates against what strict
//! YAML hosts reject while supporting only the single-line `key: value`
//! subset SKILL.md frontmatter uses: an unquoted `: ` (or a trailing `:`)
//! ends the scalar there, and a leading indicator char starts a construct
//! this line-based format forbids.

use std::collections::HashMap;
use std::fmt;

/// One non-fatal frontmatter parse problem. Parsing continues past it, so a
/// checker reports every issue in one pass; the caller decides whether the
/// file loads at all. The `Display` text is the checker/runtime diagnostic —
/// it is pinned by the tests below because two surfaces prepend their own
/// path context to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontmatterIssue {
    /// A line with no `key: value` shape at all.
    UnparseableLine(String),
    /// A quoted scalar missing its closing quote.
    UnterminatedQuote(String),
    /// A plain scalar containing `: ` or ending in `:` — strict YAML rejects
    /// it; the fix is quoting the value.
    PlainScalarColon(String),
    /// A value starting with a YAML indicator char — a construct this
    /// line-based format forbids.
    IndicatorStart(String),
}

impl fmt::Display for FrontmatterIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnparseableLine(line) => write!(f, "unparseable frontmatter line: {line:?}"),
            Self::UnterminatedQuote(key) => {
                write!(f, "unterminated quoted scalar for {key:?}")
            }
            Self::PlainScalarColon(key) => write!(
                f,
                "plain scalar for {key:?} contains ':' — strict YAML hosts reject this; wrap the value in double quotes"
            ),
            Self::IndicatorStart(key) => write!(
                f,
                "value for {key:?} starts with a YAML indicator — only single-line plain or quoted scalars are supported in SKILL.md frontmatter"
            ),
        }
    }
}

/// Split raw file text into (frontmatter, body), keeping the two failure
/// modes apart for actionable messages. Windows checkouts may carry CRLF
/// line endings (git autocrlf); YAML hosts accept both, so normalize before
/// the line-oriented split.
pub fn split_frontmatter(text: &str) -> Result<(String, String), &'static str> {
    let text = text.replace("\r\n", "\n");
    let Some(rest) = text.strip_prefix("---\n") else {
        return Err("missing YAML frontmatter (must start with ---)");
    };
    let Some(end) = rest.find("\n---\n") else {
        return Err("frontmatter is not closed with ---");
    };
    Ok((rest[..end].to_string(), rest[end + 5..].to_string()))
}

/// Parse the single-line `key: value` subset this format supports. Returns
/// the parsed pairs plus every issue found — an issue never aborts the
/// parse, so a checker reports all problems in one pass.
#[must_use]
pub fn parse_frontmatter(text: &str) -> (HashMap<String, String>, Vec<FrontmatterIssue>) {
    let mut frontmatter = HashMap::new();
    let mut issues = Vec::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            issues.push(FrontmatterIssue::UnparseableLine(line.to_string()));
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        if value.starts_with('"') || value.starts_with('\'') {
            let quote = value.as_bytes()[0];
            if value.len() < 2 || !value.ends_with(char::from(quote)) {
                issues.push(FrontmatterIssue::UnterminatedQuote(key));
                continue;
            }
        } else if !value.is_empty() {
            if value.contains(": ") || value.ends_with(':') {
                issues.push(FrontmatterIssue::PlainScalarColon(key));
                continue;
            }
            if ">|&*!%@`[{".contains(value.as_bytes()[0] as char) {
                issues.push(FrontmatterIssue::IndicatorStart(key));
                continue;
            }
        }
        frontmatter.insert(key, value.trim_matches(['"', '\'']).to_string());
    }
    (frontmatter, issues)
}

/// Validates one parsed SKILL.md frontmatter against the Agent Skills spec
/// (agentskills.io, verified 2026-09-10): `name` and `description` are
/// required; the name is 1–64 chars of lowercase letters, digits and
/// hyphens with no leading, trailing or consecutive hyphens, and must equal
/// the skill's directory name; the description is 1–1024 chars.
///
/// This is the one validator both surfaces share — the runtime loader
/// (warn-skip) and the agent-check smoke gate (hard failure) must agree on
/// what a loadable skill is, or the repo could ship a skill its own loader
/// refuses. The failure text is the diagnostic both prepend context to.
pub fn validate<S: ::std::hash::BuildHasher>(
    frontmatter: &HashMap<String, String, S>,
    dir_name: &str,
) -> Result<(String, String), String> {
    let name = frontmatter
        .get("name")
        .ok_or_else(|| "frontmatter requires `name`".to_string())?;
    // ASCII-only by construction: the byte check rejects anything else, so
    // byte length equals char count for every accepted name.
    let well_formed = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if !well_formed {
        return Err(format!(
            "name {name:?} must be 1-64 chars of lowercase letters, digits and hyphens, with no leading, trailing or consecutive hyphens"
        ));
    }
    if name != dir_name {
        return Err(format!(
            "name {name:?} must equal directory name {dir_name:?}"
        ));
    }
    let description = frontmatter
        .get("description")
        .ok_or_else(|| "frontmatter requires `description`".to_string())?;
    let chars = description.chars().count();
    if !(1..=1024).contains(&chars) {
        return Err(format!("description must be 1..1024 chars (got {chars})"));
    }
    Ok((name.clone(), description.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stringified issues, so assertions can match on the exact diagnostic
    /// text both surfaces prepend their path context to.
    fn parse(text: &str) -> (HashMap<String, String>, Vec<String>) {
        let (frontmatter, issues) = parse_frontmatter(text);
        (
            frontmatter,
            issues.iter().map(ToString::to_string).collect(),
        )
    }

    #[test]
    fn split_tolerates_crlf_checkouts() {
        // git materializes CRLF on Windows checkouts (autocrlf); YAML hosts
        // accept both endings, so the parser must too.
        let (frontmatter, body) =
            split_frontmatter("---\r\nname: x\r\n---\r\nbody\r\n").expect("valid frontmatter");
        assert_eq!(frontmatter, "name: x");
        assert_eq!(body, "body\n");
    }

    #[test]
    fn split_distinguishes_open_and_close_failures() {
        assert_eq!(
            split_frontmatter("name: x\n").unwrap_err(),
            "missing YAML frontmatter (must start with ---)"
        );
        assert_eq!(
            split_frontmatter("---\nname: x\n").unwrap_err(),
            "frontmatter is not closed with ---"
        );
    }

    #[test]
    fn plain_scalars_parse() {
        let (frontmatter, issues) = parse("name: pr-preflight\ndescription: short\n");
        assert!(issues.is_empty());
        assert_eq!(frontmatter["description"], "short");
    }

    #[test]
    fn unquoted_colon_space_is_rejected() {
        let (_, issues) = parse("description: layer on top: diff self-review\n");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("wrap the value in double quotes"));
    }

    #[test]
    fn quoted_colon_space_is_accepted() {
        let (frontmatter, issues) = parse("description: \"layer on top: diff\"\n");
        assert!(issues.is_empty());
        assert_eq!(frontmatter["description"], "layer on top: diff");
    }

    #[test]
    fn unterminated_quote_and_indicators_are_rejected() {
        let (_, issues) = parse("description: \"never closed\n");
        assert_eq!(issues.len(), 1);
        let (_, issues) = parse("description: >- folded\n");
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn trailing_colon_plain_scalar_is_rejected() {
        let (_, issues) = parse_frontmatter("name: foo:\n");
        assert_eq!(
            issues,
            vec![FrontmatterIssue::PlainScalarColon("name".into())]
        );
    }

    #[test]
    fn comment_and_empty_lines_are_skipped() {
        let (frontmatter, issues) = parse("# a comment\n\nname: x\n");
        assert!(issues.is_empty());
        assert_eq!(frontmatter["name"], "x");
    }

    #[test]
    fn a_bare_quote_char_is_not_a_terminated_scalar() {
        // `"` alone satisfies starts_with == ends_with on the same character;
        // the length guard is what rejects it.
        let (_, issues) = parse_frontmatter("name: \"\n");
        assert_eq!(
            issues,
            vec![FrontmatterIssue::UnterminatedQuote("name".into())]
        );
    }

    #[test]
    fn bracket_indicators_are_rejected_too() {
        let (_, issues) = parse_frontmatter("name: [flow-seq]\n");
        assert_eq!(
            issues,
            vec![FrontmatterIssue::IndicatorStart("name".into())]
        );
    }

    #[test]
    fn issue_messages_are_pinned_verbatim() {
        // Two surfaces prepend their own path context to these strings; a
        // wording drift here silently changes the agent-check output.
        let (_, issues) = parse("description: layer on top: diff\n");
        assert_eq!(
            issues[0],
            "plain scalar for \"description\" contains ':' — strict YAML hosts reject this; wrap the value in double quotes"
        );
        let (_, issues) = parse("not a pair\n");
        assert_eq!(issues[0], "unparseable frontmatter line: \"not a pair\"");
        let (_, issues) = parse("name: \"open\n");
        assert_eq!(issues[0], "unterminated quoted scalar for \"name\"");
        let (_, issues) = parse("name: > folded\n");
        assert_eq!(
            issues[0],
            "value for \"name\" starts with a YAML indicator — only single-line plain or quoted scalars are supported in SKILL.md frontmatter"
        );
    }

    fn frontmatter(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_valid_skill_validates() {
        let (name, description) = validate(
            &frontmatter(&[("name", "pr-preflight"), ("description", "review a PR")]),
            "pr-preflight",
        )
        .expect("valid");
        assert_eq!(name, "pr-preflight");
        assert_eq!(description, "review a PR");
    }

    #[test]
    fn name_and_description_are_required() {
        let err = validate(&frontmatter(&[("description", "d")]), "x").unwrap_err();
        assert_eq!(err, "frontmatter requires `name`");
        let err = validate(&frontmatter(&[("name", "x")]), "x").unwrap_err();
        assert_eq!(err, "frontmatter requires `description`");
    }

    #[test]
    fn the_name_charset_is_the_spec_subset() {
        for bad in ["Foo", "a_b", "a b", "-lead", "trail-", "double--dash", ""] {
            let err =
                validate(&frontmatter(&[("name", bad), ("description", "d")]), bad).unwrap_err();
            assert!(err.contains("1-64 chars"), "{bad:?}: {err}");
        }
        // 65 chars exceeds the cap; 64 is the last legal length.
        let long = "a".repeat(65);
        let err = validate(
            &frontmatter(&[("name", &long), ("description", "d")]),
            &long,
        )
        .unwrap_err();
        assert!(err.contains("1-64 chars"));
        let max = "a".repeat(64);
        validate(&frontmatter(&[("name", &max), ("description", "d")]), &max).expect("64 is legal");
    }

    #[test]
    fn the_name_must_equal_the_directory_name() {
        let err = validate(
            &frontmatter(&[("name", "x"), ("description", "d")]),
            "other",
        )
        .unwrap_err();
        assert_eq!(err, "name \"x\" must equal directory name \"other\"");
    }

    #[test]
    fn the_description_length_window_is_enforced() {
        let err = validate(&frontmatter(&[("name", "x"), ("description", "")]), "x").unwrap_err();
        assert_eq!(err, "description must be 1..1024 chars (got 0)");
        let over = "d".repeat(1025);
        let err =
            validate(&frontmatter(&[("name", "x"), ("description", &over)]), "x").unwrap_err();
        assert_eq!(err, "description must be 1..1024 chars (got 1025)");
        let max = "d".repeat(1024);
        validate(&frontmatter(&[("name", "x"), ("description", &max)]), "x")
            .expect("1024 is legal");
    }
}
