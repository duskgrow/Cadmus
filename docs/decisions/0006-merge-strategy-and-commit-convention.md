# 0006. Merge strategy & commit convention: squash-only + modified Conventional Commits with a custom checker

- Status: accepted (amends the enforcement tooling in ADR-0003)
- Date: 2026-08-29

## Context

ADR-0003 made commit messages the version-intent SSOT and enforced Conventional Commits with cocogitto (`cog verify` at the commit-msg hook, `cog check` in CI). Two refinements proved necessary:

1. **Merge strategy needed fixing**: with squash merge, the PR title + body become the landed commit message — which moves the enforcement point from branch commits to the PR title, and makes PR format part of the convention.
2. **The convention needed tightening** beyond stock Conventional Commits: `type(scope): subject` with lowercase type/scope, a pure-ASCII English subject, a ≤100-char header, a free-form body (any language), and a blank line before footers. cocogitto cannot express these rules.

## Decision

- **Squash merge only**, set in repository settings ("Default to pull request title and description"), with branch protection on `main`. main's history is one-line-one-meaning; the changelog consumes exactly the PR titles.
- **The commit convention** (SSOT: the `check-commit` subcommand of `crates/xtask`):
  - header `type(scope)!: subject`; type ∈ `feat fix docs style refactor perf test build ci chore revert`; lowercase type/scope; ASCII-only subject; header ≤ 100 chars;
  - body free-form (Chinese allowed, no width limit), one blank line after the header; footer tokens (`BREAKING CHANGE:`, `TOKEN: value`, `TOKEN #value`) preceded by a blank line;
  - enforced at the commit-msg hook (local) and against the PR title + body (CI `commits` job); branch commits inside a PR are *not* checked, because squash makes them irrelevant to main's history.
- cocogitto is removed from the toolchain — one rule set, one checker, no second implementation. GitHub's automatic ` (#NNN)` suffix on squash merges is expected and tolerated by the changelog parsers (prefix matching).
- PR format: the title obeys the convention; the body follows `.github/pull_request_template.md`; merge-time hygiene note (tidy the generated body) lives in CONTRIBUTING.
- Issue intake gets structured forms (`.github/ISSUE_TEMPLATE/`, blank issues disabled).

## Consequences

- The version-intent chain is: PR title (CI-checked) → squash commit → release-plz version/changelog. No unchecked path into main's history short of an admin override.
- The checker is ours: ~150 lines of std-only Rust in `crates/xtask`, unit-tested in the existing nextest harness, gated by the same clippy/fmt as product code. Rule changes land in one place and propagate to both hook and CI.
- Scope is optional but load-bearing downstream: release-plz's `[changelog]` commit_parsers group by scope, so each version's release notes have one section per module (Zed-style), and cargo-dist renders the same section as the GitHub Release body.
- Trade-off accepted: body content (beyond footer formatting) is not machine-checked; the PR template + review culture cover it.
