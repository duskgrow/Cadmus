---
name: release-review
description: Use when a release-plz Release PR is waiting for the human merge decision. The agent prepares the verdict — does the semver bump match the commit types, is the changelog complete and honest, what did semver-checks say, is the publish surface intact — and the human clicks merge.
---

# Release review (the human gate)

release-plz prepares everything; this checklist is what a human needs to
decide "merge or hold". The agent gathers and judges; **the human merges**
(AGENTS.md NEVER rules).

## What the Release PR should contain

- Version bump in the root `Cargo.toml`'s `[workspace.package] version` —
  touched _only_ by release-plz (hand edits are a red flag).
- `CHANGELOG.md` entries generated from Conventional Commits since the last
  tag. Hand-written day-to-day entries are a red flag (hand-polish of already
  released sections is fine).

## Judgment checklist

1. **Bump vs intent** — walk `git log <last-tag>..HEAD`: every `feat` means
   minor, every breaking marker means major, fixes mean patch. Does the
   proposed version match? A breaking change landing as a patch/minor is a
   hold.
2. **semver-checks verdict** in the PR body: `incompatible` without a major
   bump → hold and decide (fix the API or accept the major).
3. **Changelog honesty** — every user-facing change present, correctly
   grouped, no internal-only noise (build/ci churn) presented as user-facing.
   Misclassified commit types found here should be fixed by amending the
   convention discipline, not by hand-editing this changelog.
4. **Publish surface intact** — `release-plz.yml` still has `id-token: write`
   on the release job and no `CARGO_REGISTRY_TOKEN`; `dist-workspace.toml`
   and the generated `release.yml` are in sync (`just dist-check`); the tag
   format still matches dist's trigger (`v*`).
5. **Green CI** on the Release PR, including `dist plan`. `conventional
   commits` passes by construction — the `pr_body` template in
   `release-plz.toml` generates a body that is landable as a whole (no HTML
   comments, task lists or `---` lines; the changelog postprocessor strips
   stray comments). A failure there means the template regressed — fix the
   template, never hand-edit the generated body.

## Verdict

Report: version, bump correctness, semver-checks outcome, changelog notes,
publish-surface status, and a clear **merge / hold** recommendation with
reasons. After a merge, the tag + crates.io publish + cargo-dist GitHub
Release run automatically; watch them and report failures.
