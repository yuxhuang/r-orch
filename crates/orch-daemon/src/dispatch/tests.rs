//! The hosted loop end to end: a real `Running` daemon (no socket), its
//! timers and the `dispatch.*` handlers. Handler-level batch, expiry and
//! concurrent-tick vectors are in `tests/ops_control.rs`.

use crate::lifecycle::{BringUp, DispatchMode, Running, internal_context};
use crate::ops::Handler;
use orch_core::clock::{Clock, iso_from_epoch_ms};
use orch_core::storage::ConfigStore;
use orch_engine::container::EngineSeams;
use orch_proto::ops::args::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

pub(crate) struct TestClock(pub(crate) AtomicI64);
impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn now_iso(&self) -> String {
        iso_from_epoch_ms(self.now_ms())
    }
}

/// A private temp project with a 100 ms poll interval and its global config
/// inside the temp dir (never `~/.orchestry`).
pub(crate) async fn project() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::DirBuilderExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join(".orchestry"))
        .unwrap();
    let mut config = orch_domain::default_config();
    config.scheduling.poll_interval_ms = 100;
    orch_infra::storage::FileConfigStore::new(orch_infra::storage::Paths::new(&root))
        .write(&config)
        .await
        .unwrap();
    (temp, root)
}

pub(crate) fn bring_up(root: &Path, clock: Option<Arc<dyn Clock>>) -> BringUp {
    let mut seams = EngineSeams {
        global_config_path: Some(root.join("global.yml")),
        ..Default::default()
    };
    if let Some(clock) = clock {
        seams.clock = clock;
    }
    BringUp {
        project_root: root.to_path_buf(),
        socket: None,
        dispatch: DispatchMode::Off,
        listen: None,
        peer_name: None,
        seams,
        processes: None,
        versions: None,
        snapshot: crate::env::capture(std::env::vars_os()),
    }
}

async fn eventually(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !done().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn switch(dispatch: DispatchSwitch) -> DispatchSetArgs {
    DispatchSetArgs { dispatch }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosted_loop_dispatches_only_while_the_switch_is_on() {
    let (_temp, root) = project().await;
    let running = Running::bring_up(bring_up(&root, None)).await.unwrap();
    let (h, ctx) = (running.handler.clone(), internal_context());
    let agent = Handler::agent_create(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({
            "name": "sleeper", "adapter": "shell", "command": "exec /bin/sleep 60",
            "workspace_mode": "shared",
            "env": [{"name": "HOME", "value": root.to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"}]
        }))
        .unwrap(),
    )
    .await
    .unwrap()
    .agent;
    let task = Handler::task_create(
        &h,
        &ctx,
        serde_json::from_value(serde_json::json!({"title": "queued", "assignee": agent.id}))
            .unwrap(),
    )
    .await
    .unwrap();

    // A fresh daemon is OFF (R5): several poll intervals launch nothing.
    assert!(!h.dispatch_state().dispatch_on);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let engine = Arc::clone(&running.engine);
    assert!(engine.state.read().await.unwrap().running.is_empty());

    Handler::dispatch_set(&h, &ctx, switch(DispatchSwitch::On))
        .await
        .unwrap();
    // `serve --once` needs OFF; with the switch on it is refused.
    let refused = Handler::dispatch_until_terminal(&h, &ctx, DispatchUntilTerminalArgs {})
        .await
        .unwrap_err();
    assert_eq!(refused.exit_code, 2);
    eventually("the loop launches the queued task", async || {
        engine
            .state
            .read()
            .await
            .unwrap()
            .running
            .contains_key(&task.id)
    })
    .await;
    let pid = engine.state.read().await.unwrap().running[&task.id].pid;

    // OFF stops future launches only; the running child is untouched.
    Handler::dispatch_set(&h, &ctx, switch(DispatchSwitch::Off))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(engine.processes.is_alive(pid), "OFF must not kill runs");
    let state = engine.state.read().await.unwrap();
    assert_eq!(state.running.len(), 1, "exactly one launch, no duplicate");

    // A clean stop kills it with the existing grace and reconciles.
    assert!(running.shutdown().await.clean);
    assert!(!engine.processes.is_alive(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiry_keeps_running_with_dispatch_off() {
    let (_temp, root) = project().await;
    let clock = Arc::new(TestClock(AtomicI64::new(1_760_000_000_000)));
    let running = Running::bring_up(bring_up(&root, Some(clock.clone())))
        .await
        .unwrap();
    let (h, ctx) = (running.handler.clone(), internal_context());
    Handler::context_set(
        &h,
        &ctx,
        ContextSetArgs {
            key: "lease".try_into().unwrap(),
            value: "v".try_into().unwrap(),
            ttl_ms: Some(1_000),
        },
    )
    .await
    .unwrap();
    let path = running.engine.paths.context_path("lease").unwrap();
    // Not yet expired on the fake clock, however long the timer runs.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(path.exists());
    clock.0.fetch_add(1_001, Ordering::SeqCst);
    eventually("the maintenance timer expires the key", async || {
        !path.exists()
    })
    .await;
    assert!(!h.dispatch_state().dispatch_on);
    assert!(running.shutdown().await.clean);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_on_at_start_is_reported_and_toggles_off() {
    let (_temp, root) = project().await;
    let mut spec = bring_up(&root, None);
    spec.dispatch = DispatchMode::On;
    let running = Running::bring_up(spec).await.unwrap();
    let (h, ctx) = (running.handler.clone(), internal_context());
    assert!(h.dispatch_state().dispatch_on);
    let state = Handler::dispatch_set(&h, &ctx, switch(DispatchSwitch::Off))
        .await
        .unwrap();
    assert!(!state.dispatch_on);
    assert!(running.shutdown().await.clean);
}
