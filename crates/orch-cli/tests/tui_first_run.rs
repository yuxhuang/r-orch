//! Bare `orch` — the first-run experience (PARITY.md §2.3).
//!
//! Driven through the real binary, because what is under test is partly the
//! argv shape: everything except the terminal itself, which no test can own.
//! The dashboard then fails to open (there is no tty under `cargo test`), and
//! that failure is the marker that the FTUE got that far.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command.current_dir(dir);
    // A terminal the dashboard could actually drive would leave this process
    // in the alternate screen; without one, `orch_tui::run` fails at
    // `enable_raw_mode` and the FTUE's init half is still observable on disk.
    command.env("TERM", "dumb");
    command
}

fn initialized(dir: &Path) -> bool {
    dir.join(".orchestry").is_dir()
}

#[test]
fn bare_orch_initializes_the_current_directory_and_opens_the_dashboard() {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_core::test_support::assert_hermetic(temp.path());
    let output = orch(temp.path()).output().expect("run orch");

    assert!(
        initialized(temp.path()),
        "bare `orch` must init the cwd; stdout was {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // The init half must not suggest a next command: the reference prints the
    // `Next:` line from `init`'s own action, not from the first-run path.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Next:"), "{stdout}");
    // It got as far as opening the terminal, which is as far as this can go.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("dashboard") || stderr.contains("dashboard"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn bare_orch_in_a_subdirectory_does_not_adopt_the_ancestor_project() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ancestor = temp.path();
    let nested = ancestor.join("services/worker");
    std::fs::create_dir_all(&nested).expect("mkdir -p");
    // An existing project above, which `find_project_root` would walk up to.
    std::fs::create_dir(ancestor.join(".orchestry")).expect("ancestor project");

    orch(&nested).output().expect("run orch");

    // The subtle half of §2.3: the probe and the init are `cwd`-local, and
    // root resolution happens afterwards. Resolving first would adopt the
    // ancestor and leave the user's new directory un-initialized.
    assert!(
        initialized(&nested),
        "the nested directory was never initialized — an ancestor was adopted"
    );
}

#[test]
fn bare_orch_in_an_initialized_project_does_not_re_init() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join(".orchestry")).expect("project");
    let marker = temp.path().join(".orchestry/config.yml");
    std::fs::write(&marker, "project:\n  name: untouched\n").expect("seed config");

    let output = orch(temp.path()).output().expect("run orch");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Already initialized"),
        "an initialized project must go straight to the dashboard: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).expect("read config"),
        "project:\n  name: untouched\n",
        "the existing config was rewritten"
    );
}

#[test]
fn flags_without_a_subcommand_print_help_rather_than_initializing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = orch(temp.path()).arg("--json").output().expect("run orch");

    // `cli.ts:206` appends `tui` only for a *bare* argv, so this is the
    // "group with no subcommand" path: help on stdout, exit 1.
    assert!(
        !initialized(temp.path()),
        "`orch --json` must not run the first-run init"
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage:"),
        "expected root help"
    );
}
