//! Project-bound lease operations on the caller's SQLite transaction.
//! The adapter owns admission, authorization and commit; these helpers neither
//! open connections nor read a clock. The v1 daemon still uses files.

use orch_core::store::leases::{ClaimOutcome, LEASE_MS, Lease, NewLease};
use orch_core::store::{Caller, Principal, Result, StoreError};
use rusqlite::{OptionalExtension, Row, Transaction, params};

fn backend(error: rusqlite::Error) -> StoreError {
    StoreError::Backend(orch_core::Error::other(format!("SQLite lease: {error}")))
}

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
    .map_err(backend)
}

pub(crate) fn get_agent_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    agent: &str,
) -> Result<Option<Lease>> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM leases WHERE project_id=?1 AND agent_id=?2 ORDER BY task_id LIMIT 1"),
        params![caller.project, agent], row,
    ).optional().map_err(backend)
}

pub(crate) fn list_leases(tx: &Transaction<'_>, caller: &Caller) -> Result<Vec<Lease>> {
    let mut query = tx
        .prepare(&format!(
            "SELECT {COLUMNS} FROM leases WHERE project_id=?1 ORDER BY task_id"
        ))
        .map_err(backend)?;
    query
        .query_map([&caller.project], row)
        .map_err(backend)?
        .collect::<rusqlite::Result<_>>()
        .map_err(backend)
}

fn lineage(tx: &Transaction<'_>) -> Result<String> {
    tx.query_row(
        "SELECT value FROM daemon_config WHERE key='lineage'",
        [],
        |r| r.get(0),
    )
    .map_err(backend)
}

fn expiry(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LEASE_MS)
        .ok_or(StoreError::CounterExhausted)
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
        return Err(StoreError::StaleEpoch);
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
        return Err(StoreError::StaleEpoch);
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
    ).optional().map_err(backend)?;
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
        .optional()
        .map_err(backend)?
        .unwrap_or(1);
    let next = epoch
        .checked_add(1)
        .filter(|_| epoch > 0)
        .ok_or(StoreError::CounterExhausted)?;
    // rusqlite's Savepoint requires &mut Connection, while the executor lends
    // &Transaction. A fixed, internal name is safe: this synchronous operation
    // never calls another lease operation inside its savepoint.
    tx.execute_batch("SAVEPOINT orch_lease_claim")
        .map_err(backend)?;
    struct Rollback<'a>(&'a Transaction<'a>);
    impl Drop for Rollback<'_> {
        fn drop(&mut self) {
            let _ = self
                .0
                .execute_batch("ROLLBACK TO orch_lease_claim; RELEASE orch_lease_claim");
        }
    }
    let rollback = Rollback(tx);
    tx.execute("INSERT INTO task_epochs(project_id,task_id,next_epoch) VALUES(?1,?2,?3) ON CONFLICT(project_id,task_id) DO UPDATE SET next_epoch=excluded.next_epoch", params![caller.project, new.task, next]).map_err(backend)?;
    tx.execute("INSERT INTO leases(project_id,task_id,run_id,agent_id,worker_id,epoch,lineage,state,granted_at,renewed_at,expires_at) VALUES(?1,?2,?3,?4,'local_worker',?5,?6,'active',?7,?7,?8)", params![caller.project, new.task, new.run, new.agent, epoch, lineage, now_ms, expires_at]).map_err(backend)?;
    tx.execute_batch("RELEASE orch_lease_claim")
        .map_err(backend)?;
    std::mem::forget(rollback);
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
    )
    .map_err(backend)?;
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
    )
    .map_err(backend)?;
    Ok(lease)
}

#[cfg(test)]
mod tests;
