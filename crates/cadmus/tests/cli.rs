//! End-to-end CLI tests: verify stdout/stderr separation and exit-code
//! discipline through the compiled binary.

use assert_cmd::Command;

fn cli() -> Command {
    Command::cargo_bin(env!("CARGO_PKG_NAME")).expect("binary built by cargo")
}

/// The `--help` output is guarded by a snapshot: changes to arguments or
/// wording show up as a diff in code review.
/// Update with `just snapshot-review` (human approval); CI is read-only.
#[test]
fn help_output() {
    let assert = cli().arg("--help").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    // clap derives the usage-line program name from argv[0], which carries an
    // .exe suffix on Windows; normalize so one snapshot serves all platforms.
    let stdout = stdout.replace(
        &format!("{}.exe", env!("CARGO_PKG_NAME")),
        env!("CARGO_PKG_NAME"),
    );
    insta::assert_snapshot!(stdout);
}

/// Same guard for the `chat` subcommand — its flags are the operational
/// surface (provider, limits, trajectory root).
#[test]
fn chat_help_output() {
    let assert = cli().args(["chat", "--help"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).expect("utf8");
    let stdout = stdout.replace(
        &format!("{}.exe", env!("CARGO_PKG_NAME")),
        env!("CARGO_PKG_NAME"),
    );
    insta::assert_snapshot!(stdout);
}

/// Failure-path discipline: diagnostics go to stderr, stdout stays clean,
/// and the exit code is non-zero. Runs before any API-key or network logic,
/// so it is environment-independent.
#[test]
fn chat_requires_a_prompt() {
    let assert = cli().arg("chat").assert().failure().code(1);
    let output = assert.get_output();
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a prompt is required"),
        "stderr should contain the error message, got: {stderr}"
    );
}

#[test]
fn chat_rejects_unknown_provider() {
    let assert = cli()
        .args(["chat", "--provider", "bogus", "hi"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("not one of: kimi, deepseek, custom"),
        "got: {stderr}"
    );
}

#[test]
fn chat_custom_requires_model_and_base_url() {
    let assert = cli()
        .args(["chat", "--provider", "custom", "hi"])
        .assert()
        .failure()
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("requires --model and --base-url"),
        "got: {stderr}"
    );
}
