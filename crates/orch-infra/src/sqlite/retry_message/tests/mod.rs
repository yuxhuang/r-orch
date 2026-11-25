//! Retry mapping on the real schema and the real disk blob store.
use std::cell::Cell;
use std::path::PathBuf;

use orch_core::store::retries::{MAX_DUE_RETRIES, Retry, RetryCursor};
use orch_core::store::{Caller, Principal, StoreError};
use rusqlite::{Connection, params};

use super::text::Objects;
use super::{Retries, RetryReader};
use crate::blob::{BlobError, BlobHash, DiskBlobStore};
use crate::sqlite::{SqliteError, migrations, test_data};

mod damage;
mod large;
mod order;

pub(super) struct Rig {
    pub(super) temp: tempfile::TempDir,
    pub(super) _data: crate::data_dir::DataDir,
    pub(super) blobs: DiskBlobStore,
    pub(super) conn: Connection,
}

pub(super) fn rig() -> Rig {
    let (temp, data) = test_data();
    let blobs = DiskBlobStore::open(data.root()).unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    Rig {
        temp,
        _data: data,
        blobs,
        conn,
    }
}

pub(super) fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner".into(),
        },
    }
}

pub(super) fn retry(task: &str, attempt: u32, due_at: i64, error: &str) -> Retry {
    Retry {
        task: task.into(),
        attempt,
        due_at,
        error: error.into(),
    }
}

impl Rig {
    /// One committed transaction of the given project.
    pub(super) fn commit<T>(
        &mut self,
        project: &str,
        work: impl FnOnce(&mut Retries<'_, '_>) -> T,
    ) -> T {
        let who = caller(project);
        let tx = self.conn.transaction().unwrap();
        let value = work(&mut Retries::new(&tx, &who, &self.blobs));
        tx.commit().unwrap();
        value
    }

    pub(super) fn put(&mut self, project: &str, retry: &Retry) {
        self.commit(project, |r| r.put_retry(retry, 7).unwrap());
    }

    pub(super) fn reader(&self, project: &'static str) -> RetryReader<'_> {
        RetryReader::new(&self.conn, project, &self.blobs)
    }

    pub(super) fn object_path(&self, hash: &str) -> PathBuf {
        self.temp
            .path()
            .join("data/blobs/sha256")
            .join(&hash[0..2])
            .join(&hash[2..4])
            .join(hash)
    }

    /// The names of the objects on disk, sorted.
    pub(super) fn objects(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut stack = vec![self.temp.path().join("data/blobs/sha256")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    stack.push(entry.path());
                } else {
                    names.push(entry.file_name().into_string().unwrap());
                }
            }
        }
        names.sort();
        names
    }

    pub(super) fn stored(&self, project: &str, task: &str) -> String {
        self.conn
            .query_row(
                "SELECT error FROM retry_queue WHERE project_id=?1 AND task_id=?2",
                params![project, task],
                |r| r.get(0),
            )
            .unwrap()
    }

    pub(super) fn set_stored(&self, project: &str, task: &str, error: &str) {
        let changed = self
            .conn
            .execute(
                "UPDATE retry_queue SET error=?3 WHERE project_id=?1 AND task_id=?2",
                params![project, task, error],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    pub(super) fn registrations(&self, project: &str) -> Vec<(String, i64)> {
        self.conn
            .prepare("SELECT sha256,size FROM blobs WHERE project_id=?1 ORDER BY sha256")
            .unwrap()
            .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}

pub(super) fn is_corrupt(result: Result<impl std::fmt::Debug, SqliteError>, detail: &str) {
    match result {
        Err(SqliteError::RetryMessageCorrupt(found)) => assert_eq!(found, detail),
        other => panic!("expected stored-data error {detail:?}, got {other:?}"),
    }
}

/// Fails publication after `puts_allowed` good publications, lies about what was
/// published, or cannot read; everything else is the real disk adapter.
pub(super) struct Faulty<'a> {
    pub(super) inner: &'a DiskBlobStore,
    pub(super) puts_allowed: Cell<usize>,
    pub(super) corrupt_reads: bool,
    pub(super) unreadable: bool,
}

impl Objects for Faulty<'_> {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash, BlobError> {
        if self.puts_allowed.get() == 0 {
            return Err(BlobError::Io(std::io::Error::other("injected publication")));
        }
        self.puts_allowed.set(self.puts_allowed.get() - 1);
        self.inner.put(bytes)
    }

    fn get(&self, hash: &BlobHash) -> Result<Vec<u8>, BlobError> {
        if self.unreadable {
            return Err(BlobError::Io(std::io::ErrorKind::PermissionDenied.into()));
        }
        let mut bytes = self.inner.get(hash)?;
        if self.corrupt_reads {
            bytes[0] ^= 1;
        }
        Ok(bytes)
    }
}

fn text_of(bytes: usize) -> String {
    "a".repeat(bytes)
}

#[test]
fn inline_text_round_trips_at_every_byte_boundary_and_stays_in_the_row() {
    let mut rig = rig();
    // The threshold counts UTF-8 bytes: 32,768 two-byte characters are 65,536.
    let two_byte = "é".repeat(32_768);
    let cases = [
        String::new(),
        "x".into(),
        text_of(65_536),
        two_byte.clone(),
        "line\0break \u{1F600} \" \\ \n\r\t".into(),
        // A document that merely looks like a stored reference is plain text.
        r#"{"v":1,"blob":{"sha256":"00","bytes":1,"chunks":[]}}"#.into(),
    ];
    for (n, text) in cases.iter().enumerate() {
        let task = format!("t{n}");
        rig.put("p", &retry(&task, 1, n as i64, text));
        let stored = rig.stored("p", &task);
        assert!(stored.starts_with(r#"{"v":1,"inline":"#), "{n}: {stored}");
        assert_eq!(
            &rig.reader("p").read_retry(&task).unwrap().unwrap().error,
            text
        );
    }
    assert!(rig.objects().is_empty());
    assert!(rig.registrations("p").is_empty());
}

#[test]
fn text_one_byte_over_the_threshold_is_one_published_object() {
    let mut rig = rig();
    // 65,537 bytes in 32,769 characters, so characters cannot decide the placement.
    let multi = format!("{}x", "é".repeat(32_768));
    for (task, text) in [("ascii", text_of(65_537)), ("multi", multi)] {
        rig.put("p", &retry(task, 3, 10, &text));
        let stored = rig.stored("p", task);
        assert!(stored.starts_with(r#"{"v":1,"blob":"#), "{stored}");
        let hash = BlobHash::of(text.as_bytes()).to_string();
        // One object, whose name is the whole hash, listed once.
        assert!(rig.objects().contains(&hash));
        assert_eq!(
            stored.matches(&hash).count(),
            2,
            "whole hash and its one chunk"
        );
        assert!(
            rig.registrations("p")
                .contains(&(hash.clone(), text.len() as i64))
        );
        assert_eq!(
            std::fs::read(rig.object_path(&hash)).unwrap(),
            text.as_bytes()
        );
        let read = rig.reader("p").read_retry(task).unwrap().unwrap();
        assert_eq!(read, retry(task, 3, 10, &text));
    }
}

#[test]
fn put_replaces_the_row_whole_and_keeps_the_old_registrations() {
    let mut rig = rig();
    let big = text_of(70_000);
    rig.put("p", &retry("t", 2, 50, &big));
    let objects = rig.objects();
    rig.put("p", &retry("t", 1, 5, "short"));
    assert_eq!(
        rig.reader("p").read_retry("t").unwrap(),
        Some(retry("t", 1, 5, "short"))
    );
    assert_eq!(
        rig.reader("p").read_retries(None, 10).unwrap().rows.len(),
        1
    );
    // The sweep, not a replacement, decides when an unnamed object may go.
    assert_eq!(rig.objects(), objects);
    assert_eq!(rig.registrations("p").len(), 1);
    assert!(rig.reader("p").named_objects().unwrap().is_empty());
}

#[test]
fn delete_reports_presence_and_is_project_local() {
    let mut rig = rig();
    rig.put("p", &retry("t", 1, 1, "a"));
    rig.put("q", &retry("t", 2, 2, "b"));
    assert!(rig.commit("p", |r| r.delete_retry("t").unwrap()));
    assert!(!rig.commit("p", |r| r.delete_retry("t").unwrap()));
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
    assert_eq!(
        rig.reader("q").read_retry("t").unwrap(),
        Some(retry("t", 2, 2, "b"))
    );
}

#[test]
fn another_projects_rows_and_objects_are_invisible() {
    let mut rig = rig();
    let big = text_of(70_000);
    rig.put("q", &retry("t", 1, 1, &big));
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
    assert!(
        rig.reader("p")
            .read_due_retries(i64::MAX, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        rig.reader("p")
            .read_retries(None, 10)
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(rig.reader("p").named_objects().unwrap().is_empty());
    // The object is shared by content, so the other project registers its own.
    rig.put("p", &retry("t", 1, 1, &big));
    assert_eq!(rig.registrations("p").len(), 1);
    assert_eq!(rig.registrations("q").len(), 1);
    assert_eq!(rig.objects().len(), 1);
}

#[test]
fn refused_rows_change_nothing_and_reads_see_the_transaction_own_writes() {
    let mut rig = rig();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    let mut retries = Retries::new(&tx, &who, &rig.blobs);
    for bad in [
        retry("", 1, 1, ""),
        retry("t", 0, 1, ""),
        retry("t", 1, -1, ""),
    ] {
        assert!(matches!(
            retries.put_retry(&bad, 0),
            Err(SqliteError::Store(StoreError::InvalidRetry(_)))
        ));
    }
    assert!(retries.get_retry("t").unwrap().is_none());
    retries.put_retry(&retry("t", 4, 9, "own"), 0).unwrap();
    assert_eq!(retries.get_retry("t").unwrap().unwrap().attempt, 4);
    assert_eq!(retries.due_retries(9, 5).unwrap().len(), 1);
    assert_eq!(retries.list_retries(None, 5).unwrap().rows.len(), 1);
    tx.rollback().unwrap();
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
}

#[test]
fn an_ended_transaction_refuses_every_operation() {
    let mut rig = rig();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    tx.execute_batch("COMMIT").unwrap();
    let mut retries = Retries::new(&tx, &who, &rig.blobs);
    assert!(matches!(
        retries.put_retry(&retry("t", 1, 1, ""), 0),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        retries.get_retry("t"),
        Err(SqliteError::TransactionAborted)
    ));
    assert!(matches!(
        retries.delete_retry("t"),
        Err(SqliteError::TransactionAborted)
    ));
}

#[test]
fn bounds_are_checked_before_any_read() {
    let rig = rig();
    let reader = rig.reader("p");
    for limit in [0, MAX_DUE_RETRIES + 1] {
        assert!(matches!(
            reader.read_due_retries(0, limit),
            Err(SqliteError::Store(StoreError::InvalidRetry(_)))
        ));
        assert!(matches!(
            reader.read_retries(None, limit),
            Err(SqliteError::Store(StoreError::InvalidRetry(_)))
        ));
    }
    for cursor in [
        RetryCursor {
            due_at: -1,
            task: "t".into(),
        },
        RetryCursor {
            due_at: 0,
            task: String::new(),
        },
    ] {
        assert!(matches!(
            reader.read_retries(Some(&cursor), 1),
            Err(SqliteError::Store(StoreError::InvalidCursor))
        ));
    }
}

#[test]
fn named_objects_list_each_indirect_row_in_task_byte_order() {
    let mut rig = rig();
    let (first, second) = (text_of(70_000), "é".repeat(40_000));
    rig.put("p", &retry("b", 1, 1, &first));
    rig.put("p", &retry("B", 1, 9, &second));
    rig.put("p", &retry("inline", 1, 1, "small"));
    rig.put("q", &retry("a", 1, 1, &first));
    let named = rig.reader("p").named_objects().unwrap();
    let rows: Vec<_> = named
        .iter()
        .map(|n| (n.task.as_str(), n.ordinal, n.size))
        .collect();
    assert_eq!(rows, [("B", 0, 80_000), ("b", 0, 70_000)]);
    assert_eq!(named[0].sha256, BlobHash::of(second.as_bytes()).to_string());
    assert_eq!(named[1].whole_sha256, named[1].sha256);
}
