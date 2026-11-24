//! Private writes on an already authorized and fenced caller's transaction.
//! This leaf grants no authority. Composition must enforce `ensure_committable`
//! and classify every structured result before public conversion (#127).
use super::super::task_format::text::TextError;
use super::*;
use crate::blob::{BlobReferences, ReferenceError};
use orch_core::store::{EntityKind, Principal, StoreError};

pub(in crate::sqlite) struct TaskWrite<'a, 'conn> {
    sql: TaskSql<'a, 'conn>,
    principal: &'a Principal,
    poisoned: bool,
}

impl<'a, 'conn> TaskWrite<'a, 'conn> {
    /// Both project and principal come from the authorized, fenced caller, never
    /// from the new task. The surrounding Storage adapter owns that check.
    pub(in crate::sqlite) fn new(sql: TaskSql<'a, 'conn>, principal: &'a Principal) -> Self {
        Self {
            sql,
            principal,
            poisoned: false,
        }
    }

    pub(in crate::sqlite) fn ensure_committable(&self) -> Result<()> {
        if self.poisoned || self.sql.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    fn atomic<T>(&mut self, work: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.ensure_committable()?;
        self.sql.tx.execute_batch("SAVEPOINT task_entity")?;
        let result = work(self).and_then(|value| {
            self.sql.tx.execute_batch("RELEASE task_entity")?;
            Ok(value)
        });
        match result {
            Ok(value) => Ok(value),
            Err(operation) => match self
                .sql
                .tx
                .execute_batch("ROLLBACK TO task_entity; RELEASE task_entity")
            {
                Ok(()) => Err(operation),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(SqliteError::OperationRollback {
                        operation: Box::new(operation),
                        rollback,
                    })
                }
            },
        }
    }

    fn current(&self, id: &str, expected: Revision) -> Result<(Revision, u64)> {
        self.ensure_committable()?;
        let row = stored(
            id,
            "tasks",
            self.sql
                .tx
                .query_row(
                    "SELECT revision,position FROM tasks WHERE project_id=?1 AND id=?2",
                    params![self.sql.project, id],
                    |r| Ok((r.get::<_, Value>(0)?, r.get::<_, Value>(1)?)),
                )
                .optional(),
        )?;
        let actual = row
            .map(|(revision, position)| {
                let revision =
                    positive(&revision).ok_or_else(|| corrupt(id, "counter", "revision"))?;
                let position =
                    positive(&position).ok_or_else(|| corrupt(id, "counter", "position"))?;
                let revision = serde_json::from_value(revision.into())
                    .map_err(|_| corrupt(id, "counter", "revision"))?;
                Ok::<_, SqliteError>((revision, position))
            })
            .transpose()?;
        match actual {
            Some(row) if row.0 == expected => Ok(row),
            other => Err(StoreError::StaleRevision {
                kind: EntityKind::Task,
                id: id.into(),
                expected,
                actual: other.map(|r| r.0),
            }
            .into()),
        }
    }

    pub(in crate::sqlite) fn create(&mut self, task: Task) -> Result<Record> {
        self.ensure_committable()?;
        let existing = stored(
            &task.id,
            "id",
            self.sql
                .tx
                .query_row(
                    "SELECT id FROM tasks WHERE project_id=?1 AND id=?2 COLLATE NOCASE",
                    params![self.sql.project, task.id],
                    |r| r.get::<_, String>(0),
                )
                .optional(),
        )?;
        if let Some(existing) = existing {
            return Err(if existing == task.id {
                StoreError::AlreadyExists {
                    kind: EntityKind::Task,
                    id: task.id,
                }
            } else {
                StoreError::IdAlias {
                    existing,
                    requested: task.id,
                }
            }
            .into());
        }
        codec::validate(&task).map_err(write_error)?;
        let position = self.next_position()?;
        let (kind, run) = match self.principal {
            Principal::Owner { .. } => ("owner", None),
            Principal::Worker { .. } => ("daemon", None),
            Principal::Task { run, .. } => ("agent", Some(run.as_str())),
        };
        let detail = codec::encode(self.sql.disk, &task).map_err(write_error)?;
        self.atomic(|this| {
            let mut values = vec![Value::Text(this.sql.project.into())];
            values.extend(projections(&task).map_err(write_error)?);
            values.extend([Value::Integer(integer(position)?), Value::Text(kind.into()), nullable(run)]);
            this.sql.tx.execute(
                "INSERT INTO tasks(project_id,id,goal_id,title,description,status,priority,assignee_agent_id,attempts,max_attempts,workspace_mode,awaiting_approval,feedback,created_at,updated_at,revision,position,created_by_kind,created_by_run_id,landing_state)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,1,?16,?17,?18,CASE WHEN ?6='done' THEN 'skipped' ELSE NULL END)",
                rusqlite::params_from_iter(values),
            )?;
            this.install(&task, &detail)?;
            this.store_position(position)?;
            Ok(Record { entity: Entity::Task(Box::new(task.clone())), revision: Revision::ONE, position })
        })
    }

    pub(in crate::sqlite) fn update(&mut self, task: Task, expected: Revision) -> Result<Record> {
        let (_, position) = self.current(&task.id, expected)?;
        let revision = next_revision(expected)?;
        codec::validate(&task).map_err(write_error)?;
        let detail = codec::encode(self.sql.disk, &task).map_err(write_error)?;
        self.atomic(|this| {
            let mut values = vec![Value::Text(this.sql.project.into())];
            values.extend(projections(&task).map_err(write_error)?);
            values.push(Value::Integer(integer(revision.get())?));
            this.sql.tx.execute(
                "UPDATE tasks SET goal_id=?3,title=?4,description=?5,status=?6,priority=?7,assignee_agent_id=?8,attempts=?9,max_attempts=?10,workspace_mode=?11,awaiting_approval=?12,feedback=?13,created_at=?14,updated_at=?15,revision=?16,
                 landing_state=CASE WHEN ?6='done' AND landing_state IS NULL THEN 'skipped' ELSE landing_state END WHERE project_id=?1 AND id=?2",
                rusqlite::params_from_iter(values),
            )?;
            this.install(&task, &detail)?;
            Ok(Record { entity: Entity::Task(Box::new(task.clone())), revision, position })
        })
    }

    /// Ordering only: deliberately does not hydrate or publish any object.
    /// The caller may hydrate the result separately in this same transaction.
    pub(in crate::sqlite) fn move_to_end(
        &mut self,
        id: &str,
        expected: Revision,
    ) -> Result<(Revision, u64)> {
        self.current(id, expected)?;
        let revision = next_revision(expected)?;
        let position = self.next_position()?;
        self.atomic(|this| {
            this.sql.tx.execute(
                "UPDATE tasks SET revision=?3,position=?4 WHERE project_id=?1 AND id=?2",
                params![
                    this.sql.project,
                    id,
                    integer(revision.get())?,
                    integer(position)?
                ],
            )?;
            this.store_position(position)?;
            Ok((revision, position))
        })
    }

    pub(in crate::sqlite) fn delete(&mut self, id: &str, expected: Revision) -> Result<()> {
        self.current(id, expected)?;
        self.atomic(|this| {
            for query in [
                "DELETE FROM task_attachments WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM task_blob_refs WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM task_details WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM task_dependencies WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM task_labels WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM task_scopes WHERE project_id=?1 AND task_id=?2",
                "DELETE FROM tasks WHERE project_id=?1 AND id=?2",
            ] {
                this.sql.tx.execute(query, params![this.sql.project, id])?;
            }
            Ok(())
        })
    }

    fn install(&self, task: &Task, detail: &Detail) -> Result<()> {
        let tx = self.sql.tx;
        let project = self.sql.project;
        for query in [
            "DELETE FROM task_details WHERE project_id=?1 AND task_id=?2",
            "DELETE FROM task_blob_refs WHERE project_id=?1 AND task_id=?2",
            "DELETE FROM task_dependencies WHERE project_id=?1 AND task_id=?2",
            "DELETE FROM task_labels WHERE project_id=?1 AND task_id=?2",
            "DELETE FROM task_scopes WHERE project_id=?1 AND task_id=?2",
        ] {
            tx.execute(query, params![project, task.id])?;
        }
        tx.execute(
            "INSERT INTO task_details VALUES(?1,?2,1,?3,?4,?5)",
            params![
                project,
                task.id,
                detail.inline,
                integer(detail.bytes)?,
                detail.sha256
            ],
        )?;
        let now = codec::milliseconds(&task.updated_at).map_err(write_error)?;
        for (locator, chunks) in &detail.refs {
            for chunk in chunks {
                // `publish` obtained both the digest and the checked slice length
                // from the successful put. Never use the source string's length.
                let hash = chunk
                    .sha256
                    .parse::<BlobHash>()
                    .map_err(|_| corrupt(&task.id, "published hash", "blob_ref"))?;
                BlobReferences::register_published(tx, project, &hash, integer(chunk.size)?, now)
                    .map_err(|error| match error {
                    ReferenceError::Sql(error) => SqliteError::Sql(error),
                    _ => corrupt(&task.id, "blob registration", "blob_ref"),
                })?;
                tx.execute(
                    "INSERT INTO task_blob_refs VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        project,
                        task.id,
                        locator,
                        integer(chunk.ordinal)?,
                        chunk.sha256,
                        integer(chunk.size)?
                    ],
                )?;
            }
        }
        for (query, values) in [
            (
                "INSERT INTO task_dependencies VALUES(?1,?2,?3)",
                task.depends_on.as_slice(),
            ),
            (
                "INSERT INTO task_labels VALUES(?1,?2,?3)",
                task.labels.as_slice(),
            ),
            (
                "INSERT INTO task_scopes VALUES(?1,?2,?3)",
                task.scope.as_deref().unwrap_or_default(),
            ),
        ] {
            let values: BTreeSet<_> = values
                .iter()
                .filter(|value| value.len() <= INLINE_TEXT_MAX_BYTES)
                .collect();
            for value in values {
                tx.execute(query, params![project, task.id, value])?;
            }
        }
        Ok(())
    }

    fn next_position(&self) -> Result<u64> {
        let counter: Option<i64> = self
            .sql
            .tx
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name='position:task'",
                [self.sql.project],
                |r| r.get(0),
            )
            .optional()?;
        let last = counter.unwrap_or(0);
        if last < 0 {
            return Err(SqliteError::CounterNegative {
                project: self.sql.project.into(),
                name: "position:task",
                value: last,
            });
        }
        let (minimum, maximum): (Option<i64>, Option<i64>) = self.sql.tx.query_row(
            "SELECT MIN(position),MAX(position) FROM tasks WHERE project_id=?1",
            [self.sql.project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if minimum.is_some_and(|value| value <= 0) {
            return Err(corrupt("", "counter", "position"));
        }
        if maximum.is_some_and(|value| value > last) {
            return Err(if counter.is_none() {
                SqliteError::CounterMissing {
                    project: self.sql.project.into(),
                    name: "position:task",
                }
            } else {
                SqliteError::CounterBehind {
                    project: self.sql.project.into(),
                    name: "position:task",
                    value: last,
                }
            });
        }
        Ok(last.checked_add(1).ok_or(StoreError::CounterExhausted)? as u64)
    }

    fn store_position(&self, position: u64) -> Result<()> {
        self.sql.tx.execute("INSERT INTO counters VALUES(?1,'position:task',?2) ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value",
            params![self.sql.project,integer(position)?])?;
        Ok(())
    }
}

fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::CounterExhausted.into())
}

fn next_revision(value: Revision) -> Result<Revision> {
    let next = value.next()?;
    integer(next.get())?;
    Ok(next)
}

fn write_error(error: CodecError) -> SqliteError {
    match error {
        CodecError::Text(TextError::Publish(error)) => SqliteError::TaskPublication(error),
        CodecError::Text(TextError::Access(kind)) | CodecError::Access { kind, .. } => {
            SqliteError::TaskAccess(kind)
        }
        CodecError::Unrepresentable(check) => SqliteError::Unrepresentable(check.into()),
        _ => SqliteError::Unrepresentable("task encoding".into()),
    }
}
