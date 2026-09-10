//! The runtime context pipeline (ADR-0007): three-segment request assembly —
//! the frozen prefix, the conversation history, and the per-request status
//! trailer. Pure logic: instruction files arrive as values, git freshness
//! through the injected [`StatusProbe`], nested-file discovery through the
//! injected [`InstructionTracker`] — so every byte the model sees is
//! reproducible from the trajectory alone.

use std::collections::BTreeMap;

use cadmus_contract::{InstructionFile, Message, PrefixRecord, TodoItem, TodoStatus, ToolSpec};

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

A status block maintained by code (never by you) ends every request: cwd, git state, tool counters and the task list. Trust it over your own recollection of these facts.";

/// `todo_write`'s wire name, shared by the loop (which folds its calls into
/// the trailer state) and the wiring layer's tool definition — one SSOT for
/// the one tool the loop knows by name (ADR-0007 item 1(c)'s exception).
pub const TODO_WRITE: &str = "todo_write";

/// The frozen prefix (ADR-0007 item 1(a)): assembled once per run from the
/// system prompt, the instruction chain and the tool specs, then byte-stable
/// for the whole run — the prompt-cache boundary. The hash is the
/// comparability key: runs with different hashes are not eval-comparable
/// (ADR-0010).
pub struct FrozenPrefix {
    text: String,
    hash: String,
    instructions: Vec<InstructionFile>,
}

impl FrozenPrefix {
    /// Assembles the prefix and its hash. `specs` enter the hash (a tool
    /// schema change is a prefix change) but not the system text — on the
    /// wire they travel in the request's `tools` field, in wire order.
    #[must_use]
    pub fn assemble(
        system_prompt: &str,
        instructions: &[InstructionFile],
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
        let hash = prefix_hash(&text, specs);
        Self {
            text,
            hash,
            instructions: instructions.to_vec(),
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
        }
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

/// The per-request trailer's inputs: the run-static cwd, the probed git
/// state, and the loop-folded tool counters and todo list.
pub struct TrailerView<'a> {
    pub cwd: &'a str,
    pub git: Option<GitStatus>,
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

    #[test]
    fn prefix_without_instructions_is_the_prompt_alone() {
        let prefix = FrozenPrefix::assemble("PROMPT", &[], &[spec("read_file")]);
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
            &[spec("read_file"), spec("edit_file")],
        );
        // The hash is deterministic for fixed inputs — pinned verbatim, so
        // any prompt, chain or tool-schema change fails two readable diffs
        // (this one and the text below) for hand review.
        insta::assert_snapshot!(prefix.hash(), @"958ef9aebc18938b");
        insta::assert_snapshot!(prefix.record().system, @"
        You are Cadmus, a coding agent working in a terminal workspace.

        Working discipline:
        - Verify before you claim done: run the build, tests or checks that prove the change works; never report success on assumption.
        - Tool errors are corrections, not failures: read the feedback, adjust, and retry with a better approach.
        - Read before you write: understand the existing code and its conventions before changing them; match the style you find.
        - Keep changes minimal and scoped to the task; do not refactor what is not broken.
        - Never abandon a broken intermediate state: finish the change or revert it, so the workspace is never left worse than you found it.
        - When a task is unclear or has multiple valid interpretations, ask instead of guessing.

        A status block maintained by code (never by you) ends every request: cwd, git state, tool counters and the task list. Trust it over your own recollection of these facts.

        # Workspace instructions

        Instruction files applying to this workspace, in precedence order: a file nearer the edited path wins on conflict, and the user's explicit prompt wins over everything.

        ## /home/u/.config/cadmus/AGENTS.md

        global rules

        ## /repo/AGENTS.md

        project rules
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
    }

    #[test]
    fn hash_is_sensitive_to_every_prefix_input() {
        let base = FrozenPrefix::assemble("P", &[file("/a", "x")], &[spec("t")]);
        let prompt_changed = FrozenPrefix::assemble("Q", &[file("/a", "x")], &[spec("t")]);
        let file_changed = FrozenPrefix::assemble("P", &[file("/a", "y")], &[spec("t")]);
        let tool_changed = FrozenPrefix::assemble("P", &[file("/a", "x")], &[spec("u")]);
        assert_ne!(base.hash(), prompt_changed.hash());
        assert_ne!(base.hash(), file_changed.hash());
        assert_ne!(base.hash(), tool_changed.hash());
        // … and stable for identical inputs (the comparability contract).
        assert_eq!(
            base.hash(),
            FrozenPrefix::assemble("P", &[file("/a", "x")], &[spec("t")]).hash()
        );
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
        let full = render_trailer(&TrailerView {
            cwd: "/repo",
            git: Some(GitStatus {
                branch: "main".into(),
                dirty_count: 2,
            }),
            tool_counts: &counts,
            todos: &todos,
        });
        insta::assert_snapshot!(full, @"
        [cadmus status]
        cwd: /repo
        git: main, dirty(2)
        tools: edit_file: 1, read_file: 3
        todo:
          [x] done thing
          [>] doing thing
          [ ] pending thing
        ");

        let minimal = render_trailer(&TrailerView {
            cwd: "/repo",
            git: None,
            tool_counts: &BTreeMap::new(),
            todos: &[],
        });
        insta::assert_snapshot!(minimal, @"
        [cadmus status]
        cwd: /repo
        ");

        let clean = render_trailer(&TrailerView {
            cwd: "/repo",
            git: Some(GitStatus {
                branch: "main".into(),
                dirty_count: 0,
            }),
            tool_counts: &BTreeMap::new(),
            todos: &[],
        });
        insta::assert_snapshot!(clean, @"
        [cadmus status]
        cwd: /repo
        git: main, clean
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
