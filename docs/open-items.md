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

Interim landed (2026-09-09, the client-protocol change): headless `chat`
renders structured progress to stderr from the live stream (turns, tool
calls, approval requests, denials), so "no run progress" is fixed for the
print-mode surface. What remains for the TUI: the full interaction floor,
and the log-channel discipline — at the default filter, tracing warns still
share stderr with the progress view.

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

## Session attach payload, fold and render all scale with history

Consumer: the TUI session picker / multi-session dashboard — the first
attaches to live runs carrying real history, and to long finished sessions.

Three coupled costs, all O(history). The in-process broadcaster retains
every durable event of the run and folds them with `replay_trace` per
attach, under the publish lock. A remote attach serializes the full
`RunState` — tool results verbatim dominate (field experience 2026-09:
attaching to a long session meant a long transfer; the temporary workaround
was transport-level compression, which is legitimate but only a
transport-layer answer). And a client that renders the fold from the head
scrolls through the whole session on attach.

Design direction (2026-09-09 maintainer discussion): `Sync` carries a
bounded recent window plus a cursor, never the full fold; the client
viewport anchors at the tail, and scrolling up pages older events lazily
through a read verb (a request/response pair — not a command: it changes
no state, and the two travel separately). The fold itself becomes
incremental (`replay_trace` rehomed as `push(&Event)` onto a `RunStateFold`
the broadcaster keeps), so attach is O(window) and the retained event vec
dies.

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
core change — the composition point is the client policy producing
`resolve_approval` commands (an auto-resolving decorator on the live
stream, as eval/headless chat already do); what lands later is policy plus
persistence. But the config layer's design must answer what the
report leaves open: no layering/precedence/XDG anywhere (ADR-0012's
precedence is our own), no config storage reconciliation — text assets go
to git (§5.3.1) while structured config goes to SQLite (§5.1.1), two homes
never reconciled, and the SQL store is deferred to phase 2 (ADR-0005).
Design constraint from §7.1.1: the report's answer to approval fatigue is
architectural enforcement (sandbox + allowlist), not rule-learning —
users approve ~93% of prompts anyway, so scoped "always" rules are comfort
for the last mile, never the safety mechanism.

## LLM compaction fires on the ceiling rule, not on ContextLength sightings

Consumer: the phase-2 compaction ADR.

ADR-0007's 2026-09-10 amendment re-anchors the layer-2 trigger: the
compactor fires when a fold at the 80% ceiling finds nothing foldable or
leaves the context over the line. Until it lands, that case is the
provider's ContextLength error. Mainstream anchors (2026-09-10): Gemini
CLI compresses at 0.5 of the window by default; Anthropic's context
editing clears tool results at 100k input tokens with keep=3. The
compounding-summaries risk stands (a summary of a summary), so the
original circuit breaker and the full-fidelity log stay load-bearing.

## Loop health: stuck detection and budget caps, never periodic nudges

Consumer: the loop-health mechanism (TUI era, or the first long-task pain
in traces).

Research digest (2026-09-10, official docs + source): no mainstream agent
injects periodic re-planning nudges, and no controlled evidence for them
exists — Anthropic's context-engineering guidance is a minimal
high-signal token set with structured notes (the todo list) as the
persistence mechanism. The evidence-backed form is event-driven:
OpenHands' StuckDetector injects one nudge on repeated action-error
patterns, then hard-stops past its thresholds; budget caps exist as
Claude's `--max-budget-usd` and Codex's `rollout_budget`. The same
convergence leaves interactive sessions without a turn limit
(Claude/Codex/Gemini default unlimited or none) while headless keeps a
hard cap — the TUI's interactive default is unlimited; the headless cap
rises to 100 alongside the context-pipeline work.

## The trailer's no-history choice costs a turn recompute on llama-server

Consumer: the phase-3 local-inference ADR.

ADR-0007's 2026-09-10 amendment keeps the status bar out of the message
history (right call under server-side request-prefix caching). On
llama-server's generation-extended KV cache, every request then
recomputes the previous assistant turn (~seconds of prefill per turn at
local speeds). If phase-3 measurements make that hurt, the revisit is a
transport-aware history policy, not a silent revert.
