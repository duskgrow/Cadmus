# 0005. Phase 1 data foundation: append-only JSONL event log as trajectory SSOT, SQL store deferred

- Status: accepted
- Date: 2026-09-03

## Context

Phase-1 kickoff followed the roadmap ritual: report §10.2.2 re-read,
time-sensitive claims re-verified (2026-09-03: rusqlite 0.40.2, sqlite-vec
0.1.9 still pre-v1 with ANN issue #25 open, refinery 0.9.2, fastembed 6.0.2
`Bgem3Embedding`, OTel `gen_ai.*` still all-Development with zero tagged
releases, bge-m3 without successor — all baseline claims hold).

One claim did not survive scrutiny: report §5.2.1 puts the trajectory SSOT in
SQLite span tables. Its evidence (LangSmith/Braintrust convergence,
Meta-Harness ablation) comes from **server-side observability platforms** — a
different problem than a local single-user agent's operational store. A
mainstream-practice verification (2026-09-03) shows the local CLI cohort
converged on per-session **append-only JSONL as SSOT** with resume/crash
recovery built on it: Claude Code (per-session JSONL transcripts), Codex CLI
(JSONL rollouts as source of truth; its SQLite DBs are derived and rebuilt
from rollouts on corruption), Gemini CLI (migrated whole-file JSON →
append-only JSONL with tombstones). DB snapshots (LangGraph) live
server-side; OLAP stores (ClickHouse in Langfuse/LangSmith) serve analytics.

Report §10.2.2's phase-1 SQLite scope (StorePort, sqlite-vec, FTS5) also has
no hard demand until phase 2's skill store; landing it now would violate the
report's own trigger-driven-evolution principle (§8.3, insight 5).

Two operational requirements pinned by the maintainer: trajectories are the
evolution asset, so they are **never auto-deleted**; and the store must
survive **relocation/tiering** (e.g. moving old data from SSD to HDD).

## Decision

1. **Trajectory SSOT = per-trace append-only JSONL event files** under a
   configurable root, date-sharded (`traces/YYYY/MM/DD/<trace-id>.jsonl`).
   One self-describing JSON event per line; events reference each other by
   `trace_id`/`span_id`/`parent_span_id`, never by file path. Full
   request/response text stays inline **at message granularity** — the
   start_run base plus the response/result events carry the complete history
   and the fold reconstructs any turn's exact request, so per-turn request
   snapshots are deliberately not duplicated (they would grow a trace
   quadratically in turns; the Meta-Harness full-text evidence is preserved,
   and §5.2.1's content-addressed blob table dies with the medium change).
   Trace ids carry the UTC run-start date (`tr-YYYYMMDD-…`), making the
   shard path a pure function of the id — no directory scans, and file names
   are immutable and time-ordered by construction. Readers tolerate a torn
   trailing line after a crash.
2. **Retention: never auto-delete** (Claude Code's 30-day default is a
   product policy hostile to an evolution asset). Deletion is explicit-only.
   Hot/cold tiering = moving shard directories and adding a symlink or an
   extra read root; replayers and future indexes resolve all configured
   roots.
3. **Event model** (ADR-0002's event-sourced loop): llm request/response,
   tool call/result, eval score, and control (command) events, carrying
   trace/span/parent IDs, timestamps, status and a free attribute bag.
   Naming references OTel `gen_ai.*` vocabulary, but long-lived keys live in
   our own namespace (`selfevol.*`) — the upstream namespace remains
   all-Development with no tagged release.
4. **Deterministic replayer**: the same log replays to an identical run
   state, snapshot-tested in CI. This replaces report §10.2.2's "JSONL export
   byte-identical" criterion — the SSOT already is JSONL, and
   training-format conversion belongs to the deferred phase 4.
5. **SQL store deferred to phase 2.** Trigger: the skill store — mutable
   relational state (skill metadata, ACE counters, gate outcomes) is the
   first hard demand. When it lands it follows the derived-index discipline
   (the Codex pattern): one-way projection (log → DB, never reverse);
   projection version stamp with rebuild-on-mismatch; append-following
   projection so lag stays within a crash window; a CI determinism test
   (same log → two rebuilds → identical state hash); disposable by policy.
6. **Open questions**: O4 stays deferred (trigger: local embedding becomes
   load-bearing); O6 moves to phase 2 alongside the store it benchmarks.
   Phase 1 adds **zero third-party dependencies**. The xtask architecture
   test is implemented as std-only text scanning (member `Cargo.toml`
   dependency whitelist + `derive(Serialize)` boundary scan) — a deviation
   from report §9.1.2's `cargo_metadata` + `syn`, forced by xtask's
   zero-dependency policy.
7. **Eval set v1**: ≥50 scenario cases; one command runs the full set and
   writes a score file; per-run scores are also recorded as score events in
   that run's log.

Phase-1 scope is thereby: event model + log writer port + replayer, eval set
v1, and the architecture test in CI.

## Consequences

- Deviations from the frozen report (recorded here per roadmap rules):
  trajectory SSOT medium is JSONL, not SQLite span tables; StorePort /
  SQLite / sqlite-vec / FTS5 move to phase 2; O6 moves with them; the export
  criterion becomes rebuild determinism; the architecture test is std-only.
- The report's #1 single-point risk (§11.1: trajectory schema irreversibility,
  history cannot be backfilled) is **downgraded**: self-describing events
  plus rebuildable projections make schema evolution additive-only.
- Session resume/fork is a replay of the log — the seam is preserved in the
  event model but not built in phase 1.
- Phase 2 inherits the store work (skill schema + derived index); its scope
  grows accordingly.
- Local inference and distillation (phases 3/4) are confirmed future scope:
  local inference is another `Provider` behind the existing port (report
  §10.2.4) — no `InferPort` exists; the training-input seam is this ADR's
  log plus a later format converter.
- `TransportPort` / `SandboxPort` land with their first real adapter (daemon
  work / phase-3 Landlock) — a deliberate narrowing of ADR-0002's
  "in-process channel first (phase 1)" timeline, since no phase-1 component
  speaks through them.
