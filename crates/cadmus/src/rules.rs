//! The client-side approval rule engine (ADR-0011 item 3 as amended
//! 2026-09-11 item 3; ADR-0018 item 8): ordered per-tool rules over the
//! call's subject, each carrying a decision and a grant scope. The approval
//! modes are presets over this rule layer — the modes-as-sugar shape of
//! `OpenCode`'s presets (the amendment's precedents: Codex `acceptForSession`,
//! `scope: turn|session`, `OpenCode` globs).
//!
//! This module is deliberately pure and unwired: no IO, no transport, no
//! TUI or config types. A follow-up slice composes it into the interactive
//! approval path — auto-decide when a rule grants, prompt the user when it
//! says ask, map `Deny` to a rejection carrying the optional comment — and
//! eventually replaces the auto-answering policies in `approval.rs` at that
//! composition point. Until then the engine answers only [`Decision`]s.
//!
//! Matching: the first rule whose tool glob matches the call's tool name
//! (a plain name is a literal glob; `*` alone catches every tool) and whose
//! input glob matches the call's subject claims the call. A call no rule
//! claims — or whose claiming rule's grant has lapsed — is answered `Ask`,
//! the safe default for gated calls: it mirrors the unattended-deny posture
//! (ADR-0011 item 3). The gate pairs the interactive wait with a
//! five-minute deny timeout (ADR-0008 item 4, the 2026-09-17 decision): an
//! ask the human never answers settles as a recorded rejection.
//!
//! Glob semantics (hand-rolled, minimal — third-party matchers stay out of
//! the tree per the dependency policy): `*` matches any run of characters,
//! *including* `/` and the empty run; `?` matches exactly one character;
//! every other character is literal. Matching is over the whole subject,
//! anchored at both ends; there is no escaping and no character classes, so
//! `**` is just two adjacent stars — the same meaning as one. `*` crossing
//! `/` is the deliberate choice: subjects are single-line tool inputs
//! (paths, patterns, commands, queries), and `src/*` covering
//! `src/deep/file.rs` matches the mainstream products' tool-input globs.
//!
//! The subject is the first non-empty string among the call's arguments
//! under the keys `pattern`, `command`, `path`, `query`, `file_path` (first
//! line only) — the key order of `cadmus_tui::transcript::tool_target`
//! (crates/cadmus-tui/src/transcript.rs), the SSOT both sides mirror until
//! the approval slice extracts a shared home. The marker's 48-cell display
//! truncation is a rendering concern and is not applied here: matching sees
//! the full first line.
//!
//! Scope bookkeeping: only `Allow` rules grant. A grant is recorded the
//! first time the engine is asked about a call matching such a rule; the
//! grant's own (recording) call still goes through the rule's decision.
//! [`Scope::Once`] covers the next matching call only, [`Scope::Turn`]
//! covers matching calls in the granting call's turn (fixed at recording;
//! a call in a new turn does not re-grant), and [`Scope::Session`] covers
//! every subsequent matching call. After a grant lapses, the rule's matches
//! fall back to `Ask` rather than to a later, broader rule — the first
//! match claims the call for good, and an expired allowance prompting again
//! is the safe direction. `Ask` and `Deny` rules re-decide every matching
//! call, so their scope field is inert.
//!
//! Assumption: the engine only ever sees calls the approval gate has already
//! flagged as needing approval (today the `Effect::Mutation` tools
//! `write_file` / `edit_file`, ADR-0008 item 4). The presets list exactly
//! those tools; `approve-writes` and `auto-edit` fall through to `Ask` —
//! the safe direction — while `read-only` fails closed: its catch-all `*`
//! rule denies every call no explicit rule claims, so a future mutation
//! tool cannot prompt its way past the mode's intent.
//!
//! Deferred, each with its consumer: the `persisted` grant scope needs the
//! TOML config layer (ADR-0018 item 7) — its consumer is the config slice,
//! and [`Scope`] is the extension point; the `plan` mode is its own slice
//! (read-only exploration plus a plan file plus mode-transition UX);
//! `shell_exec` is in no preset (phase 3, ADR-0008 item 1).

use cadmus_contract::ToolCall;

/// The per-call verdict the engine answers with.
///
/// The composing consumer maps this onto the wire vocabulary
/// (`cadmus_contract::Approval`): `Allow` becomes `Approved` without
/// prompting, `Ask` shows the approval prompt, `Deny` becomes `Rejected`.
/// The rejection's optional comment is minted at composition (a per-rule
/// reason field is a follow-up if the trajectory needs it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The call executes without asking.
    Allow,
    /// The call needs an explicit human decision (also the engine's default
    /// for a call no rule claims).
    Ask,
    /// The call never executes.
    Deny,
}

/// How far an [`Decision::Allow`] rule's grant extends past its granting
/// call (the 2026-09-11 amendment's once / turn / session scopes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The grant covers the next matching call only.
    Once,
    /// The grant covers matching calls in the granting call's turn.
    Turn,
    /// The grant covers every subsequent matching call.
    Session,
    // The amendment's fourth scope, `persisted`, is deliberately absent: it
    // needs the TOML config layer (ADR-0018 item 7) — see the module doc.
}

/// One ordered entry of a rule table. Rules are tried in order; the first
/// entry whose tool glob matches the call's tool name and whose input glob
/// matches the call's subject claims the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Tool name, matched as a glob (a plain name is a literal glob, so
    /// exact names keep exact semantics); `*` alone catches every tool.
    pub tool: String,
    /// Optional glob over the call's subject (derivation and semantics in
    /// the module doc); `None` matches any subject.
    pub input: Option<String>,
    /// The verdict for a matching call.
    pub decision: Decision,
    /// How far the grant extends when `decision` is [`Decision::Allow`];
    /// inert for `Ask` / `Deny`, which re-decide every matching call.
    pub scope: Scope,
}

/// A recorded grant: an [`Decision::Allow`] rule's allowance, extended per
/// its scope. Grants carry no decision of their own — they always allow.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Grant {
    scope: Scope,
    /// The granting call's turn ([`Scope::Turn`] compares against it).
    turn: u32,
    /// [`Scope::Once`]: consumed by the next matching call.
    once_used: bool,
}

impl Grant {
    fn covers(&self, turn: u32) -> bool {
        match self.scope {
            Scope::Session => true,
            Scope::Turn => self.turn == turn,
            Scope::Once => !self.once_used,
        }
    }
}

/// The stateful rule engine: an ordered rule table plus per-rule grant
/// bookkeeping. It is stateful because a grant recorded by one `decide`
/// must be observed by the next; construct one per run (the presets are the
/// mode constructors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    rules: Vec<Rule>,
    /// One slot per rule, parallel to `rules`: `None` until the rule's first
    /// matching call records the grant.
    grants: Vec<Option<Grant>>,
}

impl Rules {
    /// An engine over `rules`, tried in order, first match wins.
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        let grants = vec![None; rules.len()];
        Self { rules, grants }
    }

    /// `read-only` (ADR-0008's conservative posture): deny the mutation
    /// tools, and anything else that arrives unclaimed (the catch-all `*`
    /// rule) — the mode fails closed, so a future mutation tool cannot
    /// prompt its way past the intent.
    #[must_use]
    pub fn read_only() -> Self {
        let mut rules = mutation_rules(Decision::Deny);
        rules.push(Rule {
            tool: "*".into(),
            input: None,
            decision: Decision::Deny,
            scope: Scope::Session,
        });
        Self::new(rules)
    }

    /// `approve-writes`, the default mode: ask on the mutation tools —
    /// every matching call, since `Ask` rules never record a grant.
    #[must_use]
    pub fn approve_writes() -> Self {
        Self::new(mutation_rules(Decision::Ask))
    }

    /// `auto-edit`: allow the write tools for the rest of the run
    /// ([`Scope::Session`] — the first match records the grant, every later
    /// match rides it).
    #[must_use]
    pub fn auto_edit() -> Self {
        Self::new(mutation_rules(Decision::Allow))
    }

    /// The verdict for `call` in `turn`.
    ///
    /// `turn` is the turn the call arrives in (the gate's
    /// `ApprovalRequested.turn`); it is stored on the grant so a
    /// [`Scope::Turn`] rule can tell same-turn from later calls. Grants are
    /// recorded here, on the engine's first matching call — never at
    /// construction — and the recording call still answers with its rule's
    /// decision (see the module doc for the full scope semantics).
    pub fn decide(&mut self, call: &ToolCall, turn: u32) -> Decision {
        let subject = subject(call);
        for (index, rule) in self.rules.iter().enumerate() {
            if !glob_match(&rule.tool, &call.name) {
                continue;
            }
            if let Some(glob) = &rule.input
                && !glob_match(glob, subject)
            {
                continue;
            }
            if rule.decision != Decision::Allow {
                // Ask / Deny re-decide every matching call; only Allow
                // records a grant.
                return rule.decision;
            }
            return match &mut self.grants[index] {
                Some(grant) if grant.covers(turn) => {
                    if grant.scope == Scope::Once {
                        grant.once_used = true;
                    }
                    Decision::Allow
                }
                // Lapsed (the Once grant spent, or a later turn): the first
                // match still claims the call, and a gated call without a
                // live grant prompts again rather than falling through to a
                // later, broader rule.
                Some(_) => Decision::Ask,
                None => {
                    self.grants[index] = Some(Grant {
                        scope: rule.scope,
                        turn,
                        once_used: false,
                    });
                    rule.decision
                }
            };
        }
        Decision::Ask
    }
}

/// One preset entry per gated mutation tool (today `write_file` and
/// `edit_file`, ADR-0008 item 4). Scope is inert for `read-only`'s Deny and
/// `approve-writes`' Ask — those re-decide every call — and `Session` is the
/// auto-edit grant; it reads naturally and keeps the presets uniform.
fn mutation_rules(decision: Decision) -> Vec<Rule> {
    ["write_file", "edit_file"]
        .into_iter()
        .map(|tool| Rule {
            tool: tool.into(),
            input: None,
            decision,
            scope: Scope::Session,
        })
        .collect()
}

/// The subject a rule's input glob matches against: the first non-empty
/// string among the call's arguments under the keys `pattern`, `command`,
/// `path`, `query`, `file_path`, first line only — the key order of
/// `cadmus_tui::transcript::tool_target` (crates/cadmus-tui/src/transcript.rs),
/// the SSOT both sides mirror until the approval slice extracts a shared
/// home. The marker's 48-cell truncation is display-only and not applied:
/// matching sees the full first line.
fn subject(call: &ToolCall) -> &str {
    let Some(arguments) = call.arguments.as_object() else {
        return "";
    };
    for key in ["pattern", "command", "path", "query", "file_path"] {
        if let Some(value) = arguments.get(key).and_then(serde_json::Value::as_str) {
            let first_line = value.lines().next().unwrap_or_default();
            if !first_line.is_empty() {
                return first_line;
            }
        }
    }
    ""
}

/// Minimal glob over the subject (full semantics in the module doc): `*` is
/// any run of characters including `/` and the empty run, `?` is exactly one
/// character, everything else is literal, and the match is whole-subject,
/// anchored at both ends. The single-backtrack scan below is linear-time for
/// the realistic cases and O(pattern × subject) only under adversarial
/// star stacks; subjects are short single-line tool inputs.
fn glob_match(pattern: &str, subject: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let subject: Vec<char> = subject.chars().collect();
    let (mut p, mut s) = (0, 0);
    // Where the last `*` fell back to, and the subject position it will
    // stretch to on the next fallback.
    let mut backtrack: Option<(usize, usize)> = None;
    while s < subject.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == subject[s]) {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            backtrack = Some((p, s));
            p += 1;
        } else if let Some((star_p, star_s)) = backtrack {
            p = star_p + 1;
            s = star_s + 1;
            backtrack = Some((star_p, star_s + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use cadmus_contract::ToolCall;
    use serde_json::json;

    use super::*;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    /// A `write_file` call — the mutation tool every preset names.
    fn write(path: &str) -> ToolCall {
        call("write_file", json!({ "path": path, "content": "x" }))
    }

    fn edit(path: &str) -> ToolCall {
        call("edit_file", json!({ "path": path, "edits": [] }))
    }

    fn rule(tool: &str, input: Option<&str>, decision: Decision, scope: Scope) -> Rule {
        Rule {
            tool: tool.into(),
            input: input.map(str::to_owned),
            decision,
            scope,
        }
    }

    fn allow(scope: Scope) -> Rule {
        rule("write_file", None, Decision::Allow, scope)
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let mut allow_first = Rules::new(vec![
            rule("write_file", None, Decision::Allow, Scope::Session),
            rule("write_file", None, Decision::Deny, Scope::Session),
        ]);
        assert_eq!(allow_first.decide(&write("a"), 1), Decision::Allow);

        let mut deny_first = Rules::new(vec![
            rule("write_file", None, Decision::Deny, Scope::Session),
            rule("write_file", None, Decision::Allow, Scope::Session),
        ]);
        assert_eq!(deny_first.decide(&write("a"), 1), Decision::Deny);
    }

    #[test]
    fn tool_names_match_exactly() {
        let mut rules = Rules::new(vec![allow(Scope::Session)]);
        // Not a prefix match: the longer name claims no rule.
        assert_eq!(
            rules.decide(&call("write_files", json!({ "path": "a" })), 1),
            Decision::Ask
        );
    }

    #[test]
    fn a_star_tool_glob_catches_every_tool() {
        let mut rules = Rules::new(vec![rule("*", None, Decision::Deny, Scope::Session)]);
        assert_eq!(
            rules.decide(&call("any_future_tool", json!({ "path": "a" })), 1),
            Decision::Deny
        );
    }

    #[test]
    fn the_glob_table() {
        // (pattern, subject, expected) — whole-subject anchored matching.
        let cases = [
            ("src/*.rs", "src/main.rs", true), // `*` spans `/` (documented choice)
            ("src/*.rs", "src/deep/nested.rs", true),
            ("src/*", "src/", true),        // `*` covers the empty run
            ("src/?.rs", "src/a.rs", true), // `?` is exactly one character
            ("src/?.rs", "src/ab.rs", false),
            ("*", "", true),
            ("*", "any/thing at all", true),
            ("main.rs", "src/main.rs", false), // anchored: no substring match
            ("**/main.rs", "src/deep/main.rs", true), // no `**` special case: two stars = one
            ("", "", true),
            ("", "a", false),
            ("?", "", false),
            ("a.b", "a.b", true), // every other character is literal
            ("a.b", "axb", false),
            ("Cargo.*", "Cargo.toml", true),
            ("ü*.rs", "ümlaut.rs", true), // char-based, not byte-based
        ];
        for (pattern, subject, expected) in cases {
            assert_eq!(
                glob_match(pattern, subject),
                expected,
                "pattern {pattern:?} vs subject {subject:?}"
            );
        }
    }

    #[test]
    fn the_subject_follows_the_tool_target_key_order() {
        // `pattern` outranks `path`, `command` outranks both — the
        // transcript marker's order, mirrored here.
        let grep = call("grep", json!({ "pattern": "fn main", "path": "src" }));
        assert_eq!(subject(&grep), "fn main");

        let shellish = call(
            "shell_exec",
            json!({ "path": "src", "command": "cargo test" }),
        );
        assert_eq!(subject(&shellish), "cargo test");

        let multiline = write("src/a.rs\nsecret");
        assert_eq!(subject(&multiline), "src/a.rs");

        let none = call("todo_write", json!({ "todos": [] }));
        assert_eq!(subject(&none), "");
    }

    #[test]
    fn input_globs_narrow_the_match_to_the_subject() {
        let mut rules = Rules::new(vec![rule(
            "write_file",
            Some("src/**"),
            Decision::Allow,
            Scope::Session,
        )]);
        assert_eq!(rules.decide(&write("src/main.rs"), 1), Decision::Allow);
        assert_eq!(
            rules.decide(&write("src/deep/nested.rs"), 1),
            Decision::Allow
        );
        assert_eq!(rules.decide(&write("docs/readme.md"), 1), Decision::Ask);
    }

    #[test]
    fn an_unclaimed_call_is_asked() {
        let mut empty = Rules::new(vec![]);
        assert_eq!(empty.decide(&write("a"), 1), Decision::Ask);

        // The ask-presets claim no other tool: they fall through to Ask
        // (read-only is the closed preset — its catch-all denies).
        let mut approve_writes = Rules::approve_writes();
        assert_eq!(
            approve_writes.decide(&call("grep", json!({ "pattern": "x" })), 1),
            Decision::Ask
        );
    }

    #[test]
    fn an_once_grant_covers_the_next_matching_call_only() {
        let mut rules = Rules::new(vec![allow(Scope::Once)]);
        // The granting call goes through the rule's decision...
        assert_eq!(rules.decide(&write("a"), 1), Decision::Allow);
        // ...the grant covers exactly the next matching call...
        assert_eq!(rules.decide(&write("b"), 1), Decision::Allow);
        // ...and is spent afterwards, whatever the turn.
        assert_eq!(rules.decide(&write("c"), 1), Decision::Ask);
        assert_eq!(rules.decide(&write("d"), 2), Decision::Ask);
    }

    #[test]
    fn unrelated_calls_do_not_consume_an_once_grant() {
        let mut rules = Rules::new(vec![allow(Scope::Once)]);
        assert_eq!(rules.decide(&write("a"), 1), Decision::Allow);
        // A call no rule claims cannot spend the grant.
        assert_eq!(rules.decide(&edit("b"), 1), Decision::Ask);
        assert_eq!(rules.decide(&write("c"), 1), Decision::Allow);
        assert_eq!(rules.decide(&write("d"), 1), Decision::Ask);
    }

    #[test]
    fn a_turn_grant_expires_when_the_granting_turn_ends() {
        let mut rules = Rules::new(vec![allow(Scope::Turn)]);
        assert_eq!(rules.decide(&write("a"), 3), Decision::Allow); // the granting call, turn 3
        assert_eq!(rules.decide(&write("b"), 3), Decision::Allow); // same turn covered
        assert_eq!(rules.decide(&write("c"), 4), Decision::Ask); // a later turn is not
        // A later turn does not re-grant: the grant stays anchored at turn 3.
        assert_eq!(rules.decide(&write("d"), 5), Decision::Ask);
    }

    #[test]
    fn a_session_grant_covers_every_subsequent_call() {
        let mut rules = Rules::new(vec![allow(Scope::Session)]);
        for turn in 1..=5 {
            assert_eq!(rules.decide(&write("loop.rs"), turn), Decision::Allow);
        }
    }

    #[test]
    fn a_lapsed_grant_prompts_again_instead_of_falling_through() {
        // The first match claims the call for good: once the Once grant is
        // spent, the later Session rule must not allow the call — a gated
        // call without a live grant prompts again.
        let mut rules = Rules::new(vec![
            rule("write_file", None, Decision::Allow, Scope::Once),
            rule("write_file", None, Decision::Allow, Scope::Session),
        ]);
        assert_eq!(rules.decide(&write("a"), 1), Decision::Allow);
        assert_eq!(rules.decide(&write("b"), 1), Decision::Allow);
        assert_eq!(rules.decide(&write("c"), 1), Decision::Ask);
    }

    #[test]
    fn a_glob_scoped_turn_grant_covers_only_matching_subjects() {
        let mut rules = Rules::new(vec![rule(
            "write_file",
            Some("src/**"),
            Decision::Allow,
            Scope::Turn,
        )]);
        assert_eq!(rules.decide(&write("src/a"), 5), Decision::Allow);
        assert_eq!(rules.decide(&write("src/b"), 5), Decision::Allow);
        assert_eq!(rules.decide(&write("src/c"), 6), Decision::Ask);
        // A non-matching subject neither is covered nor re-grants.
        assert_eq!(rules.decide(&write("docs/d"), 6), Decision::Ask);
        assert_eq!(rules.decide(&write("src/e"), 7), Decision::Ask);
    }

    #[test]
    fn read_only_denies_everything_unclaimed() {
        // The mode fails closed: the mutation tools explicitly, every other
        // tool via the catch-all rule — a future mutation tool must never
        // see a prompt in read-only mode.
        let mut rules = Rules::read_only();
        assert_eq!(rules.decide(&write("a"), 1), Decision::Deny);
        assert_eq!(rules.decide(&write("b"), 1), Decision::Deny);
        assert_eq!(rules.decide(&edit("a"), 2), Decision::Deny);
        assert_eq!(
            rules.decide(&call("grep", json!({ "pattern": "x" })), 2),
            Decision::Deny
        );
    }

    #[test]
    fn approve_writes_asks_on_every_mutation_call() {
        let mut rules = Rules::approve_writes();
        for turn in 1..=3 {
            assert_eq!(rules.decide(&write("a"), turn), Decision::Ask);
            assert_eq!(rules.decide(&edit("a"), turn), Decision::Ask);
        }
    }

    #[test]
    fn auto_edit_allows_writes_and_edits_from_the_first_call() {
        let mut rules = Rules::auto_edit();
        assert_eq!(rules.decide(&write("a"), 1), Decision::Allow);
        assert_eq!(rules.decide(&edit("a"), 1), Decision::Allow);
        assert_eq!(rules.decide(&write("b"), 9), Decision::Allow);
    }
}
