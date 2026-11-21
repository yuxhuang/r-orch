//! Synthetic capability checks, not an importer or a query authorization policy.
use rusqlite::{
    Connection, ErrorCode, OpenFlags,
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn interrupted(error: rusqlite::Error) {
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::OperationInterrupted)
    );
}

fn import_limits(c: &Connection) -> Result<()> {
    // R113 import limits. Read every value back, including the newer bundle APIs.
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, 2_097_152),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 100_000),
        (Limit::SQLITE_LIMIT_COLUMN, 100),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 20),
        (Limit::SQLITE_LIMIT_PARSER_DEPTH, 100),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 3),
        (Limit::SQLITE_LIMIT_VDBE_OP, 25_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 8),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 50),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 10),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 10),
        (Limit::SQLITE_LIMIT_SCHEMA, 128),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        c.set_limit(limit, value)?;
        assert_eq!(c.limit(limit)?, value);
    }
    Ok(())
}

fn runtime_limits(c: &Connection) -> Result<()> {
    import_limits(c)?;
    assert!(
        c.prepare("SELECT 1 UNION SELECT 2 UNION SELECT 3 UNION SELECT 4")
            .is_err()
    );
    assert!(c.prepare("SELECT ?11").is_err());
    assert!(c.execute("ATTACH ':memory:' AS forbidden", []).is_err());
    assert!(
        c.query_row("SELECT zeroblob(2097153)", [], |r| r.get::<_, Vec<u8>>(0))
            .is_err()
    );
    assert!(
        c.prepare(&format!("SELECT 1{}", " ".repeat(100_000)))
            .is_err()
    );
    // R130 query profile differs from the import profile.
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 16_384),
        (Limit::SQLITE_LIMIT_LENGTH, 1_048_576),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 64),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 8),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 8),
        (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 1_000),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
    ] {
        c.set_limit(limit, value)?;
        assert_eq!(c.limit(limit)?, value);
    }
    assert!(
        c.query_row("SELECT zeroblob(1048577)", [], |r| r.get::<_, Vec<u8>>(0))
            .is_err()
    );
    assert!(c.prepare("SELECT coalesce(1,2,3,4,5,6,7,8,9)").is_err());
    assert!(
        c.query_row("SELECT 'a' LIKE ?1", ["a".repeat(1001)], |r| r
            .get::<_, i64>(0))
            .is_err()
    );
    println!(
        "PASS import limits 14/14; query limits 7/7; seven over-limit refusals plus ATTACH refusal"
    );
    Ok(())
}

fn diagnostic_authorizer(root: &Path) -> Result<()> {
    let file = root.join("diagnostic.db");
    assert!(!file.exists());
    let writer = Connection::open(&file)?;
    writer.execute_batch("CREATE TABLE tasks(n INTEGER, secret TEXT) STRICT; INSERT INTO tasks VALUES(7,'synthetic'); CREATE VIEW diag_tasks AS SELECT n FROM tasks;")?;
    let mut c = Connection::open_with_flags(&file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let denied = Arc::new(AtomicUsize::new(0));
    let refused = denied.clone();
    c.authorizer(Some(move |ctx: AuthContext<'_>| {
        seen.fetch_add(1, Ordering::SeqCst);
        match ctx.action {
            AuthAction::Select => Authorization::Allow,
            AuthAction::Read {
                table_name: "diag_tasks",
                column_name: "n",
            } => Authorization::Allow,
            AuthAction::Read {
                table_name: "tasks",
                column_name: "n",
            } if ctx.accessor == Some("diag_tasks") => Authorization::Allow,
            AuthAction::Function {
                function_name: "count",
            } => Authorization::Allow,
            _ => {
                refused.fetch_add(1, Ordering::SeqCst);
                Authorization::Deny
            }
        }
    }))?;
    assert_eq!(
        c.query_row("SELECT n FROM diag_tasks", [], |r| r.get::<_, i64>(0))?,
        7
    );
    for sql in [
        "SELECT n FROM tasks",
        "SELECT secret FROM tasks",
        "SELECT count(*) FROM tasks",
        "WITH x AS (SELECT secret FROM tasks) SELECT * FROM x",
        "SELECT name FROM sqlite_schema",
        "SELECT random() FROM diag_tasks",
        "ATTACH ':memory:' AS forbidden",
        "PRAGMA user_version",
        "CREATE TABLE forbidden(n)",
        "UPDATE tasks SET n=8",
        "SELECT load_extension('forbidden')",
    ] {
        let before = denied.load(Ordering::SeqCst);
        let error = c.prepare(sql).expect_err(sql);
        assert!(denied.load(Ordering::SeqCst) > before, "{sql}: {error:?}");
    }
    // Keep the callback installed while stepping; force automatic reprepare after preparation.
    let mut statement = c.prepare("SELECT n FROM diag_tasks")?;
    let prepared_calls = calls.load(Ordering::SeqCst);
    writer.execute_batch("CREATE TABLE schema_change(n INTEGER) STRICT;")?;
    assert_eq!(statement.query_row([], |r| r.get::<_, i64>(0))?, 7);
    assert!(calls.load(Ordering::SeqCst) > prepared_calls);
    drop(statement);
    assert_eq!(
        writer.query_row("SELECT n FROM tasks", [], |r| r.get::<_, i64>(0))?,
        7
    );
    println!(
        "PASS diagnostic view; 11 authorizer denials; callback retained across step/reprepare; row unchanged"
    );
    Ok(())
}

fn cancellation() -> Result<()> {
    const LONG: &str = "WITH RECURSIVE x(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<100000000) SELECT sum(n) FROM x";
    let mut c = Connection::open_in_memory()?;
    let steps = Arc::new(AtomicUsize::new(0));
    let seen = steps.clone();
    c.progress_handler(100, Some(move || seen.fetch_add(1, Ordering::SeqCst) >= 10))?;
    interrupted(c.query_row(LONG, [], |r| r.get::<_, i64>(0)).unwrap_err());
    assert!(steps.load(Ordering::SeqCst) >= 11);
    c.progress_handler(0, None::<fn() -> bool>)?;
    assert_eq!(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?, 1);
    // Handshake from an executing VM, not a timer race before the statement starts.
    let handle = c.get_interrupt_handle();
    let (ready, waiting) = std::sync::mpsc::sync_channel(1);
    let mut ready = Some(ready);
    c.progress_handler(
        100,
        Some(move || {
            if let Some(sender) = ready.take() {
                sender.send(()).expect("interrupt observer");
            }
            false
        }),
    )?;
    let observer = std::thread::spawn(move || {
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("running VM");
        handle.interrupt();
    });
    let result = c.query_row(LONG, [], |r| r.get::<_, i64>(0));
    observer.join().expect("interrupt observer joined");
    interrupted(result.unwrap_err());
    c.progress_handler(0, None::<fn() -> bool>)?;
    assert_eq!(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?, 1);
    println!(
        "PASS progress budget and cross-thread interrupt: SQLITE_INTERRUPT; connections reusable"
    );
    Ok(())
}

fn immutable_source(root: &Path) -> Result<()> {
    let file = root.join("source.db");
    assert!(!file.exists());
    let c = Connection::open(&file)?;
    c.execute_batch("CREATE TABLE t(n INTEGER) STRICT; INSERT INTO t VALUES(42); CREATE VIEW forbidden_view AS SELECT n FROM t;")?;
    c.close().map_err(|(_, e)| e)?;
    let original = std::fs::read(&file)?;
    let companions = ["source.db-wal", "source.db-shm", "source.db-journal"];
    for name in companions {
        std::fs::write(root.join(name), b"synthetic companion canary")?;
    }
    let mut source = Connection::open_with_flags(
        format!(
            "file:{}?mode=ro&immutable=1",
            file.to_str().ok_or("UTF-8 scratch path required")?
        ),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    for (setting, enabled) in [
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
    ] {
        assert_eq!(source.set_db_config(setting, enabled)?, enabled);
        assert_eq!(source.db_config(setting)?, enabled);
    }
    import_limits(&source)?;
    source.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Select
        | AuthAction::Read {
            table_name: "t",
            column_name: "n",
        } => Authorization::Allow,
        AuthAction::Pragma {
            pragma_name:
                "hard_heap_limit" | "mmap_size" | "cell_size_check" | "cache_size" | "quick_check",
            ..
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }))?;
    source.progress_handler(1_000, Some(|| false))?;
    let heap: i64 = source.query_row("PRAGMA hard_heap_limit=268435456", [], |r| r.get(0))?;
    assert_eq!(heap, 268_435_456);
    source
        .execute_batch("PRAGMA mmap_size=0; PRAGMA cell_size_check=ON; PRAGMA cache_size=-8192;")?;
    assert_eq!(
        source.query_row("PRAGMA mmap_size", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        source.query_row("PRAGMA cell_size_check", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        source.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0))?,
        -8192
    );
    assert_eq!(
        source.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    source.progress_handler(1, Some(|| true))?;
    interrupted(
        source
            .query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0))
            .unwrap_err(),
    );
    source.progress_handler(0, None::<fn() -> bool>)?;
    assert_eq!(
        source.query_row("SELECT n FROM t", [], |r| r.get::<_, i64>(0))?,
        42
    );
    assert!(source.prepare("SELECT * FROM forbidden_view").is_err());
    assert!(
        source
            .prepare("SELECT load_extension('forbidden')")
            .is_err()
    );
    assert!(source.execute("UPDATE t SET n=0", []).is_err());
    drop(source);
    assert_eq!(std::fs::read(&file)?, original);
    for name in companions {
        assert_eq!(
            std::fs::read(root.join(name))?,
            b"synthetic companion canary"
        );
    }
    println!(
        "PASS immutable read-only source; defensive/untrusted/triggers/views config 4/4; import limits 14/14; heap/mmap/cell/cache; quick_check and its interruption; unchanged DB and 3 companions"
    );
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    runtime_limits(&Connection::open_in_memory()?)?;
    diagnostic_authorizer(root)?;
    cancellation()?;
    immutable_source(root)?;
    Ok(())
}

// A separate executable invocation: the heap ceiling is process-wide and SQL cannot raise it.
pub fn heap_limit() -> Result<()> {
    let c = Connection::open_in_memory()?;
    assert_eq!(
        c.query_row("PRAGMA hard_heap_limit=8388608", [], |r| r.get::<_, i64>(0))?,
        8_388_608
    );
    let error = c
        .query_row("SELECT zeroblob(16777216)", [], |r| r.get::<_, Vec<u8>>(0))
        .unwrap_err();
    assert_eq!(error.sqlite_error_code(), Some(ErrorCode::OutOfMemory));
    assert_eq!(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?, 1);
    println!(
        "PASS process-wide hard_heap_limit: 8 MiB ceiling refuses 16 MiB allocation with SQLITE_NOMEM; connection reusable"
    );
    Ok(())
}
