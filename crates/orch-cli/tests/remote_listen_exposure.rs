//! The peer listener has no authentication, so binding it beyond loopback
//! must be an explicit choice and must say so (security review F1).
//! Exercised through the real binary: the warning is user-facing output.

use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;

const EXPOSURE: &str = "no authentication";

fn orch(project: &Path, home: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("NO_COLOR", "1")
        // No agent CLIs on PATH: nothing here may spawn a real one.
        .env("PATH", "/usr/bin:/bin")
        .env_remove("ORCHESTRY_DEBUG");
    command
}

struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        project
            .run(&["init", "--name", "p", "--adapter", "shell"])
            .assert()
            .success();
        project
    }

    fn run(&self, args: &[&str]) -> Command {
        let mut command = orch(self.dir.path(), self.home.path());
        command.args(args);
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.run(args).output().unwrap()
    }

    fn configured_listen(&self) -> String {
        let output = self.output(&["--json", "config", "get", "peer.listen"]);
        assert!(output.status.success(), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        value["value"]
            .as_str()
            .expect("peer.listen is set")
            .to_owned()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn remote_listen_on_without_an_address_binds_loopback_only_and_stays_quiet() {
    let project = Project::new();
    let output = project.output(&["remote", "listen", "on"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(project.configured_listen(), "127.0.0.1:7690");
    let all = text(&output.stdout) + &text(&output.stderr);
    assert!(!all.contains(EXPOSURE), "loopback warned: {all}");
    assert!(all.contains("127.0.0.1:7690"), "{all}");
}

#[test]
fn remote_listen_on_a_wildcard_address_persists_it_and_warns_on_stderr() {
    for addr in ["0.0.0.0:7690", "[::]:7690"] {
        let project = Project::new();
        let output = project.output(&["remote", "listen", "on", addr]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(project.configured_listen(), addr);
        let stderr = text(&output.stderr);
        for needle in [addr, "ALL network interfaces", EXPOSURE, "127.0.0.1"] {
            assert!(stderr.contains(needle), "{needle} missing: {stderr}");
        }
    }
}

#[test]
fn remote_listen_on_a_specific_lan_address_warns_on_stderr() {
    let project = Project::new();
    let output = project.output(&["remote", "listen", "on", "192.168.1.5:7690"]);
    assert!(output.status.success(), "{output:?}");
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("192.168.1.5:7690") && stderr.contains(EXPOSURE),
        "{stderr}"
    );
}

#[test]
fn an_engine_started_with_a_network_listen_address_warns_before_binding() {
    // 192.0.2.0/24 is TEST-NET-1: never a local address, so the bind fails
    // after the warning and nothing is ever exposed by this test.
    let project = Project::new();
    let output = project.output(&["serve", "--once", "--listen", "192.0.2.1:7690"]);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("192.0.2.1:7690") && stderr.contains(EXPOSURE),
        "{output:?}"
    );

    let project = Project::new();
    project
        .run(&["remote", "listen", "on", "192.0.2.1:7690"])
        .assert()
        .success();
    let output = project.output(&["run", "--watch"]);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("192.0.2.1:7690") && stderr.contains(EXPOSURE),
        "configured peer.listen did not warn: {output:?}"
    );
}

#[test]
fn an_engine_with_loopback_listening_does_not_warn() {
    let project = Project::new();
    let output = project.output(&["serve", "--once", "--listen", "127.0.0.1:0"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!text(&output.stderr).contains(EXPOSURE), "{output:?}");
}
