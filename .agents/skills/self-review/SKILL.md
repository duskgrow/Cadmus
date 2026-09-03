---
name: self-review
description: Use after implementing a change and before committing or staging it — a fast judgment-layer pass over your own working diff. The mechanical sweep (diff hygiene, size) is yours; the judgment pass itself is delegated to a fresh subagent, because authors are structurally blind to their own assumptions. For the PR gate, or for reviewing someone else's PR, use pr-preflight instead.
---

# Self-review (before the commit exists)

The machine gates check syntax, lint, types and tests. This pass catches what
they cannot, while the fix is still cheap — but the author cannot run the
judgment part alone: the assumptions that wrote the code would also review
it. The split of labor:

- **You (the author)**: the mechanical sweep (§0), size sanity (§4), then
  triage of the reviewer's findings.
- **A fresh subagent**: the judgment pass (§1–§3). It gets the diff and the
  design context, not your reasoning — never pre-argue the change in the
  prompt beyond neutral pointers, or you export your own blind spots.

Stay fast; depth belongs to `pr-preflight`.

## 0. Read the whole diff (author, mechanical)

`git diff` + `git diff --cached` — every hunk must be intentional and part of
_this_ change. Remove drive-by edits, debug leftovers, commented-out code.
One clause that pays for itself: no tokens, keys or `.env` contents — catching
a secret here beats rotating it later.

## 1. Spawn the reviewer — the five axes are its checklist

Delegate to a subagent. Its prompt carries: the diff (or commit range), the
task/ADR context to ground in, and this checklist. It returns findings
labeled by §3; you fix or push back.

1. **Correctness** — does it do what the task asked? Edge cases and error
   paths, not just the happy path.
2. **Simplicity** — could it be fewer lines? Is every abstraction earning its
   complexity? (Deep cleanup is its own change: the `code-simplification`
   skill.)
3. **Architecture** — fits the existing layers? Logic in the module that owns
   the concept, not bolted onto an unrelated flow?
4. **Security** — no `unsafe`; a new third-party dependency must have gone
   through `adding-dependencies`; external data validated at the boundary.
5. **Performance** — only obvious hot-path waste; micro-tuning is out of scope
   here.

## 2. Tests first, then verify your verification

- Tests reveal intent: does the change ship with behavior tests at the lowest
  layer that can catch the bug? Would they actually fail if the code
  regressed? The reviewer checks test power by mutation thinking — which
  single deletion would keep the suite green?
- What did _you_ actually run? Never report green without the run — the
  Validation discipline in AGENTS.md applies to commit-time claims too.

## 3. Label findings by severity

| Prefix               | Meaning           | Before committing                               |
| -------------------- | ----------------- | ----------------------------------------------- |
| Critical             | broken / insecure | must fix                                        |
| Required (no prefix) | must address      | fix now                                         |
| Nit / Optional       | taste             | your call — never let nits delay a sound commit |

The reviewer labels; you triage. Order fixes by leverage: correctness first,
structure next, cosmetics last. A few high-conviction findings beat a long
list — one structural problem outweighs ten nits. Pushing back on a finding
is legitimate; record the reasoning in the PR.

## 4. Size sanity (author, mechanical)

One logical change per commit: ~100 lines is ideal, ~300 is acceptable when
single-purpose, ~1000 means split (stacked, by layer, or by file group).
Refactor and feature are two commits — cleanup always rides separately.

## Rationalizations that fail this review

| Rationalization                     | Reality                                                                                 |
| ----------------------------------- | --------------------------------------------------------------------------------------- |
| "The tests pass"                    | Tests can't see architecture, readability or missing edge cases.                        |
| "It's only a small change"          | Judge the resulting structure, not the diff size.                                       |
| "I'll clean it up later"            | Later never comes — the commit is the quality gate.                                     |
| "I wrote it, I know it's right"     | Authors are blind to their own assumptions — exactly why §1 is delegated, not self-run. |
| "A clean review is wasted overhead" | A second pair of eyes finding nothing is the desired outcome, not waste.                |
