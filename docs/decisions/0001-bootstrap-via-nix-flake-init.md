# 0001. Bootstrap mechanism: nix flake init + self-destructing `just init`

- Status: accepted
- Date: 2026-08-29

## Context

Template consumers should start from an "empty repository": no template git history, no leftover template artifacts, one command to transform. Candidates:

1. **cargo-generate**: the Rust ecosystem's standard templating tool with placeholders and interactive variables; but it requires installing `cargo-generate` first (yet another tool) and is essentially "file copying with variable substitution".
2. **GitHub Template repository button**: zero tooling, no git history; but no variable substitution and outside the CLI workflow.
3. **nix flake templates**: `nix flake init -t github:org/repo` copies the directory pointed to by `templates.default.path` (without `.git`) — Nix-native, zero extra tooling; the cost is no variable substitution.

## Decision

Adopt **nix flake templates for distribution + a self-destructing `just init`**:

- `flake.nix` exposes `templates.default = { path = ./.; ... }`; one `nix flake init` performs the copy;
- `flake.nix` also exposes a template-only `apps.default`: `nix run <flake> -- <name> <owner>` copies the template into an empty directory and invokes the same `xtask` init subcommand — the one-command form of the same bootstrap, self-contained via `writeShellApplication` runtimeInputs (git/cargo/prek), no `nix develop` needed;
- `just init <name> <owner>` (the template-only `init` module in `crates/xtask`, registered behind `// >>> template-only` fences) does placeholder substitution (`__project_name__` / `__GITHUB_OWNER__` — snake_case in `.rs` files, kebab-case elsewhere), removes template artifacts (its own module, the template README, `docs/engspec.md`, the CI smoke job, the flake templates block, the init workflow), rebuilds `Cargo.lock`, and runs `git init` + stages the tree — deliberately without committing, so the project's history is authored entirely by its owner (a template-authored or fallback-identity commit would be residue too);
- template files carry `# >>> template-only` / `# <<< template-only` markers (`// …` in Rust sources) around blocks that init removes, so one file serves both the "template itself" and the "generated project" identities without a second copy;
- the GitHub Template button is documented as an equivalent alternative path (clone, then run the same `just init`).

## Consequences

- One command on the user side (`nix run <flake> -- <name> <owner>`), or two via the `templates.default` copy-then-init split; the result is indistinguishable from a hand-built empty repo. History starts empty — the owner's first commit is the first commit (the tree is staged and the hooks are armed, so the commit convention is enforced from commit #1). The template repository itself runs `just ci` directly (the placeholder crate name `__project_name__` is itself a valid crate name).
- The `smoke-init` CI job runs the full init → `just ci` flow in a temp directory, so template rot (e.g. a missed placeholder) is caught by the template's own CI.
- The placeholder approach has no interactive variables (description, license choice, ...); the description is guided by a TODO comment. If the variable count ever grows, re-evaluate migrating to cargo-generate (compatible: same file tree).
