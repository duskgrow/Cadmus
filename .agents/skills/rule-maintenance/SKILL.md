---
name: rule-maintenance
description: "How to update AGENTS.md and .agents/skills when the rules themselves change — when an agent mistake repeats, when tooling changes invalidate a command, when a rule goes stale, or when a repeated procedure deserves to become a skill. Instructions are software artifacts: versioned, reviewed, and co-evolving with code in the same PR."
---

# Rule maintenance: keeping agent instructions alive

Instruction files rot silently (context rot). This skill is the write-back
mechanism: mistakes and changes must flow back into permanent rules instead
of being re-corrected in chat every session.

## When to update (triggers)

- **A mistake repeats** — you (the agent) were corrected twice for the same
  thing, or a human had to re-explain something in a new session. That is a
  missing or wrong rule.
- **A rule goes stale** — it references a deleted file/command/dependency.
  Fix or delete it in the *same PR that caused the staleness*.
- **Tooling changes** — the justfile changed → sync the Commands section of
  AGENTS.md; a new generated file or a new workflow appeared → sync the NEVER
  section; a new skill is added → it is auto-discovered, AGENTS.md only needs
  its one-line listing.
- **A deep procedure repeats** — a multi-step, judgment-heavy workflow that
  has now been done twice → it graduates to a skill.

## The filter (apply before adding ANY rule)

From the empirical research on agent context files: their value is
*incremental information density*, not length. Ask: **"Would an agent make a
mistake without this rule?"** If no — do not add it; if an existing rule
fails this test — delete it. Never restate what code, config, or `just
--list` already says; never write codebase overviews (agents explore faster
than they read).

## How to write rules

- AGENTS.md stays lean — the always-on budget is ~200 lines; one line per
  rule where possible.
- Each rule carries its *reason* and, when one exists, its *enforcement
  pointer* (the lint / CI job / drift check that bites if violated). A rule
  without an enforcement point will rot — say so explicitly when accepting
  one.
- Keep the freshness comment at the top of AGENTS.md current (what each
  section tracks + last-reviewed date).

## Frontmatter discipline (why there is no `triggers:` field)

Progressive disclosure loads exactly `name` + `description` at level 1 —
**nothing else is guaranteed to be visible to the agent when it decides
whether to activate the skill**. The SKILL.md standard defines no `triggers`
field; adding one would be metadata no host reads. So the activation
condition ("when to use this") must live in `description`, and the detailed
trigger list stays in the body, where it is procedure content read after
activation. `just agent-check` rejects frontmatter keys outside the allowlist
(`name`, `description`, `license`, `allowed-tools`, `metadata`) — a field no
host reads is a silent lie.

Scalar discipline: hosts parse frontmatter with strict YAML, where a plain
(unquoted) scalar containing `: ` fails to load — the skill then silently
disappears from the host's skill list. Quote any value containing a colon
(2026-08: this exact bug took `pr-preflight` and `rule-maintenance` offline
while the pre-hardening `agent-check` passed; the checker now rejects plain
scalars a strict parser would misparse).

## How to write skills

- Layout: `.agents/skills/<name>/SKILL.md`; `name` must equal the directory
  name; YAML frontmatter requires exactly `name` and `description`.
- `description` (≤1024 chars) states **what it does and when to use it** —
  it is the only always-loaded part, so the activation decision depends on it.
- Body budget: < ~5k tokens. Heavy reference material goes into files under
  the skill directory and is loaded on demand (progressive disclosure).
- Division of labor: constraints → AGENTS.md; procedures → skills; reference
  knowledge → `docs/` (linked, never copied).

## Process

1. Instruction changes ride in the **same PR** as the code change that
   motivates them (co-evolution), or as a standalone PR when fixing rot.
2. After editing, run `just agent-check` (validates frontmatter, budgets and
   pointer integrity; part of `just ci`).
3. In the PR description, say which trigger fired (repeat mistake / stale
   rule / tooling change / new procedure) — this makes rule churn reviewable.
