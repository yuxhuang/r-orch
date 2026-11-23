//! Test-only request keys, held per project. Pure and synchronous: the enclosing
//! view checks that the scope belongs to its caller.

use std::collections::BTreeMap;

use super::*;

type Id = (String, String, u64, String);

#[derive(Clone)]
struct Row {
    op: String,
    req_sha256: String,
    /// None when the result was over the bound and was not kept.
    result: Option<String>,
    expires_at: i64,
}

#[derive(Clone, Default)]
pub struct KeyState {
    rows: BTreeMap<Id, Row>,
}

fn id(scope: &RequestScope) -> Id {
    (
        scope.identity().into(),
        scope.run().into(),
        scope.epoch(),
        scope.key().into(),
    )
}

impl KeyState {
    pub fn check(
        &self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> Result<KeyCheck> {
        check_request(op, req_sha256, now_ms)?;
        Ok(match self.rows.get(&id(scope)) {
            Some(row) if row.expires_at > now_ms => {
                if row.op != op || row.req_sha256 != req_sha256 {
                    KeyCheck::Mismatch
                } else {
                    row.result
                        .clone()
                        .map_or(KeyCheck::ReplayWithoutResult, KeyCheck::Replay)
                }
            }
            _ => KeyCheck::New,
        })
    }

    pub fn store(
        &mut self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> Result<()> {
        check_request(op, req_sha256, now_ms)?;
        let expires_at = expires_at(now_ms)?;
        let id = id(scope);
        if self
            .rows
            .get(&id)
            .is_some_and(|row| row.expires_at > now_ms)
        {
            return Err(StoreError::InvalidRequestKey(
                "the key is already stored; check first and store only a new one".into(),
            ));
        }
        self.rows.insert(
            id,
            Row {
                op: op.into(),
                req_sha256: req_sha256.into(),
                result: (result.len() <= MAX_RESULT_BYTES).then(|| result.into()),
                expires_at,
            },
        );
        Ok(())
    }

    pub fn delete_expired(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        check_now(now_ms)?;
        check_bound(max_rows)?;
        let mut due: Vec<(i64, &Id)> = self
            .rows
            .iter()
            .filter(|(_, row)| row.expires_at <= now_ms)
            .map(|(id, row)| (row.expires_at, id))
            .collect();
        due.sort();
        let doomed: Vec<Id> = due
            .into_iter()
            .take(max_rows)
            .map(|(_, id)| id.clone())
            .collect();
        for id in &doomed {
            self.rows.remove(id);
        }
        Ok(doomed.len())
    }
}
