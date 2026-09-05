# CONTRIBUTING

[中文文档](./CONTRIBUTING.zh-CN.md)

## Environment setup

```bash
direnv allow   # or: nix develop — entering the devShell also arms the git hooks
just ci        # verify everything is green
```

Git hooks (pre-commit + commit-msg) arm themselves on devShell entry — git
cannot ship hooks with a clone, so in a non-Nix shell run `just setup` once
(`prek uninstall` to remove them).

All day-to-day commands: `just --list` (living documentation, evolves with the repo). Core entries: `just fmt` / `just lint` / `just test` / `just ci`.

## Merging and commit messages

Merge strategy is **squash merge only** (set in repository settings — see "Repository settings"). main's history stays one-line-one-meaning, and the PR title + body become the landed commit message — so **the PR title must follow the commit convention**, and CI checks it. The merge flow is: review on the web → green checks → merge. The squash dialog pre-fills the PR title + body and needs **zero edits** — the whole body is landable by construction.

Commit convention — modified Conventional Commits:

    type(scope)!: subject

- `type`: one of `feat fix docs style refactor perf test build ci chore revert` (lowercase)
- `scope`: optional, lowercase (crate name, `cli`, …); `!` marks breaking (or a `BREAKING CHANGE:` footer). Scope doubles as the changelog's module section (release notes are grouped by scope, Zed-style), so prefer setting it
- `subject`: pure-ASCII English; the whole header is at most 100 chars
- body: any language, no line-width limit; separated from the header by one blank line. The whole body lands in history verbatim, so it must be landable as a whole: no HTML comments (delete the template's guidance comment, don't merge it), no task lists (`- [ ]`), no bare `---` lines — process scaffolding is rejected outright (enforced by check-commit). Keep it readable: a prose paragraph runs at most 7 lines — split longer bodies into paragraphs or use lists (list items, blockquotes and fenced code are exempt)
- footer (`TOKEN: value` / `TOKEN #value`): the block is preceded by a blank line. Blessed tokens: `Closes #N` (GitHub auto-closes the issue on merge) and `BREAKING CHANGE:` (semver-MAJOR signal). GitHub itself appends `Co-authored-by:` for multi-author PRs; tooling-attribution trailers stay banned (see AGENTS.md)

Semver mapping: `fix` → PATCH, `feat` → MINOR, `!` → MAJOR. Version derivation and the CHANGELOG are generated from these messages — a wrong type is a wrong release.

Enforcement (SSOT: the `check-commit` subcommand of `crates/xtask`):

- local commit-msg hook (armed on devShell entry; `just setup` in non-Nix shells);
- CI checks the PR title + body on every pull request.

Notes:

- GitHub appends ` (#NNN)` to the squash-merge commit title — expected, leave it.
- `git revert`'s default `Revert "…"` header doesn't match the convention — rewrite it as `revert: <what>`.
- The PR template is a single zone: everything below its HTML guidance comment lands as the commit body (delete the comment before opening — CI rejects it). AI assistance is disclosed with the `ai-assisted` label, not in the message.
- Dependabot PRs: the generated body is not landable as-is (HTML comments, long prose runs, a metadata block) — rewrite it to a one-line summary before merging; the generated title already follows the convention.

### PR gate: secret scan

- **Secret scan** of the PR diff (added lines matched against token / private-key / `.env`-assignment shapes) — hard fail; the offending line is annotated right on the PR's Files tab. A leaked secret needs rotation, not just deletion. Documented _example_ secrets can carry a `pr-guard:allow` marker on their line (visible in review). The same scan runs locally as a pre-commit hook over the staged diff (SSOT: the `pr-guard` subcommand of `crates/xtask`).
- `ai-assisted`: disclosure label for PRs co-authored with generative AI (the `pr-preflight` skill was run, or the diff was reviewed against it). Metadata, not a gate — nothing enforces it.

## Adding a dependency

1. Declare the version in the root `Cargo.toml`'s `[workspace.dependencies]` (the only place in the repo);
2. Member crates inherit with `dep.workspace = true` and may only add `features` / `optional`;
3. `just deny` enforces license and source policy; a new license requires a PR discussion before extending the `deny.toml` allow-list.
4. `just arch-test` gates the direction: forbidden edges live in `crates/xtask/src/arch.rs` (the contract takes no workspace crate and its third-party deps are a closed set, the core no adapter or runtime/IO crate, adapters only the contract). Routine adds never touch it — a rejection there is an architecture inversion surfacing; a genuinely intended new edge earns a deliberate posture edit.

## Adding a crate

```bash
just new-crate <name>
```

This creates an **internal crate** (`version = "0.0.0"`, `publish = false`) with no semver burden. To publish a crate: switch to `version.workspace = true`, drop `publish = false`, and register it as `[[package]]` in `release-plz.toml`. Note: a published crate may only depend on path dependencies that carry a version (a hard `cargo publish` requirement).

New crates inherit the adapter posture (internal deps limited to the contract); `just arch-test` fails only when the crate is a new _kind_ — that edit in `crates/xtask/src/arch.rs` is the direction decision.

## Tests and snapshots

- Write tests at the lowest layer that can catch the bug; CLI behavior goes in `tests/` (assert_cmd), pure logic in unit tests.
- Large-output assertions (help text, diagnostics, serialized formats) use insta snapshots: snapshots are committed and reviewed; CI is read-only. Nondeterministic fields (timestamps, paths, UUIDs) must be filtered/redacted before snapshotting.
- Update snapshots with `just snapshot-review` — read each diff before approving.

## Releasing

Nothing to do day to day. release-plz keeps a Release PR up to date (version + CHANGELOG + semver-check verdict); **merging it** triggers:

1. tag `vX.Y.Z` is pushed;
2. the tag triggers cargo-dist: four-platform builds → GitHub Release (installers, checksums, attestation) whose body renders the version's module-grouped changelog section.

crates.io publishing is **opt-in**: the project runs in release-plz's `git_only` mode (versions come from git tags; no cargo registry is contacted), so nothing reaches crates.io until you deliberately enable it — see "Opting into crates.io publishing".

The changelog is generated from commit messages, grouped by module: the commit scope (`feat(cli): …`) becomes the section name, so scoped commits write the release notes for free.

Don't want to release yet? Just don't merge — the Release PR accumulates and updates itself.

### One-time setup (for the Release PR)

**GitHub**: Settings → Actions → General → Workflow permissions: "Read and write", and check "Allow GitHub Actions to create and approve pull requests" — without it the Release PR cannot be opened.

### Opting into crates.io publishing

1. In `release-plz.toml`, drop the workspace's `git_only = true` / `publish = false` lines; in `.github/workflows/release-plz.yml`, add `id-token: write` to the release job's permissions (OIDC Trusted Publishing — no long-lived token).
2. **First crates.io publish**: Trusted Publishing can only bind to an existing crate, so publish once manually with `cargo publish` (revoke the local token afterwards).
3. **Register TP**: crates.io → crate → Settings → Trusted Publishing → add the GitHub repository and the workflow filename `release-plz.yml`. Optionally enable "Trusted Publishing only" on crates.io to disable token publishing entirely.
4. Sanity check: no hand-made tags in `git tag`; no crates.io token in repository secrets.

## CI map

| job / workflow                 | purpose                                                                                              |
| ------------------------------ | ---------------------------------------------------------------------------------------------------- |
| `ci.yml` → quality-gate        | `prek run --all-files`, same source as local git hooks                                               |
| `ci.yml` → test (ubuntu/macos) | `nix develop -c just ci`                                                                             |
| `ci.yml` → test (windows)      | native rustup route, same `rust-toolchain.toml`                                                      |
| `commits.yml`                  | commit-convention check on the PR title + body (the squash-merge commit message); re-runs on retitle |
| `pr-guard.yml`                 | secret scan of the PR diff — hard fail, the hit is annotated on the PR's Files tab                   |
| `ci.yml` → dist-drift          | consistency between generated release.yml and `dist-workspace.toml`                                  |
| `release-plz.yml`              | Release PR + tag (crates.io publish is opt-in)                                                       |
| `release.yml` (dist-generated) | tag-triggered cross-platform build + GitHub Release; `dist plan` on PRs                              |
| `flake-update.yml`             | weekly flake.lock upgrade PR                                                                         |
| `toolchain-update.yml`         | weekly rust-toolchain.toml upgrade PR (validated in-job by `just ci`)                                |

## Repository settings (one-time, GitHub side)

These live in GitHub settings rather than in code; set them once when the repo is created:

- General → Pull Requests: **squash merge only** (disable merge commits and rebase merge); set the squash commit message to "Default to pull request title and description"; enable "Automatically delete head branches".
- Branches → protect `main`: require a pull request before merging; require status checks `quality gate (prek)`, `test (ubuntu-latest)`, `test (macos-latest)`, `test (windows)`, `conventional commits`, `pr guard`, `release.yml drift check`; require branches to be up to date.
- Actions → General: workflow permissions per the release setup above.

The same settings as a one-shot `gh` block (run inside the repo, authenticated as yourself with `gh auth login` — `{owner}`/`{repo}` auto-resolve from the remote). This is deliberately a documented command block rather than a shipped script: it runs once per repository, needs your admin credentials, and a copy-paste block cannot rot silently the way a shipped one-off script would.

```bash
gh repo edit --enable-squash-merge --enable-merge-commit=false --enable-rebase-merge=false --delete-branch-on-merge --squash-merge-commit-message=pr-title-description
gh api repos/{owner}/{repo}/actions/permissions/workflow -X PUT -F default_workflow_permissions=write -F can_approve_pull_request_reviews=true
gh api repos/{owner}/{repo}/branches/main/protection -X PUT -F 'required_status_checks[strict]=true' -F 'required_status_checks[contexts][]=quality gate (prek)' -F 'required_status_checks[contexts][]=test (ubuntu-latest)' -F 'required_status_checks[contexts][]=test (macos-latest)' -F 'required_status_checks[contexts][]=test (windows)' -F 'required_status_checks[contexts][]=conventional commits' -F 'required_status_checks[contexts][]=pr guard' -F 'required_status_checks[contexts][]=release.yml drift check' -F 'required_pull_request_reviews[required_approving_review_count]=0' -F enforce_admins=null -F restrictions=null
gh label create ai-assisted --color 8250df --description "Generative AI co-authored this PR; the pr-preflight judgment pass was run"
```

Settings live outside git and can drift — verify with:

```bash
gh api repos/{owner}/{repo} --jq '{allow_squash_merge, allow_merge_commit, allow_rebase_merge, delete_branch_on_merge, squash_merge_commit_title, squash_merge_commit_message}'
# expect: squash only; title PR_TITLE + message PR_BODY; delete_branch_on_merge true
```

(The block above is one-shot imperative setup. If you later want continuous settings-as-code, the probot `settings` app reads `.github/settings.yml` — adopt it deliberately, since it also gates `.github/` changes.)

## Validating CI locally (optional)

Every command CI runs is already locally reproducible (`just ci`, `prek run --all-files`, `just dist-check`) — that is by design. The workflow YAML itself is statically checked: `just lint` includes actionlint. Dynamic replay of the orchestration (e.g. with act/Docker) was evaluated and deliberately rejected: the added fidelity doesn't justify the Docker dependency when the task layer is already the single source of truth.

Jobs needing secrets/OIDC (release-plz publish, crates.io Trusted Publishing) cannot run locally _by design_ — their pre-merge seam is the Release PR plus `dist plan` on every PR.

## Agent-assisted development

This repository is a dual human/agent artifact. The agent surface:

- `AGENTS.md` — always-on constraints (commands, NEVER rules, style). Incremental information only: every rule must pass "would an agent err without this?".
- `.agents/skills/<name>/SKILL.md` — task-activated procedures (progressive disclosure); the authoritative one-line listing lives in AGENTS.md. Constraints live in AGENTS.md; procedures live in skills; reference knowledge lives in `docs/` (linked, never copied).
- `.claude/skills` symlinks to `.agents/skills` — one source of truth for every host.
- `just agent-check` (part of `just ci`) smoke-validates this surface: frontmatter shape, name/dir match, size budgets, pointer integrity.

Boundaries: agents may open PRs but never merge, push tags, or publish; the hard boundary is branch protection, not the instruction files. Repeated agent mistakes become permanent rules via the `rule-maintenance` skill — in the same PR as the fix, not as repeated chat corrections. See ADR-0005 in the template repository for the rationale.

## Documentation discipline

Hand-written docs carry exactly three things: **intent** (why), **rationale** (ADRs), and **entry points** (runnable commands). Facts that code can state itself (parameter tables, version numbers, command lists) are never hand-copied into prose; architecture decisions go to `docs/decisions/` and everywhere else references the number only.

Language: English is canonical. `*.zh-CN.md` files are translations — update the English version first, and treat translation drift as a bug. Translations exist for external-facing docs (README, CONTRIBUTING) only; ADRs, AGENTS.md and `.agents/skills` stay English-only — a translation serves its readers, and those docs' readers (the maintainer, agents) all read English.
