---
name: pr-preflight
description: "Run this before opening or updating a PR, and when reviewing someone else's PR. The machine gates (just ci) are necessary but not sufficient — this skill is the judgment layer on top: diff self-review, doc co-evolution, snapshot intent, semver-correct commit types, ADR necessity, and a security pass."
---

# PR preflight

Machines check what machines can check. This checklist is the rest — the
judgment layer that decides whether a change is actually ready to be merged.
Run it before opening/updating a PR, and use it as the review frame when
reviewing another person's (or another agent's) PR.

## 0. Machine gates first (never negotiate with them)

```bash
just ci                      # lint (clippy -D warnings + actionlint), tests, docs, deny, agent-check
prek run --all-files         # same hooks as the commit-time gate
```

Both must be green. Never suppress, skip, or "fix later" a failure. If a gate
is wrong, fix the gate in its own PR.

## 1. Diff self-review

Each commit should already have passed the `self-review` skill (the code axes);
this section is the process gate over the assembled PR.

- `git diff <base>...HEAD` — read the _entire_ diff. Every hunk must be
  intentional and related to the task; remove drive-by edits, debug leftovers,
  commented-out code.
- Scan for secrets: tokens, private keys, `.env` contents, hardcoded
  credentials — in code, tests, snapshots, and docs. A leaked secret requires
  rotation, not just deletion.
- Change-size sanity: if the diff is large, could it be split into reviewable
  stages? Recommend the split instead of forcing a giant PR.

## 2. Docs co-evolution (same PR, not a follow-up)

- Behavior, commands, flags, or config changed → README / CONTRIBUTING /
  AGENTS.md / snapshots updated **in this PR**? The English version is
  canonical; `*.zh-CN.md` translations follow it.
- New hand-written prose must pass the three-question filter: can code state
  it (delete or generate)? Does it answer _why_ (ADR/explanation)? Is it an
  entry point (runnable command)? Anything else is a future drift source.

## 3. Snapshots

- Read every `.snap` diff. Each change must be an _intended_ consequence of
  this PR — not noise, not an accident of environment. Nondeterministic
  fields (timestamps, paths, UUIDs) must be filtered before snapshotting.
- Snapshots are approved by humans via `just snapshot-review`; the agent
  prepares and explains the diff, the human accepts.

## 4. Commits are version intent

- The modified Conventional Commits convention (`type(scope): subject`, ASCII
  subject, ≤100-char header — see CONTRIBUTING.md) must match real semver
  impact: user-facing feature → `feat`; bug fix → `fix`; breaking → `!` /
  `BREAKING CHANGE:` footer. Changelog entries are generated from these
  messages — a wrong type is a wrong release.
- Merge strategy is squash-only, landing **the PR title as the commit header
  and the PR body as the commit body**; CI checks both. Help the human get the
  title right; the body may be any language, the title may not. Body hygiene:
  delete the template's HTML comments before opening (CI rejects them); at
  merge time, cut everything below the `---` line (checklist etc.) in the
  merge dialog.
- PR messages are authored on GitHub and never pass the local commit-msg
  hook — CI is otherwise the first gate that sees them. Before opening or
  editing a PR, run the assembled message through the checker locally:
  `printf '%s\n\n%s\n' "$TITLE" "$BODY" | cargo run -q -p xtask -- check-commit -`.
  Structure is judgment on top of that: a one-two-sentence what/why up front,
  then short paragraphs or lists — no wall of text (check-commit rejects
  prose paragraphs over 7 lines; readable beats barely-legal).
- No agent-attribution footers (`Co-Authored-By`, `Generated-with`) in
  commits or PRs — see AGENTS.md.

## 5. ADR necessity (judgment call)

Add an ADR under `docs/decisions/` when the change is architecturally
significant: a new layer or boundary, a new external contract, a replaced
core dependency, a reversed previous decision. When referencing a past
decision, cite the number (`see ADR-0003`) — never restate its rationale.

## 6. Security & boundary pass

- New third-party dependency in the diff? It must have gone through the
  `adding-dependencies` skill (and the human said yes).
- `.github/` touched? That requires explicit human approval (AGENTS.md ASK
  rule) — confirm it happened.
- `unsafe` code is forbidden workspace-wide; if the diff somehow needs it,
  stop and escalate.

## 7. Verdict

Report a checklist with pass/fail per section. Any failure: fix it before
opening the PR, or explicitly flag it to the human with a recommendation.
The agent stops at the PR gate — never merge, never mark ready, never push
tags (AGENTS.md NEVER rules; branch protection is the hard boundary).
