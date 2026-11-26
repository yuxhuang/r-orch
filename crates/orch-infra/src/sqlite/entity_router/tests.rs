use super::*;
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, Synchronous, Writer, WriterHandle, migrations, open_connection,
};
use orch_core::store::{EntityKind, Principal, Sort, StoreError};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn payload(family: Family, id: &str, text: &str) -> Payload {
    use serde_json::json;
    match family {
        Family::Task => Payload::Task(Box::new(serde_json::from_value(json!({
            "id":id,"title":text,"description":"", "status":"todo","priority":2,
            "created_at":"2026-11-20T10:00:00.000Z","updated_at":"2026-11-20T10:00:00.000Z",
            "attempts":0,"max_attempts":3
        })).unwrap())),
        Family::Agent => Payload::Agent(Box::new(serde_json::from_value(json!({
            "id":id,"name":text,"adapter":"shell","config":{},"status":"idle",
            "stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}
        })).unwrap())),
        Family::Team => Payload::Team(Box::new(serde_json::from_value(json!({
            "id":id,"name":text,"status":"active","members":[],"task_pool":[],
            "lead_agent_id":"a","created_at":"opaque","updated_at":"opaque","config":{"auto_claim":false}
        })).unwrap())),
        Family::Run => Payload::Run(Box::new(serde_json::from_value(json!({
            "id":id,"task_id":id,"agent_id":"a","attempt":1,"status":"running",
            "started_at":"opaque","workspace_path":"","prompt":text
        })).unwrap())),
        Family::Message => Payload::Message(Box::new(serde_json::from_value(json!({
            "id":id,"channel":"broadcast","from_agent_id":"a","to_agent_id":null,
            "subject":text,"body":"body","created_at":"opaque","status":"pending"
        })).unwrap())),
    }
}

fn entity(payload: Payload) -> Entity {
    match payload {
        Payload::Task(v) => Entity::Task(v),
        Payload::Agent(v) => Entity::Agent(v),
        Payload::Team(v) => Entity::Team(v),
        Payload::Run(v) => Entity::Run(v),
        Payload::Message(v) => Entity::Message(v),
    }
}

fn setup(data: &crate::data_dir::DataDir) {
    let mut conn = open_connection(data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(data, &mut conn).unwrap();
}

fn submit<T: Send + 'static>(
    handle: &WriterHandle,
    work: impl FnOnce(&Transaction<'_>) -> Result<T> + Send + 'static,
) -> Result<T> {
    handle
        .submit(
            Lane::Entities,
            Some(Instant::now() + Duration::from_secs(10)),
            work,
        )?
        .wait()
}

fn scenario(body: impl FnOnce(&crate::data_dir::DataDir, &WriterHandle, &Arc<DiskBlobStore>)) {
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let thread = scope.spawn(|| writer.run());
        struct Close<'a>(&'a WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let close = Close(&handle);
        body(&data, &handle, &disk);
        drop(close);
        thread.join().unwrap();
    });
}

const FAMILIES: [Family; 5] = [
    Family::Task,
    Family::Agent,
    Family::Team,
    Family::Run,
    Family::Message,
];

#[test]
fn every_admitted_family_routes_the_full_lifecycle_and_reopens() {
    scenario(|data, handle, disk| {
        for family in FAMILIES {
            let state = handle.failure_state();
            let blobs = disk.clone();
            let (changed, moved) = submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state, -1234);
                assert_eq!(unit.caller(), &caller);
                let first = unit.create(payload(family, "same-id", "first"))?;
                assert_eq!(first.entity, entity(payload(family, "same-id", "first")));
                assert_eq!(first.revision, Revision::ONE);
                unit.create(payload(family, "later", "other"))?;
                let changed = unit.update(payload(family, "same-id", "changed"), first.revision)?;
                assert_eq!(changed.position, first.position);
                let moved = unit.move_to_end(family, "same-id", changed.revision)?;
                assert!(moved.position > changed.position);
                assert_eq!(moved.entity, changed.entity);
                assert_eq!(moved.revision, changed.revision.next().unwrap());
                assert_eq!(unit.get(family, "same-id")?, Some(moved.clone()));
                unit.delete(family, "later", Revision::ONE)?;
                assert_eq!(unit.get(family, "later")?, None);
                unit.ensure_committable()?;
                Ok((changed, moved))
            })
            .unwrap();
            assert_eq!(
                changed.entity,
                entity(payload(family, "same-id", "changed"))
            );
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                    .unwrap();
            let got = pool
                .read(Instant::now() + Duration::from_secs(10), |tx| {
                    let caller = owner("p");
                    let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
                    assert_eq!(read.caller(), &caller);
                    read.get(family, "same-id")
                })
                .unwrap();
            assert_eq!(got, Some(moved));
        }
    });
}

#[test]
fn caller_project_cannot_be_overridden_by_an_entity_id() {
    scenario(|data, handle, disk| {
        let blobs = disk.clone();
        let state = handle.failure_state();
        submit(handle, move |tx| {
            for project in ["p", "q"] {
                let caller = owner(project);
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                for family in FAMILIES {
                    unit.create(payload(family, "same", project))?;
                }
                unit.ensure_committable()?;
            }
            Ok(())
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            for project in ["p", "q", "empty"] {
                let caller = owner(project);
                let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
                for family in FAMILIES {
                    let got = read.get(family, "same")?;
                    if project == "empty" {
                        assert_eq!(got, None);
                    } else {
                        assert_eq!(
                            got.unwrap().entity,
                            entity(payload(family, "same", project))
                        );
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn caught_ordinary_refusal_preserves_other_work_but_callback_error_rolls_everything_back() {
    scenario(|data, handle, disk| {
        for family in FAMILIES {
            let blobs = disk.clone();
            let state = handle.failure_state();
            submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
                let record = unit.create(payload(family, "kept", "first"))?;
                let refusal = unit.update(payload(family, "missing", "wrong"), record.revision);
                assert!(matches!(
                    refusal,
                    Err(SqliteError::Store(StoreError::StaleRevision {
                        actual: None,
                        ..
                    }))
                ));
                assert!(state.failure().is_none());
                unit.create(payload(family, "also-kept", "second"))?;
                unit.ensure_committable()
            })
            .unwrap();
            let blobs = disk.clone();
            let state = handle.failure_state();
            let result: Result<()> = submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                unit.create(payload(family, "rolled-back", "third"))?;
                Err(StoreError::InvalidPage("synthetic callback failure".into()).into())
            });
            assert!(matches!(
                result,
                Err(SqliteError::Store(StoreError::InvalidPage(_)))
            ));
            let pool =
                ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
                    .unwrap();
            pool.read(Instant::now() + Duration::from_secs(10), |tx| {
                let caller = owner("p");
                let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
                assert!(read.get(family, "kept")?.is_some());
                assert!(read.get(family, "also-kept")?.is_some());
                assert!(read.get(family, "rolled-back")?.is_none());
                Ok(())
            })
            .unwrap();
        }
    });
}

#[test]
fn caught_native_corruption_latches_the_real_writer_and_discards_prior_work() {
    scenario(|data, handle, disk| {
        let blobs = disk.clone();
        let state = handle.failure_state();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            unit.create(payload(Family::Agent, "broken", "agent"))?;
            unit.ensure_committable()?;
            tx.execute(
                "DELETE FROM entity_details WHERE project_id='p' AND family='agent'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let blobs = disk.clone();
        let state = handle.failure_state();
        let result = submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
            unit.create(payload(Family::Task, "must-rollback", "task"))?;
            assert!(matches!(
                unit.get(Family::Agent, "broken"),
                Err(SqliteError::EntityCorrupt {
                    family: "agent",
                    ..
                })
            ));
            assert_eq!(state.failure().unwrap().source, FailureSource::Writer);
            // Deliberately omit the router guard: the executor must still refuse
            // a callback that swallows an observed fatal error and returns Ok.
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
        let conn = open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM tasks WHERE id='must-rollback'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    });
}

#[test]
fn reader_caught_damage_is_attributed_to_reader_and_latches_the_shared_writer() {
    scenario(|data, handle, disk| {
        let blobs = disk.clone();
        let state = handle.failure_state();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            unit.create(payload(Family::Agent, "broken", "message"))?;
            unit.ensure_committable()?;
            tx.execute(
                "DELETE FROM entity_details WHERE project_id='p' AND family='agent'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            assert!(matches!(
                read.get(Family::Agent, "broken"),
                Err(SqliteError::EntityCorrupt { .. })
            ));
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::StorageUnavailable(_))));
        assert_eq!(
            handle.failure_state().failure().unwrap().source,
            FailureSource::Reader
        );
        assert!(matches!(
            submit(handle, |_| Ok(())),
            Err(SqliteError::StorageUnavailable(_))
        ));
    });
}

#[test]
fn task_page_and_move_return_original_snapshot_records() {
    scenario(|data, handle, disk| {
        let cursors = TaskCursors::new("generation");
        let blobs = disk.clone();
        let state = handle.failure_state();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            unit.create(payload(Family::Task, "a", "first"))?;
            unit.create(payload(Family::Task, "b", "second"))?;
            let page = unit.task_page(
                &cursors,
                0,
                &Filter::Tasks {
                    status: None,
                    goal: None,
                },
                &PageRequest {
                    limit: 1,
                    cursor: None,
                    sort: Sort::Default,
                    with_count: true,
                },
            )?;
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.total, Some(2));
            assert_eq!(page.items[0].entity.id(), "a");
            assert!(page.next_cursor.is_some());
            unit.ensure_committable()
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            let old = read.get(Family::Task, "a")?.unwrap();
            let blobs = disk.clone();
            let state = handle.failure_state();
            submit(handle, move |tx| {
                let caller = owner("p");
                let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
                unit.update(payload(Family::Task, "a", "new value"), Revision::ONE)?;
                unit.ensure_committable()
            })?;
            let page = read.task_page(
                &TaskCursors::new("generation"),
                0,
                &Filter::Tasks {
                    status: None,
                    goal: None,
                },
                &PageRequest {
                    limit: 10,
                    cursor: None,
                    sort: Sort::Default,
                    with_count: true,
                },
            )?;
            assert_eq!(page.items[0], old);
            assert_eq!(page.items[0].entity.kind(), EntityKind::Task);
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn message_damage_keeps_its_existing_ordinary_class_and_typed_error() {
    scenario(|data, handle, disk| {
        let blobs = disk.clone();
        let state = handle.failure_state();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            unit.create(payload(Family::Message, "broken", "message"))?;
            unit.ensure_committable()?;
            tx.execute(
                "DELETE FROM entity_details WHERE project_id='p' AND family='message'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            EntityRead::reader(tx, &caller, disk, pool.failure_state().clone())
                .get(Family::Message, "broken")
        });
        let error = result.unwrap_err();
        assert!(matches!(error, SqliteError::MessageCorrupt { .. }));
        assert!(!crate::sqlite::classify(&error).is_fatal());
        assert!(handle.failure_state().failure().is_none());
    });
}

#[test]
fn message_blob_registration_uses_the_supplied_signed_clock() {
    scenario(|data, handle, disk| {
        let blobs = disk.clone();
        let state = handle.failure_state();
        let text = "é".repeat(32769);
        let expected = text.clone();
        submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, -4321);
            let saved = unit.create(payload(Family::Message, "large", &text))?;
            assert_eq!(
                saved.entity,
                entity(payload(Family::Message, "large", &text))
            );
            unit.ensure_committable()
        })
        .unwrap();
        let conn = open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        let (count, min, max): (i64, i64, i64) = conn
            .query_row(
                "SELECT count(*),min(created_at),max(created_at) FROM blobs WHERE project_id='p'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(count > 0);
        assert_eq!((min, max), (-4321, -4321));
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let got = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone())
                .get(Family::Message, "large")?
                .unwrap();
            assert_eq!(
                got.entity,
                entity(payload(Family::Message, "large", &expected))
            );
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn caught_failed_native_cleanup_aborts_all_families_and_next_transaction_succeeds() {
    scenario(|data, handle, disk| {
        submit(handle, |tx| {
            tx.execute_batch("CREATE TRIGGER abort_agent BEFORE INSERT ON agents WHEN NEW.id='fatal' BEGIN SELECT RAISE(ROLLBACK, 'synthetic'); END")?;
            Ok(())
        }).unwrap();
        let blobs = disk.clone();
        let state = handle.failure_state();
        let result = submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
            unit.create(payload(Family::Task, "earlier", "before failure"))?;
            let error = unit
                .create(payload(Family::Agent, "fatal", "agent"))
                .unwrap_err();
            assert!(
                matches!(&error, SqliteError::OperationRollback { operation, .. } if matches!(**operation, SqliteError::Sql(_)))
            );
            assert!(crate::sqlite::classify(&error).savepoint_rollback_failed());
            for family in FAMILIES {
                assert!(matches!(
                    unit.create(payload(family, "after", "after failure")),
                    Err(SqliteError::TransactionAborted)
                ));
            }
            assert!(state.failure().is_none());
            Ok(())
        });
        assert!(matches!(result, Err(SqliteError::TransactionAborted)));
        let conn = open_connection(data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM tasks WHERE id='earlier'", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
        let blobs = disk.clone();
        let state = handle.failure_state();
        let fresh = submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
            let fresh = unit.create(payload(Family::Task, "fresh", "healthy"))?;
            unit.ensure_committable()?;
            Ok(fresh)
        })
        .unwrap();
        assert_eq!(fresh.position, 1);
    });
}

#[test]
fn ended_read_transaction_refuses_before_looking_up_any_family() {
    scenario(|data, handle, disk| {
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            tx.execute_batch("ROLLBACK")?;
            for family in FAMILIES {
                assert!(matches!(
                    read.get(family, "missing"),
                    Err(SqliteError::TransactionAborted)
                ));
            }
            Err::<(), _>(SqliteError::TransactionAborted)
        });
        assert!(matches!(result, Err(SqliteError::TransactionAborted)));
        assert!(handle.failure_state().failure().is_none());
    });
}

#[test]
fn failed_cleanup_with_active_transaction_also_refuses_reads_and_task_pages() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.for_migration()
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                operation: TransactionOperation::Rollback,
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let state = FailureState::new();
    let mut unit = EntityWrite::new(&tx, &caller, &disk, state.clone(), 0);
    unit.create(payload(Family::Task, "kept", "before failure"))
        .unwrap();
    tx.execute_batch("CREATE TEMP TRIGGER fail_agent BEFORE INSERT ON agents BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    assert!(matches!(
        unit.create(payload(Family::Agent, "broken", "agent")),
        Err(SqliteError::OperationRollback { .. })
    ));
    assert!(!tx.is_autocommit());
    assert!(state.failure().is_none());
    for family in FAMILIES {
        assert!(matches!(
            unit.get(family, "kept"),
            Err(SqliteError::TransactionAborted)
        ));
    }
    assert!(matches!(
        unit.task_page(
            &TaskCursors::new("generation"),
            0,
            &Filter::Tasks {
                status: None,
                goal: None
            },
            &PageRequest {
                limit: 1,
                cursor: None,
                sort: Sort::Default,
                with_count: true
            }
        ),
        Err(SqliteError::TransactionAborted)
    ));
    tx.rollback().unwrap();
}

fn assert_poisoned_mutations(trigger: &str, poison: Payload) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.for_migration()
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                operation: TransactionOperation::Rollback,
                ..
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let state = FailureState::new();
    let mut unit = EntityWrite::new(&tx, &caller, &disk, state.clone(), 0);
    for family in FAMILIES {
        unit.create(payload(family, "kept", "before")).unwrap();
    }
    tx.execute_batch(trigger).unwrap();
    assert!(matches!(
        unit.create(poison),
        Err(SqliteError::OperationRollback { .. })
    ));
    assert!(!tx.is_autocommit());
    assert_mutations_refused(&mut unit);
    drop(unit);
    tx.rollback().unwrap();
}

fn assert_mutations_refused(unit: &mut EntityWrite<'_, '_>) {
    let mut out = Vec::new();
    let aborted = |r: &Result<()>| matches!(r, Err(SqliteError::TransactionAborted));
    for family in FAMILIES {
        let n = format!("{:?}", entity(payload(family, "x", "x")).kind());
        let r = unit.create(payload(family, "new", "after")).map(|_| ());
        out.push((format!("{n} create"), aborted(&r)));
        let r = unit
            .update(payload(family, "kept", "after"), Revision::ONE)
            .map(|_| ());
        out.push((format!("{n} update"), aborted(&r)));
        let r = unit.move_to_end(family, "kept", Revision::ONE).map(|_| ());
        out.push((format!("{n} move"), aborted(&r)));
        let r = unit.delete(family, "kept", Revision::ONE);
        out.push((format!("{n} delete"), aborted(&r)));
    }
    out.push((
        "ensure_committable".into(),
        aborted(&unit.ensure_committable()),
    ));
    let bad: Vec<_> = out.into_iter().filter(|(_, ok)| !ok).collect();
    assert!(bad.is_empty(), "operations not refused: {bad:?}");
}

#[test]
fn native_cleanup_poison_refuses_every_mutation() {
    assert_poisoned_mutations(
        "CREATE TEMP TRIGGER fail_agent BEFORE INSERT ON agents BEGIN SELECT RAISE(ABORT,'injected'); END",
        payload(Family::Agent, "broken", "agent"),
    );
}

#[test]
fn task_cleanup_poison_refuses_every_mutation() {
    assert_poisoned_mutations(
        "CREATE TEMP TRIGGER fail_task BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT,'injected'); END",
        payload(Family::Task, "broken", "task"),
    );
}

#[test]
fn every_native_write_observes_corruption_before_returning() {
    for operation in ["create", "update", "move", "delete"] {
        let (_temp, data) = crate::sqlite::test_data();
        setup(&data);
        let disk = DiskBlobStore::open(data.root()).unwrap();
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        let tx = conn.transaction().unwrap();
        let caller = owner("p");
        let state = FailureState::new();
        let mut unit = EntityWrite::new(&tx, &caller, &disk, state.clone(), 0);
        unit.create(payload(Family::Agent, "a", "agent")).unwrap();
        if operation == "create" {
            tx.execute("DELETE FROM agents WHERE project_id='p'", [])
                .unwrap();
        } else {
            tx.execute("DELETE FROM entity_details WHERE project_id='p'", [])
                .unwrap();
        }
        let error = match operation {
            "create" => unit
                .create(payload(Family::Agent, "a", "agent"))
                .map(|_| ()),
            "update" => unit
                .update(payload(Family::Agent, "a", "agent"), Revision::ONE)
                .map(|_| ()),
            "move" => unit
                .move_to_end(Family::Agent, "a", Revision::ONE)
                .map(|_| ()),
            "delete" => unit.delete(Family::Agent, "a", Revision::ONE),
            _ => unreachable!(),
        }
        .unwrap_err();
        assert!(
            matches!(error, SqliteError::EntityCorrupt { .. }),
            "{operation}: {error:?}"
        );
        let failure = state.failure().expect(operation);
        assert_eq!(failure.source, FailureSource::Writer);
        assert!(crate::sqlite::writer::failure::classify(&error).is_fatal());
        assert!(matches!(
            unit.ensure_committable(),
            Err(SqliteError::StorageUnavailable(_))
        ));
        drop(unit);
        tx.rollback().unwrap();
    }
}

#[test]
fn native_blob_registration_uses_signed_clock() {
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let mut unit = EntityWrite::new(&tx, &caller, &disk, FailureState::new(), -777);
    unit.create(payload(Family::Run, "r", &"é".repeat(40000)))
        .unwrap();
    let (count, min, max): (i64, i64, i64) = tx
        .query_row(
            "SELECT count(*),min(created_at),max(created_at) FROM blobs WHERE project_id='p'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(count > 0);
    assert_eq!((min, max), (-777, -777));
    drop(unit);
    tx.rollback().unwrap();
}

#[test]
fn ended_read_refuses_task_page() {
    scenario(|data, handle, disk| {
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            tx.execute_batch("ROLLBACK")?;
            let page = read.task_page(
                &TaskCursors::new("generation"),
                0,
                &Filter::Tasks {
                    status: None,
                    goal: None,
                },
                &PageRequest {
                    limit: 1,
                    cursor: None,
                    sort: Sort::Default,
                    with_count: true,
                },
            );
            assert!(matches!(page, Err(SqliteError::TransactionAborted)));
            Err::<(), _>(SqliteError::TransactionAborted)
        });
        assert!(matches!(result, Err(SqliteError::TransactionAborted)));
    });
}

fn task_storage(tx: &Transaction<'_>) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    [
        "SELECT * FROM tasks WHERE project_id='p' ORDER BY id",
        "SELECT * FROM task_details WHERE project_id='p' ORDER BY task_id",
        "SELECT * FROM counters WHERE project_id='p' AND name='position:task'",
    ]
    .into_iter()
    .map(|sql| {
        let mut statement = tx.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

#[test]
fn caught_task_move_hydration_failure_preserves_rows_and_counter() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let deny = Arc::new(AtomicBool::new(false));
    let flag = deny.clone();
    // Benign connection-local fault: only hydration's detail SELECT is
    // refused. The move's revision/position queries and writes work.
    conn.for_migration()
        .authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Read {
                table_name: "task_details",
                ..
            } if flag.load(Ordering::SeqCst) => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let state = FailureState::new();
    let mut unit = EntityWrite::new(&tx, &caller, &disk, state.clone(), 0);
    let original = unit
        .create(payload(Family::Task, "kept", "before"))
        .unwrap();
    unit.create(payload(Family::Task, "other", "second"))
        .unwrap();
    let before = task_storage(&tx);
    deny.store(true, Ordering::SeqCst);
    let error = unit
        .move_to_end(Family::Task, "kept", original.revision)
        .unwrap_err();
    assert!(matches!(error, SqliteError::Sql(_)), "{error:?}");
    let classification = crate::sqlite::writer::failure::classify(&error);
    assert_eq!(
        classification.leaves(),
        &[(
            crate::sqlite::writer::failure::Role::Operation,
            crate::sqlite::writer::failure::Class::Transient,
        )]
    );
    assert!(!classification.savepoint_rollback_failed());
    // Even when hydration would fail, stale revision must win.
    let stale: Revision = serde_json::from_value(9.into()).unwrap();
    assert!(matches!(
        unit.move_to_end(Family::Task, "kept", stale),
        Err(SqliteError::Store(StoreError::StaleRevision { actual: Some(actual), .. })) if actual == Revision::ONE
    ));
    deny.store(false, Ordering::SeqCst);
    assert_eq!(task_storage(&tx), before);
    assert_eq!(unit.get(Family::Task, "kept").unwrap(), Some(original));
    assert!(state.failure().is_none());
    unit.create(payload(Family::Message, "unrelated", "committed"))
        .unwrap();
    unit.ensure_committable().unwrap();
    drop(unit);
    tx.commit().unwrap();
    let pool = ReadPool::open_with_state(&data, Synchronous::Normal, 1, state).unwrap();
    pool.read(Instant::now() + Duration::from_secs(10), |tx| {
        assert_eq!(task_storage(tx), before);
        let read = EntityRead::reader(tx, &caller, &disk, pool.failure_state().clone());
        assert!(read.get(Family::Message, "unrelated")?.is_some());
        Ok(())
    })
    .unwrap();
}

#[test]
fn task_move_cleanup_poison_refuses_all_reads_and_mutations() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
    let (_temp, data) = crate::sqlite::test_data();
    setup(&data);
    let disk = DiskBlobStore::open(data.root()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.for_migration()
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                savepoint_name: "entity_task_move",
                operation: TransactionOperation::Release | TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner("p");
    let state = FailureState::new();
    let mut unit = EntityWrite::new(&tx, &caller, &disk, state.clone(), 0);
    for family in FAMILIES {
        unit.create(payload(family, "kept", "before")).unwrap();
    }
    let error = unit
        .move_to_end(Family::Task, "kept", Revision::ONE)
        .unwrap_err();
    assert!(matches!(error, SqliteError::OperationRollback { .. }));
    assert!(crate::sqlite::writer::failure::classify(&error).savepoint_rollback_failed());
    assert!(!tx.is_autocommit());
    assert!(state.failure().is_none());
    assert_mutations_refused(&mut unit);
    for family in FAMILIES {
        assert!(matches!(
            unit.get(family, "kept"),
            Err(SqliteError::TransactionAborted)
        ));
    }
    assert!(matches!(
        unit.task_page(
            &TaskCursors::new("generation"),
            0,
            &Filter::Tasks {
                status: None,
                goal: None
            },
            &PageRequest {
                limit: 1,
                cursor: None,
                sort: Sort::Default,
                with_count: true
            },
        ),
        Err(SqliteError::TransactionAborted)
    ));
    drop(unit);
    tx.rollback().unwrap();
}
