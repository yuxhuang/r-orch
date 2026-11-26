//! Internal goal-only persistence through the real executor, on the shared
//! native driver of "Entity format v1": the original timestamp strings, the
//! R97 text rule and the whole typed value are kept (see `cctg`). This is not
//! `Storage`: no authorization, public cursor or shared fatal policy is implied.
//! Trusted callers supply the project, never raw SQL. The daemon continues to
//! use files until every port method and safeguard is proven.
use super::cctg::{Family, NativeSql};
use super::{Lane, ReadPool, Result, WriteReceipt, WriterHandle};
use crate::blob::DiskBlobStore;
use orch_core::clock::Clock;
use orch_core::store::{Entity, Record, Revision};
use orch_domain::Goal;
use std::sync::Arc;
use std::time::Instant;

pub(super) struct GoalAdapter<'a, 'data> {
    writer: &'a WriterHandle,
    readers: &'a ReadPool<'data>,
    disk: Arc<DiskBlobStore>,
    clock: Arc<dyn Clock>,
}

impl<'a, 'data> GoalAdapter<'a, 'data> {
    pub(super) fn new(
        writer: &'a WriterHandle,
        readers: &'a ReadPool<'data>,
        disk: Arc<DiskBlobStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            writer,
            readers,
            disk,
            clock,
        }
    }

    /// One callback is one writer transaction. Returning an error rolls back
    /// every successful operation in it; receipts acknowledge only commit.
    pub(super) fn write<T: Send + 'static>(
        &self,
        project: String,
        deadline: Instant,
        work: impl FnOnce(&mut GoalWrite<'_, '_>) -> Result<T> + Send + 'static,
    ) -> Result<WriteReceipt<T>> {
        let (disk, now) = (Arc::clone(&self.disk), self.clock.now_ms());
        self.writer
            .submit(Lane::Entities, Some(deadline), move |tx| {
                let mut unit = GoalWrite {
                    sql: NativeSql::new(tx, &disk, &project, None, now),
                };
                let result = work(&mut unit);
                if result.is_ok() {
                    unit.sql.ensure_committable()?;
                }
                result
            })
    }

    /// All gets in this callback use one pool connection and one snapshot.
    /// Owned results may escape; the connection and transaction may not.
    pub(super) fn read<T>(
        &self,
        project: &str,
        deadline: Instant,
        read: impl FnOnce(&GoalRead<'_, '_>) -> Result<T>,
    ) -> Result<T> {
        self.readers.read(deadline, |tx| {
            read(&GoalRead {
                sql: NativeSql::new(tx, &self.disk, project, None, 0),
            })
        })
    }
}

pub(super) struct GoalRead<'a, 'conn> {
    sql: NativeSql<'a, 'conn>,
}

impl GoalRead<'_, '_> {
    pub(super) fn get(&self, id: &str) -> Result<Option<Record>> {
        Ok(self.sql.get(Family::Goal, id)?)
    }
}

pub(super) struct GoalWrite<'a, 'conn> {
    sql: NativeSql<'a, 'conn>,
}

impl GoalWrite<'_, '_> {
    pub(super) fn get(&self, id: &str) -> Result<Option<Record>> {
        Ok(self.sql.get(Family::Goal, id)?)
    }

    pub(super) fn create(&mut self, goal: Goal) -> Result<Record> {
        Ok(self.sql.create(Entity::Goal(Box::new(goal)))?)
    }

    pub(super) fn update(&mut self, goal: Goal, expected: Revision) -> Result<Record> {
        Ok(self.sql.update(Entity::Goal(Box::new(goal)), expected)?)
    }

    pub(super) fn delete(&mut self, id: &str, expected: Revision) -> Result<()> {
        Ok(self.sql.delete(Family::Goal, id, expected)?)
    }

    pub(super) fn move_to_end(&mut self, id: &str, expected: Revision) -> Result<Record> {
        Ok(self.sql.move_to_end(Family::Goal, id, expected)?)
    }
}

#[cfg(test)]
mod tests;
