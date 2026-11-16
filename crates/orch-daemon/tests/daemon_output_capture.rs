#![cfg(feature = "testing")]
//! Direct writes must stay inside a scenario process, away from outer results.

use orch_daemon::testing::DaemonHarness;
use orch_domain::OrchestratorEvent;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

#[test]
fn direct_diagnostics_are_captured_before_the_outer_result_boundary() {
    let root = tempfile::Builder::new()
        .prefix("orch-output-oracle-")
        .tempdir_in("/tmp")
        .unwrap();
    for name in ["home", "tmp", "bin"] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.path().join(name))
            .unwrap();
    }
    for name in [
        "claude",
        "codex",
        "cursor",
        "cursor-agent",
        "agent",
        "opencode",
        "pi",
        "grok",
        "agy",
        "gemini",
    ] {
        let path = root.path().join("bin").join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 127\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(root.path().join("global.yml"), "{}\n").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "direct_diagnostic_fixture"])
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("TMPDIR", root.path().join("tmp"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("global.yml"),
        )
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.path().join("bin").display()),
        )
        .current_dir(root.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && output.stderr.is_empty()
            && !stdout.contains("OUTPUT_CAPTURE_SENTINEL")
            && stdout.contains("test direct_diagnostic_fixture ... ok")
            && stdout.contains("test result: ok. 1 passed; 0 failed;"),
        "direct writes escaped the scenario capture: {output:?}"
    );
}

#[tokio::test]
async fn direct_diagnostic_fixture() {
    if orch_daemon::testing::capture_test_output("direct_diagnostic_fixture") {
        return;
    }
    let daemon = DaemonHarness::start().await.unwrap();
    let mut follow = daemon.engine().events.subscribe();
    let event = OrchestratorEvent::OrchestratorError {
        error: "output fixture error".into(),
        context: "output fixture context".into(),
        fatal: false,
    };
    daemon.engine().events.emit(event.clone());
    std::io::stdout()
        .lock()
        .write_all(b"OUTPUT_CAPTURE_SENTINEL\n")
        .unwrap();
    assert!(
        follow
            .try_drain()
            .iter()
            .any(|received| **received == event)
    );
    daemon.shutdown().await.unwrap();
}
