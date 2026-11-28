//! One row per task in `retry_queue`, replaced whole, read by deadline and then
//! task id as bytes. Rows are project-local: another project's row is absent.
use std::collections::BTreeMap;

use orch_core::store::Caller;
use orch_core::store::retries::{Retry, RetryCursor, RetryPage, check_limit};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::text::{self, Objects, Stored};
use super::{corrupt, reference_error, text_error};
use crate::blob::{BlobReferences, DiskBlobStore};
use crate::sqlite::{Result, SqliteError};

const COLUMNS: &str = "task_id,attempt,due_at,length(CAST(error AS BLOB)),
    CASE WHEN length(CAST(error AS BLOB)) <= ?2 THEN error END";

/// What a row names in the blob store: one entry for each ordered occurrence of
/// an object, repeats included, for the sweep, verification, export and backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::sqlite) struct NamedObject {
    pub task: String,
    pub ordinal: u64,
    pub sha256: String,
    pub size: u64,
    /// The hash of the whole value, which names no object when there are several.
    pub whole_sha256: String,
}

struct Raw {
    task: String,
    attempt: i64,
    due_at: i64,
    length: Option<i64>,
    stored: Option<String>,
}

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        task: row.get(0)?,
        attempt: row.get(1)?,
        due_at: row.get(2)?,
        length: row.get(3)?,
        stored: row.get(4)?,
    })
}

// The due and page reads walk `retries_due` in deadline order, so a read costs
// the rows it returns and the ties of one deadline, not the whole table: the
// planner would otherwise pick the primary key and sort every row of the project.
/// Reads on any connection or transaction the caller selected: a pooled read
/// snapshot or the writer's own transaction.
pub(in crate::sqlite) struct RetryReader<'a> {
    conn: &'a Connection,
    project: &'a str,
    objects: &'a dyn Objects,
}

impl<'a> RetryReader<'a> {
    pub(in crate::sqlite) fn new(
        conn: &'a Connection,
        project: &'a str,
        blobs: &'a DiskBlobStore,
    ) -> Self {
        Self::with_objects(conn, project, blobs)
    }

    pub(super) fn with_objects(
        conn: &'a Connection,
        project: &'a str,
        objects: &'a dyn Objects,
    ) -> Self {
        Self {
            conn,
            project,
            objects,
        }
    }

    pub(in crate::sqlite) fn read_retry(&self, task: &str) -> Result<Option<Retry>> {
        let found = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM retry_queue WHERE project_id=?1 AND task_id=?3"),
                params![self.project, text::MAX_STORED_BYTES as i64, task],
                raw,
            )
            .optional()?;
        Ok(self.hydrate(found.into_iter().collect())?.pop())
    }

    /// Rows with `due_at <= now_ms`, earliest first, ties by task id.
    pub(in crate::sqlite) fn read_due_retries(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<Retry>> {
        check_limit(limit)?;
        let rows = self.select(
            &format!(
                "SELECT {COLUMNS} FROM retry_queue INDEXED BY retries_due
                 WHERE project_id=?1 AND due_at <= ?3
                 ORDER BY due_at,task_id LIMIT ?4"
            ),
            params![
                self.project,
                text::MAX_STORED_BYTES as i64,
                now_ms,
                limit as i64
            ],
        )?;
        self.hydrate(rows)
    }

    /// Every row from just after `after`, in the order of the due read. Only the
    /// rows returned are resolved, so a damaged row beyond the page does not
    /// fail it.
    pub(in crate::sqlite) fn read_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryPage> {
        let (rows, more) = self.selected_retries(after, limit)?;
        let rows = self.hydrate(rows)?;
        let next = rows.last().filter(|_| more).map(RetryCursor::after);
        Ok(RetryPage { rows, next })
    }

    fn selected_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<(Vec<Raw>, bool)> {
        check_limit(limit)?;
        if let Some(cursor) = after {
            cursor.validate()?;
        }
        // One more row than the page tells whether a row follows it.
        let fetch = limit as i64 + 1;
        let (rows, more) = match after {
            None => self.select_stream_page(
                &format!(
                    "SELECT {COLUMNS} FROM retry_queue INDEXED BY retries_due WHERE project_id=?1
                     ORDER BY due_at,task_id LIMIT ?3"
                ),
                params![self.project, text::MAX_STORED_BYTES as i64, fetch],
                limit,
            )?,
            Some(cursor) => self.select_stream_page(
                &format!(
                    "SELECT {COLUMNS} FROM retry_queue INDEXED BY retries_due
                     WHERE project_id=?1 AND due_at >= ?3 AND (due_at > ?3 OR task_id > ?4)
                     ORDER BY due_at,task_id LIMIT ?5"
                ),
                params![
                    self.project,
                    text::MAX_STORED_BYTES as i64,
                    cursor.due_at,
                    cursor.task,
                    fetch
                ],
                limit,
            )?,
        };
        Ok((rows, more))
    }

    /// Select and validate only the returned rows, with checked logical-byte
    /// accounting before any object is opened. The enclosing snapshot must
    /// remain alive through streaming. This is a preparation leaf, not the
    /// public bounded API: selection/JSON allocation still needs admission in
    /// the continuation before it can be used by a production caller.
    #[allow(
        dead_code,
        reason = "private streaming plan awaits bounded public-port integration"
    )]
    pub(in crate::sqlite) fn preflight_stream_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryStreamPage> {
        let (rows, more) = self.selected_retries(after, limit)?;
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(rows.len())
            .map_err(|_| stream_resource())?;
        let mut bytes = 0u64;
        for row in rows {
            let (attempt, stored) = self.parse(&row)?;
            let length = match &stored {
                Stored::Inline(text) => text.len() as u64,
                Stored::Blob(blob) => blob.bytes,
            };
            bytes = text::checked_page_bytes(bytes, length).map_err(|_| stream_resource())?;
            prepared.push((row, attempt, stored));
        }
        let next = prepared
            .last()
            .filter(|_| more)
            .map(|(row, _, _)| RetryCursor {
                task: row.task.clone(),
                due_at: row.due_at,
            });
        Ok(RetryStreamPage {
            rows: prepared,
            bytes,
            next,
        })
    }

    /// Every object the project's retry rows name, by task and position. Damage
    /// in any row is an error: a sweep or a backup must never skip a row.
    pub(in crate::sqlite) fn named_objects(&self) -> Result<Vec<NamedObject>> {
        let rows = self.select(
            &format!("SELECT {COLUMNS} FROM retry_queue WHERE project_id=?1 ORDER BY task_id"),
            params![self.project, text::MAX_STORED_BYTES as i64],
        )?;
        let mut named = Vec::new();
        for row in rows {
            if let Stored::Blob(blob) = self.parse(&row)?.1 {
                for (ordinal, chunk) in blob.chunks.iter().enumerate() {
                    named.push(NamedObject {
                        task: row.task.clone(),
                        ordinal: ordinal as u64,
                        sha256: chunk.sha256.clone(),
                        size: chunk.size,
                        whole_sha256: blob.sha256.clone(),
                    });
                }
            }
        }
        Ok(named)
    }

    fn select(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<Raw>> {
        let mut statement = self.conn.prepare_cached(sql)?;
        Ok(statement
            .query_map(args, raw)?
            .collect::<rusqlite::Result<_>>()?)
    }

    // The lookahead row is used only for existence. Do not decode any of its
    // fields: even a malformed native column beyond the page is unselected.
    fn select_stream_page(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
        limit: usize,
    ) -> Result<(Vec<Raw>, bool)> {
        let mut statement = self.conn.prepare_cached(sql)?;
        let mut query = statement.query(args)?;
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(limit)
            .map_err(|_| stream_resource())?;
        while let Some(row) = query.next()? {
            if selected.len() == limit {
                return Ok((selected, true));
            }
            selected.push(raw(row)?);
        }
        Ok((selected, false))
    }

    /// The structural checks of one row: its native columns and its document.
    fn parse(&self, row: &Raw) -> Result<(u32, Stored)> {
        if row.task.is_empty() {
            return Err(corrupt("task id"));
        }
        let attempt = u32::try_from(row.attempt)
            .ok()
            .filter(|attempt| *attempt >= 1)
            .ok_or_else(|| corrupt("attempt"))?;
        if row.due_at < 0 {
            return Err(corrupt("deadline"));
        }
        let Some(stored) = row.stored.as_deref() else {
            return Err(corrupt(if row.length.is_some() {
                "document size"
            } else {
                "missing diagnostic"
            }));
        };
        let stored = text::parse(stored).map_err(text_error)?;
        if let Stored::Blob(blob) = &stored {
            self.registrations(blob)?;
        }
        Ok((attempt, stored))
    }

    /// Every named object has a registration in this project that agrees with
    /// the row, in the selecting transaction.
    fn registrations(&self, blob: &text::Blob) -> Result<()> {
        let mut sizes = BTreeMap::new();
        for chunk in &blob.chunks {
            if sizes.insert(chunk.sha256.as_str(), chunk.size) == Some(chunk.size) {
                continue;
            }
            let registered: Option<(i64, String)> = self
                .conn
                .query_row(
                    "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                    params![self.project, chunk.sha256],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match registered {
                Some((size, location)) if location == "disk" && size as u64 == chunk.size => {}
                _ => return Err(corrupt("blob registration")),
            }
        }
        Ok(())
    }

    /// Every row is checked before any object is opened, so proven damage is
    /// never hidden by an access error on an earlier row.
    fn hydrate(&self, rows: Vec<Raw>) -> Result<Vec<Retry>> {
        let parsed = rows
            .iter()
            .map(|row| self.parse(row))
            .collect::<Result<Vec<_>>>()?;
        rows.into_iter()
            .zip(parsed)
            .map(|(row, (attempt, stored))| {
                let error = match stored {
                    Stored::Inline(text) => text,
                    Stored::Blob(blob) => {
                        text::reconstruct(self.objects, &blob).map_err(text_error)?
                    }
                };
                Ok(Retry {
                    task: row.task,
                    attempt,
                    due_at: row.due_at,
                    error,
                })
            })
            .collect()
    }
}

/// Retries on the enclosing writer transaction.
pub(in crate::sqlite) struct Retries<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    objects: &'a dyn Objects,
}

impl<'a, 'conn> Retries<'a, 'conn> {
    pub(in crate::sqlite) fn new(
        tx: &'a Transaction<'conn>,
        caller: &'a Caller,
        blobs: &'a DiskBlobStore,
    ) -> Self {
        Self::with_objects(tx, caller, blobs)
    }

    pub(super) fn with_objects(
        tx: &'a Transaction<'conn>,
        caller: &'a Caller,
        objects: &'a dyn Objects,
    ) -> Self {
        Self {
            tx,
            caller,
            objects,
        }
    }

    fn active(&self) -> Result<()> {
        if self.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    fn reader(&self) -> RetryReader<'_> {
        RetryReader::with_objects(self.tx, &self.caller.project, self.objects)
    }

    // The registrations and the row are one operation: a caught SQL failure,
    // a trigger's included, must not leave half of it for the caller to commit.
    fn atomic<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.active()?;
        self.tx.execute_batch("SAVEPOINT orch_retry")?;
        let result = work().and_then(|value| {
            self.tx.execute_batch("RELEASE orch_retry")?;
            Ok(value)
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Err(cleanup) = self
                    .tx
                    .execute_batch("ROLLBACK TO orch_retry; RELEASE orch_retry")
                {
                    // As with leases and intents, a failed cleanup must escape a
                    // callback that could otherwise catch the error and commit.
                    // The typed unwind is observed and raised again by the
                    // containing catch; the writer ends and drops its connection.
                    crate::sqlite::writer::contain::cleanup_failed(error, cleanup);
                }
                Err(error)
            }
        }
    }

    /// Inserts the row, or replaces the task's row whole. Text above 65,536
    /// bytes is published, synced and verified before any row or registration is
    /// written, so a failed publication changes nothing. `now_ms` stamps the
    /// blob registrations. Objects of a replaced or deleted row stay registered:
    /// the sweep, not this call, decides when an unnamed object may go.
    pub(in crate::sqlite) fn put_retry(&mut self, retry: &Retry, now_ms: i64) -> Result<()> {
        self.active()?;
        retry.validate()?;
        let prepared = text::encode(self.objects, &retry.error).map_err(text_error)?;
        self.atomic(|| {
            for object in &prepared.objects {
                BlobReferences::register_published(
                    self.tx,
                    &self.caller.project,
                    &object.hash,
                    object.size as i64,
                    now_ms,
                )
                .map_err(reference_error)?;
            }
            self.tx.execute(
                "INSERT INTO retry_queue (project_id,task_id,attempt,due_at,error)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(project_id,task_id) DO UPDATE SET
                   attempt=excluded.attempt,due_at=excluded.due_at,error=excluded.error",
                params![
                    self.caller.project,
                    retry.task,
                    retry.attempt,
                    retry.due_at,
                    prepared.document
                ],
            )?;
            Ok(())
        })
    }

    /// Removes the task's row; true if there was one.
    pub(in crate::sqlite) fn delete_retry(&mut self, task: &str) -> Result<bool> {
        self.active()?;
        Ok(self.tx.execute(
            "DELETE FROM retry_queue WHERE project_id=?1 AND task_id=?2",
            params![self.caller.project, task],
        )? == 1)
    }

    pub(in crate::sqlite) fn get_retry(&self, task: &str) -> Result<Option<Retry>> {
        self.active()?;
        self.reader().read_retry(task)
    }

    pub(in crate::sqlite) fn due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>> {
        self.active()?;
        self.reader().read_due_retries(now_ms, limit)
    }

    pub(in crate::sqlite) fn list_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryPage> {
        self.active()?;
        self.reader().read_retries(after, limit)
    }

    pub(in crate::sqlite) fn named_objects(&self) -> Result<Vec<NamedObject>> {
        self.active()?;
        self.reader().named_objects()
    }
}

fn stream_resource() -> SqliteError {
    SqliteError::Io(std::io::ErrorKind::OutOfMemory.into())
}

/// Selected rows from one caller-owned snapshot. No cursor is released until
/// all selected text has passed final verification. All output is provisional
/// until this method succeeds; a sink must discard it on any error.
#[allow(
    dead_code,
    reason = "private streaming plan awaits bounded public-port integration"
)]
pub(in crate::sqlite) struct RetryStreamPage {
    rows: Vec<(Raw, u32, Stored)>,
    bytes: u64,
    next: Option<RetryCursor>,
}

#[allow(
    dead_code,
    reason = "private streaming plan awaits bounded public-port integration"
)]
impl RetryStreamPage {
    pub(in crate::sqlite) fn declared_bytes(&self) -> u64 {
        self.bytes
    }

    pub(in crate::sqlite) fn stream(
        self,
        mut read: impl FnMut(
            &crate::blob::BlobHash,
            &mut dyn FnMut(&[u8]) -> std::result::Result<(), text::StreamError>,
        ) -> std::result::Result<(), text::StreamError>,
        mut begin: impl FnMut(&str, u32, i64) -> std::result::Result<(), text::StreamError>,
        mut consume: impl FnMut(&[u8]) -> std::result::Result<(), text::StreamError>,
    ) -> std::result::Result<Option<RetryCursor>, text::StreamError> {
        for (row, attempt, stored) in self.rows {
            begin(&row.task, attempt, row.due_at)?;
            match stored {
                Stored::Inline(value) => {
                    for part in value.as_bytes().chunks(64 * 1024) {
                        consume(part)?;
                    }
                }
                Stored::Blob(blob) => text::verify_stream(&blob, &mut read, &mut consume)?,
            }
        }
        Ok(self.next)
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use crate::sqlite::retry_message::tests::{retry, rig};

    #[test]
    fn streaming_preflight_selects_ordered_rows_without_resolving_lookahead() {
        let mut rig = rig();
        rig.put("p", &retry("b", 2, 1, "second"));
        rig.put("p", &retry("a", 1, 1, "first"));
        rig.put("p", &retry("z", 1, 2, "later"));
        rig.conn
            .execute(
                "UPDATE retry_queue SET error='damaged' WHERE task_id='z'",
                [],
            )
            .unwrap();
        let plan = rig.reader("p").preflight_stream_retries(None, 2).unwrap();
        assert_eq!(plan.declared_bytes(), 11);
        let mut tasks = Vec::new();
        let mut bytes = Vec::new();
        let next = plan
            .stream(
                |_, _| panic!("inline rows open no objects"),
                |task, attempt, deadline| {
                    tasks.push((task.to_owned(), attempt, deadline));
                    Ok(())
                },
                |part| {
                    bytes.extend_from_slice(part);
                    Ok(())
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(tasks, [("a".to_owned(), 1, 1), ("b".to_owned(), 2, 1)]);
        assert_eq!(bytes, b"firstsecond");
        assert_eq!(
            next,
            RetryCursor {
                task: "b".into(),
                due_at: 1
            }
        );
        assert!(
            rig.reader("p")
                .preflight_stream_retries(Some(&next), 2)
                .is_err()
        );
    }
}
