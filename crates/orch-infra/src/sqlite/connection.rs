use super::{Result, SqliteError};
use crate::data_dir::DataDir;
use crate::data_dir::confined::{ConfinedError, FileAccess};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::ops::{Deref, DerefMut};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionRole {
    Writer,
    Checkpoint,
    Reader,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Synchronous {
    #[default]
    Normal,
    Full,
}

/// A live connection cannot outlive its data directory's exclusive guard.
pub struct ConnectionHandle<'a> {
    connection: Connection,
    _directory: &'a DataDir,
}
impl Deref for ConnectionHandle<'_> {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}
impl DerefMut for ConnectionHandle<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

pub fn open_connection(
    data: &DataDir,
    role: ConnectionRole,
    sync: Synchronous,
) -> Result<ConnectionHandle<'_>> {
    // SQLite opens these fixed names itself. Check descriptors first as defence
    // in depth; the locked 0700 root is the actual protection for its VFS.
    let access = if role == ConnectionRole::Reader {
        FileAccess::Read
    } else {
        FileAccess::Modify
    };
    for name in ["orch.db", "orch.db-wal", "orch.db-shm"] {
        match data.root().open_file(name, access) {
            Ok(_) => (),
            Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    if role != ConnectionRole::Reader {
        match data.root().create_file("orch.db") {
            Ok(_) => (),
            Err(ConfinedError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
    }
    let flags = if role == ConnectionRole::Reader {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let mut conn = Connection::open_with_flags(
        data.database_path(),
        flags | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let version = conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))? as usize;
    if version > super::migrations::SCHEMA_VERSION {
        return Err(SqliteError::NewerSchema {
            database: version,
            binary: super::migrations::SCHEMA_VERSION,
        });
    }
    configure(&mut conn, role, sync)?;
    Ok(ConnectionHandle {
        connection: conn,
        _directory: data,
    })
}

pub(super) fn configure(
    conn: &mut Connection,
    role: ConnectionRole,
    sync: Synchronous,
) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let journal: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    if journal != "wal" {
        return Err(SqliteError::Setting {
            name: "journal_mode",
            expected: "wal".into(),
            actual: journal,
        });
    }
    let sync = match sync {
        Synchronous::Normal => 1,
        Synchronous::Full => 2,
    };
    set_and_check(conn, "synchronous", sync)?;
    set_and_check(conn, "foreign_keys", 1)?;
    check(conn, "busy_timeout", 5000)?;
    if role != ConnectionRole::Reader {
        set_and_check(conn, "wal_autocheckpoint", 0)?;
        set_and_check(conn, "fullfsync", 1)?;
        set_and_check(conn, "checkpoint_fullfsync", 1)?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
    }
    Ok(())
}

fn set_and_check(conn: &Connection, name: &'static str, expected: i64) -> Result<()> {
    conn.pragma_update(None, name, expected)?;
    check(conn, name, expected)
}
fn check(conn: &Connection, name: &'static str, expected: i64) -> Result<()> {
    let actual: i64 = conn.pragma_query_value(None, name, |r| r.get(0))?;
    if actual != expected {
        return Err(SqliteError::Setting {
            name,
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}
