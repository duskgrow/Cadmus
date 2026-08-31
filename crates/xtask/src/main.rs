//! Repository automation, following the `xtask` pattern: every check that git
//! hooks and CI run lives here in std-only Rust, so the repository has exactly
//! one language, one toolchain and one lockfile. Zero third-party dependencies
//! on purpose — the text checks hand-roll what they would otherwise pull
//! `regex` for, and quality is gated by the same clippy/fmt/tests as the
//! product code. See `docs/decisions/0002` for the policy.

use std::process::ExitCode;

mod agent_docs;
mod commit;
mod new_crate;
// >>> template-only: the bootstrap subcommand deletes this module and these
// marker lines, so generated projects never carry their own scaffolding
mod init;
// <<< template-only

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
        // >>> template-only
        "init" => init::run(&rest),
        // <<< template-only
        other => {
            eprintln!("error: unknown subcommand {other:?}");
            usage()
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run -q -p xtask -- <check-commit [FILE] | agent-check | new-crate <name>>"
    );
    // >>> template-only: bootstrap subcommand, stripped with the init module
    eprintln!("       template bootstrap: init <name> <owner>");
    // <<< template-only
    ExitCode::from(2)
}
