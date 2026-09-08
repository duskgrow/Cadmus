# Roadmap

Status snapshot of the six-phase plan defined by the research report
([baseline 2026-08-29](research/2026-08-29-self-evolving-agent-research.md) §10).
Details live in the report and in the ADRs; this file only tracks where we are.

| Phase                                  | Goal (one line)                                                                                                | Status      | ADRs                                                                                                                                                                      |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0. Minimal agent loop                  | chat + coding tools + two providers validating the dialect seam                                                | completed   | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md), [0003](decisions/0003-provider-abstraction-layers.md), [0004](decisions/0004-defer-rmcp-adoption.md) |
| 1. Architecture & data foundation      | JSONL event-log trajectory store + replayer + eval set v1 + arch test (SQL store deferred to phase 2 per 0005) | in progress | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md), [0005](decisions/0005-jsonl-event-log-trajectory-ssot.md)                                            |
| 2. Self-evolution loop v1              | reflect → skill delta → gate → deterministic merge → versioned rollback                                        | not started | [0006](decisions/0006-agent-skills-format-skill-library.md) (skill format pre-decided)                                                                                    |
| 3. Local inference + cascade + sandbox | llama-server node, escalation routing, Landlock                                                                | not started | —                                                                                                                                                                         |
| 4. Distillation pipeline               | accumulated trajectories → LoRA adapters                                                                       | not started | —                                                                                                                                                                         |
| 5. Remote control plane                | daemon + remote attach client; CF Tunnel → iroh evolution                                                      | not started | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md) (control-plane seam)                                                                                  |

Capability track — what users can do once a phase lands (ADR-0011: a
capability is not landed until its UX is landed). Details live in the ADRs;
this matrix only assigns per-phase ownership.

| Capability                                   | Phase 1                                                                                                                                                            | Phase 2                                                                                                           | Phase 3                 | Phase 5                                                                                                                                                     |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Interaction (ADR-0011/0012)                  | TUI: streaming render, interrupt + steering, approval modes, checkpoint/rewind, resume/fork, status line, slash commands; headless `chat --json` + CLI citizenship | evolution review UX (delta diffs, gate results, counters)                                                         | sandbox-aware approvals | remote attach = same client protocol ([ADR-0013](decisions/0013-client-protocol-sync-and-commands.md)), new transport; out-of-repo GUI renderer lands after |
| Tool surface (ADR-0008, [catalog](tools.md)) | `write_file` + `edit_file` behind L0–L3 gates                                                                                                                      | frozen                                                                                                            | `shell_exec` in sandbox | —                                                                                                                                                           |
| Context pipeline (ADR-0007)                  | frozen prefix + `AGENTS.md` + status bar + mechanical compaction                                                                                                   | LLM archival compaction (trigger-driven)                                                                          | —                       | —                                                                                                                                                           |
| Memory (ADR-0009)                            | project instructions in prefix                                                                                                                                     | user-memory write path (human-approved diffs); episodic projection lands with the store (retrieval trigger-gated) | —                       | —                                                                                                                                                           |
| Skills (ADR-0006)                            | static load + catalog injection (human-authored)                                                                                                                   | evolution loop v1 (reflect → delta → gate → merge)                                                                | —                       | —                                                                                                                                                           |
| MCP (ADR-0004)                               | deferred — triggers only                                                                                                                                           | same                                                                                                              | same                    | same                                                                                                                                                        |

Phases 0 and 4 add no user-facing capability rows (0 is complete; 4 is an
offline pipeline).

Rules for this file:

- The status table is updated when a phase starts or completes; the
  capability track is updated when a capability lands or moves phases —
  nothing else.
- Deviations from the report are recorded as new ADRs, never by editing the
  report. Phase-kickoff ritual: re-read report §10.2.N, re-verify its
  time-sensitive claims, then write the phase's ADR(s).
- Open questions O1–O10 (report §11.3) are killed by phase acceptance
  criteria, recorded in the phase's ADR or PR — not tracked here.
