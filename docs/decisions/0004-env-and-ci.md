# 0004. Environment & CI: flake + direnv as SSOT, zero environment duplication in CI, actions pinned by SHA

- Status: accepted
- Date: 2026-08-29

## Context

- A "setup-rust + apt install" CI is a second environment description that evolves independently and inevitably drifts from local; meanwhile Nix has no native Windows support, so Windows members need a fallback path.
- Third-party actions referenced by tag can be silently re-pointed; GitHub officially recommends pinning to full commit SHAs.
- magic-nix-cache saves 30–50% CI time with zero configuration, but its implementation relies on reverse-engineered GitHub cache APIs — less robust than FlakeHub Cache / Cachix.

## Decision

- **Environment SSOT chain**: `rust-toolchain.toml` (language version) + `flake.lock` (toolchain & ecosystem tools) + `Cargo.lock` (language dependencies), all committed; the flake consumes rust-toolchain.toml via `rust-bin.fromRustupToolchainFile` — zero version restatement.
- **Responsibility boundary**: flake provides toolchains and ecosystem tools; cargo manages language dependencies; no 2nix tooling during development.
- **CI three-step**: checkout → nix-installer-action → magic-nix-cache-action, then uniformly `nix develop -c just …`; CI YAML contains only triggers/matrix/permissions/caches.
- **Windows**: CI has a dedicated `test-windows` job on the native rustup route (the runner's preinstalled rustup consumes rust-toolchain.toml) — version consistency is guaranteed by file sharing, not environment duplication; Windows members develop locally via WSL2.
- **Supply chain**: all third-party actions pinned to full SHAs (with `# vX.Y.Z` comments), dependabot keeps them fresh weekly; actions inside dist's generated release.yml are pinned via `dist.github-action-commits`.
- **Cache**: magic-nix-cache by default (zero config); if stability issues appear, switch to FlakeHub Cache or Cachix — only the cache step in ci.yml changes. Norm (2026-08-30): every compile/download path in CI is cached — the Nix store via magic-nix-cache on every nix job, cargo's registry + `target/` via rust-cache (`cmd-format: nix develop -c {0}` routes its key computation through the devShell) on every job that compiles, including the smoke-init project's out-of-checkout target via actions/cache.
- **Freshness**: `update-flake-lock` opens a flake.lock upgrade PR weekly.

## Consequences

- Toolchain upgrade = change `rust-toolchain.toml` once + `nix flake update`, verify with `just ci`, merge; no second copy of a version number exists in README/CI/Dockerfiles.
- "CI fails and I can't reproduce locally" is structurally eliminated: every command in the CI log can be replayed locally verbatim.
- SHA pinning adds reading cost, amortized by comments and dependabot; the `merge_group` trigger is already in place in ci.yml should merge queue be enabled.
