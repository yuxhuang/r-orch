//! Request-key rows of the file reference, written from the port's rules
//! (`orch_core::store::request_keys`) and not from the test double: a row is
//! found by identity, run, epoch and key within the project's document, is
//! replayable until `created_at + KEY_TTL_MS` and is deleted earliest expiry
//! first within the bound.

use orch_core::store::StoreError;
use orch_core::store::request_keys::{
    KEY_TTL_MS, KeyCheck, MAX_RESULT_BYTES, RequestScope, check_bound, check_key, check_now,
    check_request, expires_at,
};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    identity: String,
    run: String,
    epoch: u64,
    key: String,
    op: String,
    req_sha256: String,
    /// Absent when the result was over the bound and was not kept.
    result: Option<String>,
    created_at: i64,
    expires_at: i64,
}

impl Row {
    fn id(&self) -> (&str, &str, u64, &str) {
        (&self.identity, &self.run, self.epoch, &self.key)
    }
}

fn id(scope: &RequestScope) -> (&str, &str, u64, &str) {
    (scope.identity(), scope.run(), scope.epoch(), scope.key())
}

/// The request keys of one project, sorted by identity, run, epoch and key. A
/// document written before request keys existed has no such field and reads as
/// an empty set.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct RequestKeys {
    rows: Vec<Row>,
}

impl RequestKeys {
    /// A stored row must be one the port would have written: sorted without a
    /// repeat, with a key, an operation and a hash of the port's shapes, a result
    /// within the bound, and an expiry one hour after its creation.
    pub(super) fn validate(&self) -> std::result::Result<(), String> {
        for (n, row) in self.rows.iter().enumerate() {
            if n > 0 && self.rows[n - 1].id() >= row.id() {
                return Err(format!("request key {n} is out of order or repeated"));
            }
            check_key(&row.key).map_err(|e| format!("request key {n}: {e}"))?;
            check_request(&row.op, &row.req_sha256, row.created_at)
                .map_err(|e| format!("request key {n}: {e}"))?;
            if row.identity.is_empty() || (row.run.is_empty() != (row.epoch == 0)) {
                return Err(format!("request key {n} has an impossible scope"));
            }
            if row
                .result
                .as_ref()
                .is_some_and(|r| r.len() > MAX_RESULT_BYTES)
            {
                return Err(format!("request key {n} keeps a result over the bound"));
            }
            if row.created_at.checked_add(KEY_TTL_MS) != Some(row.expires_at) {
                return Err(format!(
                    "request key {n} does not expire an hour after it was stored"
                ));
            }
        }
        Ok(())
    }

    fn find(&self, scope: &RequestScope) -> std::result::Result<usize, usize> {
        self.rows.binary_search_by(|row| row.id().cmp(&id(scope)))
    }

    pub(super) fn check(
        &self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> Result<KeyCheck> {
        check_request(op, req_sha256, now_ms)?;
        Ok(match self.find(scope) {
            Ok(at) if self.rows[at].expires_at > now_ms => {
                let row = &self.rows[at];
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

    pub(super) fn store(
        &mut self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> Result<()> {
        check_request(op, req_sha256, now_ms)?;
        let expires_at = expires_at(now_ms)?;
        let found = self.find(scope);
        if let Ok(at) = found
            && self.rows[at].expires_at > now_ms
        {
            return Err(StoreError::InvalidRequestKey(
                "the key is already stored; check first and store only a new one".into(),
            ));
        }
        let row = Row {
            identity: scope.identity().into(),
            run: scope.run().into(),
            epoch: scope.epoch(),
            key: scope.key().into(),
            op: op.into(),
            req_sha256: req_sha256.into(),
            result: (result.len() <= MAX_RESULT_BYTES).then(|| result.into()),
            created_at: now_ms,
            expires_at,
        };
        match found {
            Ok(at) => self.rows[at] = row,
            Err(at) => self.rows.insert(at, row),
        }
        Ok(())
    }

    pub(super) fn delete_expired(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        check_now(now_ms)?;
        check_bound(max_rows)?;
        let mut due: Vec<usize> = (0..self.rows.len())
            .filter(|&n| self.rows[n].expires_at <= now_ms)
            .collect();
        due.sort_by_key(|&n| (self.rows[n].expires_at, n));
        due.truncate(max_rows);
        // Highest index first, so that each removal leaves the rest in place.
        due.sort_by_key(|&n| std::cmp::Reverse(n));
        for n in &due {
            self.rows.remove(*n);
        }
        Ok(due.len())
    }
}
