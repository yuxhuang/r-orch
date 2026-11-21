#![forbid(unsafe_code)]
mod security;
use rusqlite::{Connection, OpenFlags, TransactionBehavior, backup::Backup};
use rusqlite_migration::{M, Migrations};
use std::time::Duration;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--heap-limit") {
        return security::heap_limit();
    }
    let root = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("scratch directory required")?,
    );
    std::fs::create_dir_all(&root)?;
    let file = root.join("probe.db");
    assert!(!file.exists());
    let mut c = Connection::open(&file)?;
    c.busy_timeout(Duration::from_secs(5))?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    c.set_transaction_behavior(TransactionBehavior::Immediate);
    let migrations = Migrations::new(vec![
        M::up(
            "CREATE TABLE t(id INTEGER PRIMARY KEY, n INTEGER NOT NULL, label TEXT NOT NULL) STRICT, WITHOUT ROWID;",
        ),
        M::up(
            "CREATE UNIQUE INDEX partial ON t(label) WHERE n>0; CREATE INDEX expr ON t(lower(label)); CREATE VIRTUAL TABLE search USING fts5(body);",
        ),
    ]);
    // One library invocation per numbered migration = one transaction each.
    for version in 1..=2 {
        migrations.to_version(&mut c, version)?;
    }
    migrations.to_latest(&mut c)?; // No-op restart.
    c.execute("INSERT INTO t VALUES(1, 10, 'alpha')", [])?;
    assert!(
        c.execute("INSERT INTO t VALUES(2, 'bad', 'beta')", [])
            .is_err()
    );
    assert!(
        c.execute("INSERT INTO t VALUES(2, 10, 'alpha')", [])
            .is_err()
    );
    c.execute("INSERT INTO search VALUES('orch search')", [])?;
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM search WHERE search MATCH 'orch'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        c.query_row("SELECT n FROM t WHERE lower(label)='alpha'", [], |r| r
            .get::<_, i64>(0))?,
        10
    );
    assert!(c.prepare("SELECT rowid FROM t").is_err());
    assert_eq!(
        c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))?,
        "wal"
    );
    assert_eq!(
        c.query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        c.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        c.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))?,
        5000
    );
    // Fixed read snapshot while a different connection commits in WAL.
    let reader = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    reader.execute_batch("BEGIN;")?;
    assert_eq!(
        reader.query_row("SELECT sum(n) FROM t", [], |r| r.get::<_, i64>(0))?,
        10
    );
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("UPDATE t SET n=11 WHERE id=1", [])?;
    tx.commit()?;
    assert_eq!(
        reader.query_row("SELECT sum(n) FROM t", [], |r| r.get::<_, i64>(0))?,
        10
    );
    assert!(
        reader
            .execute("INSERT INTO t VALUES(2,0,'blocked')", [])
            .is_err()
    );
    // Backup the pinned source snapshot through the safe online API.
    let mut dest = Connection::open(root.join("backup.db"))?;
    Backup::new(&reader, &mut dest)?.run_to_completion(1, Duration::from_millis(1), None)?;
    assert_eq!(
        dest.query_row("SELECT sum(n) FROM t", [], |r| r.get::<_, i64>(0))?,
        10
    );
    assert_eq!(
        dest.query_row(
            "SELECT count(*) FROM search WHERE search MATCH 'orch'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        dest.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    reader.execute_batch("ROLLBACK;")?;
    assert_eq!(
        reader.query_row("SELECT sum(n) FROM t", [], |r| r.get::<_, i64>(0))?,
        11
    );
    // Failed migration leaves prior version and rows intact, never silently repairs.
    let failing = Migrations::new(vec![
        M::up("CREATE TABLE keep(n INTEGER) STRICT;"),
        M::up("CREATE TABLE undo(n INTEGER) STRICT; INSERT INTO absent VALUES(1);"),
    ]);
    let mut m = Connection::open_in_memory()?;
    failing.to_version(&mut m, 1)?;
    assert!(failing.to_version(&mut m, 2).is_err());
    assert_eq!(
        m.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert!(m.prepare("SELECT * FROM undo").is_err());
    m.pragma_update(None, "user_version", 3)?;
    assert!(failing.to_latest(&mut m).is_err());
    security::check(&root)?;
    println!(
        "PASS SQLite {}: STRICT, FTS5, WAL, IMMEDIATE, indexes, WITHOUT ROWID, read-only snapshot, online backup 10 vs live 11, migration rollback/newer refusal",
        rusqlite::version()
    );
    Ok(())
}
