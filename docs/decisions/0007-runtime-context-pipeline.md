# 0007. Runtime context pipeline: frozen prefix, code-maintained status bar, layered compaction

- Status: accepted
- Date: 2026-09-06

## Context

The loop assembles context minimally today: no system prompt (`AgentLoop`
sends the user prompt directly), no project-instruction loading, no
compaction — `ModelError::ContextLength` is modeled but has no designed
response. Meanwhile ADR-0005's log keeps full request/response text forever,
so the _recorded_ context is complete; what is missing is the _runtime_
context discipline. This is one of the agent-surface gaps identified in the
2026-09 design review (the roadmap plans assets, not agent capabilities).

Evidence base beyond the frozen report: 《深入理解 AI Agent》 (ai-agent-book
v2.0, repo main fetched 2026-09-06; product behaviors are cited at that
baseline and are re-verified per the report's freshness policy before
implementation), chapter 2, whose controlled experiments and production
teardowns establish:

- Prompt-cache economics make a byte-frozen prefix a hard constraint: a
  one-byte change invalidates the cache from that point on, cache reads cost
  ~1/10 of first compute, and one timestamp-in-prefix incident doubled a
  monthly bill. Every binary condition before the cache boundary doubles the
  cache key space.
- Uncontrolled tool output is the dominant context consumer (book experiment:
  7 read calls ≈ 367k chars, overflowing a 128k window by turn 5).
- A code-maintained status bar appended at the _end_ of context lifts
  small-model accuracy toward frontier models and cuts per-turn
  thinking/latency/cost by roughly an order of magnitude. The LLM trusts it
  unconditionally — so it must never be LLM-computed (poisoning surface).
- Sliding-window truncation is proven harmful (lost tool results cause
  repeat-call loops); adaptive batch compression at ~80% window usage
  outperforms both no-compression and per-call summarization.

## Decision

1. **Three-segment context assembly.** (a) Frozen prefix: SOP-style system
   prompt + built-in tool definitions + workspace instruction files (the
   `AGENTS.md` chain) + the approved memory card (ADR-0009) + the skill
   catalog (ADR-0006), byte-frozen for the whole run. MCP tools are not in
   the prefix: adoption is trigger-gated (ADR-0004) and, per ADR-0008 item
   6, enters as an index/proxy tool whose on-demand schemas append past the
   cache boundary. Instruction-file loading follows the agents.md spec
   (deliberately minimal plain Markdown; precedence: the file nearest the
   edited file wins, user prompts override everything) via the convergent
   product pattern — user-global plus ancestors root→cwd concatenated into
   the prefix, nested files injected on demand when the agent touches their
   subtree (Claude Code/Codex/Gemini CLI, 2026-09 baseline). The prefix
   hash is recorded in `start_run` attributes: it is the comparability key
   for ADR-0010's pairing — scores group only within one prompt build, and
   a prompt change is itself a candidate change. (b) Conversation events.
   (c) Dynamic trailer: an agent
   status bar as the final user message, maintained by deterministic code
   only (cwd, git branch/dirty state, per-tool call counters) — those fields
   are never computed or bulk-edited by the LLM. The one exception is the
   TODO list: model-authored content, but stored and rendered verbatim by
   code (`todo_write`, docs/tools.md), never re-computed or summarized by
   the model.
2. **Layered compaction, cheapest first.** Layer 1 (lands with the write
   tools of ADR-0008): mechanical only — oversized tool outputs are
   truncated head+tail with the full text spilled to a file plus a pointer;
   pure noise is dropped outright; every truncation is explicit
   ("showing lines 1-200 of 5000, use offset to continue") because silent
   truncation lets the model reason over partial data. The trigger is
   adaptive: estimated usage > 80% of `Capabilities.max_context` folds all
   uncompressed tool results in one batch, marked `[COMPRESSED]` so results
   are never folded twice. The fold targets tool results only — the bulk of
   a coding session's context and the only segment re-obtainable from the
   spill files — and one batch means one cache-invalidation event per
   compaction instead of one per folded call; the book's controlled
   experiment puts the token saving at 75%+ versus no compression, and ~1/10
   cache-read pricing makes the single rebuild cheap by comparison. The 80%
   threshold is the tunable: earlier folding saves more tokens, later
   folding preserves more verbatim fidelity. Layer 2 (deferred; trigger:
   `ContextLength` observed in real traces): archival per-turn LLM summary —
   the oldest turns are summarized into an append-only archive while recent
   turns stay verbatim — with a circuit
   breaker (3 consecutive failures disables compaction for the run). A
   sliding window is never used.
3. **Compaction never rewrites history.** The JSONL log stays the
   full-fidelity SSOT (ADR-0005); folding happens at request-render time,
   and each compaction decision is itself appended as an event: a fold
   directive references the ids of the events it folds plus its spill
   artifacts, and the renderer substitutes the folded placeholder from that
   directive onward — log and directives live in the same per-trace file,
   and replay walks both. Replay therefore reconstructs both the true
   history and the exact context the model saw — the reflector's
   full-trajectory input (report §2.5) is unaffected by runtime folding.
4. **Cache discipline.** Nothing per-request-dynamic in the prefix: no
   timestamps, no retrieved few-shot rotation, no dynamic tool ordering;
   everything time-varying lives in the trailer. Few-shot examples, when
   used, are 2–3 byte-stable boundary cases.

## Amendment — 2026-09-10: trailer outside history; fold cadence and the ceiling rule

Implementation discussion (maintainer, 2026-09-09/10) with a same-day
baseline re-check (the agents.md and agentskills.io specs; Claude Code,
Codex CLI and Gemini CLI docs/source; Anthropic's context-editing docs):

1. **The trailer renders outside history.** Item 1(c)'s status bar is
   appended at request-render time and never enters the message history:
   under server-side request-prefix caching, accumulating it buys nothing
   (generated output is not part of the cache key on today's server APIs)
   while polluting the context with contradictory stale bars. The known
   cost is phase 3's: llama-server's generation-extended KV cache then
   recomputes the previous assistant turn per request (open item). Exact
   replay is preserved by recording the rendered trailer on the per-turn
   `LlmRequest` event (additive, ADR-0005's rule). Git state enters the
   trailer as bounded scalars only — branch + dirty count, never a file
   list; enumeration is one tool call away if the model wants it.
2. **Fold is periodic hygiene, not a threshold response.** Item 2's single
   80% trigger is replaced: a fold runs every Δ estimated tokens of growth
   (default Δ = min(100k, 10% of `max_context`)) and folds tool results
   older than the last X turns (default X = 5; Anthropic's context-editing
   `keep: 3` is the mainstream anchor). The post-fold size triggers nothing
   — the run continues regardless. (Layer-1's placement note also slips:
   the write-tools PR shipped only the per-tool truncation standard; the
   fold machinery lands with the context-pipeline implementation.) Δ and X
   are tunables to validate against trace evidence — the cadence trades
   extra cache invalidations for attention hygiene, and the book's
   batch-at-80% experiment is the counter-evidence to watch.
3. **The ceiling rule.** At the hard line — estimated usage ≥ 80% of
   `max_context`, the window ceiling rather than an attention threshold —
   fold first if foldable content remains; LLM compaction fires only when
   nothing is foldable or the context is still over the line afterwards.
   Until the phase-2 compactor exists, that case surfaces as the provider's
   `ContextLength` error (the pre-existing path); the compactor's trigger
   is re-anchored from "ContextLength observed in traces" to this rule
   (open item). The ceiling is a tunable, not an attention optimum —
   mainstream anchors fire earlier (Gemini CLI compresses at 0.5 by
   default; Anthropic's context editing clears tool results at 100k input
   tokens).
4. **Estimation.** Provider-reported input tokens when available, a
   chars/4 heuristic otherwise; the estimate rides every fold/compaction
   decision event.
5. **Prefix details.** Instruction-file loading recognizes only
   `AGENTS.md` (no vendor-specific fallbacks — the
   universal-standards-first rule); the user-global file lives at the XDG
   config path; a nested file is injected as a standalone user message when
   the run first touches its subtree, never appended to a tool result. The
   SOP system prompt never names individual tools — tool-specific guidance
   rides each tool's own `description`, so disabling a tool removes its
   rules with its schema; the prompt keeps an explicit identity slot for
   the persona/profile layer (open item).

## Consequences

- Phase placement: segment assembly + status bar + layer-1 compaction land
  in phase 1 — they define the context every eval-v1 scenario runs under, so
  they must precede the eval set, not follow it.
- The prefix hash joins provider/model (ADR-0005's run attributes) as eval
  provenance: runs with different prefix hashes are not comparable under
  ADR-0010's pairing discipline.
- Deliberately not done (book-verified negatives): provider-side context
  editing / API-layer micro-compaction (forks the server-side view from our
  replayable log); tool-search / lazy schema loading (worthwhile only past
  ~100 tools; we have 3–5); context-isolation sub-agents for now (no
  bulky-artifact workload yet — the event model's nested-span seam already
  allows them when the trigger fires).
