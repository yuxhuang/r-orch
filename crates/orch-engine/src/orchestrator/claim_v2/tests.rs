use super::*;
use orch_core::store::fake::FakeStorage;
use orch_core::store::retries::Retry;
use orch_domain::{AgentStatus, Task};
use orch_infra::storage::v2::FileStorage;

fn caller() -> Caller {
    Caller {
        principal: orch_core::store::Principal::Owner {
            identity: "test".into(),
        },
        project: "synthetic".into(),
    }
}

fn request() -> ClaimRequest {
    ClaimRequest {
        task_id: "task-selected".into(),
        task_revision: Revision::ONE,
        agent_id: "agent-selected".into(),
        agent_revision: Revision::ONE,
        run_id: "run-selected".into(),
        now_ms: 1234,
    }
}

fn task(id: &str) -> Task {
    serde_json::from_value(serde_json::json!({
        "id": id, "title": "Synthetic claim", "description": "", "status": "todo",
        "priority": 2, "labels": [], "depends_on": [], "created_at": "1970-01-01T00:00:00.000Z",
        "updated_at": "1970-01-01T00:00:00.000Z", "attempts": 0, "max_attempts": 3
    }))
    .expect("synthetic task")
}

async fn seed(storage: &dyn Storage) {
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            for id in ["task-selected", "task-other"] {
                unit.create(Entity::Task(Box::new(task(id))))?;
            }
            for id in ["agent-selected", "agent-other"] {
                let agent = serde_json::from_value(serde_json::json!({
                    "id": id, "name": id, "adapter": "shell", "status": "idle",
                    "config": {}, "stats": {"tasks_completed": 0, "tasks_failed": 0,
                        "total_runs": 0, "total_runtime_ms": 0}
                }))
                .expect("synthetic agent");
                unit.create(Entity::Agent(Box::new(agent)))?;
            }
            Ok(())
        })
        .await
        .expect("seed");
}

async fn state(storage: &dyn Storage) -> (Vec<Record>, Vec<Lease>) {
    let view = storage.read(caller()).await.expect("snapshot");
    let mut rows = Vec::new();
    for (kind, ids) in [
        (EntityKind::Task, vec!["task-selected", "task-other"]),
        (EntityKind::Agent, vec!["agent-selected", "agent-other"]),
        (EntityKind::Run, vec!["run-selected", "run-other"]),
    ] {
        for id in ids {
            if let Some(row) = view.get(kind, id).await.expect("row") {
                rows.push(row);
            }
        }
    }
    (rows, view.read_leases().await.expect("leases"))
}

async fn success(storage: &dyn Storage) {
    seed(storage).await;
    let before = state(storage).await;
    let ClaimResult::Reserved { task, run, lease } =
        claim(storage, caller(), request()).await.expect("claim")
    else {
        panic!("reservation refused");
    };
    assert_eq!(lease.task, "task-selected");
    assert_eq!(lease.agent, "agent-selected");
    assert_eq!(lease.run, "run-selected");
    assert_eq!(lease.epoch, 1);
    assert_eq!(lease.expires_at, 1234 + orch_core::store::leases::LEASE_MS);
    let Entity::Task(task_data) = &task.entity else {
        panic!("task")
    };
    assert_eq!(task_data.status, TaskStatus::InProgress);
    assert_eq!(task_data.attempts, 1);
    assert_eq!(task_data.updated_at, "1970-01-01T00:00:01.234Z");
    assert_eq!(task_data.created_at, "1970-01-01T00:00:00.000Z");
    assert_eq!(task_data.assignee.as_deref(), Some(lease.agent.as_str()));
    assert_eq!(task.revision.get(), 2);
    let Entity::Run(run_data) = &run.entity else {
        panic!("run")
    };
    assert_eq!(run_data.status, RunStatus::Preparing);
    assert_eq!(run_data.started_at, "1970-01-01T00:00:01.234Z");
    assert!(run_data.finished_at.is_none());
    assert_eq!(run_data.attempt, task_data.attempts);
    assert_eq!(run_data.task_id, lease.task);
    assert_eq!(run_data.agent_id, lease.agent);
    assert!(run_data.pid.is_none());
    assert!(run_data.prompt.is_empty());
    let after = state(storage).await;
    for row in before.0.iter().filter(|r| r.entity.id() != "task-selected") {
        assert!(after.0.contains(row), "unselected row changed");
    }
    assert!(after.0.contains(task.as_ref()));
    assert!(after.0.contains(run.as_ref()));
    assert_eq!(after.1, vec![lease]);
}

#[tokio::test]
async fn selected_task_agent_and_preparing_run_commit_together_in_memory() {
    success(&FakeStorage::default()).await;
}

#[tokio::test]
async fn selected_task_agent_and_preparing_run_commit_together_on_files() {
    let root = tempfile::tempdir().unwrap();
    let storage = FileStorage::new(root.path());
    success(&storage).await;
    let committed = state(&storage).await;
    drop(storage);
    assert_eq!(state(&FileStorage::new(root.path())).await, committed);
}

// Backend-neutral scenarios stay here so SQLite can register the same checks
// once its reservation prerequisites are accepted (#157).
async fn approval_refusal(storage: &dyn Storage) {
    seed(storage).await;
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut row.entity else {
                panic!("task")
            };
            task.labels.push("awaiting-approval".into());
            unit.update(row.entity, row.revision)?;
            Ok(())
        })
        .await
        .unwrap();
    let before = state(storage).await;
    let mut req = request();
    req.task_revision = Revision::ONE.next().unwrap();
    assert_eq!(
        claim(storage, caller(), req.clone()).await.unwrap(),
        ClaimResult::Refused(Refusal::TaskUnavailable)
    );
    assert_eq!(state(storage).await, before);

    // Removing the hold must leave the first epoch available: refusal cannot
    // silently consume an epoch even when the visible rows stay unchanged.
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut row.entity else {
                panic!("task")
            };
            task.labels.clear();
            unit.update(row.entity, row.revision)?;
            Ok(())
        })
        .await
        .unwrap();
    req.task_revision = req.task_revision.next().unwrap();
    let ClaimResult::Reserved { lease, .. } = claim(storage, caller(), req).await.unwrap() else {
        panic!("claim after approval")
    };
    assert_eq!(lease.epoch, 1, "refusal must not consume an epoch");
}

#[tokio::test]
async fn awaiting_approval_refuses_without_changes_in_memory() {
    approval_refusal(&FakeStorage::default()).await;
}

#[tokio::test]
async fn awaiting_approval_refuses_without_changes_on_files() {
    let root = tempfile::tempdir().unwrap();
    approval_refusal(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn stale_task_or_agent_revision_changes_nothing() {
    for kind in [EntityKind::Task, EntityKind::Agent] {
        let storage = FakeStorage::default();
        seed(&storage).await;
        let before = state(&storage).await;
        let mut req = request();
        if kind == EntityKind::Task {
            req.task_revision = Revision::ONE.next().unwrap();
        } else {
            req.agent_revision = Revision::ONE.next().unwrap();
        }
        assert!(matches!(claim(&storage, caller(), req).await,
            Err(StoreError::StaleRevision {kind: actual, ..}) if actual == kind));
        assert_eq!(state(&storage).await, before);
    }
}

#[tokio::test]
async fn missing_selected_record_does_not_substitute_another_candidate() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    let before = state(&storage).await;
    for missing_task in [true, false] {
        let mut req = request();
        if missing_task {
            req.task_id = "missing".into();
        } else {
            req.agent_id = "missing".into();
        }
        assert!(matches!(
            claim(&storage, caller(), req).await,
            Err(StoreError::StaleRevision { actual: None, .. })
        ));
        assert_eq!(state(&storage).await, before);
    }
}

#[tokio::test]
async fn unavailable_task_and_agent_refuse_without_writes() {
    for task_unavailable in [true, false] {
        let storage = FakeStorage::default();
        seed(&storage).await;
        storage
            .transaction(caller(), Lane::Entities, move |unit| {
                let kind = if task_unavailable {
                    EntityKind::Task
                } else {
                    EntityKind::Agent
                };
                let id = if task_unavailable {
                    "task-selected"
                } else {
                    "agent-selected"
                };
                let mut row = unit.get(kind, id)?.expect("row");
                match &mut row.entity {
                    Entity::Task(task) => task.status = TaskStatus::Retrying,
                    Entity::Agent(agent) => agent.status = AgentStatus::Disabled,
                    _ => unreachable!(),
                }
                unit.update(row.entity, row.revision)?;
                Ok(())
            })
            .await
            .unwrap();
        let before = state(&storage).await;
        let mut req = request();
        if task_unavailable {
            req.task_revision = Revision::ONE.next().unwrap();
        } else {
            req.agent_revision = Revision::ONE.next().unwrap();
        }
        let expected = if task_unavailable {
            Refusal::TaskUnavailable
        } else {
            Refusal::AgentUnavailable
        };
        assert_eq!(
            claim(&storage, caller(), req).await.unwrap(),
            ClaimResult::Refused(expected)
        );
        assert_eq!(state(&storage).await, before);
    }
}

async fn rollback(storage: &dyn Storage) {
    seed(storage).await;
    let before = state(storage).await;
    let result: Result<()> = storage
        .transaction(caller(), Lane::Claims, |unit| {
            assert!(matches!(
                reserve(unit, request(), Mode::Dispatch)?,
                ClaimResult::Reserved { .. }
            ));
            Err(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    assert_eq!(state(storage).await, before);
    let ClaimResult::Reserved { lease, .. } = claim(storage, caller(), request()).await.unwrap()
    else {
        panic!("claim")
    };
    assert_eq!(lease.epoch, 1, "rollback must restore epoch too");
}

#[tokio::test]
async fn error_after_all_writes_rolls_back_task_run_lease_and_epoch_in_memory() {
    rollback(&FakeStorage::default()).await;
}

#[tokio::test]
async fn error_after_all_writes_rolls_back_task_run_lease_and_epoch_on_files() {
    let root = tempfile::tempdir().unwrap();
    rollback(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn existing_run_aborts_the_new_lease_and_preserves_all_rows() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    claim(&storage, caller(), request()).await.unwrap();
    storage
        .transaction(caller(), Lane::Claims, |unit| {
            let lease = unit.get_lease("task-selected")?.expect("lease");
            unit.release_lease(&lease.task, &lease.lineage, lease.epoch)?;
            Ok(())
        })
        .await
        .unwrap();
    let before = state(&storage).await;
    let mut req = request();
    req.task_id = "task-other".into();
    req.agent_id = "agent-other".into();
    assert!(matches!(
        claim(&storage, caller(), req.clone()).await,
        Err(StoreError::AlreadyExists {
            kind: EntityKind::Run,
            ..
        })
    ));
    assert_eq!(state(&storage).await, before);
    req.run_id = "run-other".into();
    let ClaimResult::Reserved { lease, .. } = claim(&storage, caller(), req).await.unwrap() else {
        panic!("claim")
    };
    assert_eq!(lease.epoch, 1);
}

#[tokio::test]
async fn held_agent_refuses_another_task_without_creating_a_run() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    claim(&storage, caller(), request()).await.unwrap();
    let before = state(&storage).await;
    let mut req = request();
    req.task_id = "task-other".into();
    req.run_id = "run-other".into();
    assert!(matches!(
        claim(&storage, caller(), req).await.unwrap(),
        ClaimResult::Refused(Refusal::Held(_))
    ));
    assert_eq!(state(&storage).await, before);
}

#[tokio::test]
async fn concurrent_duplicate_claim_has_one_winner() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    let (a, b) = tokio::join!(
        claim(&storage, caller(), request()),
        claim(&storage, caller(), request())
    );
    assert_eq!(
        [a, b]
            .iter()
            .filter(|r| matches!(r, Ok(ClaimResult::Reserved { .. })))
            .count(),
        1
    );
    let before = state(&storage).await;
    assert!(matches!(
        claim(&storage, caller(), request()).await,
        Err(StoreError::StaleRevision { .. })
    ));
    assert_eq!(state(&storage).await, before);
}

#[tokio::test]
async fn assignment_and_capability_mismatch_refuse_the_selected_agent() {
    for capability in [false, true] {
        let storage = FakeStorage::default();
        seed(&storage).await;
        storage
            .transaction(caller(), Lane::Entities, move |unit| {
                let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
                let Entity::Task(task) = &mut row.entity else {
                    panic!("task")
                };
                if capability {
                    task.labels.push("cap:unavailable".into());
                } else {
                    task.assignee = Some("agent-other".into());
                }
                unit.update(row.entity, row.revision)?;
                Ok(())
            })
            .await
            .unwrap();
        let before = state(&storage).await;
        let mut req = request();
        req.task_revision = Revision::ONE.next().unwrap();
        assert_eq!(
            claim(&storage, caller(), req).await.unwrap(),
            ClaimResult::Refused(Refusal::AgentUnavailable)
        );
        assert_eq!(state(&storage).await, before);
    }
}

#[tokio::test]
async fn attempt_overflow_aborts_without_issuing_a_lease() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut row.entity else {
                panic!("task")
            };
            task.attempts = u32::MAX;
            unit.update(row.entity, row.revision)?;
            Ok(())
        })
        .await
        .unwrap();
    let before = state(&storage).await;
    let mut req = request();
    req.task_revision = Revision::ONE.next().unwrap();
    assert!(matches!(
        claim(&storage, caller(), req).await,
        Err(StoreError::CounterExhausted)
    ));
    assert_eq!(state(&storage).await, before);
}

#[tokio::test]
async fn a_lease_on_the_selected_task_refuses_without_changes() {
    let storage = FakeStorage::default();
    seed(&storage).await;
    storage
        .transaction(caller(), Lane::Claims, |unit| {
            unit.claim_lease(
                &NewLease {
                    task: "task-selected".into(),
                    agent: "agent-other".into(),
                    run: "run-other".into(),
                },
                0,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let before = state(&storage).await;
    assert!(matches!(
        claim(&storage, caller(), request()).await.unwrap(),
        ClaimResult::Refused(Refusal::Held(_))
    ));
    assert_eq!(state(&storage).await, before);
}

// ── explicit rerun of a failed or cancelled task ────────────────────────────
//
// 1.0 reset the budget in `TaskService::retry` (failed/cancelled → todo, attempts
// 0) and dispatched the todo task in a later save. Its `dispatch_claimed`
// re-run branch cannot be reached (`is_dispatchable` refuses a terminal task
// first), so `retry` plus the next dispatch is the behaviour to keep, and an
// automatic retry (`retrying`, attempts kept) is a different path. The retry
// service's own cases are `task_service::tests::retry_resets_a_failed_task_to_todo_with_no_attempts`,
// `::retry_also_resets_a_cancelled_task` and `::retry_refuses_anything_but_failed_or_cancelled`.
// The scenarios below are backend-neutral, like the ones above, so SQLite can
// register them with the rest of #157.

fn old_run() -> Run {
    Run {
        id: "run-old".into(),
        task_id: "task-selected".into(),
        agent_id: "agent-selected".into(),
        attempt: 3,
        status: RunStatus::Failed,
        started_at: "1970-01-01T00:00:00.000Z".into(),
        finished_at: Some("1970-01-01T00:00:00.000Z".into()),
        workspace_path: String::new(),
        prompt: String::new(),
        pid: None,
        error: Some("synthetic".into()),
        error_kind: None,
        tokens: None,
    }
}

/// Seeds the usual rows, then makes the selected task `status` with `attempts`
/// spent and one finished run of it; returns the selected task's revision.
async fn seed_terminal(storage: &dyn Storage, status: TaskStatus, attempts: u32) -> Revision {
    seed(storage).await;
    storage
        .transaction(caller(), Lane::Entities, move |unit| {
            let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut row.entity else {
                panic!("task")
            };
            task.status = status;
            task.attempts = attempts;
            task.max_attempts = 3;
            task.feedback = Some("synthetic feedback".into());
            let updated = unit.update(row.entity, row.revision)?;
            unit.create(Entity::Run(Box::new(old_run())))?;
            Ok(updated.revision)
        })
        .await
        .expect("seed terminal")
}

async fn rerun_state(storage: &dyn Storage) -> (Vec<Record>, Vec<Lease>) {
    let (mut rows, leases) = state(storage).await;
    let view = storage.read(caller()).await.expect("snapshot");
    rows.extend(view.get(EntityKind::Run, "run-old").await.expect("old run"));
    (rows, leases)
}

fn rerun_request(revision: Revision) -> ClaimRequest {
    ClaimRequest {
        task_revision: revision,
        ..request()
    }
}

async fn rerun_success(storage: &dyn Storage, status: TaskStatus, attempts: u32) {
    let revision = seed_terminal(storage, status, attempts).await;
    let before = rerun_state(storage).await;
    let ClaimResult::Reserved { task, run, lease } =
        claim_explicit_rerun(storage, caller(), rerun_request(revision))
            .await
            .expect("rerun")
    else {
        panic!("rerun refused");
    };
    let Entity::Task(task_data) = &task.entity else {
        panic!("task")
    };
    assert_eq!(task_data.status, TaskStatus::InProgress);
    assert_eq!(
        task_data.attempts, 1,
        "the budget restarts and one is spent"
    );
    assert_eq!(task_data.max_attempts, 3);
    assert_eq!(task_data.updated_at, "1970-01-01T00:00:01.234Z");
    assert_eq!(task_data.created_at, "1970-01-01T00:00:00.000Z");
    assert_eq!(task_data.feedback.as_deref(), Some("synthetic feedback"));
    assert_eq!(task_data.assignee.as_deref(), Some("agent-selected"));
    assert_eq!(task.revision, revision.next().unwrap());
    let Entity::Run(run_data) = &run.entity else {
        panic!("run")
    };
    assert_eq!(run_data.status, RunStatus::Preparing);
    assert_eq!(run_data.attempt, 1);
    assert_eq!(run_data.started_at, "1970-01-01T00:00:01.234Z");
    assert_eq!(lease.task, "task-selected");
    assert_eq!(lease.agent, "agent-selected");
    assert_eq!(lease.run, "run-selected");
    assert_eq!(lease.epoch, 1);
    assert_eq!(lease.expires_at, 1234 + orch_core::store::leases::LEASE_MS);
    let after = rerun_state(storage).await;
    for row in before.0.iter().filter(|r| r.entity.id() != "task-selected") {
        assert!(
            after.0.contains(row),
            "an unselected or earlier row changed"
        );
    }
    assert!(after.0.contains(task.as_ref()));
    assert!(after.0.contains(run.as_ref()));
    assert_eq!(after.1, vec![lease]);
    // The same budget cannot be reset twice: the task is now in progress.
    assert_eq!(
        claim_explicit_rerun(storage, caller(), rerun_request(task.revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::TaskUnavailable)
    );
    assert_eq!(rerun_state(storage).await, after);
}

#[tokio::test]
async fn a_failed_or_cancelled_task_restarts_its_budget_in_one_transaction_in_memory() {
    rerun_success(&FakeStorage::default(), TaskStatus::Failed, 3).await;
    rerun_success(&FakeStorage::default(), TaskStatus::Cancelled, 1).await;
}

#[tokio::test]
async fn a_failed_or_cancelled_task_restarts_its_budget_in_one_transaction_on_files() {
    for (status, attempts) in [(TaskStatus::Failed, 3), (TaskStatus::Cancelled, 1)] {
        let root = tempfile::tempdir().unwrap();
        let storage = FileStorage::new(root.path());
        rerun_success(&storage, status, attempts).await;
        let committed = rerun_state(&storage).await;
        drop(storage);
        assert_eq!(rerun_state(&FileStorage::new(root.path())).await, committed);
    }
}

#[tokio::test]
async fn an_exhausted_counter_does_not_stop_an_explicit_rerun() {
    // The ordinary claim at `u32::MAX` is `attempt_overflow_aborts_without_issuing_a_lease`;
    // a rerun starts from zero, as 1.0's `retry` did.
    let storage = FakeStorage::default();
    let revision = seed_terminal(&storage, TaskStatus::Failed, u32::MAX).await;
    let ClaimResult::Reserved { task, .. } =
        claim_explicit_rerun(&storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("rerun refused")
    };
    let Entity::Task(task) = task.entity else {
        panic!("task")
    };
    assert_eq!(task.attempts, 1);
}

async fn rerun_refused_without_change(storage: &dyn Storage, revision: Revision) {
    let before = rerun_state(storage).await;
    assert_eq!(
        claim_explicit_rerun(storage, caller(), rerun_request(revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::TaskUnavailable)
    );
    assert_eq!(rerun_state(storage).await, before);
}

#[tokio::test]
async fn only_failed_and_cancelled_tasks_can_be_rerun_and_the_ordinary_claim_refuses_them() {
    // `retry_refuses_anything_but_failed_or_cancelled`, at the reservation.
    // `retrying` has a queue and keeps its attempts: it is not an explicit rerun.
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Retrying,
        TaskStatus::Review,
        TaskStatus::Done,
    ] {
        let storage = FakeStorage::default();
        let revision = seed_terminal(&storage, status, 2).await;
        rerun_refused_without_change(&storage, revision).await;
    }
    for status in [TaskStatus::Failed, TaskStatus::Cancelled] {
        let storage = FakeStorage::default();
        let revision = seed_terminal(&storage, status, 2).await;
        let before = rerun_state(&storage).await;
        assert_eq!(
            claim(&storage, caller(), rerun_request(revision))
                .await
                .unwrap(),
            ClaimResult::Refused(Refusal::TaskUnavailable),
            "the ordinary claim keeps refusing {status}"
        );
        assert_eq!(rerun_state(&storage).await, before);
    }
}

async fn rerun_approval_refusal(storage: &dyn Storage) {
    seed_terminal(storage, TaskStatus::Failed, 3).await;
    let hold = |add: bool| {
        move |unit: &mut dyn UnitOfWork| {
            let mut row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut row.entity else {
                panic!("task")
            };
            if add {
                task.labels.push("awaiting-approval".into());
            } else {
                task.labels.clear();
            }
            Ok(unit.update(row.entity, row.revision)?.revision)
        }
    };
    let revision = storage
        .transaction(caller(), Lane::Entities, hold(true))
        .await
        .unwrap();
    // The refusal must not keep a budget reset: 1.0 reset in `retry` and
    // refused later, but the reservation is one unit.
    rerun_refused_without_change(storage, revision).await;
    let revision = storage
        .transaction(caller(), Lane::Entities, hold(false))
        .await
        .unwrap();
    let ClaimResult::Reserved { lease, task, .. } =
        claim_explicit_rerun(storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("rerun after approval")
    };
    assert_eq!(lease.epoch, 1, "refusal must not consume an epoch");
    let Entity::Task(task) = task.entity else {
        panic!("task")
    };
    assert_eq!(task.attempts, 1);
}

#[tokio::test]
async fn awaiting_approval_refuses_an_explicit_rerun_without_a_budget_reset_in_memory() {
    rerun_approval_refusal(&FakeStorage::default()).await;
}

#[tokio::test]
async fn awaiting_approval_refuses_an_explicit_rerun_without_a_budget_reset_on_files() {
    let root = tempfile::tempdir().unwrap();
    rerun_approval_refusal(&FileStorage::new(root.path())).await;
}

async fn rerun_rollback(storage: &dyn Storage) {
    let revision = seed_terminal(storage, TaskStatus::Failed, 3).await;
    let before = rerun_state(storage).await;
    let result: Result<()> = storage
        .transaction(caller(), Lane::Claims, move |unit| {
            assert!(matches!(
                reserve(unit, rerun_request(revision), Mode::ExplicitRerun)?,
                ClaimResult::Reserved { .. }
            ));
            Err(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    assert_eq!(
        rerun_state(storage).await,
        before,
        "the reset, the run, the lease and the task update roll back together"
    );
    let ClaimResult::Reserved { lease, task, .. } =
        claim_explicit_rerun(storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("rerun")
    };
    assert_eq!(lease.epoch, 1, "rollback must restore the epoch too");
    assert_eq!(task.revision, revision.next().unwrap());
}

#[tokio::test]
async fn error_after_an_explicit_rerun_rolls_back_the_reset_run_lease_and_epoch_in_memory() {
    rerun_rollback(&FakeStorage::default()).await;
}

#[tokio::test]
async fn error_after_an_explicit_rerun_rolls_back_the_reset_run_lease_and_epoch_on_files() {
    let root = tempfile::tempdir().unwrap();
    rerun_rollback(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn a_held_lease_stale_revision_or_unavailable_agent_keeps_the_spent_budget() {
    // Held: another run owns the task's lease.
    let storage = FakeStorage::default();
    let revision = seed_terminal(&storage, TaskStatus::Failed, 3).await;
    storage
        .transaction(caller(), Lane::Claims, |unit| {
            unit.claim_lease(
                &NewLease {
                    task: "task-selected".into(),
                    agent: "agent-other".into(),
                    run: "run-other".into(),
                },
                0,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let before = rerun_state(&storage).await;
    assert!(matches!(
        claim_explicit_rerun(&storage, caller(), rerun_request(revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::Held(_))
    ));
    assert_eq!(rerun_state(&storage).await, before);

    // Stale: the selected revision is not the stored one.
    let storage = FakeStorage::default();
    let revision = seed_terminal(&storage, TaskStatus::Cancelled, 2).await;
    let before = rerun_state(&storage).await;
    assert!(matches!(
        claim_explicit_rerun(&storage, caller(), rerun_request(revision.next().unwrap())).await,
        Err(StoreError::StaleRevision {
            kind: EntityKind::Task,
            ..
        })
    ));
    assert_eq!(rerun_state(&storage).await, before);

    // Unavailable: the selected agent is disabled, and a task assigned to
    // another agent refuses this one, as the ordinary claim does.
    for disable in [true, false] {
        let storage = FakeStorage::default();
        let seeded = seed_terminal(&storage, TaskStatus::Failed, 3).await;
        let mut req = rerun_request(seeded);
        let changed = storage
            .transaction(caller(), Lane::Entities, move |unit| {
                let (kind, id) = if disable {
                    (EntityKind::Agent, "agent-selected")
                } else {
                    (EntityKind::Task, "task-selected")
                };
                let mut row = unit.get(kind, id)?.expect("row");
                match &mut row.entity {
                    Entity::Agent(agent) => agent.status = AgentStatus::Disabled,
                    Entity::Task(task) => task.assignee = Some("agent-other".into()),
                    _ => unreachable!(),
                }
                Ok(unit.update(row.entity, row.revision)?.revision)
            })
            .await
            .unwrap();
        if disable {
            req.agent_revision = changed;
        } else {
            req.task_revision = changed;
        }
        let before = rerun_state(&storage).await;
        assert_eq!(
            claim_explicit_rerun(&storage, caller(), req).await.unwrap(),
            ClaimResult::Refused(Refusal::AgentUnavailable)
        );
        assert_eq!(rerun_state(&storage).await, before);
    }
}

#[tokio::test]
async fn concurrent_explicit_reruns_have_one_winner_and_spend_one_attempt() {
    let storage = FakeStorage::default();
    let revision = seed_terminal(&storage, TaskStatus::Failed, 3).await;
    let (a, b) = tokio::join!(
        claim_explicit_rerun(&storage, caller(), rerun_request(revision)),
        claim_explicit_rerun(&storage, caller(), rerun_request(revision))
    );
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| matches!(r, Ok(ClaimResult::Reserved { .. })))
            .count(),
        1
    );
    let after = rerun_state(&storage).await;
    let task = after
        .0
        .iter()
        .find_map(|row| match &row.entity {
            Entity::Task(task) if task.id == "task-selected" => Some(task),
            _ => None,
        })
        .expect("task");
    assert_eq!((task.status, task.attempts), (TaskStatus::InProgress, 1));
    assert_eq!(after.1.len(), 1);
    assert!(matches!(
        claim_explicit_rerun(&storage, caller(), rerun_request(revision)).await,
        Err(StoreError::StaleRevision { .. })
    ));
    assert_eq!(rerun_state(&storage).await, after);
}

// ── due automatic retry ─────────────────────────────────────────────────────
//
// An automatic retry is a `retrying` task with a row in the retry table. 1.0
// held it back while `now < due_at` and spent the next attempt of the budget it
// had when it dispatched it (`attempts + 1`); it never reset the budget. The
// reservation does that and removes the row (R35) in the transaction that takes
// the lease, so a refusal or an error leaves the task, its row and its
// attempts as they were. The scenarios take a `&dyn Storage` and run on both
// references, like the rerun ones above, so SQLite can register them with the
// rest of #157.

const DUE: i64 = 1234;

fn row(task: &str, attempt: u32, due_at: i64) -> Retry {
    Retry {
        task: task.into(),
        attempt,
        due_at,
        error: "synthetic failure".into(),
    }
}

/// Seeds the usual rows, then makes the selected task `retrying` with
/// `attempts` spent and one failed run of it. The selected task's row falls due
/// at `due_at`; `task-other` has a row of its own that must never change.
/// Returns the selected task's revision.
async fn seed_retrying(storage: &dyn Storage, attempts: u32, due_at: i64) -> Revision {
    seed(storage).await;
    storage
        .transaction(caller(), Lane::Entities, move |unit| {
            let mut task_row = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut task_row.entity else {
                panic!("task")
            };
            task.status = TaskStatus::Retrying;
            task.attempts = attempts;
            task.max_attempts = u32::MAX;
            task.feedback = Some("synthetic feedback".into());
            let updated = unit.update(task_row.entity, task_row.revision)?;
            unit.create(Entity::Run(Box::new(old_run())))?;
            unit.put_retry(&row("task-selected", attempts.saturating_add(1), due_at))?;
            unit.put_retry(&row("task-other", 1, i64::MAX))?;
            Ok(updated.revision)
        })
        .await
        .expect("seed retrying")
}

async fn retry_state(storage: &dyn Storage) -> (Vec<Record>, Vec<Lease>, Vec<Retry>) {
    let (rows, leases) = rerun_state(storage).await;
    let view = storage.read(caller()).await.expect("snapshot");
    let retries = view.read_retries(None, 1000).await.expect("retries").rows;
    (rows, leases, retries)
}

/// The selected task's row, if it still has one.
fn selected_row(retries: &[Retry]) -> Option<&Retry> {
    retries.iter().find(|retry| retry.task == "task-selected")
}

async fn due_refused(storage: &dyn Storage, revision: Revision, now_ms: i64, expected: Refusal) {
    let before = retry_state(storage).await;
    let mut req = rerun_request(revision);
    req.now_ms = now_ms;
    assert_eq!(
        claim_due_retry(storage, caller(), req).await.unwrap(),
        ClaimResult::Refused(expected)
    );
    assert_eq!(
        retry_state(storage).await,
        before,
        "a refusal changes no row, no lease and no retry row"
    );
}

async fn due_success(storage: &dyn Storage) {
    let revision = seed_retrying(storage, 2, DUE).await;
    let before = retry_state(storage).await;
    let ClaimResult::Reserved { task, run, lease } =
        claim_due_retry(storage, caller(), rerun_request(revision))
            .await
            .expect("due retry")
    else {
        panic!("due retry refused");
    };
    let Entity::Task(task_data) = &task.entity else {
        panic!("task")
    };
    assert_eq!(task_data.status, TaskStatus::InProgress);
    assert_eq!(
        task_data.attempts, 3,
        "the budget is kept and one more is spent, not reset"
    );
    assert_eq!(task_data.updated_at, "1970-01-01T00:00:01.234Z");
    assert_eq!(task_data.created_at, "1970-01-01T00:00:00.000Z");
    assert_eq!(task_data.feedback.as_deref(), Some("synthetic feedback"));
    assert_eq!(task_data.assignee.as_deref(), Some("agent-selected"));
    assert_eq!(task.revision, revision.next().unwrap());
    let Entity::Run(run_data) = &run.entity else {
        panic!("run")
    };
    assert_eq!(run_data.status, RunStatus::Preparing);
    assert_eq!(run_data.attempt, 3);
    assert_eq!(run_data.started_at, "1970-01-01T00:00:01.234Z");
    assert_eq!(lease.task, "task-selected");
    assert_eq!(lease.agent, "agent-selected");
    assert_eq!(lease.run, "run-selected");
    assert_eq!(lease.epoch, 1);
    assert_eq!(lease.expires_at, DUE + orch_core::store::leases::LEASE_MS);
    let after = retry_state(storage).await;
    for stored in before.0.iter().filter(|r| r.entity.id() != "task-selected") {
        assert!(
            after.0.contains(stored),
            "an unselected or earlier row changed"
        );
    }
    assert!(after.0.contains(task.as_ref()));
    assert!(after.0.contains(run.as_ref()));
    assert_eq!(after.1, vec![lease]);
    assert_eq!(
        after.2,
        vec![row("task-other", 1, i64::MAX)],
        "the selected task's row went with the claim; the other task's row did not"
    );
    // The row is gone and the task is in progress: it cannot be claimed twice.
    due_refused(storage, task.revision, DUE, Refusal::TaskUnavailable).await;
}

#[tokio::test]
async fn a_due_retry_keeps_its_budget_and_loses_its_row_in_one_transaction_in_memory() {
    due_success(&FakeStorage::default()).await;
}

#[tokio::test]
async fn a_due_retry_keeps_its_budget_and_loses_its_row_in_one_transaction_on_files() {
    let root = tempfile::tempdir().unwrap();
    let storage = FileStorage::new(root.path());
    due_success(&storage).await;
    let committed = retry_state(&storage).await;
    drop(storage);
    assert_eq!(retry_state(&FileStorage::new(root.path())).await, committed);
}

async fn due_boundary(storage: &dyn Storage) {
    let revision = seed_retrying(storage, 1, DUE + 1).await;
    // One millisecond early: refused, and the refusal consumed nothing.
    let early = Refusal::RetryNotDue { due_at: DUE + 1 };
    due_refused(storage, revision, DUE, early.clone()).await;
    // A row that someone moves later after the scheduler chose the task is
    // judged by its stored deadline, without touching the task's revision.
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            unit.put_retry(&row("task-selected", 2, i64::MAX))?;
            Ok(())
        })
        .await
        .unwrap();
    due_refused(
        storage,
        revision,
        DUE + 1,
        Refusal::RetryNotDue { due_at: i64::MAX },
    )
    .await;
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            unit.put_retry(&row("task-selected", 2, DUE + 1))?;
            Ok(())
        })
        .await
        .unwrap();
    // Exactly at the deadline it is due (`due_at <= now`), a millisecond late too.
    let mut req = rerun_request(revision);
    req.now_ms = DUE + 1;
    let ClaimResult::Reserved { lease, task, .. } =
        claim_due_retry(storage, caller(), req).await.unwrap()
    else {
        panic!("a row due at this very millisecond refused")
    };
    assert_eq!(lease.epoch, 1, "the refusals consumed no epoch");
    let Entity::Task(task) = task.entity else {
        panic!("task")
    };
    assert_eq!(task.attempts, 2);
    assert!(selected_row(&retry_state(storage).await.2).is_none());
}

#[tokio::test]
async fn a_retry_is_refused_before_its_deadline_and_taken_at_it_in_memory() {
    due_boundary(&FakeStorage::default()).await;
}

#[tokio::test]
async fn a_retry_is_refused_before_its_deadline_and_taken_at_it_on_files() {
    let root = tempfile::tempdir().unwrap();
    due_boundary(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn a_deadline_long_past_or_at_the_epoch_is_due() {
    for (due_at, now_ms) in [(0, 0), (0, DUE), (DUE - 1, DUE), (DUE, DUE)] {
        let storage = FakeStorage::default();
        let revision = seed_retrying(&storage, 1, due_at).await;
        let mut req = rerun_request(revision);
        req.now_ms = now_ms;
        assert!(
            matches!(
                claim_due_retry(&storage, caller(), req).await.unwrap(),
                ClaimResult::Reserved { .. }
            ),
            "due_at {due_at} at {now_ms}"
        );
    }
}

#[tokio::test]
async fn the_budget_comes_from_the_task_and_the_row_attempt_is_not_interpreted() {
    let storage = FakeStorage::default();
    let revision = seed_retrying(&storage, 2, DUE).await;
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            unit.put_retry(&row("task-selected", 1, DUE))?;
            Ok(())
        })
        .await
        .unwrap();
    let ClaimResult::Reserved { run, .. } =
        claim_due_retry(&storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("due retry refused")
    };
    let Entity::Run(run) = run.entity else {
        panic!("run")
    };
    assert_eq!(run.attempt, 3);
}

#[tokio::test]
async fn a_retrying_task_without_a_row_is_refused_and_nothing_changes() {
    let storage = FakeStorage::default();
    let revision = seed_retrying(&storage, 1, DUE).await;
    storage
        .transaction(caller(), Lane::Entities, |unit| {
            assert!(unit.delete_retry("task-selected")?);
            Ok(())
        })
        .await
        .unwrap();
    due_refused(&storage, revision, DUE, Refusal::NoRetry).await;
}

#[tokio::test]
async fn only_a_retrying_task_is_reserved_as_a_due_retry_and_the_others_keep_their_contracts() {
    // A stray row never makes another status reservable, and the row stays.
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        let storage = FakeStorage::default();
        let revision = seed_retrying(&storage, 2, DUE).await;
        let revision = storage
            .transaction(caller(), Lane::Entities, move |unit| {
                let mut stored = unit.get(EntityKind::Task, "task-selected")?.expect("task");
                let Entity::Task(task) = &mut stored.entity else {
                    panic!("task")
                };
                task.status = status;
                Ok(unit.update(stored.entity, revision)?.revision)
            })
            .await
            .unwrap();
        due_refused(&storage, revision, DUE, Refusal::TaskUnavailable).await;
    }
    // The ordinary claim and the explicit rerun keep refusing a due retry.
    let storage = FakeStorage::default();
    let revision = seed_retrying(&storage, 2, DUE).await;
    let before = retry_state(&storage).await;
    assert_eq!(
        claim(&storage, caller(), rerun_request(revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::TaskUnavailable)
    );
    assert_eq!(
        claim_explicit_rerun(&storage, caller(), rerun_request(revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::TaskUnavailable)
    );
    assert_eq!(retry_state(&storage).await, before);
}

async fn due_approval_refusal(storage: &dyn Storage) {
    seed_retrying(storage, 2, DUE).await;
    let hold = |add: bool| {
        move |unit: &mut dyn UnitOfWork| {
            let mut stored = unit.get(EntityKind::Task, "task-selected")?.expect("task");
            let Entity::Task(task) = &mut stored.entity else {
                panic!("task")
            };
            if add {
                task.labels.push("awaiting-approval".into());
            } else {
                task.labels.clear();
            }
            Ok(unit.update(stored.entity, stored.revision)?.revision)
        }
    };
    let revision = storage
        .transaction(caller(), Lane::Entities, hold(true))
        .await
        .unwrap();
    due_refused(storage, revision, DUE, Refusal::TaskUnavailable).await;
    let revision = storage
        .transaction(caller(), Lane::Entities, hold(false))
        .await
        .unwrap();
    let ClaimResult::Reserved { lease, .. } =
        claim_due_retry(storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("due retry after approval")
    };
    assert_eq!(lease.epoch, 1, "refusal must not consume an epoch");
    assert!(selected_row(&retry_state(storage).await.2).is_none());
}

#[tokio::test]
async fn awaiting_approval_refuses_a_due_retry_and_keeps_its_row_in_memory() {
    due_approval_refusal(&FakeStorage::default()).await;
}

#[tokio::test]
async fn awaiting_approval_refuses_a_due_retry_and_keeps_its_row_on_files() {
    let root = tempfile::tempdir().unwrap();
    due_approval_refusal(&FileStorage::new(root.path())).await;
}

async fn due_rollback(storage: &dyn Storage) {
    let revision = seed_retrying(storage, 2, DUE).await;
    let before = retry_state(storage).await;
    let result: Result<()> = storage
        .transaction(caller(), Lane::Claims, move |unit| {
            assert!(matches!(
                reserve(unit, rerun_request(revision), Mode::DueRetry)?,
                ClaimResult::Reserved { .. }
            ));
            assert!(
                unit.get_retry("task-selected")?.is_none(),
                "the row is already gone inside the transaction"
            );
            Err(StoreError::CounterExhausted)
        })
        .await;
    assert!(matches!(result, Err(StoreError::CounterExhausted)));
    assert_eq!(
        retry_state(storage).await,
        before,
        "the row, the run, the lease and the task update roll back together"
    );
    let ClaimResult::Reserved { lease, task, .. } =
        claim_due_retry(storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("due retry")
    };
    assert_eq!(lease.epoch, 1, "rollback must restore the epoch too");
    assert_eq!(task.revision, revision.next().unwrap());
}

#[tokio::test]
async fn error_after_a_due_retry_rolls_back_the_row_run_lease_and_epoch_in_memory() {
    due_rollback(&FakeStorage::default()).await;
}

#[tokio::test]
async fn error_after_a_due_retry_rolls_back_the_row_run_lease_and_epoch_on_files() {
    let root = tempfile::tempdir().unwrap();
    due_rollback(&FileStorage::new(root.path())).await;
}

async fn due_conflict_keeps_the_row(storage: &dyn Storage) {
    // A run id that exists aborts the transaction after the lease, and the
    // row is deleted only after that: both go back.
    let revision = seed_retrying(storage, 2, DUE).await;
    let mut req = rerun_request(revision);
    req.run_id = "run-old".into();
    let before = retry_state(storage).await;
    assert!(matches!(
        claim_due_retry(storage, caller(), req).await,
        Err(StoreError::AlreadyExists {
            kind: EntityKind::Run,
            ..
        })
    ));
    assert_eq!(retry_state(storage).await, before);

    // A stale task or agent revision, an attempt counter that cannot take one
    // more (no reset for an automatic retry, unlike an explicit rerun), a held
    // lease and an unavailable agent change nothing either.
    let mut stale = rerun_request(revision.next().unwrap());
    assert!(matches!(
        claim_due_retry(storage, caller(), stale.clone()).await,
        Err(StoreError::StaleRevision {
            kind: EntityKind::Task,
            ..
        })
    ));
    stale.task_revision = revision;
    stale.agent_revision = Revision::ONE.next().unwrap();
    assert!(matches!(
        claim_due_retry(storage, caller(), stale).await,
        Err(StoreError::StaleRevision {
            kind: EntityKind::Agent,
            ..
        })
    ));
    assert_eq!(retry_state(storage).await, before);

    let held = storage
        .transaction(caller(), Lane::Claims, |unit| {
            unit.claim_lease(
                &NewLease {
                    task: "task-selected".into(),
                    agent: "agent-other".into(),
                    run: "run-other".into(),
                },
                0,
            )?;
            Ok(())
        })
        .await;
    held.unwrap();
    let before = retry_state(storage).await;
    assert!(matches!(
        claim_due_retry(storage, caller(), rerun_request(revision))
            .await
            .unwrap(),
        ClaimResult::Refused(Refusal::Held(_))
    ));
    assert_eq!(retry_state(storage).await, before);
}

#[tokio::test]
async fn a_conflict_stale_revision_or_held_lease_keeps_the_retry_row_in_memory() {
    due_conflict_keeps_the_row(&FakeStorage::default()).await;
}

#[tokio::test]
async fn a_conflict_stale_revision_or_held_lease_keeps_the_retry_row_on_files() {
    let root = tempfile::tempdir().unwrap();
    due_conflict_keeps_the_row(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn an_unavailable_agent_or_an_exhausted_counter_keeps_the_row_and_the_budget() {
    for disable in [true, false] {
        let storage = FakeStorage::default();
        let seeded = seed_retrying(&storage, 2, DUE).await;
        let mut req = rerun_request(seeded);
        let changed = storage
            .transaction(caller(), Lane::Entities, move |unit| {
                let (kind, id) = if disable {
                    (EntityKind::Agent, "agent-selected")
                } else {
                    (EntityKind::Task, "task-selected")
                };
                let mut stored = unit.get(kind, id)?.expect("row");
                match &mut stored.entity {
                    Entity::Agent(agent) => agent.status = AgentStatus::Disabled,
                    Entity::Task(task) => task.assignee = Some("agent-other".into()),
                    _ => unreachable!(),
                }
                Ok(unit.update(stored.entity, stored.revision)?.revision)
            })
            .await
            .unwrap();
        if disable {
            req.agent_revision = changed;
        } else {
            req.task_revision = changed;
        }
        let before = retry_state(&storage).await;
        assert_eq!(
            claim_due_retry(&storage, caller(), req).await.unwrap(),
            ClaimResult::Refused(Refusal::AgentUnavailable)
        );
        assert_eq!(retry_state(&storage).await, before);
    }
    let storage = FakeStorage::default();
    let revision = seed_retrying(&storage, u32::MAX, DUE).await;
    let before = retry_state(&storage).await;
    assert!(matches!(
        claim_due_retry(&storage, caller(), rerun_request(revision)).await,
        Err(StoreError::CounterExhausted)
    ));
    assert_eq!(retry_state(&storage).await, before);
}

async fn due_race(storage: &dyn Storage) {
    let revision = seed_retrying(storage, 2, DUE).await;
    let (a, b) = tokio::join!(
        claim_due_retry(storage, caller(), rerun_request(revision)),
        claim_due_retry(storage, caller(), rerun_request(revision))
    );
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| matches!(r, Ok(ClaimResult::Reserved { .. })))
            .count(),
        1,
        "{a:?} {b:?}"
    );
    assert!(
        [&a, &b]
            .iter()
            .any(|r| matches!(r, Err(StoreError::StaleRevision { .. }))),
        "the loser saw the winner's task update: {a:?} {b:?}"
    );
    let after = retry_state(storage).await;
    let task = after
        .0
        .iter()
        .find_map(|stored| match &stored.entity {
            Entity::Task(task) if task.id == "task-selected" => Some(task),
            _ => None,
        })
        .expect("task");
    assert_eq!((task.status, task.attempts), (TaskStatus::InProgress, 3));
    assert_eq!(after.1.len(), 1);
    assert!(selected_row(&after.2).is_none());
    assert!(matches!(
        claim_due_retry(storage, caller(), rerun_request(revision)).await,
        Err(StoreError::StaleRevision { .. })
    ));
    assert_eq!(retry_state(storage).await, after);
}

#[tokio::test]
async fn concurrent_due_retries_have_one_winner_and_spend_one_attempt_in_memory() {
    due_race(&FakeStorage::default()).await;
}

#[tokio::test]
async fn concurrent_due_retries_have_one_winner_and_spend_one_attempt_on_files() {
    let root = tempfile::tempdir().unwrap();
    due_race(&FileStorage::new(root.path())).await;
}
