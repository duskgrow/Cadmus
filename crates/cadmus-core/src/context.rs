//! The runtime context pipeline (ADR-0007): three-segment request assembly —
//! the frozen prefix, the conversation history, and the per-request status
//! trailer. Pure logic: instruction files arrive as values, git freshness
//! through the injected [`StatusProbe`], nested-file discovery through the
//! injected [`InstructionTracker`] — so every byte the model sees is
//! reproducible from the trajectory alone.

use std::collections::BTreeMap;

use cadmus_contract::{
    FoldedRef, InstructionFile, Message, PrefixRecord, SkillSummary, TodoItem, TodoStatus, ToolSpec,
};

/// The v1 system prompt (ADR-0007 item 1(a)). Two content rules, both
/// load-bearing: it never names an individual tool — tool-specific guidance
/// rides each tool's own `description`, so disabling a tool removes its
/// rules with its schema — and its first line is the identity slot the
/// persona/profile layer (open item) will replace, leaving the SOP body
/// untouched. Any edit is a prefix change: it invalidates eval
/// comparability via the prefix hash.
pub const SYSTEM_PROMPT: &str = "\
You are Cadmus, a coding agent working in a terminal workspace.

Working discipline:
- Verify before you claim done: run the build, tests or checks that prove the change works; never report success on assumption.
- Tool errors are corrections, not failures: read the feedback, adjust, and retry with a better approach.
- Read before you write: understand the existing code and its conventions before changing them; match the style you find.
- Keep changes minimal and scoped to the task; do not refactor what is not broken.
- Never abandon a broken intermediate state: finish the change or revert it, so the workspace is never left worse than you found it.
- When a task is unclear or has multiple valid interpretations, ask instead of guessing.

A status block maintained by code (never by you) ends every request: cwd, git state, the clock, tool counters and the task list. Trust it over your own recollection of these facts.";

/// `todo_write`'s wire name, shared by the loop (which folds its calls into
/// the trailer state) and the wiring layer's tool definition — one SSOT for
/// the one tool the loop knows by name (ADR-0007 item 1(c)'s exception).
pub const TODO_WRITE: &str = "todo_write";

/// The frozen prefix (ADR-0007 item 1(a)): assembled once per run from the
/// system prompt, the instruction chain, the skill catalog and the tool
/// specs, then byte-stable for the whole run — the prompt-cache boundary.
/// The hash is the comparability key: runs with different hashes are not
/// eval-comparable (ADR-0010).
pub struct FrozenPrefix {
    text: String,
    hash: String,
    instructions: Vec<InstructionFile>,
    skills: Vec<SkillSummary>,
}

impl FrozenPrefix {
    /// Assembles the prefix and its hash. `specs` enter the hash (a tool
    /// schema change is a prefix change) but not the system text — on the
    /// wire they travel in the request's `tools` field, in wire order. The
    /// skill catalog renders as level-1 name+description lines only
    /// (ADR-0006's progressive disclosure): bodies stay out of the prefix
    /// and load on activation. The section text names no tool — the
    /// zero-tool-names rule of the system prompt applies here too; the
    /// activation loop closes in the activating tool's own description.
    #[must_use]
    pub fn assemble(
        system_prompt: &str,
        instructions: &[InstructionFile],
        skills: &[SkillSummary],
        specs: &[cadmus_contract::ToolSpec],
    ) -> Self {
        let mut text = String::from(system_prompt);
        if !instructions.is_empty() {
            text.push_str(
                "\n\n# Workspace instructions\n\n\
                 Instruction files applying to this workspace, in precedence order: \
                 a file nearer the edited path wins on conflict, and the user's explicit \
                 prompt wins over everything.\n",
            );
            for file in instructions {
                text.push_str("\n## ");
                text.push_str(&file.path);
                text.push_str("\n\n");
                text.push_str(file.content.trim());
                text.push('\n');
            }
        }
        if !skills.is_empty() {
            // Section separator discipline: exactly one blank line whether
            // the predecessor is the instructions block (trailing newline)
            // or the bare prompt (none).
            text.truncate(text.trim_end().len());
            text.push_str(
                "\n\n# Skills\n\n\
                 Skills available to this run, as name: description pairs — a skill's \
                 description says when it applies. When the task matches one, activate \
                 that skill before starting the work; its full instructions then join \
                 the conversation.\n\n",
            );
            for skill in skills {
                text.push_str("- ");
                text.push_str(&skill.name);
                text.push_str(": ");
                text.push_str(&skill.description);
                text.push('\n');
            }
        }
        let hash = prefix_hash(&text, specs);
        Self {
            text,
            hash,
            instructions: instructions.to_vec(),
            skills: skills.to_vec(),
        }
    }

    /// The system message prepended to every request render.
    #[must_use]
    pub fn message(&self) -> Message {
        Message::system(self.text.clone())
    }

    /// The comparability key recorded as [`cadmus_contract::attrs::PREFIX_HASH`].
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// The record embedded in the start-run command, keeping the trace
    /// self-sufficient (ADR-0005).
    #[must_use]
    pub fn record(&self) -> PrefixRecord {
        PrefixRecord {
            hash: self.hash.clone(),
            system: self.text.clone(),
            instructions: self.instructions.clone(),
            skills: self.skills.clone(),
        }
    }

    /// The assembled system text's byte length — the usage heuristic's
    /// prefix term, without cloning the string.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.text.len()
    }
}

/// FNV-1a 64 over the system text plus the serialized tool specs. Hand-rolled
/// under the zero-new-dependency policy (the same constants as the eval
/// corpus digest): this is a change-detection key, not a cryptographic hash,
/// and it must stay stable across toolchains — `DefaultHasher` guarantees
/// neither.
fn prefix_hash(text: &str, specs: &[ToolSpec]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(text.as_bytes());
    for spec in specs {
        feed(&serde_json::to_vec(spec).expect("ToolSpec serializes"));
    }
    format!("{hash:016x}")
}

/// Git facts in the trailer — bounded scalars only (ADR-0007's 2026-09-10
/// amendment): a file list explodes on large repos; enumeration stays one
/// tool call away when the model wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    pub dirty_count: usize,
}

/// The per-turn freshness probe (ADR-0002's injected-IO rule): the wiring
/// layer implements it over `git status`; tests inject fixed values. Called
/// once per request render, at the turn boundary.
pub trait StatusProbe: Send + Sync {
    /// `None` = not a git work tree (or git unavailable): the trailer omits
    /// the git line.
    fn snapshot(&self) -> Option<GitStatus>;
}

/// Watches tool calls for newly-entered subtrees carrying their own
/// instruction file (ADR-0007 item 1(a)'s nested on-demand injection). The
/// wiring layer tracks the filesystem; the loop appends each returned file
/// as a standalone user message and records an `instruction_injected` event.
pub trait InstructionTracker: Send + Sync {
    fn on_calls(&self, calls: &[cadmus_contract::ToolCall]) -> Vec<InstructionFile>;
}

/// The no-op tracker for hermetic contexts (eval) and tests.
pub struct NoInstructions;

impl InstructionTracker for NoInstructions {
    fn on_calls(&self, _calls: &[cadmus_contract::ToolCall]) -> Vec<InstructionFile> {
        Vec::new()
    }
}

/// The no-git probe for hermetic contexts (eval): a scratch workspace
/// sitting inside an enclosing work tree must never leak the operator's
/// git state into scores.
pub struct NoProbe;

impl StatusProbe for NoProbe {
    fn snapshot(&self) -> Option<GitStatus> {
        None
    }
}

/// The user-message text of one nested instruction injection — one pure
/// function so the live history and the replayed fold produce identical
/// bytes (ADR-0005's fold invariant).
#[must_use]
pub fn format_injected(file: &InstructionFile) -> String {
    format!(
        "Entered a new subtree; its instruction file now applies (nearest-file precedence):\n\n## {}\n\n{}",
        file.path,
        file.content.trim()
    )
}

/// Epoch millis → `YYYY-MM-DDTHH:MM:SSZ`. Hand-rolled civil math under the
/// zero-new-dependency policy (std has no calendar): Howard Hinnant's
/// civil-from-days algorithm, UTC only — std exposes no local timezone, and
/// an unambiguous `Z` beats a wrong local guess. `z` is always positive
/// (u64 epoch days + the era offset), so Hinnant's negative-era adjustment
/// is deliberately omitted.
fn civil_utc(epoch_ms: u64) -> String {
    let ms = i64::try_from(epoch_ms).unwrap_or(i64::MAX);
    let days = ms.div_euclid(86_400_000);
    let secs = ms.rem_euclid(86_400_000) / 1000;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    let (hh, mm, ss) = (secs / 3600, secs / 60 % 60, secs % 60);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Run-elapsed render, bounded to the largest two units (`17s`, `42m17s`,
/// `3h02m`) — two units keep the line short regardless of run length.
fn format_elapsed(delta_ms: u64) -> String {
    let secs = delta_ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, secs / 60 % 60)
    }
}

/// The per-request trailer's inputs: the run-static cwd, the probed git
/// state, the injected clock's reading, and the loop-folded tool counters
/// and todo list.
pub struct TrailerView<'a> {
    pub cwd: &'a str,
    pub git: Option<GitStatus>,
    /// Wall-clock now and the run's recorded start (the `StartRun` event's
    /// timestamp), epoch millis — minted from the injected clock, never
    /// read directly (ADR-0002's time seam). Rendered bytes are recorded on
    /// the per-turn `LlmRequest` event, so replay never re-derives them.
    pub now_ms: u64,
    pub run_start_ms: u64,
    pub tool_counts: &'a BTreeMap<String, usize>,
    pub todos: &'a [TodoItem],
}

/// Renders the status trailer appended to every request at render time —
/// never entering the message history (ADR-0007's 2026-09-10 amendment).
/// Deterministic: same view, same bytes (the `LlmRequest` event records the
/// result, so replay audits it exactly).
#[must_use]
pub fn render_trailer(view: &TrailerView) -> String {
    let mut out = String::from("[cadmus status]\n");
    out.push_str("cwd: ");
    out.push_str(view.cwd);
    out.push('\n');
    if let Some(git) = &view.git {
        out.push_str("git: ");
        out.push_str(&git.branch);
        if git.dirty_count == 0 {
            out.push_str(", clean\n");
        } else {
            out.push_str(", dirty(");
            out.push_str(&git.dirty_count.to_string());
            out.push_str(")\n");
        }
    }
    out.push_str("time: ");
    out.push_str(&civil_utc(view.now_ms));
    out.push_str(" (run elapsed ");
    out.push_str(&format_elapsed(
        view.now_ms.saturating_sub(view.run_start_ms),
    ));
    out.push_str(")\n");
    if !view.tool_counts.is_empty() {
        out.push_str("tools: ");
        let mut first = true;
        for (name, count) in view.tool_counts {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(name);
            out.push_str(": ");
            out.push_str(&count.to_string());
        }
        out.push('\n');
    }
    if !view.todos.is_empty() {
        out.push_str("todo:\n");
        for item in view.todos {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[>]",
                TodoStatus::Completed => "[x]",
            };
            out.push_str("  ");
            out.push_str(marker);
            out.push(' ');
            out.push_str(item.content.trim());
            out.push('\n');
        }
    }
    out
}

// ---- The fold machinery (ADR-0007 item 2 + the 2026-09-10 amendment) ----

/// The fold's tunables (the amendment: validate against trace evidence).
/// Production uses `Default`; tests shrink the numbers to fold early.
#[derive(Debug, Clone)]
pub struct FoldPolicy {
    /// Tool results from the last N completed turns stay verbatim — the
    /// model is still actively working with them.
    pub recent_turns: usize,
    /// Nothing smaller is worth folding: a folded result must always
    /// shrink, and the placeholder itself runs ~1.2 KB with the marker.
    pub min_bytes: usize,
    /// The growth cadence cap: Δ = `min(growth_max_tokens, max_context/10)`.
    pub growth_max_tokens: u64,
    /// The ceiling rule's hard line, as a percentage of the window — the
    /// overflow line, never an attention optimum. Fold first there; the
    /// (phase-2) compactor answers "nothing foldable or still over".
    pub ceiling_percent: u64,
}

impl Default for FoldPolicy {
    fn default() -> Self {
        Self {
            recent_turns: FOLD_RECENT_TURNS,
            min_bytes: FOLD_MIN_BYTES,
            growth_max_tokens: FOLD_GROWTH_MAX_TOKENS,
            ceiling_percent: FOLD_CEILING_PERCENT,
        }
    }
}

/// A fold fires every Δ estimated tokens of growth; Δ caps at 100k and
/// scales with the window. Tunable, per the amendment: validate against
/// trace evidence (the cache-invalidation trade).
pub const FOLD_GROWTH_MAX_TOKENS: u64 = 100_000;

/// The ceiling rule's default hard line: 80% of the window.
pub const FOLD_CEILING_PERCENT: u64 = 80;

/// The recency scope: tool results from the last X completed turns stay
/// verbatim — the model is still actively working with them.
pub const FOLD_RECENT_TURNS: usize = 5;

/// The size floor: a folded result must always shrink. With 512-byte
/// excerpts and the marker, the placeholder lands near 1.2 KB, so nothing
/// under 2 KB is worth folding.
pub const FOLD_MIN_BYTES: usize = 2048;

/// The head/tail excerpt kept visible in a folded placeholder (the ADR's
/// head+tail truncation standard).
pub const FOLD_EXCERPT_BYTES: usize = 512;

// The placeholder must always shrink: floor > head + tail + marker.
const _: () = assert!(FOLD_MIN_BYTES > 2 * FOLD_EXCERPT_BYTES);

/// The placeholder text standing in for a folded tool result. Explicit
/// (never silent, ADR-0007 item 2): the fold marker names the folded size
/// and where the full text lives — the spill artifact (trace store, outside
/// the workspace, for audit) and the safe re-obtaining path for the model
/// (re-read the source narrower; never re-run a side-effecting command).
#[must_use]
pub fn fold_placeholder_text(original: &str, folded: &FoldedRef) -> String {
    let head = excerpt_boundary(original, FOLD_EXCERPT_BYTES, true);
    let tail = excerpt_boundary(original, FOLD_EXCERPT_BYTES, false);
    format!(
        "{head}\n\n[COMPRESSED: the middle of this tool result was folded for context budget \
         — {} bytes in total. The full text is archived in spill artifact {} (trace store, \
         outside the workspace). To re-obtain it, re-read the original source with a narrower \
         window or pattern — never re-run a side-effecting command just to regenerate output.]\n\n{tail}",
        folded.original_bytes, folded.spill
    )
}

/// The substituted message for one folded tool result: role, call id and
/// the `is_error` flag carry over verbatim; only the text is replaced.
#[must_use]
pub fn fold_placeholder_message(original: &Message, folded: &FoldedRef) -> Message {
    let text = message_text(original);
    let mut placeholder = Message::tool_result(
        original
            .tool_call_id
            .clone()
            .expect("a folded message is a tool result"),
        serde_json::Value::String(fold_placeholder_text(&text, folded)),
    );
    placeholder.is_error = original.is_error;
    placeholder
}

/// The message's text body (its text parts concatenated) — the spill
/// content and the placeholder's excerpt source.
#[must_use]
pub fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            cadmus_contract::ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A UTF-8-boundary-safe head or tail excerpt of at most `bytes` bytes.
fn excerpt_boundary(text: &str, bytes: usize, head: bool) -> &str {
    if text.len() <= bytes {
        return text;
    }
    if head {
        let mut end = bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        let mut start = text.len() - bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        &text[start..]
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placeholder_render_is_snapshot_locked() {
        let original = format!("{}middle{}", "h".repeat(600), "t".repeat(600));
        let folded = FoldedRef {
            event_id: "e12".into(),
            call_id: "c7".into(),
            spill: "2026/09/10/tr-x.artifacts/m4.txt".into(),
            original_bytes: original.len() as u64,
        };
        let message = Message::tool_result("c7", json!(original));
        let placeholder = fold_placeholder_message(&message, &folded);
        insta::assert_snapshot!(message_text(&placeholder), @"
        hhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhhh

        [COMPRESSED: the middle of this tool result was folded for context budget — 1206 bytes in total. The full text is archived in spill artifact 2026/09/10/tr-x.artifacts/m4.txt (trace store, outside the workspace). To re-obtain it, re-read the original source with a narrower window or pattern — never re-run a side-effecting command just to regenerate output.]

        tttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttttt
        ");
    }

    #[test]
    fn excerpt_respects_char_boundaries() {
        // 3-byte characters: a naive byte cut would split one.
        let text = "€".repeat(400);
        let head = excerpt_boundary(&text, 10, true);
        assert_eq!(head.len(), 9);
        let tail = excerpt_boundary(&text, 10, false);
        assert_eq!(tail.len(), 9);
    }

    #[test]
    fn fold_policy_defaults_match_the_amendment() {
        let policy = FoldPolicy::default();
        assert_eq!(policy.recent_turns, 5);
        assert_eq!(policy.min_bytes, FOLD_MIN_BYTES);
        assert_eq!(policy.growth_max_tokens, 100_000);
        assert_eq!(policy.ceiling_percent, 80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file(path: &str, content: &str) -> InstructionFile {
        InstructionFile {
            path: path.into(),
            content: content.into(),
        }
    }

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            parameters: json!({"type": "object"}),
        }
    }

    fn skill(name: &str, description: &str) -> SkillSummary {
        SkillSummary {
            name: name.into(),
            description: description.into(),
        }
    }

    #[test]
    fn prefix_without_instructions_or_skills_is_the_prompt_alone() {
        let prefix = FrozenPrefix::assemble("PROMPT", &[], &[], &[spec("read_file")]);
        let Message { role, content, .. } = prefix.message();
        assert_eq!(role, cadmus_contract::Role::System);
        assert!(
            matches!(&content[..], [cadmus_contract::ContentPart::Text { text }] if text == "PROMPT")
        );
    }

    #[test]
    fn prefix_assembly_is_snapshot_locked() {
        let prefix = FrozenPrefix::assemble(
            SYSTEM_PROMPT,
            &[
                file("/home/u/.config/cadmus/AGENTS.md", "global rules\n"),
                file("/repo/AGENTS.md", "project rules"),
            ],
            &[
                skill("pr-preflight", "review a PR before opening it"),
                skill("self-review", "review your own diff before committing"),
            ],
            &[spec("read_file"), spec("edit_file"), spec("skill")],
        );
        // The hash is deterministic for fixed inputs — pinned verbatim, so
        // any prompt, chain or tool-schema change fails two readable diffs
        // (this one and the text below) for hand review.
        insta::assert_snapshot!(prefix.hash(), @"e919c6682d3d425c");
        insta::assert_snapshot!(prefix.record().system, @"
        You are Cadmus, a coding agent working in a terminal workspace.

        Working discipline:
        - Verify before you claim done: run the build, tests or checks that prove the change works; never report success on assumption.
        - Tool errors are corrections, not failures: read the feedback, adjust, and retry with a better approach.
        - Read before you write: understand the existing code and its conventions before changing them; match the style you find.
        - Keep changes minimal and scoped to the task; do not refactor what is not broken.
        - Never abandon a broken intermediate state: finish the change or revert it, so the workspace is never left worse than you found it.
        - When a task is unclear or has multiple valid interpretations, ask instead of guessing.

        A status block maintained by code (never by you) ends every request: cwd, git state, the clock, tool counters and the task list. Trust it over your own recollection of these facts.

        # Workspace instructions

        Instruction files applying to this workspace, in precedence order: a file nearer the edited path wins on conflict, and the user's explicit prompt wins over everything.

        ## /home/u/.config/cadmus/AGENTS.md

        global rules

        ## /repo/AGENTS.md

        project rules

        # Skills

        Skills available to this run, as name: description pairs — a skill's description says when it applies. When the task matches one, activate that skill before starting the work; its full instructions then join the conversation.

        - pr-preflight: review a PR before opening it
        - self-review: review your own diff before committing
        ");
        insta::assert_debug_snapshot!(prefix.record().instructions, @r#"
        [
            InstructionFile {
                path: "/home/u/.config/cadmus/AGENTS.md",
                content: "global rules\n",
            },
            InstructionFile {
                path: "/repo/AGENTS.md",
                content: "project rules",
            },
        ]
        "#);
        insta::assert_debug_snapshot!(prefix.record().skills, @r#"
        [
            SkillSummary {
                name: "pr-preflight",
                description: "review a PR before opening it",
            },
            SkillSummary {
                name: "self-review",
                description: "review your own diff before committing",
            },
        ]
        "#);
    }

    #[test]
    fn each_section_renders_without_the_other() {
        // The two optional sections are independent: instructions-only (the
        // common production shape — a workspace with AGENTS.md and no
        // skills) and skills-only both pin their exact bytes, including the
        // one-blank-line boundary each way.
        let instructions_only = FrozenPrefix::assemble(
            "PROMPT",
            &[file("/repo/AGENTS.md", "project rules")],
            &[],
            &[spec("read_file")],
        );
        insta::assert_snapshot!(instructions_only.record().system, @"
        PROMPT

        # Workspace instructions

        Instruction files applying to this workspace, in precedence order: a file nearer the edited path wins on conflict, and the user's explicit prompt wins over everything.

        ## /repo/AGENTS.md

        project rules
        ");

        let skills_only = FrozenPrefix::assemble(
            "PROMPT",
            &[],
            &[skill("pr-preflight", "review a PR before opening it")],
            &[],
        );
        insta::assert_snapshot!(skills_only.record().system, @"
        PROMPT

        # Skills

        Skills available to this run, as name: description pairs — a skill's description says when it applies. When the task matches one, activate that skill before starting the work; its full instructions then join the conversation.

        - pr-preflight: review a PR before opening it
        ");
    }

    #[test]
    fn hash_is_sensitive_to_every_prefix_input() {
        let base = FrozenPrefix::assemble("P", &[file("/a", "x")], &[], &[spec("t")]);
        let prompt_changed = FrozenPrefix::assemble("Q", &[file("/a", "x")], &[], &[spec("t")]);
        let file_changed = FrozenPrefix::assemble("P", &[file("/a", "y")], &[], &[spec("t")]);
        let skills_changed =
            FrozenPrefix::assemble("P", &[file("/a", "x")], &[skill("s", "d")], &[spec("t")]);
        let tool_changed = FrozenPrefix::assemble("P", &[file("/a", "x")], &[], &[spec("u")]);
        assert_ne!(base.hash(), prompt_changed.hash());
        assert_ne!(base.hash(), file_changed.hash());
        assert_ne!(base.hash(), skills_changed.hash());
        assert_ne!(base.hash(), tool_changed.hash());
        // … and stable for identical inputs (the comparability contract).
        assert_eq!(
            base.hash(),
            FrozenPrefix::assemble("P", &[file("/a", "x")], &[], &[spec("t")]).hash()
        );
    }

    #[test]
    fn civil_utc_matches_known_dates() {
        assert_eq!(civil_utc(0), "1970-01-01T00:00:00Z");
        // The leap rules both ways: 2024 divisible by 4, 2000 divisible by
        // 400 (IS leap — pins the doe/146096 correction), 2100 divisible by
        // 100 but not 400 (NOT leap).
        assert_eq!(civil_utc(1_709_210_096_000), "2024-02-29T12:34:56Z");
        assert_eq!(civil_utc(951_782_400_000), "2000-02-29T00:00:00Z");
        assert_eq!(civil_utc(4_107_542_400_000), "2100-03-01T00:00:00Z");
        assert_eq!(civil_utc(1_788_393_600_000), "2026-09-03T00:00:00Z");
    }

    #[test]
    fn elapsed_render_is_bounded_to_two_units() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(999), "0s");
        assert_eq!(format_elapsed(17_000), "17s");
        // The unit boundaries: 59.999s stays seconds, 60s rolls to minutes.
        assert_eq!(format_elapsed(59_999), "59s");
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(2_537_000), "42m17s");
        assert_eq!(format_elapsed(3_599_000), "59m59s");
        assert_eq!(format_elapsed(3_600_000), "1h00m");
        assert_eq!(format_elapsed(3_600_000 + 120_000 + 5_000), "1h02m");
        assert_eq!(format_elapsed(25 * 3_600_000), "25h00m");
    }

    #[test]
    fn a_clock_reading_before_the_run_start_clamps_to_zero() {
        // Clock skew (now < run start) must never produce a negative or
        // garbage elapsed — the render saturates at zero.
        let out = render_trailer(&TrailerView {
            cwd: "/repo",
            git: None,
            now_ms: 1_000,
            run_start_ms: 2_000,
            tool_counts: &BTreeMap::new(),
            todos: &[],
        });
        assert!(out.contains("run elapsed 0s"), "{out}");
    }

    #[test]
    fn trailer_render_is_snapshot_locked() {
        let mut counts = BTreeMap::new();
        counts.insert("edit_file".into(), 1);
        counts.insert("read_file".into(), 3);
        let todos = vec![
            TodoItem {
                content: "done thing".into(),
                status: TodoStatus::Completed,
                active_form: None,
            },
            TodoItem {
                content: "doing thing".into(),
                status: TodoStatus::InProgress,
                active_form: Some("doing the thing".into()),
            },
            TodoItem {
                content: "pending thing".into(),
                status: TodoStatus::Pending,
                active_form: None,
            },
        ];
        // Fixed clock readings: run start 2026-09-03T00:00:00Z, now +42m17s.
        let (run_start_ms, now_ms) = (1_788_393_600_000, 1_788_396_137_000);
        let full = render_trailer(&TrailerView {
            cwd: "/repo",
            git: Some(GitStatus {
                branch: "main".into(),
                dirty_count: 2,
            }),
            now_ms,
            run_start_ms,
            tool_counts: &counts,
            todos: &todos,
        });
        insta::assert_snapshot!(full, @"
        [cadmus status]
        cwd: /repo
        git: main, dirty(2)
        time: 2026-09-03T00:42:17Z (run elapsed 42m17s)
        tools: edit_file: 1, read_file: 3
        todo:
          [x] done thing
          [>] doing thing
          [ ] pending thing
        ");

        let minimal = render_trailer(&TrailerView {
            cwd: "/repo",
            git: None,
            now_ms,
            run_start_ms,
            tool_counts: &BTreeMap::new(),
            todos: &[],
        });
        insta::assert_snapshot!(minimal, @"
        [cadmus status]
        cwd: /repo
        time: 2026-09-03T00:42:17Z (run elapsed 42m17s)
        ");

        let clean = render_trailer(&TrailerView {
            cwd: "/repo",
            git: Some(GitStatus {
                branch: "main".into(),
                dirty_count: 0,
            }),
            now_ms,
            run_start_ms,
            tool_counts: &BTreeMap::new(),
            todos: &[],
        });
        insta::assert_snapshot!(clean, @"
        [cadmus status]
        cwd: /repo
        git: main, clean
        time: 2026-09-03T00:42:17Z (run elapsed 42m17s)
        ");
    }

    #[test]
    fn injected_message_text_is_stable() {
        let text = format_injected(&file("/repo/crates/x/AGENTS.md", "crate rules\n"));
        insta::assert_snapshot!(text, @"
        Entered a new subtree; its instruction file now applies (nearest-file precedence):

        ## /repo/crates/x/AGENTS.md

        crate rules
        ");
    }
}
