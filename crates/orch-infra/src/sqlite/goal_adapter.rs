//! Internal goal-only persistence through the real executor. This is not
//! `Storage`: no authorization, list/cursor, blob or shared fatal policy is
//! implied. Trusted callers supply the project, never raw SQL. The daemon
//! continues to use files until every port method and safeguard is proven.
//!
//! The schema's signed counters and millisecond timestamps cover less than
//! port v2's u64 counters and opaque timestamp strings. Reject loss instead of
//! truncating or silently normalizing. Only canonical millisecond UTC strings
//! round-trip here. Broader port acceptance must be settled before wiring.
use super::entity::{self, EntityError, GoalRow};
use super::{Lane, ReadPool, Result, SqliteError, WriteReceipt, WriterHandle};
use orch_core::store::{Entity, Record, Revision};
use orch_domain::Goal;
use rusqlite::Transaction;
use std::time::Instant;

pub(super) struct GoalAdapter<'a, 'data> {
    writer: &'a WriterHandle,
    readers: &'a ReadPool<'data>,
}

impl<'a, 'data> GoalAdapter<'a, 'data> {
    pub(super) fn new(writer: &'a WriterHandle, readers: &'a ReadPool<'data>) -> Self {
        Self { writer, readers }
    }

    /// One callback is one writer transaction. Returning an error rolls back
    /// every successful operation in it; receipts acknowledge only commit.
    pub(super) fn write<T: Send + 'static>(
        &self,
        project: String,
        deadline: Instant,
        work: impl FnOnce(&mut GoalWrite<'_, '_>) -> Result<T> + Send + 'static,
    ) -> Result<WriteReceipt<T>> {
        self.writer
            .submit(Lane::Entities, Some(deadline), move |tx| {
                let mut unit = GoalWrite {
                    read: GoalRead {
                        tx,
                        project: &project,
                    },
                    poisoned: false,
                };
                let result = work(&mut unit);
                if (unit.poisoned || unit.read.tx.is_autocommit()) && result.is_ok() {
                    return Err(SqliteError::TransactionAborted);
                }
                result
            })
    }

    /// All gets in this callback use one pool connection and one snapshot.
    /// Owned results may escape; the connection and transaction may not.
    pub(super) fn read<T>(
        &self,
        project: &str,
        deadline: Instant,
        read: impl FnOnce(&GoalRead<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        self.readers
            .read(deadline, |tx| read(&GoalRead { tx, project }))
    }
}

pub(super) struct GoalRead<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    project: &'a str,
}

impl GoalRead<'_, '_> {
    pub(super) fn get(&self, id: &str) -> Result<Option<Record>> {
        entity::get_goal(self.tx, self.project, id)
            .map_err(classify)?
            .map(decode)
            .transpose()
    }
}

pub(super) struct GoalWrite<'a, 'conn> {
    read: GoalRead<'a, 'conn>,
    poisoned: bool,
}

impl GoalWrite<'_, '_> {
    fn usable(&self) -> Result<()> {
        if self.poisoned || self.read.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    fn operation<T>(&mut self, result: std::result::Result<T, EntityError>) -> Result<T> {
        if matches!(&result, Err(EntityError::Rollback { .. })) {
            self.poisoned = true;
        }
        result.map_err(classify)
    }

    pub(super) fn get(&self, id: &str) -> Result<Option<Record>> {
        self.usable()?;
        self.read.get(id)
    }

    pub(super) fn create(&mut self, goal: Goal) -> Result<Record> {
        self.usable()?;
        let row = encode(&goal)?;
        let result = entity::create_goal(self.read.tx, self.read.project, &row);
        self.operation(result).and_then(decode)
    }

    pub(super) fn update(&mut self, goal: Goal, expected: Revision) -> Result<Record> {
        self.usable()?;
        let row = encode(&goal)?;
        let result = entity::update_goal(self.read.tx, self.read.project, &row, expected);
        self.operation(result).and_then(decode)
    }

    pub(super) fn delete(&mut self, id: &str, expected: Revision) -> Result<()> {
        self.usable()?;
        let result = entity::delete_goal(self.read.tx, self.read.project, id, expected);
        self.operation(result)
    }

    pub(super) fn move_to_end(&mut self, id: &str, expected: Revision) -> Result<Record> {
        self.usable()?;
        // Check conversion before mutation: a caught decode error must leave
        // the operation unchanged, including its position allocation.
        self.get(id)?;
        let result = entity::move_goal_to_end(self.read.tx, self.read.project, id, expected);
        self.operation(result).and_then(decode)
    }
}

fn classify(error: EntityError) -> SqliteError {
    match error {
        EntityError::Sql(error) => SqliteError::Sql(error),
        EntityError::Store(error) => SqliteError::Store(error),
        EntityError::Rollback {
            operation,
            rollback,
        } => SqliteError::OperationRollback {
            operation: Box::new(classify(*operation)),
            rollback,
        },
    }
}

fn timestamp(ms: i64) -> Result<String> {
    jiff::Timestamp::from_millisecond(ms)
        .map(|time| format!("{time:.3}"))
        .map_err(|_| SqliteError::Unrepresentable("goal timestamp outside API range".into()))
}

fn milliseconds(text: &str) -> Result<i64> {
    let parsed = text
        .parse::<jiff::Timestamp>()
        .map_err(|_| SqliteError::Unrepresentable("invalid goal timestamp".into()))?;
    let ms = parsed.as_millisecond();
    if timestamp(ms)? != text {
        return Err(SqliteError::Unrepresentable(
            "goal timestamp must round-trip as millisecond UTC".into(),
        ));
    }
    Ok(ms)
}

fn encode(goal: &Goal) -> Result<GoalRow> {
    Ok(GoalRow {
        id: goal.id.clone(),
        title: goal.title.clone(),
        description: goal.description.clone(),
        status: goal.status,
        assignee: goal.assignee.clone(),
        created_at: milliseconds(&goal.created_at)?,
        updated_at: goal.updated_at.as_deref().map(milliseconds).transpose()?,
        revision: Revision::ONE,
        position: 0,
    })
}

fn decode(row: GoalRow) -> Result<Record> {
    Ok(Record {
        entity: Entity::Goal(Box::new(Goal {
            id: row.id,
            title: row.title,
            description: row.description,
            status: row.status,
            assignee: row.assignee,
            created_at: timestamp(row.created_at)?,
            updated_at: row.updated_at.map(timestamp).transpose()?,
        })),
        revision: row.revision,
        position: row.position,
    })
}

#[cfg(test)]
mod tests;
