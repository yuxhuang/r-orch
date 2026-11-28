//! Issue 477: both Task savepoint owners retire the real Storage writer.
use super::*;
use super::{owner, team};
use orch_core::store::{Revision, StorageExt};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
#[derive(Clone, Copy)]
enum TaskCause {
    Callback,
    OperationCorrupt,
    CleanupNotAdb,
    OrdinaryCleanup,
}

fn task(id: &str) -> Entity {
    let mut task = orch_core::test_support::make_task();
    task.id = id.into();
    task.created_at = "2025-01-01T00:00:00.000Z".into();
    task.updated_at = task.created_at.clone();
    Entity::Task(Box::new(task))
}
#[tokio::test]
async fn task_entity_ordinary_cleanup() {
    task_crash_case(TaskCause::OrdinaryCleanup, false).await;
}
#[tokio::test]
async fn task_entity_fatal_operation() {
    task_crash_case(TaskCause::OperationCorrupt, false).await;
}
#[tokio::test]
async fn task_entity_fatal_cleanup() {
    task_crash_case(TaskCause::CleanupNotAdb, false).await;
}
#[tokio::test]
async fn task_entity_callback_panic() {
    task_crash_case(TaskCause::Callback, false).await;
}
#[tokio::test]
async fn task_move_ordinary_cleanup() {
    task_crash_case(TaskCause::OrdinaryCleanup, true).await;
}
#[tokio::test]
async fn task_move_fatal_operation() {
    task_crash_case(TaskCause::OperationCorrupt, true).await;
}
#[tokio::test]
async fn task_move_fatal_cleanup() {
    task_crash_case(TaskCause::CleanupNotAdb, true).await;
}
#[tokio::test]
async fn task_move_callback_panic() {
    task_crash_case(TaskCause::Callback, true).await;
}
async fn task_crash_case(cause: TaskCause, moving: bool) {
    use crate::sqlite::{FatalCause, Role};

    use std::io::{Read, Seek, SeekFrom, Write};
    const LIMIT: Duration = Duration::from_secs(10);
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    // Migrate before planting a fault. Never damage a real project or a database
    // with open connections; preserve the original byte for the reopen oracle.
    let seed = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    seed.transaction(owner(), store::Lane::Entities, |u| u.create(task("seed")))
        .await
        .unwrap();
    seed.close().await.unwrap();
    let damaged = if matches!(cause, TaskCause::OperationCorrupt) {
        let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let page: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name=?1",
                ["task_details"],
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
                if matches!(cause, TaskCause::CleanupNotAdb) {
                    crate::sqlite::writer::contain::set_cleanup_error(rusqlite::ffi::SQLITE_NOTADB);
                }
            })),
            writer_setup: Some(Box::new(move |conn| {
                conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                    AuthAction::Savepoint {
                        savepoint_name,
                        operation: TransactionOperation::Rollback,
                    } if savepoint_name
                        == if moving {
                            "entity_task_move"
                        } else {
                            "task_entity"
                        }
                        && matches!(cause, TaskCause::OperationCorrupt) =>
                    {
                        Authorization::Deny
                    }
                    AuthAction::Savepoint {
                        savepoint_name,
                        operation: TransactionOperation::Release | TransactionOperation::Rollback,
                    } if savepoint_name
                        == if moving {
                            "entity_task_move"
                        } else {
                            "task_entity"
                        }
                        && matches!(
                            cause,
                            TaskCause::CleanupNotAdb | TaskCause::OrdinaryCleanup
                        ) =>
                    {
                        Authorization::Deny
                    }
                    _ => Authorization::Allow,
                }))
                .unwrap();
            })),
            ..super::super::tests::without_admission_expiry()
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
                        TaskCause::Callback => {
                            unit.create(task("partial-task"))?;
                            if moving {
                                unit.move_to_end(EntityKind::Task, "seed", Revision::ONE)?;
                            }
                            std::panic::panic_any(String::from("combined callback payload"))
                        }
                        _ => {
                            if moving {
                                let _ = unit.move_to_end(EntityKind::Task, "seed", Revision::ONE);
                            } else {
                                let _ = unit.create(task("must-rollback"));
                            }
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
        TaskCause::OperationCorrupt => Some((Role::Operation, FatalCause::SqliteCorrupt)),
        TaskCause::CleanupNotAdb => Some((Role::Rollback, FatalCause::SqliteNotADatabase)),
        _ => None,
    };
    if matches!(cause, TaskCause::Callback) {
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
    assert!(!storage.failure_state().claims_suppressed());
    let close = storage.close().await;
    assert_eq!(close.is_ok(), matches!(cause, TaskCause::Callback));
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
    let expected = if matches!(cause, TaskCause::Callback) {
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
    for key in ["partial-task", "must-rollback"] {
        assert!(snapshot.get(EntityKind::Task, key).await.unwrap().is_none());
    }
    let seed = snapshot
        .get(EntityKind::Task, "seed")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seed.revision, Revision::ONE);
    assert_eq!(seed.position, 1);
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
async fn task_successful_cleanup_keeps_the_writer_usable() {
    for moving in [false, true] {
        let (_temp, data) = crate::sqlite::test_data();
        let storage = SqliteStorage::open(
            Arc::new(data),
            StorageOptions {
                writer_setup: Some(Box::new(move |conn| {
                    let first_release = AtomicBool::new(true);
                    conn.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
                        AuthAction::Savepoint {
                            savepoint_name,
                            operation: TransactionOperation::Release,
                        } if savepoint_name
                            == if moving {
                                "entity_task_move"
                            } else {
                                "task_entity"
                            }
                            && first_release.swap(false, Ordering::SeqCst) =>
                        {
                            Authorization::Deny
                        }
                        _ => Authorization::Allow,
                    }))
                    .unwrap();
                })),
                ..super::super::tests::without_admission_expiry()
            },
        )
        .unwrap();
        storage
            .transaction(owner(), store::Lane::Entities, move |u| {
                u.create(team("before"))?;
                if moving {
                    u.create(task("seed"))?;
                    assert!(
                        u.move_to_end(EntityKind::Task, "seed", Revision::ONE)
                            .is_err()
                    );
                    let seed = u.get(EntityKind::Task, "seed")?.unwrap();
                    assert_eq!((seed.revision, seed.position), (Revision::ONE, 1));
                } else {
                    assert!(u.create(task("reject")).is_err());
                    assert!(u.get(EntityKind::Task, "reject")?.is_none());
                }
                u.create(task("after"))
            })
            .await
            .unwrap();
        storage
            .transaction(owner(), store::Lane::Entities, |u| {
                u.create(task("following"))
            })
            .await
            .unwrap();
        let read = storage.read(owner()).await.unwrap();
        for key in ["after", "following"] {
            assert!(read.get(EntityKind::Task, key).await.unwrap().is_some());
        }
        assert!(storage.failure_state().failure().is_none());
        drop(read);
        storage.close().await.unwrap();
    }
}
