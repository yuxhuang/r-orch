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

fn scenario_environment(root: &Path) -> [(std::ffi::OsString, std::ffi::OsString); 2] {
    [
        ("HOME".into(), root.join("home").into_os_string()),
        ("PATH".into(), root.join("bin").into_os_string()),
    ]
}

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
        .environment(scenario_environment(root))
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

#[cfg(target_os = "macos")]
#[test]
fn held_diagnostic_keeps_control_and_owned_drain_available() {
    if let Some(root) = std::env::var_os(SCENARIO) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(held_scenario(Path::new(&root)));
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
printf started > "$HOME/$ORCH_TASK_ID.started"
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
            "held_diagnostic_keeps_control_and_owned_drain_available",
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
    let deadline = Instant::now() + 3 * DEADLINE;
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

#[cfg(target_os = "macos")]
struct HeldSpawn {
    entered: tokio::sync::Notify,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}
#[cfg(target_os = "macos")]
impl orch_infra::process::gate::SpawnObserver for HeldSpawn {
    fn observe(&self, event: orch_infra::process::gate::SpawnObservation) {
        use orch_infra::process::gate::{SpawnSite, SpawnStage};
        if event.site == SpawnSite::Discovery && event.stage == SpawnStage::Entered {
            let release = self.release.lock().unwrap().take();
            if let Some(release) = release {
                self.entered.notify_one();
                release
                    .recv_timeout(3 * DEADLINE)
                    .expect("release real diagnostic spawn");
            }
        }
    }
}
#[cfg(target_os = "macos")]
struct ReleaseSpawn(Option<std::sync::mpsc::Sender<()>>);
#[cfg(target_os = "macos")]
impl Drop for ReleaseSpawn {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}
#[cfg(target_os = "macos")]
async fn queued_launch() {
    use orch_infra::process::gate::{SpawnSite, SpawnStage, pending_spawns};
    tokio::time::timeout(DEADLINE, async {
        loop {
            if pending_spawns().iter().any(|record| {
                record.site == SpawnSite::Launch && record.stage == SpawnStage::Waiting
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("healthy final launch reached asynchronous admission");
}
#[cfg(target_os = "macos")]
async fn held_scenario(root: &Path) {
    use std::sync::Arc;
    let home = root.join("home");
    let daemon = DaemonHarness::builder()
        .on_socket()
        .global_config_path(home.join("global.yml"))
        .environment(scenario_environment(root))
        .start_in(&root.join("project"))
        .await
        .unwrap();
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let mut launch = Raw::session(&daemon, "cli", "normal").await;
    let mut probe = Raw::session(&daemon, "cli", "normal").await;
    let reaper = Reaper::new(&daemon);
    let make = |name: &str| json!({"name":name,"adapter":"claude","workspace_mode":"shared","approval_policy":"manual"});
    let a = owner
        .request(1, "agent.create", make("registered live agent"))
        .await;
    let a = ok(&a)["agent"]["id"].as_str().unwrap().to_owned();
    let live = task(&mut owner, "already running", json!({"assignee":a})).await;
    ok(&owner.request(2, "dispatch.tick", json!({})).await);
    let pid = pidfile(&home, "agent.pid").await;
    assert_eq!(running_pid(&daemon, &live).await, Some(pid));
    let descendant = pidfile(&home, "descendant.pid").await;
    reaper.watch(pid);
    reaper.watch(descendant);
    assert!(reaper.alive(pid) && reaper.alive(descendant));
    let b = owner.request(3, "agent.create", make("queued agent")).await;
    let b = ok(&b)["agent"]["id"].as_str().unwrap().to_owned();
    let queued = task(&mut owner, "cancel while queued", json!({"assignee":b})).await;
    let (send, receive) = std::sync::mpsc::channel();
    let mut release = ReleaseSpawn(Some(send));
    let observer = Arc::new(HeldSpawn {
        entered: tokio::sync::Notify::new(),
        release: std::sync::Mutex::new(Some(receive)),
    });
    daemon.observe_spawns(observer.clone());
    let diagnostic = tokio::spawn(async move {
        probe
            .request(4, "models.catalog", json!({"adapters":["claude"]}))
            .await
    });
    tokio::time::timeout(DEADLINE, observer.entered.notified())
        .await
        .unwrap();
    let launch = tokio::spawn(async move { launch.request(5, "dispatch.tick", json!({})).await });
    queued_launch().await;
    // Every assertion in this block precedes holder release.
    eprintln!(
        "CONTROL: healthy final launch queued behind held diagnostic; checking read before release"
    );
    let read = owner.request(6, "task.get", json!({"id":queued})).await;
    if read.get("ok").is_none() {
        // Preserve the failed before-release oracle, then drain the real daemon
        // before unwinding its runtime during a containment negative control.
        release.0.take().unwrap().send(()).unwrap();
        let drained = tokio::time::timeout(DEADLINE, daemon.shutdown()).await;
        assert!(
            drained.is_ok(),
            "negative-control daemon did not drain: {read}"
        );
        panic!("independent control read failed before diagnostic holder release: {read}");
    }
    let cancelled = owner.request(7, "task.cancel", json!({"id":queued})).await;
    assert_eq!(ok(&cancelled)["status"], "cancelled");
    let reply = tokio::time::timeout(DEADLINE, launch)
        .await
        .expect("queued cancellation returns before holder release")
        .unwrap();
    ok(&reply);
    assert!(
        !daemon
            .engine()
            .state
            .read()
            .await
            .unwrap()
            .claimed
            .contains(&queued),
        "cancelled preparation retained its claim"
    );
    assert_eq!(
        daemon
            .engine()
            .tasks
            .get(&queued)
            .await
            .unwrap()
            .unwrap()
            .attempts,
        0
    );
    assert!(
        !home.join(format!("{queued}.started")).exists(),
        "cancelled waiter spawned before holder release"
    );
    owner
        .send(&json!({"id":8,"req":{"op":"daemon.stop","args":{}}}))
        .await;
    tokio::time::timeout(DEADLINE, async {
        while reaper.alive(pid) || reaper.alive(descendant) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registered group stops before held diagnostic is released");
    let stopped = owner.terminal(8).await;
    assert!(
        stopped.get("err").is_some(),
        "held entered work must report incomplete shutdown: {stopped}"
    );
    let project = root.join("project");
    let rival = DaemonHarness::builder()
        .global_config_path(home.join("global.yml"))
        .start_in(&project)
        .await;
    assert!(
        rival.is_err(),
        "ownership released while an entered job can still spawn"
    );
    let drain = tokio::spawn(daemon.shutdown());
    tokio::task::yield_now().await;
    assert!(
        !drain.is_finished(),
        "late spawn must remain owned through lifecycle drain"
    );
    if let Some(send) = release.0.take() {
        send.send(()).unwrap();
    }
    let _ = tokio::time::timeout(DEADLINE, diagnostic)
        .await
        .unwrap()
        .unwrap();
    assert!(
        tokio::time::timeout(DEADLINE, drain)
            .await
            .expect("release permits late cleanup and drain")
            .unwrap()
            .is_err(),
        "incomplete stop must not become a clean result"
    );
    assert!(
        !home.join(format!("{queued}.started")).exists(),
        "cancelled waiter spawned late"
    );
    // A new owner is possible only after owned late cleanup is finished.
    let successor = DaemonHarness::builder()
        .global_config_path(home.join("global.yml"))
        .start_in(&project)
        .await
        .unwrap();
    successor.shutdown().await.unwrap();
}

#[test]
fn stop_with_both_live_agent_and_live_probe() {
    if let Some(root) = std::env::var_os(SCENARIO) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(live_stop_scenario(Path::new(&root)));
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
            "stop_with_both_live_agent_and_live_probe",
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

async fn live_stop_scenario(root: &Path) {
    for name in ["ORCH_AGENT_ID", "ORCH_AGENT_NAME", "ORCH_TASK_ID"] {
        assert!(std::env::var_os(name).is_none(), "test inherited {name}");
    }
    let home = root.join("home");
    assert_eq!(std::env::var_os("HOME").unwrap(), home);
    let daemon = DaemonHarness::builder()
        .on_socket()
        .global_config_path(home.join("global.yml"))
        .environment(scenario_environment(root))
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
    assert!(
        reaper.alive(probe_pid) && reaper.alive(agent_pid) && reaper.alive(descendant),
        "stop must start with both diagnostic and registered agent group alive"
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
    assert!(
        !reaper.alive(agent_pid) && !reaper.alive(descendant),
        "stop must terminate the registered group"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn pause_while_queued_prevents_late_launch() {
    if let Some(root) = std::env::var_os(SCENARIO) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(paused_scenario(Path::new(&root)));
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
printf started > "$HOME/$ORCH_TASK_ID.started"
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
            "pause_while_queued_prevents_late_launch",
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
    let deadline = Instant::now() + 3 * DEADLINE;
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

#[cfg(target_os = "macos")]
async fn paused_scenario(root: &Path) {
    use std::sync::Arc;
    let home = root.join("home");
    let daemon = DaemonHarness::builder()
        .on_socket()
        .global_config_path(home.join("global.yml"))
        .environment(scenario_environment(root))
        .start_in(&root.join("project"))
        .await
        .unwrap();
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let mut launch = Raw::session(&daemon, "cli", "normal").await;
    let mut probe = Raw::session(&daemon, "cli", "normal").await;
    let agent = owner.request(1,"agent.create",json!({"name":"queued on goal","adapter":"claude","workspace_mode":"shared","approval_policy":"manual"})).await;
    let agent = ok(&agent)["agent"]["id"].as_str().unwrap().to_owned();
    let goal = owner
        .request(2, "goal.create", json!({"title":"pause while waiting"}))
        .await;
    let goal = ok(&goal)["id"].as_str().unwrap().to_owned();
    let queued = task(
        &mut owner,
        "must not spawn after pause",
        json!({"assignee":agent,"goal_id":goal}),
    )
    .await;
    let (send, receive) = std::sync::mpsc::channel();
    let mut release = ReleaseSpawn(Some(send));
    let observer = Arc::new(HeldSpawn {
        entered: tokio::sync::Notify::new(),
        release: std::sync::Mutex::new(Some(receive)),
    });
    daemon.observe_spawns(observer.clone());
    let diagnostic = tokio::spawn(async move {
        probe
            .request(4, "models.catalog", json!({"adapters":["claude"]}))
            .await
    });
    tokio::time::timeout(DEADLINE, observer.entered.notified())
        .await
        .unwrap();
    let launch = tokio::spawn(async move { launch.request(5, "dispatch.tick", json!({})).await });
    queued_launch().await;
    let paused = owner
        .request(6, "goal.status", json!({"id":goal,"status":"paused"}))
        .await;
    assert_eq!(
        ok(&paused)["status"],
        "paused",
        "pause must commit while diagnostic still holds gate"
    );
    assert!(!home.join(format!("{queued}.started")).exists());
    release.0.take().unwrap().send(()).unwrap();
    let reply = tokio::time::timeout(DEADLINE, launch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ok(&reply)["launched"], json!([]));
    assert!(
        !home.join(format!("{queued}.started")).exists(),
        "paused queued task launched after release"
    );
    // Cancel the long-lived fake diagnostic through its observation context.
    diagnostic.abort();
    daemon.shutdown().await.unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn forwarded_effort_cancellation_drops_both_preparations_before_release() {
    if let Some(root) = std::env::var_os(SCENARIO) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(effort_scenario(Path::new(&root)));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for dir in [
        root.path().join("home"),
        root.path().join("bin"),
        root.path().join("project/.orchestry"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let script = root.path().join("bin/opencode");
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf invoked >> "$HOME/invoked"
case "$*" in
  *--version*) printf 'opencode v2.1.2\n'; exit 0 ;;
  *--help*) printf '%s\n' '--model provider/model#variant'; exit 0 ;;
esac
printf started > "$HOME/$ORCH_TASK_ID.started"
"#,
    )
    .unwrap();
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
    struct Owned(Option<std::process::Child>);
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut()
                && !matches!(child.try_wait(), Ok(Some(_)))
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "forwarded_effort_cancellation_drops_both_preparations_before_release",
            "--nocapture",
        ])
        .env_clear()
        .env(SCENARIO, root.path())
        .env("HOME", root.path().join("home"))
        .env("PATH", root.path().join("bin"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("home/global.yml"),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut owned = Owned(Some(child));
    let child = owned.0.as_mut().unwrap();
    let deadline = Instant::now() + 3 * DEADLINE;
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let watchdog = child.try_wait().unwrap().is_none();
    if watchdog {
        child.kill().unwrap();
    }
    let output = owned.0.take().unwrap().wait_with_output().unwrap();
    assert!(
        !watchdog && output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "macos")]
async fn effort_scenario(root: &Path) {
    use orch_infra::process::gate::{SpawnSite, SpawnStage, pending_spawns};
    use std::sync::Arc;
    let home = root.join("home");
    let daemon = DaemonHarness::builder()
        .on_socket()
        .global_config_path(home.join("global.yml"))
        .environment(scenario_environment(root))
        .start_in(&root.join("project"))
        .await
        .unwrap();
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let mut probe = Raw::session(&daemon, "cli", "normal").await;
    let mut tasks = Vec::new();
    for name in ["first effort", "second effort"] {
        let a = owner
            .request(
                1,
                "agent.create",
                json!({"name":name,"adapter":"opencode",
            "workspace_mode":"shared","approval_policy":"manual",
            "model":"openrouter/anthropic/claude-opus-5.5","effort":"xhigh"}),
            )
            .await;
        let agent = ok(&a)["agent"]["id"].as_str().unwrap().to_owned();
        tasks.push(task(&mut owner, name, json!({"assignee":agent})).await);
    }
    let (send, receive) = std::sync::mpsc::channel();
    let mut release = ReleaseSpawn(Some(send));
    let observer = Arc::new(HeldSpawn {
        entered: tokio::sync::Notify::new(),
        release: std::sync::Mutex::new(Some(receive)),
    });
    daemon.observe_spawns(observer.clone());
    let diagnostic = tokio::spawn(async move {
        probe
            .request(4, "models.catalog", json!({"adapters":["opencode"]}))
            .await
    });
    tokio::time::timeout(DEADLINE, observer.entered.notified())
        .await
        .unwrap();
    let launches: Vec<_> = tasks
        .iter()
        .map(|task| {
            let orchestrator = daemon.engine().orchestrator.clone();
            let task = task.clone();
            tokio::spawn(async move { orchestrator.daemon_launch_task(&task).await })
        })
        .collect();
    let result = async {
        tokio::time::timeout(DEADLINE, async {
            while pending_spawns().iter().filter(|r| r.site == SpawnSite::Version && r.stage == SpawnStage::Waiting).count() != 2 {
                tokio::task::yield_now().await;
            }
        }).await.map_err(|_| "two forwarded-effort preparations never reached admission")?;
        for task in &tasks {
            let read = owner.request(5, "task.get", json!({"id":task})).await;
            if read.get("ok").is_none() { return Err("independent read failed before release"); }
            let cancelled = owner.request(6, "task.cancel", json!({"id":task})).await;
            if ok(&cancelled)["status"] != "cancelled" { return Err("cancel failed before release"); }
        }
        for launch in launches {
            tokio::time::timeout(DEADLINE, launch).await.map_err(|_| "preparation cancellation blocked before release")?
                .map_err(|_| "preparation panicked")?.map_err(|_| "preparation failed")?;
        }
        for task in &tasks {
            let stored = daemon.engine().tasks.get(task).await.unwrap().unwrap();
            if stored.attempts != 0 || daemon.engine().state.read().await.unwrap().claimed.contains(task) {
                return Err("cancel consumed attempt or retained claim");
            }
        }
        if home.join("invoked").exists() { return Err("queued effort executable entered before release"); }
        eprintln!("EFFORT: two confirmed waiters, both controls/cancellations complete before release; attempts=0, no executable");
        Ok(())
    }.await;
    release.0.take().unwrap().send(()).unwrap();
    let _ = tokio::time::timeout(DEADLINE, diagnostic).await;
    daemon.shutdown().await.unwrap();
    for task in &tasks {
        assert!(
            !home.join(format!("{task}.started")).exists(),
            "late model launch"
        );
    }
    result.unwrap();
}
