//! The retry delay is honoured: a task waiting in the retry queue is started
//! only once its entry comes due, by whichever dispatch path gets there.

use std::sync::Arc;

use orch_core::adapter::AdapterError;
use orch_core::test_support::{
    FIXED_NOW_MS, FakeAdapter, done_event, make_agent, make_run, make_task,
};
use orch_domain::{AdapterErrorKind, OrchestratorState, RetryEntry, Run, Task, TaskStatus};

use super::harness::{Harness, NOW_ISO, iso_before, state_running};

fn done_adapter() -> Arc<FakeAdapter> {
    Arc::new(FakeAdapter::with_events([done_event(NOW_ISO)]))
}

fn retrying_task() -> Task {
    Task {
        id: "tsk_retry".into(),
        status: TaskStatus::Retrying,
        attempts: 1,
        ..make_task()
    }
}

fn queued_retry(due_at: String) -> OrchestratorState {
    OrchestratorState {
        retry_queue: vec![RetryEntry {
            task_id: "tsk_retry".into(),
            attempt: 2,
            due_at,
            error: "previous error".into(),
        }],
        ..OrchestratorState::default()
    }
}

fn due_in(delta_ms: i64) -> String {
    orch_core::clock::iso_from_epoch_ms(FIXED_NOW_MS + delta_ms)
}

#[tokio::test]
async fn a_tick_does_not_start_a_retry_before_it_is_due() {
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry(due_in(60_000)))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_retry").status, TaskStatus::Retrying);
    assert_eq!(h.persisted_state().retry_queue.len(), 1);
}

#[tokio::test]
async fn the_reactive_dispatch_does_not_start_a_retry_before_it_is_due() {
    // `run_all` is the reactive dispatch's body: no reconcile, so no
    // retry-queue processing, only `dispatch_all`.
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry(due_in(60_000)))
        .build();

    h.orchestrator.run_all().await.unwrap();

    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_retry").status, TaskStatus::Retrying);
}

#[tokio::test]
async fn the_reactive_dispatch_starts_a_retry_that_has_come_due_and_drops_its_entry() {
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry(iso_before(1_000)))
        .build();

    h.orchestrator.run_all().await.unwrap();

    assert_eq!(h.adapter.execute_calls.len(), 1);
    // A stale entry would swallow the next failure's retry (enqueue_retry
    // deduplicates by task id) and start it with no delay when it came due.
    assert!(h.persisted_state().retry_queue.is_empty());
}

#[tokio::test]
async fn a_retrying_task_with_no_queue_entry_is_still_dispatched() {
    // An entry can be lost (queue overflow, a reset state file). The task
    // must not be stranded in `retrying`.
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.adapter.execute_calls.len(), 1);
}

#[tokio::test]
async fn a_retry_entry_with_an_unparseable_due_at_does_not_hold_the_task_back() {
    // `process_retry_queue` never drains such an entry, so treating it as
    // pending would strand the task for good.
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry("not a timestamp".into()))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.adapter.execute_calls.len(), 1);
    assert!(h.persisted_state().retry_queue.is_empty());
}

#[tokio::test]
async fn run_task_starts_a_waiting_retry_at_once_and_consumes_its_entry() {
    // `orch run <id>` is an explicit operator request: it overrides the wait.
    let h = Harness::builder()
        .tasks([retrying_task()])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry(due_in(60_000)))
        .build();

    h.orchestrator.run_task("tsk_retry").await.unwrap();

    assert_eq!(h.adapter.execute_calls.len(), 1);
    assert!(h.persisted_state().retry_queue.is_empty());
}

#[tokio::test]
async fn a_failed_run_is_retried_only_after_its_backoff_has_elapsed() {
    // The stream throws on every attempt, so every run fails.
    let adapter = Arc::new(FakeAdapter::failing_after(
        [],
        AdapterError::with_kind("boom", AdapterErrorKind::ProcessCrash, Some(3)),
    ));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter(Arc::clone(&adapter))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;
    assert_eq!(adapter.execute_calls.len(), 1);
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);

    // Both dispatch paths, inside the 10 s first-retry delay.
    h.tick().await.unwrap();
    h.orchestrator.run_all().await.unwrap();
    h.clock.advance_ms(9_999);
    h.tick().await.unwrap();
    assert_eq!(
        adapter.execute_calls.len(),
        1,
        "the retry started before retry_base_delay_ms elapsed"
    );

    h.clock.advance_ms(1);
    h.tick().await.unwrap();
    h.settle().await;
    assert_eq!(adapter.execute_calls.len(), 2);
}

#[tokio::test]
async fn a_task_moved_back_to_todo_is_not_held_by_its_old_retry_entry() {
    // The wait belongs to the `retrying` state. Once the task is fresh work
    // again, a leftover entry must not delay it; starting it drops the entry.
    let h = Harness::builder()
        .tasks([Task {
            status: TaskStatus::Todo,
            ..retrying_task()
        }])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(queued_retry(due_in(60_000)))
        .build();

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert_eq!(h.adapter.execute_calls.len(), 1);
    assert!(h.persisted_state().retry_queue.is_empty());
}

#[tokio::test]
async fn a_run_that_died_is_not_restarted_by_the_same_tick() {
    // Reconcile fails the dead run and queues the retry; the dispatch pass
    // that follows it in the same tick must leave the task waiting.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_deadpid".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .adapter(done_adapter())
        .state(state_running(
            "tsk_deadpid",
            "agt_test1",
            "run_deadpid",
            99999,
            NOW_ISO,
        ))
        .build();
    h.runs.seed(Run {
        id: "run_deadpid".into(),
        task_id: "tsk_deadpid".into(),
        ..make_run()
    });
    h.processes.set_dead(99999);

    h.load_state().await.unwrap();
    h.tick().await.unwrap();

    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_deadpid").status, TaskStatus::Retrying);
    assert_eq!(h.persisted_state().retry_queue.len(), 1);
}
