//! Request keys on the enclosing writer transaction. Scope comes only from
//! its authenticated caller; authorization remains the dispatcher's job.
//! No snapshot read or runtime wiring exposes stored results.
use crate::sqlite::{Result, SqliteError};
use orch_core::store::request_keys::{
    KeyCheck, MAX_RESULT_BYTES, RequestScope, check_bound, check_now, check_request, expires_at,
};
use orch_core::store::{Caller, StoreError};
use rusqlite::{OptionalExtension, Transaction, params};

pub(super) struct RequestKeys<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
}

impl<'a, 'conn> RequestKeys<'a, 'conn> {
    pub(super) fn new(tx: &'a Transaction<'conn>, caller: &'a Caller) -> Self {
        Self { tx, caller }
    }
}

fn invalid(detail: &str) -> SqliteError {
    StoreError::InvalidRequestKey(detail.into()).into()
}

impl RequestKeys<'_, '_> {
    pub(super) fn check_request_key(
        &self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> Result<KeyCheck> {
        scope.check_caller(self.caller)?;
        let epoch = i64::try_from(scope.epoch()).map_err(|_| invalid("epoch is out of range"))?;
        check_request(op, req_sha256, now_ms)?;
        let found = self
            .tx
            .query_row(
                "SELECT op, req_sha256, result, result_truncated, state FROM request_keys
             WHERE identity_id=?1 AND project_id=?2 AND run_id=?3 AND epoch=?4 AND key=?5
             AND expires_at>?6",
                params![
                    scope.identity(),
                    scope.project(),
                    scope.run(),
                    epoch,
                    scope.key(),
                    now_ms
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(SqliteError::from)?;
        let Some((stored_op, hash, result, truncated, state)) = found else {
            return Ok(KeyCheck::New);
        };
        // Never replay an impossible or incomplete row as a successful change.
        if state != "done"
            || !matches!((&result, truncated), (Some(_), 0) | (None, 1))
            || result
                .as_ref()
                .is_some_and(|value| value.len() > MAX_RESULT_BYTES)
        {
            return Err(StoreError::Backend(orch_core::Error::other(
                "SQLite request key: invalid stored result",
            ))
            .into());
        }
        if stored_op != op || hash != req_sha256 {
            Ok(KeyCheck::Mismatch)
        } else {
            Ok(result.map_or(KeyCheck::ReplayWithoutResult, KeyCheck::Replay))
        }
    }

    pub(super) fn store_request_key(
        &mut self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> Result<()> {
        scope.check_caller(self.caller)?;
        let epoch = i64::try_from(scope.epoch()).map_err(|_| invalid("epoch is out of range"))?;
        check_request(op, req_sha256, now_ms)?;
        let expiry = expires_at(now_ms)?;
        let truncated = result.len() > MAX_RESULT_BYTES;
        // One statement protects even callers that catch a refusal and commit
        // other work. A live row is never overwritten, including its first result.
        let changed = self.tx.execute(
            "INSERT INTO request_keys
             (identity_id,project_id,run_id,epoch,key,op,req_sha256,state,result,result_truncated,created_at,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'done',?8,?9,?10,?11)
             ON CONFLICT(identity_id,project_id,run_id,epoch,key) DO UPDATE SET
             op=excluded.op, req_sha256=excluded.req_sha256, state='done', result=excluded.result,
             result_truncated=excluded.result_truncated, commit_seq=NULL,
             created_at=excluded.created_at, expires_at=excluded.expires_at
             WHERE request_keys.expires_at<=excluded.created_at",
            params![scope.identity(), scope.project(), scope.run(), epoch, scope.key(),
                op, req_sha256, (!truncated).then_some(result), truncated, now_ms, expiry],
        ).map_err(SqliteError::from)?;
        if changed == 0 {
            Err(invalid(
                "the key is already stored; check first and store only a new one",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn delete_expired_request_keys(
        &mut self,
        now_ms: i64,
        max_rows: usize,
    ) -> Result<usize> {
        check_now(now_ms)?;
        check_bound(max_rows)?;
        // The table is WITHOUT ROWID. Select its whole primary key, in the
        // port's expiry/scope order, without extending cleanup to other projects.
        self.tx
            .execute(
                "DELETE FROM request_keys WHERE (identity_id,project_id,run_id,epoch,key) IN
             (SELECT identity_id,project_id,run_id,epoch,key FROM request_keys
              WHERE project_id=?1 AND expires_at<=?2
              ORDER BY expires_at,identity_id,project_id,run_id,epoch,key LIMIT ?3)",
                params![self.caller.project, now_ms, max_rows as i64],
            )
            .map_err(SqliteError::from)
    }
}

#[cfg(test)]
mod tests;
