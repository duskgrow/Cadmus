# Tool catalog

The built-in tool surface: what exists, what is planned, what is excluded
and why. Principles and contract invariants live in ADR-0008, approval UX in
ADR-0011, the L0–L3 execution tiers in the frozen report §7.1.1; this file
is the plan and the admission record.

Rules for this file:

- The code (`crates/cadmus/src/tools.rs`) is the live list; this file is the
  plan. A new tool lands as a row here in the same PR, with the ADR-0008
  admission bar: goal, tier, why a dedicated tool beats shell — plus the two
  hard criteria: cross-platform (Linux/macOS/Windows; CI's windows job
  enforces it) and model-agnostic (mainstream models' shared habits;
  exact-match edits, regex search, conservative JSON Schema; never a
  single-vendor DSL). Tool changes are evidence-driven: add/remove/reshape
  needs a cited baseline or trace evidence, recorded in the row.
- The excluded table is as load-bearing as the included ones — every row is
  a decision not to build, with its revisit trigger.
- Comparison baseline (2026-09-06, re-verified per phase): Claude Code v2.1.x
  (45 built-ins), Codex CLI (~20), Gemini CLI (28), oh-my-pi (31);
  ai-agent-book v2.0 ch4–5.

## Tier model (report §7.1.1, mapped to pre-sandbox behavior)

Tiers are assigned _per invocation_, computed from the call's arguments —
not statically per tool (oh-my-pi's pattern): `shell_exec("ls")` and
`shell_exec("rm -rf ~")` are different tiers.

| Tier | Meaning (report §7.1.1)                                | Behavior                                                     |
| ---- | ------------------------------------------------------ | ------------------------------------------------------------ |
| L0   | workspace r/w, no network                              | free — safety net is checkpoint/rewind (ADR-0011), not gates |
| L1   | allowlisted network egress                             | free, audited (usage recorded as events)                     |
| L2   | outside allowlist, write outside workspace, `git push` | confirmation gate (ADR-0008/0011)                            |
| L3   | credentials paths, host sockets                        | hard deny, cannot be confirmed                               |

The user's approval _mode_ (ADR-0011) layers comfort on top of this
security floor: modes can ask more than the floor requires, never less.

## Perception

| Tool                       | Tier | Phase | Status  | Intent / design notes                                                                                                                                                                              | Serves         |
| -------------------------- | ---- | ----- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------- |
| `read_file`                | L0   | 0     | shipped | workspace-confined read, line windows, 512 KiB cap; the exact-resume-range truncation footer is the ADR-0007 layer-1 standard, retrofitted with the write tools                                    | —              |
| `grep`                     | L0   | 0     | shipped | structured `path:line` matches, capped count; dedicated despite shell (cross-platform consistency, book ch4); retrofit: regex support (the models' native habit; currently literal substring only) | —              |
| `list_dir`                 | L0   | 0     | shipped | one-level listing, capped entries                                                                                                                                                                  | —              |
| `glob`                     | L0   | 1     | planned | pattern file-find; the model-preferred slot over `list_dir` chains (first-class in all four baseline agents)                                                                                       | —              |
| scheme reads (`trace://`…) | L0   | 2     | planned | `read_file` resolves internal schemes — `trace://<id>`, `skill://<name>`, `memory://` (oh-my-pi's one-surface pattern; trace id → shard path is already a pure function, ADR-0005)                 | 0005/0006/0009 |

## Workspace mutation

| Tool         | Tier | Phase | Status  | Intent / design notes                                                                                                    | Serves |
| ------------ | ---- | ----- | ------- | ------------------------------------------------------------------------------------------------------------------------ | ------ |
| `write_file` | L0   | 1     | planned | create/overwrite inside the workspace; post-write verifier feedback merged into the result (ADR-0008 item 5)             | 0008   |
| `edit_file`  | L0   | 1     | planned | exact `old_string`→`new_string`, unique-match-or-fail, near-idempotent (ADR-0008 item 3); batch edits via parallel calls | 0008   |

## Execution

| Tool             | Tier             | Phase | Status    | Intent / design notes                                                                                                                                                                                     | Serves |
| ---------------- | ---------------- | ----- | --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| `shell_exec`     | per args (L0–L3) | 3     | planned   | persistent session default; sandboxed (report §7); head+tail truncation with spill file; per-invocation tier classification (report §7.1.1 channels; classifier design is phase-3 scope), deny-by-default | 0008   |
| background tasks | per args         | —     | candidate | start/poll/stop for long-running processes; trigger: first real need (dev server, watcher); natural fit with the daemon era                                                                               | 0011   |

## Interaction (user communication)

| Tool             | Tier | Phase | Status  | Intent / design notes                                                                             | Serves |
| ---------------- | ---- | ----- | ------- | ------------------------------------------------------------------------------------------------- | ------ |
| `ask_user`       | L0   | 1     | planned | structured choice questions (1–4 options); renders as a TUI dialog, the answer is a command event | 0011   |
| `exit_plan_mode` | L0   | 1     | planned | presents the plan file; approval exits into a chosen approval mode (ADR-0011 plan-mode flow)      | 0011   |

## Task tracking

| Tool         | Tier | Phase | Status  | Intent / design notes                                                                                                                   | Serves    |
| ------------ | ---- | ----- | ------- | --------------------------------------------------------------------------------------------------------------------------------------- | --------- |
| `todo_write` | L0   | 1     | planned | atomic whole-list replace; rendered in the ADR-0007 status bar; description carries oh-my-pi's rule: never the sole tool call of a turn | 0007/0011 |

## Skills and memory

| Tool            | Tier | Phase | Status  | Intent / design notes                                                                                                                      | Serves    |
| --------------- | ---- | ----- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------ | --------- |
| `skill`         | L0   | 1     | planned | activates a skill by name → body injected as the tool result (progressive disclosure L2; the L1 catalog lives in the frozen prefix)        | 0006/0007 |
| `memory_search` | L0   | 2     | planned | read-only search over the user-memory log once it outgrows the prefix cap                                                                  | 0009      |
| `remember`      | L0   | 2     | planned | proposal-only: enqueues a memory/skill delta for the human gate; writes nothing itself — the in-session half of ADR-0009's propose/approve | 0009/0010 |

## Web (candidates)

| Tool         | Tier  | Phase | Status    | Intent / design notes                                                                                                          | Serves |
| ------------ | ----- | ----- | --------- | ------------------------------------------------------------------------------------------------------------------------------ | ------ |
| `web_fetch`  | L1/L2 | —     | candidate | trigger: recurring docs-lookup need; prompt-guided lossy extraction (Claude Code's WebFetch shape), domain allowlist via proxy | —      |
| `web_search` | L1/L2 | —     | candidate | single provider, answer + citations; no provider-chain fan-out (oh-my-pi's 23-provider chain is a maintenance surface)         | —      |

## Deferred (trigger-gated)

| Tool       | Tier                        | Phase | Status   | Intent / design notes                                                                                                                                                                                                                  | Serves |
| ---------- | --------------------------- | ----- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| LSP family | L0 reads / per-action tiers | —     | deferred | eventually wanted (maintainer, 2026-09-06); adopt oh-my-pi's containment for the lifecycle cost: lazy start, per-action approval tier, global kill switch. Trigger: refactor-heavy work becomes daily after the daily-driver milestone | 0008   |

## Context isolation (trigger-gated)

| Tool   | Tier              | Phase | Status    | Intent / design notes                                                                                                                                                | Serves |
| ------ | ----------------- | ----- | --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------ |
| `task` | inherits parent's | —     | candidate | context-isolated subtask as a nested event stream returning a structured summary; trigger: bulky intermediate artifacts or independent-perspective review (ADR-0007) | 0007   |

## MCP (trigger-gated per ADR-0004)

| Tool      | Tier | Phase | Status    | Intent / design notes                                                                    | Serves    |
| --------- | ---- | ----- | --------- | ---------------------------------------------------------------------------------------- | --------- |
| mcp proxy | L2   | —     | triggered | index/proxy exposure (~200 tokens), server start deferred to first use (ADR-0008 item 6) | 0004/0008 |

## Excluded (deliberately not built)

| Capability | Reason | Revisit trigger |
| ---------- | ------ | --------------- |

| code interpreter / persistent kernel | shell covers it; kernel re-entrancy complexity buys nothing at our scale | real data-analysis need |
| browser / computer-use / image / TTS | outside coding scope (oh-my-pi itself defaults them off) | product scope change |
| notebook tools | no Jupyter workflow | — |
| batch-edit tool (MultiEdit-style) | parallel `edit_file` calls suffice; Claude Code retired MultiEdit | edit-failure loops visible in traces |
| hashline / patch-language edits | exact match is the mainstream consensus (Claude Code, Gemini); patch DSLs optimize for one model family | edit mismatch loops visible in traces |
| tool_search / lazy schema loading | worthwhile only past ~100 tools (book ch4); steady state here is ~13 | catalog grows past ~20 tools |
| essential/discoverable split | same scale argument (oh-my-pi needs it for 31 tools, we do not) | same trigger |
| cron / schedule / wakeup | presupposes the async runtime (book ch6) we do not have | remote daemon era |
| workflow / multi-agent orchestration | excluded project boundary (ADR-0002) | never (boundary) |
| direct memory-write tool | violates the human gate (ADR-0009); `remember` is proposal-only | never (principle) |
| repo-map | ADR-0011: just-in-time retrieval consensus; a stale map is worse than none | — |

## Shipped-tool retrofits

The phase-0 tools predate this catalog's standards; their rework is planned,
not ad-hoc. Each item lands in the same PR as the behavior change.

| Tool              | Retrofit                                                                                                                               | Evidence / standard                                             |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `grep`            | regex support (currently literal substring only — the mainstream models' native habit is ripgrep-style regex)                          | model-agnostic admission criterion (ADR-0008)                   |
| `grep`/`list_dir` | .gitignore-aware walking instead of the hardcoded Rust-centric skip list (`target`); hidden-file policy review                         | ripgrep/`rg --files` convention                                 |
| `read_file`       | exact-resume-range truncation footer on the byte-cap path (the line-window footer exists; the 512 KiB byte path has no resume pointer) | ADR-0007 layer-1 truncation standard                            |
| `read_file`       | description alignment: absolute paths inside the workspace are honored but undocumented; encoding behavior (UTF-8 only) made explicit  | book-ch4 description standard (boundaries and counter-examples) |
| all three         | descriptions brought up to this catalog's standard (when-to-use / when-not, parameter examples, cost notes)                            | tools.md disclosure standard                                    |

## Disclosure and description standard

- All built-ins are resident in the frozen prefix (steady state ≈ 13 tools;
  the ADR-0007 cache discipline forbids dynamic tool lists at this scale).
- Every description follows the book-ch4/oh-my-pi standard: when-to-use and
  when-_not_-to-use, boundaries with counter-examples, parameter examples,
  cost notes, and hard NEVER rules (e.g. once `shell_exec` exists: never use
  it for grep/find/cat — dedicated tools win on feedback quality).
- Errors are corrections, not failures (ADR-0008): non-zero exits return
  `isError` results with next-step guidance; out-of-range reads reply with
  the exact window to retry, never a bare error.
- Truncation follows ADR-0007 layer 1 everywhere: dual cap (lines + bytes),
  spill to an artifact file, and a footer naming the precise resume range —
  silent truncation is a defect.
