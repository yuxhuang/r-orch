//! Project-bound lease operations on the caller's SQLite transaction.
//! The adapter owns admission, authorization and commit; these helpers neither
//! open connections nor read a clock. The v1 daemon still uses files.

use orch_core::store::leases::{ClaimOutcome, LEASE_MS, Lease, NewLease};
use orch_core::store::{Caller, Principal, StoreError};
use rusqlite::{OptionalExtension, Row, Transaction, params};

/// A lease failure that keeps the SQLite error structured. Typed refusals are
/// ordinary outcomes a caller may handle; `Sql` is an engine failure that must
/// reach the writer's classification and end the transaction, so it is never
/// flattened into a backend string inside a callback that then returns `Ok`.
/// `Corrupt` is a stored value outside its column's contract (no SQLite result
/// code): like `Sql` it ends the job rather than being offered as a refusal,
/// and unlike `StoreError::CounterExhausted` it says the data is damaged.
#[derive(Debug)]
pub(crate) enum LeaseError {
    Store(StoreError),
    Sql(rusqlite::Error),
    Corrupt(String),
}

impl From<StoreError> for LeaseError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for LeaseError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

/// Lets a composed writer job propagate a lease failure with `?` and keep the
/// SQLite code: the executor then rolls the whole job back and can classify it.
impl From<LeaseError> for crate::sqlite::SqliteError {
    fn from(error: LeaseError) -> Self {
        match error {
            LeaseError::Store(error) => Self::Store(error),
            LeaseError::Sql(error) => Self::Sql(error),
            LeaseError::Corrupt(detail) => Self::LeaseCorrupt(detail),
        }
    }
}

type Result<T> = std::result::Result<T, LeaseError>;

fn row(row: &Row<'_>) -> rusqlite::Result<Lease> {
    Ok(Lease {
        task: row.get(0)?,
        agent: row.get(1)?,
        run: row.get(2)?,
        lineage: row.get(3)?,
        epoch: u64::try_from(row.get::<_, i64>(4)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        expires_at: row.get(5)?,
    })
}

const COLUMNS: &str = "task_id, agent_id, run_id, lineage, epoch, expires_at";

pub(crate) fn get_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    task: &str,
) -> Result<Option<Lease>> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM leases WHERE project_id=?1 AND task_id=?2"),
        params![caller.project, task],
        row,
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn get_agent_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    agent: &str,
) -> Result<Option<Lease>> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM leases WHERE project_id=?1 AND agent_id=?2 ORDER BY task_id LIMIT 1"),
        params![caller.project, agent], row,
    ).optional().map_err(Into::into)
}

pub(crate) fn list_leases(tx: &Transaction<'_>, caller: &Caller) -> Result<Vec<Lease>> {
    let mut query = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM leases WHERE project_id=?1 ORDER BY task_id"
    ))?;
    Ok(query
        .query_map([&caller.project], row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn lineage(tx: &Transaction<'_>) -> Result<String> {
    tx.query_row(
        "SELECT value FROM daemon_config WHERE key='lineage'",
        [],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

fn expiry(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LEASE_MS)
        .ok_or(StoreError::CounterExhausted.into())
}

/// Fences both an explicit token and a task principal. The same check can be
/// used by event storage before it writes any rows for the lease's run.
pub(crate) fn check(
    tx: &Transaction<'_>,
    caller: &Caller,
    lease: &Lease,
    token_lineage: &str,
    epoch: u64,
) -> Result<()> {
    if token_lineage != lineage(tx)? || lease.lineage != token_lineage || lease.epoch != epoch {
        return Err(StoreError::StaleEpoch.into());
    }
    if let Principal::Task {
        task,
        run,
        lineage,
        epoch,
        ..
    } = &caller.principal
        && (task != &lease.task
            || run != &lease.run
            || lineage != &lease.lineage
            || *epoch != lease.epoch)
    {
        return Err(StoreError::StaleEpoch.into());
    }
    Ok(())
}

/// Requires the enclosing writer's BEGIN IMMEDIATE. The operation savepoint
/// also protects a caller that catches a SQL error and commits other work.
pub(crate) fn claim_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    new: &NewLease,
    now_ms: i64,
) -> Result<ClaimOutcome> {
    if let Some(held) = get_lease(tx, caller, &new.task)? {
        return Ok(ClaimOutcome::Held(held));
    }
    if let Some(held) = get_agent_lease(tx, caller, &new.agent)? {
        return Ok(ClaimOutcome::Held(held));
    }
    let held = tx.query_row(
        &format!("SELECT {COLUMNS} FROM leases WHERE project_id=?1 AND run_id=?2 ORDER BY task_id LIMIT 1"),
        params![caller.project, new.run], row,
    ).optional()?;
    if let Some(held) = held {
        return Ok(ClaimOutcome::Held(held));
    }
    let expires_at = expiry(now_ms)?;
    let lineage = lineage(tx)?;
    let epoch: i64 = tx
        .query_row(
            "SELECT next_epoch FROM task_epochs WHERE project_id=?1 AND task_id=?2",
            params![caller.project, new.task],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(1);
    // `task_epochs.next_epoch` is the epoch the next claim issues: 1 for a task
    // with no run, else one more than the largest epoch issued (spec "Import":
    // `next_epoch` is one more than the largest; `orch db verify`: every epoch
    // is below its task's `next_epoch`). Zero or a negative value is damage, not
    // exhaustion: granting it would issue epoch 0 or, through `as u64`, a huge
    // one. Nothing has been written yet, so the refusal changes no row.
    if epoch < 1 {
        return Err(LeaseError::Corrupt(format!(
            "task_epochs.next_epoch is {epoch} for task {}; it must be at least 1",
            new.task
        )));
    }
    let next = epoch.checked_add(1).ok_or(StoreError::CounterExhausted)?;
    // rusqlite's Savepoint requires &mut Connection, while the executor lends
    // &Transaction. A fixed, internal name is safe: this synchronous operation
    // never calls another lease operation inside its savepoint.
    tx.execute_batch("SAVEPOINT orch_lease_claim")?;
    let result = (|| -> rusqlite::Result<()> {
        tx.execute("INSERT INTO task_epochs(project_id,task_id,next_epoch) VALUES(?1,?2,?3) ON CONFLICT(project_id,task_id) DO UPDATE SET next_epoch=excluded.next_epoch", params![caller.project, new.task, next])?;
        tx.execute("INSERT INTO leases(project_id,task_id,run_id,agent_id,worker_id,epoch,lineage,state,granted_at,renewed_at,expires_at) VALUES(?1,?2,?3,?4,'local_worker',?5,?6,'active',?7,?7,?8)", params![caller.project, new.task, new.run, new.agent, epoch, lineage, now_ms, expires_at])?;
        tx.execute_batch("RELEASE orch_lease_claim")
    })();
    if let Err(operation) = result {
        if let Err(cleanup) =
            tx.execute_batch("ROLLBACK TO orch_lease_claim; RELEASE orch_lease_claim")
        {
            // The borrowed Transaction cannot install hooks or mark its writer
            // poisoned. Unwind instead of returning a catchable operation error,
            // so a caller can never ignore the failed cleanup and commit. The
            // unwind is typed: the containing catch reports both errors to the
            // failure state (a fatal cause latches) and raises it again, and the
            // writer ends, drops the transaction and connection and closes receipts.
            crate::sqlite::writer::contain::cleanup_failed(operation, cleanup);
        }
        return Err(operation.into());
    }
    Ok(ClaimOutcome::Granted(Lease {
        task: new.task.clone(),
        agent: new.agent.clone(),
        run: new.run.clone(),
        lineage,
        epoch: epoch as u64,
        expires_at,
    }))
}

pub(crate) fn renew_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    task: &str,
    lineage: &str,
    epoch: u64,
    now_ms: i64,
) -> Result<Lease> {
    let mut lease = get_lease(tx, caller, task)?.ok_or(StoreError::StaleEpoch)?;
    check(tx, caller, &lease, lineage, epoch)?;
    let expires_at = expiry(now_ms)?;
    tx.execute(
        "UPDATE leases SET renewed_at=?3, expires_at=?4 WHERE project_id=?1 AND task_id=?2",
        params![caller.project, task, now_ms, expires_at],
    )?;
    lease.expires_at = expires_at;
    Ok(lease)
}

pub(crate) fn release_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    task: &str,
    lineage: &str,
    epoch: u64,
) -> Result<Lease> {
    let lease = get_lease(tx, caller, task)?.ok_or(StoreError::StaleEpoch)?;
    check(tx, caller, &lease, lineage, epoch)?;
    tx.execute(
        "DELETE FROM leases WHERE project_id=?1 AND task_id=?2",
        params![caller.project, task],
    )?;
    Ok(lease)
}

#[cfg(test)]
mod tests;

mod adapter;
#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod executor_tests;

#[cfg(test)]
mod classifier_tests;
