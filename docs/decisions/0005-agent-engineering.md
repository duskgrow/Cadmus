# 0005. Agent engineering: skills layer, judgment-layer gates, self-maintaining rules

- Status: accepted
- Date: 2026-08-29

## Context

AGENTS.md alone is necessary but not sufficient. The evidence and the exemplars:

- The ETH study shows always-on context files pay a fixed token cost every session, and only *incremental* information helps — so the always-on file must stay lean while total knowledge stays unbounded. The layered-instruction answer (AGENTS.md → path-scoped rules → skills → docs) is now an open standard: SKILL.md (Anthropic, 2025-12) is adopted by 26+ tools, with `.agents/skills/` as the cross-tool location (Next.js uses exactly this layout and lists its skills in AGENTS.md).
- Machine gates (just ci) check what machines can check. What remains — doc co-evolution, snapshot-diff intent, semver-correct commit types, ADR necessity, dependency necessity — is judgment. Airflow encodes this as an explicit pre-push self-review checklist plus a path-scoped code-review instruction; we want the same judgment layer available to both agents and humans, on demand rather than always loaded.
- Instruction files rot (context rot) unless there is a write-back mechanism; dsh/Next.js (`$authoring-skills`) and Claude Code's `#` shortcut all encode "repeated mistakes become permanent rules". dsh additionally smoke-tests that its agent harness actually loads — an unloaded skill is a silent failure.
- Agents need hard boundaries that instruction files can't provide (an AGENTS.md "never" is a request, not a boundary): approval gating is policy, branch protection / scoped credentials are enforcement.

## Decision

- **`.agents/skills/` with the SKILL.md standard**, three starter skills that encode judgment-layer procedures: `pr-preflight` (pre-PR review, usable for reviewing others' PRs too), `adding-dependencies` (judgment + ASK discipline around cargo-deny's mechanical checks), `rule-maintenance` (the write-back mechanism: triggers, the "would an agent err without this?" filter, budgets, same-PR co-evolution).
- **`just agent-check`** (the `agent-check` subcommand of `crates/xtask`, in `just ci`) smoke-validates the agent surface: SKILL.md frontmatter shape, name/dir equality, description and body budgets, AGENTS.md line budget and freshness note, CLAUDE.md as a pure `@AGENTS.md` reference, and `.claude/skills` symlink integrity. Rationale: dsh's "can the host load it" as a CI smoke test.
- **Division of labor**: constraints → AGENTS.md; procedures → skills; reference knowledge → docs/. AGENTS.md gains a three-line skill listing (for hosts without auto-discovery) and explicit agent boundaries (never merge / tag / publish; no attribution footers; no force-push without asking).
- **PR template** gains an agent-doc co-evolution checkbox and an AI-co-authorship disclosure (Airflow's Gen-AI disclosure pattern).
- Not adopted: dsh-style plugin architecture and session-log SSOT (that's an agent *runtime*, out of scope for a project template); Zed's xtask-generated CI YAML (recorded as a possible third stage in ADR-0002's lineage).

## Consequences

- Agents do what machines can't (judgment rounds), machines do what they can (gates); the PR gate stays with humans.
- Rule churn is reviewable: the rule-maintenance skill requires naming the trigger in the PR description.
- Every generated project inherits the same agent surface; the smoke check keeps it loadable as the project evolves.
- Cost: one more directory of Markdown to keep truthful, guarded by `just agent-check` and the freshness discipline.
