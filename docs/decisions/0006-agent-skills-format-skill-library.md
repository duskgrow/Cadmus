# 0006. Agent Skills format as the skill-library base, evolution state external

- Status: accepted
- Date: 2026-09-03

## Context

Decided during the phase-1 kickoff discussion; binding primarily on phase 2
(the self-evolution loop), where the skill library lands. The Agent Skills
specification (verified 2026-09-03 at agentskills.io and
`anthropics/skills`): a skill is a folder with a required `SKILL.md` (YAML
frontmatter: `name` and `description` required; `license`, `compatibility`,
`metadata`, `allowed-tools` optional) plus optional `scripts/`,
`references/`, `assets/`; agents load it by progressive disclosure
(metadata → body → resources); a reference validator (`skills-ref`) exists.
The format originated at Anthropic, is released as an open standard, and is
adopted by a growing number of agent clients.

The report's phase-2 skill schema (§10.2.3: ReasoningBank's
title/description/content + ACE helpful/harmful counters + version chain)
predates this decision but maps cleanly onto the standard. Report §5.3
already routes skill-definition text assets through git.

## Decision

1. **A Cadmus skill is a standard Agent Skills folder.** ReasoningBank
   mapping: title → `name`, description → `description`, content → the
   markdown body. Folders are git-versioned text assets (report §5.3): the
   version chain **is** git history, a self-evolution delta is a commit, the
   gate is tests-before-commit, rollback is `git revert`.
2. **Operational evolution state lives outside the skill folder** — ACE
   helpful/harmful counters, gate history, provenance links to source
   traces — keyed by skill name+version in the store (ADR-0005's phase-2 SQL
   store). Skill folders stay standard-clean and portable across clients.
3. Content-level extensions use only the spec's `metadata` map with
   `cadmus.*`-namespaced keys. The spec's `allowed-tools` field is
   experimental — evaluated at phase 2, not assumed.
4. Retrieval design follows the spec's progressive disclosure:
   name+description is the always-loaded discovery unit and the
   embedding/retrieval target; the body loads on activation; bundled
   resources on demand.

## Consequences

- Interoperability: ecosystem skills import unchanged; Cadmus-evolved skills
  export to any compatible client. This repository's own `.agents/skills`
  already dogfoods the `SKILL.md` format.
- Phase-2 schema work shrinks to operational state plus gates; delta review
  becomes ordinary git diff review.
- `skills-ref validate` can be wired into CI as a mechanical gate on skill
  folders.
- The standard is young; a breaking spec change is the re-evaluation trigger
  (watch the spec repository), with exit cost bounded to the loader plus the
  `metadata` mapping layer.
