//! Intent rows of the file reference, written from the port's rules
//! (`orch_core::store::intents`) and not from the test double: ids are handed
//! out by a counter that is never lowered, completion is first-wins and
//! idempotent, and retention deletes the lowest ids first within its bound.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use orch_core::store::StoreError;
use orch_core::store::intents::{
    DONE_RETENTION_MS, FinishOutcome, Intent, IntentPayload, check_bound,
};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    payload: IntentPayload,
    created_at: i64,
    done_at: Option<i64>,
}

fn intent(id: u64, row: &Row) -> Intent {
    Intent {
        id,
        payload: row.payload.clone(),
        created_at: row.created_at,
        done_at: row.done_at,
    }
}

/// The intents of one project. A document written before intents existed has
/// no such field and reads as an empty set with its counter at 0.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Intents {
    rows: BTreeMap<u64, Row>,
    /// The last id handed out. Never lowered, so a deletion frees no id.
    last: u64,
}

impl Intents {
    /// A stored row must hold a payload the port would accept and an id that
    /// the counter has passed; anything else is a document nobody wrote.
    pub(super) fn validate(&self) -> std::result::Result<(), String> {
        for (id, row) in &self.rows {
            if *id == 0 || *id > self.last {
                return Err(format!("intent {id} is past its counter"));
            }
            row.payload
                .validate()
                .map_err(|e| format!("intent {id}: {e}"))?;
        }
        Ok(())
    }

    /// The caller has already checked the lease a process intent needs.
    pub(super) fn add(&mut self, payload: &IntentPayload, now_ms: i64) -> Result<Intent> {
        payload.validate()?;
        let id = self
            .last
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)?;
        let row = Row {
            payload: payload.clone(),
            created_at: now_ms,
            done_at: None,
        };
        let added = intent(id, &row);
        self.last = id;
        self.rows.insert(id, row);
        Ok(added)
    }

    pub(super) fn finish(&mut self, id: u64, now_ms: i64) -> Result<FinishOutcome> {
        if id == 0 || id > self.last {
            return Err(StoreError::InvalidIntent(format!(
                "intent {id} was never issued"
            )));
        }
        // An id below the counter with no row was deleted after it was done.
        match self.rows.get_mut(&id) {
            Some(row) if row.done_at.is_none() => {
                row.done_at = Some(now_ms);
                Ok(FinishOutcome::Finished)
            }
            _ => Ok(FinishOutcome::AlreadyDone),
        }
    }

    pub(super) fn delete_finished(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        check_bound(max_rows)?;
        let cutoff = now_ms.saturating_sub(DONE_RETENTION_MS);
        let doomed: Vec<u64> = self
            .rows
            .iter()
            .filter(|(_, row)| row.done_at.is_some_and(|at| at <= cutoff))
            .map(|(id, _)| *id)
            .take(max_rows)
            .collect();
        for id in &doomed {
            self.rows.remove(id);
        }
        Ok(doomed.len())
    }

    pub(super) fn get(&self, id: u64) -> Option<Intent> {
        self.rows.get(&id).map(|row| intent(id, row))
    }

    pub(super) fn pending(&self, after: u64, limit: usize) -> Result<Vec<Intent>> {
        check_bound(limit)?;
        Ok(self
            .rows
            .range((Excluded(after), Unbounded))
            .filter(|(_, row)| row.done_at.is_none())
            .take(limit)
            .map(|(id, row)| intent(*id, row))
            .collect())
    }
}
