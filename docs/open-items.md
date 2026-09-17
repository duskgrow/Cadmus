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
scope (once/turn/session/persisted), with the approval modes as presets
over the rule layer (ADR-0011's 2026-09-11 amendment item 3); one
decorator may serve N sessions as fleet policy (ADR-0015 item 5). The
design questions above remain for the config-layer implementation.

Direction added 2026-09-13 (ADR-0018 item 7): config files are TOML
data; the expression zone (computed rule conditions, hooks) is
Starlark's trigger, deferred until a real consumer lands — a
programmable-config platform is rejected as a pseudo-requirement at
single-user scale.

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

## The serde-derive tripwire covers config file types too

Consumer: cadmus-ui's theme loader (and later the TUI's settings/keymap
files).

`check_serialization_boundary` (crates/xtask/src/arch.rs) flags any
`Serialize`/`Deserialize` derive outside cadmus-contract, but ADR-0018
item 7 makes theme/settings/keymap files TOML data — local config, not
wire protocol. When the theme loader lands, resolve deliberately: scope
the check to actual wire boundaries, or parse `toml::Value` by hand (no
derives). The check's intent (wire types only in the contract,
ADR-0002) stands either way.

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

## The turn-end band collapse leaves the residue mid-page

Consumer: the layout-hardening slice (or the ratatui-bump decision below,
whichever lands first).

Forensics 2026-09-17 (vt100 rig, two-turn scripted session): the residue
mechanism is confirmed and bounded — at turn end the band shrinks from its
streaming height to composer+status, and the Δ vacated rows are the band's
own blank stream-viewport rows relocating into the page above it. On a full
screen that is one screen-height blank run per completed turn, sitting
between the flushed history and the band; the existing tests plus the
session verified the healthy parts (no phantom band rows, no missed shrink,
flush ordering intact), so the blank runs in field captures come from this
plus the CJK artifact, not from a pump defect.

Eliminating the residue is structural to the portable insert path (the
amendment's accepted cost): the shrink must move the band's top edge down
across rows that only ever held viewport blanks. The recorded options:
scroll-relocation (scroll_up(Δ) at the collapse so the blanks land at the
old page top instead of next to the band — auto-scroll at idle makes this a
quirk-matrix item, coupled to the scroll-while-streaming probes), holding
the settled tail in-band across the collapse (a flush-contract change), or
DEC row deletion (already rejected). Field severity decides; until then the
residue stays the accepted cost the amendment records.

## Settled approvals render no explicit record on attach

Consumer: the session-attach payload slice (which already owns the
fold/render seam this rides on).

The transcript rebuilds from `RunState.messages`, and command events —
including the recorded `ResolveApproval` — are not in that fold, so an
approval that settled before an attach shows its consequence (the tool
call, or the error result carrying the rejection) but no explicit
approve/reject line in the rebuilt transcript, unlike the live path's
`push_resolution`. The pending half is covered (an attach mid-wait
re-seeds the dialog queue and the names map). Closing it means either
folding command events into the attach payload's bounded window or
projecting resolutions into the history — the slice's own design
direction decides.

## The approval dialog cannot name its deadline

Consumer: the session-attach payload slice (it already extends the
pending-approval payloads).

The gate's human-wait timeout (five minutes, ADR-0008 item 4, decided
2026-09-17) exists only core-side: `ApprovalRequested` carries no
deadline, so the TUI's dialog cannot tell the waiting human their prompt
self-denies — no countdown, no hint; a timed-out request just vanishes
(cleared by its recorded resolution). Carry the deadline in
`ApprovalRequested` — attach clients reconstruct the same dialog — and
the header can name it statically; a live countdown rides the stream
later.

## The next ratatui bump moves the inline spike's accepted costs

Consumer: the first ratatui version bump (0.30.3 or later).

crates.io still serves 0.30.2 (2026-06-19) — the version the inline
spike measured — but upstream's inline-viewport area is converging on
Codex-class behavior fast (GitHub issue tracker, 2026-09-14):
merged-unreleased #2670 (breaking: no full-screen clear when an
inline viewport shrinks horizontally) and #2731 (skip the redundant
shrink clear), #2666 closed (the live viewport duplicating into
scrollback on resize under continuous draw + insert_before), #2527
in progress (wide-grapheme continuation cells in insert_before,
tagged v0.31.0). Field confirmation (2026-09-16): every flushed CJK
prompt row lands in scrollback with a spurious space per continuation
cell (`你好` becomes `你 好`, the tail shifting right) — the bug bites
on the ordinary path, no exotic setup needed, which raises the bump's
priority. Separately, same day: the first interactive session died on
`insert_before`'s closing `Terminal::clear` — its cursor-position query
stalled behind crossterm's parked event-reader thread (ratatui #2640's
mechanism, reproduced on a pty). cadmus-tui now answers all post-boot
cursor queries from tracked state (`src/cursor.rs`), so #2640 no longer
reaches the shell; the tracker's seed query is the session's only CPR. Two of the spike verdict's accepted costs live
exactly here: resize residue and the shrink clear+replay. On the
next bump, before merging: re-run the inline-spike harness matrix —
the shrink replay may flip from necessary to harmful (inserting rows
nothing lost) — plus the dynamic-height probe suite
(`tests/dynamic_height_spike.rs`), and re-check the ADR-0018
amendment's evidence lines. MSRV holds at 1.88 through 0.30.2.
