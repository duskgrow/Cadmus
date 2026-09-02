---
name: doc-maintenance
description: Use when writing, updating, or reviewing documentation — README, CONTRIBUTING, docs/, AGENTS.md prose, and *.zh-CN.md translations. Encodes this repo's documentation discipline — the three-question filter for what prose is allowed to exist, English-canonical translation sync, same-PR co-evolution with code, and drift hunting.
---

# Documentation maintenance

Hand-written docs carry exactly three things: **intent** (why), **rationale**
(ADRs), and **entry points** (runnable commands). Everything else rots.

## The three-question filter (apply to every paragraph)

1. Can code/config state this fact? → delete the prose, or make it generated /
   referenced (version numbers, command lists, help text, parameter tables are
   never hand-copied).
2. Does it answer _why_? → it belongs in an ADR (`docs/decisions/`) or an
   explanation section — write it once, reference it elsewhere by number.
3. Is it an entry point? → write it as an executable command that CI or a
   doctest can keep honest.

If none applies, the paragraph is a future drift source — don't write it.

## Placement (Diátaxis quick map)

- `README.md` — portal: what/why one line, install, run, links. No parameter
  tables, no implementation details.
- `CONTRIBUTING.md` — how-to for the development process.
- `docs/decisions/` — ADRs; one decision per file, rationale only here.
- API reference — rustdoc comments next to the code; never a separate
  hand-maintained copy.

## Language sync

English is canonical; `*.zh-CN.md` files are translations.

- Which docs get translations: external-facing portals only (README,
  CONTRIBUTING). ADRs, AGENTS.md and skills are English-only — a translation
  serves its readers, and those docs' readers all read English.
- Update the English version **first**; the translation follows in the same PR.
- Every zh-CN file carries the cross-link header (`[English](./X.md)` +
  "以英文版为准"). Translation drift is a bug — flag or fix it when noticed.

## Co-evolution

Behavior, commands, flags, or config changed → the affected docs change in the
**same PR**. A doc-only PR is for rot fixes and translations. Never promise
documentation "in a follow-up".

## Drift hunting (run when asked to "check the docs")

- Grep the docs for commands/files that were renamed or deleted; every hit is
  a fix.
- Version numbers, counts, paths in prose: find their authoritative source; if
  none exists, delete or generate.
- AGENTS.md / skills touched → run `just agent-check`; docs build → `just doc`.
