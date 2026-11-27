//! Typed intents on the enclosing writer transaction, with no external effects.
//! Authorization and the borrowed `UnitOfWork` facade remain integration duties.
use orch_core::store::intents::{
    DONE_RETENTION_MS, FinishOutcome, Intent, IntentPayload, check_bound,
};
use orch_core::store::{Caller, StoreError};
use rusqlite::{OptionalExtension, Row, Transaction, params};

use super::{Result, SqliteError, leases};

const COUNTER: &str = "intent";
const COLUMNS: &str = "id,kind,run_id,length(CAST(payload AS BLOB)),payload,created_at,done_at";

pub(super) struct Intents<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
}

fn corrupt(detail: &'static str) -> SqliteError {
    SqliteError::IntentCorrupt(detail)
}

fn invalid() -> SqliteError {
    StoreError::InvalidIntent("intent id was never issued in this project".into()).into()
}

fn row(row: &Row<'_>) -> Result<Intent> {
    let id: i64 = row.get(0)?;
    let bytes: i64 = row.get(3)?;
    // The closed payload holds at most one 128-byte ASCII id or 64-byte hash.
    // Check before allocating or parsing text from a damaged database.
    if id <= 0 || !(1..=256).contains(&bytes) {
        return Err(corrupt("invalid id or payload size"));
    }
    let payload: String = row.get(4)?;
    let payload: IntentPayload =
        serde_json::from_str(&payload).map_err(|_| corrupt("invalid typed payload"))?;
    payload
        .validate()
        .map_err(|_| corrupt("invalid payload value"))?;
    let kind: String = row.get(1)?;
    let run: Option<String> = row.get(2)?;
    if kind != payload.kind().as_str() || run.as_deref() != payload.run() {
        return Err(corrupt("kind or run disagrees with payload"));
    }
    Ok(Intent {
        id: id as u64,
        payload,
        created_at: row.get(5)?,
        done_at: row.get(6)?,
    })
}

impl<'a, 'conn> Intents<'a, 'conn> {
    pub(super) fn new(tx: &'a Transaction<'conn>, caller: &'a Caller) -> Self {
        Self { tx, caller }
    }

    fn active(&self) -> Result<()> {
        if self.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    // Every mutator is operation-atomic, including SQL triggers that fail after
    // changing other rows. A callback may catch an ordinary refusal and commit.
    fn atomic<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.active()?;
        self.tx.execute_batch("SAVEPOINT orch_intent")?;
        let result = work().and_then(|value| {
            self.tx.execute_batch("RELEASE orch_intent")?;
            Ok(value)
        });
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Err(cleanup) = self
                    .tx
                    .execute_batch("ROLLBACK TO orch_intent; RELEASE orch_intent")
                {
                    // As with leases, a failed cleanup must escape a callback
                    // that could otherwise catch the error and commit. The typed
                    // unwind is observed and raised again by the containing
                    // catch; the writer ends and drops its connection.
                    crate::sqlite::writer::contain::cleanup_failed(error, cleanup);
                }
                Err(error)
            }
        }
    }

    fn last(&self) -> Result<i64> {
        self.active()?;
        let stored: Option<i64> = self
            .tx
            .query_row(
                "SELECT value FROM counters WHERE project_id=?1 AND name=?2",
                params![self.caller.project, COUNTER],
                |r| r.get(0),
            )
            .optional()?;
        let last = stored.unwrap_or(0);
        if last < 0 {
            return Err(SqliteError::CounterNegative {
                project: self.caller.project.clone(),
                name: COUNTER,
                value: last,
            });
        }
        let (min, max): (Option<i64>, Option<i64>) = self.tx.query_row(
            "SELECT MIN(id),MAX(id) FROM intents WHERE project_id=?1",
            [&self.caller.project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if min.is_some_and(|id| id <= 0) {
            return Err(corrupt("nonpositive stored id"));
        }
        if max.is_some_and(|id| id > last) {
            return Err(if stored.is_none() {
                SqliteError::CounterMissing {
                    project: self.caller.project.clone(),
                    name: COUNTER,
                }
            } else {
                SqliteError::CounterBehind {
                    project: self.caller.project.clone(),
                    name: COUNTER,
                    value: last,
                }
            });
        }
        Ok(last)
    }

    pub(super) fn add_intent(&mut self, payload: &IntentPayload, now_ms: i64) -> Result<Intent> {
        self.active()?;
        payload.validate()?;
        if let Some(run) = payload.run() {
            let mut query = self.tx.prepare(
                "SELECT task_id FROM leases WHERE project_id=?1 AND run_id=?2 ORDER BY task_id LIMIT 2",
            )?;
            let tasks = query
                .query_map(params![self.caller.project, run], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let [task] = tasks.as_slice() else {
                return Err(StoreError::StaleEpoch.into());
            };
            let lease =
                leases::get_lease(self.tx, self.caller, task)?.ok_or(StoreError::StaleEpoch)?;
            leases::check(self.tx, self.caller, &lease, &lease.lineage, lease.epoch)?;
        }
        let id = self
            .last()?
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)?;
        let text =
            serde_json::to_string(payload).map_err(|_| corrupt("cannot encode typed payload"))?;
        self.atomic(|| {
            self.tx.execute(
                "INSERT INTO intents(project_id,id,run_id,kind,payload,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![self.caller.project,id,payload.run(),payload.kind().as_str(),text,now_ms],
            )?;
            self.tx.execute(
                "INSERT INTO counters(project_id,name,value) VALUES(?1,?2,?3)
                 ON CONFLICT(project_id,name) DO UPDATE SET value=excluded.value",
                params![self.caller.project,COUNTER,id],
            )?;
            Ok(Intent { id: id as u64, payload: payload.clone(), created_at: now_ms, done_at: None })
        })
    }

    pub(super) fn finish_intent(&mut self, id: u64, now_ms: i64) -> Result<FinishOutcome> {
        let last = self.last()? as u64;
        if id == 0 || id > last {
            return Err(invalid());
        }
        self.atomic(|| {
            let changed = self.tx.execute(
                "UPDATE intents SET done_at=?3 WHERE project_id=?1 AND id=?2 AND done_at IS NULL",
                params![self.caller.project, id as i64, now_ms],
            )?;
            Ok(if changed == 0 {
                FinishOutcome::AlreadyDone
            } else {
                FinishOutcome::Finished
            })
        })
    }

    pub(super) fn delete_finished_intents(
        &mut self,
        now_ms: i64,
        max_rows: usize,
    ) -> Result<usize> {
        check_bound(max_rows)?;
        self.active()?;
        // Below the signed minimum no stored timestamp can be old enough.
        // Saturation would wrongly delete a row completed at i64::MIN.
        let Some(cutoff) = now_ms.checked_sub(DONE_RETENTION_MS) else {
            return Ok(0);
        };
        self.atomic(|| {
            Ok(self.tx.execute(
                "DELETE FROM intents WHERE project_id=?1 AND id IN
             (SELECT id FROM intents WHERE project_id=?1 AND done_at<=?2 ORDER BY id LIMIT ?3)",
                params![self.caller.project, cutoff, max_rows as i64],
            )?)
        })
    }

    pub(super) fn get_intent(&self, id: u64) -> Result<Option<Intent>> {
        self.active()?;
        let Ok(id) = i64::try_from(id) else {
            return Ok(None);
        };
        let mut query = self.tx.prepare(&format!(
            "SELECT {COLUMNS} FROM intents WHERE project_id=?1 AND id=?2"
        ))?;
        let mut rows = query.query(params![self.caller.project, id])?;
        rows.next()?.map(row).transpose()
    }

    pub(super) fn list_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<Intent>> {
        self.active()?;
        check_bound(limit)?;
        let Ok(after) = i64::try_from(after) else {
            return Ok(Vec::new());
        };
        let mut query = self.tx.prepare(&format!(
            "SELECT {COLUMNS} FROM intents WHERE project_id=?1 AND done_at IS NULL AND id>?2 ORDER BY id LIMIT ?3"
        ))?;
        let mut rows = query.query(params![self.caller.project, after, limit as i64])?;
        let mut result = Vec::new();
        while let Some(value) = rows.next()? {
            result.push(row(value)?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
