use super::tests::{owner, scenario, submit};
use super::*;
use crate::sqlite::{ReadPool, Synchronous};
use orch_core::store::attention::{AttentionFacts, AttentionFilter, ResolutionReason};
use orch_core::store::events::SequencedEvent;
use orch_core::store::intents::{DONE_RETENTION_MS, FinishOutcome, IntentPayload};
use orch_core::store::leases::{ClaimOutcome, LEASE_MS, NewLease};
use orch_core::store::request_keys::{KEY_TTL_MS, KeyCheck, RequestScope};
use orch_core::store::retries::Retry;
use std::time::{Duration, Instant};

fn event(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: orch_domain::RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: orch_domain::RunEventType::AgentOutput,
            data: serde_json::json!({"seq":seq}),
        },
    }
}

#[test]
fn operational_writes_share_transaction_and_caller_with_snapshot_reads() {
    scenario(|data, handle, disk| {
        let state = handle.failure_state();
        let blobs = disk.clone();
        let (attention, intent, lease) = submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            let attention = unit.open_attention(
                &AttentionFacts::DispatchOffWithWork {
                    waiting_tasks: 2,
                    oldest_waiting_at: 1,
                },
                10,
            )?;
            assert_eq!(unit.get_attention(attention.id)?, Some(attention.clone()));
            assert_eq!(
                unit.attention_page(AttentionFilter::Open, None, 1)?.items,
                vec![attention.clone()]
            );
            let ack = unit.ack_attention(attention.id, Revision::ONE, 11)?;
            assert_eq!(ack.acknowledgement.as_ref().unwrap().identity, "synthetic");
            let intent =
                unit.add_intent(&IntentPayload::WorkspaceRemove { task: "t".into() }, -777)?;
            assert_eq!(unit.get_intent(intent.id)?, Some(intent.clone()));
            assert_eq!(unit.list_pending_intents(0, 1)?, vec![intent.clone()]);
            let ClaimOutcome::Granted(lease) = unit.claim_lease(
                &NewLease {
                    task: "t".into(),
                    agent: "a".into(),
                    run: "r".into(),
                },
                -10,
            )?
            else {
                panic!("grant")
            };
            assert_eq!(lease.expires_at, LEASE_MS - 10);
            assert_eq!(unit.get_lease("t")?, Some(lease.clone()));
            assert_eq!(unit.get_agent_lease("a")?, Some(lease.clone()));
            assert_eq!(unit.list_leases()?, vec![lease.clone()]);
            unit.append_events("r", &lease.lineage, lease.epoch, &[event(1), event(2)])?;
            unit.record_event_gap("r", 2, 3)?;
            assert_eq!(unit.read_events("r", 0, 10)?, vec![event(1), event(2)]);
            assert_eq!(unit.read_events_tail("r", 1)?, vec![event(2)]);
            assert_eq!(unit.read_event_gaps("r")?.len(), 1);
            let retry = Retry {
                task: "t".into(),
                attempt: 1,
                due_at: 20,
                error: "x".repeat(70_000),
            };
            unit.put_retry(&retry, -777)?;
            assert_eq!(unit.get_retry("t")?, Some(retry.clone()));
            assert!(unit.due_retries(19, 1)?.is_empty());
            assert_eq!(unit.due_retries(20, 1)?, vec![retry.clone()]);
            assert_eq!(unit.list_retries(None, 1)?.rows, vec![retry]);
            let scope = RequestScope::of(&caller, "0123456789abcdef")?;
            unit.store_request_key(&scope, "task.update", &"a".repeat(64), "result", 0)?;
            assert_eq!(
                unit.check_request_key(&scope, "task.update", &"a".repeat(64), 1)?,
                KeyCheck::Replay("result".into())
            );
            unit.ensure_committable()?;
            Ok((attention, intent, lease))
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            assert!(
                read.get_attention(attention.id)?
                    .unwrap()
                    .acknowledgement
                    .is_some()
            );
            assert_eq!(
                read.attention_page(AttentionFilter::All, None, 1)?
                    .items
                    .len(),
                1
            );
            assert_eq!(read.get_intent(intent.id)?, Some(intent.clone()));
            assert_eq!(read.list_pending_intents(0, 1)?, vec![intent.clone()]);
            assert_eq!(read.get_lease("t")?, Some(lease.clone()));
            assert_eq!(read.get_agent_lease("a")?, Some(lease.clone()));
            assert_eq!(read.list_leases()?, vec![lease.clone()]);
            assert_eq!(read.read_events("r", 0, 10)?, vec![event(1), event(2)]);
            assert_eq!(read.read_events_tail("r", 1)?, vec![event(2)]);
            assert_eq!(read.read_event_gaps("r")?.len(), 1);
            assert_eq!(read.get_retry("t")?.unwrap().error.len(), 70_000);
            assert_eq!(read.due_retries(20, 1)?.len(), 1);
            assert_eq!(read.list_retries(None, 1)?.rows.len(), 1);
            let other = owner("other");
            let read = EntityRead::reader(tx, &other, disk, pool.failure_state().clone());
            assert!(read.get_attention(attention.id)?.is_none());
            assert!(read.get_intent(intent.id)?.is_none());
            assert!(read.list_pending_intents(0, 1)?.is_empty());
            assert!(read.get_lease("t")?.is_none());
            assert!(read.get_agent_lease("a")?.is_none());
            assert!(read.list_leases()?.is_empty());
            assert!(read.read_events("r", 0, 10)?.is_empty());
            assert!(read.read_events_tail("r", 1)?.is_empty());
            assert!(read.read_event_gaps("r")?.is_empty());
            assert!(read.get_retry("t")?.is_none());
            assert!(read.due_retries(20, 1)?.is_empty());
            assert!(read.list_retries(None, 1)?.rows.is_empty());
            Ok(())
        })
        .unwrap();
        let state = handle.failure_state();
        let blobs = disk.clone();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            let current = unit.get_attention(attention.id)?.unwrap();
            unit.resolve_attention(
                attention.id,
                current.revision,
                ResolutionReason::DispatchEnabled,
                12,
            )?;
            assert_eq!(unit.expire_attention(13, 1)?, 1);
            assert_eq!(
                unit.finish_intent(intent.id, -700)?,
                FinishOutcome::Finished
            );
            assert_eq!(unit.delete_finished_intents(DONE_RETENTION_MS - 700, 1)?, 1);
            assert_eq!(
                unit.renew_lease("t", &lease.lineage, lease.epoch, 40)?
                    .expires_at,
                LEASE_MS + 40
            );
            assert!(matches!(
                unit.release_lease("t", "wrong", lease.epoch),
                Err(SqliteError::Store(orch_core::store::StoreError::StaleEpoch))
            ));
            unit.release_lease("t", &lease.lineage, lease.epoch)?;
            assert_eq!(unit.prune_events("r", 1)?.deleted, 1);
            assert!(unit.delete_retry("t")?);
            assert_eq!(unit.delete_expired_request_keys(KEY_TTL_MS, 1)?, 1);
            unit.ensure_committable()
        })
        .unwrap();
    });
}

#[test]
fn caught_partial_event_failure_poison_survives_rebuilt_views_and_next_job_recovers() {
    for operation in ["append", "gap", "prune"] {
        scenario(|data, handle, disk| {
            let state = handle.failure_state();
            let blobs = disk.clone();
            let result = submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                unit.create(super::tests::payload(Family::Task, "prior", "rollback"))?;
                let ClaimOutcome::Granted(lease) = unit.claim_lease(
                    &NewLease {
                        task: "t".into(),
                        agent: "a".into(),
                        run: "r".into(),
                    },
                    0,
                )?
                else {
                    panic!("grant")
                };
                let error = match operation {
                    "append" => {
                        tx.execute_batch("CREATE TEMP TRIGGER fail_event BEFORE INSERT ON run_events
                            WHEN NEW.seq=2 BEGIN SELECT RAISE(ABORT,'private synthetic detail'); END")?;
                        unit.append_events("r", &lease.lineage, lease.epoch, &[event(1), event(2)])
                            .unwrap_err()
                    }
                    "gap" => {
                        tx.execute_batch(
                            "CREATE TEMP TRIGGER fail_gap BEFORE INSERT ON run_event_gaps
                            BEGIN SELECT RAISE(ABORT,'private synthetic detail'); END",
                        )?;
                        unit.record_event_gap("r", 0, 1).unwrap_err()
                    }
                    "prune" => {
                        unit.append_events("r", &lease.lineage, lease.epoch, &[event(1)])?;
                        tx.execute_batch(
                            "CREATE TEMP TRIGGER fail_prune BEFORE DELETE ON run_events
                            BEGIN SELECT RAISE(ABORT,'private synthetic detail'); END",
                        )?;
                        unit.prune_events("r", 1).unwrap_err()
                    }
                    _ => unreachable!(),
                };
                assert!(matches!(error, SqliteError::Sql(_)), "{error:?}");
                assert!(
                    !tx.is_autocommit(),
                    "failed statement leaves a live callback transaction"
                );
                assert!(
                    state.failure().is_none(),
                    "ordinary SQL failure never latches"
                );
                if operation == "append" {
                    let partial: i64 =
                        tx.query_row("SELECT count(*) FROM run_events", [], |r| r.get(0))?;
                    assert_eq!(partial, 1, "fixture must exercise partial append");
                }
                drop(unit);
                let mut rebuilt = EntityWrite::new(tx, &caller, &blobs, state, 0);
                assert_operational_refused(&mut rebuilt);
                assert!(
                    matches!(
                        rebuilt.get_attention(1),
                        Err(SqliteError::TransactionAborted)
                    ),
                    "event backend error must poison even a reconstructed view"
                );
                assert!(matches!(
                    rebuilt.ensure_committable(),
                    Err(SqliteError::TransactionAborted)
                ));
                // Catch the result and return success to exercise the real executor.
                Ok(())
            });
            assert!(
                matches!(result, Err(SqliteError::TransactionAborted)),
                "{operation}: {result:?}"
            );
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                    .unwrap();
            pool.read(Instant::now()+Duration::from_secs(10), |tx| {
                let counts: (i64,i64,i64) = tx.query_row("SELECT (SELECT count(*) FROM tasks), (SELECT count(*) FROM run_events), (SELECT count(*) FROM leases)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
                assert_eq!(counts, (0,0,0));
                Ok(())
            }).unwrap();
            let state = handle.failure_state();
            let blobs = disk.clone();
            submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                assert!(matches!(
                    unit.append_events("r", "wrong", 1, &[]),
                    Err(SqliteError::Store(orch_core::store::StoreError::StaleEpoch))
                ));
                unit.create(super::tests::payload(Family::Task, "next", "commits"))?;
                unit.ensure_committable()
            })
            .unwrap();
        });
    }
}

#[test]
fn message_cleanup_unwinds_actual_writer_and_drop_discards_connection_and_receipts() {
    use crate::sqlite::{ConnectionRole, Lane, Writer, open_connection};
    let (_temp, data) = crate::sqlite::test_data();
    super::tests::setup(&data);
    let disk = std::sync::Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let state = handle.failure_state();
    let blobs = disk.clone();
    let failed = handle
        .submit::<(), _>(Lane::Entities, None, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            unit.create(super::tests::payload(Family::Task, "prior", "rollback"))?;
            tx.execute_batch(
                "CREATE TEMP TRIGGER fail_message BEFORE INSERT ON messages
            BEGIN SELECT RAISE(ROLLBACK,'synthetic message cleanup'); END",
            )?;
            unit.create(super::tests::payload(Family::Message, "m", "fails"))?;
            Ok(())
        })
        .unwrap();
    let queued = handle.submit(Lane::Entities, None, |_| Ok(())).unwrap();
    // This catch is in the test, outside Writer::run. Production run invokes
    // its job directly; unwinding runs Writer::drop and disposes the connection.
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.run())).is_err());
    assert!(matches!(failed.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(queued.wait(), Err(SqliteError::WriterClosed)));
    assert!(matches!(
        handle.submit(Lane::Entities, None, |_| Ok(())),
        Err(SqliteError::WriterClosed)
    ));
    let conn = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    let counts: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT count(*) FROM tasks), (SELECT count(*) FROM messages)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0));
    drop(conn);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    let state = handle.failure_state();
    let next = handle
        .submit(Lane::Entities, None, move |tx| {
            let caller = owner("p");
            EntityWrite::new(tx, &caller, &disk, state, 0).create(super::tests::payload(
                Family::Task,
                "next",
                "commits",
            ))?;
            Ok(())
        })
        .unwrap();
    handle.close();
    writer.run();
    next.wait().unwrap();
    let conn = open_connection(&data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn common_port_boundary_preserves_refusals_sanitizes_diagnostics_and_observes_before_catch() {
    use crate::sqlite::writer::failure::{FatalCause, Role};
    use orch_core::store::StoreError;
    scenario(|data, handle, disk| {
        let state = handle.failure_state();
        let blobs = disk.clone();
        let result = submit(handle, move |tx| {
            let caller = owner("p");
            let unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
            assert!(matches!(
                unit.port::<()>(Err(StoreError::StaleEpoch.into())),
                Err(StoreError::StaleEpoch)
            ));
            assert!(state.transaction_must_abort().is_none());
            let error = unit
                .port::<()>(Err(SqliteError::Sql(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    Some("private row and path".into()),
                ))))
                .unwrap_err();
            assert!(!error.to_string().contains("private"));
            assert_eq!(state.failure().unwrap().source, FailureSource::Writer);
            assert_eq!(
                state.failure().unwrap().causes,
                [(Role::Operation, FatalCause::SqliteCorrupt)]
            );
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
        // A fresh pool below uses a separate state to exercise trusted Reader
        // attribution of NOTADB, not artifact classification or actual damage.
        let state = FailureState::new();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, state.clone());
            let error = read
                .port::<()>(Err(SqliteError::Sql(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
                    Some("private database path".into()),
                ))))
                .unwrap_err();
            assert!(!error.to_string().contains("private"));
            assert_eq!(state.failure().unwrap().source, FailureSource::Reader);
            assert_eq!(
                state.failure().unwrap().causes,
                [(Role::Operation, FatalCause::SqliteNotADatabase)]
            );
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
    });
}

#[test]
fn reader_snapshot_does_not_consult_writer_callback_abort_bit() {
    scenario(|data, handle, disk| {
        let state = handle.failure_state();
        state.abort_transaction();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, state.clone()).unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, state.clone());
            assert!(read.get_attention(1)?.is_none());
            assert!(read.get_intent(1)?.is_none());
            assert!(read.get_lease("t")?.is_none());
            assert!(read.get_retry("t")?.is_none());
            assert!(read.read_events("r", 0, 1)?.is_empty());
            Ok(())
        })
        .unwrap();
        // Writer resets only its transaction-local bit at the next job boundary.
        submit(handle, |_| Ok(())).unwrap();
    });
}

pub(super) fn assert_operational_refused(unit: &mut EntityWrite<'_, '_>) {
    let scope = RequestScope::of(unit.caller(), "0123456789abcdef").unwrap();
    let facts = AttentionFacts::DispatchOffWithWork {
        waiting_tasks: 1,
        oldest_waiting_at: 0,
    };
    let payload = IntentPayload::WorkspaceRemove { task: "t".into() };
    let retry = Retry {
        task: "t".into(),
        attempt: 1,
        due_at: 0,
        error: "".into(),
    };
    let new = NewLease {
        task: "t".into(),
        agent: "a".into(),
        run: "r".into(),
    };
    // Deliberately invalid limits/timestamps/tokens would give other answers if
    // any entry point reached its leaf after callback poison.
    for result in [
        unit.get_attention(1).map(|_| ()),
        unit.attention_page(AttentionFilter::All, None, 0)
            .map(|_| ()),
        unit.get_intent(0).map(|_| ()),
        unit.list_pending_intents(0, 0).map(|_| ()),
        unit.get_lease("t").map(|_| ()),
        unit.get_agent_lease("a").map(|_| ()),
        unit.list_leases().map(|_| ()),
        unit.read_events("r", 0, 0).map(|_| ()),
        unit.read_events_tail("r", 0).map(|_| ()),
        unit.read_event_gaps("r").map(|_| ()),
        unit.get_retry("t").map(|_| ()),
        unit.due_retries(0, 0).map(|_| ()),
        unit.list_retries(None, 0).map(|_| ()),
        unit.open_attention(&facts, -1).map(|_| ()),
        unit.ack_attention(1, Revision::ONE, -1).map(|_| ()),
        unit.resolve_attention(1, Revision::ONE, ResolutionReason::DispatchEnabled, -1)
            .map(|_| ()),
        unit.expire_attention(-1, 0).map(|_| ()),
        unit.add_intent(&payload, 0).map(|_| ()),
        unit.finish_intent(0, 0).map(|_| ()),
        unit.delete_finished_intents(0, 0).map(|_| ()),
        unit.claim_lease(&new, 0).map(|_| ()),
        unit.renew_lease("t", "wrong", 0, 0).map(|_| ()),
        unit.release_lease("t", "wrong", 0).map(|_| ()),
        unit.put_retry(&retry, 0).map(|_| ()),
        unit.delete_retry("t").map(|_| ()),
        unit.check_request_key(&scope, "", "", -1).map(|_| ()),
        unit.store_request_key(&scope, "", "", "", -1).map(|_| ()),
        unit.delete_expired_request_keys(-1, 0).map(|_| ()),
        unit.append_events("r", "wrong", 0, &[]).map(|_| ()),
        unit.record_event_gap("r", 0, 0).map(|_| ()),
        unit.prune_events("r", 0).map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(SqliteError::TransactionAborted)),
            "{result:?}"
        );
    }
}

#[test]
fn ended_snapshot_refuses_every_operational_read_before_leaf_validation() {
    scenario(|data, handle, disk| {
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        // The pool will reject an externally ended transaction; the assertions
        // inside pin refusal at each router call before that outer cleanup.
        let _ = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            tx.execute_batch("ROLLBACK")?;
            for result in [
                read.get_attention(1).map(|_| ()),
                read.attention_page(AttentionFilter::All, None, 0)
                    .map(|_| ()),
                read.get_intent(0).map(|_| ()),
                read.list_pending_intents(0, 0).map(|_| ()),
                read.get_lease("t").map(|_| ()),
                read.get_agent_lease("a").map(|_| ()),
                read.list_leases().map(|_| ()),
                read.read_events("r", 0, 0).map(|_| ()),
                read.read_events_tail("r", 0).map(|_| ()),
                read.read_event_gaps("r").map(|_| ()),
                read.get_retry("t").map(|_| ()),
                read.due_retries(0, 0).map(|_| ()),
                read.list_retries(None, 0).map(|_| ()),
            ] {
                assert!(
                    matches!(result, Err(SqliteError::TransactionAborted)),
                    "{result:?}"
                );
            }
            Ok(())
        });
    });
}

#[test]
fn attention_six_stored_row_guards_remain_typed_through_router_reads() {
    for update in [
        "UPDATE attention SET project_id=''",
        r#"UPDATE attention SET facts_json='{"kind":"dispatch_off_with_work","waiting_tasks":0,"oldest_waiting_at":1}'"#,
        "UPDATE attention SET id='01'",
        "UPDATE attention SET author_kind='agent'",
        "UPDATE attention SET acked_at=9,acked_by='owner'",
        "UPDATE attention SET resolved_at=10,resolved_reason='dispatch_enabled',resolved_by='worker'",
    ] {
        scenario(|data, handle, disk| {
            let state = handle.failure_state();
            let blobs = disk.clone();
            submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                unit.open_attention(
                    &AttentionFacts::DispatchOffWithWork {
                        waiting_tasks: 1,
                        oldest_waiting_at: 1,
                    },
                    10,
                )?;
                tx.execute_batch(update)?;
                Ok(())
            })
            .unwrap();
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                    .unwrap();
            pool.read(Instant::now() + Duration::from_secs(10), |tx| {
                let caller = owner(if update.contains("project_id") {
                    ""
                } else {
                    "p"
                });
                let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
                let page = read
                    .attention_page(AttentionFilter::All, None, 1)
                    .err()
                    .expect("malformed stored row must fail");
                assert!(
                    matches!(
                        page,
                        SqliteError::Store(orch_core::store::StoreError::InvalidAttention(_))
                    ),
                    "{update}: {page:?}"
                );
                assert!(pool.failure_state().failure().is_none());
                Ok(())
            })
            .unwrap();
        });
    }
}

#[test]
fn request_keys_keep_native_codes_before_conversion_and_exact_ttl_metadata_after_reopen() {
    use orch_core::store::StoreError;
    use orch_core::store::request_keys::MAX_RESULT_BYTES;
    scenario(|data, handle, disk| {
        for stage in 0..3 {
            let state = handle.failure_state();
            let blobs = disk.clone();
            submit(handle, move |tx| {
                let caller = owner("p");
                let scope = RequestScope::of(&caller, "0123456789abcdef")?;
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                let now = stage * KEY_TTL_MS;
                let hash = ["a", "b", "c"][stage as usize].repeat(64);
                let text = if stage == 1 {
                    "small λ".into()
                } else {
                    "x".repeat(MAX_RESULT_BYTES + 1)
                };
                assert_eq!(
                    unit.check_request_key(&scope, "task.update", &hash, now)?,
                    KeyCheck::New
                );
                unit.store_request_key(&scope, "task.update", &hash, &text, now)?;
                assert!(matches!(
                    unit.store_request_key(&scope, "task.update", &hash, "overwrite", now),
                    Err(SqliteError::Store(StoreError::InvalidRequestKey(_)))
                ));
                if stage == 2 {
                    tx.execute_batch(
                        "CREATE TEMP TRIGGER fail_key BEFORE UPDATE ON request_keys
                        BEGIN SELECT RAISE(ABORT,'private key diagnostic'); END",
                    )?;
                    let error = unit
                        .store_request_key(&scope, "task.update", &hash, "fails", now + KEY_TTL_MS)
                        .unwrap_err();
                    match &error {
                        SqliteError::Sql(rusqlite::Error::SqliteFailure(code, _)) => {
                            assert_eq!(code.code, rusqlite::ErrorCode::ConstraintViolation);
                            assert_eq!(
                                code.extended_code,
                                rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
                            );
                        }
                        other => panic!("native code was lost: {other:?}"),
                    }
                    let public = unit.port::<()>(Err(error)).unwrap_err();
                    assert!(!public.to_string().contains("private"));
                    assert!(state.failure().is_none());
                    assert!(state.transaction_must_abort().is_none());
                    unit.create(super::tests::payload(Family::Task, "unrelated", "commits"))?;
                }
                unit.ensure_committable()
            })
            .unwrap();
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                    .unwrap();
            pool.read(Instant::now()+Duration::from_secs(10), |tx| {
                let row: (String, Option<String>, i64, i64, i64) = tx.query_row(
                    "SELECT req_sha256,result,result_truncated,created_at,expires_at FROM request_keys",
                    [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
                assert_eq!(row.0, ["a", "b", "c"][stage as usize].repeat(64));
                assert_eq!(row.1, (stage==1).then(|| "small λ".into()));
                assert_eq!(row.2, i64::from(stage!=1));
                assert_eq!((row.3,row.4), (stage*KEY_TTL_MS,(stage+1)*KEY_TTL_MS));
                Ok(())
            }).unwrap();
        }
    });
}
