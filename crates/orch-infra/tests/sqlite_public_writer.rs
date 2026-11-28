//! Public Storage proofs for issue 127 (R128/R140), complementing sqlite_writer_policy.
//! SQL is used only to plant deterministic faults in this test's private database.
//! Automatic retries, event coalescing and bulk accounting need producer support;
//! a rebuilt caller request below does not claim to replay an admitted FnOnce.
use orch_core::{
    store::{
        Caller, Entity, EntityKind, Lane, Principal, Storage, StoreError, UnitOfWork, WriteWork,
    },
    test_support::make_task,
};
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{FailureSource, FatalCause, Role, SqliteStorage, StorageOptions},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    task::Poll,
    time::{Duration, Instant},
};

// These tests control the real executor with barriers. Daemon-owned admission
// cannot expire while an unrelated host process delays the barrier's release.
// Normal Storage::write budgets/withdrawal remain pinned in storage/tests.rs.
trait FixtureAdmission {
    async fn fixture_write(
        &self,
        caller: Caller,
        lane: Lane,
        work: WriteWork,
    ) -> orch_core::store::Result<()>;
    async fn fixture_transaction<F>(
        &self,
        caller: Caller,
        lane: Lane,
        work: F,
    ) -> orch_core::store::Result<()>
    where
        F: FnOnce(&mut dyn UnitOfWork) -> orch_core::store::Result<()> + Send + 'static;
}
impl FixtureAdmission for SqliteStorage {
    async fn fixture_write(
        &self,
        caller: Caller,
        lane: Lane,
        work: WriteWork,
    ) -> orch_core::store::Result<()> {
        let mut operation = self.retained_write(caller, lane, None);
        let receipt = operation.attempt(work).await?;
        assert!(
            operation
                .receive()
                .await
                .err()
                .expect("no second receipt")
                .to_string()
                .contains("no pending"),
            "an admitted attempt has exactly one original terminal receipt"
        );
        receipt.result
    }
    async fn fixture_transaction<F>(
        &self,
        caller: Caller,
        lane: Lane,
        work: F,
    ) -> orch_core::store::Result<()>
    where
        F: FnOnce(&mut dyn UnitOfWork) -> orch_core::store::Result<()> + Send + 'static,
    {
        self.fixture_write(caller, lane, Box::new(work)).await
    }
}

fn owner() -> Caller {
    Caller {
        project: "synthetic".into(),
        principal: Principal::Owner {
            identity: "test".into(),
        },
    }
}
fn team(id: &str) -> Entity {
    Entity::Team(Box::new(
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "status": "active", "members": [],
            "task_pool": [], "lead_agent_id": "a", "created_at": "opaque",
            "updated_at": "opaque", "config": {"auto_claim": false}
        }))
        .unwrap(),
    ))
}
struct Fixture {
    _root: tempfile::TempDir,
    data: Arc<DataDir>,
    storage: SqliteStorage,
}
impl Fixture {
    fn new() -> Self {
        Self::with_capacity(1)
    }
    fn with_capacity(capacity: usize) -> Self {
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
        let storage = SqliteStorage::open(
            data.clone(),
            StorageOptions {
                writer_capacity: capacity,
                ..Default::default()
            },
        )
        .unwrap();
        Self {
            _root: root,
            data,
            storage,
        }
    }
    fn sql(&self, sql: &str) {
        rusqlite::Connection::open(self.data.database_path())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    fn team_ids(&self) -> Vec<String> {
        let conn = rusqlite::Connection::open(self.data.database_path()).unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM teams WHERE project_id='synthetic' ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
    async fn corrupt_task(&self) {
        self.storage
            .fixture_transaction(owner(), Lane::Entities, |u| {
                let mut task = make_task();
                task.created_at = "2025-01-01T00:00:00.000Z".into();
                task.updated_at = task.created_at.clone();
                u.create(Entity::Task(Box::new(task))).map(|_| ())
            })
            .await
            .unwrap();
        self.sql("DELETE FROM task_details WHERE project_id='synthetic' AND task_id='tsk_test1'");
    }
    async fn reopen(&self) -> SqliteStorage {
        self.storage.close().await.unwrap();
        SqliteStorage::open(self.data.clone(), StorageOptions::default()).unwrap()
    }
}

// Poll exactly once to establish admission before releasing the active callback.
// No timeout/scheduling guess is used as evidence that the request was queued.
async fn admit<F: Future>(mut future: Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "write must await its queued receipt"
        );
        Poll::Ready(())
    })
    .await;
}
struct Release(Option<mpsc::SyncSender<()>>);
impl Release {
    fn now(&mut self) {
        self.0.take().unwrap().send(()).unwrap();
    }
}
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}

#[tokio::test]
async fn a_request_answered_overloaded_never_commits_and_a_rebuilt_retry_drops_nothing() {
    let f = Fixture::new();
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::sync_channel(1);
    let mut release = Release(Some(release));
    let first = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(move |u| {
            u.create(team("active"))?;
            started.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("release active callback");
            Ok(())
        }),
    );
    tokio::pin!(first);
    admit(first.as_mut()).await;
    active.await.unwrap();
    let queued = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(|u| {
            u.create(team("queued"))?;
            Ok(())
        }),
    );
    tokio::pin!(queued);
    admit(queued.as_mut()).await;
    let invocations = Arc::new(AtomicUsize::new(0));
    let refused_count = invocations.clone();
    let refused = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(move |u| {
            refused_count.fetch_add(1, Ordering::SeqCst);
            u.create(team("retry-a"))?;
            u.create(team("retry-b"))?;
            Ok(())
        }),
    );
    tokio::pin!(refused);
    let Poll::Ready(Err(refused)) = futures::poll!(refused.as_mut()) else {
        panic!("full admission must refuse before waiting for a receipt");
    };
    assert!(refused.to_string().contains("overloaded"), "{refused}");
    release.now();
    first.await.unwrap();
    queued.await.unwrap();
    // Both receipts have drained the refused request's possible queue position.
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    assert_eq!(f.team_ids(), ["active", "queued"]);
    let retry_count = invocations.clone();
    f.storage
        .fixture_transaction(owner(), Lane::Entities, move |u| {
            retry_count.fetch_add(1, Ordering::SeqCst);
            u.create(team("retry-a"))?;
            u.create(team("retry-b"))?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    let reopened = f.reopen().await;
    assert_eq!(f.team_ids(), ["active", "queued", "retry-a", "retry-b"]);
    let snapshot = reopened.read(owner()).await.unwrap();
    for id in ["retry-a", "retry-b"] {
        let record = snapshot.get(EntityKind::Team, id).await.unwrap().unwrap();
        assert_eq!(record.revision, orch_core::store::Revision::ONE);
    }
    drop(snapshot);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn a_failed_request_rolls_back_all_writes_and_a_rebuilt_request_commits_each_once() {
    let f = Fixture::new();
    let failed = f
        .storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("first"))?;
            u.create(team("second"))?;
            Err::<(), _>(StoreError::StaleEpoch)
        })
        .await;
    assert!(matches!(failed, Err(StoreError::StaleEpoch)));
    assert!(f.team_ids().is_empty());
    f.storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("first"))?;
            u.create(team("second"))?;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = f.reopen().await;
    assert_eq!(f.team_ids(), ["first", "second"]);
    let snapshot = reopened.read(owner()).await.unwrap();
    for (id, position) in [("first", 1), ("second", 2)] {
        let record = snapshot.get(EntityKind::Team, id).await.unwrap().unwrap();
        assert_eq!(
            (record.position, record.revision),
            (position, orch_core::store::Revision::ONE)
        );
    }
    drop(snapshot);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn an_ordinary_failure_rolled_back_to_its_savepoint_may_be_caught_and_unrelated_work_commits()
{
    let f = Fixture::new();
    f.sql(
        "CREATE TRIGGER refuse_detail BEFORE INSERT ON entity_details
        WHEN NEW.family='team' AND NEW.entity_key='refused'
        BEGIN SELECT RAISE(ABORT, 'synthetic constraint'); END",
    );
    f.storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("before"))?;
            assert!(u.create(team("refused")).is_err());
            // The failed create had already inserted its native teams row. A working
            // savepoint must remove it without undoing the enclosing callback.
            assert!(u.get(EntityKind::Team, "refused")?.is_none());
            u.create(team("after"))?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(f.storage.failure_state().failure().is_none());
    let reopened = f.reopen().await;
    assert_eq!(f.team_ids(), ["after", "before"]);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn no_statement_is_durable_when_sqlite_ends_the_transaction_and_the_callback_catches_it() {
    let f = Fixture::new();
    f.sql(
        "CREATE TRIGGER end_transaction BEFORE INSERT ON entity_details
        WHEN NEW.family='team' AND NEW.entity_key='refused'
        BEGIN SELECT RAISE(ROLLBACK, 'synthetic rollback'); END",
    );
    let result = f
        .storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("before"))?;
            assert!(u.create(team("refused")).is_err());
            assert!(u.create(team("after")).is_err());
            Ok(())
        })
        .await;
    let error = result.unwrap_err();
    assert!(
        error.to_string().contains("SQLite writer is closed"),
        "{error}"
    );
    assert!(
        f.storage.failure_state().failure().is_none(),
        "ordinary cleanup failure retires the worker without latching"
    );
    let later = f
        .storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("refused-later")).map(|_| ())
        })
        .await
        .unwrap_err();
    assert!(
        later.to_string().contains("SQLite writer is closed"),
        "{later}"
    );
    assert!(f.storage.close().await.is_err());
    let reopened = SqliteStorage::open(f.data.clone(), StorageOptions::default()).unwrap();
    assert!(
        f.team_ids().is_empty(),
        "nothing before or after cleanup was committed"
    );
    reopened
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("following")).map(|_| ())
        })
        .await
        .unwrap();
    assert_eq!(f.team_ids(), ["following"]);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn a_fatal_cause_that_the_callback_catches_still_fails_the_writer_and_aborts_the_transaction()
{
    let f = Fixture::new();
    f.corrupt_task().await;
    let result = f
        .storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(team("before"))?;
            assert!(u.get(EntityKind::Task, "tsk_test1").is_err());
            assert!(u.create(team("after")).is_err());
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(
        result.to_string().contains("storage_unavailable"),
        "{result}"
    );
    let failure = f.storage.failure_state().failure().unwrap();
    assert_eq!(
        (failure.source, failure.causes),
        (
            FailureSource::Writer,
            vec![(Role::Operation, FatalCause::LogicalCorruption)]
        )
    );
    f.storage.close().await.unwrap();
    assert!(f.team_ids().is_empty());
}

#[tokio::test]
async fn a_corrupt_result_on_a_read_connection_fails_every_public_writer_lane() {
    let f = Fixture::new();
    f.corrupt_task().await;
    let snapshot = f.storage.read(owner()).await.unwrap();
    assert!(snapshot.get(EntityKind::Task, "tsk_test1").await.is_err());
    drop(snapshot);
    let failure = f.storage.failure_state().failure().unwrap();
    assert_eq!(
        (failure.source, failure.causes),
        (
            FailureSource::Reader,
            vec![(Role::Operation, FatalCause::LogicalCorruption)]
        )
    );
    let invocations = Arc::new(AtomicUsize::new(0));
    for lane in [
        Lane::Claims,
        Lane::Entities,
        Lane::Events,
        Lane::Maintenance,
    ] {
        let calls = invocations.clone();
        let error = f
            .storage
            .write(
                owner(),
                lane,
                Box::new(move |u| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    u.create(team("forbidden"))?;
                    Ok(())
                }),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("storage_unavailable"),
            "{lane:?}: {error}"
        );
    }
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    f.storage.close().await.unwrap();
    assert!(f.team_ids().is_empty());
}

#[tokio::test]
async fn only_one_event_transaction_waits_across_runs_and_a_refused_batch_can_be_rebuilt() {
    // Spare capacity distinguishes the event-specific bound from the queue limit.
    let f = Fixture::with_capacity(8);
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::sync_channel(1);
    let mut release = Release(Some(release));
    let first = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(move |_| {
            started.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("release active callback");
            Ok(())
        }),
    );
    tokio::pin!(first);
    admit(first.as_mut()).await;
    active.await.unwrap();
    let queued = f.storage.fixture_write(
        owner(),
        Lane::Events,
        Box::new(|u| u.record_event_gap("run-one", 0, 1)),
    );
    tokio::pin!(queued);
    admit(queued.as_mut()).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let refused_calls = calls.clone();
    let refused = f.storage.fixture_write(
        owner(),
        Lane::Events,
        Box::new(move |u| {
            refused_calls.fetch_add(1, Ordering::SeqCst);
            u.record_event_gap("run-two", 0, 2)
        }),
    );
    tokio::pin!(refused);
    let Poll::Ready(Err(error)) = futures::poll!(refused.as_mut()) else {
        panic!("a second waiting event transaction must be refused with spare capacity");
    };
    assert!(error.to_string().contains("overloaded"), "{error}");
    release.now();
    first.await.unwrap();
    queued.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    f.storage
        .fixture_transaction(owner(), Lane::Events, |u| {
            u.record_event_gap("run-two", 0, 2)
        })
        .await
        .unwrap();
    let reopened = f.reopen().await;
    let snapshot = reopened.read(owner()).await.unwrap();
    for (run, dropped) in [("run-one", 1), ("run-two", 2)] {
        assert_eq!(
            snapshot.read_event_gaps(run).await.unwrap(),
            [orch_core::store::events::EventGap {
                after_seq: 0,
                dropped
            }]
        );
    }
    drop(snapshot);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn caught_native_entity_corruption_rolls_back_and_refuses_an_unrelated_queued_write() {
    let f = Fixture::with_capacity(8);
    f.storage
        .fixture_transaction(owner(), Lane::Entities, |u| {
            u.create(Entity::Config(orch_core::store::ConfigEntry {
                key: "damaged".into(),
                value: "synthetic".into(),
            }))
            .map(|_| ())
        })
        .await
        .unwrap();
    f.sql("DELETE FROM entity_details WHERE project_id='synthetic' AND family='config' AND entity_key='damaged'");
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::sync_channel(1);
    let mut release = Release(Some(release));
    let first = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(move |u| {
            u.create(team("before"))?;
            started.send(()).unwrap();
            released
                .recv_timeout(Duration::from_secs(10))
                .expect("release before corrupt read");
            assert!(u.get(EntityKind::Config, "damaged").is_err());
            assert!(u.create(team("after")).is_err());
            Ok(())
        }),
    );
    tokio::pin!(first);
    admit(first.as_mut()).await;
    active.await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let queued_calls = calls.clone();
    let queued = f.storage.fixture_write(
        owner(),
        Lane::Entities,
        Box::new(move |u| {
            queued_calls.fetch_add(1, Ordering::SeqCst);
            u.create(team("queued"))?;
            Ok(())
        }),
    );
    tokio::pin!(queued);
    admit(queued.as_mut()).await;
    release.now();
    for result in [first.await, queued.await] {
        let error = result.unwrap_err();
        assert!(error.to_string().contains("storage_unavailable"), "{error}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let failure = f.storage.failure_state().failure().unwrap();
    assert_eq!(
        (failure.source, failure.causes),
        (
            FailureSource::Writer,
            vec![(Role::Operation, FatalCause::LogicalCorruption)]
        )
    );
    f.storage.close().await.unwrap();
    assert!(f.team_ids().is_empty());
}

#[tokio::test]
async fn an_ordinary_public_close_is_distinct_from_a_failed_writer() {
    let f = Fixture::new();
    f.storage.close().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let closed_calls = calls.clone();
    let error = f
        .storage
        .write(
            owner(),
            Lane::Claims,
            Box::new(move |_| {
                closed_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("closed"), "{error}");
    assert!(
        !error.to_string().contains("storage_unavailable"),
        "{error}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(f.storage.failure_state().failure().is_none());
}

#[tokio::test]
async fn an_original_expired_deadline_refuses_the_callback_and_has_one_terminal_receipt() {
    let f = Fixture::new();
    let mut operation = f.storage.retained_write(
        owner(),
        Lane::Entities,
        Some(Instant::now() - Duration::from_secs(1)),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let receipt = operation
        .attempt(Box::new(move |u| {
            seen.fetch_add(1, Ordering::SeqCst);
            u.create(team("expired")).map(|_| ())
        }))
        .await
        .unwrap();
    assert!(
        receipt
            .result
            .unwrap_err()
            .to_string()
            .contains("overloaded")
    );
    assert_eq!(
        receipt.outcome,
        orch_infra::sqlite::WriteOutcome::NotStarted
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        operation
            .receive()
            .await
            .err()
            .expect("no second receipt")
            .to_string()
            .contains("no pending")
    );
    let reopened = f.reopen().await;
    assert!(f.team_ids().is_empty());
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn cancelling_delivery_recovers_the_original_receipt_without_reinvoking_the_callback() {
    let f = Fixture::new();
    let mut operation = f.storage.retained_write(owner(), Lane::Entities, None);
    let (started, active) = tokio::sync::oneshot::channel();
    let (release, released) = mpsc::sync_channel(1);
    let mut release = Release(Some(release));
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut delivery = Box::pin(operation.attempt(Box::new(move |u| {
        seen.fetch_add(1, Ordering::SeqCst);
        u.create(team("original"))?;
        started.send(()).unwrap();
        released
            .recv_timeout(Duration::from_secs(10))
            .expect("release original callback");
        Ok(())
    })));
    admit(delivery.as_mut()).await;
    active.await.unwrap();
    drop(delivery);
    // Cancellation does not permit a new attempt while the original is pending.
    let retry_calls = calls.clone();
    let refused = operation
        .attempt(Box::new(move |_| {
            retry_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }))
        .await;
    assert!(
        refused
            .err()
            .expect("pending original must refuse reconstruction")
            .to_string()
            .contains("prior receipt")
    );
    release.now();
    let receipt = operation.receive().await.unwrap();
    receipt.result.unwrap();
    assert_eq!(receipt.outcome, orch_infra::sqlite::WriteOutcome::Committed);
    assert!(
        operation
            .receive()
            .await
            .err()
            .expect("no second receipt")
            .to_string()
            .contains("no pending")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let reopened = f.reopen().await;
    assert_eq!(f.team_ids(), ["original"]);
    reopened.close().await.unwrap();
}
