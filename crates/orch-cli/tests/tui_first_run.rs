//! Bare `orch` — a dashboard client, never an initializer.
//!
//! Since S2a-cli bare `orch` opens the dashboard as another socket client and
//! never creates `.orchestry` (initialisation is the explicit `orch init`,
//! which prints `Next: orch daemon start`). Driven through the real binary,
//! because what is under test is partly the argv shape. Every run aims at a
//! socket nobody listens on, so no daemon of the developer's machine is
//! reachable and none can be started by the command under test.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

fn orch(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command.current_dir(dir);
    // A terminal the dashboard could actually drive would leave this process
    // in the alternate screen; without one the dashboard client cannot open.
    command.env("TERM", "dumb");

    // The dashboard probes every supported
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

/// Bare `orch` in `dir`, aimed at a socket nobody listens on (a fresh short
/// directory, so no real daemon of this machine is ever reached).
fn bare_without_daemon(
    dir: &Path,
) -> (std::process::Output, std::path::PathBuf, tempfile::TempDir) {
    let short = tempfile::Builder::new()
        .prefix("otd-")
        // 0700: the daemon refuses a group- or world-accessible socket dir.
        .permissions(<std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
        .tempdir_in("/tmp")
        .expect("short socket dir");
    let socket = short.path().join("s");
    let output = orch(dir)
        .env("ORCH_SOCKET", &socket)
        .env("ORCH_TEST_SOCKET_ROOT", short.path())
        .output()
        .expect("run orch");
    (output, socket, short)
}

/// Every entry under `dir`, sorted, so "nothing was created" is a comparison.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            // The fixture's private HOME is not the project's state.
            if entry.file_name() == ".first-run-home" {
                continue;
            }
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            out.push(name.clone());
            if entry.file_type().expect("file type").is_dir() {
                walk(&entry.path(), &format!("{name}/"), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out
}

#[test]
fn bare_orch_in_an_empty_directory_does_not_initialize_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_core::test_support::assert_hermetic(temp.path());
    let (output, _socket, _short) = bare_without_daemon(temp.path());

    assert!(
        !initialized(temp.path()),
        "bare `orch` is a dashboard client and must never create `.orchestry`; stdout was {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        tree(temp.path()),
        Vec::<String>::new(),
        "bare `orch` wrote into the working directory"
    );
}

#[test]
fn bare_orch_without_a_daemon_fails_and_prints_neither_a_next_hint_nor_an_init_banner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (output, _socket, _short) = bare_without_daemon(temp.path());

    assert!(
        !output.status.success(),
        "no daemon and no tty cannot succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Next:"), "{stdout}");
    assert!(!stdout.contains("Initialized"), "{stdout}");
}

#[test]
fn bare_orch_creates_no_socket_and_leaves_no_daemon_behind() {
    // A client never starts a daemon: the socket it dialled still does not
    // exist, and the project holds no PID lock or listener record.
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join(".orchestry")).expect("project");
    let before = tree(&temp.path().join(".orchestry"));
    let (_output, socket, _short) = bare_without_daemon(temp.path());

    assert!(!socket.exists(), "bare `orch` bound a socket: {socket:?}");
    assert_eq!(
        tree(&temp.path().join(".orchestry")),
        before,
        "bare `orch` wrote into the project's state directory"
    );
}

#[test]
fn bare_orch_in_a_subdirectory_does_not_initialize_it_or_touch_the_ancestor_project() {
    let temp = tempfile::tempdir().expect("tempdir");
    let ancestor = temp.path();
    let nested = ancestor.join("services/worker");
    std::fs::create_dir_all(&nested).expect("mkdir -p");
    std::fs::create_dir(ancestor.join(".orchestry")).expect("ancestor project");
    let before = tree(ancestor);

    let (_output, _socket, _short) = bare_without_daemon(&nested);

    assert!(
        !initialized(&nested),
        "the nested directory was initialized by a dashboard client"
    );
    assert_eq!(tree(ancestor), before, "the ancestor project changed");
}

#[test]
fn bare_orch_in_an_initialized_project_does_not_re_init() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(temp.path().join(".orchestry")).expect("project");
    let marker = temp.path().join(".orchestry/config.yml");
    std::fs::write(&marker, "project:\n  name: untouched\n").expect("seed config");

    let (output, _socket, _short) = bare_without_daemon(temp.path());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Already initialized"),
        "a dashboard client never runs init: {stdout}"
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

#[test]
fn init_prints_only_the_daemon_start_hint_and_starts_nothing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let output = orch(temp.path())
        .args(["init", "--name", "proj", "--adapter", "claude"])
        .output()
        .expect("init");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.matches("Next:").count(),
        1,
        "init prints exactly one next-step line: {stdout}"
    );
    assert!(stdout.contains("Next: orch daemon start"), "{stdout}");
    assert!(!stdout.contains("orch run"), "{stdout}");
}
