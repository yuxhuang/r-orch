//! Bare `orch` — the first-run experience.
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

    // The FTUE opens the dashboard, and the dashboard probes every supported
    // adapter for its model catalogue — so without this these tests exec
    // whichever of `claude`, `codex`, `cursor-agent`, `opencode`, `pi`,
    // `grok` and `agy` happen to be installed, and those write their own
    // config into `$HOME`. Measured: 14 real CLI executions from this file
    // alone. Pointing PATH at the fixtures makes the probe hermetic *and*
    // deterministic, which is what the gate's `no-real-cli` step checks.
    let fakes = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents");
    let inherited = std::env::var("PATH").unwrap_or_default();
    command.env("PATH", format!("{}:{inherited}", fakes.display()));

    // Keep `global.yml` and the update-check cache out of the real home.
    let home = dir.join(".first-run-home");
    std::fs::create_dir_all(home.join(".orchestry")).expect("create test home");
    command.env("HOME", &home).env("USERPROFILE", &home).env(
        "ORCHESTRY_GLOBAL_CONFIG_PATH",
        home.join(".orchestry/global.yml"),
    );
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
    // The init half must not suggest a next command: the `Next:` line
    // belongs to `init`'s own action, not the first-run path.
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

    // The subtle half: the probe and the init are `cwd`-local, and
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
fn flags_without_a_subcommand_do_not_initialize() {
    // `tui` is appended only for a *bare* argv, so `orch --json` is
    // not the first-run path. What it *is* depends on the directory, and both
    // halves are pinned here:
    //
    //   - uninitialized: the container is built before argument parsing, so
    //     this is `Not initialized`, exit **3** — not the root help.
    //   - initialized: no subcommand to run, so the root help and exit 1.
    let temp = tempfile::tempdir().expect("tempdir");
    let output = orch(temp.path()).arg("--json").output().expect("run orch");
    assert!(
        !initialized(temp.path()),
        "`orch --json` must not run the first-run init"
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let ready = tempfile::tempdir().expect("tempdir");
    orch(ready.path())
        .args(["init", "--name", "proj", "--adapter", "claude"])
        .output()
        .expect("init");
    let output = orch(ready.path()).arg("--json").output().expect("run orch");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage:"),
        "expected root help"
    );
}
