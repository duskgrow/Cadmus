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

## Amendment — 2026-09-08: N frontends; the GUI is planned

Item 2's "two frontends" becomes N frontends over the client protocol of
ADR-0013 (sync-on-subscribe live stream + idempotent commands): the TUI,
headless print mode, the post-phase-5 GUI and remote attach are one client
kind. The consequences line "deliberately not pursued: desktop/mobile/web
frontends" is narrowed: a desktop GUI is now planned (maintainer,
2026-09-08), Rust-native and possibly GPUI — GPL-licensed, so it lives in a
separate repository importing cadmus as a library and lands after phase 5
as another renderer over the transport. Mobile and web frontends stay
unpursued. The TUI-first strategy and this ADR's interaction floor are
unchanged: the floor remains the TUI's acceptance criterion, and the GUI
inherits parity through the shared protocol rather than a second UX spec.

## Amendment — 2026-09-11: interaction floor re-baselined

First re-baseline of item 3 under item 1's moving-target rule, at the TUI
kickoff. Source survey: `docs/research/2026-09-11-agent-uiux-landscape.md`
§2 (official docs fetched 2026-09-11); where it and the 2026-09-06 baseline
differ, this amendment wins.

1. **Steering gains a third granularity.** Queue-for-next-turn and
   inject-now are joined by inject-at-next-tool-boundary, which is the
   default: immediate yet deterministic — the injection point lands in the
   event stream and replays exactly (ADR-0013's record-on-effect rule).
   Bindings are chosen at implementation, not copied: the field is split
   (Claude Code Enter=queue, Codex Enter=inject).
2. **Mid-turn settings steering.** "Input never blocks" now includes live
   configuration: `/model` / effort-class changes apply to the next request
   within the running turn (Claude Code v2 precedent).
3. **Approval surface extends; modes stay as sugar.** Item 3's per-tool
   allow/ask/deny rules gain matching over tool input (OpenCode globs,
   Codex execpolicy precedents) and grant scope — once / turn / session /
   persisted (Codex `acceptForSession` and `scope: turn|session`
   precedents). The four approval modes remain, as presets over the rule
   layer (OpenCode's presets-as-sugar). The scoped-rules open item keeps
   its consumer (the config-layer implementation); this amendment sets the
   direction.
4. **Rewind becomes a four-action algebra.** The existing restore split
   (conversation-only / code-only / both) and edit-and-fork are joined by
   summarize (from-here / up-to-here — rewind merged with targeted
   compaction) and re-decide (restore re-proposes the pending tool call —
   Gemini CLI precedent). Lands in phases with the checkpoint feature.
   Storage mechanism: a shadow git repository (Gemini precedent) realizes
   the shadow store — diff/log semantics for free; the
   never-mutate-user-git invariant stands.
5. **Status surface splits and becomes composable.** Footer plus terminal
   title (OSC), both user-composable via pick-and-reorder pickers (Codex
   `/statusline` precedent); the model-facing trailer stays core's
   (ADR-0013 item 7). VCS state may extend to PR/MR review status.
6. **Notifications: push + pull.** Unfocused-only push stands (OSC works
   over SSH — crush), joined by a recap-on-refocus pull (Claude Code
   precedent). A state-truthfulness invariant joins the floor: render
   unknown rather than a guessed idle (ccmanager #227 lesson:
   scraping-derived detectors false-idle by design; ours derives from
   events, and the invariant binds every future heuristic, dashboard
   included).
7. **Subagent/background-task visualization joins the floor when the task
   tool lands** (Codex `/ps`, Claude Code `/tasks` precedents); parked with
   the persona-profiles open item.
8. **alt-screen revisited: stance kept, escalation pattern noted.**
   ADR-0012 item 2's inline-default stands. Claude Code v2's fullscreen
   renderer demonstrates the escalation pattern (auto `/diff` side panel
   at ≥144 columns); an opt-in escalation strategy may be evaluated at
   implementation — no philosophy change.

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
