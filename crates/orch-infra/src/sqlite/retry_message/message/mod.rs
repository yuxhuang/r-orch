//! SQLite Message mapping (T10, issue 109): the `message` family of Entity
//! format v1 behind the entity port's create, get, update, move, delete and
//! filtered page, with the opaque time strings of the domain kept whole.
//!
//! The detail (`entity_details`, `entity_blob_refs`) is the authority for the
//! complete typed `Message`. The `messages` row is a projection of it, written
//! in the same savepoint and compared with it on every read, so no read can
//! return a value that its row and its detail disagree about. All comparison
//! and ordering is on original UTF-8 bytes, so a text over 65,536 bytes takes
//! part like any other: candidates are hydrated whole, in the selecting
//! snapshot, before they are filtered, ordered, counted or cut.
//!
//! The helpers borrow the caller's connection or writer transaction, never
//! commit it, and are not the borrowed `UnitOfWork` facade. No clock is read:
//! the time a blob registration is stamped is supplied by the caller, and so is
//! `now` of `Filter::PendingMessages`.
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;

use orch_core::store::{Caller, Entity, EntityKind, Filter, Record, Revision, Sort, StoreError};
use orch_domain::{Message, MessageStatus};
use rusqlite::{
    Connection, OptionalExtension, Transaction, params_from_iter, types::Value as SqlValue,
};

use super::{publish_error, reference_error_for};
use crate::blob::{BlobHash, BlobReferences, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use crate::sqlite::task_format::{codec::Detail, text::Chunk};
use crate::sqlite::time_projection::rfc3339_millis;
#[cfg(test)]
use crate::sqlite::time_projection::{MAX_MILLIS, MIN_MILLIS};
use crate::sqlite::{Result, SqliteError};
use format::{FormatError, SLOTS};

mod format;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod time_tests;

const FAMILY: &str = "message";
const POSITION: &str = "position:message";

/// The text column that holds each slot, as `<column>`, `<column>_form`,
/// `<column>_sha256`, `<column>_bytes`.
fn column_of(slot: &str) -> &str {
    match slot {
        "created_at" => "created_at_text",
        "expires_at" => "expires_at_text",
        "delivered_at" => "delivered_at_text",
        other => other,
    }
}

/// Every projected column, in the order of [`projection`].
fn columns() -> Vec<String> {
    let mut out = Vec::new();
    for (slot, _) in SLOTS {
        let base = column_of(slot);
        out.extend([
            base.to_owned(),
            format!("{base}_form"),
            format!("{base}_sha256"),
            format!("{base}_bytes"),
        ]);
    }
    out.extend(
        [
            "channel",
            "status",
            "created_at",
            "expires_at",
            "delivered_at",
        ]
        .map(str::to_owned),
    );
    out
}

fn corrupt(form: i64, check: &'static str, locator: &'static str) -> SqliteError {
    SqliteError::MessageCorrupt {
        form,
        check,
        locator,
    }
}

/// A key is the text itself up to 65,536 bytes and its digest beyond.
fn key(id: &str) -> (i64, String) {
    if id.len() <= INLINE_TEXT_MAX_BYTES {
        (0, id.to_owned())
    } else {
        (1, BlobHash::of(id.as_bytes()).to_string())
    }
}

fn check_limit(limit: usize) -> Result<()> {
    if (1..=1000).contains(&limit) {
        Ok(())
    } else {
        Err(StoreError::InvalidPage("limit must be 1..=1000".into()).into())
    }
}

fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::CounterExhausted.into())
}

fn text_value(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, |text| SqlValue::Text(text.to_owned()))
}

fn quartet(value: Option<&str>, identity: bool) -> [SqlValue; 4] {
    match value {
        Some(text) if text.len() > INLINE_TEXT_MAX_BYTES => {
            let hash = BlobHash::of(text.as_bytes()).to_string();
            [
                SqlValue::Text(if identity {
                    hash.clone()
                } else {
                    String::new()
                }),
                SqlValue::Integer(1),
                SqlValue::Text(hash),
                SqlValue::Integer(text.len() as i64),
            ]
        }
        other => [
            text_value(other),
            SqlValue::Integer(0),
            SqlValue::Null,
            SqlValue::Null,
        ],
    }
}

/// The integer projection of a time string: the one rule of `time_projection`
/// (an RFC 3339 instant of whole milliseconds inside the range). It is never an
/// order or a filter.
fn millis(text: Option<&str>) -> SqlValue {
    text.and_then(rfc3339_millis)
        .map_or(SqlValue::Null, SqlValue::Integer)
}

fn projection(message: &Message) -> Vec<SqlValue> {
    let mut out = Vec::new();
    for (slot, _) in SLOTS {
        out.extend(quartet(format::slot_text(message, slot), slot == "id"));
    }
    out.extend([
        SqlValue::Text(message.channel.as_str().into()),
        SqlValue::Text(message.status.as_str().into()),
        millis(Some(&message.created_at)),
        millis(message.expires_at.as_deref()),
        millis(message.delivered_at.as_deref()),
    ]);
    out
}

/// The failure of a read: stored text that is not UTF-8 is damage, not a
/// transient fault.
fn read_error(form: i64, error: rusqlite::Error) -> SqliteError {
    match error {
        rusqlite::Error::Utf8Error(..) => corrupt(form, "text encoding", "row"),
        other => SqliteError::Sql(other),
    }
}

fn format_error(form: i64, error: FormatError, writing: bool) -> SqliteError {
    match error {
        FormatError::Access(kind) => SqliteError::Io(kind.into()),
        FormatError::Publish(error) => publish_error(error),
        FormatError::Invalid { check, .. } if writing => SqliteError::Unrepresentable(format!(
            "message cannot be stored in the schema's bounds: {check}"
        )),
        FormatError::Invalid { check, locator } => corrupt(form, check, locator),
    }
}

/// What a page selects: a message filter and one of the sorts the family has.
pub(in crate::sqlite) struct MessageSelection<'a> {
    pub filter: &'a Filter,
    pub sort: Sort,
}

pub(in crate::sqlite) struct MessagePage {
    pub items: Vec<Record>,
    /// Matches only, before the cursor.
    pub total: u64,
    pub has_more: bool,
}

/// What a page is resumed after: the *original* values a message order
/// compares, in order and ending with the identity ("Cursors of the eight
/// families other than Task" in the spec). It is not a row, so a message that
/// has since been deleted, changed or has left the filter (a pending message
/// that was delivered) still names its place. Texts compare as bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sqlite) enum OrderKey<'a> {
    /// Sort `id`.
    Id(Cow<'a, str>),
    /// The default order: `created_at`, the position, then the id.
    Default {
        created_at: Cow<'a, str>,
        position: u64,
        id: Cow<'a, str>,
    },
}

impl OrderKey<'_> {
    /// The total order of the selection the two keys share; keys of another
    /// shape are never compared, because a selection checks its keys first.
    fn cmp_key(&self, other: &OrderKey<'_>) -> Ordering {
        let bytes = |a: &str, b: &str| a.as_bytes().cmp(b.as_bytes());
        match (self, other) {
            (OrderKey::Id(a), OrderKey::Id(b)) => bytes(a, b),
            (
                OrderKey::Default {
                    created_at: u,
                    position: p,
                    id: a,
                },
                OrderKey::Default {
                    created_at: v,
                    position: q,
                    id: b,
                },
            ) => bytes(u, v).then(p.cmp(q)).then_with(|| bytes(a, b)),
            _ => Ordering::Equal,
        }
    }
}

impl MessageSelection<'_> {
    fn check(&self) -> Result<()> {
        if !matches!(
            self.filter,
            Filter::Messages { .. } | Filter::PendingMessages { .. }
        ) {
            return Err(StoreError::InvalidPage("not a message filter".into()).into());
        }
        if !matches!(self.sort, Sort::Default | Sort::Id) {
            return Err(StoreError::InvalidPage("sort is not offered for messages".into()).into());
        }
        Ok(())
    }

    /// Exact original bytes, whichever form a text is stored in.
    fn matches(&self, record: &Record) -> bool {
        let Entity::Message(message) = &record.entity else {
            return false;
        };
        match self.filter {
            Filter::Messages { recipient } => recipient
                .as_deref()
                .is_none_or(|r| message.to_agent_id.as_deref() == Some(r)),
            Filter::PendingMessages { recipient, now } => {
                message.to_agent_id.as_deref() == Some(recipient.as_str())
                    && message.status == MessageStatus::Pending
                    && message
                        .expires_at
                        .as_deref()
                        .is_none_or(|deadline| deadline.as_bytes() >= now.as_bytes())
            }
            _ => false,
        }
    }

    /// The ordering tuple of a hydrated message, on its original bytes: the
    /// `created_at`, the position and the id, or the id alone for the id sort.
    /// `None` when the record is not a message.
    pub(in crate::sqlite) fn key_of<'r>(&self, record: &'r Record) -> Option<OrderKey<'r>> {
        let Entity::Message(message) = &record.entity else {
            return None;
        };
        let id = Cow::Borrowed(message.id.as_str());
        Some(match self.sort {
            Sort::Id => OrderKey::Id(id),
            _ => OrderKey::Default {
                created_at: Cow::Borrowed(message.created_at.as_str()),
                position: record.position,
                id,
            },
        })
    }

    /// The total order: the original `created_at`, then position, then the id
    /// as bytes; the id sort is the id alone.
    fn compare(&self, a: &Record, b: &Record) -> Ordering {
        match (self.key_of(a), self.key_of(b)) {
            (Some(a), Some(b)) => a.cmp_key(&b),
            _ => Ordering::Equal,
        }
    }

    /// Whether a recorded tuple is of the shape this selection's sort orders by.
    fn accepts(&self, key: &OrderKey<'_>) -> bool {
        matches!(
            (self.sort, key),
            (Sort::Id, OrderKey::Id(_)) | (Sort::Default, OrderKey::Default { .. })
        )
    }

    /// The narrowing SQL: rows that cannot match are not hydrated. It is exact
    /// only as a superset; [`Self::matches`] decides on the hydrated originals.
    fn candidates(&self, project: &str) -> (String, Vec<SqlValue>) {
        let mut sql = "SELECT id_form,id FROM messages WHERE project_id=?1".to_owned();
        let mut values = vec![SqlValue::Text(project.to_owned())];
        let recipient = match self.filter {
            Filter::PendingMessages { recipient, .. } => {
                sql.push_str(" AND status='pending'");
                Some(recipient)
            }
            Filter::Messages { recipient } => recipient.as_ref(),
            _ => None,
        };
        if let Some(recipient) = recipient {
            if recipient.len() <= INLINE_TEXT_MAX_BYTES {
                sql.push_str(" AND to_agent_id_form=0 AND to_agent_id=?2");
                values.push(SqlValue::Text(recipient.clone()));
            } else {
                sql.push_str(" AND to_agent_id_form=1 AND to_agent_id_sha256=?2");
                values.push(SqlValue::Text(
                    BlobHash::of(recipient.as_bytes()).to_string(),
                ));
            }
        }
        (sql, values)
    }
}

/// Reads on any connection or transaction the caller selected: a pooled read
/// snapshot or the writer's own transaction.
pub(in crate::sqlite) struct MessageReader<'a> {
    conn: &'a Connection,
    project: &'a str,
    blobs: &'a DiskBlobStore,
}

type Rows = Vec<Vec<SqlValue>>;

impl<'a> MessageReader<'a> {
    pub(in crate::sqlite) fn new(
        conn: &'a Connection,
        project: &'a str,
        blobs: &'a DiskBlobStore,
    ) -> Self {
        Self {
            conn,
            project,
            blobs,
        }
    }

    // Compiled identifiers only: keys, locators and payloads are always bound.
    fn rows(&self, query: &str, values: &[SqlValue]) -> rusqlite::Result<Rows> {
        let mut stmt = self.conn.prepare(query)?;
        let count = stmt.column_count();
        stmt.query_map(params_from_iter(values), |row| {
            (0..count).map(|i| row.get(i)).collect()
        })?
        .collect()
    }

    fn identity(&self, form: i64, key: &str) -> Vec<SqlValue> {
        vec![
            SqlValue::Text(self.project.to_owned()),
            SqlValue::Integer(form),
            SqlValue::Text(key.to_owned()),
        ]
    }

    pub(in crate::sqlite) fn get(&self, id: &str) -> Result<Option<Record>> {
        let (form, stored) = key(id);
        let found = self.read(form, &stored)?;
        if let Some(record) = &found
            && record.entity.id() != id
        {
            return Err(corrupt(form, "identity", "/id"));
        }
        Ok(found)
    }

    /// One hydrated, fully checked message by its stored key, or `None` when
    /// no row has the key.
    pub(in crate::sqlite) fn read(&self, form: i64, stored: &str) -> Result<Option<Record>> {
        let mut names = columns();
        names.extend(["revision".into(), "position".into()]);
        let rows = self
            .rows(
                &format!(
                    "SELECT {} FROM messages WHERE project_id=?1 AND id_form=?2 AND id=?3",
                    names.join(",")
                ),
                &self.identity(form, stored),
            )
            .map_err(|e| read_error(form, e))?;
        let Some(native) = rows.first() else {
            return Ok(None);
        };
        let n = names.len() - 2;
        let (SqlValue::Integer(revision), SqlValue::Integer(position)) =
            (&native[n], &native[n + 1])
        else {
            return Err(corrupt(form, "counter type", "revision"));
        };
        if *revision < 1 || *position < 0 {
            return Err(corrupt(form, "counter", "revision"));
        }
        let detail = self.detail(form, stored)?;
        let message =
            format::decode(self.blobs, &detail).map_err(|e| format_error(form, e, false))?;
        if key(&message.id) != (form, stored.to_owned()) {
            return Err(corrupt(form, "identity", "/id"));
        }
        // Every projection is compared with the detail; nothing is repaired.
        if native[..n] != projection(&message)[..] {
            return Err(corrupt(form, "projection", "row"));
        }
        let revision = serde_json::from_value::<Revision>((*revision).into())
            .map_err(|_| corrupt(form, "revision", "revision"))?;
        Ok(Some(Record {
            entity: Entity::Message(Box::new(message)),
            revision,
            position: *position as u64,
        }))
    }

    /// The detail and reference rows of one key, with every object's
    /// registration, before any object is opened.
    fn detail(&self, form: i64, stored: &str) -> Result<Detail> {
        let mut identity = self.identity(form, stored);
        identity.push(SqlValue::Text(FAMILY.into()));
        let rows = self
            .rows(
                "SELECT format,detail,detail_bytes,detail_sha256 FROM entity_details
                 WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4",
                &identity,
            )
            .map_err(|e| read_error(form, e))?;
        let Some(row) = rows.first() else {
            return Err(corrupt(form, "detail missing", ""));
        };
        let [
            SqlValue::Integer(1),
            inline,
            SqlValue::Integer(bytes),
            SqlValue::Text(hash),
        ] = row.as_slice()
        else {
            return Err(corrupt(form, "detail metadata", ""));
        };
        if *bytes < 0 || hash.parse::<BlobHash>().is_err() {
            return Err(corrupt(form, "detail metadata", ""));
        }
        let inline = match inline {
            SqlValue::Null if *bytes > INLINE_TEXT_MAX_BYTES as i64 => None,
            SqlValue::Text(text)
                if text.len() <= INLINE_TEXT_MAX_BYTES && text.len() as i64 == *bytes =>
            {
                Some(text.clone())
            }
            _ => return Err(corrupt(form, "detail size", "")),
        };
        let rows = self
            .rows(
                "SELECT locator,ordinal,sha256,size FROM entity_blob_refs
                 WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4
                 ORDER BY locator COLLATE BINARY,ordinal",
                &identity,
            )
            .map_err(|e| read_error(form, e))?;
        let mut refs: BTreeMap<String, Vec<Chunk>> = BTreeMap::new();
        let mut registered = BTreeMap::new();
        for row in rows {
            let [
                SqlValue::Text(locator),
                SqlValue::Integer(ordinal),
                SqlValue::Text(hash),
                SqlValue::Integer(size),
            ] = row.as_slice()
            else {
                return Err(corrupt(form, "reference metadata", "refs"));
            };
            // A message has no zero-byte row: an empty text is inline.
            if *ordinal < 0
                || *size <= 0
                || *size as u64 > MAX_BLOB_BYTES
                || hash.parse::<BlobHash>().is_err()
            {
                return Err(corrupt(form, "reference metadata", "refs"));
            }
            if registered.insert(hash.clone(), *size) != Some(*size) {
                self.registration(form, hash, *size)?;
            }
            refs.entry(locator.clone()).or_default().push(Chunk {
                ordinal: *ordinal as u64,
                sha256: hash.clone(),
                size: *size as u64,
            });
        }
        Ok(Detail {
            inline,
            bytes: *bytes as u64,
            sha256: hash.clone(),
            refs,
        })
    }

    /// The object is registered in this project, on disk, with this size.
    fn registration(&self, form: i64, hash: &str, size: i64) -> Result<()> {
        let found = self
            .rows(
                "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                &[
                    SqlValue::Text(self.project.to_owned()),
                    SqlValue::Text(hash.to_owned()),
                ],
            )
            .map_err(|e| read_error(form, e))?;
        if found != [vec![SqlValue::Integer(size), SqlValue::Text("disk".into())]] {
            return Err(corrupt(form, "blob registration", "refs"));
        }
        Ok(())
    }

    /// A page of matches. Candidates are narrowed by the schema's own columns,
    /// then hydrated whole and decided on their original bytes, so no digest,
    /// prefix or projection can decide a comparison. Rows that the narrowing
    /// excludes are not read: damage there does not fail a page that cannot
    /// return them. Any candidate that is damaged fails the page.
    /// The anchor is a record that must still match the filter; a page that
    /// resumes after a recorded ordering tuple is [`Self::select_after`].
    pub(in crate::sqlite) fn select(
        &self,
        query: MessageSelection<'_>,
        after: Option<&Record>,
        limit: usize,
    ) -> Result<MessagePage> {
        query.check()?;
        check_limit(limit)?;
        let key = match after {
            Some(anchor) => {
                if !query.matches(anchor) {
                    return Err(StoreError::InvalidCursor.into());
                }
                Some(query.key_of(anchor).ok_or(StoreError::InvalidCursor)?)
            }
            None => None,
        };
        self.page(&query, key.as_ref(), limit)
    }

    /// The same page, resumed after the ordering tuple a cursor recorded. The
    /// candidates are hydrated and filtered in this reader's own view, and the
    /// first message returned is the first that sorts after `after` on the
    /// original bytes. The tuple names a place, not a row: the message it came
    /// from may be deleted, changed or no longer match the filter (a pending
    /// message that was delivered), and nothing is refused for that. A tuple of
    /// another shape than the sort's is `InvalidCursor`. Binding the cursor and
    /// holding the tuple are the composition owner's.
    pub(in crate::sqlite) fn select_after(
        &self,
        query: MessageSelection<'_>,
        after: Option<&OrderKey<'_>>,
        limit: usize,
    ) -> Result<MessagePage> {
        query.check()?;
        check_limit(limit)?;
        if after.is_some_and(|key| !query.accepts(key)) {
            return Err(StoreError::InvalidCursor.into());
        }
        self.page(&query, after, limit)
    }

    fn page(
        &self,
        query: &MessageSelection<'_>,
        after: Option<&OrderKey<'_>>,
        limit: usize,
    ) -> Result<MessagePage> {
        let (sql, values) = query.candidates(self.project);
        let mut stmt = self.conn.prepare(&sql)?;
        let keys = stmt
            .query_map(params_from_iter(&values), |row| {
                Ok((row.get::<_, SqlValue>(0)?, row.get::<_, SqlValue>(1)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut records = Vec::new();
        for (form, stored) in keys {
            let SqlValue::Integer(form @ (0 | 1)) = form else {
                return Err(corrupt(-1, "identity metadata", "id"));
            };
            let stored = stored.map_err(|e| read_error(form, e))?;
            let SqlValue::Text(stored) = stored else {
                return Err(corrupt(form, "identity metadata", "id"));
            };
            let record = self
                .read(form, &stored)?
                .ok_or_else(|| corrupt(form, "selected row missing", "id"))?;
            if query.matches(&record) {
                records.push(record);
            }
        }
        records.sort_by(|a, b| query.compare(a, b));
        let total = records.len() as u64;
        let mut remaining = records.into_iter().filter(|row| {
            after.is_none_or(|key| {
                query
                    .key_of(row)
                    .is_some_and(|row| row.cmp_key(key) == Ordering::Greater)
            })
        });
        let items = remaining.by_ref().take(limit).collect();
        let has_more = remaining.next().is_some();
        Ok(MessagePage {
            items,
            total,
            has_more,
        })
    }
}

/// Messages on the enclosing writer transaction.
pub(in crate::sqlite) struct Messages<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    blobs: &'a DiskBlobStore,
}

impl<'a, 'conn> Messages<'a, 'conn> {
    pub(in crate::sqlite) fn new(
        tx: &'a Transaction<'conn>,
        caller: &'a Caller,
        blobs: &'a DiskBlobStore,
    ) -> Self {
        Self { tx, caller, blobs }
    }

    fn active(&self) -> Result<()> {
        if self.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    fn reader(&self) -> MessageReader<'_> {
        MessageReader::new(self.tx, &self.caller.project, self.blobs)
    }

    // The row, the detail, the reference rows, the registrations and the counter
    // are one operation: a caught SQL failure, a trigger's included, must not
    // leave part of it for the caller to commit.
    fn atomic<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.active()?;
        self.tx.execute_batch("SAVEPOINT orch_message")?;
        let result = work().and_then(|value| {
            self.tx.execute_batch("RELEASE orch_message")?;
            Ok(value)
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Err(cleanup) = self
                    .tx
                    .execute_batch("ROLLBACK TO orch_message; RELEASE orch_message")
                {
                    // As with leases, intents and retries, a failed cleanup must
                    // escape a callback that could otherwise catch the error and
                    // commit. The typed unwind is observed and raised again by
                    // the containing catch; the writer ends and drops its
                    // connection.
                    crate::sqlite::writer::contain::cleanup_failed(error, cleanup);
                }
                Err(error)
            }
        }
    }

    fn current(&self, id: &str, expected: Revision) -> Result<Record> {
        match self.reader().get(id)? {
            Some(record) if record.revision == expected => Ok(record),
            other => Err(StoreError::StaleRevision {
                kind: EntityKind::Message,
                id: id.to_owned(),
                expected,
                actual: other.map(|record| record.revision),
            }
            .into()),
        }
    }

    /// The next position of the project-local counter. The counter is never
    /// rebuilt from `MAX(position)`: a delete never makes a position available.
    fn next_position(&self) -> Result<u64> {
        let project = &self.caller.project;
        let counter: Option<i64> = self
            .tx
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name=?2",
                [project.as_str(), POSITION],
                |row| row.get(0),
            )
            .optional()?;
        let last = counter.unwrap_or(0);
        if last < 0 {
            return Err(SqliteError::CounterNegative {
                project: project.clone(),
                name: POSITION,
                value: last,
            });
        }
        let highest: Option<i64> = self.tx.query_row(
            "SELECT MAX(position) FROM messages WHERE project_id=?1",
            [project],
            |row| row.get(0),
        )?;
        if highest.is_some_and(|position| position > last) {
            return Err(if counter.is_none() {
                SqliteError::CounterMissing {
                    project: project.clone(),
                    name: POSITION,
                }
            } else {
                SqliteError::CounterBehind {
                    project: project.clone(),
                    name: POSITION,
                    value: last,
                }
            });
        }
        Ok(last.checked_add(1).ok_or(StoreError::CounterExhausted)? as u64)
    }

    fn store_position(&self, position: u64) -> Result<()> {
        self.tx.execute(
            "INSERT INTO counters(project_id,name,value) VALUES(?1,?2,?3)
             ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value",
            rusqlite::params![self.caller.project, POSITION, integer(position)?],
        )?;
        Ok(())
    }

    fn encode(&self, message: &Message) -> Result<Detail> {
        format::encode(self.blobs, message).map_err(|e| format_error(key(&message.id).0, e, true))
    }

    /// Replace the detail, its reference rows and their registrations.
    fn install(&self, message: &Message, detail: &Detail, now_ms: i64) -> Result<()> {
        let (form, stored) = key(&message.id);
        self.clear(form, &stored)?;
        self.tx.execute(
            "INSERT INTO entity_details(project_id,family,key_form,entity_key,format,detail,detail_bytes,detail_sha256)
             VALUES(?1,?2,?3,?4,1,?5,?6,?7)",
            rusqlite::params![
                self.caller.project,
                FAMILY,
                form,
                stored,
                detail.inline,
                integer(detail.bytes)?,
                detail.sha256
            ],
        )?;
        for (locator, chunks) in &detail.refs {
            for chunk in chunks {
                let hash: BlobHash = chunk
                    .sha256
                    .parse()
                    .map_err(|_| corrupt(form, "reference hash", "refs"))?;
                BlobReferences::register_published(
                    self.tx,
                    &self.caller.project,
                    &hash,
                    integer(chunk.size)?,
                    now_ms,
                )
                .map_err(|e| reference_error_for(e, form))?;
                self.tx.execute(
                    "INSERT INTO entity_blob_refs(project_id,family,key_form,entity_key,locator,ordinal,sha256,size)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    rusqlite::params![
                        self.caller.project,
                        FAMILY,
                        form,
                        stored,
                        locator,
                        integer(chunk.ordinal)?,
                        chunk.sha256,
                        integer(chunk.size)?
                    ],
                )?;
            }
        }
        Ok(())
    }

    /// Remove the detail and reference rows of a key. Registrations and objects
    /// stay for the sweep (R60).
    fn clear(&self, form: i64, stored: &str) -> Result<()> {
        for table in ["entity_details", "entity_blob_refs"] {
            self.tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4"
                ),
                rusqlite::params![self.caller.project, form, stored, FAMILY],
            )?;
        }
        Ok(())
    }

    /// Inserts a new message at revision 1 and a fresh position. Objects of a
    /// text over 65,536 bytes are published and verified before the savepoint.
    pub(in crate::sqlite) fn create(&mut self, message: &Message, now_ms: i64) -> Result<Record> {
        self.active()?;
        if self.reader().get(&message.id)?.is_some() {
            return Err(StoreError::AlreadyExists {
                kind: EntityKind::Message,
                id: message.id.clone(),
            }
            .into());
        }
        let (form, stored) = key(&message.id);
        for table in ["entity_details", "entity_blob_refs"] {
            let orphan = self.tx.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table}
                     WHERE project_id=?1 AND key_form=?2 AND entity_key=?3 AND family=?4)"
                ),
                rusqlite::params![self.caller.project, form, stored, FAMILY],
                |row| row.get::<_, bool>(0),
            )?;
            if orphan {
                return Err(corrupt(form, "orphan row", "refs"));
            }
        }
        let position = self.next_position()?;
        let detail = self.encode(message)?;
        let mut names = columns();
        names.extend(["project_id".into(), "revision".into(), "position".into()]);
        let mut values = projection(message);
        values.extend([
            SqlValue::Text(self.caller.project.clone()),
            SqlValue::Integer(1),
            SqlValue::Integer(integer(position)?),
        ]);
        let slots = (1..=values.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        self.atomic(|| {
            self.tx.execute(
                &format!("INSERT INTO messages({}) VALUES({slots})", names.join(",")),
                params_from_iter(values),
            )?;
            self.install(message, &detail, now_ms)?;
            self.store_position(position)?;
            Ok(Record {
                entity: Entity::Message(Box::new(message.clone())),
                revision: Revision::ONE,
                position,
            })
        })
    }

    /// Replaces the whole message if the revision matches, keeping its position.
    pub(in crate::sqlite) fn update(
        &mut self,
        message: &Message,
        expected: Revision,
        now_ms: i64,
    ) -> Result<Record> {
        self.active()?;
        let old = self.current(&message.id, expected)?;
        let revision = expected.next()?;
        integer(revision.get())?;
        let detail = self.encode(message)?;
        let (form, stored) = key(&message.id);
        // The identity quartet is the row's key and is not assigned.
        let names = columns();
        let mut assignments = names[4..]
            .iter()
            .enumerate()
            .map(|(i, name)| format!("{name}=?{}", i + 4))
            .collect::<Vec<_>>();
        let mut values = vec![
            SqlValue::Text(self.caller.project.clone()),
            SqlValue::Integer(form),
            SqlValue::Text(stored),
        ];
        values.extend(projection(message).into_iter().skip(4));
        assignments.push(format!("revision=?{}", values.len() + 1));
        values.push(SqlValue::Integer(integer(revision.get())?));
        self.atomic(|| {
            self.tx.execute(
                &format!(
                    "UPDATE messages SET {} WHERE project_id=?1 AND id_form=?2 AND id=?3",
                    assignments.join(",")
                ),
                params_from_iter(values),
            )?;
            self.install(message, &detail, now_ms)?;
            Ok(Record {
                entity: Entity::Message(Box::new(message.clone())),
                revision,
                position: old.position,
            })
        })
    }

    /// Gives the message a new position after every one issued so far.
    pub(in crate::sqlite) fn move_to_end(
        &mut self,
        id: &str,
        expected: Revision,
    ) -> Result<Record> {
        self.active()?;
        let old = self.current(id, expected)?;
        let revision = expected.next()?;
        integer(revision.get())?;
        let position = self.next_position()?;
        let (form, stored) = key(id);
        self.atomic(|| {
            self.tx.execute(
                "UPDATE messages SET revision=?4,position=?5 WHERE project_id=?1 AND id_form=?2 AND id=?3",
                rusqlite::params![
                    self.caller.project,
                    form,
                    stored,
                    integer(revision.get())?,
                    integer(position)?
                ],
            )?;
            self.store_position(position)?;
            Ok(Record {
                entity: old.entity,
                revision,
                position,
            })
        })
    }

    /// Removes the row, the detail and the reference rows; objects stay for the
    /// sweep and the counter is never lowered.
    pub(in crate::sqlite) fn delete(&mut self, id: &str, expected: Revision) -> Result<()> {
        self.active()?;
        self.current(id, expected)?;
        let (form, stored) = key(id);
        self.atomic(|| {
            self.clear(form, &stored)?;
            self.tx.execute(
                "DELETE FROM messages WHERE project_id=?1 AND id_form=?2 AND id=?3",
                rusqlite::params![self.caller.project, form, stored],
            )?;
            Ok(())
        })
    }

    pub(in crate::sqlite) fn get(&self, id: &str) -> Result<Option<Record>> {
        self.active()?;
        self.reader().get(id)
    }

    pub(in crate::sqlite) fn select(
        &self,
        query: MessageSelection<'_>,
        after: Option<&Record>,
        limit: usize,
    ) -> Result<MessagePage> {
        self.active()?;
        self.reader().select(query, after, limit)
    }

    pub(in crate::sqlite) fn select_after(
        &self,
        query: MessageSelection<'_>,
        after: Option<&OrderKey<'_>>,
        limit: usize,
    ) -> Result<MessagePage> {
        self.active()?;
        self.reader().select_after(query, after, limit)
    }
}
