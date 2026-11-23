//! Private snapshot hydration. The caller supplies the authorized project and
//! the SAME transaction that selected page IDs. This leaf never opens a second
//! connection or returns a partial page. Storage composition must classify its
//! structured errors and immediately latch live corruption under R140; neither
//! public error conversion nor the shared failure state is implemented here.
use super::task_format::{
    codec::{self, CodecError, Detail},
    text::Chunk,
};
use super::{Result, SqliteError};
use crate::blob::{BlobHash, DiskBlobStore, INLINE_TEXT_MAX_BYTES, MAX_BLOB_BYTES};
use orch_core::store::{Entity, Record, Revision};
use orch_domain::Task;
use rusqlite::{OptionalExtension, Transaction, params, types::Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct TaskSql<'a, 'conn> {
    pub tx: &'a Transaction<'conn>,
    pub project: &'a str,
    pub disk: &'a DiskBlobStore,
}

fn corrupt(task: &str, check: &'static str, locator: &'static str) -> SqliteError {
    SqliteError::TaskCorrupt {
        task: task.into(),
        check,
        locator,
    }
}

// Column identifiers are compiled SQL, never stored keys or payload values.
const PROJECTIONS: [&str; 16] = [
    "id",
    "goal_id",
    "title",
    "description",
    "status",
    "priority",
    "assignee_agent_id",
    "attempts",
    "max_attempts",
    "workspace_mode",
    "awaiting_approval",
    "feedback",
    "created_at",
    "updated_at",
    "revision",
    "position",
];

impl TaskSql<'_, '_> {
    pub(super) fn get(&self, id: &str) -> Result<Option<Record>> {
        let native = self
            .tx
            .query_row(
                "SELECT id,goal_id,title,description,status,priority,assignee_agent_id,
             attempts,max_attempts,workspace_mode,awaiting_approval,feedback,
             created_at,updated_at,revision,position
             FROM tasks WHERE project_id=?1 AND id=?2",
                params![self.project, id],
                |r| {
                    (0..PROJECTIONS.len())
                        .map(|i| r.get::<_, Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                },
            )
            .optional()?;
        let Some(native) = native else {
            return Ok(None);
        };
        let detail = self.detail(id)?;
        // Check every registration before object access: proven metadata damage
        // must not be hidden by a transient error opening an earlier object.
        self.blobs(id, &detail)?;
        let task = codec::decode(self.disk, &detail).map_err(|error| match error {
            CodecError::Access {
                kind: std::io::ErrorKind::NotFound,
                ..
            } => corrupt(id, "object missing", "detail"),
            CodecError::Access { kind, .. } => SqliteError::TaskAccess(kind),
            // Do not echo locators from reference rows or huge array ordinals.
            _ => corrupt(id, "detail encoding", "detail"),
        })?;
        if task.id != id {
            return Err(corrupt(id, "identity", "id"));
        }
        let expected = projections(&task).map_err(|_| corrupt(id, "projection", "detail"))?;
        for (i, value) in expected.iter().enumerate() {
            if native[i] != *value {
                return Err(corrupt(id, "projection", PROJECTIONS[i]));
            }
        }
        let revision = positive(&native[14]).ok_or_else(|| corrupt(id, "counter", "revision"))?;
        let position = positive(&native[15]).ok_or_else(|| corrupt(id, "counter", "position"))?;
        let revision: Revision = serde_json::from_value(revision.into())
            .map_err(|_| corrupt(id, "counter", "revision"))?;
        self.memberships(id, &task)?;
        Ok(Some(Record {
            entity: Entity::Task(Box::new(task)),
            revision,
            position,
        }))
    }

    /// Missing selected IDs are damage. `collect` discards preceding records if
    /// any hydration fails. The caller must use this object's selecting tx.
    pub(super) fn hydrate(&self, ids: &[String]) -> Result<Vec<Record>> {
        ids.iter()
            .map(|id| {
                self.get(id)?
                    .ok_or_else(|| corrupt(id, "selected row missing", "id"))
            })
            .collect()
    }

    fn detail(&self, id: &str) -> Result<Detail> {
        let row = self.tx.query_row(
            "SELECT format,detail,detail_bytes,detail_sha256 FROM task_details WHERE project_id=?1 AND task_id=?2",
            params![self.project,id],
            |r| Ok((r.get::<_,Value>(0)?,r.get::<_,Value>(1)?,r.get::<_,Value>(2)?,r.get::<_,Value>(3)?)),
        ).optional()?.ok_or_else(|| corrupt(id, "detail missing", "detail"))?;
        let (Value::Integer(1), inline, Value::Integer(bytes), Value::Text(hash)) = row else {
            return Err(corrupt(id, "format or metadata", "detail"));
        };
        let inline = match inline {
            Value::Null if bytes > INLINE_TEXT_MAX_BYTES as i64 => None,
            Value::Text(text)
                if bytes >= 0
                    && bytes <= INLINE_TEXT_MAX_BYTES as i64
                    && text.len() as i64 == bytes =>
            {
                Some(text)
            }
            _ => return Err(corrupt(id, "detail size", "detail")),
        };
        if hash.parse::<BlobHash>().is_err() {
            return Err(corrupt(id, "hash syntax", "detail"));
        }
        let mut refs: BTreeMap<String, Vec<Chunk>> = BTreeMap::new();
        let mut stmt = self.tx.prepare(
            "SELECT locator,ordinal,sha256,size FROM task_blob_refs WHERE project_id=?1 AND task_id=?2 ORDER BY locator COLLATE BINARY,ordinal")?;
        let rows = stmt.query_map(params![self.project, id], |r| {
            Ok((
                r.get::<_, Value>(0)?,
                r.get::<_, Value>(1)?,
                r.get::<_, Value>(2)?,
                r.get::<_, Value>(3)?,
            ))
        })?;
        for row in rows {
            let (
                Value::Text(locator),
                Value::Integer(ordinal),
                Value::Text(sha256),
                Value::Integer(size),
            ) = row?
            else {
                return Err(corrupt(id, "reference metadata", "blob_ref"));
            };
            if ordinal < 0
                || size <= 0
                || size as u64 > MAX_BLOB_BYTES
                || sha256.parse::<BlobHash>().is_err()
            {
                return Err(corrupt(id, "reference metadata", "blob_ref"));
            }
            refs.entry(locator).or_default().push(Chunk {
                ordinal: ordinal as u64,
                sha256,
                size: size as u64,
            });
        }
        Ok(Detail {
            inline,
            bytes: bytes as u64,
            sha256: hash,
            refs,
        })
    }

    fn blobs(&self, id: &str, detail: &Detail) -> Result<()> {
        for chunks in detail.refs.values() {
            for chunk in chunks {
                let metadata = self
                    .tx
                    .query_row(
                        "SELECT size,storage_location FROM blobs WHERE project_id=?1 AND sha256=?2",
                        params![self.project, chunk.sha256],
                        |r| Ok((r.get::<_, Value>(0)?, r.get::<_, Value>(1)?)),
                    )
                    .optional()?;
                if metadata
                    != Some((
                        Value::Integer(chunk.size as i64),
                        Value::Text("disk".into()),
                    ))
                {
                    return Err(corrupt(id, "blob registration", "blob_ref"));
                }
            }
        }
        Ok(())
    }

    fn memberships(&self, id: &str, task: &Task) -> Result<()> {
        for (query, values, locator) in [
            (
                "SELECT depends_on_id FROM task_dependencies WHERE project_id=?1 AND task_id=?2",
                task.depends_on.as_slice(),
                "depends_on",
            ),
            (
                "SELECT label FROM task_labels WHERE project_id=?1 AND task_id=?2",
                task.labels.as_slice(),
                "labels",
            ),
            (
                "SELECT pattern FROM task_scopes WHERE project_id=?1 AND task_id=?2",
                task.scope.as_deref().unwrap_or_default(),
                "scope",
            ),
        ] {
            let expected: BTreeSet<_> = values
                .iter()
                .filter(|s| s.len() <= INLINE_TEXT_MAX_BYTES)
                .cloned()
                .collect();
            let mut actual = BTreeSet::new();
            let mut stmt = self.tx.prepare(query)?;
            for row in stmt.query_map(params![self.project, id], |r| r.get::<_, Value>(0))? {
                let Value::Text(text) = row? else {
                    return Err(corrupt(id, "membership type", locator));
                };
                actual.insert(text);
            }
            if actual != expected {
                return Err(corrupt(id, "membership", locator));
            }
        }
        Ok(())
    }
}

fn positive(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(n) if *n > 0 => Some(*n as u64),
        _ => None,
    }
}

fn nullable(value: Option<&str>) -> Value {
    value.map(|s| Value::Text(s.into())).unwrap_or(Value::Null)
}

fn inline(value: &str) -> &str {
    if value.len() <= INLINE_TEXT_MAX_BYTES {
        value
    } else {
        ""
    }
}

fn projections(task: &Task) -> std::result::Result<Vec<Value>, CodecError> {
    let mode = task.workspace_mode.map(|mode| mode.to_string());
    Ok(vec![
        Value::Text(task.id.clone()),
        nullable(task.goal_id.as_deref()),
        Value::Text(inline(&task.title).into()),
        Value::Text(inline(&task.description).into()),
        Value::Text(task.status.to_string()),
        Value::Integer(task.priority),
        nullable(task.assignee.as_deref()),
        Value::Integer(task.attempts.into()),
        Value::Integer(task.max_attempts.into()),
        nullable(mode.as_deref()),
        Value::Integer(i64::from(
            task.labels.iter().any(|s| s == "awaiting-approval"),
        )),
        nullable(
            task.feedback
                .as_deref()
                .filter(|s| s.len() <= INLINE_TEXT_MAX_BYTES),
        ),
        Value::Integer(codec::milliseconds(&task.created_at)?),
        Value::Integer(codec::milliseconds(&task.updated_at)?),
    ])
}

#[cfg(test)]
mod tests;
