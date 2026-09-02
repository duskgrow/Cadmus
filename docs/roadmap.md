# Roadmap

Status snapshot of the six-phase plan defined by the research report
([baseline 2026-08-29](research/2026-08-29-self-evolving-agent-research.md) §10).
Details live in the report and in the ADRs; this file only tracks where we are.

| Phase                                  | Goal (one line)                                                         | Status      | ADRs                                                                                                                       |
| -------------------------------------- | ----------------------------------------------------------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------- |
| 0. Minimal agent loop                  | chat + coding tools + two providers validating the dialect seam         | not started | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md), [0003](decisions/0003-provider-abstraction-layers.md) |
| 1. Architecture & data foundation      | ports/adapters crates + SQLite + trajectory spans + eval set v1         | not started | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md)                                                        |
| 2. Self-evolution loop v1              | reflect → skill delta → gate → deterministic merge → versioned rollback | not started | —                                                                                                                          |
| 3. Local inference + cascade + sandbox | llama-server node, escalation routing, Landlock                         | not started | —                                                                                                                          |
| 4. Distillation pipeline               | accumulated trajectories → LoRA adapters                                | not started | —                                                                                                                          |
| 5. Remote control plane                | daemon + remote attach client; CF Tunnel → iroh evolution               | not started | [0002](decisions/0002-hexagonal-architecture-event-sourced-core.md) (control-plane seam)                                   |

Rules for this file:

- Update it when a phase starts or completes — nothing else.
- Deviations from the report are recorded as new ADRs, never by editing the
  report. Phase-kickoff ritual: re-read report §10.2.N, re-verify its
  time-sensitive claims, then write the phase's ADR(s).
- Open questions O1–O10 (report §11.3) are killed by phase acceptance
  criteria, recorded in the phase's ADR or PR — not tracked here.
