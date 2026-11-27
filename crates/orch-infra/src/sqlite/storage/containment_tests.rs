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
