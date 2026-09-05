//! Repository automation, following the `xtask` pattern: every check that git
//! hooks and CI run lives here in std-only Rust, so the repository has exactly
//! one language, one toolchain and one lockfile. Zero third-party dependencies
//! on purpose — the text checks hand-roll what they would otherwise pull
//! `regex` for, and quality is gated by the same clippy/fmt/tests as the
//! product code. See `docs/decisions/0002` for the policy.

use std::process::ExitCode;

mod agent_docs;
mod bump_toolchain;
mod commit;
mod new_crate;
mod pr_guard;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return usage();
    };
    let rest: Vec<String> = args.collect();
    match command.as_str() {
        "check-commit" => commit::run(&rest),
        "agent-check" => agent_docs::run(&rest),
        "new-crate" => new_crate::run(&rest),
        "bump-toolchain" => bump_toolchain::run(&rest),
        "pr-guard" => pr_guard::run(&rest),
        other => {
            eprintln!("error: unknown subcommand {other:?}");
            usage()
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run -q -p xtask -- <check-commit [FILE] | agent-check | new-crate <name> | bump-toolchain | pr-guard [PR] | pr-guard --staged>"
    );
    ExitCode::from(2)
}
