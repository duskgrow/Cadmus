# 0013. Client protocol: sync-on-subscribe live stream, idempotent commands, N frontends

- Status: accepted
- Date: 2026-09-08

## Context

ADR-0011 item 2 established "two frontends, one core" (interactive TUI +
headless print mode), and ADR-0012 item 4 noted a future GUI would be
"another client of the same event protocol". Two maintainer updates
(2026-09-08) turn that seam from implied to load-bearing:

- A desktop GUI is now **planned** — Rust-native, possibly GPUI. GPUI is
  GPL-licensed, so the GUI cannot live in this repo: it will be a separate
  repository importing cadmus as a library, landing after phase 5 (when the
  transport exists). The public APIs of `cadmus-contract` and `cadmus-core`
  thereby become a semver stability surface, and GPL code must never enter
  this repo (the cargo-deny license gate is the mechanical backstop).
- Field pain from the maintainer's prior agent work: a client attaching
  mid-turn receives only the deltas after its attach point, renders a
  fragmented message tail, then jumps when the complete response event
  arrives. This is not GUI-specific — the TUI hits it on session-picker
  attach to a running session and on the multi-session dashboard (ADR-0012
  item 3), and phase-5 remote attach hits it by definition.

Current code reality: the agent loop's only observer is the durable log
(`Telemetry.sink` in cadmus-core). There is no live delta broadcast, no
approval request/resolve cycle, no steering command — `EventKind` has no
such variants. A TUI built without this seam will either poll the log or
reach into loop internals; both are the boundary leak ADR-0002 audits.

Constraints already decided and unchanged: the durable log stays
message-granular (ADR-0005 — per-token deltas would bloat the trajectory
asset); crash recovery is the log's job (a process crash kills the
in-flight aggregation; the partial response lands as an errored message
event); clients are command producers and event subscribers under a single
writer per run (ADR-0002).

## Decision

1. **One client protocol, N frontends.** A client is an event subscriber
   plus a command producer. The TUI, headless `chat --json`, the
   post-phase-5 GUI and remote attach are the same kind of client over the
   same protocol; transports (in-process, stdio NDJSON, the phase-5
   socket) and renderers vary, the protocol does not. ADR-0011's "two
   frontends" is superseded by N frontends.
2. **Two downstream channels with different durability.** The durable log
   (ADR-0005, message granularity, never auto-deleted) and an ephemeral
   live stream (turn/span boundaries, text/reasoning deltas, tool-call
   argument deltas, approval requests, status counters). Live deltas are
   never appended to the log. The log writer is one subscriber of the live
   stream; frontends are others.
3. **Sync-on-subscribe, never a bare delta firehose.** The attach
   handshake replies `Sync { history, in_flight, as_of_seq }`: `history`
   is the log fold (`RunState`); `in_flight` is the live aggregator's
   current state — the open turn's assembled text/reasoning so far, open
   tool calls with args so far, pending approval requests (an attach
   during an approval wait must render the dialog immediately), usage and
   cost counters. The client's render baseline is history + in_flight; it
   then applies only deltas with `seq > as_of_seq`. The aggregation state
   already exists in the loop (it must, to assemble the final message) —
   Sync exposes it and adds no new aggregation logic.
4. **One comparable per-run ordering across both channels.** Without a
   durable-side ordering the handshake has a TOCTOU hole: an event
   appended between the fold and the subscription is missed or duplicated.
   Every run therefore carries a totally ordered, monotonic position that
   stamps durable events and ephemeral deltas alike; attach means
   "subscribe from position X" and the broadcaster backfills `(X, now]`
   from a ring buffer bounded by the current turn, with older history
   coming from the fold. Whether this reuses the existing id sequence or
   adds an envelope field is an implementation detail, provided the
   ordering is total per run and additive to the log schema (ADR-0005's
   additive-only rule). The client rule is one line: drop positions
   `≤ as_of_seq`, apply the rest — idempotent and retry-safe over lossy
   transports.
5. **Lag recovery by re-sync.** A subscriber that falls behind gets
   `Lagged` and re-attaches for a fresh Sync (tokio broadcast semantics
   in-process; the same rule serializes for remote transports). The final
   `llm_response` reconciles exactly with a complete delta buffer, and
   Sync makes deltas complete from the turn start — the completed message
   lands with zero visual jump.
6. **Commands are the only upstream.** Steering (queue-for-next-turn vs
   inject-now), interrupt, resolve_approval (with the optional comment of
   ADR-0011), rewind, plan approval and `ask_user` answers are contract
   types with idempotent command ids (ADR-0002). No frontend gets a second
   API. Interactive clients wait on the user; unattended ones answer deny
   (ADR-0008/0011) — both through the same command path.
7. **Semantics in core, presentation in clients.** Events carry semantic
   payloads (paths, edit strings, tier, tool names), never presentation
   strings (rendered diffs, markdown, highlighting). Policy — the L0–L3
   tiers, timeouts, the approval floor — is core's; how a prompt is drawn
   is the client's. Agent and session state derive from events alone
   (ADR-0012 item 3); window state (scroll, folds, keymap, theme) is
   client-local. A frontend needing core to compute a display value is a
   boundary-leak signal (ADR-0002's audit trigger). The two "status bars"
   never share code: the model-facing trailer is core's context assembly
   (ADR-0007), the human-facing status line is a client projection.
8. **GUI boundary.** The GUI lives out-of-repo (GPL) and imports cadmus as
   a library; contract types stay runtime-agnostic pure data (no tokio
   types in the wire vocabulary). Landing after phase 5, it attaches over
   the transport like any remote client — a new renderer, not a new client
   kind.
9. **Crate topology and the arch test.** Frontends are sibling crates
   (`cadmus-tui` when it lands); the `cadmus` binary stays thin wiring.
   When the first frontend crate lands, the architecture test gains a
   forbidden edge: presentation dependencies (terminal/GUI frameworks,
   markdown renderers) may appear only in frontend crates; core and
   contract never depend on a frontend.
10. **Where precision lives.** This ADR pins the invariants and the
    rationale — it is not the wire spec. The normative message vocabulary
    lands with the write tools as `cadmus-contract` types with rustdoc;
    the executable semantics (drop positions `≤ as_of_seq`, `Lagged`
    forces re-sync, command idempotency) become a client-protocol
    contract-test suite in the ADR-0003 port-suite pattern, run by fakes,
    the in-process broadcaster and the stdio transport alike; the
    byte-level NDJSON shape is insta-snapshot-locked. Prose here never
    hand-copies field lists — that is drift by construction.

The attach handshake of items 3–5 as one picture (the prose is the
invariant SSOT; the diagram illustrates, it does not specify):

```mermaid
sequenceDiagram
    participant C as Client
    participant B as Broadcaster
    participant L as JSONL log
    C->>B: Attach(trace_id)
    B->>L: fold history
    B->>B: snapshot live aggregator
    B-->>C: Sync: history, in_flight, as_of_seq
    Note over C: baseline = history + in_flight; drop seq ≤ as_of_seq
    B-->>C: Delta as_of_seq + 1
    B-->>C: Delta as_of_seq + 2
    alt client falls behind
        B-->>C: Lagged
        C->>B: Attach again
        B-->>C: fresh Sync
    end
```

## Consequences

- The write-tools PR (ADR-0008) implements the minimal protocol — the
  live-stream port, approval request/resolve, steer, interrupt — so the
  TUI is born as a client rather than grown out of the loop.
- Open item "the agent loop has no tracing instrumentation" keeps its
  interim answer (info-level tracing helps `eval` and pre-TUI `chat`); the
  TUI's structured progress comes from the live stream, and "interaction
  surfaces never render logs" stays binding.
- Attach semantics are uniform: `attach = replay(log) + sync(live) +
  tail(deltas)` — session picker, dashboard, GUI, remote attach and
  headless `--json` (the degenerate case: attach at position 0) share one
  code path.
- Phase-5 remote attach inherits the handshake; the per-run ordering plus
  idempotent commands make retries over unreliable transports safe by
  construction.
- The insta mechanical gates (ADR-0011/0012) extend to serialized Sync
  payloads over fixed event streams.
- This ADR amends ADR-0011 item 2 ("two frontends") and its "desktop
  frontends not pursued" consequence, and ADR-0012 item 4 ("GUI distant,
  unlikely"); the original texts stand as historical context per the
  amendment convention.
