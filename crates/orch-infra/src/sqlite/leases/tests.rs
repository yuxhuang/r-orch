use super::*;
use rusqlite::{Connection, TransactionBehavior};

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db
}
pub(super) fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
pub(super) fn new(task: &str, agent: &str, run: &str) -> NewLease {
    NewLease {
        task: task.into(),
        agent: agent.into(),
        run: run.into(),
    }
}
pub(super) fn grant(tx: &Transaction<'_>, who: &Caller, n: NewLease, now: i64) -> Lease {
    match claim_lease(tx, who, &n, now).unwrap() {
        ClaimOutcome::Granted(l) => l,
        other => panic!("{other:?}"),
    }
}
pub(super) fn holder(who: &Caller, l: &Lease) -> Caller {
    Caller {
        project: who.project.clone(),
        principal: Principal::Task {
            identity: "synthetic".into(),
            task: l.task.clone(),
            run: l.run.clone(),
            lineage: l.lineage.clone(),
            epoch: l.epoch,
        },
    }
}

#[test]
fn claims_use_schema_constants_and_task_agent_run_precedence_without_writes() {
    let mut db = db();
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let who = owner("p");
    let a = grant(&tx, &who, new("a", "x", "r1"), 10);
    let b = grant(&tx, &who, new("b", "y", "r2"), 20);
    assert_eq!(a.epoch, 1);
    assert_eq!(a.expires_at, LEASE_MS + 10);
    let before = tx.total_changes();
    for (n, held) in [
        (new("a", "y", "r2"), &a),
        (new("c", "x", "r2"), &a),
        (new("c", "z", "r2"), &b),
    ] {
        assert_eq!(
            claim_lease(&tx, &who, &n, i64::MAX).unwrap(),
            ClaimOutcome::Held(held.clone())
        );
    }
    assert_eq!(tx.total_changes(), before);
    assert_eq!(list_leases(&tx, &who).unwrap(), vec![a.clone(), b]);
    assert_eq!(get_agent_lease(&tx, &who, "x").unwrap(), Some(a));
    let constants: (String, String, i64, i64) = tx
        .query_row(
            "SELECT worker_id,state,granted_at,renewed_at FROM leases WHERE task_id='a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(constants, ("local_worker".into(), "active".into(), 10, 10));
}

#[test]
fn past_deadline_is_held_and_renewable_using_only_the_supplied_clock() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let who = owner("p");
    let l = grant(&tx, &who, new("a", "x", "r"), -10);
    assert_eq!(
        claim_lease(&tx, &who, &new("a", "z", "q"), LEASE_MS * 10).unwrap(),
        ClaimOutcome::Held(l.clone())
    );
    let renewed = renew_lease(&tx, &holder(&who, &l), "a", &l.lineage, 1, LEASE_MS * 10).unwrap();
    assert_eq!(renewed.expires_at, LEASE_MS * 11);
}

#[test]
fn stale_tokens_and_each_holder_field_are_refused_without_writes() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let who = owner("p");
    let l = grant(&tx, &who, new("a", "x", "r"), 0);
    let mut callers = vec![owner("other")];
    for bad in [
        Lease {
            task: "other".into(),
            ..l.clone()
        },
        Lease {
            run: "other".into(),
            ..l.clone()
        },
        Lease {
            lineage: "other".into(),
            ..l.clone()
        },
        Lease {
            epoch: 2,
            ..l.clone()
        },
    ] {
        callers.push(holder(&who, &bad));
    }
    let before = tx.total_changes();
    for c in callers {
        assert!(matches!(
            renew_lease(&tx, &c, "a", &l.lineage, 1, 7),
            Err(LeaseError::Store(StoreError::StaleEpoch))
        ));
        assert!(matches!(
            release_lease(&tx, &c, "a", &l.lineage, 1),
            Err(LeaseError::Store(StoreError::StaleEpoch))
        ));
    }
    for (lineage, epoch) in [("wrong", 1), (&*l.lineage, 0), (&*l.lineage, u64::MAX)] {
        assert!(matches!(
            renew_lease(&tx, &who, "a", lineage, epoch, 7),
            Err(LeaseError::Store(StoreError::StaleEpoch))
        ));
        assert!(matches!(
            release_lease(&tx, &who, "a", lineage, epoch),
            Err(LeaseError::Store(StoreError::StaleEpoch))
        ));
    }
    assert_eq!(tx.total_changes(), before);
    assert_eq!(get_lease(&tx, &who, "a").unwrap(), Some(l));
}

#[test]
fn current_database_lineage_fences_even_a_matching_old_holder() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let who = owner("p");
    let l = grant(&tx, &who, new("a", "x", "r"), 0);
    tx.execute(
        "UPDATE daemon_config SET value='new' WHERE key='lineage'",
        [],
    )
    .unwrap();
    let before = tx.total_changes();
    assert!(matches!(
        renew_lease(&tx, &holder(&who, &l), "a", &l.lineage, 1, 7),
        Err(LeaseError::Store(StoreError::StaleEpoch))
    ));
    assert!(matches!(
        release_lease(&tx, &who, "a", &l.lineage, 1),
        Err(LeaseError::Store(StoreError::StaleEpoch))
    ));
    assert_eq!(tx.total_changes(), before);
}

#[test]
fn projects_have_independent_slots_counters_and_ordered_reads() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let a = grant(&tx, &owner("a"), new("t", "x", "r"), 0);
    assert!(list_leases(&tx, &owner("b")).unwrap().is_empty());
    assert!(get_agent_lease(&tx, &owner("b"), "x").unwrap().is_none());
    let b = grant(&tx, &owner("b"), new("t", "x", "r"), 0);
    assert_eq!(a, b);
    grant(&tx, &owner("a"), new("a", "y", "q"), 0);
    assert_eq!(
        list_leases(&tx, &owner("a"))
            .unwrap()
            .iter()
            .map(|l| l.task.as_str())
            .collect::<Vec<_>>(),
        ["a", "t"]
    );
}

#[test]
fn release_retains_epoch_and_lost_streak_across_reopen_without_task_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("leases.db");
    let who = owner("p");
    {
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        let tx = db.transaction().unwrap();
        let l = grant(&tx, &who, new("t", "a", "r1"), 0);
        tx.execute("UPDATE task_epochs SET lost_streak=4", [])
            .unwrap();
        assert_eq!(
            release_lease(&tx, &holder(&who, &l), "t", &l.lineage, 1).unwrap(),
            l
        );
        assert!(matches!(
            release_lease(&tx, &who, "t", &l.lineage, 1),
            Err(LeaseError::Store(StoreError::StaleEpoch))
        ));
        tx.commit().unwrap();
    }
    let mut db = Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    let l = grant(&tx, &who, new("t", "a", "r2"), 0);
    assert_eq!(l.epoch, 2);
    let counters: (i64, i64) = tx
        .query_row("SELECT next_epoch,lost_streak FROM task_epochs", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(counters, (3, 4));
}

#[test]
fn outer_rollback_undoes_claim_epoch_renew_and_release() {
    let mut db = db();
    let who = owner("p");
    {
        let tx = db.transaction().unwrap();
        grant(&tx, &who, new("t", "a", "r"), 0);
    }
    let l;
    {
        let tx = db.transaction().unwrap();
        l = grant(&tx, &who, new("t", "a", "r"), 0);
        assert_eq!(l.epoch, 1);
        tx.commit().unwrap();
    }
    {
        let tx = db.transaction().unwrap();
        renew_lease(&tx, &who, "t", &l.lineage, 1, 50).unwrap();
        release_lease(&tx, &who, "t", &l.lineage, 1).unwrap();
    }
    let tx = db.transaction().unwrap();
    assert_eq!(get_lease(&tx, &who, "t").unwrap(), Some(l));
}

#[test]
fn clock_and_sqlite_counter_overflow_change_nothing() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let who = owner("p");
    let before = tx.total_changes();
    assert!(matches!(
        claim_lease(&tx, &who, &new("t", "a", "r"), i64::MAX),
        Err(LeaseError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(tx.total_changes(), before);
    let l = grant(&tx, &who, new("t", "a", "r"), 0);
    let before = tx.total_changes();
    assert!(matches!(
        renew_lease(&tx, &who, "t", &l.lineage, 1, i64::MAX),
        Err(LeaseError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(tx.total_changes(), before);
    release_lease(&tx, &who, "t", &l.lineage, 1).unwrap();
    tx.execute("UPDATE task_epochs SET next_epoch=?1", [i64::MAX])
        .unwrap();
    let before = tx.total_changes();
    assert!(matches!(
        claim_lease(&tx, &who, &new("t", "a", "r2"), 0),
        Err(LeaseError::Store(StoreError::CounterExhausted))
    ));
    assert_eq!(tx.total_changes(), before);
}

#[test]
fn failed_insert_rolls_back_counter_even_when_caller_commits() {
    let mut db = db();
    let tx = db.transaction().unwrap();
    let who = owner("p");
    tx.execute_batch("CREATE TRIGGER fail_lease BEFORE INSERT ON leases BEGIN SELECT RAISE(ABORT, 'synthetic'); END").unwrap();
    assert!(claim_lease(&tx, &who, &new("t", "a", "r"), 0).is_err());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM task_epochs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    tx.execute_batch("DROP TRIGGER fail_lease").unwrap();
    assert_eq!(grant(&tx, &who, new("t", "a", "r"), 0).epoch, 1);
}

#[test]
fn a_borrowed_read_transaction_keeps_its_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.db");
    let mut writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.execute_batch(include_str!("../schema.sql")).unwrap();
    let mut reader = Connection::open(&path).unwrap();
    let who = owner("p");
    let snapshot = reader.transaction().unwrap();
    assert!(list_leases(&snapshot, &who).unwrap().is_empty());
    let tx = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let l = grant(&tx, &who, new("t", "a", "r"), 0);
    tx.commit().unwrap();
    assert!(get_lease(&snapshot, &who, "t").unwrap().is_none());
    snapshot.commit().unwrap();
    let snapshot = reader.transaction().unwrap();
    assert_eq!(get_lease(&snapshot, &who, "t").unwrap(), Some(l));
}

#[test]
fn serialized_sqlite_claims_have_one_winner_for_task_agent_and_run() {
    for mode in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(include_str!("../schema.sql"))
            .unwrap();
        let barrier = std::sync::Barrier::new(3);
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..3)
                .map(|i| {
                    let path = &path;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let mut db = Connection::open(path).unwrap();
                        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                        barrier.wait();
                        let tx = db
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .unwrap();
                        let n = new(
                            &format!("t{}", if mode == 0 { 0 } else { i }),
                            &format!("a{}", if mode == 1 { 0 } else { i }),
                            &format!("r{}", if mode == 2 { 0 } else { i }),
                        );
                        let result = claim_lease(&tx, &owner("p"), &n, 0).unwrap();
                        tx.commit().unwrap();
                        result
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        let winners: Vec<_> = outcomes
            .iter()
            .filter_map(|o| match o {
                ClaimOutcome::Granted(l) => Some(l),
                _ => None,
            })
            .collect();
        assert_eq!(winners.len(), 1);
        for o in &outcomes {
            if let ClaimOutcome::Held(l) = o {
                assert_eq!(l, winners[0]);
            }
        }
    }
}

fn denies(db: &mut Connection, table: &'static str, update: bool, delete: bool, read: bool) {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    db.authorizer(Some(move |context: AuthContext<'_>| match context.action {
        AuthAction::Read { table_name, .. } if read && table_name == table => Authorization::Deny,
        AuthAction::Update { table_name, .. } if update && table_name == table => {
            Authorization::Deny
        }
        AuthAction::Delete { table_name } if delete && table_name == table => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
}

fn site<'a>(
    db: &'a mut Connection,
    (table, update, delete, read): (&'static str, bool, bool, bool),
) -> Transaction<'a> {
    denies(db, table, update, delete, read);
    db.transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap()
}

fn sql_code(error: LeaseError) -> rusqlite::ffi::ErrorCode {
    match error {
        LeaseError::Sql(error) => error
            .sqlite_error_code()
            .unwrap_or_else(|| panic!("SQLite error lost its code: {error:?}")),
        LeaseError::Store(error) => panic!("SQL failure became a typed refusal: {error:?}"),
    }
}

#[test]
fn sql_failures_keep_their_sqlite_code_at_every_lease_site() {
    use rusqlite::ffi::ErrorCode::AuthorizationForStatementDenied as Denied;
    let mut db = db();
    let who = owner("p");
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let lease = grant(&tx, &who, new("t", "a", "r"), 0);
    tx.commit().unwrap();
    // Each site is read or written through a transaction that SQLite refuses
    // part of; the structured code must survive instead of a backend string.
    let tx = site(&mut db, ("leases", false, false, true));
    assert_eq!(sql_code(get_lease(&tx, &who, "t").unwrap_err()), Denied);
    assert_eq!(
        sql_code(get_agent_lease(&tx, &who, "a").unwrap_err()),
        Denied
    );
    assert_eq!(sql_code(list_leases(&tx, &who).unwrap_err()), Denied);
    drop(tx);
    // Pre-savepoint claim read: the task and agent lookups pass, the epoch read fails.
    let tx = site(&mut db, ("task_epochs", false, false, true));
    assert_eq!(
        sql_code(claim_lease(&tx, &who, &new("u", "b", "s"), 0).unwrap_err()),
        Denied
    );
    drop(tx);
    let tx = site(&mut db, ("daemon_config", false, false, true));
    assert_eq!(
        sql_code(claim_lease(&tx, &who, &new("u", "b", "s"), 0).unwrap_err()),
        Denied
    );
    assert_eq!(
        sql_code(renew_lease(&tx, &who, "t", &lease.lineage, 1, 5).unwrap_err()),
        Denied
    );
    drop(tx);
    let tx = site(&mut db, ("leases", true, false, false));
    assert_eq!(
        sql_code(renew_lease(&tx, &who, "t", &lease.lineage, 1, 5).unwrap_err()),
        Denied
    );
    drop(tx);
    let tx = site(&mut db, ("leases", false, true, false));
    assert_eq!(
        sql_code(release_lease(&tx, &who, "t", &lease.lineage, 1).unwrap_err()),
        Denied
    );
    drop(tx);
    // Typed refusals stay refusals and the conversion keeps both classes apart.
    use crate::sqlite::SqliteError;
    assert!(matches!(
        SqliteError::from(LeaseError::Store(StoreError::StaleEpoch)),
        SqliteError::Store(StoreError::StaleEpoch)
    ));
    let sql = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(11), None);
    assert!(matches!(
        SqliteError::from(LeaseError::Sql(sql)),
        SqliteError::Sql(error) if error.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::DatabaseCorrupt)
    ));
}
