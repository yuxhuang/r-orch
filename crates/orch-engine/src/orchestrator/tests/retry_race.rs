//! Three guards against dispatching work that has already finished: the
//! dispatchable check in `dispatch_task`, the re-read when a retry comes due,
//! and the running-entry check at the top of the failure handler.

use std::sync::Arc;

use orch_core::test_support::{FakeAdapter, done_event, make_agent, make_task};
use orch_domain::{
    Agent, AgentStatus, OrchestratorState, RetryEntry, RunningEntry, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, iso_before};

fn done_adapter() -> Arc<FakeAdapter> {
    let adapter = FakeAdapter::new();
    adapter.set_events([done_event(NOW_ISO)]);
    Arc::new(adapter)
}

/// `run <id>` against a task in a terminal state.
async fn run_terminal_task(status: TaskStatus, attempts: u32) -> Harness {
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_1".into(),
            status,
            attempts,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_1".into(),
            status: AgentStatus::Idle,
            ..make_agent()
        }])
        .adapter(done_adapter())
        .build();
    h.orchestrator.run_task("tsk_1").await.unwrap();
    h
}

#[tokio::test]
async fn dispatch_silently_skips_a_task_that_is_already_done() {
    let h = run_terminal_task(TaskStatus::Done, 1).await;
    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_1").status, TaskStatus::Done);
}

#[tokio::test]
async fn dispatch_silently_skips_a_cancelled_task() {
    let h = run_terminal_task(TaskStatus::Cancelled, 0).await;
    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_1").status, TaskStatus::Cancelled);
}

#[tokio::test]
async fn dispatch_silently_skips_a_failed_task() {
    let h = run_terminal_task(TaskStatus::Failed, 3).await;
    assert!(h.adapter.execute_calls.is_empty());
    assert_eq!(h.task("tsk_1").status, TaskStatus::Failed);
}

#[tokio::test]
async fn the_retry_queue_skips_a_task_that_succeeded_while_it_waited() {
    // Queued as `retrying`, but it reached `done` through another path.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_r1".into(),
            status: TaskStatus::Done,
            attempts: 2,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_1".into(),
            ..make_agent()
        }])
        .adapter(done_adapter())
        .state(OrchestratorState {
            retry_queue: vec![RetryEntry {
                task_id: "tsk_r1".into(),
                attempt: 2,
                due_at: iso_before(1_000),
                error: "temp fail".into(),
            }],
            ..OrchestratorState::default()
        })
        .build();

    h.start_watch().await.unwrap();

    assert!(h.adapter.execute_calls.is_empty());
    // The entry is still dropped from the queue — it came due, it was simply
    // not dispatchable.
    assert!(h.persisted_state().retry_queue.is_empty());
    h.orchestrator.stop().await.unwrap();
}

#[tokio::test]
async fn the_failure_handler_returns_early_when_the_entry_is_already_gone() {
    // The success handler got there first and dropped the running entry; the
    // failure handler must not then charge the agent with a failure.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_race".into(),
            status: TaskStatus::Done,
            attempts: 1,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_1".into(),
            ..make_agent()
        }])
        .build();

    h.own_lock();
    h.set_state(OrchestratorState::default()).await;

    let entry = RunningEntry {
        run_id: "run_test".into(),
        agent_id: "agt_1".into(),
        task_id: "tsk_race".into(),
        pid: 99999,
        started_at: NOW_ISO.into(),
        last_event_at: NOW_ISO.into(),
        remote: None,
    };
    h.orchestrator
        .handle_run_failure("tsk_race", &entry, "stall", None)
        .await
        .unwrap();

    assert_eq!(h.agent("agt_1").stats.tasks_failed, 0);
    assert_eq!(h.agent("agt_1").stats.total_runs, 0);
    assert_eq!(h.task("tsk_race").status, TaskStatus::Done);
}

#[tokio::test]
async fn the_success_handler_returns_early_when_the_entry_is_already_gone() {
    // The symmetric guard: a late success must not double-count.
    let h = Harness::builder()
        .tasks([Task {
            id: "tsk_race2".into(),
            status: TaskStatus::InProgress,
            attempts: 1,
            ..make_task()
        }])
        .agents([Agent {
            id: "agt_1".into(),
            ..make_agent()
        }])
        .build();

    h.own_lock();
    h.set_state(OrchestratorState::default()).await;

    h.orchestrator
        .handle_run_success("tsk_race2", "run_test", "agt_1", None, None, vec![])
        .await
        .unwrap();

    assert_eq!(h.agent("agt_1").stats.tasks_completed, 0);
    assert_eq!(h.task("tsk_race2").status, TaskStatus::InProgress);
}
