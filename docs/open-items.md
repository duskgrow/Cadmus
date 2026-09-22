# Open items

Field findings waiting for their consuming change — an inbox, not a
backlog. Every item names its consumer; when the consuming change lands,
delete the item (its rationale then lives in that change's ADR). An item
with no consumer does not belong here.

## The quiesce/paste seams are built ahead of their consumers

Consumer: the `$EDITOR` handoff (Ctrl-G) and the input broker's
paste-burst classification — the ADR-0018 items 1/5/6 wiring change.

Found by the 2026-09 simplification sweep: `InputBroker::quiesce` (incl.
the `EventSource::Quiesced` GAT), `Quiesced::discard_buffered_input`, and
the composer's `PasteBurst` classifier have no production call sites —
their docs name the consumers, which do not exist yet. Kept because
ADR-0018 designates them and Phase 1's interaction row owns the wiring,
but ADR-0018's own rule is "none are built ahead of evidence": when the
handoff lands, wire them; if the handoff's shape changes, cut them there.

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
attaches to live runs carrying real history, and to long finished
sessions — and the ADR-0014 ACP adapter's `session/load`.

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
8. Scope sizing evidence (12-factor-agents factor 10, fetched
   2026-09-22): LLM reliability degrades with step count — the cited
   working envelope is ~3–10 steps (20 max) per agent — so a child run
   should own one deliverable, not an open-ended subtask. The same
   text's answer to "what if LLMs get smarter" — grow an agent's slice
   only behind measured quality — is ADR-0010's gate discipline
   restated, and phase-2's reflect→delta→gate→merge pipeline should
   hold its LLM steps to the same envelope.

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

Direction set 2026-09-11: rules gain matching over tool input and grant
scope, with the approval modes as presets over the rule layer (one
decorator may serve N sessions as fleet policy, ADR-0015 item 5). The
scope lattice is ADR-0011's 2026-09-19 amendment (once / session /
persisted, the last one located by settings-precedence level); the design
questions above remain for the config-layer implementation.

Merge-rule questions raised 2026-09-19, for the same consumer (they bind
the rule engine, so they live here until the config slice answers them):

1. **Store shape.** A keyed map per origin (`(tool glob, input glob) ->
   decision`) makes two rules unable to conflict and re-granting idempotent;
   an ordered list keeps explicit precedence but admits duplicates and
   order-dependent behavior. The engine's evaluation is already
   order-sensitive (first match wins), so the store must either fix a
   canonical order or hand the engine a documented one.
2. **May a narrower origin widen a broader one?** Straight
   specificity-wins (narrow overrides broad, mirroring ADR-0012's settings
   precedence) is simple, but the workspace origin is attacker-controlled
   data — a cloned repository's rule file could then grant itself tool
   permissions the user's own config denies. The alternative is monotone
   narrowing: a workspace rule may add `ask` / `deny` but never an `allow`
   the user level does not already permit, an in-session human grant is the
   only path that relaxes a stored `ask`, and nothing relaxes a stored
   `deny` (the engine never prompts for a `Deny`, so there is no UI path to
   click — deny is absorbing by construction).
3. **Session grants versus stored rules.** Recommended: session grants are
   consulted first (they are the most recent human decision) and may relax a
   stored `ask`, never a stored `deny`.
4. **Workspace identity.** A `project`-located grant needs a stable key for
   "this project" — the identity the trace attributes also lack (the "traces
   carry no workspace or ruler identity" item above), so that decision now
   has two consumers and is no longer phase-2-only.

Direction added 2026-09-13 (ADR-0018 item 7): config files are TOML
data; the expression zone (computed rule conditions, hooks) is
Starlark's trigger, deferred until a real consumer lands — a
programmable-config platform is rejected as a pseudo-requirement at
single-user scale.

## Config discoverability: the self-describing `cadmus config` subcommand

Consumer: the second settings.toml consumer's slice (today that is the
scoped approval rules' persistence above — it needs "did my rule take
effect?" answers on day one).

Direction (maintainer, 2026-09-22): discovery lives in the CLI. The
schema in `cadmus-tui::config` stays the SSOT; a `cadmus config`
subcommand projects it — `path` (the layer files, which exist, which
served), `show` (effective values with their winning layer), and a
keys/defaults listing with one-line descriptions. Strict-file errors
already list the valid keys on a typo; with a one-key surface that
suffices, so nothing lands now. A README "Configuration" section rides
the same slice (external-facing: the zh-CN translation syncs with it).
The future GUI (ADR-0012's 2026-09-08 amendment) adds a settings page
over the same schema — the projection splits, the SSOT does not. Not
the story: editor schema integration (taplo) — speculative at this
surface size.

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

12-factor-agents factor 9 (humanlayer/12-factor-agents, fetched
2026-09-22) adds the numbered discipline for the error side: the base
self-healing pattern (append the formatted error, continue) is already
ADR-0008's "tool errors never terminate a run"; on top of it, track a
per-tool consecutive-error counter and break at ~3 attempts — reset
part of the rendered context or escalate to a human (the loop-health
break path above). Spin-out mitigation: never feed a repeated error
back verbatim — restructure how it is represented in context (the
render-time seam of the item below); the text's own "number one
prevention" is small, focused agents (the persona item's scope
evidence).

## Resolved errors can leave the model's view without leaving the log

Consumer: the ADR-0007 context-pipeline implementation (the fold/render
machinery).

12-factor-agents factors 3+9 (fetched 2026-09-22): once an error is
resolved, dropping the failed call and its error from the _rendered_
context raises information density and removes a repeat-offense
attractor, while the full-fidelity record stays in the JSONL log
(ADR-0005) so replay and the reflector are unaffected. The render-time
fold-directive seam already makes this possible without touching
history; it is a candidate policy to validate against trace evidence,
not a decided behavior — a resolved error can also be signal the model
still needs mid-task.

## Escalation is a structured intent, not a plaintext reply

Consumer: the loop-health mechanism's break path above, and the
`ask_user` interaction surface (ADR-0011 floor) when it lands.

12-factor-agents factor 7 (fetched 2026-09-22): human contact as a
first-class structured intent — `request_human_input` carrying
urgency (low/medium/high) and answer format (free_text / yes_no /
multiple_choice) — emitted by the model or raised deterministically by
stuck detection; the loop breaks on the intent and resumes on the
answer event instead of holding an in-memory wait. Synchronous and
in-band, so it stays inside ADR-0008's exclusion of async/proactive
communication families; the factor's always-JSON experiment (never the
plaintext-vs-tool-call first-token gamble) is noted, not adopted — our
finish line stays "an assistant turn without tool calls" (ADR-0005).

## Agent UI/UX landscape survey (2026-09-11)

Consumer: the GUI ADR — scheduled (maintainer, 2026-09-11) for right
after the daily-driver milestone, no longer post-phase-5. Remaining for
it: the tech re-anchoring (report §5
— `gpui` is Apache-2.0 but its crates.io line stopped at 0.2.2 (2025-10)
and the git line's dependency tree is heavy (maintainer-verified); §5.3
options table) and the orchestration/review patterns of §2.2 as design
input. The design language is no longer its scope: consumed by
ADR-0017 (2026-09-13, Linear-structured tokens, one theme SSOT,
renderer-level degradation), which the GUI ADR inherits whole. The
other consumers landed 2026-09-11: floor re-baseline → ADR-0011
amendment, ACP → ADR-0014, orchestration layer → ADR-0015. Delete once
the GUI ADR lands.

`docs/research/2026-09-11-agent-uiux-landscape.md` — five-track survey
(agent TUIs, agent GUIs, Rust terminal-style GUI tech, orchestrator↔agent
seam mechanics, gap/issue hunting).

## The trailer's no-history choice costs a turn recompute on llama-server

Consumer: the phase-3 local-inference ADR.

ADR-0007's 2026-09-10 amendment keeps the status bar out of the message
history (right call under server-side request-prefix caching). On
llama-server's generation-extended KV cache, every request then
recomputes the previous assistant turn (~seconds of prefill per turn at
local speeds). If phase-3 measurements make that hurt, the revisit is a
transport-aware history policy, not a silent revert.

## Scroll-while-streaming interaction policy

Consumer: the in-app transcript fallback slice (mouse-less contexts) —
and, evidence-gated, the pause/batch behaviors below.

Maintainer-raised surface (2026-09-14). Decided 2026-09-16 (the app-wiring
change): **mouse capture stays off** — no widget clicks exist yet, and
native scroll/select/copy is the benefit ADR-0012 item 2 chose inline
rendering for. Resize reflow resetting the reading position is accepted
(debounce-bounded). Still open: an in-app transcript fallback for
mouse-less contexts (Codex's verified answer is a temporary alt-screen
transcript view, already permitted by ADR-0012's modal-sub-app exception);
the per-terminal quirk-matrix probes (the scroll anchor when rows insert
while scrolled up, scroll-to-bottom-on-keypress while composing,
selection/copy drift under a moving stream); and the evidence-gated
behaviors — pause or batch inserts while the user is scrolled up, only if
the matrix shows bottom-anchored terminals in the support set.

## Reassess history ownership on the next ratatui bump

Consumer: the first ratatui dependency upgrade after the history-write port.

ADR-0018's 2026-09-21 amendment removes `insert_before` from production and
uses stock Fixed after boot. Upstream #2527/#2640 and the inline shrink
clear changes therefore no longer repair our active write path directly.
On the next bump, rerun `history_insert`, `dynamic_height_spike` and the
native inline-spike matrix; only then consider returning geometry or
insertion to upstream. A fixed upstream writer alone is not evidence that
its resize/ack behavior satisfies the shell's contract.

## The typewriter drain emits in arrival-sized bursts, not a smooth rhythm

Consumer: a pacing-refinement pass (no driver yet — accepted as later
optimization by the maintainer, 2026-09-20).

Field note from the first real sessions with the paced drain: the emission
FEELS like chunks, not a typewriter. The mechanism is structural, not a
constant to tune: prose rows become stable in paragraph-sized bursts (a
paragraph is unstable until it closes), so the queue alternates between
burst arrivals and empty stretches, and the drain visibly follows that
rhythm even at 1–2 rows per 33 ms tick. Ideas for the pass, cheapest
first: a slower base rate that lets a backlog smooth the rhythm across
bursts (at the cost of display lag), sub-paragraph stability for prose
(a wrap-stability proof, not a heuristic), character-grain pacing for the
current row. Decide with a real-terminal A/B, not the vt100 rig.

The history-port review also found a pre-existing scaling cost for this
consumer: `Transcript::queued_len` scans every emission on each paced
pump. Draining many one-row emissions is quadratic in emission count.
Measure backlog-heavy turns in the refinement pass and maintain the
pending-row total incrementally if the queue remains emission-based.

## The floor's session-cost field has no pricing source

Consumer: the provider-metadata work — trigger-gated on a real consumer
(the floor's cost field, a `/usage` cost view; ADR-0011 item 3).

ADR-0011's floor is model, cwd+git, context-usage %, session cost. The
usage ratio ships: the window is the registry's declared
`Capabilities::max_context` (crates/cadmus-contract/src/capabilities.rs).
Cost does not: nothing in the tree carries pricing — `Capabilities`
declares limits, `Usage` counts tokens, and neither the registry
dialects nor the wire carry rates. Do not invent a model→price lookup
in the frontend; the consumer lands pricing as provider metadata and
the floor renders it then.
