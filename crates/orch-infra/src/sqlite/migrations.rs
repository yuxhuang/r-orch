//! Numbered forward-only migrations, applied one transaction at a time.
use super::{ConnectionHandle, ConnectionRole, Result, SqliteError, Synchronous};
use crate::data_dir::DataDir;
use crate::data_dir::confined::{ConfinedDir, FileAccess};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

pub const SCHEMA_VERSION: usize = 4;
pub const AGENT_TEAM_RUN: &str = include_str!("migrations/004_agent_team_run.sql");
pub const TASK_FORMAT: &str = include_str!("migrations/003_task_format.sql");
pub const ATTENTION_FACTS: &str = include_str!("migrations/002_attention_facts.sql");
pub const INITIAL_SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, Clone, Copy)]
pub struct DiskSpace {
    pub available: u64,
    pub total: u64,
}
impl DiskSpace {
    pub fn read(data: &DataDir) -> Result<Self> {
        let stat = nix::sys::statvfs::fstatvfs(data.root()).map_err(std::io::Error::from)?;
        let block = stat.fragment_size();
        Ok(Self {
            available: capacity_bytes(stat.blocks_available(), block),
            total: capacity_bytes(stat.blocks(), block),
        })
    }
}

// fsblkcnt_t is 32 bits on macOS and 64 bits on Linux.
fn capacity_bytes(blocks: impl Into<u64>, block: u64) -> u64 {
    blocks.into().saturating_mul(block)
}

pub fn migrate(data: &DataDir, conn: &mut ConnectionHandle<'_>) -> Result<()> {
    conn.ensure_directory(data)?;
    apply_migrations(
        data,
        conn,
        &[INITIAL_SCHEMA, ATTENTION_FACTS, TASK_FORMAT, AGENT_TEAM_RUN],
        DiskSpace::read(data)?,
    )
}

/// A migration set is trusted SQL shipped with the binary, never request input.
/// Disk space is supplied by the startup probe; tests use synthetic capacity.
/// The directory must be the same guard used to open the connection.
pub fn apply_migrations(
    data: &DataDir,
    conn: &mut ConnectionHandle<'_>,
    steps: &[&str],
    space: DiskSpace,
) -> Result<()> {
    conn.ensure_directory(data)?;
    let conn = conn.for_migration();
    let version = conn.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))? as usize;
    if version > steps.len() {
        return Err(SqliteError::NewerSchema {
            database: version,
            binary: steps.len(),
        });
    }
    if version > 0 {
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if versions != (1..=version as i64).collect::<Vec<_>>() {
            return Err(SqliteError::Migration(
                "schema_migrations disagrees with user_version".into(),
            ));
        }
    }
    if version == steps.len() {
        return Ok(());
    }
    // Refuse a known-lossy predecessor before any intermediate migration or
    // checked copy. Keep the hook too: it is the migration's transactional guard.
    if version > 0 && steps.iter().skip(version).any(|step| *step == TASK_FORMAT) {
        refuse_lossy_tasks(conn).map_err(|error| {
            SqliteError::Migration(rusqlite_migration::Error::from(error).to_string())
        })?;
    }
    if version > 0
        && steps
            .iter()
            .skip(version)
            .any(|step| *step == AGENT_TEAM_RUN)
    {
        refuse_lossy_entities(conn).map_err(|error| {
            SqliteError::Migration(rusqlite_migration::Error::from(error).to_string())
        })?;
    }
    let holds_project = version > 0
        && conn.query_row::<i64, _, _>("SELECT count(*) FROM projects", [], |r| r.get(0))? > 0;
    if holds_project {
        let pages = u64::from(conn.pragma_query_value(None, "page_count", |r| r.get::<_, u32>(0))?);
        let page_size =
            u64::from(conn.pragma_query_value(None, "page_size", |r| r.get::<_, u32>(0))?);
        let required = (5 * 1024 * 1024 * 1024)
            .max(space.total / 20)
            .saturating_add(pages.saturating_mul(page_size));
        if space.available < required {
            return Err(SqliteError::DiskFloor {
                available: space.available,
                required,
            });
        }
        checked_copy(data, conn, version)?;
    }
    let scripts: Vec<String> = steps.iter().enumerate().map(|(i, sql)| format!("{sql};\nINSERT INTO schema_migrations(version, applied_at) VALUES({}, CAST(unixepoch('subsec') * 1000 AS INTEGER));", i+1)).collect();
    let migrations = Migrations::new(
        scripts
            .iter()
            .zip(steps)
            .map(|(sql, step)| {
                if *step == TASK_FORMAT {
                    // A hook with empty SQL checks the predecessor inside the same
                    // migration transaction, before any schema or ledger change.
                    let sql = sql.clone();
                    M::up_with_hook("", move |tx: &rusqlite::Transaction<'_>| {
                        refuse_lossy_tasks(tx)?;
                        tx.execute_batch(&sql)?;
                        Ok(())
                    })
                } else if *step == AGENT_TEAM_RUN {
                    let sql = sql.clone();
                    M::up_with_hook("", move |tx: &rusqlite::Transaction<'_>| {
                        refuse_lossy_entities(tx)?;
                        tx.execute_batch(&sql)?;
                        Ok(())
                    })
                } else {
                    M::up(sql)
                }
            })
            .collect(),
    );
    for target in version + 1..=steps.len() {
        migrations
            .to_version(conn, target)
            .map_err(|e| SqliteError::Migration(e.to_string()))?;
    }
    if holds_project && let Err(error) = retain_two(data) {
        // Every migration has already committed. Cleanup cannot undo it or
        // truthfully turn that success into a migration failure.
        tracing::warn!(%error, "could not prune old pre-migration copies after migration committed");
    }
    Ok(())
}

// No predecessor task can recover ordered duplicates, absent lists, workspace
// or proof. Do not fabricate a backfill from the value-keyed memberships.
fn refuse_lossy_tasks(tx: &Connection) -> rusqlite_migration::HookResult {
    let mut counts = Vec::new();
    for table in [
        "tasks",
        "task_dependencies",
        "task_labels",
        "task_scopes",
        "task_attachments",
    ] {
        let count: i64 =
            tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        counts.push((table, count));
    }
    if counts.iter().any(|(_, count)| *count != 0) {
        let summary = counts
            .iter()
            .map(|(table, count)| format!("{table}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(rusqlite_migration::HookError::Hook(format!(
            "task format v1 refuses ambiguous predecessor rows: {summary}"
        )));
    }
    Ok(())
}

// None of these predecessor projections is a lossless entity authority.
// Refuse orphan memberships too; silently dropping them would make loss durable.
fn refuse_lossy_entities(conn: &Connection) -> rusqlite_migration::HookResult {
    let mut counts = Vec::new();
    for table in ["agents", "teams", "team_members", "team_tasks", "runs"] {
        let count: i64 =
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        counts.push((table, count));
    }
    if counts.iter().any(|(_, count)| *count != 0) {
        let summary = counts
            .iter()
            .map(|(table, count)| format!("{table}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(rusqlite_migration::HookError::Hook(format!(
            "entity format v1 refuses ambiguous predecessor rows: {summary}"
        )));
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    kind: String,
    schema_version: usize,
    files: Vec<BackupFile>,
}
#[derive(Debug, Serialize, Deserialize)]
struct BackupFile {
    name: String,
    size: u64,
    sha256: String,
}

fn hash_file(dir: &ConfinedDir) -> Result<(u64, String)> {
    let mut file = dir.open_file("orch.db", FileAccess::Untrusted)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hash.update(&buf[..n]);
    }
    Ok((size, format!("{:x}", hash.finalize())))
}

fn checked_copy(data: &DataDir, source: &Connection, version: usize) -> Result<()> {
    let (name, dir, path) = data
        .create_migration_backup(version)
        .map_err(|e| SqliteError::Backup(e.to_string()))?;
    let copy = || -> Result<()> {
        let mut dest = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        super::connection::configure(&mut dest, ConnectionRole::Writer, Synchronous::Full)?;
        {
            let backup = Backup::new(source, &mut dest)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match backup.step(128)? {
                    StepResult::Done => break,
                    StepResult::More => (),
                    StepResult::Busy | StepResult::Locked => {
                        if Instant::now() >= deadline {
                            return Err(SqliteError::Backup(
                                "online copy stayed busy for 5 seconds".into(),
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => return Err(SqliteError::Backup("unknown online copy result".into())),
                }
            }
        }
        let checks: Vec<String> = dest
            .prepare("PRAGMA integrity_check")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if checks != ["ok"] {
            return Err(SqliteError::Backup(format!("integrity_check: {checks:?}")));
        }
        dest.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        dest.close().map_err(|(_, e)| e)?;
        dir.open_file("orch.db", FileAccess::Modify)?.sync_all()?;
        let (size, sha256) = hash_file(&dir)?;
        let manifest = Manifest {
            kind: "database_only".into(),
            schema_version: version,
            files: vec![BackupFile {
                name: "orch.db".into(),
                size,
                sha256,
            }],
        };
        let mut file = dir.create_file("manifest.json")?;
        file.write_all(&serde_json::to_vec(&manifest).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        dir.sync()?;
        data.root().open_dir("backups")?.sync()?;
        // Read independently from the descriptors, not the values just written.
        let mut bytes = Vec::new();
        dir.open_file("manifest.json", FileAccess::Untrusted)?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(SqliteError::Backup("manifest exceeds 4096 bytes".into()));
        }
        let read: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| SqliteError::Backup(e.to_string()))?;
        let (size, hash) = hash_file(&dir)?;
        if read.kind != "database_only"
            || read.schema_version != version
            || read.files.len() != 1
            || read.files[0].name != "orch.db"
            || read.files[0].size != size
            || read.files[0].sha256 != hash
        {
            return Err(SqliteError::Backup("manifest does not match copy".into()));
        }
        Ok(())
    };
    if let Err(error) = copy() {
        // Only this attempt's fixed entries are scratch. Never sweep arbitrary
        // names or another backup when cleaning a failed copy.
        for name in ["orch.db-wal", "orch.db-shm", "manifest.json", "orch.db"] {
            let _ = dir.remove_file(name);
        }
        let _ = data.root().open_dir("backups")?.remove_dir(name);
        return Err(error);
    }
    Ok(())
}

fn retain_two(data: &DataDir) -> Result<()> {
    let backups = data.root().open_dir("backups")?;
    let mut copies = Vec::new();
    for name in backups.list()? {
        let Some(text) = name.to_str() else { continue };
        let Some(rest) = text.strip_prefix("pre-migration-v") else {
            continue;
        };
        let Some((version, at)) = rest.split_once('-') else {
            continue;
        };
        if version.parse::<usize>().is_err() {
            continue;
        }
        let Ok(at) = at.parse::<u128>() else { continue };
        let dir = match backups.open_dir(&name) {
            Ok(dir) => dir,
            Err(_) => continue,
        };
        // A crash may leave a partial copy. Preserve it for inspection and do
        // not let it displace a completed copy in the retention count.
        if dir.list()?
            != [
                std::ffi::OsString::from("manifest.json"),
                std::ffi::OsString::from("orch.db"),
            ]
        {
            continue;
        }
        copies.push((at, name));
    }
    copies.sort_by_key(|r| r.0);
    let remove = copies.len().saturating_sub(2);
    for (_, name) in copies.into_iter().take(remove) {
        let dir = backups.open_dir(&name)?;
        let names = dir.list()?;
        if names
            != [
                std::ffi::OsString::from("manifest.json"),
                std::ffi::OsString::from("orch.db"),
            ]
        {
            return Err(SqliteError::Backup(
                "unexpected entry in old migration copy".into(),
            ));
        }
        dir.remove_file("orch.db")?;
        dir.remove_file("manifest.json")?;
        backups.remove_dir(name)?;
    }
    backups.sync()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::{Class, Role, classify, open_connection, test_data};

    fn plenty() -> DiskSpace {
        DiskSpace {
            available: 100 * 1024 * 1024 * 1024,
            total: 200 * 1024 * 1024 * 1024,
        }
    }

    #[test]
    fn task_format_hook_refuses_late_predecessor_and_preserves_committed_intermediate() {
        let (_tmp, data) = test_data();
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        // Starting at zero skips preflight. A trusted synthetic second step
        // creates an orphan membership after 001 commits; only 003's hook
        // can refuse it. No shipped migration SQL is changed.
        let intermediate = "CREATE TABLE committed_marker(value TEXT); INSERT INTO committed_marker VALUES('kept'); INSERT INTO task_labels VALUES('p','synthetic-task','synthetic-label');";
        let error = apply_migrations(
            &data,
            &mut conn,
            &[INITIAL_SCHEMA, intermediate, TASK_FORMAT],
            plenty(),
        )
        .unwrap_err();
        assert!(matches!(error, SqliteError::Migration(_)), "{error:?}");
        assert!(error.to_string().contains("task_labels=1"), "{error}");
        assert!(!error.to_string().contains("synthetic-"), "{error}");
        assert_eq!(
            classify(&error).leaves(),
            &[(Role::Operation, Class::Ordinary)]
        );
        // The durable predecessor, its ledger and 003's absent tables form
        // one atomic refusal contract, including after closing and reopening.
        drop(conn);
        let conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        let state: (i64, String, i64, i64, String) = conn.query_row(
            "SELECT (SELECT user_version FROM pragma_user_version), (SELECT group_concat(version, ',') FROM (SELECT version FROM schema_migrations ORDER BY version)), (SELECT count(*) FROM sqlite_schema WHERE name IN ('task_details','task_blob_refs','task_blob_refs_by_hash')), (SELECT count(*) FROM task_labels WHERE project_id='p' AND task_id='synthetic-task' AND label='synthetic-label'), (SELECT value FROM committed_marker)",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).unwrap();
        assert_eq!(state, (2, "1,2".into(), 0, 1, "kept".into()));
        assert!(
            data.root()
                .open_dir("backups")
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn task_format_preflight_statement_error_keeps_its_sqlite_category() {
        let (_tmp, data) = test_data();
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        apply_migrations(&data, &mut conn, &[INITIAL_SCHEMA], plenty()).unwrap();
        // A malformed synthetic predecessor makes the actual preflight SELECT
        // fail. This must differ structurally from the intentional row refusal.
        conn.execute_batch("DROP TABLE task_labels").unwrap();
        let error = apply_migrations(
            &data,
            &mut conn,
            &[INITIAL_SCHEMA, ATTENTION_FACTS, TASK_FORMAT],
            plenty(),
        )
        .unwrap_err();
        assert!(
            matches!(error, SqliteError::Sql(rusqlite::Error::SqliteFailure(ref code, _)) if code.extended_code == rusqlite::ffi::SQLITE_ERROR),
            "preflight lost its SQLite category: {error:?}"
        );
    }
}
