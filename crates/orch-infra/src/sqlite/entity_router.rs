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
        if self.source == FailureSource::Writer
            && let Some(error) = self.state.transaction_must_abort()
        {
            return Err(error);
        }
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

    /// Converts only after observation through the trusted executor source.
    /// Operations already observe before returning, so catching their typed
    /// error without converting it cannot bypass the shared failure policy.
    pub(super) fn port<T>(&self, result: Result<T>) -> orch_core::store::Result<T> {
        port(self.observed(result))
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
            // TaskSql observes reads at source. Composite operations such as
            // move_task also observe their cleanup result; the metric counts
            // observations and is not a count of distinct incidents.
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
    poisoned: bool,
}

impl<'a, 'conn> EntityWrite<'a, 'conn> {
    /// Only inside the writer job, after dispatcher authorization and fencing.
    /// Every reconstructed view must receive the same executor state: failed
    /// cleanup and event backend errors poison the entire callback, including
    /// when the callback catches the operation error.
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
            poisoned: false,
        }
    }

    pub(super) fn caller(&self) -> &Caller {
        self.read.caller()
    }
    pub(super) fn port<T>(&self, result: Result<T>) -> orch_core::store::Result<T> {
        self.read.port(result)
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
        if self.poisoned {
            return Err(SqliteError::TransactionAborted);
        }
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

    // The leaf checks stale revisions before reading details. Protect both the
    // move and its returned Record without changing that error precedence.
    fn move_task(&mut self, id: &str, expected: Revision) -> Result<Record> {
        self.read.observed(
            self.read
                .tx
                .execute_batch("SAVEPOINT entity_task_move")
                .map_err(SqliteError::from),
        )?;
        let result = self
            .task
            .move_to_end(id, expected)
            .and_then(|_| {
                self.read.task().get(id)?.ok_or(SqliteError::TaskCorrupt {
                    task: id.into(),
                    check: "moved row missing",
                    locator: "tasks",
                })
            })
            .and_then(|record| {
                self.read.tx.execute_batch("RELEASE entity_task_move")?;
                Ok(record)
            });
        let result = match result {
            Ok(record) => Ok(record),
            Err(operation) => match self
                .read
                .tx
                .execute_batch("ROLLBACK TO entity_task_move; RELEASE entity_task_move")
            {
                Ok(()) => Err(operation),
                Err(rollback) => {
                    self.poisoned = true;
                    Err(SqliteError::OperationRollback {
                        operation: Box::new(operation),
                        rollback,
                    })
                }
            },
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
            Family::Task => return self.move_task(id, expected),
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

/// Only after the structured result has been observed. Private diagnostics
/// remain in the internal error; callers receive no stored value or path.
fn port<T>(result: Result<T>) -> orch_core::store::Result<T> {
    result.map_err(|error| match error {
        SqliteError::Store(refusal) => refusal,
        _ => orch_core::store::StoreError::Backend(orch_core::Error::other(
            "SQLite storage operation failed",
        )),
    })
}

mod operational;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod operational_tests;
