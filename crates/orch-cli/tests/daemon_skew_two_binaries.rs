//! Two-executable version skew (T2a; spec R6, R32; design 8.20): two real
//! `orch` binaries built from this one tree that differ ONLY in
//! `LOCAL_PROTOCOL` (1 and 2), driven both ways.
//!
//! The in-process skew harness proves the daemon's side; this proves what an
//! owner sees: an old binary's daemon and a new binary's clients (and the
//! reverse) must each answer with the uniform skew message that names both
//! protocols and the restart procedure, exit 7, start nothing, change no
//! state, and the daemon must keep serving its own binary afterwards. Only
//! the frozen stop intent crosses the skew, so the owner can always follow
//! the printed restart advice.
//!
//! The second binary is built with the non-default `skew-seam` feature and
//! `ORCH_SEAM_LOCAL_PROTOCOL=2` into a private target dir under
//! `CARGO_TARGET_TMPDIR` (cached across runs). Without the feature the
//! constant is 1 and the variable is ignored, so no shipped binary can
//! differ.

mod daemon_support;

use daemon_support::frontends::*;
use daemon_support::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SEAM_PROTOCOL: u32 = 2;

/// Build (once per run; incremental across runs) the protocol-2 binary.
fn skewed_binary() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("skew-seam-build");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .current_dir(workspace)
            .args([
                "build",
                "--locked",
                "--offline",
                "--package",
                "orch-cli",
                "--bin",
                "orch",
                "--features",
                "skew-seam",
                "--target-dir",
            ])
            .arg(&target)
            .env("ORCH_SEAM_LOCAL_PROTOCOL", SEAM_PROTOCOL.to_string())
            .output()
            .expect("run cargo");
        assert!(
            out.status.success(),
            "building the protocol-{SEAM_PROTOCOL} binary failed:\n{}",
            text(&out.stderr)
        );
        let exe = target.join("debug").join("orch");
        assert!(exe.is_file(), "{} was not built", exe.display());
        exe
    })
}

fn normal() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_orch"))
}

/// A sandbox project with a daemon started by `owner` (explicitly).
struct Pair {
    sandbox: Sandbox,
    daemon: DaemonChild,
    owner: &'static Path,
    owner_protocol: u32,
}

impl Pair {
    fn start(owner: &'static Path, owner_protocol: u32) -> Pair {
        let sandbox = Sandbox::new();
        sandbox.init_project();
        let mut cmd = orch_command_with(
            owner,
            &sandbox.root(),
            &sandbox.socket(),
            sandbox.short_dir(),
        );
        cmd.args(["daemon", "start"]);
        let mut daemon = DaemonChild::spawn(cmd, sandbox.short_dir().join("owner.err"));
        daemon.expect_ready(&sandbox.socket());
        Pair {
            sandbox,
            daemon,
            owner,
            owner_protocol,
        }
    }

    fn client(&self, exe: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = orch_command_with(
            exe,
            &self.sandbox.root(),
            &self.sandbox.socket(),
            self.sandbox.short_dir(),
        );
        cmd.args(args);
        output_with_deadline(cmd, CALL)
    }

    /// The skew text a client of `client_protocol` must print.
    fn assert_skew_text(&self, who: &str, stderr: &str, client_protocol: u32) {
        let root = self.sandbox.root();
        let seen = normalize(stderr);
        let first = format!(
            "No orch daemon is running for {} (daemon is orch {bin} protocol {theirs}, \
             this client is orch {bin} protocol {ours})",
            root.display(),
            bin = orch_proto::BINARY_VERSION,
            theirs = self.owner_protocol,
            ours = client_protocol,
        );
        assert!(seen.contains(&normalize(&first)), "{who}: {stderr}");
        assert!(
            seen.contains("Start it with: orch daemon stop, then orch daemon start"),
            "{who}: {stderr}"
        );
    }
}

/// Every non-stop client of `client` against `pair`'s daemon.
fn assert_clients_refuse(pair: &mut Pair, client: &Path, client_protocol: u32) {
    let watch = Watch::begin();
    let before = tree(&pair.sandbox.root());
    for args in [
        vec!["daemon", "status"],
        vec!["daemon", "dispatch", "on"],
        vec!["task", "list"],
        vec!["task", "add", "must not exist"],
        vec!["status"],
    ] {
        let out = pair.client(client, &args);
        assert_eq!(
            out.status.code(),
            Some(7),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?} wrote {}",
            text(&out.stdout)
        );
        pair.assert_skew_text(&format!("{args:?}"), &text(&out.stderr), client_protocol);
    }
    // MCP: the bridge surfaces the same text and stays up.
    let mut cmd = orch_command_with(
        client,
        &pair.sandbox.root(),
        &pair.sandbox.socket(),
        pair.sandbox.short_dir(),
    );
    cmd.env("ORCH_PROJECT_ROOT", pair.sandbox.root());
    let mut mcp = Mcp::spawn(cmd, &[]);
    let response = mcp.initialize();
    let failure = mcp_failure("mcp initialize", &response);
    pair.assert_skew_text("mcp initialize", &failure, client_protocol);
    assert_eq!(mcp.finish(), Some(0));

    // Nothing was started, repaired or written; the daemon is still the
    // original and still serves its own binary.
    assert!(watch.new_orch().is_empty(), "{:?}", watch.new_orch());
    assert_eq!(tree(&pair.sandbox.root()), before, "a skewed client wrote");
    let status = pair.client(pair.owner, &["--json", "daemon", "status"]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    let status: orch_proto::ops::results::DaemonStatus =
        serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status.pid, pair.daemon.pid);
}

#[test]
fn protocol_2_clients_against_a_protocol_1_daemon_print_the_skew_message_and_change_nothing() {
    let skewed = skewed_binary();
    let mut pair = Pair::start(normal(), orch_proto::LOCAL_PROTOCOL);
    assert_clients_refuse(&mut pair, skewed, SEAM_PROTOCOL);
}

#[test]
fn protocol_1_clients_against_a_protocol_2_daemon_print_the_skew_message_and_change_nothing() {
    let skewed = skewed_binary();
    let mut pair = Pair::start(skewed, SEAM_PROTOCOL);
    assert_clients_refuse(&mut pair, normal(), orch_proto::LOCAL_PROTOCOL);
}

#[test]
fn the_other_binarys_daemon_stop_crosses_the_skew_so_the_printed_restart_advice_works() {
    let skewed = skewed_binary();
    let mut pair = Pair::start(normal(), orch_proto::LOCAL_PROTOCOL);
    // `orch daemon stop` of the NEWER binary stops the OLDER daemon...
    let out = pair.client(skewed, &["daemon", "stop"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(pair.daemon.wait_exit(READY).code(), Some(0));
    assert!(!pair.sandbox.socket().exists(), "stop left the socket");
    // ...and the explicit start of the new binary then serves it.
    let mut cmd = orch_command_with(
        skewed,
        &pair.sandbox.root(),
        &pair.sandbox.socket(),
        pair.sandbox.short_dir(),
    );
    cmd.args(["daemon", "start"]);
    let mut second = DaemonChild::spawn(cmd, pair.sandbox.short_dir().join("second.err"));
    second.expect_ready(&pair.sandbox.socket());
    let status = pair.client(skewed, &["--json", "daemon", "status"]);
    assert!(status.status.success(), "{}", text(&status.stderr));
    // Reverse: the old client now refuses the new daemon, exit 7.
    let refused = pair.client(normal(), &["daemon", "status"]);
    assert_eq!(refused.status.code(), Some(7));
}
