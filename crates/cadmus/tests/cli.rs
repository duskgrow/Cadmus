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

#[test]
fn greet_defaults_to_world() {
    cli()
        .arg("greet")
        .assert()
        .success()
        .stdout("Hello, world!\n");
}

#[test]
fn greet_json_is_machine_readable() {
    cli()
        .args(["greet", "ferris", "--json"])
        .assert()
        .success()
        .stdout("{\"greeting\":\"Hello, ferris!\"}\n");
}

/// Failure-path discipline: diagnostics go to stderr, stdout stays clean,
/// and the exit code is non-zero.
#[test]
fn empty_name_fails_cleanly() {
    let assert = cli().args(["greet", ""]).assert().failure().code(1);
    let output = assert.get_output();
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on failure"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("name must not be empty"),
        "stderr should contain the error message, got: {stderr}"
    );
}
