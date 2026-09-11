# 0015. Orchestration layer: single-user fleet projection, attention, review — all derived from the event stream

- Status: accepted
- Date: 2026-09-11

## Context

Maintainer direction (2026-09-11): Cadmus is both agent and orchestrator.
The market splits the two, and the 2026-09-11 research
(`docs/research/2026-09-11-agent-uiux-landscape.md` §3) shows the split is
structurally shallow: orchestrators observe foreign agents through PTY
passthrough, screen scraping, env-var seams, and installed hooks, and six
information kinds never cross the seam (per-event cost, policy-bearing
approvals, checkpoint semantics, cross-session context, attention intent,
rate-limit proximity). The failure evidence is public: ccmanager #227 (a
vendor UI string change false-idles the detector), Superset #7395
(subagent work invisible to rollups), Windsurf's LRU eviction silently
deleting unmerged worktrees, fleet-scale approval fatigue pushing users to
`--dangerously-skip-permissions`. Post-mortems (Vibe Kanban's shutdown,
Crystal→Nimbalyst) show orchestration-only products fail: value accrues
to whoever owns the agent, and scraping-based integration is a treadmill
you lose. Owning both sides turns each of those gaps into a designed
advantage, because ADR-0002's event-sourced core and ADR-0013's client
protocol make "orchestrator view = projection of the agent's own event
stream" true by construction.

Two existing texts bound the direction: the frozen report §2.5.1 cut
multi-session swarm orchestration, and ADR-0012 item 3 made the session
state machine derivable from events while listing the dashboard only as a
candidate. This ADR formalizes the boundary the maintainer's direction
implies.

## Decision

1. **Boundary.** The orchestration layer is the single-user,
   multi-session projection/attention/review surface: fleet dashboard,
   attention routing, review objects, merge gating. Excluded, unchanged:
   cross-node task division (ADR-0002), swarm or multi-agent
   collaboration logic, team and marketplace features (ADR-0011
   consequences). Multi-session concurrency means one user's concurrent
   sessions (ADR-0012 item 3), generalized across nodes by phase 5.
2. **One event stream, four projections.** Session (the TUI
   conversation), orchestration (fleet state), review (diffs, comments,
   merge gate) and evolution (skill/memory deltas, gate results — ADR-0011
   item 4) are all client-side projections of the same event streams
   (ADR-0005, ADR-0013). No orchestration state may be sourced from
   anything but events — ADR-0012 item 3's invariant, restated for the
   layer.
3. **State-truthfulness invariant.** A session or attention state renders
   unknown rather than a guessed idle; on uncertainty, hold the last
   known state (ccmanager #227's own fix direction). Our states derive
   from events, so unknown should be rare; the invariant binds every
   future heuristic, including any the GUI adds.
4. **Attention routing.** blocked-first ordering in every fleet view
   (herdr's field verdict: "blocked was the most useful signal"); push
   notifications unfocused-only, plus recap-on-refocus pull (ADR-0011
   2026-09-11 amendment item 6); done persists until seen (ADR-0012
   item 3).
5. **Fleet policy layer.** The scoped-rules policy decorator (open item;
   ADR-0011 2026-09-11 amendment item 3) generalizes: one auto-resolving
   decorator may serve N sessions — fleet-level approval policy that no
   vendor-split product offers. The safety stance is unchanged: rules are
   comfort for the last mile; architectural enforcement (sandbox +
   allowlist) is the mechanism (report §7.1.1, ADR-0008).
6. **Worktree workflow.** When concurrent sessions touch one repo, each
   works in its own git worktree (the market's converged isolation unit).
   A `.worktreeinclude` manifest (ccmanager precedent) carries untracked
   env files into new worktrees. Invariant: never silently delete
   unmerged work (Windsurf's 20-worktree LRU eviction is the
   counter-example) — cleanup requires merged-or-explicit.
7. **Review objects.** Review comments are shared, addressable objects in
   the event stream (path, line range, side, thread, resolve state), read
   by the agent through a tool — never compiled into a text prompt, which
   is the proxy pattern of products that lack a shared stream. Human
   direct diff edits are recorded as events too (Conductor's
   invisible-feedback lesson). The merge gate is the PR; conflict
   resolution may be delegated to an agent at prompt level; a semantic
   merge engine is explicitly not pursued (nobody has one; single-user
   scale does not justify inventing it).
8. **Scheduling.** The layer's semantics derive from events, so it starts
   inside the TUI: the session list gains the state machine and
   blocked-first ordering in phase 1; fleet policy and the worktree
   workflow land with concurrent sessions; the full surface becomes the
   post-phase-5 GUI's core job (ADR-0013 item 8's renderer). This ADR
   does not pull the GUI earlier, and the TUI-first order of ADR-0011 is
   unchanged.

## Consequences

- The roadmap capability track assigns the orchestration surface to
  phase 5 (Interaction row). ACP (ADR-0014) is deliberately assigned to
  no phase — secondary support, trigger-scheduled; the GUI is the
  main-line destination for this layer.
- The boundary restatement narrows report §2.5.1's swarm cut to swarm
  _logic_, licensing multi-session projection; ADR-0002's cross-node
  exclusion is untouched. Recorded here per the deviations-as-ADRs rule;
  the frozen report stands.
- Review objects enter the event vocabulary as additive `EventKind`
  variants (ADR-0005's additive-only rule) and must fold (the fold
  invariant); the normative shape lands in `cadmus-contract` with the
  feature, not here (ADR-0013 item 10's precision discipline).
- Consumes the orchestration/review parts of the 2026-09-11 survey open
  item; the remainder (GUI tech re-anchoring, research §5) stays for the
  GUI ADR. The scoped-rules open item's fleet generalization is recorded
  in item 5; its consumer (the config-layer implementation) is unchanged.
- best-of-n fan-out with side-by-side diff compare is licensed by items
  1–2 but waits on the persona/subagent ADR (the `task` tool trigger).
- The evolution projection's genealogy visualization (which skill grew
  from which trajectories; gate results and counters over time) is a
  candidate for phase 2's review UX — a differentiator no split product
  can copy, and deliberately not floor.
