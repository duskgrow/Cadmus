# 0011. Mainstream-parity interaction as a hard requirement: TUI, approval modes, checkpoints, session UX

- Status: accepted
- Date: 2026-09-06

## Context

Maintainer directive, 2026-09-06: Cadmus is a self-evolving agent, but it is
_first_ an agent — usability, interaction quality and feature completeness
must match mainstream coding agents, otherwise it will not be used daily;
and a self-evolving agent that is not used collects no trajectories,
starving its own evolution loop. Usability is therefore not polish applied
later but a hard requirement that shapes phase content. This ADR is the
product-surface counterpart of the 2026-09 design review (which found the
docs plan evolution assets, not agent capabilities).

Current state: a one-shot, print-only CLI (final turn only). No TUI, no
incremental rendering, no interrupt/steering, no approval UX, no
checkpoints, no session picker, no plan mode. ADR-0002's event-sourced core
was designed exactly for this gap: every client operation is a command on
the log, and the local CLI and future remote clients are the same kind of
client.

Baseline survey (2026-09-06, official product docs; re-verified at each
phase kickoff per the report's freshness policy §1.2.2): Claude Code v2.1.x,
Codex CLI, Gemini CLI, Aider. Their convergent interaction floor is
distilled in item 3; the products' own docs — not this ADR — remain the
SSOT for details.

## Decision

1. **Parity is an acceptance criterion.** Each phase's "usable system"
   (report §10.1.1) includes the interaction floor for the capabilities that
   phase adds: a capability is not landed until its UX is landed. The floor
   checklist is re-baselined at every phase kickoff — parity is a
   relationship to a moving target.
2. **Two frontends, one core.** Interactive TUI (primary) and headless
   print mode (today's `chat`, the scripting surface), both clients of the
   event-sourced core (ADR-0002): steering, approvals and interrupts are
   command events, so interactivity adds no new core concept, and phase 5's
   remote attach inherits the full UX as a transport swap. The TUI crate is
   wiring; its framework and rendering dependencies (ratatui is the
   Codex-CLI-proven candidate, not pre-decided) go through the
   `adding-dependencies` skill at implementation time.
3. **Interaction floor v1** (baseline 2026-09-06):
   - Streaming incremental markdown rendering with syntax highlighting;
     edits shown as diffs (inside the approval prompt and in a `/diff`
     view); tool activity collapsed by default with an expandable
     transcript view.
   - Interrupt that preserves completed work (Esc); two steering
     granularities — queue for next turn vs inject into the current turn
     (Codex's Tab/Enter split); edit-any-history-message-and-fork (Esc×2).
   - Approval modes mapping ADR-0008's L0–L3 tiers: read-only /
     approve-writes (default) / auto-edit+approve-shell / plan; per-tool
     allow/ask/deny rules persisted to config; rejection carries an
     optional comment back into the trajectory (the user's judgment becomes
     learning signal); interactive prompts wait for the user, unattended
     runs default to deny (ADR-0008).
   - Checkpoint/rewind: files touched by agent write tools are snapshotted
     per user prompt; snapshots live in a shadow store under the data dir —
     checkpointing never mutates the user's git state (an agent must not run
     mutating git commands on a possibly-dirty user tree). Rewind offers
     conversation-only / code-only / both (Gemini CLI's three-way split);
     retention bounded (order of magnitude:
     ~100 checkpoints, ~30 days).
   - Session UX: resume picker (search, rename, filter), fork, explicit
     prompt when cwd differs from the session's recorded cwd.
   - Plan mode: read-only exploration plus a plan file; approving the plan
     exits into a chosen approval mode.
   - Status line (model, cwd+git, context-usage %, session cost) and a
     `/usage` cost view — cost visibility is trust, and the numbers come
     from the event log, so they are exact and replayable.
   - Slash commands expanded client-side (no model round-trip, no log
     noise); `!` shell pass-through with output into context; `@` file
     completion.
   - Notifications on permission-wait / idle: terminal bell or OSC escape,
     default unfocused-only.
   - The input never blocks while the model streams; status updates are
     debounced; the UI never waits on an LLM call.
4. **The differentiator must be visible.** Evolution is UX too: skill and
   memory deltas arrive as reviewable diffs in the TUI
   (approve / reject / comment — reusing the approval machinery of item 3),
   gate results and skill counters are inspectable, and all evolution work
   runs in the background, never blocking the interactive loop.
5. **Configuration surface:** layered settings (user config dir → project
   → flags), hot-reloaded where cheap; keybindings and theme configurable.
   Kept deliberately small — every knob needs a real user.
6. **Hooks are deferred with a trigger** (first real extension need). The
   event stream makes hook points natural subscribers, but hooks that call
   models on error paths are a proven death-spiral source (ai-agent-book
   ch5); adoption starts deterministic-only, with recursion limits.

## Consequences

- The "daily driver" milestone — write tools + TUI + streaming + approval
  modes + checkpoints + resume — is one deliverable line (with
  ADR-0008/0009) and lands _before_ phase 2: the evolution loop's review UX
  reuses this approval/diff machinery, and only daily use generates the
  trajectories phase 2 learns from. Scheduling honesty: this grows
  phase 1 well beyond the report §10.2.2 estimate (3–4 weeks) — a deliberate
  deviation recorded here, the frozen report untouched.
- Roadmap gains a capability track assigning per-phase ownership (the
  status table keeps its start/complete-only rule).
- Eval scenarios stay product-level: a scenario's pass criterion is
  user-observable behavior, which this checklist defines.
- Deliberately not pursued (parity traps at single-user scale): plugin or
  marketplace ecosystems; voice input; desktop/mobile/web frontends (the
  phase-5 remote client is the same TUI over a transport); teammate-style
  multi-agent UI; cloud session sync/teleport; IDE extensions; aider-style
  repo-map (the Claude Code/Codex consensus is just-in-time retrieval over
  precomputed maps — and a stale map is worse than none).
- Once the TUI exists, insta snapshot tests lock the rendering of fixed
  event streams, keeping the floor mechanical rather than aspirational.
- Terminal citizenship (CLI discipline), the adopted TUI design philosophy
  (blocks, hint bar, inline rendering, fuzzy+preview pickers), the session
  state machine, and the composability/embedding stance live in ADR-0012.
