//! Test-only retry rows: the reference for the table's rules, per project.

use std::collections::BTreeMap;

use super::*;

#[derive(Clone, Default)]
pub struct RetryState {
    /// By task id: one row per task.
    rows: BTreeMap<String, Retry>,
}

impl RetryState {
    pub fn put(&mut self, retry: &Retry) -> Result<()> {
        retry.validate()?;
        self.rows.insert(retry.task.clone(), retry.clone());
        Ok(())
    }

    pub fn delete(&mut self, task: &str) -> bool {
        self.rows.remove(task).is_some()
    }

    pub fn get(&self, task: &str) -> Option<Retry> {
        self.rows.get(task).cloned()
    }

    pub fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>> {
        check_limit(limit)?;
        let mut due: Vec<_> = self.rows.values().filter(|r| r.due_at <= now_ms).collect();
        due.sort_by(|a, b| (a.due_at, &a.task).cmp(&(b.due_at, &b.task)));
        Ok(due.into_iter().take(limit).cloned().collect())
    }
}
