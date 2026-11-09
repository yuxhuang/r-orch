//! Diagnostics go to stderr, and only when asked for.
//!
//! Nothing installed a `tracing` subscriber before, so every deliberate
//! swallowed error in the engine, the adapters and the stores was invisible.
//! Installing one risks the opposite problem: stdout is the contract,
//! and `--json` has to stay parseable, so a diagnostic must never land there.
//!
//! `task add` and `task list --json` are daemon RPCs since S2a-cli, so
//! [`project`] adopts a test daemon right after `init`. The two tests about
//! *our own* warnings read the daemon's stderr, because the global config is
//! read in the daemon, not in any client.

#[path = "common/daemon.rs"]
mod daemon;
use daemon::OwnedDir;

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
    daemon::apply_for(dir, &mut command);
    command
}

fn project() -> OwnedDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // No daemon yet: `init` is the one command that still runs bare.
    let status = Command::cargo_bin("orch")
        .expect("the orch binary")
        .current_dir(dir.path())
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .args(["init", "--name", "p", "--adapter", "claude"])
        .status()
        .expect("init");
    assert!(status.success(), "init failed");
    let owned = OwnedDir::adopt(dir);
    let added = orch(owned.path())
        .args(["task", "add", "a task", "--quiet"])
        .output()
        .expect("task add");
    assert!(
        added.status.success(),
        "task add failed: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    owned
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

/// The diagnostics a daemon wrote to its stderr after serving one global
/// config read, with the global config unreadable and `extra` added to the
/// daemon's environment.
///
/// The warning is emitted where the global config is read, and since S2a-cli
/// only the daemon reads it (`orch tui` and the CLI are clients), so the
/// warning is looked for on the daemon's stderr, not on a client's. The
/// unreadable file is one of the deliberate degradations:
/// `FileGlobalConfigStore::read` warns and returns the defaults.
fn daemon_stderr_with_an_unreadable_global_config(extra: &[(&str, &str)]) -> (String, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    let status = Command::cargo_bin("orch")
        .expect("the orch binary")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .args(["init", "--name", "p", "--adapter", "claude"])
        .status()
        .expect("init");
    assert!(status.success(), "init failed");
    let broken = root.join("broken-global.yml");
    std::fs::write(&broken, b"\x00\x01 not: [valid").expect("write a broken global config");
    let extra: Vec<(&str, std::path::PathBuf)> =
        std::iter::once(("ORCHESTRY_GLOBAL_CONFIG_PATH", broken.clone()))
            .chain(extra.iter().map(|(k, v)| (*k, std::path::PathBuf::from(v))))
            .collect();
    let extra_refs: Vec<(&str, &Path)> = extra.iter().map(|(k, v)| (*k, v.as_path())).collect();
    let daemon = daemon::TestDaemon::start_with_env(&root, &root.join("home"), &extra_refs);
    let mut show_config = Command::cargo_bin("orch").expect("the orch binary");
    show_config
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", &broken)
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG")
        .args(["config", "global", "show"]);
    daemon.apply(&mut show_config);
    let client = show_config.output().expect("run the client");
    let client_stderr = show(&client.stderr);
    let daemon_stderr =
        std::fs::read_to_string(daemon.socket_dir().join("daemon.err")).unwrap_or_default();
    drop(daemon);
    (daemon_stderr, client_stderr)
}

/// Silence by default is a *choice*, so the test has to tell it apart from a
/// subscriber that was never installed: asserting an empty stderr passes
/// identically whether the subscriber is silent or absent. It is driven
/// through a *warning we own* (see
/// [`daemon_stderr_with_an_unreadable_global_config`]), asserted absent by
/// default and present with `ORCHESTRY_DEBUG`, which is also the only coverage
/// that path has.
#[test]
fn nothing_is_logged_by_default_but_orchestry_debug_turns_it_on() {
    let (quiet, _) = daemon_stderr_with_an_unreadable_global_config(&[]);
    assert!(
        !quiet.contains("unreadable global config"),
        "an ordinary daemon logged a warning:\n{quiet}"
    );

    // Same command, same tree, one environment variable. `ORCHESTRY_DEBUG`
    // takes no filter spec: it means `debug`.
    let (debug, _) = daemon_stderr_with_an_unreadable_global_config(&[("ORCHESTRY_DEBUG", "1")]);
    assert!(
        debug.contains("unreadable global config"),
        "ORCHESTRY_DEBUG logged nothing, so the silence above proves nothing \
         about the subscriber:\n{debug}"
    );
}

#[test]
fn a_swallowed_error_reaches_stderr_when_logging_is_on() {
    let (daemon_stderr, client_stderr) =
        daemon_stderr_with_an_unreadable_global_config(&[("RUST_LOG", "warn")]);
    assert!(
        daemon_stderr.contains("unreadable global config"),
        "the warning never surfaced:\n{daemon_stderr}"
    );
    // Diagnostics stay on the daemon's stderr; the client's own output is not
    // polluted by a daemon-side warning.
    assert!(
        !client_stderr.contains("unreadable global config"),
        "the daemon's warning leaked into the client's stderr:\n{client_stderr}"
    );
}

fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
