//! Diagnostics go to stderr, and only when asked for.
//!
//! Nothing installed a `tracing` subscriber before, so every deliberate
//! swallowed error in the engine, the adapters and the stores was invisible.
//! Installing one risks the opposite problem: stdout is the contract the
//! differential harness compares byte for byte, and `--json` has to stay
//! parseable, so a diagnostic must never land there.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(dir)
        .env("NO_COLOR", "1")
        // No agent CLI on `PATH`: nothing here needs one, and the gate forbids
        // a test exec'ing the real thing.
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    orch(dir.path())
        .args(["init", "--name", "p", "--adapter", "claude"])
        .output()
        .expect("init");
    orch(dir.path())
        .args(["task", "add", "a task", "--quiet"])
        .output()
        .expect("task add");
    dir
}

#[test]
fn stdout_is_byte_identical_whether_or_not_tracing_is_on() {
    let dir = project();

    let quiet = orch(dir.path())
        .args(["task", "list", "--json"])
        .output()
        .expect("run");
    let loud = orch(dir.path())
        .args(["task", "list", "--json"])
        .env("RUST_LOG", "trace")
        .output()
        .expect("run");

    assert_eq!(quiet.status.code(), Some(0), "{}", show(&quiet.stderr));
    assert_eq!(loud.status.code(), Some(0), "{}", show(&loud.stderr));
    // The whole point: turning diagnostics all the way up must not add a byte
    // to stdout, or `--json` stops parsing and every scenario diff lights up.
    assert_eq!(
        quiet.stdout,
        loud.stdout,
        "RUST_LOG=trace changed stdout:\n{}",
        show(&loud.stdout)
    );
    serde_json::from_slice::<serde_json::Value>(&loud.stdout).expect("stdout is still JSON");
    // And it did go somewhere, so this is not passing because tracing is
    // simply broken.
    assert!(
        !loud.stderr.is_empty(),
        "RUST_LOG=trace produced no diagnostics at all"
    );
}

#[test]
fn nothing_is_logged_by_default() {
    let dir = project();

    let output = orch(dir.path())
        .args(["task", "list", "--json"])
        .output()
        .expect("run");

    assert!(
        output.stderr.is_empty(),
        "an ordinary invocation logged to stderr:\n{}",
        show(&output.stderr)
    );
}

#[test]
fn a_swallowed_error_reaches_stderr_when_logging_is_on() {
    let dir = project();
    // An unreadable global config is one of the deliberate degradations:
    // `FileGlobalConfigStore::read` warns and returns the defaults. Reading it
    // needs the full tier, so this uses `tui`, which builds it and then fails
    // to open a terminal — after the read, which is all this needs.
    let broken = dir.path().join("broken-global.yml");
    std::fs::write(&broken, b"\x00\x01 not: [valid").expect("write a broken global config");

    let output = orch(dir.path())
        .arg("tui")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", &broken)
        .env("RUST_LOG", "warn")
        .output()
        .expect("run");

    let stderr = show(&output.stderr);
    assert!(
        stderr.contains("unreadable global config"),
        "the warning never surfaced:\n{stderr}"
    );
    // Still on stderr, not mixed into the command's own output.
    assert!(
        !show(&output.stdout).contains("unreadable global config"),
        "the warning leaked into stdout"
    );
}

fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
