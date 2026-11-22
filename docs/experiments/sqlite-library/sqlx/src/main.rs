use sqlx::{
    Connection, Executor,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode},
};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("scratch directory required")?,
    );
    let directory = root.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(directory)).await??;
    let file = root.join("probe.db");
    assert!(!file.exists());
    let opts = SqliteConnectOptions::new()
        .filename(file)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let mut c = SqliteConnection::connect_with(&opts).await?;
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,n INTEGER NOT NULL,label TEXT NOT NULL) STRICT, WITHOUT ROWID;").await?;
    c.execute("CREATE UNIQUE INDEX partial ON t(label) WHERE n>0; CREATE INDEX expr ON t(lower(label)); CREATE VIRTUAL TABLE search USING fts5(body);").await?;
    let mut tx = c.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO t VALUES(1,10,'alpha')")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    assert!(
        c.execute("INSERT INTO t VALUES(2,'bad','beta')")
            .await
            .is_err()
    );
    c.execute("INSERT INTO search VALUES('orch search')")
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM search WHERE search MATCH 'orch'")
            .fetch_one(&mut c)
            .await?,
        1
    );
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&mut c)
        .await?;
    println!("PASS SQLite {version}: STRICT, FTS5, WAL, IMMEDIATE, indexes, WITHOUT ROWID");
    Ok(())
}
