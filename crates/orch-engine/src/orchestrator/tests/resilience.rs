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
    OrchestratorConfig, OrchestratorEvent, OrchestratorEventType, Run, Task, TaskStatus, TokenUsage,
};

use super::harness::{Harness, NOW_ISO, state_running, test_config};

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
