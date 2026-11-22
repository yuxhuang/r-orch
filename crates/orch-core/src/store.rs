//! Additive storage port v2, alongside the 1.0 [`crate::storage`] traits.
//!
//! A caller selects the project at entry; entity keys never override it.
//! Authorization of operations belongs to the dispatcher, not to this port.
//! Writes run a synchronous callback on the backend's writer, so no transaction
//! can escape or wait for an external side effect. A callback error aborts it.
//! Events and leases share the entity transaction and its project boundary.
//! The retry table shares it too. The remaining operational families arrive in
//! later slices.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod blob;
pub mod entity;
pub mod events;
#[cfg(feature = "test-support")]
pub mod fake;
pub mod leases;
pub mod page;
pub mod retries;

pub use entity::{ConfigEntry, Entity, EntityKind, Record, Revision, TemplateEntry};
pub use page::{Cursor, Filter, Page, PageRequest, Sort};

/// The authority and project carried by both storage entry points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    pub principal: Principal,
    pub project: String,
}

/// A task authority includes both parts of its fencing token (R132).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    Owner {
        identity: String,
    },
    Worker {
        identity: String,
    },
    Task {
        identity: String,
        task: String,
        run: String,
        lineage: String,
        epoch: u64,
    },
}

/// Scheduling lanes; choosing a lane does not grant authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Claims,
    Entities,
    Events,
    Maintenance,
}

/// Errors specific to the revisioned port, independent of legacy errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("stale revision for {kind:?} {id}: expected {expected:?}, actual {actual:?}")]
    StaleRevision {
        kind: EntityKind,
        id: String,
        expected: Revision,
        actual: Option<Revision>,
    },
    #[error("{kind:?} {id} already exists")]
    AlreadyExists { kind: EntityKind, id: String },
    #[error("task ids {existing} and {requested} have the same ASCII fold key")]
    IdAlias { existing: String, requested: String },
    #[error("invalid page: {0}")]
    InvalidPage(String),
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("stale lease lineage or epoch")]
    StaleEpoch,
    #[error("invalid events: {0}")]
    InvalidEvents(String),
    #[error("invalid retry: {0}")]
    InvalidRetry(String),
    #[error("storage counter exhausted")]
    CounterExhausted,
    #[error(transparent)]
    Backend(#[from] crate::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// All reads share one committed state, including repeated page reads.
#[async_trait]
pub trait ReadSnapshot:
    events::EventRead + leases::LeaseRead + retries::RetryRead + Send + Sync
{
    fn caller(&self) -> &Caller;
    async fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>>;
    async fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>>;
}

/// Entity operations executed synchronously within a single transaction.
///
/// Errors leave that operation unchanged. Returning an error from `work`
/// discards every operation, including positions and revision increments.
pub trait UnitOfWork: events::EventWrite + leases::LeaseWrite + retries::RetryWrite + Send {
    fn caller(&self) -> &Caller;
    fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>>;
    fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>>;
    fn create(&mut self, entity: Entity) -> Result<Record>;
    fn update(&mut self, entity: Entity, expected: Revision) -> Result<Record>;
    fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<()>;
    /// Moving changes both revision and position; ordinary updates never move.
    fn move_to_end(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record>;
}

/// Owned work can cross the dedicated writer-task boundary. It is invoked once
/// when admitted; a backend may refuse admission before invoking it.
pub type WriteWork = Box<dyn FnOnce(&mut dyn UnitOfWork) -> Result<()> + Send + 'static>;

/// The only entry points into v2 storage. Backends serialize writers.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>>;
    async fn write(&self, caller: Caller, lane: Lane, work: WriteWork) -> Result<()>;
}

/// Typed callback results are returned only after the backend confirms commit.
#[async_trait]
pub trait StorageExt: Storage {
    async fn transaction<T, F>(&self, caller: Caller, lane: Lane, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut dyn UnitOfWork) -> Result<T> + Send + 'static,
    {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.write(
            caller,
            lane,
            Box::new(move |unit| {
                let value = work(unit)?;
                // Cancellation of the waiting caller does not abort admitted work.
                let _ = reply.send(value);
                Ok(())
            }),
        )
        .await?;
        result.await.map_err(|_| {
            StoreError::Backend(crate::Error::other(
                "writer returned without executing work",
            ))
        })
    }
}

impl<S: Storage + ?Sized> StorageExt for S {}
