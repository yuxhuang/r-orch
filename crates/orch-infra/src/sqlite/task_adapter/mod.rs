//! Internal Task list on the selecting transaction: validated cursors and
//! ordered hydration. This is not `Storage`: no dispatcher authorization or
//! blob/failure policy beyond the borrowed `TaskSql` is implied, and the daemon
//! continues to use files until T24. The caller supplies the authorized
//! caller's project; no token or key can name another.
//!
//! One page is one transaction: the IDs, the count and every record come from
//! the transaction's snapshot, however many commits land meanwhile. Any damaged
//! record fails the whole page, never a shorter one.
mod cursor;

use super::task_page::{self, TaskIdPage};
use super::task_sql::TaskSql;
use super::{FailureSource, ReadPool, Result, SqliteError};
use crate::blob::DiskBlobStore;
pub(super) use cursor::TaskCursors;
use orch_core::clock::Clock;
use orch_core::store::{Filter, Page, PageRequest, Record};
use std::sync::Arc;
use std::time::Instant;

/// Task pages through the read pool. A writer callback calls [`page`] directly
/// with its own `TaskSql`, so that it sees its own uncommitted writes.
pub(super) struct TaskAdapter<'a, 'data> {
    readers: &'a ReadPool<'data>,
    disk: &'a DiskBlobStore,
    cursors: TaskCursors,
    clock: Arc<dyn Clock>,
}

impl<'a, 'data> TaskAdapter<'a, 'data> {
    /// `generation` identifies this daemon generation: a restore must pass a new
    /// one, so that no token issued before it resumes a walk after it.
    pub(super) fn new(
        readers: &'a ReadPool<'data>,
        disk: &'a DiskBlobStore,
        generation: &str,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            readers,
            disk,
            cursors: TaskCursors::new(generation),
            clock,
        }
    }

    /// `project` is the authorized caller's. A fatal result latches the pool's
    /// shared state before it returns, so the caller only converts it.
    pub(super) fn list(
        &self,
        project: &str,
        filter: &Filter,
        request: &PageRequest,
        deadline: Instant,
    ) -> Result<Page<Record>> {
        let now = self.clock.now_ms();
        self.readers.read(deadline, |tx| {
            let sql = TaskSql {
                tx,
                project,
                state: self.readers.failure_state().clone(),
                source: FailureSource::Reader,
                disk: self.disk,
            };
            page(&sql, &self.cursors, now, filter, request)
        })
    }
}

fn selected(error: task_page::Error) -> SqliteError {
    match error {
        task_page::Error::Page(error) => error.into(),
        task_page::Error::Sql(error) => error.into(),
    }
}

/// Request errors come first: limit, filter family, sort and the token's size
/// are checked before any token is decoded, so a bad request keeps its own
/// error whatever cursor it carries. Then the token is resolved, the IDs are
/// selected and hydrated on `sql.tx` in the selected order, and the next token
/// is issued.
pub(super) fn page(
    sql: &TaskSql<'_, '_>,
    cursors: &TaskCursors,
    now_ms: i64,
    filter: &Filter,
    request: &PageRequest,
) -> Result<Page<Record>> {
    page_between(sql, cursors, now_ms, filter, request, || {})
}

/// `between` runs after the IDs are selected and before they are hydrated, to
/// let a test commit from another connection at exactly that point.
fn page_between(
    sql: &TaskSql<'_, '_>,
    cursors: &TaskCursors,
    now_ms: i64,
    filter: &Filter,
    request: &PageRequest,
    between: impl FnOnce(),
) -> Result<Page<Record>> {
    task_page::validate_request(filter, request)?;
    let after = request
        .cursor
        .as_ref()
        .map(|token| cursors.resume(token, sql.project, filter, request.sort, now_ms))
        .transpose()?;
    // The token is resolved; the leaf refuses a request that still carries one.
    let walk = PageRequest {
        cursor: None,
        ..request.clone()
    };
    let TaskIdPage { ids, next, total } =
        task_page::query(sql.tx, sql.project, filter, &walk, after.as_ref()).map_err(selected)?;
    between();
    let items = sql.hydrate(&ids)?;
    let next_cursor = next
        .map(|next| cursors.issue(sql.project, filter, request.sort, &next, now_ms))
        .transpose()?;
    Ok(Page {
        items,
        next_cursor,
        total,
    })
}

#[cfg(test)]
mod tests;
