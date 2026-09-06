# 0009. User memory and session continuity

- Status: accepted
- Date: 2026-09-06

## Context

The agent has no product-level memory: project instructions (a workspace's
`AGENTS.md`) are not loaded (their placement is fixed by ADR-0007's frozen
prefix), user preferences are nowhere stored, and sessions are one-shot —
the event model preserved the resume/fork seam (ADR-0005) but no phase
builds it. Identified in the 2026-09 design review: the roadmap designs the
evolution assets but not the agent's own continuity.

Evidence base beyond the frozen report: ai-agent-book v2.0 (fetched
2026-09-06), chapter 3 (memory taxonomy; append-only facts with
retrieval-time conflict resolution — Mem0 v3's move from write-time
UPDATE/DELETE to append+timestamp took LoCoMo 71.4→92.5; buffered
offline-extraction writes; human-approved memory diffs) and chapter 5
(persistent memory as the fourth lethal element — an attack amplifier that
lets malicious instructions lurk across sessions; the neutral-trajectory
format whose goals our event-sourced log already meets). The frozen report's
memory position stands: strategy-level memory _is_ the skill library
(§2.5.1), so this ADR adds no second strategy store.

## Decision

1. **Memory split (SSOT discipline).** Procedural/strategy memory = the
   skill library (ADR-0006), nothing else. User/preference memory = an
   append-only fact log rendered to Markdown, git-versioned under the same
   discipline as skill folders. Episodic memory = a derived projection over
   the JSONL traces, landing with the phase-2 SQL store (rebuildable,
   disposable, log→DB one-way per ADR-0005).
2. **Write path: agent proposes, human approves.** The single-user case of
   the book's proposer/reviewer flow — every memory write is a visible diff
   the user approves (same gate machinery as skill deltas, phase 2). No
   write-time UPDATE/DELETE: superseding facts are appended with timestamps
   and conflicts resolve at retrieval by recency. Nothing is ever
   auto-deleted (trajectory retention rule, ADR-0005). No silent or
   automatic memory writes, ever — an open auto-write channel is precisely
   the attack amplifier the book warns about.

   Open question (maintainer, 2026-09-06): preference drift is the known
   hard part — contradictory facts accumulate (a preference stated in March
   vs its opposite in June). The v1 answer is structural: append-only with
   timestamps, supersede pointers instead of rewrites, recency resolution
   at retrieval, and a human gate that sees the contradiction at proposal
   time. This is a bet on Mem0 v3's evidence, not a settled conclusion;
   the revisit trigger is contradiction-handling failures observed in the
   review queue during real use.
3. **Injection.** The approved memory card renders into ADR-0007's frozen
   prefix (stable within a run; changes take effect on the next run),
   size-capped; when the fact log outgrows the cap it stays retrievable on
   demand through a read-only memory tool rather than inflating the prefix.
4. **Session continuity.** Resume = replay the event prefix to rebuild
   working state — the deterministic replayer (ADR-0005 item 4) is the
   mechanism, this ADR only schedules its consumer. Fork = copy the prefix
   under a new trace id with the lineage recorded in `start_run` attributes.
   Interactive steering and interrupts are command events (ADR-0002): the
   local TUI and future remote clients share one path, so interactivity adds
   no new core concept.
5. **Phase placement.** Interactive session + resume/fork land with the write
   tools (ADR-0008) — together they are the "daily driver" milestone the
   report's single-machine-usable-early principle (§10.1.1) points at. The
   user-memory write path lands in phase 2, reusing the skill loop's
   git+gate machinery. Episodic cross-session retrieval waits for the
   phase-2 store (its trigger: the user actually needs to recall past
   sessions).

## Consequences

- Memory writes become gate-reviewed evolution artifacts alongside skill
  deltas; the same rollback discipline (git revert) covers them.
- Cross-session recall quality is initially bounded by trace-file search;
  embeddings stay deferred per ADR-0005 (O4 fires only when local embedding
  becomes load-bearing).
- Deliberately not done (book-verified negatives at single-user scale):
  GraphRAG/RAPTOR-style structured knowledge graphs; LLM write-time
  consolidation of the fact log; a cloud memory-sync service (git-synced
  Markdown covers 2–5 devices); per-user weight adaptation (fact-LoRA fails
  on indirect reasoning; distillation stays scenario-scoped per report §6).
- The eval set gains memory-related scenarios only after the write path
  exists — evaluating recall of a memory the agent was never allowed to
  write measures nothing.
