# Open items

Field findings waiting for their consuming change — an inbox, not a
backlog. Every item names its consumer; when the consuming change lands,
delete the item (its rationale then lives in that change's ADR). An item
with no consumer does not belong here.

## Interaction surfaces never render logs

Consumer: the ADR-0011 TUI implementation, over the ADR-0013 live stream.

First live `chat` run (2026-09-07): at the default `warn` filter the user
saw only genai's `EMPTY CHOICE CONTENT` spam and no run progress, and the
bare final answer did not read as addressed to them. Requirement from the
field: the interaction surface renders structured progress (turn blocks,
tool activity); logs go to a file or an opt-in verbose channel, never into
the interaction view.

## The agent loop has no tracing instrumentation

Consumer: same as above, or a standalone interim change.

`cadmus-core`'s agent loop emits nothing at any level: `RUST_LOG=info`
gives per-case progress for `eval` but stays silent for `chat`. If wanted
before the TUI lands, info-level instrumentation (turn start, tool-call
name, finish status) is a small standalone change. The TUI's structured
progress carrier is ADR-0013's live stream; interim tracing stays useful
for `eval` and pre-TUI `chat`.

## Traces carry no workspace or ruler identity

Consumer: phase 2's reflector input selection.

Run attributes record provider/model/version/eval_split only (ADR-0005 §3),
so all projects' traces mix in one date-organized pool and the workspace
can only be reverse-engineered from tool arguments. Eval traces likewise
can't be grouped by ruler without the score files. When the reflector
lands, add `selfevol.workspace` and the corpus digest to start_run
(additive attrs, ADR-0005 amendment).

## The OOD probe is policy, not yet a mechanism

Consumer: the phase-2 gate ADR.

Report §11.2 mechanism 2 (periodic OOD probes: 10–20 fresh real-usage
samples, human-curated into the set) has no cadence, sampling or admission
procedure — a policy sentence without a mechanism, the failure mode
ADR-0010's context calls out for holdout isolation. The phase-2 ADR should
pin all three.

## read_file's line axis has no column resume

Consumer: a read_file byte-window parameter, if evals or traces ever show
models needing it.

Lines over 64 KiB are cut inline with a marker and their tails are
unreachable; grep's match preview likewise shows only the 512 B head. Both
deliberate — such lines are machine-generated (minified bundles, source
maps, serialized records) and paged raw into context they are a net
negative. If evidence ever justifies it, the additive extension is a
byte-window parameter; do not build it ahead of evidence.

## Approval resolve is not yet a command event

Consumer: the TUI PR (the ADR-0011/0013 implementation).

The write-tools change (ADR-0008 items 2–5) landed the gate as an
in-process `Approver` port: the loop asks, the injected client policy
answers, and only rejections enter the trajectory (as tool results).
ADR-0008 item 4's full design — `resolve_approval` as a command event, so
remote clients share one approval path and the approval itself is
recorded — lands with the first interactive client, together with the
ADR-0013 live-stream vocabulary (approval request/resolve, steer,
interrupt) it needs. Until then the ACP-seam assessment above describes
the design, not the code.

## ACP adoption seam assessment

Consumer: the ACP adoption ADR, whenever an ACP frontend is scheduled.

Assessed 2026-09-09 against ADR-0013: an ACP frontend is another client
kind of the client protocol, and the hard parts already align — trajectory
events map to session/update tool-call notifications, the approval path is
a command event shared by local and remote clients (ADR-0008 item 4), and
the tool-result is_error flag maps to ACP's failed status. The one real
seam: the built-in tools do their own filesystem IO, so ACP's client-side
fs capabilities (remote workspaces, editor-native diffs) would need an IO
port injected into the tools — additive behind `AgentTool`, aligned with
the constructor-injection style rule, not a redesign.

## Persona profiles: agent definitions, not settings presets

Consumer: the subagent/persona ADR (lands when the `task` tool's trigger
fires or the planner use case is scheduled).

Direction from the 2026-09-09 maintainer discussion; details deliberately
undesigned. A "profile" here is a complete agent definition — model
selection, role system prompt, tool config (approval mode/rules included),
MCP servers — not a Codex-style settings preset. Driving use case: a
planner persona invoking other personas via subagent calls.

Anchors and the boundary check:

- Report §2.4.1 (Prime Agent) already gives the concept first-class
  status: subagent specs are typed harness state behind one CRUD
  interface, local (single-session) by default, promoted to global
  explicitly — the scoping pattern to borrow, and the precedent for
  personas as evolution assets (`/refine` evolves them).
- The planner scenario stays inside the project boundaries: ADR-0002
  excludes cross-node task division, report §2.5.1 cuts multi-session
  swarm orchestration — a synchronous in-process nested run is neither.
  Mechanism slot: the `task` tool candidate (docs/tools.md) gains a
  profile parameter; the child run is a nested event stream returning a
  structured summary.

Parked design questions for that ADR:

1. Profile schema: model/provider, role prompt (inline vs file ref), tool
   allowlist + approval mode + rules, MCP servers (per-server least
   privilege, ADR-0008 item 6).
2. Invocation semantics: parent/child span relation in the trajectory;
   the structured-summary return shape.
3. Permission narrowing invariant: a child's effective permissions never
   exceed its invoker's — a permissive planner must not spawn a wider
   persona.
4. The profile layer's slot in the settings precedence (ADR-0012) — see
   the settings-stack entry below.
5. Storage and sharing: git-versioned text (report §5.3.1), user dir vs
   project dir, local/global promotion (§2.4.1).
6. Evolution: human-authored first (like phase-1 skills); persona
   evolution is a phase-2+ gate question.
7. Boundary with skills (ADR-0006): a skill injects procedural knowledge
   into the current agent; a profile defines an agent's identity and
   capability envelope. The overlap zone (a persona preloading skills)
   needs an explicit rule.

## Scoped approval rules and the settings stack

Consumer: the configuration-layer implementation (ADR-0011 item 5, TUI
era).

From the same discussion. Session/workspace-scoped "always allow" extends
the decided per-tool allow/ask/deny rules (ADR-0011 item 3) and needs no
core change — the `Approver` port composes; what lands later is policy
plus persistence. But the config layer's design must answer what the
report leaves open: no layering/precedence/XDG anywhere (ADR-0012's
precedence is our own), no config storage reconciliation — text assets go
to git (§5.3.1) while structured config goes to SQLite (§5.1.1), two homes
never reconciled, and the SQL store is deferred to phase 2 (ADR-0005).
Design constraint from §7.1.1: the report's answer to approval fatigue is
architectural enforcement (sandbox + allowlist), not rule-learning —
users approve ~93% of prompts anyway, so scoped "always" rules are comfort
for the last mile, never the safety mechanism.
