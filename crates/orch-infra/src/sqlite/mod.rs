//! SQLite connection setup and forward-only schema migrations (T9).
//! The file backend remains the product backend until the T24 switch.
mod connection;
#[allow(
    dead_code,
    reason = "private entity helpers staged for the T10 adapter"
)]
mod entity;
pub mod migrations;
pub use connection::{ConnectionHandle, ConnectionRole, Synchronous, open_connection};

#[derive(Debug, thiserror::Error)]
pub enum SqliteError {
    #[error("stored task {task} failed {check} at {locator}")]
    TaskCorrupt {
        task: String,
        check: &'static str,
        locator: &'static str,
    },
    #[error("task object access failed: {0:?}")]
    TaskAccess(std::io::ErrorKind),
    /// Context, Config, Template or Goal: names the family and the identity's key
    /// form, never a text or a path.
    #[error("stored {family} form {form} failed {check} at {locator}")]
    EntityCorrupt {
        family: &'static str,
        form: i64,
        check: &'static str,
        locator: String,
    },
    #[error("entity object access failed: {0:?}")]
    EntityAccess(std::io::ErrorKind),
    #[error(transparent)]
    EntityPublication(crate::blob::BlobError),
    #[error(transparent)]
    TaskPublication(crate::blob::BlobError),
    #[error("SQLite counter {name} is missing for project {project} with existing rows")]
    CounterMissing { project: String, name: &'static str },
    #[error("SQLite counter {name} is negative ({value}) for project {project}")]
    CounterNegative {
        project: String,
        name: &'static str,
        value: i64,
    },
    #[error("SQLite counter {name} ({value}) is behind existing rows for project {project}")]
    CounterBehind {
        project: String,
        name: &'static str,
        value: i64,
    },
    #[error("value cannot be represented losslessly in the SQLite schema: {0}")]
    Unrepresentable(String),
    #[error("operation failed: {operation}; rollback failed: {rollback}")]
    OperationRollback {
        operation: Box<SqliteError>,
        rollback: rusqlite::Error,
    },
    #[error("goal transaction cannot commit after a failed operation rollback")]
    TransactionAborted,
    #[error("storage admission deadline or queue bound reached")]
    Overloaded,
    /// The shared failure state is latched (R128, R140): the writer refuses every
    /// mutation, no claim is granted and no checkpoint is called until a restart.
    #[error("storage_unavailable: {0}")]
    StorageUnavailable(Failure),
    #[error(transparent)]
    Store(#[from] orch_core::store::StoreError),
    #[error("stored run event is damaged: {0}")]
    EventCorrupt(String),
    #[error("stored intent is damaged: {0}")]
    IntentCorrupt(&'static str),
    #[error("stored lease data is damaged: {0}")]
    LeaseCorrupt(String),
    #[error("stored retry data is damaged: {0}")]
    RetryMessageCorrupt(&'static str),
    #[error("SQLite writer is closed")]
    WriterClosed,
    #[error("SQLite connection and queue capacities must be greater than zero")]
    InvalidCapacity,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Confined(#[from] crate::data_dir::confined::ConfinedError),
    #[error("SQLite setting {name}: expected {expected}, read back {actual}")]
    Setting {
        name: &'static str,
        expected: String,
        actual: String,
    },
    #[error("database schema version {database} is newer than binary schema version {binary}")]
    NewerSchema { database: usize, binary: usize },
    #[error("schema migration refused: {0}")]
    Migration(String),
    #[error(
        "not enough free space for pre-migration copy: available {available}, required {required}"
    )]
    DiskFloor { available: u64, required: u64 },
    #[error("pre-migration backup verification failed: {0}")]
    Backup(String),
}
pub type Result<T> = std::result::Result<T, SqliteError>;

#[allow(
    dead_code,
    reason = "event integration is internal until the combined storage adapter lands"
)]
pub(crate) mod events;
mod read_pool;
mod writer;
pub use read_pool::ReadPool;
pub use writer::failure::{
    Class, Failure, FailureSource, FailureState, FatalCause, Observation, Role, classify,
};
pub use writer::{Lane, WriteReceipt, Writer, WriterHandle};

#[cfg(test)]
fn test_data() -> (tempfile::TempDir, crate::data_dir::DataDir) {
    use crate::data_dir::{DataDir, Holder, LockKind};
    let temp = tempfile::tempdir().unwrap();
    let data = DataDir::acquire(
        &temp.path().join("data"),
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap();
    (temp, data)
}

#[allow(
    dead_code,
    reason = "internal goal persistence slice; full Storage and safety policy are not wired yet"
)]
mod goal_adapter;

#[allow(
    dead_code,
    reason = "Context, Config, Template and Goal mapping awaits the combined T10 Storage adapter"
)]
mod cctg;

#[allow(
    dead_code,
    reason = "Internal lease adapter awaits T10 Storage integration; issue 127 holds production wiring"
)]
mod leases;

#[allow(
    dead_code,
    reason = "Internal task paging awaits T10 hydration and public cursor integration"
)]
mod task_page;

#[allow(
    dead_code,
    reason = "internal attention mapping awaits T10 Storage composition"
)]
mod attention;

#[allow(
    dead_code,
    reason = "internal request-key persistence awaits the combined T10 Storage adapter"
)]
mod request_keys;

#[allow(
    dead_code,
    reason = "internal typed intents await the combined T10 Storage adapter"
)]
mod intents;

#[allow(
    dead_code,
    reason = "task format building blocks await lossless task CRUD integration"
)]
mod task_format;

#[allow(
    dead_code,
    reason = "inactive Agent/Team/Run detail format awaiting SQL mapping"
)]
mod agent_team_run;

#[allow(dead_code, reason = "private Task SQL awaits Storage composition")]
mod task_sql;

#[allow(
    dead_code,
    reason = "internal retry mapping awaits the combined T10 Storage adapter"
)]
mod retry_message;

#[allow(
    dead_code,
    reason = "private Task list awaits Storage composition and the public cursor wiring"
)]
mod task_adapter;
