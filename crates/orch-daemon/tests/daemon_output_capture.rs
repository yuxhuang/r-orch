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

// These bodies are dormant during ordinary suite execution. Their parents run
// the exact fixture explicitly; the capture child retains only its own selector.
fn negative_scenario(name: &str) -> bool {
    std::env::var("ORCH_CAPTURE_NEGATIVE").as_deref() == Ok(name)
        || std::env::var("ORCH_TEST_OUTPUT_CAPTURE").as_deref() == Ok(name)
}

fn negative_output(name: &str) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("home")).unwrap();
    std::fs::write(root.path().join("global.yml"), "{}\n").unwrap();
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name])
        .env_clear()
        .env("ORCH_CAPTURE_NEGATIVE", name)
        .env("HOME", root.path().join("home"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("global.yml"),
        )
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root.path())
        .output()
        .unwrap()
}

#[test]
fn failed_capture_fixture() {
    let name = "failed_capture_fixture";
    if !negative_scenario(name) || orch_daemon::testing::capture_test_output(name) {
        return;
    }
    std::io::stdout()
        .lock()
        .write_all(b"CHILD_STDOUT_DETAIL\n")
        .unwrap();
    std::io::stderr()
        .lock()
        .write_all(b"CHILD_STDERR_DETAIL\n")
        .unwrap();
    panic!("CHILD_ASSERTION_DETAIL");
}

#[test]
fn private_home_writer_fixture() {
    let name = "private_home_writer_fixture";
    if !negative_scenario(name) || orch_daemon::testing::capture_test_output(name) {
        return;
    }
    std::fs::write(
        std::path::Path::new(&std::env::var_os("HOME").unwrap()).join("planted-writer"),
        "unexpected",
    )
    .unwrap();
}

#[test]
fn ai_blocker_fixture() {
    let name = "ai_blocker_fixture";
    if !negative_scenario(name) || orch_daemon::testing::capture_test_output(name) {
        return;
    }
    let status = std::process::Command::new("codex").status().unwrap();
    assert_eq!(status.code(), Some(127));
}

#[test]
fn failed_capture_keeps_child_diagnostics_as_labelled_data() {
    let output = negative_output("failed_capture_fixture");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    for detail in [
        "CHILD_STDOUT_DETAIL",
        "CHILD_STDERR_DETAIL",
        "CHILD_ASSERTION_DETAIL",
    ] {
        assert!(stdout.contains(detail), "{stdout}");
    }
    assert!(stdout.contains("child stdout | failures:"), "{stdout}");
    assert!(stdout.contains("child stderr | "), "{stdout}");
    assert_eq!(
        stdout.matches("\nfailures:\n").count(),
        2,
        "child failure list escaped: {stdout}"
    );
}

#[test]
fn private_home_writes_fail_before_capture_cleanup() {
    let output = negative_output("private_home_writer_fixture");
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("planted-writer"), "{stdout}");
    assert!(
        stdout.contains("child stdout | test result: ok. 1 passed;"),
        "{stdout}"
    );
}

#[test]
fn ai_blocker_invocation_fails_capture_even_if_body_accepts_127() {
    let output = negative_output("ai_blocker_fixture");
    assert_eq!(output.status.code(), Some(101), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("ai-cli-calls"),
        "{output:?}"
    );
}
