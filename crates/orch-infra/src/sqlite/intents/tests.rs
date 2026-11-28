use super::*;
use crate::sqlite::{
    ConnectionRole, Lane, Synchronous, Writer, migrations, open_connection, test_data,
};
use orch_core::store::{Principal, leases::NewLease};
use rusqlite::Connection;

fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    conn.execute(
        "UPDATE daemon_config SET value='current' WHERE key='lineage'",
        [],
    )
    .unwrap();
    conn
}

fn remove(id: &str) -> IntentPayload {
    IntentPayload::WorkspaceRemove { task: id.into() }
}

fn claim(tx: &Transaction<'_>, caller: &Caller) {
    leases::claim_lease(
        tx,
        caller,
        &NewLease {
            task: "t".into(),
            agent: "a".into(),
            run: "r".into(),
        },
        0,
    )
    .unwrap();
}

#[test]
fn typed_rows_read_their_writes_and_rollback_with_the_lease() {
    let mut conn = database();
    let who = caller("p");
    {
        let tx = conn.transaction().unwrap();
        claim(&tx, &who);
        let mut intents = Intents::new(&tx, &who);
        let payloads = [
            IntentPayload::ProcessStart { run: "r".into() },
            IntentPayload::ProcessStop { run: "r".into() },
            remove("t"),
            IntentPayload::BlobDelete {
                hash: "a".repeat(64),
            },
        ];
        for (n, payload) in payloads.into_iter().enumerate() {
            let added = intents.add_intent(&payload, -123).unwrap();
            assert_eq!(added.id, n as u64 + 1);
            assert_eq!(added.created_at, -123);
            assert_eq!(intents.get_intent(added.id).unwrap(), Some(added));
        }
        assert_eq!(
            intents.finish_intent(2, 99).unwrap(),
            FinishOutcome::Finished
        );
        assert_eq!(intents.get_intent(2).unwrap().unwrap().done_at, Some(99));
        assert_eq!(intents.list_pending_intents(1, 1).unwrap()[0].id, 3);
        assert_eq!(intents.list_pending_intents(3, 2).unwrap()[0].id, 4);
        tx.rollback().unwrap();
    }
    let tx = conn.transaction().unwrap();
    let mut intents = Intents::new(&tx, &who);
    assert!(intents.list_pending_intents(0, 1000).unwrap().is_empty());
    assert!(leases::get_lease(&tx, &who, "t").unwrap().is_none());
    assert_eq!(intents.add_intent(&remove("t"), 0).unwrap().id, 1);
}

#[test]
fn lease_fencing_uses_the_current_transaction_and_task_token() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let owner = caller("p");
    let payload = IntentPayload::ProcessStop { run: "r".into() };
    assert!(matches!(
        Intents::new(&tx, &owner).add_intent(&payload, 0),
        Err(SqliteError::Store(StoreError::StaleEpoch))
    ));
    claim(&tx, &owner);
    let lease = leases::get_lease(&tx, &owner, "t").unwrap().unwrap();
    let task = Caller {
        project: "p".into(),
        principal: Principal::Task {
            identity: "task".into(),
            task: "t".into(),
            run: "r".into(),
            lineage: lease.lineage.clone(),
            epoch: lease.epoch,
        },
    };
    assert_eq!(
        Intents::new(&tx, &task).add_intent(&payload, 1).unwrap().id,
        1
    );
    for field in 0..5 {
        let mut wrong = task.clone();
        let Principal::Task {
            task,
            run,
            lineage,
            epoch,
            ..
        } = &mut wrong.principal
        else {
            panic!()
        };
        match field {
            0 => *task = "other".into(),
            1 => *run = "other".into(),
            2 => *lineage = "old".into(),
            3 => *epoch += 1,
            _ => wrong.project = "other".into(),
        }
        assert!(matches!(
            Intents::new(&tx, &wrong).add_intent(&payload, 2),
            Err(SqliteError::Store(StoreError::StaleEpoch))
        ));
    }
    tx.execute(
        "UPDATE daemon_config SET value='new' WHERE key='lineage'",
        [],
    )
    .unwrap();
    assert!(matches!(
        Intents::new(&tx, &owner).add_intent(&payload, 3),
        Err(SqliteError::Store(StoreError::StaleEpoch))
    ));
    tx.execute(
        "UPDATE daemon_config SET value='current' WHERE key='lineage'",
        [],
    )
    .unwrap();
    leases::release_lease(&tx, &owner, "t", &lease.lineage, lease.epoch).unwrap();
    assert!(matches!(
        Intents::new(&tx, &owner).add_intent(&payload, 4),
        Err(SqliteError::Store(StoreError::StaleEpoch))
    ));
    assert_eq!(
        Intents::new(&tx, &owner)
            .get_intent(1)
            .unwrap()
            .unwrap()
            .payload,
        payload
    );
}

#[test]
fn invalid_inputs_caught_before_commit_do_not_spend_ids() {
    let mut conn = database();
    let who = caller("p");
    let tx = conn.transaction().unwrap();
    let mut intents = Intents::new(&tx, &who);
    for payload in [
        remove("../escape"),
        remove("."),
        remove(&"x".repeat(129)),
        IntentPayload::BlobDelete {
            hash: "A".repeat(64),
        },
    ] {
        assert!(matches!(
            intents.add_intent(&payload, 0),
            Err(SqliteError::Store(StoreError::InvalidIntent(_)))
        ));
    }
    for id in [0, 1, u64::MAX] {
        assert!(matches!(
            intents.finish_intent(id, 0),
            Err(SqliteError::Store(StoreError::InvalidIntent(_)))
        ));
    }
    for bound in [0, 1001, usize::MAX] {
        assert!(intents.list_pending_intents(0, bound).is_err());
        assert!(intents.delete_finished_intents(0, bound).is_err());
    }
    assert_eq!(
        intents
            .add_intent(&remove(&"x".repeat(128)), i64::MAX)
            .unwrap()
            .id,
        1
    );
    assert!(
        intents
            .list_pending_intents(u64::MAX, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(intents.get_intent(u64::MAX).unwrap(), None);
    tx.commit().unwrap();
}

#[test]
fn completion_is_first_wins_retention_is_exact_and_ids_are_not_reused() {
    let mut conn = database();
    let who = caller("p");
    let tx = conn.transaction().unwrap();
    let mut intents = Intents::new(&tx, &who);
    for n in 1..=5 {
        assert_eq!(intents.add_intent(&remove("t"), 0).unwrap().id, n);
    }
    for id in [5, 1, 3] {
        assert_eq!(
            intents.finish_intent(id, 42).unwrap(),
            FinishOutcome::Finished
        );
    }
    assert_eq!(
        intents.finish_intent(1, 999).unwrap(),
        FinishOutcome::AlreadyDone
    );
    assert_eq!(intents.get_intent(1).unwrap().unwrap().done_at, Some(42));
    assert_eq!(
        intents
            .delete_finished_intents(41 + DONE_RETENTION_MS, 1000)
            .unwrap(),
        0
    );
    assert_eq!(
        intents
            .delete_finished_intents(42 + DONE_RETENTION_MS, 2)
            .unwrap(),
        2
    );
    assert!(intents.get_intent(1).unwrap().is_none());
    assert!(intents.get_intent(3).unwrap().is_none());
    assert!(intents.get_intent(5).unwrap().is_some());
    assert_eq!(
        intents.finish_intent(1, 1000).unwrap(),
        FinishOutcome::AlreadyDone
    );
    assert_eq!(intents.delete_finished_intents(i64::MAX, 1000).unwrap(), 1);
    assert_eq!(
        intents
            .list_pending_intents(0, 1000)
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        [2, 4]
    );
    for id in [2, 4] {
        intents.finish_intent(id, 0).unwrap();
    }
    assert_eq!(
        intents
            .delete_finished_intents(DONE_RETENTION_MS, 1000)
            .unwrap(),
        2
    );
    assert_eq!(intents.add_intent(&remove("t"), 0).unwrap().id, 6);
}

#[test]
fn retention_underflow_keeps_equal_signed_minimum_until_seven_days() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    let mut intents = Intents::new(&tx, &who);
    intents.add_intent(&remove("t"), i64::MIN).unwrap();
    intents.finish_intent(1, i64::MIN).unwrap();
    for now in [i64::MIN, i64::MIN + DONE_RETENTION_MS - 1] {
        assert_eq!(intents.delete_finished_intents(now, 1000).unwrap(), 0);
        assert!(intents.get_intent(1).unwrap().is_some());
    }
    assert_eq!(
        intents
            .delete_finished_intents(i64::MIN + DONE_RETENTION_MS, 1000)
            .unwrap(),
        1
    );
}

#[test]
fn pending_backlog_is_numeric_bounded_and_project_local() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let p = caller("p");
    let q = caller("q");
    let mut intents = Intents::new(&tx, &p);
    for n in 1..=1001 {
        assert_eq!(intents.add_intent(&remove("t"), 0).unwrap().id, n);
    }
    let first = intents.list_pending_intents(0, 1000).unwrap();
    assert_eq!(
        first.iter().map(|i| i.id).collect::<Vec<_>>(),
        (1..=1000).collect::<Vec<_>>()
    );
    assert_eq!(
        intents.list_pending_intents(1000, 1000).unwrap()[0].id,
        1001
    );
    let mut foreign = Intents::new(&tx, &q);
    assert_eq!(foreign.get_intent(1).unwrap(), None);
    assert!(foreign.finish_intent(1, 0).is_err());
    foreign.add_intent(&remove("other"), 0).unwrap();
    foreign.finish_intent(1, 0).unwrap();
    for id in 1..=1001 {
        intents.finish_intent(id, 0).unwrap();
    }
    assert_eq!(
        intents
            .delete_finished_intents(DONE_RETENTION_MS, 1000)
            .unwrap(),
        1000
    );
    assert!(foreign.get_intent(1).unwrap().is_some());
    assert!(intents.get_intent(1001).unwrap().is_some());
}

#[test]
fn caught_statement_failures_restore_rows_counter_and_trigger_effects() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    let mut intents = Intents::new(&tx, &who);
    intents.add_intent(&remove("t"), 0).unwrap();
    tx.execute_batch(
        "CREATE TEMP TRIGGER refuse_counter BEFORE UPDATE ON counters BEGIN
        INSERT INTO daemon_config(key,value,revision) VALUES('side','effect',1);
        SELECT RAISE(FAIL,'injected'); END;",
    )
    .unwrap();
    assert!(matches!(
        intents.add_intent(&remove("second"), 0),
        Err(SqliteError::Sql(_))
    ));
    assert!(intents.get_intent(2).unwrap().is_none());
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM daemon_config WHERE key='side'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    tx.execute_batch("DROP TRIGGER refuse_counter").unwrap();
    assert_eq!(intents.add_intent(&remove("second"), 0).unwrap().id, 2);
    tx.execute_batch("CREATE TEMP TRIGGER refuse_finish AFTER UPDATE ON intents BEGIN SELECT RAISE(FAIL,'injected'); END;").unwrap();
    assert!(matches!(
        intents.finish_intent(1, 0),
        Err(SqliteError::Sql(_))
    ));
    assert_eq!(intents.get_intent(1).unwrap().unwrap().done_at, None);
    tx.execute_batch("DROP TRIGGER refuse_finish").unwrap();
    intents.finish_intent(1, 0).unwrap();
    tx.execute_batch("CREATE TEMP TRIGGER refuse_delete AFTER DELETE ON intents BEGIN SELECT RAISE(FAIL,'injected'); END;").unwrap();
    assert!(matches!(
        intents.delete_finished_intents(DONE_RETENTION_MS, 1000),
        Err(SqliteError::Sql(_))
    ));
    assert!(intents.get_intent(1).unwrap().is_some());
    tx.commit().unwrap();
}

#[test]
fn damaged_counters_and_signed_exhaustion_are_typed_and_change_nothing() {
    for value in [None, Some(-1), Some(0), Some(i64::MAX)] {
        let mut conn = database();
        let tx = conn.transaction().unwrap();
        let who = caller("p");
        let mut intents = Intents::new(&tx, &who);
        intents.add_intent(&remove("t"), 0).unwrap();
        tx.execute("DELETE FROM counters WHERE project_id='p'", [])
            .unwrap();
        if let Some(value) = value {
            tx.execute(
                "INSERT INTO counters(project_id,name,value) VALUES('p','intent',?1)",
                [value],
            )
            .unwrap();
        }
        let error = intents.add_intent(&remove("second"), 0).unwrap_err();
        assert!(match value {
            None => matches!(error, SqliteError::CounterMissing { .. }),
            Some(-1) => matches!(error, SqliteError::CounterNegative { .. }),
            Some(0) => matches!(error, SqliteError::CounterBehind { .. }),
            _ => matches!(error, SqliteError::Store(StoreError::CounterExhausted)),
        });
        assert!(intents.get_intent(2).unwrap().is_none());
    }
}

#[test]
fn damaged_payloads_fail_closed_without_echoing_stored_text() {
    for sql in [
        "UPDATE intents SET kind='process.stop'",
        "UPDATE intents SET run_id='other'",
        "UPDATE intents SET payload='{\"kind\":\"workspace.remove\",\"task\":\"../secret\"}'",
        "UPDATE intents SET payload='{\"kind\":\"unknown\",\"secret\":true}'",
        "UPDATE intents SET payload=printf('%300s','secret')",
        "UPDATE intents SET id=0",
    ] {
        let mut conn = database();
        let tx = conn.transaction().unwrap();
        let who = caller("p");
        let mut intents = Intents::new(&tx, &who);
        intents.add_intent(&remove("t"), 0).unwrap();
        tx.execute_batch(sql).unwrap();
        let error = intents
            .list_pending_intents(0, 1)
            .err()
            .or_else(|| intents.get_intent(0).err())
            .unwrap();
        assert!(matches!(error, SqliteError::IntentCorrupt(_)));
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn transaction_abort_cannot_be_caught_and_followed_by_a_new_intent() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    let mut intents = Intents::new(&tx, &who);
    intents.add_intent(&remove("first"), 0).unwrap();
    tx.execute_batch("CREATE TEMP TRIGGER abort_intent AFTER INSERT ON intents BEGIN SELECT RAISE(ROLLBACK,'injected'); END;").unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        intents.add_intent(&remove("second"), 0)
    }));
    assert!(result.is_err());
    assert!(matches!(
        intents.add_intent(&remove("third"), 0),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        intents.get_intent(1),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        intents.list_pending_intents(0, 1),
        Err(SqliteError::TransactionAborted)
    ));
    assert_eq!(
        tx.query_row("SELECT count(*) FROM intents", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn real_writer_reopen_retains_the_counter_after_every_row_is_pruned() {
    let (_temp, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut connection).unwrap();
    drop(connection);
    for expected in [1, 2] {
        let (writer, handle) = Writer::open(&data, Synchronous::Normal, 4).unwrap();
        struct Close<'a>(&'a crate::sqlite::WriterHandle);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        std::thread::scope(|threads| {
            let _close = Close(&handle);
            threads.spawn(move || writer.run());
            handle
                .submit(Lane::Entities, None, move |tx| {
                    let who = caller("p");
                    let mut intents = Intents::new(tx, &who);
                    assert_eq!(intents.add_intent(&remove("t"), i64::MIN)?.id, expected);
                    intents.finish_intent(expected, 0)?;
                    assert_eq!(intents.delete_finished_intents(DONE_RETENTION_MS, 1000)?, 1);
                    Ok(())
                })
                .unwrap()
                .wait()
                .unwrap();
        });
    }
}

#[test]
fn read_pool_keeps_pending_snapshot_across_writer_completion_and_retention() {
    use crate::sqlite::ReadPool;
    use std::time::{Duration, Instant};
    let (_temp, data) = test_data();
    let mut connection =
        open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut connection).unwrap();
    drop(connection);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 4).unwrap();
    let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
    struct Close<'a>(&'a crate::sqlite::WriterHandle);
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    std::thread::scope(|threads| {
        let _close = Close(&handle);
        threads.spawn(move || writer.run());
        handle
            .submit(Lane::Entities, None, |tx| {
                let who = caller("p");
                let mut intents = Intents::new(tx, &who);
                for _ in 0..3 {
                    intents.add_intent(&remove("t"), 0)?;
                }
                Ok(())
            })
            .unwrap()
            .wait()
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(5), |tx| {
            let who = caller("p");
            let intents = Intents::new(tx, &who);
            let before = intents.list_pending_intents(0, 1000)?;
            handle
                .submit(Lane::Entities, None, |tx| {
                    let who = caller("p");
                    let mut intents = Intents::new(tx, &who);
                    intents.finish_intent(2, 0)?;
                    assert_eq!(intents.get_intent(2)?.unwrap().done_at, Some(0));
                    assert_eq!(intents.list_pending_intents(1, 1)?[0].id, 3);
                    assert_eq!(intents.delete_finished_intents(DONE_RETENTION_MS, 1)?, 1);
                    Ok(())
                })?
                .wait()?;
            assert_eq!(intents.list_pending_intents(0, 1000)?, before);
            assert_eq!(intents.get_intent(2)?.unwrap().done_at, None);
            Ok(())
        })
        .unwrap();
        pool.read(Instant::now() + Duration::from_secs(5), |tx| {
            let who = caller("p");
            let intents = Intents::new(tx, &who);
            assert_eq!(intents.get_intent(2)?, None);
            assert_eq!(
                intents
                    .list_pending_intents(0, 1000)?
                    .iter()
                    .map(|i| i.id)
                    .collect::<Vec<_>>(),
                [1, 3]
            );
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn valid_padded_payloads_over_the_stored_bound_are_redacted_on_both_reads() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    let mut intents = Intents::new(&tx, &who);
    let payload = remove("synthetic-secret");
    intents.add_intent(&payload, 0).unwrap();
    let text = serde_json::to_string(&payload).unwrap();
    // Exact boundary remains readable; one extra JSON whitespace byte does not.
    for bytes in [256, 257] {
        let padded = format!("{text:bytes$}");
        assert_eq!(
            serde_json::from_str::<IntentPayload>(&padded).unwrap(),
            payload
        );
        tx.execute("UPDATE intents SET payload=?1", [&padded])
            .unwrap();
        for result in [
            intents.get_intent(1).map(|_| ()),
            intents.list_pending_intents(0, 1).map(|_| ()),
        ] {
            if bytes == 256 {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert!(matches!(
                    error,
                    SqliteError::IntentCorrupt("invalid id or payload size")
                ));
                assert!(!error.to_string().contains("synthetic-secret"));
                assert!(!format!("{error:?}").contains("synthetic-secret"));
            }
        }
    }
}

#[test]
fn ambiguous_run_leases_refuse_both_process_intents_without_spending_an_id() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    claim(&tx, &who);
    // Normal claims refuse a reused run. Inject the damaged two-lease state
    // directly so this test reaches the intent helper's ambiguity guard.
    tx.execute_batch("INSERT INTO leases(project_id,task_id,run_id,agent_id,worker_id,epoch,lineage,state,granted_at,renewed_at,expires_at) SELECT project_id,'second',run_id,'b',worker_id,epoch,lineage,state,granted_at,renewed_at,expires_at FROM leases WHERE task_id='t'").unwrap();
    let mut intents = Intents::new(&tx, &who);
    for payload in [
        IntentPayload::ProcessStart { run: "r".into() },
        IntentPayload::ProcessStop { run: "r".into() },
    ] {
        assert!(matches!(
            intents.add_intent(&payload, 0),
            Err(SqliteError::Store(StoreError::StaleEpoch))
        ));
    }
    assert!(intents.list_pending_intents(0, 10).unwrap().is_empty());
    assert_eq!(intents.add_intent(&remove("t"), 1).unwrap().id, 1);
    tx.commit().unwrap();
}

#[test]
fn nonpositive_stored_ids_refuse_allocation_without_changing_rows_or_counter() {
    for id in [0, -1] {
        let mut conn = database();
        let tx = conn.transaction().unwrap();
        let who = caller("p");
        let mut intents = Intents::new(&tx, &who);
        intents.add_intent(&remove("t"), 0).unwrap();
        tx.execute("UPDATE intents SET id=?1", [id]).unwrap();
        assert!(matches!(
            intents.add_intent(&remove("second"), 1),
            Err(SqliteError::IntentCorrupt("nonpositive stored id"))
        ));
        assert_eq!(
            tx.query_row("SELECT id FROM intents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            id
        );
        assert_eq!(
            tx.query_row(
                "SELECT value FROM counters WHERE project_id='p' AND name='intent'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        tx.commit().unwrap();
    }
}

#[test]
fn missing_lineage_refuses_process_intents_with_the_sql_error_and_no_writes() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let who = caller("p");
    claim(&tx, &who);
    tx.execute("DELETE FROM daemon_config WHERE key='lineage'", [])
        .unwrap();
    let mut intents = Intents::new(&tx, &who);
    for payload in [
        IntentPayload::ProcessStart { run: "r".into() },
        IntentPayload::ProcessStop { run: "r".into() },
    ] {
        assert!(matches!(
            intents.add_intent(&payload, 0),
            Err(SqliteError::Sql(rusqlite::Error::QueryReturnedNoRows))
        ));
    }
    assert!(intents.list_pending_intents(0, 10).unwrap().is_empty());
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM counters WHERE project_id='p' AND name='intent'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(intents.add_intent(&remove("t"), 1).unwrap().id, 1);
    tx.commit().unwrap();
}

#[test]
fn aborted_transaction_diagnostic_applies_to_every_transaction_family() {
    assert_eq!(
        SqliteError::TransactionAborted.to_string(),
        "transaction cannot commit after a failed operation rollback"
    );
}
