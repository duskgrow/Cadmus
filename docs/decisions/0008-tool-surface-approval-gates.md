# 0008. Built-in tool surface and approval gates

- Status: accepted
- Date: 2026-09-06

## Context

Phase 0 shipped a read-only toolset (`read_file` / `grep` / `list_dir`), and
no phase has since owned the write/edit surface: the report schedules
"controlled code execution" in phase 3 (§10.2.4), ADR-0002's crate list
mentioned phase-0 confirmation gates that never landed, and ADR-0004 closed
the rmcp question without touching the built-in surface. The tool surface is
the agent's task surface — it defines what eval set v1 can measure and what
skills phase 2 can learn — so its absence leaves phase-2's acceptance
criterion ("first skill item with positive gain") anchored to an undefined
surface. Identified in the 2026-09 design review.

Evidence base beyond the frozen report: ai-agent-book v2.0 (fetched
2026-09-06), chapters 4–5: the production coding-agent consensus minimal set
(read, grep, glob, write, edit, shell, code interpreter); the ACI principle
(tools map to agent goals, not API endpoints); parameter-fidelity incidents
(Cursor's silent quote conversion breaking edit matching; an IDE injecting a
flag into `git commit` breaking old git); the edit-tool consensus
(`old_string`→`new_string` exact match; apply-model and line-number editing
abandoned by their products); the execution-verification-feedback loop
(write followed by lint, structured errors returned to the model); and
risk-tiered approvals whose rejections are fed back as tool results.

## Decision

1. **Surface plan with phase ownership.** Phase 1: `write_file` +
   `edit_file` behind the approval gates of item 4 — landing before eval set
   v1 freezes, so v1 covers the actual coding task surface. Phase 3:
   `shell_exec` inside `sandbox-local` (report §7, unchanged). MCP: per
   ADR-0004's triggers, amended by item 6. The live tool list is the code
   (`crates/cadmus/src/tools/`); this ADR owns only the intent and the
   per-tool admission bar: one-line justification of goal, risk tier, and
   why a dedicated tool beats shell (book: dedicated grep/find earn their
   place via cross-platform consistency and line-number feedback even when a
   shell exists). Two admission criteria are hard requirements:
   cross-platform operation (Linux/macOS/Windows — CI's windows job enforces
   it mechanically) and model-agnostic design (the mainstream models' shared
   habits — exact-match edits, regex search, conservative JSON Schema; never
   a single-vendor DSL).
2. **Contract invariants on `AgentTool`** (port-level, contract-tested so
   fakes and built-ins are held to them): parameter fidelity — an
   implementation never silently transforms or injects parameters (when
   normalization is unavoidable it is declared in the description and echoed
   in the result); explicit truncation marking; structured, model-actionable
   errors; declared concurrency safety, defaulting to non-parallel
   (fail-safe). The loop executes batches of independent parallel-safe
   calls concurrently (all perception tools are parallel-safe);
   approval-gated and undeclared calls serialize, and a failure cascades
   only within its own batch (book ch5).
3. **`edit_file` semantics**: exact `old_string`→`new_string`; succeeds iff
   exactly one match exists (repeat-application fails safely, making edits
   near-idempotent); any fuzzy matching is a separate explicit mode, never a
   silent fallback.
4. **Approval gates land with the write tools.** The report's L0–L3 tiers
   (§7) live in core; a rejection is returned as a tool result so it enters
   the trajectory (reflection can learn from it, ADR-0005); an unanswered
   approval request times out to deny — human-in-the-loop always pairs a
   timeout with the conservative default. User escalation is a command event
   (ADR-0002), so local and future remote clients share one approval path.
5. **Execution-verification loop**: write/edit results merge fast
   deterministic feedback (parse/lint/typecheck where a checker exists) —
   completion means "checks pass", never "file written" (aligns with the
   report's externalized completion criterion, §2.5).
6. **MCP supplement to ADR-0004.** When a trigger fires: exposure is
   index/proxy-style (tool names + on-demand schemas; never bulk-inject
   definitions — the book notes five servers can cost tens of thousands of
   tokens per session); security baseline — server descriptions are
   untrusted input (audit against poisoning/shadowing), server versions are
   pinned, credentials are least-privilege per server. For _exposing_ cadmus
   capabilities, prefer the Pi pattern (capability = CLI + `SKILL.md`) over
   running an MCP server unless third-party clients must consume them.

## Consequences

- Eval set v1's task surface is read + write/edit scenarios; a scenario
  corpus built before the write tools land would measure a different product
  than the one phase 2 evolves.
- Deliberately never (book-verified negatives): a semantic-search index over
  the codebase (Claude Code deliberately builds none; Cursor built and
  abandoned one — grep/glob just-in-time retrieval wins at our scale);
  apply-model editing; event-trigger and
  asynchronous/proactive user-communication tool families (they presuppose
  the async runtime of book ch6, which we do not have) — synchronous
  in-band asks (`ask_user`, `exit_plan_mode`, docs/tools.md) are part of
  ADR-0011's interaction surface, not of this family.
- LSP-grade symbol tools are deferred, not excluded (maintainer,
  2026-09-06): they are eventually wanted. The trigger and the containment
  pattern (lazy start, per-action approval tier, global kill switch —
  oh-my-pi's containment of the lifecycle cost) live in docs/tools.md.
- Tool errors never terminate a run: they return as structured tool results
  (book ch5's failure taxonomy); loop termination stays with turn/budget
  limits and escalation.
- ADR-0004's triggers are unchanged; item 6 only pre-decides the adoption
  shape, so a future adoption PR starts from decided constraints instead of
  fresh debate.
- The catalog of included, planned and excluded tools lives in
  `docs/tools.md` (plan + admission record); this ADR owns the principles
  and contract invariants only.

## Amendment — 2026-09-09: multi-replacement edit_file, batch approval

From the phase-1 tool-surface rework's review discussions (maintainer,
2026-09-09):

- **Item 3**: `edit_file` carries an _array_ of exact
  `old_string`→`new_string` replacements per call, all targeting one
  file; each must match exactly once, and the call applies all or none —
  the batch is the unit, so the model never leaves a file half-edited,
  and one call per file avoids the multi-turn round trips that
  single-replacement editing would cost.
- **Item 4**: approval becomes _batch_ approval — a turn's gated calls
  are presented together and each is approved or rejected independently,
  with approved calls executing immediately; "approval-gated calls
  serialize" is superseded. The accepted trade-off: a rejection can
  depend on the batch's contents but never on an earlier gated call's
  _result_ (results postdate the approval moment).
- **Item 2, design principle recorded**: built-in tools are designed for
  parallel safety on purpose, so the `Concurrency` declaration on
  `AgentTool` records the analysis rather than excusing it; the serial
  default protects what we cannot vouch for (third-party wrappers,
  unanalyzed additions). The loop's schedule is all-or-nothing per turn
  batch — the model emits calls as one unordered batch, so intra-batch
  order carries no information worth a finer schedule.

## Amendment — 2026-09-09 (evening): per-call approval resolution, approve-and-execute-immediately

From the maintainer's review of the client-protocol implementation (same
day, refining the batch-approval amendment above). The batch amendment made
one `resolve_approval` command settle the whole batch before anything
executes. Interactive use wants finer granularity: decisions arrive per
call and possibly out of order — approve the third of five and it runs at
once; approve the second later and it runs then.

Decided (lands with the TUI, its interactive consumer; the in-process
auto-resolvers keep answering the whole batch at once, the degenerate
case):

- The batch is still **presented** together (the dialog shows a turn's
  gated calls as one batch — batch _contents_ inform every decision), but
  **settlement is per call**: `ResolveApproval` gains per-call addressing
  (an additive contract change), the gate keeps the request open until
  every gated call is decided, and each decision executes or denies its
  call on arrival. Each resolve command is recorded as its own command
  event (the audit granularity improves).
- Tool results still land in **call order** (ordered reassembly — the
  `dispatch_parallel` outcomes pattern), so the trajectory reads
  schedule-independent and the prompt prefix stays decision-order-stable.
- The batch amendment's accepted trade-off inverts: a decision may then
  observe a sibling call's _result_ (results no longer all postdate the
  approval moment). For an interactive user that is the point; the
  fail-safe direction is untouched — any call may still be denied at any
  time, undecided calls at the wait's timeout deny, and a closed command
  channel denies the remainder.
- Unchanged: the L0–L3 tiers, rejection-as-tool-result (denied calls enter
  the trajectory as `approval_rejected` feedback), and the batch
  presentation itself.
