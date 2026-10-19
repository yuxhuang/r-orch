//! Diagnostics go to stderr, and only when asked for.
//!
//! Nothing installed a `tracing` subscriber before, so every deliberate
//! swallowed error in the engine, the adapters and the stores was invisible.
//! Installing one risks the opposite problem: stdout is the contract,
//! and `--json` has to stay parseable, so a diagnostic must never land there.

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
    // simply broken. Note this says nothing about *our* records: at `trace`
    // the first line on that command comes from `mio`. It is still a real
    // check — no subscriber means no output at all — but
    // `nothing_is_logged_by_default_but_orchestry_debug_turns_it_on` is the
    // one that proves our own warnings flow.
    assert!(
        !loud.stderr.is_empty(),
        "RUST_LOG=trace produced no diagnostics at all"
    );
}

/// Silence by default is a *choice*, so the test has to tell it apart from a
/// subscriber that was never installed — which an earlier version of this test
/// could not: asserting an empty stderr passes identically whether the
/// subscriber is silent or absent, and the name claimed more than the body
/// established.
///
/// It is driven through a *warning we own*, for a reason worth recording:
/// `task list --json` logs nothing of ours at any level, and what
/// `RUST_LOG=trace` produces for it comes from `mio`. A test that turned
/// logging up on that command and asserted "something appeared" would be
/// measuring a dependency. So this uses the unreadable global config — one of
/// the deliberate degradations — and asserts that *its* warning is absent by
/// default and present with `ORCHESTRY_DEBUG`, which is also the only coverage
/// that path has.
#[test]
fn nothing_is_logged_by_default_but_orchestry_debug_turns_it_on() {
    let dir = project();
    let broken = dir.path().join("broken-global.yml");
    std::fs::write(&broken, b"\x00\x01 not: [valid").expect("write a broken global config");

    // Stderr is not empty here — the dashboard still fails to open a terminal,
    // and that error is reported — so the assertion is about the *warning*,
    // which is the thing logging controls.
    let quiet = orch(dir.path())
        .arg("tui")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", &broken)
        .output()
        .expect("run");
    assert!(
        !show(&quiet.stderr).contains("unreadable global config"),
        "an ordinary invocation logged a warning:\n{}",
        show(&quiet.stderr)
    );

    // Same command, same tree, one environment variable. `ORCHESTRY_DEBUG`
    // takes no filter spec — it means `debug`.
    let debug = orch(dir.path())
        .arg("tui")
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", &broken)
        .env("ORCHESTRY_DEBUG", "1")
        .output()
        .expect("run");
    assert!(
        show(&debug.stderr).contains("unreadable global config"),
        "ORCHESTRY_DEBUG logged nothing, so the silence above proves nothing \
         about the subscriber:\n{}",
        show(&debug.stderr)
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
