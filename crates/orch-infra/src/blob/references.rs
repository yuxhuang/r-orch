//! SQLite row linkage for T11. The caller supplies its project-scoped writer transaction.
//! No transaction is opened or committed here. A savepoint makes each operation atomic;
//! an outer rollback leaves only a harmless unreferenced file. Authorization belongs to
//! the dispatcher, and the active file backend is unchanged until T24.
use super::{BlobError, BlobHash, DiskBlobStore};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

#[derive(Debug, thiserror::Error)]
pub enum ReferenceError {
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("missing {kind} in selected project")]
    MissingOwner { kind: &'static str },
    #[error("blob metadata is missing or disagrees with its bytes")]
    Metadata,
    #[error("prompt blob is not UTF-8")]
    PromptEncoding,
}
type Result<T> = std::result::Result<T, ReferenceError>;

/// A project's references to a daemon-wide content-addressed disk store.
/// `project` must come from the caller context, never from an entity payload.
pub struct BlobReferences<'a> {
    disk: &'a DiskBlobStore,
    project: &'a str,
}
impl<'a> BlobReferences<'a> {
    pub fn new(disk: &'a DiskBlobStore, project: &'a str) -> Self {
        Self { disk, project }
    }

    /// All prompts, including short/empty ones, are blobs (R59).
    /// The run must already exist in the same transaction. Revision changes belong
    /// to the surrounding run operation, which may set other fields as well.
    pub fn store_prompt(
        &self,
        tx: &mut Transaction<'_>,
        run: &str,
        prompt: &str,
        now: i64,
    ) -> Result<BlobHash> {
        let sp = tx.savepoint()?;
        if !sp.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE project_id=?1 AND id=?2)",
            params![self.project, run],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(ReferenceError::MissingOwner { kind: "run" });
        }
        let hash = self.register(&sp, prompt.as_bytes(), now)?;
        sp.execute(
            "UPDATE runs SET prompt_sha256=?3 WHERE project_id=?1 AND id=?2",
            params![self.project, run, hash.to_string()],
        )?;
        sp.commit()?;
        Ok(hash)
    }

    /// The display name is bound as data and is never used for filesystem access.
    /// Replacing an attachment is atomic with its blob registration.
    pub fn store_attachment(
        &self,
        tx: &mut Transaction<'_>,
        task: &str,
        name: &str,
        bytes: &[u8],
        now: i64,
    ) -> Result<BlobHash> {
        let sp = tx.savepoint()?;
        if !sp.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE project_id=?1 AND id=?2)",
            params![self.project, task],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(ReferenceError::MissingOwner { kind: "task" });
        }
        let hash = self.register(&sp, bytes, now)?;
        sp.execute("INSERT INTO task_attachments (project_id,task_id,name,sha256) VALUES (?1,?2,?3,?4) ON CONFLICT(project_id,task_id,name) DO UPDATE SET sha256=excluded.sha256", params![self.project,task,name,hash.to_string()])?;
        sp.commit()?;
        Ok(hash)
    }

    fn register(&self, conn: &Connection, bytes: &[u8], now: i64) -> Result<BlobHash> {
        let size = i64::try_from(bytes.len()).map_err(|_| ReferenceError::Metadata)?;
        // No row may name bytes until put has completed its sync and publication.
        let hash = self.disk.put(bytes)?;
        Self::register_published(conn, self.project, &hash, size, now)?;
        Ok(hash)
    }

    /// Register disk bytes the caller has already durably published.
    /// `project` must be authorized by the caller. `conn` is its existing transaction
    /// or savepoint; this helper neither opens nor commits one and performs no disk I/O.
    /// The caller checks the byte length fits `i64` before supplying `size`.
    pub(crate) fn register_published(
        conn: &Connection,
        project: &str,
        hash: &BlobHash,
        size: i64,
        now: i64,
    ) -> Result<()> {
        if size < 0 {
            return Err(ReferenceError::Metadata);
        }
        let changed = conn.execute("INSERT INTO blobs (project_id,sha256,size,storage_location,created_at,unreferenced_since) VALUES (?1,?2,?3,'disk',?4,NULL) ON CONFLICT(project_id,sha256) DO UPDATE SET unreferenced_since=NULL WHERE blobs.size=excluded.size AND blobs.storage_location=excluded.storage_location", params![project,hash.to_string(),size,now])?;
        if changed != 1 {
            return Err(ReferenceError::Metadata);
        }
        Ok(())
    }

    pub fn load_prompt(&self, conn: &Connection, run: &str) -> Result<Option<String>> {
        let hash: Option<Option<String>> = conn
            .query_row(
                "SELECT prompt_sha256 FROM runs WHERE project_id=?1 AND id=?2",
                params![self.project, run],
                |r| r.get(0),
            )
            .optional()?;
        hash.flatten()
            .map(|h| {
                String::from_utf8(self.read(conn, &h)?).map_err(|_| ReferenceError::PromptEncoding)
            })
            .transpose()
    }

    pub fn load_attachment(
        &self,
        conn: &Connection,
        task: &str,
        name: &str,
    ) -> Result<Option<Vec<u8>>> {
        let hash: Option<String> = conn.query_row("SELECT sha256 FROM task_attachments WHERE project_id=?1 AND task_id=?2 AND name=?3", params![self.project,task,name], |r| r.get(0)).optional()?;
        hash.map(|h| self.read(conn, &h)).transpose()
    }

    fn read(&self, conn: &Connection, text: &str) -> Result<Vec<u8>> {
        // Parse even when the database row was imported or tampered with.
        let hash: BlobHash = text.parse().map_err(BlobError::from)?;
        let meta: Option<(i64, String)> = conn
            .query_row(
                "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                params![self.project, text],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((size, location)) = meta else {
            return Err(ReferenceError::Metadata);
        };
        if size < 0 || location != "disk" {
            return Err(ReferenceError::Metadata);
        }
        let bytes = self.disk.get(&hash)?;
        if bytes.len() as u64 != size as u64 {
            return Err(ReferenceError::Metadata);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_dir::{DataDir, Holder, LockKind};
    use crate::sqlite::{ConnectionRole, Synchronous, migrations::migrate, open_connection};

    fn with_database(test: impl FnOnce(&mut crate::sqlite::ConnectionHandle<'_>)) {
        let root = tempfile::tempdir().unwrap();
        let data = DataDir::acquire(
            &root.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "test".into(),
            },
        )
        .unwrap();
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrate(&data, &mut conn).unwrap();
        // No disk blob store is opened: the helper needs only the caller's SQL context.
        test(&mut conn);
        for directory in ["data/blobs/sha256", "data/blobs/tmp"] {
            assert_eq!(
                std::fs::read_dir(root.path().join(directory))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    type Row = (String, String, i64, String, i64, Option<i64>);
    fn rows(conn: &Connection) -> Vec<Row> {
        conn.prepare("SELECT project_id,sha256,size,storage_location,created_at,unreferenced_since FROM blobs ORDER BY project_id,sha256")
            .unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    #[test]
    fn published_row_insert_and_reuse_preserve_creation_and_clear_mark() {
        with_database(|conn| {
            let tx = conn.transaction().unwrap();
            let hash = BlobHash::of(b"synthetic published bytes");
            assert!(rows(&tx).is_empty());
            BlobReferences::register_published(&tx, "p", &hash, 25, 123).unwrap();
            assert_eq!(
                rows(&tx),
                vec![("p".into(), hash.to_string(), 25, "disk".into(), 123, None)]
            );
            tx.execute("UPDATE blobs SET unreferenced_since=456", [])
                .unwrap();
            BlobReferences::register_published(&tx, "p", &hash, 25, 789).unwrap();
            assert_eq!(
                rows(&tx),
                vec![("p".into(), hash.to_string(), 25, "disk".into(), 123, None)]
            );
            tx.commit().unwrap();
            assert_eq!(rows(conn).len(), 1);
        });
    }

    #[test]
    fn published_row_conflicts_refuse_without_modifying_metadata() {
        with_database(|conn| {
            for (size, location) in [(24, "disk"), (25, "object")] {
                let tx = conn.transaction().unwrap();
                let hash = BlobHash::of(b"synthetic published bytes");
                tx.execute(
                    "INSERT INTO blobs VALUES ('p',?1,?2,?3,123,456)",
                    params![hash.to_string(), size, location],
                )
                .unwrap();
                let before = rows(&tx);
                assert!(matches!(
                    BlobReferences::register_published(&tx, "p", &hash, 25, 789),
                    Err(ReferenceError::Metadata)
                ));
                assert_eq!(rows(&tx), before);
                // Committing a caught refusal must not hide any mutation.
                tx.commit().unwrap();
                assert_eq!(rows(conn), before);
                conn.execute("DELETE FROM blobs", []).unwrap();
            }
        });
    }

    #[test]
    fn published_rows_and_reuse_obey_outer_rollback_after_savepoint_commit() {
        with_database(|conn| {
            let hash = BlobHash::of(b"existing");
            let new_hash = BlobHash::of(b"new");
            conn.execute(
                "INSERT INTO blobs VALUES ('p',?1,8,'disk',123,456)",
                [hash.to_string()],
            )
            .unwrap();
            let before = rows(conn);
            let mut tx = conn.transaction().unwrap();
            let sp = tx.savepoint().unwrap();
            BlobReferences::register_published(&sp, "p", &hash, 8, 789).unwrap();
            BlobReferences::register_published(&sp, "p", &new_hash, 3, 789).unwrap();
            assert_eq!(rows(&sp).len(), 2);
            assert!(rows(&sp).iter().all(|r| r.5.is_none()));
            sp.commit().unwrap();
            tx.rollback().unwrap();
            assert_eq!(rows(conn), before);
        });
    }

    #[test]
    fn published_rows_are_isolated_by_authorized_project() {
        with_database(|conn| {
            let tx = conn.transaction().unwrap();
            let hash = BlobHash::of(b"same hash");
            tx.execute(
                "INSERT INTO blobs VALUES ('other',?1,99,'object',111,222)",
                [hash.to_string()],
            )
            .unwrap();
            let other = rows(&tx)[0].clone();
            BlobReferences::register_published(&tx, "p", &hash, 9, 333).unwrap();
            assert_eq!(
                rows(&tx),
                vec![
                    other.clone(),
                    ("p".into(), hash.to_string(), 9, "disk".into(), 333, None)
                ]
            );
            tx.execute(
                "UPDATE blobs SET unreferenced_since=444 WHERE project_id='p'",
                [],
            )
            .unwrap();
            BlobReferences::register_published(&tx, "p", &hash, 9, 555).unwrap();
            assert_eq!(rows(&tx)[0], other);
            assert_eq!(rows(&tx)[1].4, 333);
            assert_eq!(rows(&tx)[1].5, None);
        });
    }

    #[test]
    fn published_row_negative_length_refuses_without_a_write() {
        with_database(|conn| {
            let tx = conn.transaction().unwrap();
            let hash = BlobHash::of(b"");
            assert!(matches!(
                BlobReferences::register_published(&tx, "p", &hash, -1, 123),
                Err(ReferenceError::Metadata)
            ));
            assert!(rows(&tx).is_empty());
            BlobReferences::register_published(&tx, "p", &hash, 0, 123).unwrap();
            let before = rows(&tx);
            assert!(matches!(
                BlobReferences::register_published(&tx, "p", &hash, -1, 456),
                Err(ReferenceError::Metadata)
            ));
            assert_eq!(rows(&tx), before);
        });
    }
}
