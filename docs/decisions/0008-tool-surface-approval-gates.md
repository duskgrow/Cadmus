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
