#![cfg(feature = "testing")]
//! A9: the production snapshot/PATH route, with an isolated in-process daemon.
mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Instant;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use support::{DEADLINE, Raw, Reaper, ok, running_pid, task};

const SCENARIO: &str = "ORCH_TEST_SPAWN_GATE_ROOT";

#[test]
fn cancel_and_stop_survive_overlapping_bare_name_diagnostics() {
    if let Some(root) = std::env::var_os(SCENARIO) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(scenario(Path::new(&root)));
        return;
    }
    // Environment changes belong in a fresh process, never set_var in a test.
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin = root.path().join("bin");
    let project = root.path().join("project");
    for dir in [&home, &bin, &project.join(".orchestry")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        r#"#!/bin/sh
case "$*" in
  *--help*)
    printf '%s' "$$" > "$HOME/probe.pid"
    exec /bin/sleep 120
    ;;
  *--version*) printf 'fake 1.0\n'; exit 0 ;;
esac
/bin/sleep 120 &
child=$!
printf '%s' "$child" > "$HOME/descendant.pid"
printf '%s' "$$" > "$HOME/agent.pid"
trap 'kill "$child" 2>/dev/null; wait "$child" 2>/dev/null; exit 0' TERM INT HUP
wait "$child"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700)).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            use orch_core::ProcessManager;
            for name in ["agent.pid", "probe.pid", "descendant.pid"] {
                if let Ok(text) = std::fs::read_to_string(self.0.join(name))
                    && let Ok(pid) = text.parse()
                {
                    orch_infra::process::SystemProcessManager
                        .kill(pid, orch_core::process::Signal::Kill);
                }
            }
        }
    }
    let _cleanup = Cleanup(home.clone());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cancel_and_stop_survive_overlapping_bare_name_diagnostics",
            "--nocapture",
        ])
        .env_clear()
        .env(SCENARIO, root.path())
        .env("HOME", &home)
        .env("PATH", &bin)
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", home.join("global.yml"))
        .env_remove("ORCH_AGENT_ID")
        .env_remove("ORCH_AGENT_NAME")
        .env_remove("ORCH_TASK_ID")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "isolated daemon scenario failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn pidfile(home: &Path, name: &str) -> u32 {
    tokio::time::timeout(DEADLINE, async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(home.join(name)).await
                && let Ok(pid) = text.parse()
            {
                return pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake executable reached through bare name and child PATH")
}

async fn scenario(root: &Path) {
    for name in ["ORCH_AGENT_ID", "ORCH_AGENT_NAME", "ORCH_TASK_ID"] {
        assert!(std::env::var_os(name).is_none(), "test inherited {name}");
    }
    let home = root.join("home");
    assert_eq!(std::env::var_os("HOME").unwrap(), home);
    let daemon = DaemonHarness::builder()
        .on_socket()
        .global_config_path(home.join("global.yml"))
        .start_in(&root.join("project"))
        .await
        .unwrap();
    assert!(daemon.socket().unwrap().as_os_str().len() < 100);
    let reaper = Reaper::new(&daemon);
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let mut probe = Raw::session(&daemon, "cli", "normal").await;
    let diagnostics = tokio::spawn(async move {
        probe
            .request(1, "models.catalog", json!({"adapters": ["claude"]}))
            .await
    });
    let probe_pid = pidfile(&home, "probe.pid").await;
    reaper.watch(probe_pid);
    let reply = owner
        .request(
            2,
            "agent.create",
            json!({
                "name":"fake spawn-gate agent", "adapter":"claude", "workspace_mode":"shared",
                "approval_policy":"manual"
            }),
        )
        .await;
    let agent = ok(&reply)["agent"]["id"].as_str().unwrap().to_owned();
    let id = task(
        &mut owner,
        "cancel the fake agent",
        json!({"assignee":agent}),
    )
    .await;
    let tick = owner.request(3, "dispatch.tick", json!({})).await;
    assert_eq!(ok(&tick)["launched"], json!([id]), "{tick}");
    let agent_pid = pidfile(&home, "agent.pid").await;
    let descendant = pidfile(&home, "descendant.pid").await;
    reaper.watch(agent_pid);
    reaper.watch(descendant);
    assert_eq!(running_pid(&daemon, &id).await, Some(agent_pid));
    assert!(reaper.alive(probe_pid) && reaper.alive(agent_pid) && reaper.alive(descendant));
    let cancel_start = Instant::now();
    let cancelled = owner.request(4, "task.cancel", json!({"id":id})).await;
    assert_eq!(ok(&cancelled)["status"], "cancelled");
    let cancel_elapsed = cancel_start.elapsed();
    assert!(cancel_elapsed < DEADLINE);
    assert!(
        !reaper.alive(agent_pid),
        "cancel returned before agent died"
    );
    tokio::time::timeout(DEADLINE, async {
        while reaper.alive(descendant) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancel kills the agent's process group");
    assert!(
        reaper.alive(probe_pid),
        "probe still overlaps cancel and stop"
    );
    let stop_start = Instant::now();
    let stopped = owner.request(5, "daemon.stop", json!({})).await;
    ok(&stopped);
    tokio::time::timeout(DEADLINE, daemon.shutdown())
        .await
        .expect("daemon stop within existing harness budget")
        .unwrap();
    assert!(stop_start.elapsed() < DEADLINE);
    let _reply = tokio::time::timeout(DEADLINE, diagnostics)
        .await
        .expect("diagnostic request drained")
        .unwrap();
    assert!(
        !reaper.alive(probe_pid),
        "stop reaps abandoned diagnostic group"
    );
    eprintln!(
        "cancel {:?}; stop {:?}; existing budget {:?}",
        cancel_elapsed,
        stop_start.elapsed(),
        DEADLINE
    );
}
