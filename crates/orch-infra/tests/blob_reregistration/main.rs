//! Synthetic preconditions for issue 135, not a blob.delete performer test.
//! T15 must reuse this committed gap and invoke the real performer after registration.
//! No sweep, deletion, completion, backup or recovery success is simulated here.
use orch_core::store::intents::IntentPayload;
use orch_infra::blob::{BlobHash, BlobReferences, DiskBlobStore};
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use orch_infra::sqlite::{
    ConnectionRole, Lane, SqliteError, Synchronous, Writer, migrations::migrate, open_connection,
};
use rusqlite::params;
use std::sync::mpsc;
use std::time::Duration;

const OLD: &str = "old-project";
const NEW: &str = "new-project";
const CREATED: i64 = 1_000;
const ELIGIBLE: i64 = CREATED + 7 * 24 * 60 * 60 * 1_000;

struct Fixture {
    // Keep the lock and disk handles alive until after every connection closes.
    disk: DiskBlobStore,
    data: DataDir,
    _temp: tempfile::TempDir,
    bytes: Vec<u8>,
    hash: BlobHash,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data = DataDir::acquire(
            &temp.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "test".into(),
            },
        )
        .unwrap();
        let disk = DiskBlobStore::open(data.root()).unwrap();
        // Non-text, more than the inline threshold; compare every byte, not a prefix.
        let bytes: Vec<u8> = (0..131_073).map(|i| (i % 251) as u8).collect();
        let hash = BlobHash::of(&bytes);
        let f = Self {
            disk,
            data,
            _temp: temp,
            bytes,
            hash,
        };
        let mut conn =
            open_connection(&f.data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrate(&f.data, &mut conn).unwrap();
        for project in [OLD, NEW] {
            conn.execute("INSERT INTO tasks (project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision) VALUES (?1,'task','synthetic','','todo',1,0,3,1,'owner',0,0,1)", [project]).unwrap();
        }
        drop(conn);
        f.register(OLD, true);
        f
    }

    fn register(&self, project: &str, commit: bool) {
        // Public BlobReferences requires &mut Transaction; Writer::submit offers
        // &Transaction only. Use the real helper on the sole writing connection,
        // with no executor running. T15 integration still owes that API seam.
        let mut conn =
            open_connection(&self.data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        let mut tx = conn.transaction().unwrap();
        let hash = BlobReferences::new(&self.disk, project)
            .store_attachment(
                &mut tx,
                "task",
                "payload",
                &self.bytes,
                if project == OLD {
                    CREATED
                } else {
                    ELIGIBLE + 1
                },
            )
            .unwrap();
        assert_eq!(hash, self.hash);
        if commit {
            tx.commit().unwrap();
        } else {
            tx.rollback().unwrap();
        }
    }

    fn remove_last_registration_and_seed_pending_intent(&self, commit: bool) {
        let hash = self.hash.to_string();
        let payload = IntentPayload::BlobDelete { hash: hash.clone() };
        payload.validate().unwrap();
        let payload = serde_json::to_string(&payload).unwrap();
        let (writer, handle) = Writer::open(&self.data, Synchronous::Normal, 4).unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| writer.run());
            struct Close<'a>(&'a orch_infra::sqlite::WriterHandle);
            impl Drop for Close<'_> {
                fn drop(&mut self) {
                    self.0.close();
                }
            }
            let _close = Close(&handle);
            let receipt = handle.submit(Lane::Maintenance, None, move |tx| {
                tx.execute("DELETE FROM task_attachments WHERE project_id=?1 AND sha256=?2", params![OLD, hash])?;
                let changed = tx.execute("DELETE FROM blobs WHERE project_id=?1 AND sha256=?2", params![OLD, hash])?;
                assert_eq!(changed, 1, "remove the last committed registration");
                // Intents is private and has no public writer facade. This is
                // schema-valid fixture setup, not a claim that T15 enqueued it.
                tx.execute("INSERT INTO counters(project_id,name,value) VALUES (?1,'intent',1)", [OLD])?;
                tx.execute("INSERT INTO intents(project_id,id,kind,payload,created_at) VALUES (?1,1,'blob.delete',?2,?3)", params![OLD, payload, ELIGIBLE])?;
                if commit { Ok(()) } else { Err(SqliteError::TransactionAborted) }
            }).unwrap();
            let (send, receive) = mpsc::channel();
            let waiter = scope.spawn(move || {
                let _ = send.send(receipt.wait());
            });
            let result = receive
                .recv_timeout(Duration::from_secs(20))
                .expect("writer receipt");
            assert_eq!(result.is_ok(), commit);
            if !commit {
                assert!(matches!(result, Err(SqliteError::TransactionAborted)));
            }
            handle.close();
            waiter.join().unwrap();
            worker.join().unwrap();
        });
    }

    fn registrations(&self) -> Vec<(String, i64, Option<i64>)> {
        let conn =
            open_connection(&self.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        conn.prepare("SELECT project_id,size,unreferenced_since FROM blobs WHERE sha256=?1 ORDER BY project_id")
            .unwrap().query_map([self.hash.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    fn assert_pending(&self, expected: bool) {
        let conn =
            open_connection(&self.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        let rows: Vec<(String, String, Option<i64>)> = conn
            .prepare("SELECT project_id,payload,done_at FROM intents ORDER BY project_id,id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let payload = IntentPayload::BlobDelete {
            hash: self.hash.to_string(),
        };
        assert_eq!(
            rows,
            if expected {
                vec![(OLD.into(), serde_json::to_string(&payload).unwrap(), None)]
            } else {
                vec![]
            }
        );
        let counter: i64 = conn
            .query_row(
                "SELECT count(*) FROM counters WHERE name='intent'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(counter, i64::from(expected));
    }

    fn assert_readable(&self, project: &str) {
        let conn =
            open_connection(&self.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        assert_eq!(
            BlobReferences::new(&self.disk, project)
                .load_attachment(&conn, "task", "payload")
                .unwrap(),
            Some(self.bytes.clone())
        );
        assert_eq!(self.disk.get(&self.hash).unwrap(), self.bytes);
    }

    fn committed_gap(&self) {
        self.remove_last_registration_and_seed_pending_intent(true);
        assert!(
            self.registrations().is_empty(),
            "zero registrations across all projects before re-registration"
        );
        self.assert_pending(true);
        assert_eq!(self.disk.get(&self.hash).unwrap(), self.bytes);
    }
}

#[test]
fn another_project_reregisters_the_same_hash_before_pending_intent_performance() {
    let f = Fixture::new();
    f.committed_gap();
    f.register(NEW, true);
    assert_eq!(
        f.registrations(),
        vec![(NEW.into(), f.bytes.len() as i64, None)]
    );
    f.assert_readable(NEW);
    f.assert_pending(true); // deliberately unperformed and unfinished
}

#[test]
fn rollback_of_last_registration_removal_restores_reference_and_removes_intent() {
    let f = Fixture::new();
    f.remove_last_registration_and_seed_pending_intent(false);
    assert_eq!(
        f.registrations(),
        vec![(OLD.into(), f.bytes.len() as i64, None)]
    );
    f.assert_readable(OLD);
    f.assert_pending(false);
}

#[test]
fn rolled_back_cross_project_registration_keeps_whole_orphan_bytes_for_retry() {
    let f = Fixture::new();
    f.committed_gap();
    f.register(NEW, false);
    assert!(f.registrations().is_empty());
    let conn = open_connection(&f.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(
        BlobReferences::new(&f.disk, NEW)
            .load_attachment(&conn, "task", "payload")
            .unwrap(),
        None
    );
    drop(conn);
    assert_eq!(f.disk.get(&f.hash).unwrap(), f.bytes);
    f.assert_pending(true);
    f.register(NEW, true);
    f.assert_readable(NEW);
    assert_eq!(
        f.registrations(),
        vec![(NEW.into(), f.bytes.len() as i64, None)]
    );
    f.assert_pending(true);
}

// These are registration preconditions only. Marks and imported timestamps are
// schema-valid synthetic setup, not evidence that a sweep or importer ran.
// At registration time ELIGIBLE + 1, each mark is over seven days old.
impl Fixture {
    fn mark_both_projects(&self, created: i64) {
        self.register(NEW, true);
        let conn =
            open_connection(&self.data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        conn.execute("DELETE FROM task_attachments", []).unwrap();
        conn.execute(
            "UPDATE blobs SET created_at=?1,unreferenced_since=?2 WHERE sha256=?3",
            params![created, CREATED, self.hash.to_string()],
        )
        .unwrap();
    }

    fn eligibility_metadata(&self) -> Vec<(String, i64, Option<i64>)> {
        let conn =
            open_connection(&self.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
        conn.prepare("SELECT project_id,created_at,unreferenced_since FROM blobs WHERE sha256=?1 ORDER BY project_id")
            .unwrap().query_map([self.hash.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }
}

fn committed_reuse_preserves_creation_and_clears_only_selected_mark(created: i64) {
    let f = Fixture::new();
    f.mark_both_projects(created);
    assert_eq!(
        f.eligibility_metadata(),
        vec![
            (NEW.into(), created, Some(CREATED)),
            (OLD.into(), created, Some(CREATED)),
        ]
    );
    // The same bytes already have matching rows in two projects. Reuse the
    // selected project's row through the public API, not the row-only helper.
    f.register(NEW, true);
    assert_eq!(
        f.eligibility_metadata(),
        vec![
            (NEW.into(), created, None),
            (OLD.into(), created, Some(CREATED)),
        ]
    );
    f.assert_readable(NEW);
}

#[test]
fn matching_cross_project_reregistration_preserves_creation_and_clears_only_selected_mark() {
    committed_reuse_preserves_creation_and_clears_only_selected_mark(CREATED);
}

#[test]
fn matching_cross_project_reregistration_preserves_old_imported_creation_and_clears_mark() {
    // 1900-01-01 UTC: predates the epoch and is much older than the mark.
    committed_reuse_preserves_creation_and_clears_only_selected_mark(-2_208_988_800_000);
}

fn rolled_back_reuse_restores_eligibility(created: i64) {
    let f = Fixture::new();
    f.mark_both_projects(created);
    let before = f.eligibility_metadata();
    let mut conn = open_connection(&f.data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    let mut tx = conn.transaction().unwrap();
    BlobReferences::new(&f.disk, NEW)
        .store_attachment(&mut tx, "task", "payload", &f.bytes, ELIGIBLE + 1)
        .unwrap();
    // Prove the change happened before rolling it back; otherwise an API that
    // silently stopped clearing marks would give a vacuous rollback pass.
    assert_eq!(
        tx.query_row(
            "SELECT created_at,unreferenced_since FROM blobs WHERE project_id=?1 AND sha256=?2",
            params![NEW, f.hash.to_string()],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .unwrap(),
        (created, None)
    );
    tx.rollback().unwrap();
    drop(conn);
    assert_eq!(f.eligibility_metadata(), before);
    let conn = open_connection(&f.data, ConnectionRole::Reader, Synchronous::Normal).unwrap();
    assert_eq!(
        BlobReferences::new(&f.disk, NEW)
            .load_attachment(&conn, "task", "payload")
            .unwrap(),
        None
    );
}

#[test]
fn rolled_back_matching_cross_project_reregistration_restores_sweep_mark() {
    rolled_back_reuse_restores_eligibility(CREATED);
}

#[test]
fn rolled_back_matching_cross_project_reregistration_restores_old_imported_metadata() {
    rolled_back_reuse_restores_eligibility(-2_208_988_800_000);
}
