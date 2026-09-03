# 0002. Hexagonal architecture with an event-sourced core and a remote control plane

- Status: accepted
- Date: 2026-09-02

## Context

Cadmus is a self-evolving coding agent built and maintained by a single
developer who refactors frequently; module boundaries must therefore double
as test boundaries, and any component must be replaceable without touching
the core. The full evidence base is the frozen research report
(`docs/research/2026-08-29-self-evolving-agent-research.md`, baseline
2026-08-29), especially §3 (architecture) and §8.3 (evolve behind a port).

The multi-device requirement shaped this decision: a task keeps running on
the machine that started it (e.g. the work computer), while other devices get
**full client parity** — everything agent-related the local client can do
(observe progress, converse, submit tasks, approve, steer) a remote client
can do too — explicitly **not** task migration. This preserves the
single-writer discipline (report §5.3): many _command producers_, exactly one
_state writer_ — the node owning the task is the only process that appends to
the event log and mutates the store. The report's state-sync design (§5.3,
Syncthing snapshots) answers a different question (sequential use across
machines) and is deferred.

## Decision

Hexagonal (ports-and-adapters) architecture on a flat `crates/*` layout
(report §3.1): the core never `use`s an external capability directly; every
external interaction goes through a port trait defined in the contract crate,
the only place serializable wire types live.

Crate layout (report §3.1.2, adapted to this repo's naming; internal crates
are scaffolded via `just new-crate` and stay `publish = false`):

- `cadmus` — existing binary: thin CLI entry (later also the daemon), wiring only
- `cadmus-contract` — port traits + wire types + `ModelProfile` (see ADR-0003)
- `cadmus-core` — agent loop, evals, skill orchestration; pure logic, no IO
- `cadmus-llm-openai` — OpenAI-compatible provider adapter + per-vendor dialects
- `cadmus-memory` — trajectory store adapter: append-only JSONL event log first (phase 1), SQL derived index in phase 2 (narrowed by ADR-0005; originally "SQLite store adapter (phase 1)")
- `cadmus-sandbox` — subprocess sandbox adapter (phase 0: confirmation gates; phase 3: Landlock)
- `cadmus-transport` — transport port; in-process channel first (phase 1), CF Tunnel / iroh later (phase 5)

The agent loop is **event-sourced**: every step appends to the same
append-only log that is the trajectory asset (report §5.2). Every client
operation — approvals, conversation messages, task submission, steering — is
a _command_ the owning node validates, orders and appends to that log;
commands carry unique IDs so retries over an unreliable transport apply
idempotently. Approval gates are first-class loop primitives. The local CLI
and future remote clients are the same kind of client — subscribe to the
event stream, send commands — so phase 5's remote control plane becomes a
transport swap, not a core change. This aligns with the repo rule that
time/randomness/IO are always injected: the core never touches stdin/stdout
directly. Infrastructure operations (daemon lifecycle, eval gates, sandbox
policy, eval-set management) are deliberately **local-only**, outside the
remote surface — they are the system's root trust anchor and stay outside
everything remotely reachable (report §11.2).

Dispatch rules (report §3.2.1): generics inside core hot paths;
`Arc<dyn Port + Send + Sync>` at port boundaries; `async_trait` only on
low-frequency IO ports.

Deliberately **not** split (report §3.2.2): the agent loop and the eval
harness stay in the core — no port buys anything there. Every split carries a
written trigger signal (e.g. transport: a second machine joins; vector store:

> 300k vectors or query p95 >100ms). Dependency direction is enforced by an
> xtask architecture test in CI (report §9.1.2): a whitelist violation fails
> the build.

## Consequences

- Phase 5 is redefined as the **remote control plane** (daemon + attach
  client over CF Tunnel → iroh, per report §8.3's three-step evolution); the
  Syncthing state-sync design becomes an optional sub-item, revisited only if
  sequential multi-device use ever becomes a real need.
- Multi-agent task division remains excluded (report §1.1.1 boundary): worker
  nodes writing to shared state would break single-writer discipline and add
  scheduling/partial-failure subsystems no current requirement justifies.
  Each device may run its own tasks (each its own single writer) and a client
  may attach to any node in `peers.toml`; what stays excluded is task
  handoff/migration between nodes.
- An adapter replacement must never force a change in `cadmus-core`; if one
  does, that is the empirical signal of a boundary leak and triggers a
  boundary audit (report §11.1, meta-decision row).
- Crate count is not a goal: a new crate exists only when a boundary carries
  an invariant (rust-analyzer discipline, report §3.2.2).
