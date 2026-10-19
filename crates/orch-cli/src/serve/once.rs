//! `serve --once`.
//!
//! Three rules decide when it stops and with what code:
//!
//! - the poll interval is a **hard-coded 2000 ms** and is *not* affected by
//!   `--tick-interval`, which only changes the orchestrator's own tick;
//! - an **empty project exits immediately**, before any waiting;
//! - any `failed` task makes the result `has_failed` and the exit code 1 —
//!   `cancelled` does not;
//! - and a signal arriving while work is **still outstanding** makes the
//!   result `interrupted` with exit code 1 (reporting `all_done` and exiting 0
//!   here would claim a success it did not achieve).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::{TaskFilter, TaskStore};
use orch_domain::{OrchestratorEvent, Task, is_terminal};
use orch_engine::EventBus;

/// The `--once` poll interval. Deliberately a constant: `--tick-interval`
/// changes `config.scheduling.poll_interval_ms`, not this.
pub const ONCE_POLL_INTERVAL: Duration = Duration::from_millis(2_000);

/// What `serve` needs from the orchestrator.
///
/// A narrow trait rather than a dependency on WP-4's concrete type: it keeps
/// the terminal rule and its exit code testable now, and WP-4's orchestrator
/// satisfies it with the three methods it already has.
#[async_trait]
pub trait ServeOrchestrator: Send + Sync {
    /// `--once` passes `skip_autonomous_seeding: true`.
    async fn start_watch(&self, skip_autonomous_seeding: bool) -> Result<()>;
    async fn stop(&self) -> Result<()>;
    /// Watch mode blocks here until the orchestrator shuts down.
    async fn wait_for_stop(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnceResult {
    AllDone,
    HasFailed,
    /// Stopped by a signal with tasks still unfinished.
    ///
    /// Computing the verdict from the task list without asking why the wait
    /// stopped would report `all_done` and exit 0 here. A run that was killed
    /// mid-flight reporting success is the kind of wrong answer that devalues
    /// every true one.
    Interrupted,
}

impl OnceResult {
    pub fn as_str(self) -> &'static str {
        match self {
            OnceResult::AllDone => "all_done",
            OnceResult::HasFailed => "has_failed",
            OnceResult::Interrupted => "interrupted",
        }
    }

    pub fn exit_code(self) -> u8 {
        match self {
            OnceResult::AllDone => 0,
            // Every non-success `--once` outcome exits 1.
            OnceResult::HasFailed | OnceResult::Interrupted => 1,
        }
    }
}

/// Start the watch, wait for every task to reach a terminal status, stop.
pub async fn run_once(
    orchestrator: &dyn ServeOrchestrator,
    tasks: &dyn TaskStore,
    events: &EventBus,
    poll_interval: Duration,
) -> Result<OnceResult> {
    // Subscribed before `start_watch`, so a shutdown during startup is not
    // missed.
    let stopped = Arc::new(AtomicBool::new(false));
    let mut subscription = events.subscribe();
    let watcher = tokio::spawn({
        let stopped = Arc::clone(&stopped);
        async move {
            while let Some(event) = subscription.recv().await {
                if matches!(*event, OrchestratorEvent::OrchestratorShutdown { .. }) {
                    stopped.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    });

    orchestrator.start_watch(true).await?;
    let (final_tasks, stop_reason) = wait_for_all_terminal(tasks, poll_interval, &stopped).await?;
    // Unconditional, and before the verdict: the wind-down releases the lock,
    // so an interrupted run must not leave the project wedged.
    orchestrator.stop().await?;
    watcher.abort();

    Ok(match stop_reason {
        StopReason::Interrupted => OnceResult::Interrupted,
        StopReason::WorkFinished => {
            if final_tasks
                .iter()
                .any(|task| task.status == orch_domain::TaskStatus::Failed)
            {
                OnceResult::HasFailed
            } else {
                OnceResult::AllDone
            }
        }
    })
}

/// Why `wait_for_all_terminal` stopped waiting.
///
/// The verdict cannot come from the task list alone, or a signal mid-run and a
/// genuine completion would be indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    /// Nothing left to do: every task terminal, or no tasks at all.
    WorkFinished,
    /// A shutdown arrived with work still outstanding.
    Interrupted,
}

/// The three exit conditions, checked in this order.
async fn wait_for_all_terminal(
    tasks: &dyn TaskStore,
    poll_interval: Duration,
    stopped: &AtomicBool,
) -> Result<(Vec<Task>, StopReason)> {
    loop {
        let current = tasks.list(&TaskFilter::default()).await?;
        // An empty project exits immediately.
        if current.is_empty() {
            return Ok((current, StopReason::WorkFinished));
        }
        if current.iter().all(|task| is_terminal(task.status)) {
            return Ok((current, StopReason::WorkFinished));
        }
        // The orchestrator gave up: return what we have rather than spin.
        //
        // Checked *after* the terminal test on purpose, and the order is the
        // contract: a signal that arrives once everything is already terminal
        // is a normal completion and still reports `all_done`. Only a signal
        // with work outstanding is an interruption.
        if stopped.load(Ordering::SeqCst) {
            return Ok((current, StopReason::Interrupted));
        }
        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{FakeTaskStore, make_task};
    use orch_domain::TaskStatus;
    use std::sync::Mutex;

    #[derive(Debug, Default)]
    struct FakeOrchestrator {
        started: Mutex<Vec<bool>>,
        stops: Mutex<usize>,
        /// Emitted on `start_watch`, to drive the "gave up" path.
        shutdown_on_start: Option<Arc<EventBus>>,
    }

    #[async_trait]
    impl ServeOrchestrator for FakeOrchestrator {
        async fn start_watch(&self, skip_autonomous_seeding: bool) -> Result<()> {
            self.started.lock().unwrap().push(skip_autonomous_seeding);
            if let Some(events) = &self.shutdown_on_start {
                events.emit(OrchestratorEvent::OrchestratorShutdown {
                    reason: "no agents".to_owned(),
                });
            }
            Ok(())
        }
        async fn stop(&self) -> Result<()> {
            *self.stops.lock().unwrap() += 1;
            Ok(())
        }
        async fn wait_for_stop(&self) {}
    }

    fn task(status: TaskStatus) -> Task {
        Task {
            status,
            ..make_task()
        }
    }

    const FAST: Duration = Duration::from_millis(5);

    #[tokio::test]
    async fn all_terminal_tasks_are_all_done_and_exit_zero() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Done));
        let orchestrator = FakeOrchestrator::default();
        let events = EventBus::new();

        let result = run_once(&orchestrator, &store, &events, FAST)
            .await
            .expect("runs");
        assert_eq!(result, OnceResult::AllDone);
        assert_eq!(result.exit_code(), 0);
        assert_eq!(result.as_str(), "all_done");

        // `startWatch({ skipAutonomousSeeding: true })`, then `stop()`.
        assert_eq!(*orchestrator.started.lock().unwrap(), vec![true]);
        assert_eq!(*orchestrator.stops.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn one_failed_task_is_has_failed_and_exit_one() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Done));
        store.seed(Task {
            id: "tsk_2".to_owned(),
            status: TaskStatus::Failed,
            ..make_task()
        });

        let result = run_once(&FakeOrchestrator::default(), &store, &EventBus::new(), FAST)
            .await
            .expect("runs");
        assert_eq!(result, OnceResult::HasFailed);
        assert_eq!(result.exit_code(), 1);
        assert_eq!(result.as_str(), "has_failed");
    }

    #[tokio::test]
    async fn a_cancelled_task_is_still_all_done() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Done));
        store.seed(Task {
            id: "tsk_2".to_owned(),
            status: TaskStatus::Cancelled,
            ..make_task()
        });

        assert_eq!(
            run_once(&FakeOrchestrator::default(), &store, &EventBus::new(), FAST)
                .await
                .expect("runs"),
            OnceResult::AllDone
        );
    }

    #[tokio::test]
    async fn an_empty_project_exits_immediately() {
        let started = std::time::Instant::now();
        let result = run_once(
            &FakeOrchestrator::default(),
            &FakeTaskStore::new(),
            &EventBus::new(),
            Duration::from_secs(30),
        )
        .await
        .expect("runs");
        assert_eq!(result, OnceResult::AllDone);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it waited {:?} for an empty project",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_shutdown_stops_the_wait_even_with_a_pending_task() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Todo));

        let events = Arc::new(EventBus::new());
        let orchestrator = FakeOrchestrator {
            shutdown_on_start: Some(Arc::clone(&events)),
            ..Default::default()
        };

        // Without the shutdown this would poll forever: the task never
        // reaches a terminal status.
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_once(&orchestrator, &store, &events, FAST),
        )
        .await
        .expect("the shutdown ends the wait")
        .expect("runs");

        // A planted unreadable task file makes `serve --once` tick forever.
        // The task here never left `Todo`, so claiming success on SIGTERM is a
        // wrong answer, not a cosmetic one.
        assert_eq!(result, OnceResult::Interrupted);
        assert_eq!(result.exit_code(), 1);
        assert_eq!(result.as_str(), "interrupted");
    }

    /// The other half of the rule, and the reason the `stopped` check sits
    /// *after* the terminal check: a signal that arrives once the work is
    /// already done is an ordinary completion.
    #[tokio::test]
    async fn a_shutdown_after_everything_is_terminal_is_still_all_done() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Done));

        let events = Arc::new(EventBus::new());
        let orchestrator = FakeOrchestrator {
            shutdown_on_start: Some(Arc::clone(&events)),
            ..Default::default()
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_once(&orchestrator, &store, &events, FAST),
        )
        .await
        .expect("returns promptly")
        .expect("runs");

        assert_eq!(result, OnceResult::AllDone, "byte-identical to today");
        assert_eq!(result.exit_code(), 0);
    }

    /// A failed task still wins over an interruption when the work *is*
    /// finished, so the existing `has_failed` verdict is untouched.
    #[tokio::test]
    async fn a_failure_with_a_late_shutdown_is_still_has_failed() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Failed));

        let events = Arc::new(EventBus::new());
        let orchestrator = FakeOrchestrator {
            shutdown_on_start: Some(Arc::clone(&events)),
            ..Default::default()
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_once(&orchestrator, &store, &events, FAST),
        )
        .await
        .expect("returns promptly")
        .expect("runs");

        assert_eq!(result, OnceResult::HasFailed);
        assert_eq!(result.exit_code(), 1);
    }

    /// The wind-down runs on the interrupted path too, which is what releases
    /// the lock. Asserted on the fake rather than through a real project here;
    /// `tests/serve_interrupted.rs` covers the lock file end to end.
    #[tokio::test]
    async fn an_interrupted_run_still_stops_the_orchestrator() {
        let store = FakeTaskStore::new();
        store.seed(task(TaskStatus::Todo));

        let events = Arc::new(EventBus::new());
        let orchestrator = FakeOrchestrator {
            shutdown_on_start: Some(Arc::clone(&events)),
            ..Default::default()
        };

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_once(&orchestrator, &store, &events, FAST),
        )
        .await
        .expect("the shutdown ends the wait")
        .expect("runs");

        assert_eq!(result, OnceResult::Interrupted);
        assert_eq!(
            *orchestrator.stops.lock().unwrap(),
            1,
            "stop() must run on the interrupted path, or the lock is stranded"
        );
    }

    #[tokio::test]
    async fn the_wait_ends_as_soon_as_the_last_task_becomes_terminal() {
        let store = Arc::new(FakeTaskStore::new());
        store.seed(task(TaskStatus::InProgress));

        let flip = tokio::spawn({
            let store = Arc::clone(&store);
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                let mut task = store.snapshot().into_iter().next().expect("one task");
                task.status = TaskStatus::Done;
                store.seed(task);
            }
        });

        let result = run_once(
            &FakeOrchestrator::default(),
            store.as_ref(),
            &EventBus::new(),
            FAST,
        )
        .await
        .expect("runs");
        flip.await.expect("join");
        assert_eq!(result, OnceResult::AllDone);
    }

    #[test]
    fn the_poll_interval_is_two_seconds_and_independent_of_the_tick() {
        assert_eq!(ONCE_POLL_INTERVAL, Duration::from_millis(2_000));
    }
}
