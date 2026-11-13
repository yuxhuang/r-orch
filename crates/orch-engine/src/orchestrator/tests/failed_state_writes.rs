//! Failed state writes must not retain uncommitted selections or prevent stop.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orch_core::error::{Error, Result};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_core::storage::StateStore;
use orch_core::test_support::{FakeStateStore, make_agent, make_run, make_task};
use orch_domain::{OrchestratorState, Run, RunStatus, Task, TaskStatus};

use super::harness::{Harness, NOW_ISO, running_entry};

/// Reads always work; writes fail while `fail` is set, or for the next `once` writes.
struct WriteFails {
    inner: Arc<FakeStateStore>,
    fail: Arc<AtomicBool>,
    once: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl StateStore for WriteFails {
    async fn read(&self) -> Result<OrchestratorState> {
        self.inner.read().await
    }
    async fn write(&self, state: &OrchestratorState) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::other("disk full"));
        }
        if self
            .once
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(Error::other("transient state write failure"));
        }
        self.inner.write(state).await
    }
}

/// Controlled live child: only the existing graceful-stop path can retire it.
#[derive(Default)]
struct Killable {
    killed: Mutex<Vec<u32>>,
}

#[async_trait::async_trait]
impl ProcessManager for Killable {
    fn is_alive(&self, pid: u32) -> bool {
        !self.killed.lock().unwrap().contains(&pid)
    }
    fn kill(&self, _: u32, _: Signal) {
        panic!("unexpected direct signal");
    }
    async fn kill_with_grace(&self, pid: u32, _: Duration) {
        self.killed.lock().unwrap().push(pid);
    }
    fn spawn(&self, _: &str, _: &[String], _: SpawnOptions) -> Result<SpawnedChild> {
        panic!("no process spawn expected");
    }
}

fn adopted(
    n: usize,
    processes: Arc<dyn ProcessManager>,
    store: Option<(Arc<AtomicBool>, Arc<AtomicUsize>)>,
) -> Harness {
    let mut state = OrchestratorState::default();
    let mut tasks = Vec::new();
    let mut runs = Vec::new();
    for i in 0..n {
        let task_id = format!("tsk_s{i}");
        let run_id = format!("run_s{i}");
        let pid = 40_000 + i as u32;
        state.running.insert(
            task_id.clone(),
            running_entry(&task_id, "agt_test1", &run_id, pid, NOW_ISO),
        );
        tasks.push(Task {
            id: task_id.clone(),
            status: TaskStatus::InProgress,
            assignee: Some("agt_test1".into()),
            ..make_task()
        });
        runs.push(Run {
            id: run_id,
            task_id,
            status: RunStatus::Running,
            pid: Some(pid),
            ..make_run()
        });
    }
    let mut builder = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .state(state)
        .processes(processes);
    if let Some((fail, once)) = store {
        builder = builder.wrap_state_store(move |inner| {
            Arc::new(WriteFails { inner, fail, once }) as Arc<dyn StateStore>
        });
    }
    let h = builder.build();
    for run in runs {
        h.runs.seed(run);
    }
    h
}

async fn failed_selection(agents: usize) -> Harness {
    let once = Arc::new(AtomicUsize::new(0));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents((0..agents).map(|i| orch_domain::Agent {
            id: if i == 0 {
                "agt_test1".into()
            } else {
                format!("agt_extra{i}")
            },
            ..make_agent()
        }))
        .wrap_state_store({
            let once = once.clone();
            move |inner| {
                Arc::new(WriteFails {
                    inner,
                    fail: Arc::new(AtomicBool::new(false)),
                    once,
                }) as Arc<dyn StateStore>
            }
        })
        .build();
    h.orchestrator.enable_daemon_mode();
    once.store(1, Ordering::SeqCst);
    let error = h
        .orchestrator
        .daemon_launch_task("tsk_test1")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("transient state write failure"));
    assert!(h.runs.snapshot().is_empty(), "no launch began");
    assert!(
        h.orchestrator
            .inner
            .pending_preparation_cleanup
            .lock()
            .await
            .is_empty(),
        "a launch that never started owns no cleanup obligation"
    );
    h
}

async fn selection_recovers(agents: usize) {
    let h = failed_selection(agents).await;
    h.orchestrator
        .daemon_reconcile_dead_local_runs()
        .await
        .unwrap();
    assert!(
        h.persisted_state().claimed.is_empty(),
        "failed selection became a durable claim"
    );
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty(),
        "agent still reserved"
    );
    assert!(
        h.orchestrator.inner.cancels.lock().await.is_empty(),
        "selection token retained"
    );
    h.orchestrator
        .daemon_launch_task("tsk_test1")
        .await
        .unwrap();
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(h.runs.snapshot().len(), 1, "task must start exactly once");
}

#[tokio::test]
async fn one_agent_can_launch_after_a_failed_selection_save() {
    selection_recovers(1).await;
}

#[tokio::test]
async fn two_agents_can_launch_after_a_failed_selection_save() {
    selection_recovers(2).await;
}

#[tokio::test]
async fn cancel_after_a_failed_selection_save_leaves_no_reservation() {
    let h = failed_selection(1).await;
    h.orchestrator
        .daemon_cancel_task("tsk_test1", false)
        .await
        .unwrap();
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Cancelled);
    assert!(h.persisted_state().claimed.is_empty());
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty()
    );
    assert!(h.orchestrator.inner.cancels.lock().await.is_empty());
    assert!(h.runs.snapshot().is_empty());
}

#[tokio::test]
async fn stop_after_a_failed_debounced_save_still_signals_live_children() {
    let processes = Arc::new(Killable::default());
    let fail = Arc::new(AtomicBool::new(false));
    let h = adopted(
        1,
        processes.clone(),
        Some((fail.clone(), Arc::new(AtomicUsize::new(0)))),
    );
    h.orchestrator.daemon_initialize().await.unwrap();
    fail.store(true, Ordering::SeqCst);
    h.orchestrator.save_state_lazy().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if h.events_of(orch_domain::OrchestratorEventType::OrchestratorError).iter().any(|event|
                matches!(event, orch_domain::OrchestratorEvent::OrchestratorError {context, error, ..}
                    if context == "debounced state save" && error.contains("disk full"))) {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("debounced save must fire and report its failure");
    let stop = h.orchestrator.daemon_stop().await.unwrap_err();
    assert!(
        stop.to_string().contains("disk full"),
        "stop must report the storage failure: {stop}"
    );
    assert_eq!(
        *processes.killed.lock().unwrap(),
        [40_000],
        "stop did not signal the live child"
    );
}

/// A collector update must not make a failed selection durable, whether the
/// next owner operation flushes it or the debounce timer fires on its own.
async fn debounced_failed_selection_recovers(agents: usize, via_timer: bool) {
    let h = failed_selection(agents).await;
    // collect_events arms this on another live run's event, without reloading.
    h.orchestrator.save_state_lazy().await;
    if via_timer {
        let writes_before = h.state.written().len();
        // The paused clock auto-advances past the debounce. Its slot clears
        // before the write, so also wait for the fake store to record it.
        tokio::time::sleep(super::super::DEBOUNCE + Duration::from_millis(1)).await;
        let mut completed = false;
        for _ in 0..1000 {
            let lazy = h.orchestrator.inner.lazy_save.lock().await;
            if !lazy.dirty && lazy.timer.is_none() && h.state.written().len() > writes_before {
                completed = true;
                break;
            }
            drop(lazy);
            tokio::task::yield_now().await;
        }
        assert!(completed, "debounce did not finish its write");
    } else {
        {
            let lazy = h.orchestrator.inner.lazy_save.lock().await;
            assert!(lazy.dirty && lazy.timer.is_some(), "debounce must be armed");
        }
        h.orchestrator
            .daemon_reconcile_dead_local_runs()
            .await
            .unwrap();
    }
    assert!(
        h.persisted_state().claimed.is_empty(),
        "debounced save persisted the failed selection's claim"
    );
    assert!(
        h.in_memory_state().await.claimed.is_empty(),
        "failed selection retained an in-memory claim"
    );
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty(),
        "failed selection retained a preparing entry"
    );
    assert!(
        h.orchestrator.inner.cancels.lock().await.is_empty(),
        "failed selection retained a cancel token"
    );
    assert!(
        h.orchestrator
            .inner
            .pending_preparation_cleanup
            .lock()
            .await
            .is_empty(),
        "failed selection retained a cleanup obligation"
    );
    h.orchestrator
        .daemon_launch_task("tsk_test1")
        .await
        .unwrap();
    h.orchestrator.daemon_tick(true).await.unwrap();
    assert_eq!(h.runs.snapshot().len(), 1, "task must start exactly once");
}

#[tokio::test(start_paused = true)]
async fn debounced_failed_selection_flush_with_one_agent() {
    debounced_failed_selection_recovers(1, false).await;
}

#[tokio::test(start_paused = true)]
async fn debounced_failed_selection_flush_with_two_agents() {
    debounced_failed_selection_recovers(2, false).await;
}

#[tokio::test(start_paused = true)]
async fn debounced_failed_selection_timer_without_another_operation() {
    debounced_failed_selection_recovers(1, true).await;
}
