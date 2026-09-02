# AGENTS.md

<!-- Freshness: Orientation tracks docs/; Commands tracks the justfile; NEVER tracks the generated-file list and .github/; Skills tracks .agents/skills/. Last reviewed: 2026-09 -->

## Orientation (read first in a fresh session)

- `docs/roadmap.md` — the six-phase tracker; phase kickoff follows its ritual (re-read report §10.2.N, re-verify time-sensitive claims, then write the phase's ADR).
- `docs/decisions/` — ADRs are the architecture-decision SSOT; they win over `docs/research/` (frozen exhibit material, never edited).

## Commands (just is the only entry point; never bypass the quality gates)

- Full quality gate: `just ci` (local green ≡ CI green; run before every commit)
- Format / static checks: `just fmt` / `just lint` (includes actionlint on workflow YAML)
- All tests: `just test`; single test: `cargo nextest run -p cadmus <name>`
- Iterate with the narrowest loop first (`cargo check -p <crate>`, scoped nextest); finish with `just ci`
- Snapshot updates: `just snapshot-review` (approve each diff by hand; never bulk-accept)
- New crate: `just new-crate <name>`
- Dependency policy: `just deny`; docs build: `just doc`; release-artifact drift check: `just dist-check`; agent-doc smoke check: `just agent-check`

## Environment

- Toolchains come from flake.nix: `direnv allow` or `nix develop`. `rust-toolchain.toml` is the version SSOT — no other file may restate toolchain versions.
- The agent terminal spawns a bare shell without the direnv hook, so `.envrc` never loads and `cargo`/`just` resolve to the user profile instead of flake.nix. Prefix such commands with `direnv exec .` when the flake toolchain is needed (e.g. `direnv exec . just ci`).
- `just deny` needs network access to fetch the RustSec advisory DB; when offline, skip that one item and run the rest.
- Native Windows development goes through WSL2 (Nix has no native Windows support); CI's windows job consumes the same `rust-toolchain.toml` / `Cargo.lock`.

## Skills (procedures live in .agents/skills/; constraints stay in this file)

- `pr-preflight` — the judgment-layer review before opening/updating a PR (also the frame for reviewing others' PRs)
- `self-review` — the pre-commit judgment pass over your own working diff (five axes, severity labels, size sanity)
- `adding-dependencies` — required procedure before touching any third-party dependency
- `rule-maintenance` — how to update AGENTS.md and skills when rules change or mistakes repeat
- `doc-maintenance` — writing/reviewing docs: three-question filter, EN-canonical translation sync, drift hunting
- `code-simplification` — behavior-preserving dedup/dead-code/abstraction cleanup, always as its own change
- `release-review` — the human-gate checklist for merging a release-plz Release PR

Done = `just ci` green + the `pr-preflight` checklist clean.

## NEVER

- NEVER hand-edit generated files: `Cargo.lock`, released sections of `CHANGELOG.md`, `.github/workflows/release.yml` (generator: `dist generate`; drift check: `just dist-check`).
- NEVER write multi-line logic in a CI YAML `run:` field — sink it into the justfile; CI only calls single-line entry points.
- NEVER hand-write version numbers: the single storage point is `[workspace.package] version` in the root `Cargo.toml`; release-plz rewrites it mechanically.
- NEVER commit code that hasn't passed `just ci`; NEVER assemble ad-hoc check pipelines that bypass just.
- NEVER merge PRs, push tags, or publish releases — the agent stops at the PR gate; merging belongs to humans (branch protection is the hard boundary, not this file).
- NEVER add agent-attribution footers (`Co-Authored-By`, "Generated with …") to commits or PRs.
- NEVER force-push or rewrite shared history unless the human explicitly asks.
- ASK before touching: new third-party dependencies (see the `adding-dependencies` skill), anything under `.github/`, branch protection and crates.io Trusted Publishing settings.

## Style (the part lints can't enforce)

- Time / randomness / IO are always injected as constructor parameters; no hidden `now()` / `rand()` / global state (test determinism depends on this seam).
- Error split: library crates use `thiserror` enums (callers branch on failure modes); the binary's top level reports with `miette` (three-part `code` + `help`); functions that propagate errors don't log — log once at the handling site.
- Commit messages: modified Conventional Commits — `type(scope): subject`, lowercase type/scope, pure-ASCII English subject, header ≤ 100 chars, blank line before body/footer (`fix`→PATCH, `feat`→MINOR, `!`→MAJOR). Enforced at the commit-msg hook and by the PR-title CI check (SSOT: the `check-commit` subcommand of `crates/xtask`); merge strategy is squash-only, so the PR title must obey the same rules.
- Docs and code are English-first; `*.zh-CN.md` files are translations and English is canonical — update the English version first. Translations exist only for external-facing docs (README, CONTRIBUTING); ADRs, AGENTS.md and skills stay English-only.
- Docs co-evolve in the same PR as the behavior change. A rule you needed but couldn't find is a missing rule: add it via the `rule-maintenance` skill in that PR, instead of re-deriving it next session.
