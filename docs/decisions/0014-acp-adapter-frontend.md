# 0014. ACP adapter frontend: secondary editor integration, trigger-scheduled — not the main line

- Status: accepted
- Date: 2026-09-11

## Context

ADR-0013 established N frontends as one client kind over the client
protocol. The 2026-09-11 UI/UX research
(`docs/research/2026-09-11-agent-uiux-landscape.md` §3.5, protocol spec
fetched that day) verified that ACP (Agent Client Protocol, v1 stable —
JSON-RPC over stdio, HTTP/WebSocket transport in RFD) is the industry's
converging editor↔agent protocol, with clients in Zed and other editors.
Its session model is isomorphic to ours: `session/update` notifications
are agent-emitted events, and `session/load` is defined as replaying that
same stream — ADR-0013's attach = replay + sync + tail. It carries what a
PTY never can: typed tool-call lifecycle, diffs as old/new text rather
than pixels, permission requests with allow/reject once/always options,
plan state, cumulative usage. Its gaps are equally explicit: no per-event
cost, no checkpoint/rewind (RFD stage), no compaction events, no
rate-limit proximity, no subagent trees, and nothing orchestration-shaped
— ACP assumes the client is an attending human's editor, so there is no
multi-session fan-out and no blocked semantic.

Coverage reality check (prompt-turn spec fetched 2026-09-11): turn
control is `session/prompt` + `session/cancel` only — no queue, no steer
granularities, no mid-turn settings; permission outcomes are
selected/cancelled with no comment channel; checkpoints/rewind,
per-event cost, subagent trees, notifications and the status-line chrome
all sit outside the protocol; and rendering is the client's — ACP
carries semantics, each editor decides presentation. The adapter
therefore covers the single-session conversation loop (read / approve /
diff — the editor's home turf) and defers the rest of the floor to the
native surfaces: the TUI stays the reference renderer with the full
floor, the GUI owns orchestration (ADR-0015).

The open item "ACP adoption seam assessment" (2026-09-09) already found
the hard parts aligned: trajectory events map to `session/update`
tool-call notifications; the approval path is a command event shared by
local and remote clients (ADR-0008 item 4); the tool-result `is_error`
flag maps to ACP's failed status. The one real seam it named: the
built-in tools do their own filesystem IO, while ACP offers client-side
fs capabilities (remote workspaces, editor-native diffs) — an IO port
injected into the tools, additive behind `AgentTool`.

Maintainer direction (2026-09-11, two steps): an initial direction
scheduled the ACP frontend as the "GUI before the GUI". The same-day
coverage check above showed ACP covers only the single-session
conversation loop — steering, rewind, exact cost, subagents, evolution
review and orchestration all stay outside — so the maintainer revised:
ACP is secondary support, not the main line; the native GUI (ADR-0015's
surface) is the main-line destination. The protocol case for adoption
stands (cheap, isomorphic); what changes is the priority.

## Decision

1. **The ACP adapter is a frontend crate** (`cadmus-acp` when it lands),
   a sibling per ADR-0013 item 9's topology: ADR-0013's client protocol
   inward, ACP outward. No core change; the arch test's forbidden-edge
   rule covers it (presentation dependencies only in frontend crates).
2. **Mapping is translation, never extension.** `session/update`
   notifications translate our live-stream items; `session/prompt` and
   `session/cancel` translate to commands; `session/request_permission`
   translates our approval request, where `allow_always` maps to the
   scoped grant of ADR-0011's 2026-09-11 amendment item 3 (turn/session
   scope; persisted rules stay our config layer's and are never promised
   by the adapter). Where ACP lacks a concept (checkpoints, per-event
   cost, subagent trees, blocked semantics), the adapter degrades
   silently. Cadmus-specific extensions live only in ACP's own
   extensibility points (the universal-standards-first rule), to be
   surveyed at implementation time.
3. **The tool IO seam lands additively.** An IO port is injected into
   the built-in tools behind `AgentTool` (constructor injection per the
   style rule), defaulting to the local filesystem; ACP's
   `fs/read_text_file` / `fs/write_text_file` realize it for
   editor-remote workspaces. Local clients see zero behavior change.
4. **Schedule: trigger-driven, never a gate.** Accepted in principle
   but assigned to no phase. The trigger: a real need — the maintainer
   daily-driving Cadmus inside an ACP editor, or external adoption
   demand — and only after the TUI has proven the client protocol. The
   adapter never gates or delays the orchestration GUI; if the trigger
   never fires, nothing is lost. When it lands, it runs the client
   protocol's executable semantics (drop positions ≤ as_of_seq, `Lagged`
   forces re-sync, idempotent commands) against an ACP fake in the
   ADR-0003 port-suite pattern, and dependency admission (a Rust ACP
   crate vs hand-rolled JSON-RPC) goes through the
   `adding-dependencies` skill.
5. **What ACP does not carry stays ours.** Orchestration semantics
   (multi-session, blocked rollup, fleet policy) are ACP's explicit
   non-goal and remain the orchestration layer's job (ADR-0015). ACP is
   a downstream translation of our event stream, not its ceiling — and
   it is a side surface: the TUI stays the reference renderer, the
   native GUI the main-line destination.

## Consequences

- Every ACP-compatible editor becomes a Cadmus GUI at protocol cost
  only — but as a convenience side surface, never the reference renderer
  (the TUI) or the main line (the native GUI and its orchestration
  surface, ADR-0015).
- Consumes the open item "ACP adoption seam assessment"; its rationale
  lives here.
- The tool IO port widens the hexagonal port surface: the arch test
  gains the port, and tool contract tests cover both realizations
  (local fs, ACP fs).
- ACP version skew: v1 stable is the target; v2-draft features (fork,
  compaction) are adopted only when stabilized — assessed at
  implementation, not tracked here.
- `session/load` replay maps to reading the JSONL log (ADR-0005), so
  long-history attach inherits the open item "Session attach payload,
  fold and render all scale with history" (bounded Sync window), which
  gains a second consumer.
