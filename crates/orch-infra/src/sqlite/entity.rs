//! Private, schema-native goal rows for the future SQLite adapter.
//!
//! These helpers borrow the caller's connection/transaction, never commit it,
//! and do not implement a public port. Timestamps are the schema's integer UTC
//! milliseconds: conversion to domain strings belongs to adapter integration.
//! Only columns represented by port v2's Goal are updated; later-phase columns
//! are preserved. No list/cursor or operational contract is introduced here.
use orch_core::store::{EntityKind, Revision, StoreError};
use orch_domain::GoalStatus;
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// Keep SQL errors intact for the writer's eventual failure classification.
#[derive(Debug, thiserror::Error)]
pub(super) enum EntityError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}
type Result<T> = std::result::Result<T, EntityError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GoalRow {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    pub assignee: Option<String>,
    pub created_at: i64,
    pub updated_at: Option<i64>,
    pub revision: Revision,
    pub position: u64,
}

fn invalid(column: usize, detail: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Integer,
        std::io::Error::new(std::io::ErrorKind::InvalidData, detail).into(),
    )
}

pub(super) fn get_goal(conn: &Connection, project: &str, id: &str) -> Result<Option<GoalRow>> {
    Ok(conn.query_row(
        "SELECT id,title,description,status,assignee_agent_id,created_at,updated_at,revision,position
         FROM goals WHERE project_id=?1 AND id=?2",
        params![project, id],
        |r| {
            let status: String = r.get(3)?;
            let status = status.parse().map_err(|()| rusqlite::Error::FromSqlConversionFailure(
                3, rusqlite::types::Type::Text,
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid goal status").into(),
            ))?;
            let revision = serde_json::from_value(r.get::<_, i64>(7)?.into())
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Integer, Box::new(e)))?;
            let position = r.get::<_, i64>(8)?;
            if position <= 0 { return Err(invalid(8, "goal position must be positive")); }
            Ok(GoalRow {
                id: r.get(0)?, title: r.get(1)?, description: r.get(2)?, status,
                assignee: r.get(4)?, created_at: r.get(5)?, updated_at: r.get(6)?, revision, position: position as u64,
            })
        },
    ).optional()?)
}

fn current(tx: &Transaction<'_>, project: &str, id: &str, expected: Revision) -> Result<GoalRow> {
    match get_goal(tx, project, id)? {
        Some(row) if row.revision == expected => Ok(row),
        other => Err(StoreError::StaleRevision {
            kind: EntityKind::Goal,
            id: id.to_owned(),
            expected,
            actual: other.map(|r| r.revision),
        }
        .into()),
    }
}

// SQLite INTEGER is signed. Refuse at its boundary instead of silently storing
// a REAL or wrapping a positive port counter. Full u64 support is not claimed.
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::CounterExhausted.into())
}

fn next_revision(row: &GoalRow) -> Result<Revision> {
    let revision = row.revision.next()?;
    integer(revision.get())?;
    Ok(revision)
}

// Internal counter namespace, shared by goal create and move only. Never use
// MAX(position): deletions must not make an issued position available again.
const GOAL_POSITION: &str = "position:goal";

fn next_position(tx: &Transaction<'_>, project: &str) -> Result<u64> {
    let last: i64 = tx
        .query_row(
            "SELECT value FROM counters WHERE project_id=?1 AND name=?2",
            params![project, GOAL_POSITION],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if last < 0 {
        return Err(invalid(0, "negative goal position counter").into());
    }
    let next = last.checked_add(1).ok_or(StoreError::CounterExhausted)?;
    Ok(next as u64)
}

fn store_position(tx: &Transaction<'_>, project: &str, position: u64) -> Result<()> {
    tx.execute(
        "INSERT INTO counters(project_id,name,value) VALUES(?1,?2,?3)
         ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value",
        params![project, GOAL_POSITION, integer(position)?],
    )?;
    Ok(())
}

/// A caught operation error must undo its row *and* counter, while keeping
/// earlier successful operations of the outer unit of work. All SQL is fixed;
/// the callback is private synchronous code, never supplied by a caller.
fn atomic<T>(tx: &Transaction<'_>, work: impl FnOnce() -> Result<T>) -> Result<T> {
    tx.execute_batch("SAVEPOINT goal_entity")?;
    let result = work().and_then(|value| {
        tx.execute_batch("RELEASE goal_entity")?;
        Ok(value)
    });
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            // A rollback failure is surfaced as SQL, never the caught semantic
            // error; the adapter must abort the outer transaction in that case.
            tx.execute_batch("ROLLBACK TO goal_entity; RELEASE goal_entity")?;
            Err(error)
        }
    }
}

/// Input metadata is ignored: creation always allocates position and revision 1.
pub(super) fn create_goal(tx: &Transaction<'_>, project: &str, goal: &GoalRow) -> Result<GoalRow> {
    if get_goal(tx, project, &goal.id)?.is_some() {
        return Err(StoreError::AlreadyExists {
            kind: EntityKind::Goal,
            id: goal.id.clone(),
        }
        .into());
    }
    let row = GoalRow {
        revision: Revision::ONE,
        position: next_position(tx, project)?,
        ..goal.clone()
    };
    atomic(tx, || {
        tx.execute(
            "INSERT INTO goals(project_id,id,title,description,status,assignee_agent_id,created_at,updated_at,revision,position)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9)",
            params![project, row.id, row.title, row.description, row.status.as_str(), row.assignee,
                row.created_at, row.updated_at, integer(row.position)?],
        )?;
        store_position(tx, project, row.position)?;
        Ok(row.clone())
    })
}

pub(super) fn update_goal(
    tx: &Transaction<'_>,
    project: &str,
    goal: &GoalRow,
    expected: Revision,
) -> Result<GoalRow> {
    let old = current(tx, project, &goal.id, expected)?;
    let row = GoalRow {
        revision: next_revision(&old)?,
        position: old.position,
        ..goal.clone()
    };
    atomic(tx, || {
        tx.execute(
            "UPDATE goals SET title=?3,description=?4,status=?5,assignee_agent_id=?6,created_at=?7,updated_at=?8,revision=?9
             WHERE project_id=?1 AND id=?2",
            params![project, row.id, row.title, row.description, row.status.as_str(), row.assignee,
                row.created_at, row.updated_at, integer(row.revision.get())?],
        )?;
        Ok(row.clone())
    })
}

pub(super) fn delete_goal(
    tx: &Transaction<'_>,
    project: &str,
    id: &str,
    expected: Revision,
) -> Result<()> {
    current(tx, project, id, expected)?;
    atomic(tx, || {
        tx.execute(
            "DELETE FROM goals WHERE project_id=?1 AND id=?2",
            params![project, id],
        )?;
        Ok(())
    })
}

pub(super) fn move_goal_to_end(
    tx: &Transaction<'_>,
    project: &str,
    id: &str,
    expected: Revision,
) -> Result<GoalRow> {
    let old = current(tx, project, id, expected)?;
    let row = GoalRow {
        revision: next_revision(&old)?,
        position: next_position(tx, project)?,
        ..old
    };
    atomic(tx, || {
        tx.execute(
            "UPDATE goals SET revision=?3,position=?4 WHERE project_id=?1 AND id=?2",
            params![
                project,
                id,
                integer(row.revision.get())?,
                integer(row.position)?
            ],
        )?;
        store_position(tx, project, row.position)?;
        Ok(row.clone())
    })
}

#[cfg(test)]
mod tests;
