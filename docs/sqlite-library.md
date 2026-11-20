# SQLite library recommendation for release 1.1

2026-11-20 · Entry step C of `goal_sMVh4SG`; input to the storage ADR, pending the owner gate.
Repository baseline: `fb6a1e1` (after v1.0.0). No product dependency or storage format changes here.

**Recommend `rusqlite` 0.41.1 with `bundled`, `backup` and `cache`, defaults off, and
`rusqlite_migration` 2.7.0 for migrations.** Add these to `[workspace.dependencies]` and
use them from `orch-infra` only after approval. Commit the resolving product lockfile then.

```toml
rusqlite = { version = "=0.41.1", default-features = false, features = ["bundled", "backup", "cache"] }
rusqlite_migration = { version = "=2.7.0", default-features = false }
```

## Comparison and library-first check

| Check | Chosen: rusqlite 0.41.1 | Alternative: SQLx 0.9.1 |
|---|---|---|
| Features checked | `bundled,backup,cache`, defaults off | `sqlite-bundled,runtime-tokio,migrate`, defaults off; no query macros, TLS or other database driver enabled |
| Maintenance | Active upstream; 0.41.1 published October 28; latest 0.43.0 published November 15 | Active upstream; 0.9.1 published November 18 |
| Declared MSRV | 1.89; migration crate 1.89 | 1.94 |
| Pinned compiler | Builds with repository Rust 1.99.1 | Builds with repository Rust 1.99.1 |
| Licences | rusqlite and libsqlite3-sys: MIT; migration crate: Apache-2.0 | MIT OR Apache-2.0; libsqlite3-sys: MIT |
| Bundled engine resolved | libsqlite3-sys 0.39.1, SQLite 3.54.1 | libsqlite3-sys 0.40.0, SQLite 3.54.1 |
| SQL capabilities | STRICT, FTS5, WAL, immediate transactions, partial/expression indexes, WITHOUT ROWID: executed successfully | Same DDL and FTS5 query executed successfully |
| Async integration | Synchronous API; orch owns the blocking writer and bounded read execution | Async API backed by a worker thread per SQLite connection; built-in async pool |
| Online backup | Safe `backup::Backup` API, exercised against a pinned read snapshot while another connection commits | No equivalent safe backup wrapper found in the checked driver; `lock_handle` exposes the raw SQLite handle, requiring an unsafe FFI integration or a separate compatible wrapper |
| Migrations | Small maintained runner; numbered SQL and transactional hook for orch's history table | Built-in migrator and checksums; requires configuring its transaction and history-table contract for orch |
| Why it wins/loses | Direct backup support and explicit connection ownership suit the already-required single writer | Pool/query ergonomics are useful, but orch still needs its priority writer queue; raw-handle backup adds work and conflicts with the workspace's unsafe-code prohibition |

The version choice is deliberate: `rusqlite_migration` 2.7.0 requires rusqlite `^0.41.0`;
Refinery's current adapter (`refinery-core` 0.10.1) also stops at 0.41. Latest rusqlite 0.43
would require a different migration integration. No long-term maintenance promise for the 0.41
line is assumed. Recheck releases, the SQLite bundle and advisories before implementation.
FTS5 is compiled by libsqlite3-sys's bundled build; no separate rusqlite `vtab` feature is needed
to execute FTS5 SQL. STRICT requires SQLite 3.37 or later.

**Dependency check: PASS.** With cargo-deny 0.20.2, both `cargo deny check` on the baseline
and the same check on a disposable full workspace with both chosen dependencies connected to
`orch-infra` exited 0: `advisories ok, bans ok, licenses ok, sources ok`.
The repository's `deny.toml` was unchanged, including its Linux target, all-features setting,
licence allow-list and two existing owner-approved advisory exceptions. The baseline's 13
duplicate-version warnings remained; the candidate added no warning or exception.
RustSec database revision: `f291af9b1cc0d21dfcea1709d6b037963cef262b`.
This checks the resolved Rust dependency graph, not every SQLite C vulnerability. The
[SQLite 3.54.1 release notes](https://sqlite.org/releaselog/3_54_1.html) were also checked;
review SQLite release/security notes again when updating the bundled engine.

## Runtime and migration contract

One owned OS thread executes the writer's bounded priority queues, keeps its connection,
and runs each unit of work with `TransactionBehavior::Immediate`. Tokio callers send commands
and await replies; no SQLite call, busy wait or synchronous mutex runs on a Tokio worker.
Use a bounded read-only connection pool: acquire capacity asynchronously, then execute the
whole read transaction through `spawn_blocking`, returning owned results before releasing it.
A connection belongs to one operation at a time. Start a read transaction and perform its first
read to establish the snapshot; keep all statements of that operation on that connection.
Set WAL on the writer; set foreign keys, synchronous mode and busy timeout as appropriate on
every connection. Open readers with `SQLITE_OPEN_READ_ONLY`; `query_only` alone is not the boundary.

An async timeout does not stop a running `spawn_blocking` closure. Implement the specified
30-second read limit with a SQLite interrupt handle and a retained job handle;
wait for rollback before returning that connection to the pool. Backpressure and shutdown
must drain/join the owned work. Scheduling and event/claim latency remain entry step D's measurement.

Run numbered, immutable, forward-only SQL migrations at start, after acquiring the data-directory
lock and before serving requests. Take the required pre-migration backup through its hook first.
Use the maintained runner, with no `.down()` migrations: set the connection's default transaction
behaviour to Immediate and call `to_version(current + 1)` repeatedly. **Do not call `to_latest`
for multiple pending migrations:** it groups them in one transaction. The probe demonstrates a
failed second migration rolls back while version 1 remains, restart is a no-op, and a newer
version is refused.

The runner stores its version in `PRAGMA user_version`, not `schema_migrations`. The backend
must create the goal-required STRICT `schema_migrations` ledger and insert each numbered
migration's identity/checksum in its `up_with_hook` transaction. Validate that ledger against
the embedded migration prefix and `user_version` before changing anything; reject disagreement,
changed checksums and newer schemas. This adapter glue and its crash tests belong to the schema
implementation task, not this probe. Existing rows are backfilled in the numbered migration's
transaction, or through an explicitly resumable plan if too large; never rewrite applied SQL.
Rollback means restoring the pre-migration backup with its matching binary, not a down migration.

Use a dedicated read-only source connection and separate destination for incremental online backup.
The probe pins a WAL read snapshot, commits a writer update, and backs up the old snapshot correctly.
Production backup also needs deadlines/retry bounds and coordination of blobs/repos; SQLite's API
alone does not prove the phase's complete restorable-point requirement. Long read snapshots retain
WAL pages, so bound backup/read duration and monitor WAL growth.

## Measured cost and verification

Standalone release probes, fresh targets, dependencies fetched before timing, default release
optimisation and `strip = true`; one run each, not a benchmark distribution. Both use bundled
SQLite; rusqlite includes its migration crate and backup code, SQLx includes Tokio and migration
support (its migrator is not exercised). SQLx has no query macros. These are **whole probe binary
sizes and build wall times**, not incremental orch cost: orch already shares Tokio and other crates.
Other agents were active on the host, so timings are indicative.

| Platform | rusqlite pair: build / bytes | SQLx: build / bytes |
|---|---:|---:|
| macOS arm64, Rust 1.99.1 | 12.723 s / 2,211,408 | 16.386 s / 2,885,312 |
| Linux (Docker linux/arm64, native), Rust 1.99.1 | 27.186 s / 2,380,208 | 29.591 s / 3,036,256 |

Both release builds and executables exited 0 on both platforms. `otool -L` on macOS and `ldd`
on Linux showed no dynamically linked SQLite. Linux used `rust:1.99.1-trixie`, image
`sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07`.
This is native Linux arm64 dependency evidence, not Linux CI or an x86_64 product build.

The chosen probe also checks STRICT rejects a wrong type, the partial unique index rejects a
conflict, rowid access fails, the expression query works, settings are effective, writes through
a read-only handle fail, the reader sees 10 while the writer commits 11, backup sees 10 and
passes `integrity_check`, then a new reader snapshot sees 11. Counts remain one entity row and
one FTS row; malformed inserts are rejected. No real project data was used.

[Probe sources, exact locks and reproduction commands](experiments/sqlite-library/README.md)
are separate from the product workspace. The schema, port and readers are unchanged; no migration
is run against an existing database. No human migration action is needed for this recommendation.

**What would change the recommendation:** an unfixable advisory/licence issue, loss of a supported
migration adapter or bundled-engine feature, failure on a supported target with the pinned compiler,
or step D showing unacceptable claim latency. Prefer a compatible rusqlite upgrade first; reconsider
SQLx if a maintained safe backup integration and a measured operational advantage outweigh the adapter cost.
The owner approves the final library/version and storage design at the entry gate.

Primary sources: [rusqlite 0.41.1](https://docs.rs/crate/rusqlite/0.41.1),
[rusqlite_migration 2.7.0](https://docs.rs/crate/rusqlite_migration/2.7.0),
[SQLx SQLite](https://docs.rs/sqlx/0.9.1/sqlx/sqlite/index.html),
[Refinery adapter manifest](https://docs.rs/crate/refinery-core/0.10.1/source/Cargo.toml),
[SQLite STRICT](https://sqlite.org/stricttables.html),
[SQLite online backup](https://sqlite.org/backup.html).
