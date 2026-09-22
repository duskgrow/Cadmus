---
name: dependency-update-review
description: "Use when a Dependabot dependency-update PR (cargo or github-actions group) is waiting for the human merge decision. The agent digests the bump: what changed upstream, which workarounds and forced complexity the new version obsoletes, behavior-equivalence evidence before deleting any local wheel, open-item consumption, deny.toml fuse clearing — CI green is the floor, not the verdict."
---

# Dependency update review (digesting the bump)

Dependabot opens grouped weekly PRs (cargo + github-actions). A bump is not
just a lockfile line: it can _remove_ code — workarounds and forced
complexity that existed only because the fix or API had not shipped yet. CI
green is necessary but not sufficient; the digest is the judgment layer on
top. The agent prepares the verdict; **the human merges** (AGENTS.md NEVER
rules).

## 1. Read the upstream delta first

Per crate in the group: changelog / release notes / diff old→new. Sort each
into **breaking for us** (migrate the call sites in this PR), **new
capability** (feeds §2–4), or **irrelevant**. Watch for:

- Deprecations our code trips — fix them in this PR, not "later".
- MSRV bumps vs `rust-toolchain.toml` — that is a toolchain decision
  (`just toolchain-bump`), never a silent side effect of a dep PR.
- Yanked versions — `just deny` denies them mechanically.

A hold verdict on one crate holds the whole group; splitting the group means
editing `dependabot.yml`, which is ASK-first (`.github/`).

github-actions bumps: the checklist collapses to "do our workflows use any
changed input/output or hit a breaking note" + `just lint` (actionlint).
Editing workflow YAML stays ASK-first, but the digest itself is review only.

## 2. Find the code that was waiting for this update

Wheels and compromises built because the update did not exist yet:

- Grep `docs/open-items.md` for the crate name — items carry a `Consumer:`
  line naming their upgrade (e.g. "the first ratatui dependency upgrade
  after the history-write port"). Items consumed here are deleted in the
  same PR.
- Grep the code for TODO / HACK / workaround comments and for upstream
  issue or PR links naming the crate.
- Every hit gets a verdict: obsoleted by this version (→ §3), still needed
  (write the reason at the spot), or unrelated.

## 3. Prove behavior before deleting the wheel

- Find the tests / spikes / snapshots that pinned the workaround's behavior
  and rerun them against the new version. An upstream fix is not evidence
  that its behavior satisfies our contract — the ratatui lesson in
  open-items.md: "a fixed upstream writer alone is not evidence that its
  resize/ack behavior satisfies the shell's contract."
- Behavior differs → either keep the wheel (reason written at the spot) or
  swap and add the patch that restores the contract. What never ships: a
  silent behavior change.
- Sensitive surface → hand-review with `just snapshot-review`; agent-loop
  shifts → `just eval`.

## 4. Simplification the update newly enables

Roundabout code that the new version lets go direct is in scope — as its own
commit(s), per code-simplification's "always its own change" line.
Opportunistic refactors the update did _not_ unlock are out of scope here;
they want their own PR.

## 5. Lockfile and gate hygiene

- `just ci` green (includes `just deny`).
- Read the `Cargo.lock` diff beyond the named crates: surprising new
  transitive dependencies, duplicate versions worth a targeted
  `cargo update -p` nudge.
- deny.toml fuses: if the bump clears an advisory-ignore or
  license-exception trigger, delete that entry in this same PR —
  `-D advisory-not-detected` / `-D license-exception-not-encountered`
  already fail CI if you don't. Never widen the global license `allow`
  list to make a bump pass; per-crate exception + PR discussion instead.

## 6. Land and report

- Digest work lands as commits separate from Dependabot's bump commit, so
  review can tell mechanics from judgment. Squash merge lands the PR
  title + body as the commit: Dependabot's `build(deps):` / `chore(deps):`
  titles already pass check-commit, but if the digest changed what the PR
  _is_, retitle accordingly.
- Docs co-evolve: a behavior-affecting swap updates its owning doc in this
  PR (ADRs own decisions, tools.md the tool catalog).
- Report, like release-review: per-crate upstream delta, wheels removed or
  kept with the behavior evidence, open items consumed, deny.toml fuses
  cleared, simplifications landed — then a clear **merge / hold**
  recommendation. The human merges.
