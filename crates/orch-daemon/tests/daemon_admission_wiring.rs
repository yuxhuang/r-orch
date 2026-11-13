#![cfg(feature = "testing")]
//! Production assembly must classify a sealed preparation as cancellation,
//! even before the engine's shutdown flag is set.

mod support;

use async_trait::async_trait;
use orch_core::process::{SpawnOptions, SpawnedChild};
use orch_core::{ProcessManager, Result, Signal};
use orch_daemon::testing::DaemonHarness;
use orch_domain::{OrchestratorEventType, RunStatus};
use serde_json::json;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use support::{DEADLINE, Raw, ok, task};
use tokio::sync::Notify;

/// The real OpenCode adapter reaches this non-forking version probe during
/// preparation. Its failure is released only after production admission seals.
#[derive(Default)]
struct HeldProbe {
    entered: Notify,
    gate: (Mutex<bool>, Condvar),
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl HeldProbe {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

/// Release even on panic or timeout: no failed assertion can park a worker.
struct ReleaseProbe(Arc<HeldProbe>);
impl Drop for ReleaseProbe {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[async_trait]
impl ProcessManager for HeldProbe {
    fn is_alive(&self, _: u32) -> bool {
        false
    }

    fn kill(&self, _: u32, _: Signal) {}

    async fn kill_with_grace(&self, _: u32, _: Duration) {}

    fn spawn(&self, command: &str, args: &[String], _: SpawnOptions) -> Result<SpawnedChild> {
        self.calls
            .lock()
            .unwrap()
            .push((command.to_owned(), args.to_vec()));
        assert_eq!(
            (command, args),
            ("opencode", ["--version".into()].as_slice())
        );
        self.entered.notify_one();
        // Tell Tokio this synchronous fake is blocking so the barrier's waiter
        // and daemon work remain runnable. This never creates an OS child.
        tokio::task::block_in_place(|| {
            let mut released = self.gate.0.lock().unwrap();
            while !*released {
                released = self.gate.1.wait(released).unwrap();
            }
        });
        Err(orch_core::Error::other(
            "isolated preparation probe refused",
        ))
    }
}

async fn sealed_preparation() {
    let processes = Arc::new(HeldProbe::default());
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .processes(processes.clone())
        .on_socket()
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    // A non-legacy effort makes the refused wire probe a preparation error,
    // rather than a legacy fallback warning followed by final launch admission.
    let reply = raw
        .request(
            100,
            "agent.create",
            json!({
                "name": "held-probe", "adapter": "opencode", "workspace_mode": "shared",
                "effort": "xhigh", "model": "openrouter/anthropic/claude-opus-5.5"
            }),
        )
        .await;
    let agent_id = ok(&reply)["agent"]["id"].as_str().unwrap().to_owned();
    let id = task(
        &mut raw,
        "cancel a sealed preparation",
        json!({"assignee": agent_id, "max_attempts": 1}),
    )
    .await;
    drop(raw);
    let full = daemon.engine();
    let before = full.tasks.get(&id).await.unwrap().unwrap();
    let agent_before = full.agents.get(&agent_id).await.unwrap().unwrap();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    let _subscription = full
        .events
        .on(OrchestratorEventType::OrchestratorError, move |event| {
            sink.lock().unwrap().push(event.clone());
        });

    let _release = ReleaseProbe(processes.clone());
    let orchestrator = full.orchestrator.clone();
    let launch_id = id.clone();
    let mut launch = tokio::spawn(async move { orchestrator.daemon_launch_task(&launch_id).await });
    tokio::select! {
        biased;
        result = &mut launch => panic!("launch missed the preparation barrier: {result:?}"),
        () = processes.entered.notified() => (),
    }
    let preparing = full.runs.list_for_task(&id).await.unwrap();
    assert_eq!(preparing.len(), 1);
    assert_eq!(preparing[0].status, RunStatus::Preparing);
    let calls_at_seal = processes.calls.lock().unwrap().clone();
    assert_eq!(calls_at_seal.len(), 1);

    // This is the daemon's existing seal step, not a replacement admission
    // provider or a stop that could mask missing wiring by setting the flag.
    daemon.seal_admission();
    assert!(!full.orchestrator.is_shutting_down());
    processes.release();
    let outcome = launch.await.unwrap();

    let runs = full.runs.list_for_task(&id).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].status,
        RunStatus::Cancelled,
        "the production admission seal must cancel the preparing run"
    );
    assert_eq!(runs[0].id, preparing[0].id);
    assert!(runs[0].error.is_none(), "{:?}", runs[0].error);
    outcome.unwrap();
    assert_eq!(
        full.tasks.get(&id).await.unwrap().unwrap(),
        before,
        "a sealed preparation must preserve status and attempts"
    );
    assert_eq!(
        full.agents.get(&agent_id).await.unwrap().unwrap(),
        agent_before,
        "cancellation must not charge failure statistics or an agent error"
    );
    assert!(
        errors.lock().unwrap().is_empty(),
        "a sealed preparation emitted an orchestrator error: {:?}",
        errors.lock().unwrap()
    );
    assert_eq!(
        *processes.calls.lock().unwrap(),
        calls_at_seal,
        "no process spawn may be attempted after the seal"
    );
    assert!(full.state.read().await.unwrap().claimed.is_empty());
    // Keep this after observing the outcome: the wired tracker is the only
    // stop signal available to the preparation recorder throughout the test.
    assert!(!full.orchestrator.is_shutting_down());
    // This isolated early seal closes requests before any stop, so the internal
    // stop request is refused and the harness reports an incomplete stop. This
    // is not a daemon-stop contract: ordinary shutdown seals requests only after
    // the engine stopped. A clean result here means the internal stop no longer
    // passes request admission; update this pin when that changes.
    let shutdown_started = Instant::now();
    let shutdown = daemon.shutdown().await;
    eprintln!(
        "early-seal harness shutdown in {:?}: {shutdown:?}",
        shutdown_started.elapsed()
    );
    assert!(
        matches!(shutdown, Err(orch_daemon::DaemonError::StopIncomplete)),
        "early request seal must report StopIncomplete; if the internal stop no longer passes request admission, update this pin: {shutdown:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_admission_seal_cancels_preparation_before_shutdown_flag() {
    tokio::time::timeout(DEADLINE, sealed_preparation())
        .await
        .expect("admission observation and harness shutdown must return");
}
