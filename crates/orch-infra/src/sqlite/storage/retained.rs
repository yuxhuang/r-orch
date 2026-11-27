//! SQLite-only producer extension. Core callbacks remain FnOnce and Send.
use super::*;
use crate::sqlite::writer::policy::Episode;
use crate::sqlite::{RetryCode, WriteAccounting, WriteOutcome};
use std::future::Future;

pub(in crate::sqlite) type UnitWork =
    Box<dyn FnOnce(&mut Unit<'_, '_>) -> store::Result<()> + Send>;

/// A transaction's public answer together with authoritative durability and work.
/// A failed result is not permission to rebuild: check `retryable()` first.
pub struct WriteAttempt {
    pub result: store::Result<()>,
    pub outcome: WriteOutcome,
    pub retry_code: Option<RetryCode>,
    pub accounting: WriteAccounting,
    rebuild: bool,
}
impl WriteAttempt {
    /// Only a confirmed noncommit with a structured retryable SQL cause permits
    /// reconstruction. An unknown outcome must first be reconciled.
    pub fn retryable(&self) -> bool {
        self.rebuild
    }
}

/// One owned logical operation, permanently bound to its storage, caller, lane
/// and original deadline. It cannot be cloned or submitted concurrently. Keep
/// immutable producer input until commit is known. Dropping it ends its episode;
/// dropping the future of an admitted attempt does not cancel that attempt.
pub struct RetainedWrite {
    storage: SqliteStorage,
    caller: Caller,
    lane: store::Lane,
    episode: Episode,
    pending: Option<tokio::task::JoinHandle<Result<()>>>,
    may_submit: bool,
    unknown: bool,
}
impl SqliteStorage {
    /// Start a producer-owned logical operation. `None` is daemon-owned work
    /// without withdrawal. A supplied deadline is never renewed on resubmission.
    pub fn retained_write(
        &self,
        caller: Caller,
        lane: store::Lane,
        deadline: Option<Instant>,
    ) -> RetainedWrite {
        RetainedWrite {
            storage: self.clone(),
            caller,
            lane,
            episode: Episode::new(self.owner.shared.state.clone(), deadline),
            pending: None,
            may_submit: true,
            unknown: false,
        }
    }
}
impl RetainedWrite {
    /// Submit a fresh FnOnce. The producer may rebuild only after a confirmed
    /// noncommit; the writer never replays a callback it has already consumed.
    pub async fn attempt(&mut self, work: WriteWork) -> store::Result<WriteAttempt> {
        self.attempt_unit(Box::new(move |unit| work(unit))).await
    }

    pub(in crate::sqlite) async fn attempt_unit(
        &mut self,
        work: UnitWork,
    ) -> store::Result<WriteAttempt> {
        if !self.may_submit || self.pending.is_some() {
            return Err(backend(
                "write requires its prior receipt or outcome reconciliation",
            ));
        }
        self.may_submit = false;
        port(self.storage.owner.shared.available())?;
        let shared = self.storage.owner.shared.clone();
        let caller = self.caller.clone();
        let lane = match self.lane {
            store::Lane::Claims => super::super::Lane::Claims,
            store::Lane::Entities => super::super::Lane::Entities,
            store::Lane::Events => super::super::Lane::Events,
            store::Lane::Maintenance => super::super::Lane::Maintenance,
        };
        let receipt = match self.storage.owner.writer.submit_episode(
            lane,
            self.episode.clone(),
            move |tx| {
                let mut unit = Unit::new(tx, &caller, &shared);
                // A3 contains only arbitrary caller work, never adapter setup or completion.
                super::super::writer::contain::contain(&shared.state, || {
                    work(&mut unit).map_err(SqliteError::Store)
                })?;
                unit.ensure_committable()
            },
            || {},
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                // Admission refused before invoking the new callback. Retain
                // daemon-owned input and its original episode on queue pressure.
                self.may_submit = matches!(error, SqliteError::Overloaded)
                    && self.episode.deadline.is_none_or(|d| Instant::now() < d);
                if !self.may_submit {
                    self.episode.finish();
                }
                return port::<WriteAttempt>(Err(error));
            }
        };
        self.pending = Some(tokio::task::spawn_blocking(move || receipt.wait()));
        self.receive().await
    }
    pub(in crate::sqlite) async fn reconcile_encoded(
        &mut self,
        expected: Arc<Vec<super::super::events::producer::EncodedExpected>>,
    ) -> store::Result<bool> {
        self.reconcile_snapshot(move |snapshot| async move {
            snapshot.call(move |view| {
                let (mut wanted, mut found) = (0, 0);
                for step in expected.iter() {
                    let rows: std::collections::BTreeMap<_, _> = step.events[step.from..step.to]
                        .iter().rev().map(|event| (event.seq, event)).collect();
                    for event in rows.values() {
                        wanted += 1;
                        match view.router.matches_encoded_event(&step.run, event)? {
                            Some(true) => found += 1,
                            None => {},
                            Some(false) => return Err(SqliteError::Store(backend(
                                "reconciliation found a stored event that differs from the expected one"))),
                        }
                    }
                }
                match found {
                    _ if wanted == 0 => Ok(true),
                    0 => Ok(false),
                    n if n == wanted => Ok(true),
                    _ => Err(SqliteError::Store(backend("reconciliation found only some rows of an atomic transaction"))),
                }
            }).await
        }).await
    }

    /// Recover the single original receipt after cancellation of `attempt`'s
    /// future. A new submission cannot race an admitted or running callback.
    pub async fn receive(&mut self) -> store::Result<WriteAttempt> {
        let pending = self
            .pending
            .as_mut()
            .ok_or_else(|| backend("no pending write receipt"))?;
        let result = pending
            .await
            .map_err(|_| backend("SQLite receipt worker failed"))?;
        self.pending.take();
        if let Some(payload) = result
            .as_ref()
            .err()
            .and_then(SqliteError::callback_panic)
            .and_then(|p| p.take())
        {
            self.episode.finish();
            std::panic::resume_unwind(payload);
        }
        let (outcome, retry_code, accounting) = self.episode.snapshot();
        let terminal = matches!(
            &result,
            Err(SqliteError::Overloaded
                | SqliteError::WriterClosed
                | SqliteError::StorageUnavailable(_))
        );
        let rebuild = !terminal
            && result.is_err()
            && retry_code.is_some()
            && matches!(outcome, WriteOutcome::RolledBack | WriteOutcome::NotStarted);
        let result = match result {
            Ok(()) => Ok(()),
            Err(error) => public_result(&self.storage.owner.shared.state, port(Err(error))),
        };
        let attempt = WriteAttempt {
            result,
            outcome,
            retry_code,
            accounting,
            rebuild,
        };
        self.may_submit = attempt.retryable();
        self.unknown = outcome == WriteOutcome::Unknown;
        if !self.may_submit && !self.unknown {
            self.episode.finish();
        }
        Ok(attempt)
    }
    /// Resolve an uncertain COMMIT using a new snapshot with the same bound
    /// authority. The producer must check its durable operation identity (for
    /// events, the complete expected sequence), never just connection state.
    /// `true` means committed; `false` is authoritative absence and permits a
    /// rebuild only if the original failure was retryable. A failed or cancelled
    /// reconciliation leaves the operation blocked.
    pub async fn reconcile<F, Fut>(&mut self, check: F) -> store::Result<bool>
    where
        F: FnOnce(Box<dyn ReadSnapshot>) -> Fut,
        Fut: Future<Output = store::Result<bool>>,
    {
        self.reconcile_snapshot(move |snapshot| check(Box::new(snapshot)))
            .await
    }

    pub(super) async fn reconcile_snapshot<F, Fut>(&mut self, check: F) -> store::Result<bool>
    where
        F: FnOnce(Snapshot) -> Fut,
        Fut: Future<Output = store::Result<bool>>,
    {
        if self.pending.is_some() || !self.unknown {
            return Err(backend("write has no settled unknown outcome"));
        }
        let snapshot = self.storage.snapshot(self.caller.clone()).await?;
        let committed = check(snapshot).await?;
        self.unknown = false;
        if committed {
            self.storage.owner.shared.state.writer_committed();
        }
        self.episode.outcome(
            if committed {
                WriteOutcome::Committed
            } else {
                WriteOutcome::RolledBack
            },
            self.episode.snapshot().1,
        );
        self.may_submit = !committed && self.episode.snapshot().1.is_some();
        if !self.may_submit {
            self.episode.finish();
        }
        Ok(committed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::{FatalCause, Role};
    use orch_core::store::{Principal, StorageExt};
    use std::sync::atomic::AtomicUsize;

    fn owner() -> Caller {
        Caller {
            project: "synthetic".into(),
            principal: Principal::Owner {
                identity: "test".into(),
            },
        }
    }
    fn team(id: &str) -> Entity {
        Entity::Team(Box::new(serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "status": "active", "members": [],
            "task_pool": [], "lead_agent_id": "a", "created_at": "opaque", "updated_at": "opaque",
            "config": {"auto_claim": false}
        })).unwrap()))
    }
    fn sql(code: i32) -> SqliteError {
        SqliteError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ))
    }
    #[tokio::test]
    async fn native_observation_survives_port_conversion_and_spans_reconstructed_callbacks() {
        let (_root, data) = crate::sqlite::test_data();
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                write_observation_probe: Some(Arc::new(|| Some(sql(10)))),
                ..Default::default()
            },
        )
        .unwrap();
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let first = write
            .attempt(Box::new(move |unit| {
                count.fetch_add(1, Ordering::SeqCst);
                unit.create(team("partial")).map(|_| ())
            }))
            .await
            .unwrap();
        assert_eq!(first.outcome, WriteOutcome::RolledBack);
        assert_eq!(first.retry_code, Some(RetryCode::Io));
        assert!(first.retryable());
        assert!(storage.failure_state().claims_suppressed());
        let snapshot = storage.read(owner()).await.unwrap();
        assert!(
            snapshot
                .get(EntityKind::Team, "partial")
                .await
                .unwrap()
                .is_none()
        );
        drop(snapshot);
        let count = calls.clone();
        let second = write
            .attempt(Box::new(move |unit| {
                count.fetch_add(1, Ordering::SeqCst);
                unit.create(team("partial")).map(|_| ())
            }))
            .await
            .unwrap();
        assert!(!second.retryable());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "each newly built FnOnce runs exactly once"
        );
        assert_eq!(
            storage.failure_state().failure().unwrap().causes,
            vec![(Role::Operation, FatalCause::ConsecutiveIoErrors)]
        );
        drop(write);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn unknown_real_commit_requires_same_caller_snapshot_reconciliation() {
        let (_root, data) = crate::sqlite::test_data();
        let mut once = true;
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_commit_probe: Some(Box::new(move |tx| {
                    if !once {
                        return None;
                    }
                    once = false;
                    tx.execute_batch("COMMIT").unwrap();
                    Some(Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(10),
                        None,
                    )))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let first = write
            .attempt(Box::new(|unit| unit.create(team("durable")).map(|_| ())))
            .await
            .unwrap();
        assert_eq!(first.outcome, WriteOutcome::Unknown);
        assert!(!first.retryable());
        assert!(
            write
                .attempt(Box::new(|_| panic!("unknown may not replay")))
                .await
                .is_err()
        );
        assert!(
            write
                .reconcile(|snapshot| async move {
                    assert_eq!(snapshot.caller(), &owner());
                    Ok(snapshot.get(EntityKind::Team, "durable").await?.is_some())
                })
                .await
                .unwrap()
        );
        assert!(!storage.failure_state().claims_suppressed());
        assert!(
            write
                .attempt(Box::new(|_| panic!("known commit may not replay")))
                .await
                .is_err()
        );
        drop(write);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_await_recovers_one_receipt_without_replaying_the_callback() {
        let (_root, data) = crate::sqlite::test_data();
        let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let (entered, running) = oneshot::channel();
        let (release, blocked) = mpsc::sync_channel(1);
        let mut attempt = Box::pin(write.attempt(Box::new(move |unit| {
            let _ = entered.send(());
            blocked.recv().unwrap();
            unit.create(team("once")).map(|_| ())
        })));
        assert!(futures::poll!(&mut attempt).is_pending());
        running.await.unwrap();
        drop(attempt);
        assert!(
            write
                .attempt(Box::new(|_| panic!("pending may not replay")))
                .await
                .is_err()
        );
        release.send(()).unwrap();
        let answer = write.receive().await.unwrap();
        assert!(answer.result.is_ok());
        assert_eq!(answer.outcome, WriteOutcome::Committed);
        assert!(write.receive().await.is_err());
        assert!(
            storage
                .read(owner())
                .await
                .unwrap()
                .get(EntityKind::Team, "once")
                .await
                .unwrap()
                .is_some()
        );
        drop(write);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn no_new_deadline_or_callback_after_one_terminal_withdrawal() {
        let (_root, data) = crate::sqlite::test_data();
        let storage = SqliteStorage::open(Arc::new(data), StorageOptions::default()).unwrap();
        let mut write = storage.retained_write(owner(), store::Lane::Events, Some(Instant::now()));
        let first = write
            .attempt(Box::new(|_| panic!("expired callback")))
            .await
            .unwrap();
        assert!(
            first
                .result
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("overloaded")
        );
        assert!(!first.retryable());
        assert!(
            write
                .attempt(Box::new(|_| panic!("withdrawn callback")))
                .await
                .is_err()
        );
        drop(write);
        storage
            .transaction(owner(), store::Lane::Claims, |_| Ok(()))
            .await
            .unwrap();
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn retained_rebuild_uses_the_running_slot_before_a_full_queue_of_claims() {
        let (_root, data) = crate::sqlite::test_data();
        let mut first_commit = true;
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_capacity: 1,
                writer_commit_probe: Some(Box::new(move |_| {
                    if !first_commit {
                        return None;
                    }
                    first_commit = false;
                    Some(Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(13),
                        None,
                    )))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let mut retained = storage.retained_write(owner(), store::Lane::Events, None);
        let (entered, running) = oneshot::channel();
        let (release, blocked) = mpsc::sync_channel(1);
        let mut first = Box::pin(retained.attempt(Box::new(move |unit| {
            let _ = entered.send(());
            blocked.recv().unwrap();
            unit.create(team("batch")).map(|_| ())
        })));
        assert!(futures::poll!(&mut first).is_pending());
        running.await.unwrap();
        let claimed = Arc::new(AtomicBool::new(false));
        let seen = claimed.clone();
        let mut claim = storage.retained_write(owner(), store::Lane::Claims, None);
        let mut claim_answer = Box::pin(claim.attempt(Box::new(move |unit| {
            seen.store(true, Ordering::SeqCst);
            assert!(
                unit.get(EntityKind::Team, "batch")?.is_some(),
                "claim must wait for known event commit"
            );
            unit.create(team("claim")).map(|_| ())
        })));
        assert!(futures::poll!(&mut claim_answer).is_pending());
        release.send(()).unwrap();
        let answer = first.await.unwrap();
        assert!(answer.retryable());
        assert!(!claimed.load(Ordering::SeqCst));
        let resumed = retained
            .attempt(Box::new(|unit| unit.create(team("batch")).map(|_| ())))
            .await
            .unwrap();
        assert_eq!(resumed.outcome, WriteOutcome::Committed);
        assert!(resumed.result.is_ok());
        assert!(claim_answer.await.unwrap().result.is_ok());
        assert!(claimed.load(Ordering::SeqCst));
        drop(claim);
        drop(retained);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn every_nonclaim_lane_waits_for_retirement_and_keeps_its_original_deadline() {
        for lane in [
            store::Lane::Entities,
            store::Lane::Events,
            store::Lane::Maintenance,
        ] {
            for expired in [false, true] {
                let (_root, data) = crate::sqlite::test_data();
                let mut first = true;
                let storage = SqliteStorage::open(
                    Arc::new(data),
                    StorageOptions {
                        writer_commit_probe: Some(Box::new(move |_| {
                            let fail = first;
                            first = false;
                            fail.then(|| {
                                Err(rusqlite::Error::SqliteFailure(
                                    rusqlite::ffi::Error::new(13),
                                    None,
                                ))
                            })
                        })),
                        ..Default::default()
                    },
                )
                .unwrap();
                let mut retained = storage.retained_write(owner(), store::Lane::Events, None);
                let answer = retained
                    .attempt(Box::new(|unit| {
                        unit.create(team("rolled-back")).map(|_| ())
                    }))
                    .await
                    .unwrap();
                assert!(answer.retryable());
                assert_eq!(answer.outcome, WriteOutcome::RolledBack);
                let state = storage.failure_state();
                assert!(state.claims_suppressed());

                // An already-passed deadline is admitted into the real queue:
                // it is the worker, after retirement, that must withdraw it.
                let deadline = expired.then(|| Instant::now() - Duration::from_secs(1));
                let mut queued = storage.retained_write(owner(), lane, deadline);
                let calls = Arc::new(AtomicUsize::new(0));
                let seen = calls.clone();
                let observed = state.clone();
                let mut waiting = Box::pin(queued.attempt(Box::new(move |unit| {
                    seen.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        !observed.claims_suppressed(),
                        "{lane:?} overtook a retained retry"
                    );
                    unit.create(team("queued")).map(|_| ())
                })));
                assert!(futures::poll!(&mut waiting).is_pending());
                let writer = storage.owner.writer.clone();
                tokio::task::spawn_blocking(move || writer.wait_for_park_test())
                    .await
                    .unwrap();
                assert!(
                    futures::poll!(&mut waiting).is_pending(),
                    "{lane:?} answered before retirement"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert!(state.claims_suppressed());

                // Dropping the known-noncommit producer retires its episode.
                // No retry or new deadline is granted to the queued operation.
                drop(retained);
                let answer = tokio::time::timeout(Duration::from_secs(5), &mut waiting)
                    .await
                    .expect("retirement did not release queued work")
                    .unwrap();
                drop(waiting);
                if expired {
                    assert_eq!(answer.outcome, WriteOutcome::NotStarted);
                    assert!(!answer.retryable());
                    assert!(
                        answer
                            .result
                            .unwrap_err()
                            .to_string()
                            .contains("overloaded")
                    );
                    assert_eq!(calls.load(Ordering::SeqCst), 0);
                    assert!(queued.receive().await.is_err(), "only one terminal receipt");
                    assert!(
                        queued
                            .attempt(Box::new(|_| panic!("expired callback replayed")))
                            .await
                            .is_err()
                    );
                } else {
                    assert_eq!(answer.outcome, WriteOutcome::Committed);
                    assert!(answer.result.is_ok());
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                }
                assert!(!state.claims_suppressed());
                assert!(state.failure().is_none());
                storage
                    .transaction(owner(), store::Lane::Maintenance, |unit| {
                        unit.create(team("after"))
                    })
                    .await
                    .unwrap();
                let snapshot = storage.read(owner()).await.unwrap();
                assert!(
                    snapshot
                        .get(EntityKind::Team, "rolled-back")
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(
                    snapshot
                        .get(EntityKind::Team, "queued")
                        .await
                        .unwrap()
                        .is_some(),
                    !expired
                );
                assert!(
                    snapshot
                        .get(EntityKind::Team, "after")
                        .await
                        .unwrap()
                        .is_some()
                );
                drop(snapshot);
                drop(queued);
                storage.close().await.unwrap();
            }
        }
    }
    #[tokio::test]
    async fn caught_clean_leaf_refusal_may_commit_unrelated_work_despite_retry_class() {
        let (_root, data) = crate::sqlite::test_data();
        let inject = Arc::new(AtomicBool::new(false));
        let injected = inject.clone();
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                write_observation_probe: Some(Arc::new(move || {
                    injected.swap(false, Ordering::SeqCst).then(|| sql(13))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        storage
            .transaction(owner(), store::Lane::Events, |unit| {
                unit.create(team("existing"))
            })
            .await
            .unwrap();
        inject.store(true, Ordering::SeqCst);
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let answer = write
            .attempt(Box::new(|unit| {
                // The real duplicate operation is cleanly refused/rolled back by
                // its leaf; inject FULL only at Unit's source-observation seam.
                assert!(unit.create(team("existing")).is_err());
                unit.create(team("unrelated")).map(|_| ())
            }))
            .await
            .unwrap();
        assert!(
            answer.result.is_ok(),
            "a caught nonfatal clean leaf refusal is not callback-wide abort"
        );
        assert_eq!(answer.outcome, WriteOutcome::Committed);
        assert!(!storage.failure_state().claims_suppressed());
        assert!(
            storage
                .read(owner())
                .await
                .unwrap()
                .get(EntityKind::Team, "unrelated")
                .await
                .unwrap()
                .is_some()
        );
        drop(write);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn a_callback_logical_error_is_not_retried_for_an_earlier_caught_storage_error() {
        let (_root, data) = crate::sqlite::test_data();
        let inject = Arc::new(AtomicBool::new(false));
        let injected = inject.clone();
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                write_observation_probe: Some(Arc::new(move || {
                    injected.swap(false, Ordering::SeqCst).then(|| sql(13))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        storage
            .transaction(owner(), store::Lane::Events, |unit| {
                unit.create(team("existing"))
            })
            .await
            .unwrap();
        inject.store(true, Ordering::SeqCst);
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let answer = write
            .attempt(Box::new(|unit| {
                assert!(unit.create(team("existing")).is_err());
                Err(StoreError::InvalidEvents(
                    "synthetic logical refusal".into(),
                ))
            }))
            .await
            .unwrap();
        assert!(matches!(&answer.result, Err(StoreError::InvalidEvents(_))));
        assert!(
            !answer.retryable(),
            "a caught earlier backend result cannot reclassify a logical callback refusal"
        );
        assert!(!storage.failure_state().claims_suppressed());
        drop(write);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn reconciled_commit_clears_shared_ioerr_streak_before_the_next_request() {
        let (_root, data) = crate::sqlite::test_data();
        let mut commits = 0;
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_commit_probe: Some(Box::new(move |tx| {
                    commits += 1;
                    if commits == 1 {
                        tx.execute_batch("COMMIT").unwrap();
                    }
                    Some(Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(10),
                        None,
                    )))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut first = storage.retained_write(owner(), store::Lane::Events, None);
        let answer = first
            .attempt(Box::new(move |unit| {
                count.fetch_add(1, Ordering::SeqCst);
                unit.create(team("reconciled")).map(|_| ())
            }))
            .await
            .unwrap();
        assert_eq!(answer.outcome, WriteOutcome::Unknown);
        assert!(!answer.retryable());
        assert!(first.receive().await.is_err(), "one terminal receipt");
        assert!(
            first
                .reconcile(|snapshot| async move {
                    Ok(snapshot
                        .get(EntityKind::Team, "reconciled")
                        .await?
                        .is_some())
                })
                .await
                .unwrap()
        );
        assert_eq!(first.episode.snapshot().0, WriteOutcome::Committed);
        assert!(!storage.failure_state().claims_suppressed());
        assert!(
            first
                .attempt(Box::new(|_| panic!("known commit replayed")))
                .await
                .is_err()
        );
        let mut second = storage.retained_write(owner(), store::Lane::Entities, None);
        let count = calls.clone();
        let answer = second
            .attempt(Box::new(move |unit| {
                count.fetch_add(1, Ordering::SeqCst);
                unit.create(team("rolled-back")).map(|_| ())
            }))
            .await
            .unwrap();
        assert_eq!(answer.outcome, WriteOutcome::RolledBack);
        assert_eq!(answer.retry_code, Some(RetryCode::Io));
        assert!(
            answer.retryable(),
            "reconciliation must end the prior IOERR streak"
        );
        assert!(storage.failure_state().failure().is_none());
        assert!(second.receive().await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            storage
                .read(owner())
                .await
                .unwrap()
                .get(EntityKind::Team, "rolled-back")
                .await
                .unwrap()
                .is_none()
        );
        drop(second);
        drop(first);
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn authoritative_absence_releases_an_unknown_token_after_its_original_deadline() {
        let (_root, data) = crate::sqlite::test_data();
        let mut once = true;
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_commit_probe: Some(Box::new(move |tx| {
                    if !once {
                        return None;
                    }
                    once = false;
                    // The empty transaction really commits, but its acknowledgement
                    // is lost. The durable operation identity is authoritatively absent.
                    tx.execute_batch("COMMIT").unwrap();
                    Some(Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(10),
                        None,
                    )))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let deadline = Instant::now() - Duration::from_secs(1);
        let mut write = storage.retained_write(owner(), store::Lane::Events, Some(deadline));
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let receipt = storage.owner.writer.submit_at_test_time(
            write.episode.clone(),
            deadline - Duration::from_secs(1),
            move |tx| {
                count.fetch_add(1, Ordering::SeqCst);
                assert!(!tx.is_autocommit());
                Ok(())
            },
        );
        // Seed only admission with a synthetic clock; consume the actual worker
        // receipt through the retained Storage boundary, exactly as attempt does.
        write.may_submit = false;
        write.pending = Some(tokio::task::spawn_blocking(move || receipt.wait()));
        let answer = write.receive().await.unwrap();
        assert_eq!(answer.outcome, WriteOutcome::Unknown);
        assert!(!answer.retryable());
        assert!(write.receive().await.is_err(), "one terminal receipt");
        assert!(
            write
                .attempt(Box::new(|_| panic!("unknown replayed")))
                .await
                .is_err()
        );

        // Queue a sentinel, then use a second fixture continuation as a worker
        // barrier. run() has necessarily checked expiry after the unknown answer
        // and before this barrier; it must keep the ordinary queue fenced.
        let ran = Arc::new(AtomicBool::new(false));
        let seen = ran.clone();
        let mut queued = storage.retained_write(owner(), store::Lane::Maintenance, None);
        let mut waiting = Box::pin(queued.attempt(Box::new(move |unit| {
            seen.store(true, Ordering::SeqCst);
            unit.create(team("after-absence")).map(|_| ())
        })));
        assert!(futures::poll!(&mut waiting).is_pending());
        let barrier = storage.owner.writer.submit_at_test_time(
            Episode::new(storage.owner.shared.state.clone(), None),
            Instant::now(),
            |_| {
                Err::<(), _>(SqliteError::Store(StoreError::InvalidEvents(
                    "barrier".into(),
                )))
            },
        );
        assert!(
            tokio::task::spawn_blocking(move || barrier.wait())
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            storage.failure_state().claims_suppressed(),
            "unknown remains fenced past deadline"
        );
        assert!(!ran.load(Ordering::SeqCst));
        assert!(
            !write
                .reconcile(|snapshot| async move {
                    Ok(snapshot
                        .get(EntityKind::Team, "absent-operation")
                        .await?
                        .is_some())
                })
                .await
                .unwrap()
        );
        assert_eq!(write.episode.snapshot().0, WriteOutcome::RolledBack);
        // No producer resubmission or Drop: the idle worker must now observe
        // reconcile(false)'s phase and retire this live expired registration.
        assert!(
            tokio::time::timeout(Duration::from_secs(5), &mut waiting)
                .await
                .expect("worker did not retire reconciled token")
                .unwrap()
                .result
                .is_ok()
        );
        drop(waiting);
        assert!(!storage.failure_state().claims_suppressed());
        let answer = write
            .attempt(Box::new(|_| panic!("original deadline renewed")))
            .await
            .unwrap();
        assert_eq!(answer.outcome, WriteOutcome::NotStarted);
        assert!(!answer.retryable());
        assert!(answer.result.is_err());
        assert!(write.receive().await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(write);
        drop(queued);
        assert!(
            !storage.failure_state().claims_suppressed(),
            "Drop after retirement is idempotent"
        );
        storage.close().await.unwrap();
    }
    #[tokio::test]
    async fn dropping_a_live_unknown_token_releases_queued_work_without_replay() {
        let (_root, data) = crate::sqlite::test_data();
        let mut once = true;
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_commit_probe: Some(Box::new(move |tx| {
                    if !once {
                        return None;
                    }
                    once = false;
                    tx.execute_batch("COMMIT").unwrap();
                    Some(Err(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(10),
                        None,
                    )))
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut write = storage.retained_write(owner(), store::Lane::Events, None);
        let answer = write
            .attempt(Box::new(move |unit| {
                count.fetch_add(1, Ordering::SeqCst);
                unit.create(team("drop-control")).map(|_| ())
            }))
            .await
            .unwrap();
        assert_eq!(answer.outcome, WriteOutcome::Unknown);
        assert!(write.receive().await.is_err());
        assert!(storage.failure_state().claims_suppressed());
        let mut queued = storage.retained_write(owner(), store::Lane::Maintenance, None);
        let mut waiting =
            Box::pin(queued.attempt(Box::new(|unit| unit.create(team("after-drop")).map(|_| ()))));
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(write);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), &mut waiting)
                .await
                .expect("Drop did not release worker")
                .unwrap()
                .result
                .is_ok()
        );
        drop(waiting);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!storage.failure_state().claims_suppressed());
        drop(queued);
        storage.close().await.unwrap();
    }
}
