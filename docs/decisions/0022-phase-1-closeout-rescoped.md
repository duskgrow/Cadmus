# 0022. Phase-1 closeout re-scoped: land resume/fork/rewind, demote TUI chrome

- Status: accepted
- Date: 2026-09-23

## Context

Maintainer direction (2026-09-23): the GUI becomes the primary
interactive surface (ADR-0019); the TUI is demoted to reference
renderer and on-box console, with investment stopping at the committed
floor. That direction lands mid-phase-1, whose capability row promised
more TUI surface than the new direction wants.

Phase-1 state at the pivot. Landed: streaming render with paced
emission and band geometry; design-slot wiring; interrupt and the
steer pair; the approval stack (per-call dialog, rule engine, deny
timeout); status line; slash commands (in flight); headless
`chat --json`; the JSONL store, replayer, eval set v1 and arch test.
Unlanded from the roadmap's phase-1 interaction row: checkpoint/rewind
and resume/fork; from ADR-0015 item 8: the TUI session list with the
state machine and blocked-first ordering; from ADR-0017: the palette
generator, the full preset set and the icon registry.

The unlanded set splits cleanly by where its value lives under the new
direction. Resume/fork and checkpoint/rewind are core + contract
capabilities — every frontend consumes them through the same protocol,
and `serve`'s drain-and-resume acceptance (ADR-0016 item 4, pulled
forward by ADR-0021) cannot pass without resume. The session dashboard
and the design-system remainder are TUI chrome whose audience just
moved to the GUI.

## Decision

1. **The closeout set is resume/fork and checkpoint/rewind.** They
   land as core + contract capabilities with a minimal TUI surface —
   frontend-agnostic by construction (ADR-0013), so the foreign GUI
   inherits them over the wire, not by re-implementation. Rewind exits
   ADR-0013's contract-future list. Phase 1 closes when these land and
   `just ci` is green.
2. **The TUI session dashboard leaves phase 1.** The state machine and
   blocked-first ordering are the GUI's core job — ADR-0015 item 8
   already assigns the full orchestration surface there. The TUI keeps
   only the minimal attach affordance its console role needs (open a
   trace, read the foreign view); fleet-grade session management is
   never built for the terminal.
3. **The design-system remainder is descoped per ADR-0023:** no
   palette generator, no presets beyond the terminal-native set, no
   icon registry.
4. **The roadmap syncs in this PR.** The capability track's phase-1
   interaction cell is rewritten to the closeout scope; nothing else
   moves phases.

## Consequences

- The deviations-as-ADRs rule is honored: the roadmap's phase-1
  promise is narrowed here rather than silently in the tracker.
- Every capability that exits phase 1 exits to a named owner — the GUI
  repo's surface (item 2) or ADR-0023's descope (item 3) — none drops
  into a void.
- The closeout set is the last TUI feature work of phase 1; per the
  demotion, post-closeout TUI changes are protocol-tracking and
  maintenance, not new surfaces.
