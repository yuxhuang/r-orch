//! Retry rows of the file reference, written from the port's rules
//! (`orch_core::store::retries`) and not from the test double: one row per
//! task, replaced whole, and read by deadline then task id.

use std::collections::BTreeMap;

use orch_core::store::StoreError;
use orch_core::store::retries::{Retry, RetryCursor, RetryPage, check_limit};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Serialize, Deserialize)]
struct Row {
    attempt: u32,
    due_at: i64,
    error: String,
}

/// The retry table of one project, by task id.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Retries {
    rows: BTreeMap<String, Row>,
}

fn retry(task: &str, row: &Row) -> Retry {
    Retry {
        task: task.to_owned(),
        attempt: row.attempt,
        due_at: row.due_at,
        error: row.error.clone(),
    }
}

impl Retries {
    pub(super) fn put(&mut self, new: &Retry) -> Result<()> {
        new.validate()?;
        self.rows.insert(
            new.task.clone(),
            Row {
                attempt: new.attempt,
                due_at: new.due_at,
                error: new.error.clone(),
            },
        );
        Ok(())
    }

    pub(super) fn delete(&mut self, task: &str) -> bool {
        self.rows.remove(task).is_some()
    }

    pub(super) fn get(&self, task: &str) -> Option<Retry> {
        self.rows.get_key_value(task).map(|(t, r)| retry(t, r))
    }

    /// Earliest deadline first, ties by task id.
    pub(super) fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>> {
        check_limit(limit)?;
        let mut due: Vec<_> = self
            .rows
            .iter()
            .filter(|(_, r)| r.due_at <= now_ms)
            .collect();
        due.sort_by(|(ta, a), (tb, b)| (a.due_at, ta).cmp(&(b.due_at, tb)));
        Ok(due
            .into_iter()
            .take(limit)
            .map(|(t, r)| retry(t, r))
            .collect())
    }

    /// Every row from just after `after`, in the order of [`Self::due`].
    pub(super) fn page(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage> {
        check_limit(limit)?;
        if let Some(cursor) = after {
            cursor.validate()?;
        }
        let mut rest: Vec<_> = self
            .rows
            .iter()
            .filter(|(t, r)| {
                after.is_none_or(|c| (r.due_at, t.as_str()) > (c.due_at, c.task.as_str()))
            })
            .collect();
        rest.sort_by(|(ta, a), (tb, b)| (a.due_at, ta).cmp(&(b.due_at, tb)));
        let more = rest.len() > limit;
        let rows: Vec<Retry> = rest
            .into_iter()
            .take(limit)
            .map(|(t, r)| retry(t, r))
            .collect();
        let next = rows.last().filter(|_| more).map(RetryCursor::after);
        Ok(RetryPage { rows, next })
    }
}
