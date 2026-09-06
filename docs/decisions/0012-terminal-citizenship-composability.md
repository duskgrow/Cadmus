# 0012. Terminal citizenship and composability: CLI discipline, TUI philosophy, embedding stance

- Status: accepted
- Date: 2026-09-06

## Context

ADR-0011 set mainstream-agent parity as the interaction bar. The
maintainer's follow-up (2026-09-06): cadmus is fundamentally a terminal
application (a GUI is distant and unlikely), so the bar also comes from the
best terminal tools, not only from agents — and we need an explicit stance
on composing with the multiplexer / agent-manager ecosystems the maintainer
pointed at (zellij, herdr), including whether cadmus embeds into them or is
embeddable by them.

Baseline survey (2026-09-06, official docs and repos; details live there,
not here): zellij (modal keybindings with a hint bar rendered from the live
keymap; WASM plugins; a pure-CLI control plane of ~70 actions with JSON
output and NDJSON subscribe streams), herdr v0.8 (agent-aware terminal
workspace runtime: client/server with detach, a blocked/working/done/idle
agent state machine rolled up pane→workspace, "restore via the managed
program's own `--resume`", and an env-seam reporting protocol for custom
agents), Warp (blocks; editor-grade input; denylist always wins), lazygit
(principle arbitration — safety+simplicity win by default; undo built on
git's reflog instead of app-private state), fzf (fuzzy+preview as a
universal picker primitive; exit-is-result pipe semantics), charmbracelet
(Elm architecture; glamour's pure renderer with adaptation at the edges;
huh's accessible line-mode degradation; vhs golden terminal recordings;
crush's unfocused-only notifications), and clig.dev (the CLI-citizenship
canon). One honest gap: no clig.dev-scale canon exists for full-screen TUIs
(verified 2026-09-06 — the field offers example catalogs and framework
manuals, no stewarded standard), so item 2 is this project's own distilled
checklist, grounded in the surveyed applications' convergent conventions
(lazygit/k9s/helix: hjkl/arrow navigation, `q` to quit, `?` for help, `/`
to filter, a footer command bar) and in ratatui's documented technical
constraints.

## Decision

1. **CLI citizenship floor** (binding on print mode and the TUI from their
   first commit; clig.dev, 2026-09 baseline): stdout is primary/machine
   output, stderr is diagnostics; exit codes map to failure modes; per-stream
   TTY detection; `NO_COLOR` / `TERM=dumb` / `--no-color` honored; no
   animation when piped; `--no-input` globally disables interaction and every
   prompt has a flag path; two-stage Ctrl-C (cleanup with timeout, a second
   Ctrl-C skips it; crash-only startup); first output within 100 ms; progress
   never swallows the log lines it replaced; secrets never in flags (they
   leak into `ps`); provider keys keep the standard env/keyring convention
   at the harness boundary but are never injected into sandboxed
   subprocesses (report §7.1.1); config precedence flags > env > project >
   user > system, XDG-respecting; errors written as documentation with the most
   important line last.
2. **TUI philosophy** (adopted, sources above):
   - Blocks: a turn (prompt + streamed response + tool calls) is one atomic
     block — status-colored, foldable, copyable, searchable; blocks are the
     natural boundary for approvals, diffs and rewind (Warp).
   - Discoverability: a mode-sensitive keybinding hint bar rendered from the
     live keymap state — never hardcoded strings, so custom keymaps cannot
     lie (zellij); Esc exits every transient state; disabled actions stay
     visible with their reason (lazygit).
   - Fuzzy finding with preview is the universal picker primitive — session
     picker, `@` completion, command palette — with exit-is-result semantics
     (fzf).
   - The composer is an editor-grade multiline input (selection, undo, word
     operations), with Ctrl-G out to `$EDITOR` for long prompts (Warp's
     lesson: readline-class input is not enough for agent prompts).
   - Inline rendering by default, not alt-screen: conversation output stays
     in scrollback so tmux copy mode, multiplexer scrollback and SSH
     reattach keep working; alt-screen only for true modal sub-apps.
   - Notifications only when the terminal reports unfocused (focus-event
     detection with an OSC/bell/native fallback chain), default on,
     unfocused-only (crush).
   - Undo derives from the workspace's own VCS where that is sound
     (lazygit's reflog lesson); ADR-0011's checkpoint snapshots live outside
     the user's repository and never mutate it.
   - Accessibility: a line-oriented no-TUI mode (`ACCESSIBLE` env or
     `TERM=dumb`), same core.
3. **Session semantics for a multi-task single user.** Session states
   idle / working / blocked / done roll up to the session list (blocked =
   needs a user decision; done persists until seen) — herdr's machine, and
   every transition is derivable from our event stream (run/turn/approval
   events plus timeouts), so the dashboard is a pure client-side view.
   Candidate for the TUI once concurrent sessions exist; phase 5 generalizes
   it across nodes.
4. **Composability stance.**
   - Cadmus embeds in nothing: no multiplexer plugins, no tmux control-mode
     client, no manager-specific UI.
   - The interop API is the headless protocol: `chat --json` NDJSON event
     stream (Codex `exec --json` precedent) from phase 1; a self-describing
     newline-JSON local socket arrives with the phase-5 daemon (zellij and
     herdr both prove CLI-first, socket-later suffices).
   - Being managed is a citizenship protocol, not embedding: when a
     manager's env seam is present (`HERDR_*`, `ZELLIJ_PANE_ID`), cadmus
     reports its semantic state (item 3's machine) through the manager's own
     CLI and exposes native `--resume <trace-id>` — external managers restore
     sessions via our replay-based resume (ADR-0009), never via screen
     scraping. Semantic state and display metadata are reported separately
     (herdr's lesson: nobody else's title pollutes our state machine). The
     reporter lives in the wiring layer; absent env vars mean zero cost and
     zero core branches.
   - A future GUI (distant, unlikely) is another client of the same event
     protocol — no core change (ADR-0002).
5. **Deliberately not adopted:** a WASM plugin runtime (zellij's — hooks
   stay deferred per ADR-0011); accounts or telemetry (clig.dev: no
   phone-home without consent; we are local-first); terminal-emulator
   features (we are an app _in_ the terminal — Warp's compatibility tables
   are the cost list of crossing that line); natural-language mode
   detection (explicit prefixes are predictable); per-feature config sprawl
   (k9s is the counterexample; lazygit's own confession).
6. **Mechanical gates.** Insta snapshots of fixed event streams lock
   rendering (ADR-0011); vhs-style golden terminal recordings
   (`golden.ascii`) join `just snapshot-review` when the TUI lands, keeping
   the floor testable rather than aspirational.

## Consequences

- Item 1 retroactively constrains the existing print-mode CLI (stdout/stderr
  split, exit codes, `--no-input`) — small, one PR.
- The item-3 state machine must stay derivable from events alone; if a state
  ever needs core changes, that is a boundary-leak signal (ADR-0002's audit
  trigger).
- Revisit triggers: an integration request the event protocol cannot serve
  reopens the socket timeline; a second manager ecosystem with a different
  protocol generalizes the reporter seam.
