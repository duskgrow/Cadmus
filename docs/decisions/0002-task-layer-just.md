# 0002. Task layer: just as the single entry point, automation code in xtask

- Status: accepted
- Date: 2026-08-29

## Context

"Where does task logic live" is the highest-leverage automation decision: every extra implementation of the same logic is a drift source. The anti-pattern (Makefile + shell scripts + CI YAML in triplicate) inevitably ends with "green locally, red in CI". The two mainstream carriers in the Rust ecosystem:

- **cargo xtask**: automation in the host language — type-safe, testable, cross-platform without a shell; the cost is that every task is a Rust subcommand, so aggregating one-liners is over-engineering.
- **just**: single binary, language-agnostic, `just --list` is living documentation; it cannot gracefully carry heavy logic, and on Windows the default `sh -c` needs an explicit shell override.

## Decision

The justfile is the only task entry point: local development, git hooks (prek) and CI are all callers; recipes only invoke the tools' own commands, and parameters live in each tool's own config file (`deny.toml`, `.config/nextest.toml`, workspace lints, ...). The justfile declares `set windows-shell` so it works on Windows.

xtask was initially deferred, with trigger criteria (any one suffices): a task needs real logic (conditionals / loops / file parsing); code generation with a drift check is required; heavy logic on Windows cannot avoid the shell. Amended 2026-08-30: the criteria fired — the hook/CI checks became maintained, unit-tested gates, and the repository adopted a one-language norm (see Consequences). `crates/xtask` now carries all automation code; the justfile stays the façade and recipes call `cargo run -q -p xtask …`.

## Consequences

- `just ci` is the single quality gate of "local green ≡ CI green"; AGENTS.md forbids bypassing it.
- The decision path for new automation is fixed: first ask which of the five layers (tool versions / tasks / hooks / CI / release) it belongs to, then where it gets written.
- **Automation code lives in `crates/xtask` — std-only Rust, zero third-party dependencies, one language / one toolchain / one lockfile.** A 2026-08-30 measurement settled the hook-latency question: a std-only xtask costs ~0.2s cold-compiled and ~0.02s warm — indistinguishable from an interpreted script at hook time — so the commit-msg / agent-check gates are plain Rust, quality-gated for free by the existing clippy/fmt/nextest harness. A brief Python interlude (stdlib scripts via uv) was reverted within the same unreleased window: a second ecosystem's manifest, lockfile, interpreter pin and parallel lint stack (ruff + basedpyright) is governance surface that grows with every added script. The norm: hook/CI check logic and scaffolding are Rust in xtask; interpreted second languages are not introduced (an exception requires a PR discussion reversing this bullet).
- Placement follows lifecycle, so one-off automation never pollutes generated projects: daily/repeated automation → justfile recipes; check/gate logic and permanent scaffolding (`new-crate`) → `crates/xtask` subcommands; one-time bootstrap → the template-only `init` module inside xtask (registered behind `// >>> template-only` fences, deleted by init itself); one-time host-side bootstrap → the self-deleting `.github/workflows/init.yml`; one-time post-init hosting administration (repository settings, Trusted Publishing) → runnable `gh` blocks inside CONTRIBUTING.md checklists — never a shipped one-off script (it would rot in generated projects), and never the justfile (it carries only what a developer runs every day).
