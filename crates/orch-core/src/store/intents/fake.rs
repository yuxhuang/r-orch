//! Test-only intent rows, held per project. Pure and synchronous: the enclosing
//! view checks the lease a process intent needs, because the leases live there.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use super::*;

#[derive(Clone, Default)]
pub struct IntentState {
    rows: BTreeMap<u64, Intent>,
    /// The last id handed out. Never lowered, so a deletion frees no id.
    last: u64,
}

impl IntentState {
    pub fn add(&mut self, payload: &IntentPayload, now_ms: i64) -> Result<Intent> {
        payload.validate()?;
        let id = self
            .last
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)?;
        let intent = Intent {
            id,
            payload: payload.clone(),
            created_at: now_ms,
            done_at: None,
        };
        self.last = id;
        self.rows.insert(id, intent.clone());
        Ok(intent)
    }

    pub fn finish(&mut self, id: u64, now_ms: i64) -> Result<FinishOutcome> {
        if id == 0 || id > self.last {
            return Err(StoreError::InvalidIntent(format!(
                "intent {id} was never issued"
            )));
        }
        match self.rows.get_mut(&id) {
            Some(row) if row.done_at.is_none() => {
                row.done_at = Some(now_ms);
                Ok(FinishOutcome::Finished)
            }
            // Done before, or done and deleted since.
            _ => Ok(FinishOutcome::AlreadyDone),
        }
    }

    pub fn delete_finished(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        check_bound(max_rows)?;
        let cutoff = now_ms.saturating_sub(DONE_RETENTION_MS);
        let doomed: Vec<u64> = self
            .rows
            .values()
            .filter(|row| row.done_at.is_some_and(|at| at <= cutoff))
            .map(|row| row.id)
            .take(max_rows)
            .collect();
        for id in &doomed {
            self.rows.remove(id);
        }
        Ok(doomed.len())
    }

    pub fn get(&self, id: u64) -> Option<Intent> {
        self.rows.get(&id).cloned()
    }

    pub fn pending(&self, after: u64, limit: usize) -> Result<Vec<Intent>> {
        check_bound(limit)?;
        Ok(self
            .rows
            .range((Excluded(after), Unbounded))
            .map(|(_, row)| row)
            .filter(|row| row.done_at.is_none())
            .take(limit)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
impl IntentState {
    /// A state whose id counter is `last`, for a limit no real run reaches.
    pub fn at_last_id(last: u64, mut state: Self) -> Self {
        state.last = last;
        state
    }
}
