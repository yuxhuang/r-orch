//! SQLite connection setup and forward-only schema migrations (T9).
//! The file backend remains the product backend until the T24 switch.
mod connection;
pub mod migrations;
pub use connection::{ConnectionHandle, ConnectionRole, Synchronous, open_connection};

#[derive(Debug, thiserror::Error)]
pub enum SqliteError {
    #[error("storage admission deadline or queue bound reached")]
    Overloaded,
    #[error(transparent)]
    Store(#[from] orch_core::store::StoreError),
    #[error("stored run event is damaged: {0}")]
    EventCorrupt(String),
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
