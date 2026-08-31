# 0003. Release pipeline: release-plz + cargo-dist + OIDC Trusted Publishing

- Status: accepted (commit-message enforcement tooling amended by ADR-0006)
- Date: 2026-08-29

## Context

Release engineering must answer three things at once: how the version number is maintained exactly once, how releases are fully automated while keeping a human gate, and how artifacts stay trustworthy under supply-chain threats. Key inputs:

- Hand-written version numbers/changelogs are duplicate copies that inevitably drift; long-lived API tokens are the weakest link in the release chain (litellm-style leaks).
- Full automation (semantic-release style) gives up an auditable release decision point; purely local command-driven releases (cargo-release style) have no CI enforcement.
- crates.io GA'd OIDC Trusted Publishing in 2025-07: CI exchanges repo/workflow/ref claims for a minutes-lived scoped token, so the repository stores zero static secrets; the first publish must be manual (TP can only bind to an existing crate).
- Both release-plz and cargo-dist can create the GitHub Release — responsibilities must be split.

## Decision

- **Version-intent SSOT = Conventional Commits**: `fix`→PATCH, `feat`→MINOR, `!`→MAJOR (the precise convention and its enforcement are defined by ADR-0006).
- **Human gate = release-plz's Release PR**: the machine computes versions, generates the Keep a Changelog CHANGELOG, and runs cargo-semver-checks to flag API breakage; the human only clicks merge. The changelog uses release-plz's built-in git-cliff default template — no separate `cliff.toml` is maintained.
- **Tag alignment**: `git_tag_name = "v{{ version }}"`; the tag pushed by release-plz triggers cargo-dist's `release.yml` (four-platform artifacts + installers + checksums + attestation). `git_release_enable = false` — the GitHub Release is created by dist, not twice.
- **Credentials**: crates.io publishing uses OIDC Trusted Publishing (no `CARGO_REGISTRY_TOKEN` in the workflow; the release job holds `id-token: write`); publish manually once, register TP on crates.io, and optionally enable TP-only mode.
- **Generated-file discipline**: `release.yml` is the output of `dist generate` — committed but never hand-edited; CI enforces it with `just dist-check` (`dist generate --check`). On PRs, dist runs `dist plan` so release-pipeline regressions surface before merge.

## Consequences

- The day-to-day release action collapses to "merge one PR"; rollback = ship a fix release (tags are immutable), bad versions are marked with `cargo yank`.
- First-time enablement needs one-time manual setup (GitHub workflow permissions, crates.io first publish + TP registration) — documented in CONTRIBUTING.md "Releasing".
- Newly publishable crates must be registered as `[[package]]` in `release-plz.toml`; when multiple packages are published, the tag format needs re-evaluation (alignment between `{{package}}-v{{version}}` and dist's trigger).
