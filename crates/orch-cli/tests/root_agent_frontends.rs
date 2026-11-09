//! The root `--agent` is the caller identity for the dashboard and `orch web`
//! too (design R8), not only for the one-shot commands and `orch mcp`.
//!
//! The identity only rides on the daemon handshake, so the observable seam is
//! its validation: an `--agent` the protocol cannot carry is refused (exit 2)
//! before anything connects. A front end that dropped the flag would instead
//! dial the (absent) daemon and print the no-daemon message with exit 7.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::Builder::new()
            .prefix("orch-root-agent-")
            .tempdir()
            .expect("tempdir");
        std::fs::create_dir_all(dir.path().join("home")).expect("home");
        std::fs::create_dir_all(dir.path().join("p")).expect("project");
        let fx = Fixture { dir };
        let init = fx.orch(&["init", "--name", "root-agent", "--adapter", "shell"]);
        assert!(init.status.success(), "init: {}", stderr(&init));
        fx
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn orch(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_orch"))
            .env_clear()
            .current_dir(self.root())
            .env("HOME", self.home())
            .env("PATH", "/usr/bin:/bin")
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.home().join("global.yml"),
            )
            // No daemon ever listens here: nothing in this file starts one.
            .env("ORCH_SOCKET", socket(self.dir.path()))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run orch")
    }
}

fn socket(dir: &Path) -> PathBuf {
    dir.join("s")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// One byte past the protocol's identifier bound (orch-proto MAX_ID_BYTES).
fn oversized_agent() -> String {
    "a".repeat(257)
}

#[test]
fn root_agent_reaches_the_dashboard_and_web() {
    let fx = Fixture::new();
    let agent = oversized_agent();
    for front_end in ["tui", "web"] {
        let out = fx.orch(&["--agent", &agent, front_end]);
        assert_eq!(out.status.code(), Some(2), "{front_end}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("invalid --agent"),
            "{front_end}: {}",
            stderr(&out)
        );
    }
    // A bare invocation is the dashboard as well.
    let out = fx.orch(&["--agent", &agent]);
    assert_eq!(out.status.code(), Some(2), "bare: {}", stderr(&out));
}

#[test]
fn without_root_agent_the_front_ends_report_no_daemon() {
    let fx = Fixture::new();
    for front_end in ["tui", "web"] {
        let out = fx.orch(&[front_end]);
        assert_eq!(out.status.code(), Some(7), "{front_end}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("No orch daemon is running for"),
            "{front_end}: {}",
            stderr(&out)
        );
    }
    assert!(
        !socket(fx.dir.path()).exists(),
        "no client creates a socket"
    );
}
