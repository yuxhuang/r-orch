use super::*;
use crate::sqlite::{
    ConnectionRole, Lane, Synchronous, Writer, migrations, open_connection, test_data,
};
use orch_core::store::Principal;
use orch_core::store::request_keys::KEY_TTL_MS;
use rusqlite::Connection;

const KEY: &str = "0123456789abcdef";
const OP: &str = "task.update";
fn hash() -> String {
    "a".repeat(64)
}
fn caller(project: &str, identity: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: identity.into(),
        },
    }
}
fn task(project: &str, identity: &str, run: &str, epoch: u64) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Task {
            identity: identity.into(),
            task: "task".into(),
            run: run.into(),
            lineage: "lineage".into(),
            epoch,
        },
    }
}
fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    conn
}
fn scope(caller: &Caller) -> RequestScope {
    RequestScope::of(caller, KEY).unwrap()
}

#[test]
fn replay_sees_own_writes_preserves_empty_and_detects_both_mismatches() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let caller = caller("p", "owner");
    let mut keys = RequestKeys::new(&tx, &caller);
    let scope = scope(&caller);
    assert_eq!(
        keys.check_request_key(&scope, OP, &hash(), 0).unwrap(),
        KeyCheck::New
    );
    keys.store_request_key(&scope, OP, &hash(), "", 0).unwrap();
    assert_eq!(
        keys.check_request_key(&scope, OP, &hash(), 1).unwrap(),
        KeyCheck::Replay("".into())
    );
    assert_eq!(
        keys.check_request_key(&scope, "other", &hash(), 1).unwrap(),
        KeyCheck::Mismatch
    );
    assert_eq!(
        keys.check_request_key(&scope, OP, &"b".repeat(64), 1)
            .unwrap(),
        KeyCheck::Mismatch
    );
    tx.commit().unwrap();
    assert_eq!(
        conn.query_row("SELECT state,commit_seq FROM request_keys", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?
        )))
        .unwrap(),
        ("done".into(), None)
    );
}

#[test]
fn caught_live_overwrite_refusal_keeps_first_result_and_expiry() {
    let mut conn = database();
    let caller = caller("p", "owner");
    let tx = conn.transaction().unwrap();
    let mut keys = RequestKeys::new(&tx, &caller);
    let scope = scope(&caller);
    keys.store_request_key(&scope, OP, &hash(), "first", 0)
        .unwrap();
    assert!(matches!(
        keys.store_request_key(&scope, "other", &"b".repeat(64), "second", 1),
        Err(StoreError::InvalidRequestKey(_))
    ));
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    let keys = RequestKeys::new(&tx, &caller);
    assert_eq!(
        keys.check_request_key(&scope, OP, &hash(), KEY_TTL_MS - 1)
            .unwrap(),
        KeyCheck::Replay("first".into())
    );
    assert_eq!(
        keys.check_request_key(&scope, OP, &hash(), KEY_TTL_MS)
            .unwrap(),
        KeyCheck::New
    );
}

#[test]
fn exact_expiry_replaces_without_maintenance_and_restarts_the_ttl() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let caller = caller("p", "owner");
    let mut keys = RequestKeys::new(&tx, &caller);
    let scope = scope(&caller);
    keys.store_request_key(&scope, OP, &hash(), "old", 0)
        .unwrap();
    keys.store_request_key(&scope, "new", &hash(), "new", KEY_TTL_MS)
        .unwrap();
    assert_eq!(
        keys.check_request_key(&scope, "new", &hash(), 2 * KEY_TTL_MS - 1)
            .unwrap(),
        KeyCheck::Replay("new".into())
    );
    assert_eq!(
        keys.check_request_key(&scope, "new", &hash(), 2 * KEY_TTL_MS)
            .unwrap(),
        KeyCheck::New
    );
}

#[test]
fn byte_boundary_keeps_whole_utf8_or_no_prefix() {
    let mut conn = database();
    let caller = caller("p", "owner");
    let tx = conn.transaction().unwrap();
    let mut keys = RequestKeys::new(&tx, &caller);
    for (n, result) in [
        "λ".repeat(MAX_RESULT_BYTES / 2),
        "λ".repeat(MAX_RESULT_BYTES / 2) + "x",
    ]
    .iter()
    .enumerate()
    {
        let scope = RequestScope::of(&caller, &format!("{n:016}")).unwrap();
        keys.store_request_key(&scope, OP, &hash(), result, 0)
            .unwrap();
        let expected = if n == 0 {
            KeyCheck::Replay(result.clone())
        } else {
            KeyCheck::ReplayWithoutResult
        };
        assert_eq!(
            keys.check_request_key(&scope, OP, &hash(), 1).unwrap(),
            expected
        );
    }
    assert_eq!(
        tx.query_row(
            "SELECT result,result_truncated FROM request_keys WHERE key='0000000000000001'",
            [],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (None, 1)
    );
}

#[test]
fn identity_project_run_epoch_and_key_are_isolated_and_foreign_scopes_refused() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let callers = [
        caller("p", "owner"),
        caller("p", "other"),
        caller("q", "owner"),
        task("p", "owner", "run1", 1),
        task("p", "owner", "run2", 1),
        task("p", "owner", "run1", 2),
        task("p", "owner", "run1", i64::MAX as u64),
    ];
    for (n, caller) in callers.iter().enumerate() {
        let mut keys = RequestKeys::new(&tx, caller);
        assert_eq!(
            keys.check_request_key(&scope(caller), OP, &hash(), 0)
                .unwrap(),
            KeyCheck::New
        );
        keys.store_request_key(&scope(caller), OP, &hash(), &n.to_string(), 0)
            .unwrap();
    }
    for (n, caller) in callers.iter().enumerate() {
        let mut keys = RequestKeys::new(&tx, caller);
        assert_eq!(
            keys.check_request_key(&scope(caller), OP, &hash(), 0)
                .unwrap(),
            KeyCheck::Replay(n.to_string())
        );
        for foreign in callers.iter().filter(|foreign| *foreign != caller) {
            assert!(matches!(
                keys.check_request_key(&scope(foreign), OP, &hash(), 0),
                Err(StoreError::InvalidRequestKey(_))
            ));
            assert!(matches!(
                keys.store_request_key(&scope(foreign), OP, &hash(), "leak", KEY_TTL_MS),
                Err(StoreError::InvalidRequestKey(_))
            ));
        }
        let other = RequestScope::of(caller, "another_key_12345").unwrap();
        assert_eq!(
            keys.check_request_key(&other, OP, &hash(), 0).unwrap(),
            KeyCheck::New
        );
    }
    assert_eq!(
        tx.query_row("SELECT count(*) FROM request_keys", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        callers.len() as i64
    );
}

#[test]
fn invalid_inputs_and_overflow_leave_no_rows_when_caught() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let caller = caller("p", "owner");
    let mut keys = RequestKeys::new(&tx, &caller);
    let scope = scope(&caller);
    for (op, hash, now) in [
        ("", hash(), 0),
        ("task;delete", hash(), 0),
        (OP, "A".repeat(64), 0),
        (OP, hash(), -1),
    ] {
        assert!(matches!(
            keys.check_request_key(&scope, op, &hash, now),
            Err(StoreError::InvalidRequestKey(_))
        ));
        assert!(matches!(
            keys.store_request_key(&scope, op, &hash, "x", now),
            Err(StoreError::InvalidRequestKey(_))
        ));
    }
    assert!(matches!(
        keys.store_request_key(&scope, OP, &hash(), "x", i64::MAX),
        Err(StoreError::InvalidRequestKey(_))
    ));
    for bound in [0, 1001, usize::MAX] {
        assert!(matches!(
            keys.delete_expired_request_keys(0, bound),
            Err(StoreError::InvalidRequestKey(_))
        ));
    }
    assert!(matches!(
        keys.delete_expired_request_keys(-1, 1),
        Err(StoreError::InvalidRequestKey(_))
    ));
    tx.commit().unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM request_keys", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn expiry_is_bounded_project_local_and_ordered_by_expiry_then_full_scope() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    // Insert in reverse scope order. Earliest expiry takes precedence over identity.
    for (project, identity, run, epoch, time) in [
        ("q", "a", "run", 1, 0),
        ("p", "z", "run", 1, 0),
        ("p", "a", "z", 1, 1),
        ("p", "a", "a", 2, 1),
        ("p", "a", "a", 1, 1),
        ("p", "a", "a", 1, 2),
    ] {
        let caller = task(project, identity, run, epoch);
        let key = RequestScope::of(&caller, &format!("{time:016}")).unwrap();
        RequestKeys::new(&tx, &caller)
            .store_request_key(&key, OP, &hash(), "x", time)
            .unwrap();
    }
    let caller = caller("p", "maintenance");
    let mut keys = RequestKeys::new(&tx, &caller);
    assert_eq!(
        keys.delete_expired_request_keys(KEY_TTL_MS + 1, 2).unwrap(),
        2
    );
    let remaining: Vec<(String,String,i64)> = tx.prepare("SELECT identity_id,run_id,epoch FROM request_keys WHERE project_id='p' ORDER BY identity_id,run_id,epoch,key").unwrap().query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        remaining,
        vec![
            ("a".into(), "a".into(), 1),
            ("a".into(), "a".into(), 2),
            ("a".into(), "z".into(), 1)
        ]
    );
    assert_eq!(
        keys.delete_expired_request_keys(KEY_TTL_MS + 1, 1000)
            .unwrap(),
        2
    );
    assert_eq!(
        keys.delete_expired_request_keys(KEY_TTL_MS + 1, 1000)
            .unwrap(),
        0
    );
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM request_keys WHERE project_id='q'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        keys.delete_expired_request_keys(KEY_TTL_MS + 2, 1).unwrap(),
        1
    );
}

#[test]
fn rollback_undoes_keys_cleanup_and_the_guarded_change_together() {
    let mut conn = database();
    let caller = caller("p", "owner");
    {
        let tx = conn.transaction().unwrap();
        RequestKeys::new(&tx, &caller)
            .store_request_key(&scope(&caller), OP, &hash(), "x", 0)
            .unwrap();
        tx.execute(
            "INSERT INTO daemon_config(key,value,revision) VALUES('guarded','change',1)",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();
    }
    let tx = conn.transaction().unwrap();
    let mut keys = RequestKeys::new(&tx, &caller);
    assert_eq!(
        keys.check_request_key(&scope(&caller), OP, &hash(), 0)
            .unwrap(),
        KeyCheck::New
    );
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM daemon_config WHERE key='guarded'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    keys.store_request_key(&scope(&caller), OP, &hash(), "x", 0)
        .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        RequestKeys::new(&tx, &caller)
            .delete_expired_request_keys(KEY_TTL_MS, 1)
            .unwrap(),
        1
    );
    tx.rollback().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        RequestKeys::new(&tx, &caller)
            .check_request_key(&scope(&caller), OP, &hash(), 0)
            .unwrap(),
        KeyCheck::Replay("x".into())
    );
}

#[test]
fn sql_failure_caught_by_callback_does_not_replace_expired_row() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let caller = caller("p", "owner");
    let mut keys = RequestKeys::new(&tx, &caller);
    keys.store_request_key(&scope(&caller), OP, &hash(), "first", 0)
        .unwrap();
    tx.execute_batch("CREATE TRIGGER fail_key BEFORE UPDATE ON request_keys BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    assert!(matches!(
        keys.store_request_key(&scope(&caller), OP, &hash(), "second", KEY_TTL_MS),
        Err(StoreError::Backend(_))
    ));
    tx.commit().unwrap();
    assert_eq!(
        conn.query_row("SELECT result,expires_at FROM request_keys", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?
        )))
        .unwrap(),
        ("first".into(), KEY_TTL_MS)
    );
}

#[test]
fn impossible_stored_result_is_not_replayed() {
    for corruption in [
        "state='in_flight'",
        "result=NULL",
        "result_truncated=1",
        "result_truncated=2",
    ] {
        let mut conn = database();
        let tx = conn.transaction().unwrap();
        let caller = caller("p", "owner");
        let mut keys = RequestKeys::new(&tx, &caller);
        keys.store_request_key(&scope(&caller), OP, &hash(), "x", 0)
            .unwrap();
        tx.execute(&format!("UPDATE request_keys SET {corruption}"), [])
            .unwrap();
        assert!(matches!(
            keys.check_request_key(&scope(&caller), OP, &hash(), 0),
            Err(StoreError::Backend(_))
        ));
    }
}

#[test]
fn real_writer_serializes_repeats_and_reopen_keeps_the_first_committed_result() {
    let (_root, data) = test_data();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrations::migrate(&data, &mut conn).unwrap();
    drop(conn);
    let (writer, handle) = Writer::open(&data, Synchronous::Normal, 8).unwrap();
    struct Close<'a>(&'a crate::sqlite::WriterHandle);
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    std::thread::scope(|threads| {
        let _close = Close(&handle);
        threads.spawn(move || writer.run());
        let mut receipts = Vec::new();
        for _ in 0..2 {
            receipts.push(
                handle
                    .submit(Lane::Entities, None, |tx| {
                        let caller = caller("p", "owner");
                        let mut keys = RequestKeys::new(tx, &caller);
                        let scope = scope(&caller);
                        let found = keys.check_request_key(&scope, OP, &hash(), 0)?;
                        if found == KeyCheck::New {
                            tx.execute(
                                "INSERT INTO daemon_config(key,value,revision) VALUES('guarded','one',1)",
                                [],
                            )?;
                            keys.store_request_key(&scope, OP, &hash(), "committed λ", 0)?;
                        }
                        Ok(found)
                    })
                    .unwrap(),
            );
        }
        let mut receipts = receipts.into_iter();
        assert_eq!(receipts.next().unwrap().wait().unwrap(), KeyCheck::New);
        assert_eq!(
            receipts.next().unwrap().wait().unwrap(),
            KeyCheck::Replay("committed λ".into())
        );
    });
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let tx = conn.transaction().unwrap();
    let caller = caller("p", "owner");
    assert_eq!(
        RequestKeys::new(&tx, &caller)
            .check_request_key(&scope(&caller), OP, &hash(), 1)
            .unwrap(),
        KeyCheck::Replay("committed λ".into())
    );
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM daemon_config WHERE key='guarded'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
