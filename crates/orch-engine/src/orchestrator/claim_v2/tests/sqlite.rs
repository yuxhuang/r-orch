//! The inactive reservation scenarios on the accepted composed SQLite adapter.
//! These do not authorize or activate the runtime dispatcher.
use super::*;
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use std::sync::Arc;

struct Fixture {
    storage: SqliteStorage,
    data: Arc<DataDir>,
    _root: tempfile::TempDir,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let data = Arc::new(
        DataDir::acquire(
            &root.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "/synthetic/orch".into(),
            },
        )
        .unwrap(),
    );
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    Fixture {
        storage,
        data,
        _root: root,
    }
}
async fn reopened(f: Fixture) {
    let before = retry_state(&f.storage).await;
    f.storage.close().await.unwrap();
    let storage = SqliteStorage::open(f.data, StorageOptions::default()).unwrap();
    assert_eq!(retry_state(&storage).await, before);
}

macro_rules! scenario {
    ($name:ident, $body:ident) => {
        #[tokio::test]
        async fn $name() {
            let f = fixture();
            $body(&f.storage).await;
            reopened(f).await;
        }
    };
}
scenario!(
    selected_task_agent_and_preparing_run_commit_together,
    success
);
scenario!(awaiting_approval_refuses_without_changes, approval_refusal);
scenario!(
    error_after_all_writes_rolls_back_task_run_lease_and_epoch,
    rollback
);
scenario!(
    awaiting_approval_refuses_an_explicit_rerun_without_a_budget_reset,
    rerun_approval_refusal
);
scenario!(
    error_after_an_explicit_rerun_rolls_back_the_reset_run_lease_and_epoch,
    rerun_rollback
);
scenario!(
    a_due_retry_keeps_its_budget_and_loses_its_row_in_one_transaction,
    due_success
);
scenario!(
    a_retry_is_refused_before_its_deadline_and_taken_at_it,
    due_boundary
);
scenario!(
    the_budget_comes_from_the_task_and_the_row_attempt_is_not_interpreted,
    due_task_budget
);
scenario!(
    a_retrying_task_without_a_row_is_refused_and_nothing_changes,
    due_missing_row
);
scenario!(
    awaiting_approval_refuses_a_due_retry_and_keeps_its_row,
    due_approval_refusal
);
scenario!(
    error_after_a_due_retry_rolls_back_the_row_run_lease_and_epoch,
    due_rollback
);
scenario!(
    a_conflict_stale_revision_or_held_lease_keeps_the_retry_row,
    due_conflict_keeps_the_row
);
scenario!(
    concurrent_due_retries_have_one_winner_and_spend_one_attempt,
    sqlite_due_race
);
scenario!(
    a_leased_idle_candidate_does_not_prevent_the_next_reservation,
    leased_idle_candidate
);
scenario!(
    reservations_preserve_caller_and_isolate_projects,
    project_and_caller
);

#[tokio::test]
async fn a_failed_or_cancelled_task_restarts_its_budget_in_one_transaction() {
    for (status, attempts) in [
        (TaskStatus::Failed, 3),
        (TaskStatus::Cancelled, 1),
        (TaskStatus::Failed, u32::MAX),
    ] {
        let f = fixture();
        rerun_success(&f.storage, status, attempts).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn a_deadline_long_past_or_at_the_epoch_is_due() {
    for (due_at, now_ms) in [(0, 0), (0, DUE), (DUE - 1, DUE), (DUE, DUE)] {
        let f = fixture();
        due_past_deadline(&f.storage, due_at, now_ms).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn only_a_retrying_task_is_reserved_as_a_due_retry_and_the_others_keep_their_contracts() {
    for status in [
        Some(TaskStatus::Todo),
        Some(TaskStatus::InProgress),
        Some(TaskStatus::Review),
        Some(TaskStatus::Done),
        Some(TaskStatus::Failed),
        Some(TaskStatus::Cancelled),
        None,
    ] {
        let f = fixture();
        due_start_mode_case(&f.storage, status).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn an_unavailable_agent_or_an_exhausted_counter_keeps_the_row_and_the_budget() {
    for disable in [Some(true), Some(false), None] {
        let f = fixture();
        due_agent_or_counter_case(&f.storage, disable).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn distinct_agents_contending_for_one_task_have_one_winner() {
    for mode in [Mode::Dispatch, Mode::ExplicitRerun, Mode::DueRetry] {
        let f = fixture();
        distinct_agent_contention(&f.storage, mode, RaceLoser::SqliteClaims).await;
        reopened(f).await;
    }
}

#[tokio::test]
async fn a_corrupt_retry_error_chunk_prevents_reservation_without_partial_writes() {
    use orch_infra::blob::{BlobHash, MAX_BLOB_BYTES};
    let f = fixture();
    let revision = seed_retrying(&f.storage, 2, DUE).await;
    let mut retry = row("task-selected", 3, DUE);
    // Two objects, with a distinct tail: damaging a later chunk must not let
    // the helper act on the already verified prefix.
    retry.error = "a".repeat(MAX_BLOB_BYTES as usize);
    retry.error.push('z');
    f.storage
        .transaction(caller(), Lane::Entities, move |u| u.put_retry(&retry))
        .await
        .unwrap();
    let before = state(&f.storage).await;
    let hash = BlobHash::of(b"z").to_string();
    let path = f
        ._root
        .path()
        .join("data/blobs/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    assert_eq!(std::fs::read(&path).unwrap(), b"z");
    std::fs::write(&path, b"x").unwrap();
    assert!(
        claim_due_retry(&f.storage, caller(), rerun_request(revision))
            .await
            .is_err()
    );
    // Reopen also detects a partial commit or an epoch spent on failure.
    f.storage.close().await.unwrap();
    std::fs::write(&path, b"z").unwrap();
    let storage = SqliteStorage::open(f.data, StorageOptions::default()).unwrap();
    assert_eq!(state(&storage).await, before);
    let stored = storage
        .read(caller())
        .await
        .unwrap()
        .read_retry("task-selected")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.error.len(), MAX_BLOB_BYTES as usize + 1);
    assert!(stored.error.starts_with('a') && stored.error.ends_with('z'));
    let ClaimResult::Reserved { lease, .. } =
        claim_due_retry(&storage, caller(), rerun_request(revision))
            .await
            .unwrap()
    else {
        panic!("repaired retry")
    };
    assert_eq!(lease.epoch, 1);
}

// This child exits without unwinding the writer or closing its connections.
// Invoked only by the isolated parent below; ordinary inventory is a no-op.
#[tokio::test]
async fn interrupted_reservation_child() {
    let Some(root) = std::env::var_os("ORCH_TEST_RESERVATION_CRASH_ROOT") else {
        return;
    };
    let data = Arc::new(
        DataDir::acquire(
            &std::path::PathBuf::from(root).join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "/synthetic/orch".into(),
            },
        )
        .unwrap(),
    );
    let storage = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    let revision = seed_retrying(&storage, 2, DUE).await;
    let committed = std::env::var_os("ORCH_TEST_RESERVATION_COMMIT").is_some();
    storage
        .transaction(caller(), Lane::Claims, move |u| {
            assert!(matches!(
                reserve(u, rerun_request(revision), Mode::DueRetry)?,
                ClaimResult::Reserved { .. }
            ));
            if !committed {
                std::process::exit(73);
            }
            Ok(())
        })
        .await
        .unwrap();
    std::process::exit(73);
}

#[tokio::test]
async fn interrupted_reservations_reopen_atomically_and_keep_epoch_and_lineage() {
    for committed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "orchestrator::claim_v2::tests::sqlite::interrupted_reservation_child",
                "--nocapture",
            ])
            .env_clear()
            .env("HOME", root.path())
            .env("TMPDIR", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("XDG_RUNTIME_DIR", root.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                root.path().join("global.yml"),
            )
            .env("ORCH_DATA_HOME", root.path().join("unused"))
            .env("ORCH_TEST_RESERVATION_CRASH_ROOT", root.path())
            .kill_on_drop(true);
        if committed {
            child.env("ORCH_TEST_RESERVATION_COMMIT", "1");
        }
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(73), "{output:?}");
        let data = Arc::new(
            DataDir::acquire(
                &root.path().join("data"),
                &Holder {
                    kind: LockKind::Daemon,
                    pid: std::process::id(),
                    started_at: "synthetic".into(),
                    binary_version: "test".into(),
                    executable: "/synthetic/orch".into(),
                },
            )
            .unwrap(),
        );
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let rows = retry_state(&storage).await;
        let lease = if committed {
            assert_eq!(rows.1.len(), 1);
            assert!(selected_row(&rows.2).is_none());
            let Entity::Task(task) = &rows
                .0
                .iter()
                .find(|r| r.entity.id() == "task-selected")
                .unwrap()
                .entity
            else {
                panic!("task")
            };
            assert_eq!(task.attempts, 3);
            assert_eq!(task.status, TaskStatus::InProgress);
            assert!(rows.0.iter().any(|r| r.entity.id() == "run-selected"));
            rows.1[0].clone()
        } else {
            assert!(rows.1.is_empty());
            assert!(selected_row(&rows.2).is_some());
            assert!(!rows.0.iter().any(|r| r.entity.id() == "run-selected"));
            let ClaimResult::Reserved { lease, .. } = claim_due_retry(
                &storage,
                caller(),
                rerun_request(Revision::ONE.next().unwrap()),
            )
            .await
            .unwrap() else {
                panic!("retry after interrupted write")
            };
            lease
        };
        assert_eq!(lease.epoch, 1);
        assert!(!lease.lineage.is_empty());
        let release = lease.clone();
        storage
            .transaction(caller(), Lane::Claims, move |u| {
                u.release_lease(&release.task, &release.lineage, release.epoch)?;
                Ok(())
            })
            .await
            .unwrap();
        storage.close().await.unwrap();
        let storage = SqliteStorage::open(data, StorageOptions::default()).unwrap();
        let next = storage
            .transaction(caller(), Lane::Claims, |u| {
                u.claim_lease(
                    &NewLease {
                        task: "task-selected".into(),
                        agent: "agent-selected".into(),
                        run: "run-next".into(),
                    },
                    DUE,
                )
            })
            .await
            .unwrap();
        let ClaimOutcome::Granted(next) = next else {
            panic!("next epoch")
        };
        assert_eq!(next.epoch, 2);
        assert_eq!(next.lineage, lease.lineage);
    }
}

async fn sqlite_due_race(storage: &dyn Storage) {
    due_race(storage, RaceLoser::SqliteClaims).await;
}
async fn sqlite_duplicate_claim(storage: &dyn Storage) {
    duplicate_claim(storage, RaceLoser::SqliteClaims).await;
}
async fn sqlite_rerun_race(storage: &dyn Storage) {
    rerun_race(storage, RaceLoser::SqliteClaims).await;
}
scenario!(
    missing_selected_record_does_not_substitute_another_candidate,
    missing_selected
);
scenario!(
    existing_run_aborts_the_new_lease_and_preserves_all_rows,
    existing_run
);
scenario!(
    held_agent_refuses_another_task_without_creating_a_run,
    held_agent
);
scenario!(
    attempt_overflow_aborts_without_issuing_a_lease,
    attempt_overflow
);
scenario!(
    a_lease_on_the_selected_task_refuses_without_changes,
    held_task
);
scenario!(
    an_exhausted_counter_does_not_stop_an_explicit_rerun,
    rerun_exhausted
);
scenario!(
    concurrent_duplicate_claim_has_one_winner,
    sqlite_duplicate_claim
);
scenario!(
    concurrent_explicit_reruns_have_one_winner_and_spend_one_attempt,
    sqlite_rerun_race
);
#[tokio::test]
async fn stale_task_or_agent_revision_changes_nothing() {
    for kind in [EntityKind::Task, EntityKind::Agent] {
        let f = fixture();
        stale_revision(&f.storage, kind).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn unavailable_task_and_agent_refuse_without_writes() {
    for task_unavailable in [true, false] {
        let f = fixture();
        unavailable(&f.storage, task_unavailable).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn assignment_and_capability_mismatch_refuse_the_selected_agent() {
    for capability in [false, true] {
        let f = fixture();
        mismatch(&f.storage, capability).await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn only_failed_and_cancelled_tasks_can_be_rerun_and_the_ordinary_claim_refuses_them() {
    for status in [
        TaskStatus::Todo,
        TaskStatus::InProgress,
        TaskStatus::Retrying,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        let f = fixture();
        rerun_status(
            &f.storage,
            status,
            matches!(status, TaskStatus::Failed | TaskStatus::Cancelled),
        )
        .await;
        reopened(f).await;
    }
}
#[tokio::test]
async fn a_held_lease_stale_revision_or_unavailable_agent_keeps_the_spent_budget() {
    let f = fixture();
    rerun_held(&f.storage).await;
    reopened(f).await;
    let f = fixture();
    rerun_stale(&f.storage).await;
    reopened(f).await;
    for disable in [true, false] {
        let f = fixture();
        rerun_unavailable(&f.storage, disable).await;
        reopened(f).await;
    }
}

#[test]
fn claims_loser_accepts_only_a_stale_task_revision_or_the_exact_overload_error() {
    let overloaded = Err(StoreError::Backend(orch_core::Error::other("overloaded")));
    assert!(RaceLoser::SqliteClaims.accepts(&overloaded));
    assert!(!RaceLoser::RevisionOnly.accepts(&overloaded));
    for outcome in [
        Err(StoreError::Backend(orch_core::Error::other(
            "storage_unavailable",
        ))),
        Err(StoreError::Backend(orch_core::Error::other(
            "SQLite storage operation failed",
        ))),
        Err(StoreError::Backend(orch_core::Error::other(
            "not overloaded",
        ))),
        Err(StoreError::Backend(orch_core::Error::invalid_arguments(
            "overloaded",
        ))),
        Err(StoreError::CounterExhausted),
        Ok(ClaimResult::Refused(Refusal::TaskUnavailable)),
        Err(StoreError::StaleRevision {
            kind: EntityKind::Agent,
            id: "agent-selected".into(),
            expected: Revision::ONE,
            actual: None,
        }),
    ] {
        assert!(!RaceLoser::SqliteClaims.accepts(&outcome), "{outcome:?}");
    }
}
