//! Slash commands: the composer's client-side command namespace
//! (ADR-0011's interaction floor — "expanded client-side, no model
//! round-trip, no log noise"). A submitted line that parses as a command is
//! executed by the app itself: it never enters the run's messages, the
//! trajectory log, or the model's context.
//!
//! Recognition is deliberately narrow: the whole (trimmed) input must be a
//! single line of `/name` with no arguments — v1's commands take none, so
//! trailing text makes the name unknown rather than silently dropping
//! args, and a multi-line paste is always a prompt. A bare `/` is never a
//! prompt either: it opens the command listing (the palette's poor-man
//! equivalent until the input layer grows one). Recognition happens at
//! the idle prompt only: mid-run, Enter is a steer (ADR-0018's 2026-09-21
//! binding amendment) and slash-looking text rides that path like any
//! other text.
//!
//! [`TABLE`] is the one list of commands: [`parse`] looks the name up in
//! it and [`help_lines`] renders it — drift between what is accepted and
//! what is listed is impossible by construction.

use cadmus_contract::Usage;
use cadmus_ui::ir::{self, Slot};

use crate::transcript::subtle_line;

/// One recognized command. `Unknown` carries the typed name for the note.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Slash {
    Help,
    Usage,
    Clear,
    Quit,
    Unknown(String),
}

/// The command table: name → one-line description → the command.
const TABLE: &[(&str, &str, Slash)] = &[
    ("help", "list the commands", Slash::Help),
    ("usage", "token usage this session", Slash::Usage),
    ("clear", "start a new conversation", Slash::Clear),
    ("quit", "exit cadmus", Slash::Quit),
];

/// Parse a submitted line into its command, or `None` when it is a prompt
/// (no leading `/`, or a multi-line text). Matching is case-sensitive —
/// the mainstream names are lowercase, and guessing case corrections
/// hides the typos the unknown-command note surfaces.
pub(crate) fn parse(text: &str) -> Option<Slash> {
    let name = text.trim().strip_prefix('/')?;
    if name.contains('\n') {
        return None;
    }
    if name.is_empty() {
        return Some(Slash::Help);
    }
    Some(
        TABLE
            .iter()
            .find(|(listed, ..)| *listed == name)
            .map_or_else(
                || Slash::Unknown(name.to_string()),
                |(.., command)| command.clone(),
            ),
    )
}

/// The `/help` block: the framing rule, the rendered table, and the
/// recognition-policy truth (the module docs' user-facing half).
pub(crate) fn help_lines() -> Vec<ir::Line> {
    let mut lines = vec![subtle_line(
        "Commands run client-side — they never reach the model or the log.",
    )];
    let width = TABLE.iter().map(|(name, ..)| name.len()).max().unwrap_or(0);
    for (name, description, _) in TABLE {
        lines.push(ir::Line::from_spans(vec![
            ir::Span::slotted(format!("/{name:<width$}"), Slot::Accent),
            ir::Span::slotted(format!("  {description}"), Slot::TextSubtle),
        ]));
    }
    lines.push(subtle_line(
        "Idle prompt only — mid-run, Enter injects text into the running turn.",
    ));
    lines
}

/// The unknown-command note: the typed name echoed, `/help` pointed at.
/// The line never becomes a prompt — a leading-slash typo must not burn a
/// model turn.
pub(crate) fn unknown_line(name: &str) -> ir::Line {
    subtle_line(format!("Unknown command /{name} — /help lists them."))
}

/// The `/clear` boundary marker: the scrollback above stays (the inline
/// shell never rewrites terminal history); the conversation below is new.
pub(crate) fn cleared_line() -> ir::Line {
    subtle_line("New conversation — the trajectory log keeps the old one.")
}

/// The session's token totals for `/usage`, accumulated from fresh
/// recorded responses at the app's token seam. Client-side accumulation:
/// the trajectory log stays the exact, replayable record (ADR-0011's
/// numbers-from-the-log rule) — this is the session-at-a-glance view.
/// `/clear` resets the totals: a session is the conversation between
/// clears (maintainer, 2026-09-24), not the process's lifetime.
#[derive(Default)]
pub(crate) struct SessionUsage {
    /// Responses that carried a usage report (≈ model requests).
    responses: u64,
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
}

impl SessionUsage {
    /// Fold one response's usage report into the session totals.
    pub(crate) fn note(&mut self, usage: &Usage) {
        self.responses += 1;
        self.input += usage.input;
        self.cache_read += usage.cache_read;
        self.cache_write += usage.cache_write;
        self.output += usage.output;
        self.reasoning += usage.reasoning;
    }
}

/// The `/usage` block: exact counts (the numbers are replayable from the
/// log), grouped for readability. Nothing is estimated and no cost is
/// shown — the pricing source is an open item, and a guessed price is
/// worse than none.
pub(crate) fn usage_lines(usage: &SessionUsage) -> Vec<ir::Line> {
    if usage.responses == 0 {
        return vec![subtle_line("No model usage yet this session.")];
    }
    vec![
        subtle_line(format!(
            "{} model request{} this session",
            grouped(usage.responses),
            if usage.responses == 1 { "" } else { "s" },
        )),
        subtle_line(format!(
            "input {} · cache read {} · cache write {}",
            grouped(usage.input),
            grouped(usage.cache_read),
            grouped(usage.cache_write),
        )),
        subtle_line(format!(
            "output {} · reasoning {}",
            grouped(usage.output),
            grouped(usage.reasoning),
        )),
    ]
}

/// `1234567` → `"1,234,567"`: exact token counts stay readable.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_name_parses_to_its_command() {
        for (name, _, expected) in TABLE {
            assert_eq!(parse(&format!("/{name}")).as_ref(), Some(expected));
        }
    }

    #[test]
    fn recognition_is_exactly_a_single_slash_name() {
        assert_eq!(parse("help"), None);
        assert_eq!(parse("/help\nfix it"), None);
        assert_eq!(parse("/HELP"), Some(Slash::Unknown("HELP".to_string())));
        // A bare `/` opens the listing; it is never a prompt.
        assert_eq!(parse("/"), Some(Slash::Help));
        // v1 takes no arguments: trailing text makes the name unknown
        // rather than dropping it.
        assert_eq!(
            parse("/help me"),
            Some(Slash::Unknown("help me".to_string()))
        );
        assert_eq!(parse("/diff"), Some(Slash::Unknown("diff".to_string())));
        // The trim lives here, not at the call site.
        assert_eq!(parse(" /help  "), Some(Slash::Help));
    }

    #[test]
    fn help_renders_every_command() {
        let text = help_lines().iter().map(ir::Line::text).collect::<String>();
        for (name, description, _) in TABLE {
            assert!(text.contains(&format!("/{name}")), "missing /{name}");
            assert!(text.contains(description), "missing {description}");
        }
    }

    #[test]
    fn usage_has_an_honest_zero_state() {
        assert_eq!(
            texts_ir(&usage_lines(&SessionUsage::default())),
            vec!["No model usage yet this session.".to_string()]
        );
    }

    #[test]
    fn usage_totals_accumulate_across_responses() {
        let mut usage = SessionUsage::default();
        usage.note(&Usage {
            input: 45_000,
            cache_read: 200,
            output: 1_500,
            ..Default::default()
        });
        usage.note(&Usage {
            input: 5_000,
            reasoning: 600,
            ..Default::default()
        });
        assert_eq!(
            texts_ir(&usage_lines(&usage)),
            vec![
                "2 model requests this session".to_string(),
                "input 50,000 · cache read 200 · cache write 0".to_string(),
                "output 1,500 · reasoning 600".to_string(),
            ]
        );
    }

    /// `ir::Line`s as plain text for content assertions.
    fn texts_ir(lines: &[ir::Line]) -> Vec<String> {
        lines.iter().map(ir::Line::text).collect()
    }

    #[test]
    fn grouped_inserts_thousands_separators() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(42), "42");
        assert_eq!(grouped(1_500), "1,500");
        assert_eq!(grouped(45_000), "45,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
