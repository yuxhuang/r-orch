//! SQLite connection setup and forward-only schema migrations (T9).
//! The file backend remains the product backend until the T24 switch.
mod connection;
pub mod migrations;
pub use connection::{ConnectionHandle, ConnectionRole, Synchronous, open_connection};

#[derive(Debug, thiserror::Error)]
pub enum SqliteError {
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
