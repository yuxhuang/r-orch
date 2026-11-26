//! Inactive borrowed routing for the five admitted entity families.
//!
//! Trusted composition supplies one authenticated caller and the executor's
//! failure state. These views never open or commit a transaction and implement
//! no port trait. The public owner/session and other families remain separate.
use super::agent_team_run::{Family as NativeFamily, NativeError, NativeSql};
use super::retry_message::{MessageReader, Messages};
use super::task_adapter::{self, TaskCursors};
use super::task_sql::{TaskSql, writes::TaskWrite};
use super::{FailureSource, FailureState, Result, SqliteError};
use crate::blob::DiskBlobStore;
use orch_core::store::{Caller, Entity, Filter, Page, PageRequest, Record, Revision};
use orch_domain::{Agent, Message, Run, Task, Team};
use rusqlite::Transaction;

/// Deliberately excludes families whose composition admission is still held.
#[derive(Clone, Copy)]
pub(super) enum Family {
    Task,
    Agent,
    Team,
    Run,
    Message,
}

/// A closed input prevents the partial router from pretending to support the
/// full Entity API. No unsupported-family fallback can reach a live facade.
pub(super) enum Payload {
    Task(Box<Task>),
    Agent(Box<Agent>),
    Team(Box<Team>),
    Run(Box<Run>),
    Message(Box<Message>),
}

fn native(error: NativeError) -> SqliteError {
    match error {
        NativeError::Corrupt {
            family,
            form,
            check,
            locator,
        } => SqliteError::EntityCorrupt {
            family,
            form,
            check,
            locator,
        },
        NativeError::Sql(error) => SqliteError::Sql(error),
        NativeError::Store(error) => SqliteError::Store(error),
        NativeError::Counter(error) => *error,
        NativeError::Access(kind) => SqliteError::EntityAccess(kind),
        NativeError::Publication(error) => SqliteError::EntityPublication(error),
        NativeError::Encoding => SqliteError::Unrepresentable("entity encoding".into()),
        NativeError::ClaimedIdentity => SqliteError::Unrepresentable("claimed run identity".into()),
        NativeError::Aborted => SqliteError::TransactionAborted,
        NativeError::Rollback {
            operation,
            rollback,
        } => SqliteError::OperationRollback {
            operation: Box::new(native(*operation)),
            rollback,
        },
    }
}

pub(super) struct EntityRead<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    caller: &'a Caller,
    disk: &'a DiskBlobStore,
    state: FailureState,
    source: FailureSource,
}

impl<'a, 'conn> EntityRead<'a, 'conn> {
    /// Only for a closure on the real read pool; pass that pool's shared state.
    pub(super) fn reader(
        tx: &'a Transaction<'conn>,
        caller: &'a Caller,
        disk: &'a DiskBlobStore,
        state: FailureState,
    ) -> Self {
        Self {
            tx,
            caller,
            disk,
            state,
            source: FailureSource::Reader,
        }
    }

    pub(super) fn caller(&self) -> &Caller {
        self.caller
    }

    fn active(&self) -> Result<()> {
        self.state.ensure_available()?;
        if self.tx.is_autocommit() {
            Err(SqliteError::TransactionAborted)
        } else {
            Ok(())
        }
    }

    fn observed<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            self.state.observe(self.source, error);
        }
        result
    }

    fn task(&self) -> TaskSql<'a, 'conn> {
        TaskSql {
            tx: self.tx,
            project: &self.caller.project,
            disk: self.disk,
            state: self.state.clone(),
            source: self.source,
        }
    }

    pub(super) fn get(&self, family: Family, id: &str) -> Result<Option<Record>> {
        self.active()?;
        let result = match family {
            // TaskSql observes its own errors at their source. Avoid a second
            // observation here: fatal_observations counts observations, not incidents.
            Family::Task => return self.task().get(id),
            Family::Message => MessageReader::new(self.tx, &self.caller.project, self.disk).get(id),
            Family::Agent | Family::Team | Family::Run => {
                let family = match family {
                    Family::Agent => NativeFamily::Agent,
                    Family::Team => NativeFamily::Team,
                    Family::Run => NativeFamily::Run,
                    _ => unreachable!(),
                };
                NativeSql::new(self.tx, self.disk, &self.caller.project, 0)
                    .get(family, id)
                    .map_err(native)
            }
        };
        self.observed(result)
    }

    /// Uses the original selecting transaction for validation, selection,
    /// hydration and counts. The caller owns generation and time inputs.
    pub(super) fn task_page(
        &self,
        cursors: &TaskCursors,
        now: i64,
        filter: &Filter,
        page: &PageRequest,
    ) -> Result<Page<Record>> {
        self.active()?;
        self.observed(task_adapter::page(&self.task(), cursors, now, filter, page))
    }
}

pub(super) struct EntityWrite<'a, 'conn> {
    read: EntityRead<'a, 'conn>,
    task: TaskWrite<'a, 'conn>,
    native: NativeSql<'a, 'conn>,
    messages: Messages<'a, 'conn>,
    now: i64,
}

impl<'a, 'conn> EntityWrite<'a, 'conn> {
    /// Only inside the writer job, after dispatcher authorization and fencing.
    /// Keep this view for the entire callback: leaf rollback poison must survive
    /// an operation error caught by that callback.
    pub(super) fn new(
        tx: &'a Transaction<'conn>,
        caller: &'a Caller,
        disk: &'a DiskBlobStore,
        state: FailureState,
        now: i64,
    ) -> Self {
        let read = EntityRead {
            tx,
            caller,
            disk,
            state,
            source: FailureSource::Writer,
        };
        Self {
            task: TaskWrite::new(read.task(), &caller.principal),
            native: NativeSql::new(tx, disk, &caller.project, now),
            messages: Messages::new(tx, caller, disk),
            read,
            now,
        }
    }

    pub(super) fn caller(&self) -> &Caller {
        self.read.caller()
    }
    pub(super) fn get(&self, family: Family, id: &str) -> Result<Option<Record>> {
        self.ensure_committable()?;
        self.read.get(family, id)
    }
    pub(super) fn task_page(
        &self,
        cursors: &TaskCursors,
        now: i64,
        filter: &Filter,
        page: &PageRequest,
    ) -> Result<Page<Record>> {
        self.ensure_committable()?;
        self.read.task_page(cursors, now, filter, page)
    }

    /// Composition must call this before acknowledging a successful callback.
    /// The executor's shared latch also prevents commit after caught corruption.
    pub(super) fn ensure_committable(&self) -> Result<()> {
        self.read.active()?;
        self.task.ensure_committable()?;
        self.read
            .observed(self.native.ensure_committable().map_err(native))
    }

    pub(super) fn create(&mut self, payload: Payload) -> Result<Record> {
        self.ensure_committable()?;
        let result = match payload {
            Payload::Task(task) => return self.task.create(*task),
            Payload::Agent(agent) => self.native.create(Entity::Agent(agent)).map_err(native),
            Payload::Team(team) => self.native.create(Entity::Team(team)).map_err(native),
            Payload::Run(run) => self.native.create(Entity::Run(run)).map_err(native),
            Payload::Message(message) => self.messages.create(&message, self.now),
        };
        self.read.observed(result)
    }

    pub(super) fn update(&mut self, payload: Payload, expected: Revision) -> Result<Record> {
        self.ensure_committable()?;
        let result = match payload {
            Payload::Task(task) => return self.task.update(*task, expected),
            Payload::Agent(agent) => self
                .native
                .update(Entity::Agent(agent), expected)
                .map_err(native),
            Payload::Team(team) => self
                .native
                .update(Entity::Team(team), expected)
                .map_err(native),
            Payload::Run(run) => self
                .native
                .update(Entity::Run(run), expected)
                .map_err(native),
            Payload::Message(message) => self.messages.update(&message, expected, self.now),
        };
        self.read.observed(result)
    }

    pub(super) fn delete(&mut self, family: Family, id: &str, expected: Revision) -> Result<()> {
        self.ensure_committable()?;
        let result = match family {
            Family::Task => return self.task.delete(id, expected),
            Family::Agent => self
                .native
                .delete(NativeFamily::Agent, id, expected)
                .map_err(native),
            Family::Team => self
                .native
                .delete(NativeFamily::Team, id, expected)
                .map_err(native),
            Family::Run => self
                .native
                .delete(NativeFamily::Run, id, expected)
                .map_err(native),
            Family::Message => self.messages.delete(id, expected),
        };
        self.read.observed(result)
    }

    pub(super) fn move_to_end(
        &mut self,
        family: Family,
        id: &str,
        expected: Revision,
    ) -> Result<Record> {
        self.ensure_committable()?;
        let result = match family {
            Family::Task => {
                self.task.move_to_end(id, expected)?;
                // Rehydrate in this transaction: the port returns the complete
                // new Record, not just the leaf's revision and position pair.
                return self
                    .read
                    .task()
                    .get(id)?
                    .ok_or(SqliteError::TaskCorrupt {
                        task: id.into(),
                        check: "moved row missing",
                        locator: "tasks",
                    })
                    .inspect_err(|error| {
                        self.read.state.observe(FailureSource::Writer, error);
                    });
            }
            Family::Agent => self
                .native
                .move_to_end(NativeFamily::Agent, id, expected)
                .map_err(native),
            Family::Team => self
                .native
                .move_to_end(NativeFamily::Team, id, expected)
                .map_err(native),
            Family::Run => self
                .native
                .move_to_end(NativeFamily::Run, id, expected)
                .map_err(native),
            Family::Message => self.messages.move_to_end(id, expected),
        };
        self.read.observed(result)
    }
}

#[cfg(test)]
mod tests;
