//! Issue #203: exact-TTL replacement through the public composed Storage port.
use orch_core::store::{
    Caller, Lane, Principal, StorageExt,
    request_keys::{KEY_TTL_MS, KeyCheck, MAX_RESULT_BYTES, RequestScope},
};
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
};
use rusqlite::{Connection, OpenFlags, params};
use std::sync::Arc;

fn caller() -> Caller {
    Caller {
        project: "synthetic".into(),
        principal: Principal::Owner {
            identity: "test-owner".into(),
        },
    }
}

#[tokio::test]
async fn exact_ttl_replacement_refreshes_one_row_and_replays_after_each_storage_reopen() {
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
    let key = "request_key_0203";
    let op = "task.update";
    let hashes = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
    let oversized = "x".repeat(MAX_RESULT_BYTES + 1);

    for (stage, result) in [oversized.as_str(), "small λ", oversized.as_str()]
        .into_iter()
        .enumerate()
    {
        let now = stage as i64 * KEY_TTL_MS;
        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let hash = hashes[stage].clone();
        let result_owned = result.to_owned();
        storage
            .transaction(caller(), Lane::Entities, move |u| {
                let scope = RequestScope::of(&caller(), key)?;
                assert_eq!(
                    u.check_request_key(&scope, op, &hash, now)?,
                    KeyCheck::New,
                    "stage {stage}: exact expiry permits replacement without maintenance"
                );
                u.store_request_key(&scope, op, &hash, &result_owned, now)
            })
            .await
            .unwrap();
        storage.close().await.unwrap();
        drop(storage);

        // Observe durable rows independently of the port's replay implementation.
        let conn =
            Connection::open_with_flags(data.database_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let predicate = params!["test-owner", "synthetic", "", 0_i64, key];
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM request_keys WHERE identity_id=?1 AND project_id=?2 AND run_id=?3 AND epoch=?4 AND key=?5",
            predicate, |r| r.get(0),
        ).unwrap();
        assert_eq!(
            count, 1,
            "stage {stage}: exactly one row for the whole scope"
        );
        let row = conn.query_row(
            "SELECT op,req_sha256,result,result_truncated,created_at,expires_at,state FROM request_keys WHERE identity_id=?1 AND project_id=?2 AND run_id=?3 AND epoch=?4 AND key=?5",
            predicate,
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, String>(6)?)),
        ).unwrap();
        assert_eq!(
            row,
            (
                op.into(),
                hashes[stage].clone(),
                (stage == 1).then(|| result.to_owned()),
                i64::from(stage != 1),
                now,
                now + KEY_TTL_MS,
                "done".into()
            ),
            "stage {stage}: hash, result size, state and lifetime persist together"
        );
        drop(conn);

        let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
        let hashes = hashes.clone();
        let expected = if stage == 1 {
            KeyCheck::Replay(result.into())
        } else {
            KeyCheck::ReplayWithoutResult
        };
        storage
            .transaction(caller(), Lane::Entities, move |u| {
                let scope = RequestScope::of(&caller(), key)?;
                for time in [now, now + KEY_TTL_MS - 1] {
                    assert_eq!(
                        u.check_request_key(&scope, op, &hashes[stage], time)?,
                        expected,
                        "stage {stage}: committed result replays through the reopened Storage"
                    );
                }
                for stale_hash in &hashes[..stage] {
                    assert_eq!(
                        u.check_request_key(&scope, op, stale_hash, now)?,
                        KeyCheck::Mismatch,
                        "stage {stage}: superseded hashes cannot replay"
                    );
                }
                assert_eq!(
                    u.check_request_key(&scope, op, &hashes[stage], now + KEY_TTL_MS)?,
                    KeyCheck::New,
                    "stage {stage}: the exact TTL is expired"
                );
                Ok(())
            })
            .await
            .unwrap();
        storage.close().await.unwrap();
        drop(storage);
    }
}
