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
        // No row may name bytes until put has completed its sync and publication.
        let hash = self.disk.put(bytes)?;
        let changed = conn.execute("INSERT INTO blobs (project_id,sha256,size,storage_location,created_at,unreferenced_since) VALUES (?1,?2,?3,'disk',?4,NULL) ON CONFLICT(project_id,sha256) DO UPDATE SET unreferenced_since=NULL WHERE blobs.size=excluded.size AND blobs.storage_location=excluded.storage_location", params![self.project,hash.to_string(),bytes.len() as i64,now])?;
        if changed != 1 {
            return Err(ReferenceError::Metadata);
        }
        Ok(hash)
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
