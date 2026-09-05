//! Validate a commit message (or PR title) against this repo's convention —
//! modified Conventional Commits (see CONTRIBUTING.md; this module is the
//! SSOT). Commit types are version intent: release-plz derives the semver bump
//! and the changelog from them, so a malformed header is a wrong release.
//!
//! Convention:
//!   header:  `type(scope)!: subject` — type from [`TYPES`], optional lowercase
//!            `[a-z0-9-]+` scope, optional `!` breaking marker; subject pure
//!            ASCII; whole header at most 100 chars
//!   body:    free-form (any language), separated from the header by one blank
//!            line. The squash merge lands the PR body as the commit body
//!            verbatim — the whole body must be landable, so process
//!            scaffolding is rejected outright: no HTML comments (delete the
//!            template's comments instead of merging them), no task-list items
//!            (`- [ ]`) and no bare `---` lines (the two-zone template with a
//!            cut line is gone); these three checks apply to every line,
//!            fenced code blocks included. A prose paragraph runs at most
//!            [`BODY_PROSE_RUN_MAX`] lines — longer bodies must be split into
//!            paragraphs or formatted as lists (walls of text read badly in
//!            `git log`; list items, blockquotes and fenced code are exempt)
//!   footer:  `TOKEN: value` / `TOKEN #value` (incl. `BREAKING CHANGE:`) must
//!            start the footer block, preceded by a blank line

use std::process::ExitCode;

const HEADER_MAX: usize = 100;
/// Longest tolerated run of consecutive prose lines in a body. Set from the
/// evidence on main: the longest paragraph in an accepted body is 7 lines.
const BODY_PROSE_RUN_MAX: usize = 7;
const TYPES: [&str; 11] = [
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

/// Entry point: `check-commit [FILE]` (`-` or no argument reads stdin).
pub fn run(args: &[String]) -> ExitCode {
    if args.len() > 1 {
        eprintln!("usage: check-commit [FILE]  ('-' or no arg = stdin)");
        return ExitCode::from(2);
    }
    let text = match args.first().map(String::as_str) {
        None | Some("-") => read_stdin(),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("error: cannot read {path}: {err}");
                return ExitCode::from(2);
            }
        },
    };
    let errors = check(&text);
    if errors.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("commit message convention violations:");
    for error in &errors {
        eprintln!("  - {error}");
    }
    ExitCode::FAILURE
}

fn read_stdin() -> String {
    use std::io::Read as _;
    let mut text = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut text) {
        eprintln!("error: cannot read stdin: {err}");
        std::process::exit(2);
    }
    text
}

/// All convention violations in `text`, in message order.
#[must_use]
pub fn check(text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    // git strips `#` comment lines *after* the commit-msg hook runs — do the same
    let mut lines: Vec<&str> = text
        .split('\n')
        .filter(|line| !line.starts_with('#'))
        .collect();
    while matches!(lines.last(), Some(&"")) {
        lines.pop();
    }
    if lines.is_empty() {
        return vec!["message is empty".to_string()];
    }

    let header = lines[0];
    if header.chars().count() > HEADER_MAX {
        errors.push(format!(
            "header is {} chars (max {HEADER_MAX})",
            header.chars().count()
        ));
    }
    match parse_header(header) {
        None => errors.push(
            "header must match `type(scope): subject` (lowercase type, optional lowercase scope, optional `!`)"
                .to_string(),
        ),
        Some((ty, subject)) => {
            if !TYPES.contains(&ty) {
                errors.push(format!(
                    "unknown type {ty:?}; allowed: {}",
                    TYPES.join(", ")
                ));
            }
            if subject.is_empty() {
                errors.push("subject must not be empty".to_string());
            } else {
                if !subject.is_ascii() {
                    errors.push("subject must be pure ASCII (English)".to_string());
                }
                if subject != subject.trim() {
                    errors.push(
                        "subject must not have leading/trailing whitespace".to_string()
                    );
                }
            }
        }
    }

    if lines.len() > 1 && !lines[1].is_empty() {
        errors.push("header and body must be separated by a blank line".to_string());
    }
    scaffolding_errors(&lines, &mut errors);
    for (index, line) in lines.iter().enumerate().skip(1) {
        if is_footer_start(line) && !lines[index - 1].is_empty() {
            let preview: String = line.chars().take(30).collect();
            errors.push(format!(
                "footer line {} ({preview:?}…) must be preceded by a blank line",
                index + 1
            ));
        }
    }
    let mut in_fence = false;
    let mut run_start = 0;
    let mut run_len = 0;
    for (index, line) in lines.iter().enumerate().skip(2) {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            run_len = 0;
            continue;
        }
        if in_fence || !is_prose_line(trimmed) {
            run_len = 0;
            continue;
        }
        if run_len == 0 {
            run_start = index;
        }
        run_len += 1;
        if run_len == BODY_PROSE_RUN_MAX + 1 {
            errors.push(format!(
                "body paragraph at line {} runs past {BODY_PROSE_RUN_MAX} prose lines — split it into shorter paragraphs or use a list",
                run_start + 1
            ));
        }
    }
    errors
}

/// Lines that must never land in history: HTML template comments, task-list
/// items and bare `---` cut lines (the PR body is merged verbatim, so process
/// scaffolding is rejected outright).
fn scaffolding_errors(lines: &[&str], errors: &mut Vec<String>) {
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if line.contains("<!--") {
            errors.push(format!(
                "line {} contains an HTML comment (`<!--`) — delete the template's comments instead of merging them",
                index + 1
            ));
        }
        if trimmed.starts_with("- [ ]")
            || trimmed.starts_with("- [x]")
            || trimmed.starts_with("- [X]")
        {
            errors.push(format!(
                "line {} is a task-list item (`- [ ]`) — checklist scaffolding must not land in history",
                index + 1
            ));
        }
        if trimmed == "---" {
            errors.push(format!(
                "line {} is a bare `---` — the cut-line convention is gone; the whole PR body lands as the commit body",
                index + 1
            ));
        }
    }
}

/// Whether a body line reads as flowing prose. Formatted content is exempt —
/// list items and blockquotes stay readable at any length. (`---` stays
/// exempt here so a rejected cut line doesn't double-report as prose.)
fn is_prose_line(trimmed: &str) -> bool {
    if trimmed.is_empty()
        || trimmed == "---"
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("> ")
    {
        return false;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &trimmed[digits..];
    let numbered = digits > 0 && (rest.starts_with(". ") || rest.starts_with(") "));
    !numbered
}

/// Split `type(scope)!: subject` into `(type, subject)`, validating the
/// prefix shape. Hand-rolled equivalent of the former Python regex
/// `^([a-z]+)(\(([a-z0-9-]+)\))?(!)?: (.*)$`.
fn parse_header(header: &str) -> Option<(&str, &str)> {
    let (prefix, subject) = header.split_once(": ")?;
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
    let (ty, scope) = match prefix.split_once('(') {
        Some((ty, rest)) => (ty, Some(rest.strip_suffix(')')?)),
        None => (prefix, None),
    };
    if ty.is_empty() || !ty.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    if let Some(scope) = scope
        && (scope.is_empty()
            || !scope
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
    {
        return None;
    }
    Some((ty, subject))
}

/// A line that opens a footer token: `BREAKING CHANGE`/`BREAKING-CHANGE` or
/// `[A-Za-z][A-Za-z0-9-]*` followed by `: ` or ` #`.
fn is_footer_start(line: &str) -> bool {
    let token_len = if line.starts_with("BREAKING CHANGE") {
        "BREAKING CHANGE".len()
    } else if line.starts_with("BREAKING-CHANGE") {
        "BREAKING-CHANGE".len()
    } else {
        let mut len = 0;
        for (index, byte) in line.bytes().enumerate() {
            let valid = if index == 0 {
                byte.is_ascii_alphabetic()
            } else {
                byte.is_ascii_alphanumeric() || byte == b'-'
            };
            if !valid {
                break;
            }
            len += 1;
        }
        if len == 0 {
            return false;
        }
        len
    };
    let rest = &line[token_len..];
    rest.starts_with(": ") || rest.starts_with(" #")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors_of(message: &str) -> Vec<String> {
        check(message)
    }

    #[test]
    fn accepts_plain_scoped_and_breaking_headers() {
        assert!(errors_of("feat: add thing").is_empty());
        assert!(errors_of("fix(cli): correct thing").is_empty());
        assert!(errors_of("chore!: drop old thing").is_empty());
        assert!(errors_of("docs(api): note").is_empty());
    }

    #[test]
    fn rejects_missing_type_and_non_ascii_subject() {
        assert!(!errors_of("initial commit").is_empty());
        assert!(!errors_of("feat: 添加了什么").is_empty());
        assert!(!errors_of("FEAT: loud").is_empty());
        assert!(!errors_of("feat(CLI): loud scope").is_empty());
    }

    #[test]
    fn enforces_header_length_and_blank_separator() {
        let long = format!("feat: {}", "x".repeat(HEADER_MAX));
        assert!(!errors_of(&long).is_empty());
        assert!(!errors_of("feat: x\nbody without blank line").is_empty());
        assert!(errors_of("feat: x\n\nbody is fine").is_empty());
    }

    #[test]
    fn footers_need_a_preceding_blank_line() {
        assert!(errors_of("feat: x\n\nbody\n\nBREAKING CHANGE: y").is_empty());
        assert!(!errors_of("feat: x\nbody\nBREAKING CHANGE: y").is_empty());
        assert!(!errors_of("feat: x\nbody\nReviewed-by: someone").is_empty());
        assert!(errors_of("feat: x\n\nReviewed-by: someone").is_empty());
    }

    #[test]
    fn rejects_html_comments_anywhere() {
        assert!(!errors_of("feat: x\n\nbody\n\n<!-- template note -->").is_empty());
        assert!(!errors_of("feat: x\n\n<!-- lone comment -->").is_empty());
        assert!(errors_of("feat: x\n\nbody without comments").is_empty());
    }

    #[test]
    fn long_prose_paragraphs_must_be_split() {
        let prose = |n: usize| {
            (1..=n)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // the boundary follows the longest paragraph in an accepted main body
        assert!(errors_of(&format!("feat: x\n\n{}", prose(BODY_PROSE_RUN_MAX))).is_empty());
        assert!(!errors_of(&format!("feat: x\n\n{}", prose(BODY_PROSE_RUN_MAX + 1))).is_empty());
        // splitting the same text into two paragraphs is fine
        let split = format!("feat: x\n\n{}\n\n{}", prose(4), prose(4));
        assert!(errors_of(&split).is_empty());
        // one violation per run, not one per line
        assert_eq!(
            errors_of(&format!("feat: x\n\n{}", prose(BODY_PROSE_RUN_MAX + 3))).len(),
            1
        );
    }

    #[test]
    fn formatted_body_blocks_are_not_prose() {
        let bullets = (1..=10)
            .map(|i| format!("- item {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errors_of(&format!("feat: x\n\n{bullets}")).is_empty());
        let numbered = (1..=10)
            .map(|i| format!("{i}. item"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errors_of(&format!("feat: x\n\n{numbered}")).is_empty());
        let quote = (1..=10)
            .map(|i| format!("> note {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(errors_of(&format!("feat: x\n\n{quote}")).is_empty());
        let fence = format!("feat: x\n\n```text\n{}\n```", "log line\n".repeat(10));
        assert!(errors_of(&fence).is_empty());
    }

    #[test]
    fn rejects_task_lists_and_cut_lines() {
        // scaffolding from the old two-zone template must not land in history
        assert!(!errors_of("feat: x\n\nbody\n\n- [ ] open item").is_empty());
        assert!(!errors_of("feat: x\n\nbody\n\n- [x] done item").is_empty());
        assert!(!errors_of("feat: x\n\nbody\n\n  - [X] nested item").is_empty());
        assert!(!errors_of("feat: x\n\nbody\n\n---").is_empty());
        assert!(!errors_of("feat: x\n\nbody\n\n\n---\n\nmore").is_empty());
        // plain lists, prose and em-dash usage stay legal
        assert!(errors_of("feat: x\n\nbody\n\n- plain item\n- another --- with text").is_empty());
    }

    #[test]
    fn git_style_comment_lines_are_stripped_first() {
        assert!(errors_of("feat: x\n# Please enter the commit message").is_empty());
        assert_eq!(errors_of("# only a comment"), vec!["message is empty"]);
    }
}
