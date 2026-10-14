//! Ported from `test/unit/application/orchestrator-resilience.test.ts`.
//!
//! The four resilience fixes: the tick circuit breaker, agent-stats failures
//! being reported rather than swallowed, signal-driven shutdown, and every
//! former `console.error` becoming an `orchestrator:error` event.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use orch_core::adapter::{Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams};
use orch_core::test_support::{make_agent, make_run, make_task};
use orch_domain::{
    Agent, AgentStatus, OrchestratorConfig, OrchestratorEvent, OrchestratorEventType,
    OrchestratorState, Run, RunStatus, Task, TaskStatus, TokenUsage,
};

use super::harness::{Harness, NOW_ISO, state_running, test_config};
use crate::orchestrator::WatchOptions;

fn fast_poll(poll_interval_ms: u64) -> OrchestratorConfig {
    let mut config = test_config();
    config.scheduling.poll_interval_ms = poll_interval_ms;
    config
}

/// Events a wildcard subscriber saw with the given `orchestrator:error`
/// context substring.
fn errors_matching(h: &Harness, needle: &str) -> Vec<(String, String, bool)> {
    h.events_of(OrchestratorEventType::OrchestratorError)
        .into_iter()
        .filter_map(|event| match event {
            OrchestratorEvent::OrchestratorError {
                error,
                context,
                fatal,
            } if context.contains(needle) => Some((error, context, fatal)),
            _ => None,
        })
        .collect()
}

// ── Fix 1: the tick circuit breaker ──────────────────────────────────────

#[tokio::test]
async fn a_successful_tick_leaves_the_failure_counter_at_zero() {
    let h = Harness::builder().config(fast_poll(20)).build();
    h.start_watch().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    assert_eq!(
        h.orchestrator
            .inner
            .consecutive_tick_failures
            .load(Ordering::SeqCst),
        0
    );
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn a_failing_tick_increments_the_failure_counter() {
    let h = Harness::builder().config(fast_poll(20)).build();
    h.start_watch().await.unwrap();
    h.state.faults.fail_always("tick-fail");
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    assert!(
        h.orchestrator
            .inner
            .consecutive_tick_failures
            .load(Ordering::SeqCst)
            > 0
    );
    h.state.faults.clear();
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn five_consecutive_failures_are_fatal_and_shut_the_orchestrator_down() {
    let h = Harness::builder().config(fast_poll(15)).build();
    h.start_watch().await.unwrap();
    h.state.faults.fail_always("persistent-error");

    h.wait_for(|h| h.orchestrator.is_shutting_down()).await;

    let fatal: Vec<_> = errors_matching(&h, "tick")
        .into_iter()
        .filter(|(_, _, fatal)| *fatal)
        .collect();
    assert_eq!(fatal.len(), 1, "exactly one fatal tick error");
    assert!(fatal[0].0.contains("persistent-error"));

    let shutdowns = h.events_of(OrchestratorEventType::OrchestratorShutdown);
    assert_eq!(shutdowns.len(), 1);
    match &shutdowns[0] {
        OrchestratorEvent::OrchestratorShutdown { reason } => {
            assert_eq!(reason, "5 consecutive tick failures");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    // `stop` ran and released the lock **even though the state store is the
    // thing that is broken** — see the note on `Orchestrator::stop`.
    assert!(!h.orchestrator.is_owner());
    assert_eq!(h.lock.release_count(), 1);
}

#[tokio::test]
async fn a_successful_tick_resets_the_counter_before_the_breaker_trips() {
    let h = Harness::builder().config(fast_poll(15)).build();
    h.start_watch().await.unwrap();

    // Four failures, then recovery: the breaker must not trip.
    h.state.faults.fail_next(4, "transient");
    h.wait_for(|h| !h.state.faults.is_armed()).await;
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;

    assert_eq!(
        h.orchestrator
            .inner
            .consecutive_tick_failures
            .load(Ordering::SeqCst),
        0
    );
    assert!(
        h.events_of(OrchestratorEventType::OrchestratorShutdown)
            .is_empty()
    );
    assert!(!h.orchestrator.is_shutting_down());
    h.orchestrator.stop().await.unwrap();
}

/// Fails the `n`-th `save`, passing everything else through.
struct FailingSaveAgentStore {
    inner: Arc<orch_core::test_support::FakeAgentStore>,
    saves: std::sync::atomic::AtomicUsize,
    fail_save: usize,
    message: &'static str,
}

#[async_trait::async_trait]
impl orch_core::storage::AgentStore for FailingSaveAgentStore {
    async fn list(&self) -> orch_core::error::Result<Vec<orch_domain::Agent>> {
        self.inner.list().await
    }

    async fn get(&self, id: &str) -> orch_core::error::Result<Option<orch_domain::Agent>> {
        self.inner.get(id).await
    }

    async fn get_by_name(
        &self,
        name: &str,
    ) -> orch_core::error::Result<Option<orch_domain::Agent>> {
        self.inner.get_by_name(name).await
    }

    async fn save(&self, agent: &orch_domain::Agent) -> orch_core::error::Result<()> {
        let count = self.saves.fetch_add(1, Ordering::SeqCst) + 1;
        if count == self.fail_save {
            return Err(orch_core::error::Error::other(self.message));
        }
        self.inner.save(agent).await
    }

    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

// ── Fix 2: agent-stats failures are reported ─────────────────────────────

#[tokio::test]
async fn a_failing_agent_stats_update_is_reported_and_does_not_abort_completion() {
    // The first agent-store *save* in the success path is the stats update, so
    // failing it targets `update_stats` precisely — a shared fault flag would
    // hit the earlier `get` instead.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_stats".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .wrap_agent_store(|inner| {
            Arc::new(FailingSaveAgentStore {
                inner,
                saves: std::sync::atomic::AtomicUsize::new(0),
                fail_save: 1,
                message: "stats-update-boom",
            }) as Arc<dyn orch_core::storage::AgentStore>
        })
        .state(state_running(
            "tsk_stats",
            "agt_test1",
            "run_stats",
            77777,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_stats".into(),
        task_id: "tsk_stats".into(),
        ..make_run()
    });

    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success(
            "tsk_stats",
            "run_stats",
            "agt_test1",
            Some(TokenUsage {
                input: 10,
                output: 20,
                reasoning: 0,
                total: 30,
                cache_read: 0,
                cache_write: 0,
            }),
            Some("result text".into()),
            vec!["file.ts".into()],
        )
        .await
        .unwrap();

    let stats_errors = errors_matching(&h, "agent stats update");
    assert_eq!(stats_errors.len(), 1);
    assert!(stats_errors[0].0.contains("stats-update-boom"));
    assert!(!stats_errors[0].2, "a stats failure is never fatal");
    // Completion still finished: the task moved on.
    assert_ne!(h.task("tsk_stats").status, TaskStatus::InProgress);
}

// ── Fix 3: signal-driven shutdown ────────────────────────────────────────

#[tokio::test]
async fn the_watch_loop_installs_both_signal_arms() {
    // The TS equivalent asserts `signalHandlers.length === 2`; here the arms
    // are `select!` branches, so what is observable is that both streams were
    // created.
    let arms = super::super::SignalArms::install();
    assert!(arms.interrupt.is_some(), "no SIGINT arm");
    assert!(arms.terminate.is_some(), "no SIGTERM arm");
}

#[tokio::test]
async fn a_disabled_signal_arm_never_fires() {
    // What replaces `removeSignalHandlers`: with signals off the arm is
    // permanently pending, so the loop cannot be woken by one.
    let mut arms = super::super::SignalArms::disabled();
    let pending = tokio::time::timeout(std::time::Duration::from_millis(50), arms.next()).await;
    assert!(pending.is_err(), "a disabled arm resolved");
}

#[tokio::test]
async fn sigint_reports_the_reason_and_stops() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();

    h.orchestrator.handle_signal("SIGINT").await;

    let shutdowns = h.events_of(OrchestratorEventType::OrchestratorShutdown);
    assert_eq!(shutdowns.len(), 1);
    match &shutdowns[0] {
        OrchestratorEvent::OrchestratorShutdown { reason } => {
            assert_eq!(reason, "Received SIGINT")
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(h.orchestrator.is_shutting_down());
    assert!(!h.orchestrator.is_owner());
}

#[tokio::test]
async fn sigterm_reports_the_reason_and_stops() {
    let h = Harness::builder().build();
    h.start_watch().await.unwrap();

    h.orchestrator.handle_signal("SIGTERM").await;

    assert!(
        h.events_of(OrchestratorEventType::OrchestratorShutdown)
            .iter()
            .any(|event| matches!(
                event,
                OrchestratorEvent::OrchestratorShutdown { reason } if reason.contains("SIGTERM")
            ))
    );
    assert!(h.orchestrator.is_shutting_down());
    // And `wait_for_stop` is released.
    tokio::time::timeout(
        std::time::Duration::from_millis(200),
        h.orchestrator.wait_for_stop(),
    )
    .await
    .expect("wait_for_stop should resolve after a signal");
}

#[tokio::test]
async fn wait_for_stop_resolves_when_the_breaker_stops_us_from_inside_the_loop() {
    // The circuit breaker calls `stop()` *from the watch task*, and `stop()`
    // aborts whatever `JoinHandle` it finds — which is that very task. If it
    // aborts itself before signalling `stopped`, `wait_for_stop` never
    // resolves and `orch serve`/`orch run --watch` hang on Ctrl+C instead of
    // shutting down.
    let h = Harness::builder().config(fast_poll(15)).build();
    h.start_watch().await.unwrap();
    h.state.faults.fail_always("persistent-error");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        h.orchestrator.wait_for_stop(),
    )
    .await
    .expect("wait_for_stop must resolve after the breaker fires");
}

// ── Fix 4: structured error events ───────────────────────────────────────

#[tokio::test]
async fn a_dispatch_failure_is_reported_as_a_non_fatal_orchestrator_error() {
    // No adapter registered, so `require` fails inside dispatch.
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .without_adapter()
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    let dispatch_errors = errors_matching(&h, "dispatch task");
    assert_eq!(dispatch_errors.len(), 1);
    assert!(!dispatch_errors[0].2);
    assert!(
        dispatch_errors[0]
            .0
            .starts_with("Unknown adapter: \"shell\""),
        "error was {:?}",
        dispatch_errors[0].0
    );
    // The claim was rolled back, so the task is dispatchable again.
    assert!(h.persisted_state().claimed.is_empty());
}

/// An adapter whose stream throws immediately — the TS `failAdapter`.
struct ThrowingAdapter;

#[async_trait::async_trait]
impl Adapter for ThrowingAdapter {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> AdapterTestResult {
        AdapterTestResult::ok("1.0.0")
    }

    fn execute(&self, _params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError> {
        Ok(ExecuteHandle {
            pid: 99999,
            events: Box::pin(futures::stream::iter(vec![Err(AdapterError::new(
                "adapter-boom",
                None,
            ))])),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn an_adapter_whose_stream_throws_fails_the_run_rather_than_the_tick() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter_impl(Arc::new(ThrowingAdapter) as Arc<dyn Adapter>)
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;

    // The run was failed by the failure handler, not left running, and the
    // tick itself did not error.
    let run = h.runs.snapshot().into_iter().next().expect("a run");
    assert_eq!(run.status, orch_domain::RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some("adapter-boom"));
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);
}

#[tokio::test]
async fn a_failing_debounced_state_save_is_reported() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_lazy".into(),
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_lazy",
            "agt_test1",
            "run_lazy",
            44444,
            NOW_ISO,
        ))
        .build();

    h.load_state().await.unwrap();
    h.state.faults.fail_once("disk-full");

    h.orchestrator.save_state_lazy().await;
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    let save_errors = errors_matching(&h, "debounced state save");
    assert_eq!(save_errors.len(), 1);
    assert!(save_errors[0].0.contains("disk-full"));
    assert!(!save_errors[0].2);
}

#[tokio::test]
async fn the_debounce_coalesces_many_lazy_saves_into_one_write() {
    let h = Harness::builder().build();
    h.load_state().await.unwrap();
    let before = h.state.written().len();

    for _ in 0..100 {
        h.orchestrator.save_state_lazy().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    assert_eq!(h.state.written().len() - before, 1);
}

#[tokio::test]
async fn flushing_forces_the_pending_write_out_immediately() {
    let h = Harness::builder().build();
    h.load_state().await.unwrap();
    let before = h.state.written().len();

    h.orchestrator.save_state_lazy().await;
    let state = h.in_memory_state().await;
    h.orchestrator.flush_state_lazy(&state).await;

    assert_eq!(h.state.written().len() - before, 1);
    // And the timer was cancelled, so nothing writes a second time.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(h.state.written().len() - before, 1);
}

// ── the bus carries both control events ──────────────────────────────────

#[tokio::test]
async fn a_wildcard_subscriber_sees_orchestrator_error_and_shutdown() {
    let h = Harness::builder().build();
    h.bus.emit(OrchestratorEvent::OrchestratorError {
        error: "test".into(),
        context: "unit".into(),
        fatal: false,
    });
    h.bus.emit(OrchestratorEvent::OrchestratorShutdown {
        reason: "test shutdown".into(),
    });

    assert_eq!(
        h.events()
            .iter()
            .map(|event| event.event_type())
            .collect::<Vec<_>>(),
        vec![
            OrchestratorEventType::OrchestratorError,
            OrchestratorEventType::OrchestratorShutdown,
        ]
    );
}

/// Fails every `save`, passing reads through — so a sweep's *write* is what
/// breaks, not the `list` that finds its work.
struct FailingSaveTaskStore {
    inner: Arc<orch_core::test_support::FakeTaskStore>,
    message: &'static str,
}

#[async_trait::async_trait]
impl orch_core::storage::TaskStore for FailingSaveTaskStore {
    async fn list(
        &self,
        filter: &orch_core::storage::TaskFilter,
    ) -> orch_core::error::Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> orch_core::error::Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, _task: &Task) -> orch_core::error::Result<()> {
        Err(orch_core::error::Error::other(self.message))
    }

    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

// ── Gate C2: a crash between the spawn and the state write ───────────────

/// `dispatch_claimed` spawns the child, marks the run `running`, inserts the
/// running entry and *then* saves. A crash inside that window leaves a
/// `running` run record with no entry — and an agent child still working in a
/// workspace whose task will be failed by the orphaned-task sweep.
#[tokio::test]
async fn startup_cancels_a_running_run_with_no_entry_and_kills_its_child() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_orphan_run".into(),
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([make_agent()])
        // Empty: the crash is *why* there is no entry for this run.
        .state(OrchestratorState::default())
        .build();
    h.runs.seed(Run {
        id: "run_orphan".into(),
        task_id: "tsk_orphan_run".into(),
        agent_id: "agt_test1".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        ..make_run()
    });
    // The child outlived the orchestrator, which is the dangerous half.
    h.processes.set_alive(4321);

    h.orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        })
        .await
        .expect("start watch");

    let run = h.runs.peek("run_orphan").expect("the run record");
    assert_eq!(run.status, RunStatus::Cancelled, "the run stayed running");
    assert_eq!(run.error.as_deref(), Some("Orchestrator restarted"));
    assert!(
        h.processes
            .killed_with_grace()
            .iter()
            .any(|(pid, _)| *pid == 4321),
        "the orphaned child was left running: {:?}",
        h.processes.killed_with_grace()
    );

    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn startup_leaves_a_running_run_that_still_has_its_entry_alone() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_live".into(),
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(state_running(
            "tsk_live",
            "agt_test1",
            "run_live",
            4321,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_live".into(),
        task_id: "tsk_live".into(),
        agent_id: "agt_test1".into(),
        status: RunStatus::Running,
        pid: Some(4321),
        ..make_run()
    });
    // Alive and accounted for: this is a healthy hand-over, not an orphan.
    h.processes.set_alive(4321);

    h.orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        })
        .await
        .expect("start watch");

    assert_eq!(
        h.runs.peek("run_live").expect("the run record").status,
        RunStatus::Running,
        "a run with a live entry must not be swept"
    );

    h.orchestrator.stop().await.unwrap();
}

// ── Gate F2: the lock is not leaked when startup fails ───────────────────

#[tokio::test]
async fn a_failed_startup_releases_the_lock_it_acquired() {
    let h = Harness::builder().agents([make_agent()]).build();
    // The first read after the lock is taken is `load_state`.
    h.state.faults.fail_once("state-read-boom");

    let err = h
        .orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: false,
        })
        .await
        .expect_err("startup failed");
    assert!(err.to_string().contains("state-read-boom"));

    // `serve` and `run --watch` exit here, so their dead pid makes the file
    // stealable — but the TUI stays up in observer mode, and a lock stamped
    // with its own *live* pid is never reclaimed.
    assert!(
        !h.lock.is_held(),
        "startup failed holding the lock: {} acquire(s), {} release(s)",
        h.lock.acquire_count(),
        h.lock.release_count()
    );
    assert!(!h.orchestrator.is_owner());
}

// ── Gate Q1: reconcile's two sweeps fail differently, on purpose ─────────

/// The reference wraps the stale-agent writes in a bare `Promise.all`
/// (`orchestrator.ts:618-622`), so one rejection fails the tick, while the
/// orphaned-task map carries its own `try`/`catch` (`:630-637`). Both halves
/// are pinned here so the asymmetry is not tidied away into consistency.
#[tokio::test]
async fn a_stale_agent_write_failure_fails_the_tick_but_an_orphaned_task_one_does_not() {
    let stale_agent = Agent {
        id: "agt_stale".into(),
        status: AgentStatus::Running,
        ..make_agent()
    };
    let h = Harness::builder()
        .agents([stale_agent])
        // Running with no entry in state: exactly what reconcile sweeps.
        .state(OrchestratorState::default())
        // Only the *save* fails: a blanket fault would break the `list` that
        // finds the stale agent in the first place, and the test would pass
        // for the wrong reason.
        .wrap_agent_store(|inner| {
            Arc::new(FailingSaveAgentStore {
                inner,
                saves: std::sync::atomic::AtomicUsize::new(0),
                fail_save: 1,
                message: "stale-agent-boom",
            }) as Arc<dyn orch_core::storage::AgentStore>
        })
        .build();

    let err = h.orchestrator.tick_once().await.expect_err("tick failed");
    assert!(err.to_string().contains("stale-agent-boom"), "{err}");

    // The other sweep, with every task write failing: the tick still
    // succeeds, and the failure is reported as an event instead.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_orphan".into(),
            status: TaskStatus::InProgress,
            ..make_task()
        }])
        .agents([make_agent()])
        .state(OrchestratorState::default())
        .wrap_task_store(|inner| {
            Arc::new(FailingSaveTaskStore {
                inner,
                message: "orphan-task-boom",
            }) as Arc<dyn orch_core::storage::TaskStore>
        })
        .build();

    h.orchestrator
        .tick_once()
        .await
        .expect("an orphaned-task write failure must not fail the tick");
    assert!(
        !errors_matching(&h, "force-write orphaned task").is_empty(),
        "the failure was swallowed without a word"
    );
}

// ── Gate C1: signals are armed before the first tick ────────────────────

/// Makes the first tick slow, and announces when it has started.
struct SlowFirstListTaskStore {
    inner: Arc<orch_core::test_support::FakeTaskStore>,
    ticking: Arc<tokio::sync::Notify>,
    slowed: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl orch_core::storage::TaskStore for SlowFirstListTaskStore {
    async fn list(
        &self,
        filter: &orch_core::storage::TaskFilter,
    ) -> orch_core::error::Result<Vec<Task>> {
        if !self.slowed.swap(true, Ordering::SeqCst) {
            // The tick has begun, and will be here a while: exactly the
            // window in which a `systemctl restart` used to kill the process
            // outright.
            self.ticking.notify_one();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> orch_core::error::Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> orch_core::error::Result<()> {
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> orch_core::error::Result<()> {
        self.inner.delete(id).await
    }
}

/// A `SIGTERM` arriving during the first tick must shut the orchestrator down,
/// not kill the process.
///
/// Before the fix the arms were installed inside the spawned watch loop, i.e.
/// *after* the first tick, so for the whole of that tick — reconcile plus
/// every dispatch it triggers, seconds to minutes in production — both signals
/// kept their default disposition.
///
/// Its failure mode without the fix depends on what else has run in this
/// binary first, and is ugly either way: if no earlier test has created a
/// `SignalArms`, nothing has displaced the default disposition and **the test
/// runner is killed**; if one has, tokio's driver absorbs the signal but no
/// arm exists to receive it, so it is dropped and the `wait_for_stop` below
/// times out. A missing signal handler cannot announce itself more gently
/// than that, which is the point.
#[tokio::test(flavor = "multi_thread")]
async fn a_signal_during_the_first_tick_shuts_down_instead_of_killing_us() {
    let ticking = Arc::new(tokio::sync::Notify::new());
    let h = Harness::builder()
        .agents([make_agent()])
        .wrap_task_store({
            let ticking = Arc::clone(&ticking);
            move |inner| {
                Arc::new(SlowFirstListTaskStore {
                    inner,
                    ticking,
                    slowed: std::sync::atomic::AtomicBool::new(false),
                }) as Arc<dyn orch_core::storage::TaskStore>
            }
        })
        .build();

    // Sent only once the tick is demonstrably under way, so the signal cannot
    // land in the microseconds before `start_watch` arms it.
    let signaller = tokio::spawn(async move {
        ticking.notified().await;
        let pid = std::process::id().to_string();
        std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("send SIGTERM");
    });

    h.orchestrator
        .start_watch(WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: true,
        })
        .await
        .expect("start watch");
    signaller.await.expect("signaller");

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        h.orchestrator.wait_for_stop(),
    )
    .await
    .expect("the signal never reached the loop");

    assert!(
        h.events_of(OrchestratorEventType::OrchestratorShutdown)
            .iter()
            .any(|event| matches!(
                event,
                OrchestratorEvent::OrchestratorShutdown { reason } if reason.contains("SIGTERM")
            )),
        "no shutdown was reported for the signal"
    );
    // The wind-down ran: the lock is gone and the state was cleared.
    assert!(!h.lock.is_held(), "the lock survived the signal");
    assert_eq!(h.state.peek().pid, None, "the state was not cleared");
}

/// A task file that cannot be *read* is not a task that is gone.
///
/// Both recovery paths used to fold the two together with a `let Ok(Some(..))
/// else { return }`, which was tolerable while a read could only fail on a
/// missing or malformed file. Since WP-0's YAML hardening it can also fail on
/// an alias-expansion cap or a parse deadline — the file is intact and a retry
/// would work — so a silent return there hides a transient and leaves the task
/// non-terminal with nothing said.
///
/// Driven through `force_task_cancelled` directly: reaching it from
/// `start_watch` needs a store that fails one specific read and serves every
/// other, and a test tuned to a call count would pass for the wrong reason the
/// first time recovery reads one more task.
#[tokio::test]
async fn an_unreadable_task_is_reported_rather_than_treated_as_absent() {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_unreadable".into(),
            // `cancelled` is not a legal transition from `todo`, so the
            // service refuses and the force-write path reads the task back.
            status: TaskStatus::Todo,
            ..make_task()
        }])
        .agents([make_agent()])
        .build();
    // As a parse deadline would: the file is there, the read is not.
    h.tasks.faults.fail_always("yaml-deadline-boom");

    h.orchestrator.force_task_cancelled("tsk_unreadable").await;

    let reported = errors_matching(&h, "startup cleanup: read task");
    assert_eq!(
        reported.len(),
        1,
        "an unreadable task passed without a word: {:?}",
        h.events_of(OrchestratorEventType::OrchestratorError)
    );
    assert!(reported[0].0.contains("yaml-deadline-boom"), "{reported:?}");
}

#[tokio::test]
async fn a_task_that_is_simply_gone_is_cancelled_in_silence() {
    let h = Harness::builder().agents([make_agent()]).build();

    // Nothing seeded: an absent task is a legitimate no-op, and must not be
    // reported as a failure now that an unreadable one is.
    h.orchestrator.force_task_cancelled("tsk_missing").await;

    assert!(
        h.events_of(OrchestratorEventType::OrchestratorError)
            .is_empty(),
        "an absent task was reported as an error"
    );
}
