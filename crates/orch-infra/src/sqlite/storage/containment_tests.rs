use super::*;
use orch_core::store::{Principal, StorageExt};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};

fn owner() -> Caller {
    Caller {
        project: "p".into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn team(id: &str) -> Entity {
    Entity::Team(Box::new(serde_json::from_value(serde_json::json!({"id":id,"name":id,"status":"active","members":[],"task_pool":[],"lead_agent_id":"a","created_at":"opaque","updated_at":"opaque","config":{"auto_claim":false}})).unwrap()))
}
fn message() -> Entity {
    Entity::Message(Box::new(serde_json::from_value(serde_json::json!({"id":"m","channel":"broadcast","from_agent_id":"sender","to_agent_id":null,"subject":"s","body":"b","created_at":"opaque","status":"pending"})).unwrap()))
}

#[tokio::test(flavor = "current_thread")]
async fn typed_leaf_cleanup_ends_writer_without_becoming_a_caller_panic() {
    leaf_cleanup_case(None).await;
}
#[tokio::test(flavor = "current_thread")]
async fn retryable_leaf_cleanup_ends_writer_without_starting_a_retry_episode() {
    for code in [5, 6, 13] {
        leaf_cleanup_case(Some(code)).await;
    }
}
async fn leaf_cleanup_case(code: Option<i32>) {
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(
        data.clone(),
        StorageOptions {
            writer_capacity: 1,
            completion_probe: code.map(|code| {
                Arc::new(move || {
                    crate::sqlite::writer::contain::set_cleanup_error(code);
                }) as Arc<dyn Fn() + Send + Sync>
            }),
            writer_setup: Some(Box::new(|conn| {
                conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Savepoint {
                        savepoint_name: "orch_message",
                        operation: TransactionOperation::Release | TransactionOperation::Rollback,
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let (entered, running) = oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let first = storage.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_calls = calls.clone();
    let writing = tokio::spawn(async move {
        first
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |unit| {
                    seen_calls.fetch_add(1, Ordering::SeqCst);
                    unit.create(team("partial"))?;
                    let _ = entered.send(());
                    blocked.recv().unwrap();
                    // A structured leaf cleanup unwind must escape even if a caller
                    // would swallow a returned error and report success.
                    let _ = unit.create(message());
                    Ok(())
                }),
            )
            .await
    });
    running.await.unwrap();
    let next = storage.clone();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = invoked.clone();
    let mut queued = Box::pin(async move {
        next.write(
            owner(),
            store::Lane::Entities,
            Box::new(move |unit| {
                seen.store(true, Ordering::SeqCst);
                unit.create(team("queued")).map(|_| ())
            }),
        )
        .await
    });
    // There is no await before Writer::submit; one poll admits the successor
    // and stops at its receipt while the first callback is still blocked.
    assert!(futures::poll!(&mut queued).is_pending());
    release.send(()).unwrap();
    let first = writing.await;
    let queued = queued.await;
    let state = storage.failure_state();
    let closed = storage.close().await;
    let first = first.expect("adapter cleanup must not resume on the caller");
    assert!(
        matches!(first, Err(ref e) if e.to_string().contains("writer is closed")),
        "{first:?}"
    );
    assert!(
        matches!(queued, Err(ref e) if e.to_string().contains("writer is closed")),
        "{queued:?}"
    );
    assert!(!invoked.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        !state.claims_suppressed(),
        "leaf cleanup must never enter the retry policy"
    );
    assert!(
        storage
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(|_| panic!("closed callback"))
            )
            .await
            .is_err()
    );
    assert!(
        state.failure().is_none(),
        "ordinary cleanup failure proves no database corruption"
    );
    assert!(
        state.transaction_must_abort().is_some(),
        "the typed unwind must be observed before it leaves the callback"
    );
    assert!(closed.is_err(), "owner reports the adapter worker unwind");
    let reopened = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    let snapshot = reopened.read(owner()).await.unwrap();
    assert!(
        snapshot
            .list(&Filter::Teams, &PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        snapshot
            .get(EntityKind::Message, "m")
            .await
            .unwrap()
            .is_none()
    );
    drop(snapshot);
    reopened
        .transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(team("following"))
        })
        .await
        .unwrap();
    reopened.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_outer_rollback_keeps_the_callback_payload_for_the_caller() {
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(
        data.clone(),
        StorageOptions {
            writer_setup: Some(Box::new(|conn| {
                conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Transaction {
                        operation: TransactionOperation::Rollback,
                    } => Authorization::Deny,
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let (entered, running) = oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let first = storage.clone();
    let writing = tokio::spawn(async move {
        first
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |unit| {
                    unit.create(team("partial"))?;
                    let _ = entered.send(());
                    blocked.recv().unwrap();
                    std::panic::panic_any(String::from("original callback payload"));
                }),
            )
            .await
    });
    running.await.unwrap();
    let next = storage.clone();
    let mut queued = Box::pin(async move {
        next.transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(team("queued"))
        })
        .await
    });
    assert!(futures::poll!(&mut queued).is_pending());
    release.send(()).unwrap();
    let outcome = writing.await;
    let queued = queued.await;
    let closed = storage.close().await;
    let panic = outcome.unwrap_err().into_panic();
    assert_eq!(
        *panic.downcast::<String>().expect("original payload type"),
        "original callback payload"
    );
    assert!(
        matches!(queued, Err(ref e) if e.to_string().contains("storage_unavailable")),
        "{queued:?}"
    );
    assert_eq!(
        storage.failure_state().failure().unwrap().causes,
        vec![(
            super::super::Role::Operation,
            super::super::FatalCause::RollbackLeftTransactionOpen
        )]
    );
    assert!(closed.is_err());
    let reopened = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    assert!(
        reopened
            .read(owner())
            .await
            .unwrap()
            .list(&Filter::Teams, &PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    reopened
        .transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(team("following"))
        })
        .await
        .unwrap();
    reopened.close().await.unwrap();
}

struct SetupPanicClock;
impl Clock for SetupPanicClock {
    fn now_ms(&self) -> i64 {
        panic!("synthetic adapter setup panic")
    }
    fn now_iso(&self) -> String {
        unreachable!("setup reads milliseconds only")
    }
}
#[tokio::test]
async fn adapter_setup_panic_is_outside_the_user_callback_catch() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            clock: Arc::new(SetupPanicClock),
            ..Default::default()
        },
    )
    .unwrap();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = invoked.clone();
    let first = storage.clone();
    let outcome = tokio::spawn(async move {
        first
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |_| {
                    seen.store(true, Ordering::SeqCst);
                    Ok(())
                }),
            )
            .await
    })
    .await;
    let closed = storage.close().await;
    let result = outcome.expect("adapter setup panic must not resume as a caller panic");
    assert!(result.is_err());
    assert!(!invoked.load(Ordering::SeqCst));
    assert!(storage.failure_state().failure().is_none());
    assert!(closed.is_err());
}

#[tokio::test]
async fn adapter_completion_panic_is_outside_the_user_callback_catch() {
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let finished = Arc::new(AtomicBool::new(false));
    let probe = finished.clone();
    let storage = SqliteStorage::open(
        data.clone(),
        StorageOptions {
            completion_probe: Some(Arc::new(move || {
                assert!(
                    !probe.load(Ordering::SeqCst),
                    "synthetic adapter completion panic"
                );
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let first = storage.clone();
    let outcome = tokio::spawn(async move {
        first
            .transaction(owner(), store::Lane::Entities, move |unit| {
                unit.create(team("partial"))?;
                finished.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
    })
    .await;
    let closed = storage.close().await;
    assert!(
        outcome
            .expect("adapter completion panic must not resume as a caller panic")
            .is_err()
    );
    assert!(storage.failure_state().failure().is_none());
    assert!(closed.is_err());
    let reopened = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    assert!(
        reopened
            .read(owner())
            .await
            .unwrap()
            .list(&Filter::Teams, &PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    reopened
        .transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(team("following"))
        })
        .await
        .unwrap();
    reopened.close().await.unwrap();
}

// Issue 149: qualify record O through Storage::write, not the lower Writer helper.
#[derive(Clone, Copy)]
enum CrashCause {
    Callback,
    Adapter,
    OperationCorrupt,
    CleanupNotAdb,
}

#[tokio::test]
async fn ordinary_callback_panic_serves_queued_work_and_reopens_without_partial_rows() {
    combined_crash_case(CrashCause::Callback).await;
}
#[tokio::test]
async fn escaping_adapter_panic_closes_both_receipts_and_reopens_without_partial_rows() {
    combined_crash_case(CrashCause::Adapter).await;
}
#[tokio::test]
async fn fatal_operation_with_ordinary_cleanup_closes_both_receipts_and_reopens_without_partial_rows()
 {
    combined_crash_case(CrashCause::OperationCorrupt).await;
}
#[tokio::test]
async fn ordinary_operation_with_fatal_cleanup_closes_both_receipts_and_reopens_without_partial_rows()
 {
    combined_crash_case(CrashCause::CleanupNotAdb).await;
}

async fn combined_crash_case(cause: CrashCause) {
    use super::super::{FatalCause, Role};
    use orch_core::store::intents::IntentPayload;
    use std::io::{Read, Seek, SeekFrom, Write};
    const LIMIT: Duration = Duration::from_secs(10);
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    // Migrate before planting a fault. Never damage a real project or a database
    // with open connections; preserve the original byte for the reopen oracle.
    SqliteStorage::open(data.clone(), StorageOptions::default())
        .unwrap()
        .close()
        .await
        .unwrap();
    let damaged = if matches!(cause, CrashCause::OperationCorrupt) {
        let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let page: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='intents'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        let path = conn.path().unwrap().to_owned();
        drop(conn);
        let offset = ((page - 1) * size) as u64;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut original = [0];
        file.read_exact(&mut original).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
        Some((path, offset, original))
    } else {
        None
    };
    let finished = Arc::new(AtomicBool::new(false));
    let probe = finished.clone();
    let storage = SqliteStorage::open(
        data.clone(),
        StorageOptions {
            writer_capacity: 1,
            completion_probe: Some(Arc::new(move || {
                if matches!(cause, CrashCause::CleanupNotAdb) {
                    crate::sqlite::writer::contain::set_cleanup_error(rusqlite::ffi::SQLITE_NOTADB);
                }
                assert!(
                    !(matches!(cause, CrashCause::Adapter) && probe.load(Ordering::SeqCst)),
                    "synthetic escaping adapter panic"
                );
            })),
            writer_setup: Some(Box::new(move |conn| {
                conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Savepoint {
                        savepoint_name: "orch_intent",
                        operation: TransactionOperation::Rollback,
                    } if matches!(cause, CrashCause::OperationCorrupt) => Authorization::Deny,
                    AuthAction::Savepoint {
                        savepoint_name: "orch_message",
                        operation: TransactionOperation::Release | TransactionOperation::Rollback,
                    } if matches!(cause, CrashCause::CleanupNotAdb) => Authorization::Deny,
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..super::tests::without_admission_expiry()
        },
    )
    .unwrap();
    let (entered, running) = oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let first = storage.clone();
    let writing = tokio::spawn(async move {
        first
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |unit| {
                    unit.create(team("partial"))?;
                    let _ = entered.send(());
                    blocked
                        .recv_timeout(LIMIT)
                        .expect("parent releases callback");
                    match cause {
                        CrashCause::Callback => {
                            std::panic::panic_any(String::from("combined callback payload"))
                        }
                        CrashCause::Adapter => {
                            finished.store(true, Ordering::SeqCst);
                        }
                        CrashCause::OperationCorrupt => {
                            let _ = unit.add_intent(
                                &IntentPayload::WorkspaceRemove {
                                    task: "synthetic-task".into(),
                                },
                                0,
                            );
                        }
                        CrashCause::CleanupNotAdb => {
                            let _ = unit.create(message());
                        }
                    }
                    Ok(())
                }),
            )
            .await
    });
    tokio::time::timeout(LIMIT, running).await.unwrap().unwrap();
    let next = storage.clone();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = invoked.clone();
    let mut queued = Box::pin(async move {
        next.transaction(owner(), store::Lane::Entities, move |unit| {
            seen.store(true, Ordering::SeqCst);
            unit.create(team("queued"))
        })
        .await
    });
    assert!(
        futures::poll!(&mut queued).is_pending(),
        "successor is admitted behind active transaction"
    );
    release.send(()).unwrap();
    let first = tokio::time::timeout(LIMIT, writing).await.unwrap();
    let queued = tokio::time::timeout(LIMIT, queued).await.unwrap();
    let fatal = match cause {
        CrashCause::OperationCorrupt => Some((Role::Operation, FatalCause::SqliteCorrupt)),
        CrashCause::CleanupNotAdb => Some((Role::Rollback, FatalCause::SqliteNotADatabase)),
        _ => None,
    };
    if matches!(cause, CrashCause::Callback) {
        let payload = first.unwrap_err().into_panic();
        assert_eq!(
            *payload.downcast::<String>().unwrap(),
            "combined callback payload"
        );
        queued.unwrap();
        assert!(invoked.load(Ordering::SeqCst));
        storage
            .transaction(owner(), store::Lane::Entities, |u| {
                u.create(team("following"))
            })
            .await
            .unwrap();
    } else {
        let expected = if fatal.is_some() {
            "storage_unavailable"
        } else {
            "SQLite writer is closed"
        };
        for result in [
            first.expect("adapter unwind is not a caller panic"),
            queued.map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(ref e) if e.to_string().contains(expected)),
                "{result:?}"
            );
        }
        assert!(!invoked.load(Ordering::SeqCst));
        let later = storage
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(|_| panic!("closed callback ran")),
            )
            .await;
        assert!(
            matches!(later, Err(ref e) if e.to_string().contains(expected)),
            "{later:?}"
        );
    }
    assert_eq!(
        storage.failure_state().failure().map(|f| f.causes.clone()),
        fatal.map(|f| vec![f])
    );
    let close = storage.close().await;
    assert_eq!(close.is_ok(), matches!(cause, CrashCause::Callback));
    // This repair restores only the deliberately damaged page-header byte. It
    // cannot undo transaction rows: the complete reopen oracle below still sees
    // any partial/queued writes that were wrongly committed.
    if let Some((path, offset, original)) = damaged {
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&original).unwrap();
        file.sync_all().unwrap();
    }
    let reopened = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    let snapshot = reopened.read(owner()).await.unwrap();
    let rows = snapshot
        .list(&Filter::Teams, &PageRequest::default())
        .await
        .unwrap();
    let expected = if matches!(cause, CrashCause::Callback) {
        vec!["following", "queued"]
    } else {
        vec![]
    };
    assert_eq!(
        rows.items.iter().map(|r| r.entity.id()).collect::<Vec<_>>(),
        expected
    );
    assert!(
        snapshot
            .get(EntityKind::Message, "m")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot
            .read_pending_intents(0, 10)
            .await
            .unwrap()
            .is_empty()
    );
    drop(snapshot);
    let recovery = reopened
        .transaction(owner(), store::Lane::Entities, |u| {
            u.create(team("recovery"))
        })
        .await
        .unwrap();
    assert_eq!(
        reopened
            .read(owner())
            .await
            .unwrap()
            .get(EntityKind::Team, "recovery")
            .await
            .unwrap(),
        Some(recovery)
    );
    reopened.close().await.unwrap();
}

// Issue 468: the Agent/Team/Run helper owns native_entity. Keep these
// cases separate from cctg so each leaf's retirement is independently exercised.
#[derive(Clone, Copy)]
enum AtrCause {
    Callback,
    OperationCorrupt,
    CleanupNotAdb,
    OrdinaryCleanup,
}
fn atr_config(key: &str) -> Entity {
    Entity::Config(orch_core::store::ConfigEntry {
        key: key.into(),
        value: "synthetic".into(),
    })
}
#[tokio::test]
async fn atr_ordinary_cleanup_retires_writer_and_closes_both_receipts() {
    atr_crash_case(AtrCause::OrdinaryCleanup).await;
}
#[tokio::test]
async fn atr_fatal_operation_latches_before_retiring_writer() {
    atr_crash_case(AtrCause::OperationCorrupt).await;
}
#[tokio::test]
async fn atr_fatal_cleanup_latches_before_retiring_writer() {
    atr_crash_case(AtrCause::CleanupNotAdb).await;
}
#[tokio::test]
async fn atr_caller_panic_preserves_payload_and_serves_queued_work() {
    atr_crash_case(AtrCause::Callback).await;
}
async fn atr_crash_case(cause: AtrCause) {
    use super::super::{FatalCause, Role};
    use std::io::{Read, Seek, SeekFrom, Write};
    const LIMIT: Duration = Duration::from_secs(10);
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    // Migrate before planting a fault. Never damage a real project or a database
    // with open connections; preserve the original byte for the reopen oracle.
    SqliteStorage::open(data.clone(), StorageOptions::default())
        .unwrap()
        .close()
        .await
        .unwrap();
    let damaged = if matches!(cause, AtrCause::OperationCorrupt) {
        let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let page: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name=?1",
                ["teams"],
                |r| r.get(0),
            )
            .unwrap();
        let size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        let path = conn.path().unwrap().to_owned();
        drop(conn);
        let offset = ((page - 1) * size) as u64;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut original = [0];
        file.read_exact(&mut original).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[0xff]).unwrap();
        file.sync_all().unwrap();
        Some((path, offset, original))
    } else {
        None
    };
    let storage = SqliteStorage::open(
        data.clone(),
        StorageOptions {
            writer_capacity: 1,
            completion_probe: Some(Arc::new(move || {
                if matches!(cause, AtrCause::CleanupNotAdb) {
                    crate::sqlite::writer::contain::set_cleanup_error(rusqlite::ffi::SQLITE_NOTADB);
                }
            })),
            writer_setup: Some(Box::new(move |conn| {
                conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Savepoint {
                        savepoint_name,
                        operation: TransactionOperation::Rollback,
                    } if savepoint_name == "native_entity"
                        && matches!(cause, AtrCause::OperationCorrupt) =>
                    {
                        Authorization::Deny
                    }
                    AuthAction::Savepoint {
                        savepoint_name,
                        operation: TransactionOperation::Release | TransactionOperation::Rollback,
                    } if savepoint_name == "native_entity"
                        && matches!(cause, AtrCause::CleanupNotAdb | AtrCause::OrdinaryCleanup) =>
                    {
                        Authorization::Deny
                    }
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..super::tests::without_admission_expiry()
        },
    )
    .unwrap();
    let (entered, running) = oneshot::channel();
    let (release, blocked) = mpsc::sync_channel(1);
    let first = storage.clone();
    let writing = tokio::spawn(async move {
        first
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(move |unit| {
                    unit.create(atr_config("partial-config"))?;
                    let _ = entered.send(());
                    blocked
                        .recv_timeout(LIMIT)
                        .expect("parent releases callback");
                    if matches!(cause, AtrCause::Callback) {
                        unit.create(team("partial-team"))?;
                        std::panic::panic_any(String::from("atr callback payload"));
                    }
                    // A callback may swallow returned errors, but failed cleanup
                    // must unwind past it, never reach this success return.
                    let _ = unit.create(team("must-rollback"));
                    Ok(())
                }),
            )
            .await
    });
    tokio::time::timeout(LIMIT, running).await.unwrap().unwrap();
    let next = storage.clone();
    let invoked = Arc::new(AtomicBool::new(false));
    let seen = invoked.clone();
    let mut queued = Box::pin(async move {
        next.transaction(owner(), store::Lane::Entities, move |unit| {
            seen.store(true, Ordering::SeqCst);
            unit.create(team("queued"))
        })
        .await
    });
    assert!(
        futures::poll!(&mut queued).is_pending(),
        "successor is admitted behind active transaction"
    );
    release.send(()).unwrap();
    let first = tokio::time::timeout(LIMIT, writing).await.unwrap();
    let queued = tokio::time::timeout(LIMIT, queued).await.unwrap();
    let fatal = match cause {
        AtrCause::OperationCorrupt => Some((Role::Operation, FatalCause::SqliteCorrupt)),
        AtrCause::CleanupNotAdb => Some((Role::Rollback, FatalCause::SqliteNotADatabase)),
        _ => None,
    };
    if matches!(cause, AtrCause::Callback) {
        let payload = first.unwrap_err().into_panic();
        assert_eq!(
            *payload.downcast::<String>().unwrap(),
            "atr callback payload"
        );
        queued.unwrap();
        assert!(invoked.load(Ordering::SeqCst));
        storage
            .transaction(owner(), store::Lane::Entities, |u| {
                u.create(team("following"))
            })
            .await
            .unwrap();
    } else {
        let expected = if fatal.is_some() {
            "storage_unavailable"
        } else {
            "SQLite writer is closed"
        };
        for result in [
            first.expect("adapter unwind is not a caller panic"),
            queued.map(|_| ()),
        ] {
            assert!(
                matches!(result, Err(ref e) if e.to_string().contains(expected)),
                "{result:?}"
            );
        }
        assert!(!invoked.load(Ordering::SeqCst));
        let later = storage
            .write(
                owner(),
                store::Lane::Entities,
                Box::new(|_| panic!("closed callback ran")),
            )
            .await;
        assert!(
            matches!(later, Err(ref e) if e.to_string().contains(expected)),
            "{later:?}"
        );
    }
    assert_eq!(
        storage.failure_state().failure().map(|f| f.causes.clone()),
        fatal.map(|f| vec![f])
    );
    assert!(!storage.failure_state().claims_suppressed());
    let close = storage.close().await;
    assert_eq!(close.is_ok(), matches!(cause, AtrCause::Callback));
    // This repair restores only the deliberately damaged page-header byte. It
    // cannot undo transaction rows: the complete reopen oracle below still sees
    // any partial/queued writes that were wrongly committed.
    if let Some((path, offset, original)) = damaged {
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&original).unwrap();
        file.sync_all().unwrap();
    }
    let reopened = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    let snapshot = reopened.read(owner()).await.unwrap();
    let rows = snapshot
        .list(&Filter::Teams, &PageRequest::default())
        .await
        .unwrap();
    let expected = if matches!(cause, AtrCause::Callback) {
        vec!["following", "queued"]
    } else {
        vec![]
    };
    assert_eq!(
        rows.items.iter().map(|r| r.entity.id()).collect::<Vec<_>>(),
        expected
    );
    assert!(
        snapshot
            .get(EntityKind::Message, "m")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        snapshot
            .read_pending_intents(0, 10)
            .await
            .unwrap()
            .is_empty()
    );
    for key in ["partial-config"] {
        assert!(
            snapshot
                .get(EntityKind::Config, key)
                .await
                .unwrap()
                .is_none()
        );
    }
    drop(snapshot);
    let recovery = reopened
        .transaction(owner(), store::Lane::Entities, |u| {
            u.create(team("recovery"))
        })
        .await
        .unwrap();
    assert_eq!(
        reopened
            .read(owner())
            .await
            .unwrap()
            .get(EntityKind::Team, "recovery")
            .await
            .unwrap(),
        Some(recovery)
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn atr_successful_cleanup_returns_error_and_keeps_writer_usable() {
    let (_temp, data) = crate::sqlite::test_data();
    let storage = SqliteStorage::open(
        Arc::new(data),
        StorageOptions {
            writer_setup: Some(Box::new(|conn| {
                let first_release = AtomicBool::new(true);
                conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Savepoint {
                        savepoint_name: "native_entity",
                        operation: TransactionOperation::Release,
                    } if first_release.swap(false, Ordering::SeqCst) => Authorization::Deny,
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..Default::default()
        },
    )
    .unwrap();
    storage
        .transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(atr_config("before"))?;
            assert!(unit.create(team("reject")).is_err());
            unit.create(team("before"))?;
            unit.create(team("after"))
        })
        .await
        .unwrap();
    storage
        .transaction(owner(), store::Lane::Entities, |unit| {
            unit.create(team("following"))
        })
        .await
        .unwrap();
    let read = storage.read(owner()).await.unwrap();
    for key in ["before", "after", "following"] {
        assert!(read.get(EntityKind::Team, key).await.unwrap().is_some());
    }
    assert!(
        read.get(EntityKind::Team, "reject")
            .await
            .unwrap()
            .is_none()
    );
    assert!(storage.failure_state().failure().is_none());
    drop(read);
    storage.close().await.unwrap();
}
