# SQLite library recommendation for release 1.1

2026-11-20 · Entry step C of `goal_sMVh4SG`; input to the storage ADR, pending the owner gate.
Original audit baseline: `fb6a1e1` (after v1.0.0); direct-main preparation baseline: `d58f9b62`.
No product dependency or storage format changes here.

**Recommend `rusqlite` 0.41.1 with `bundled`, `backup`, `cache`, `hooks` and `limits`, defaults off, and
`rusqlite_migration` 2.7.0 with defaults off for migrations.** This is a proposed five-feature contract.
[Current main's ADR 0017 decision 19 at `d58f9b62`](https://github.com/yuxhuang/r-orch/blob/d58f9b62f707a5741be9c83415e6261b373da2e5/docs/decisions/0017-sqlite-storage.md)
still records only `bundled`, `backup` and `cache`. Adding `hooks` and `limits` becomes binding only
after the owner merges the corrected storage spec/plan/ADR. Entry C may be delivered independently;
this report does not amend the ADR or authorize product dependencies. The following manifest is the
proposal for that later installation in `[workspace.dependencies]` and `orch-infra`, after owner
entry approval and the installation task's dependency check; commit the resolving product lockfile then.

```toml
rusqlite = { version = "=0.41.1", default-features = false, features = ["bundled", "backup", "cache", "hooks", "limits"] }
rusqlite_migration = { version = "=2.7.0", default-features = false }
```

## Direct-main reconciliation — 2026-11-21

The library-only slice has been rebased onto fetched main `d58f9b62`. The earlier delivery holds
below describe their historical attempts. The delegated entry decision now allows this slice to
qualify independently of the MCP and root-fixture streams; it waives no check or review. Acceptance
still requires fresh native fast, every ordinary Pre-PR command including `cargo test --workspace`,
a complete qualifying native pr tier, its separate full-SHA verifier, independent code/security
reviews and final-head Linux CI. Those results must identify the actual delivery commit; the
historical successes below do not establish qualification of this rebased branch.

Under the pending corrected spec/plan/ADR, T9 would install all five features together in the
workspace and `orch-infra`, with defaults off for both crates; T23 would add no feature. These
assignments become effective only after the owner merges that correction. Product installation
remains outside this entry. Registry readback on 2026-11-21 still lists rusqlite 0.43.0 and rusqlite_migration 2.7.0 as newest, and neither pinned choice is yanked.
The older rusqlite line has no assumed maintenance guarantee.

The proposed R126/ADR decision 23 contract additionally requires WAL frame accounting through
`Connection::wal_hook` (`hooks`) and `Connection::wal_checkpoint_v2` (`wal`, enabled by `hooks`).
The library source provides both; this probe does **not** execute them. The pending correction
assigns first execution to T4b.
WAL setup, snapshot and backup assertions below do not prove that backlog accounting contract.

The probe manifests, locks, Rust source and both historical verification JSON files are byte-identical
to [the rebased five-feature audit `78f0f6e1`](https://github.com/yuxhuang/r-orch/tree/78f0f6e1febf098bfd7ded48ab5b2ee7f8344215/docs/experiments/sqlite-library);
their recorded source digests were checked again after rebase. The independent Python
reader, deny-policy comparison, negative development controls and per-platform receipts are retained
as evidence for those exact bytes, on their stated baseline and platforms. Rebase does not renew the
disposable-workspace dependency audit for the new main graph. A fresh candidate/baseline deny audit
and standalone native checks must be distinguished from the repository's own Pre-PR results.
Native Linux hardware and GitHub Linux CI remain unproven by the Docker VM/emulation results.

## Renewed security-feature audit — 2026-11-20

The five-feature standalone audit extends the original three-feature recommendation with `hooks`
and `limits` for the proposed R113/R127/R130 requirements. It supports the pending correction to
ADR 0017 decision 19; main still records three features, as disclosed above. The audit does not make
the proposal binding: that requires the owner's merge of the corrected spec/plan/ADR. Product
installation and owner entry approval remain separate. `rusqlite_migration` also has defaults off.

| Required capability | Feature / safe API | Renewed evidence |
|---|---|---|
| Pinned bundled engine, STRICT, FTS5, WAL, indexes, WITHOUT ROWID, immediate transactions | `bundled`; core API | Prior capability assertions rerun on all three platforms below; SQLite 3.54.1 |
| Stable read snapshot and online backup | `backup`; `backup::Backup` | One live row changes 10 → 11; pinned source and backup retain 10, FTS count stays 1; backup integrity is `ok` |
| Prepared-statement cache | `cache` | Enabled and resolved to `hashlink`; not a cache-performance measurement |
| Default-deny query authorization | `hooks` → `wal`; `Connection::authorizer` | Fixed diagnostic view allowed; 11 forbidden preparations refused with a denial callback observed, including direct/protected reads, protected `count(*)`, CTE, schema, function, ATTACH, pragma, DDL, update and extension call; row unchanged |
| Authorization through execution | `hooks` | Callback kept installed; schema change forces automatic reprepare during stepping and invokes it again |
| VM budget / cancellation | `hooks`; `progress_handler` | Long recursive statement and `quick_check` return `SQLITE_INTERRUPT`; subsequent query succeeds |
| Cross-thread read cancellation | Core `get_interrupt_handle` / `InterruptHandle::interrupt` | Executing VM signals readiness; another thread interrupts, joins, and the connection is reusable |
| Runtime bounds | `limits`; `set_limit` / `limit` | All 14 import-profile limits and 7 query-profile limits read back; 8 over-limit/ATTACH negatives refused |
| Hostile-source connection primitives | Core `set_db_config`, URI read-only open; SQL pragmas | Defensive on, trusted schema/triggers/views off; immutable source reads 42, `quick_check` is `ok`, mmap off/cell checking on/cache 8 MiB; DB and 3 planted companion canaries byte-identical afterward |
| Offline import heap ceiling | SQL `PRAGMA hard_heap_limit` | 256 MiB read back; separate process with injected 8 MiB ceiling refuses a 16 MiB allocation with `SQLITE_NOMEM`, then reads successfully |
| Per-migration transactions | `rusqlite_migration` | Earlier rollback, restart and newer-version assertions rerun; product history/checksum/backup adapter still due |

The standalone crate now forbids unsafe code. It uses no raw handle/FFI and enables neither
`load_extension` nor the wasm default. The source probe sets and reads back all import limits on the
actual source connection before its authorizer/first content query. Its synthetic schema and allowlist
are intentionally small: this does not implement the export-header validator, exact export-schema
comparison, row/byte/result budgets, production function/view policy, deadline clock or filesystem
confinement. No malformed-file fuzzing, hostile-file race proof, product restore or snapshot-expiry
policy is claimed. `immutable=1` requires the caller to ensure the file cannot change.

**Renewed dependency check: PASS for this feature contract.** On disposable copies of `fb6a1e1`,
`cargo deny --locked check` on the unchanged baseline and `cargo deny --locked --offline check` with
the five-feature pair connected to `orch-infra` both exit 0: advisories, bans, licences and sources ok.
The same RustSec revision `f291af9b1cc0d21dfcea1709d6b037963cef262b` and unchanged `deny.toml`
SHA-256 `70b17eabf01801a94dc17652e5e8c122bcc4809fa3526ebdcd21d04119c454f6` apply.
Both have the same 13 duplicate-version warning headers, no added warning or exception. A locked
three-feature control and five-feature candidate resolve identical registry packages and lock bytes;
only rusqlite's resolved features add `hooks`, `limits`, `wal`. This comparison is independent of the
old audit. No product manifest/lock was changed.

An independent Python sqlite3 3.54.1 reader opened copies of the live/backup databases: one entity
row each, sums 11/10 respectively, one FTS row each and `integrity_check = ok`.

Pinned-crate manifests declare MSRV 1.89 for rusqlite/migration and 1.88 for libsqlite3-sys; the
standalone builds use Rust 1.99.1, not a claim of testing on those minimum versions. Registry readback
still lists rusqlite 0.43.0 as newest and migration 2.7.0 as newest; both chosen versions are not yanked.
The bundled `sqlite3.c` SHA3-256 is
`e2b869d3003b33c942b7dd7063604fbc53553e1b66609570df39b9b7b558ee81`, matching the
[SQLite 3.54.1 release](https://sqlite.org/releaselog/3_54_1.html). That match, runtime version, feature
readback and linkage checks identify the bundle; cargo-deny is not a complete SQLite C security audit.

| Platform, Rust 1.99.1 | Cold build | Expanded probe bytes | Main / heap probe |
|---|---:|---:|---|
| macOS arm64 | 17.007 s | 2,707,664 | PASS / PASS |
| Linux (Docker linux/arm64, native ISA VM) | 27.909 s | 2,511,288 | PASS / PASS |
| Linux (Docker linux/amd64, emulated) | 54.969 s | 2,682,920 | PASS / PASS |

Every listed build, main probe, separate heap probe and linkage command exits 0; no dynamic SQLite
appears in `otool -L`/`ldd`. Native macOS standalone fmt and clippy `-D warnings` pass. These are fresh
standalone targets, default release optimization/strip, offline/locked builds; one run each under
concurrent host load. Costs are whole expanded-probe costs, not incremental product overhead or
performance evidence. Linux arm64 is native ISA in a Docker VM on this Mac; amd64 is emulated.
Neither is independent native Linux hardware or GitHub Linux CI.

[Five-feature evidence and source digests](experiments/sqlite-library/security-features-verification.json)
supplement the unchanged historical `verification.json`. Its source hashes resolve at
[rebased three-feature source `0bfe8d81`](https://github.com/yuxhuang/r-orch/tree/0bfe8d81518fbc3744c6bd246ea0327d35d74296/docs/experiments/sqlite-library).
The original SQLx comparison and three-feature costs below remain historical; SQLx was not re-audited here.

Both JSON files remain immutable historical receipts. Their embedded SHAs, including
`source_parent`, `spec_contract_head` and `historical_record.source_commit`, identify the original
local history; the rebased citations above provide the equivalent source bytes for published readers.
Their baseline and `delivery.status = "BLOCKED"` describe the original attempts, not a new-head verdict.
The cited rebased commits are ancestors of this delivery branch and become remotely reachable only
when it is published. **The eventual owner merge must use a merge commit to retain this evidence
ancestry; squash or rebase merge would discard the cited commits.** No historical success qualifies
the new delivery head.

**Historical delivery block — 2026-11-20.** After fetch, `origin/main` is still `fb6a1e1`; PR92/root/MCP fixture corrections
are not ancestors. Lead convergence `tsk_KPH2_ni` and successor `tsk_T09HVD_` explicitly withhold
root/base acceptance: retained root ordinary 694 passed / 2 failed / 2 ignored and MCP fast
7543 passed / 1 failed / 7 ignored remain red. A conditional code/security review is not acceptance.
No authorized new prerequisite permits broad checks or publication. Therefore this continuation did
not run workspace fast, ordinary Pre-PR checks or pr, push, open a PR, or claim final-head Linux CI.
The local audit can be reviewed, but entry C is not delivered. Future delivery needs actual accepted
fixtures/base, all required native checks and final-head PR/CI evidence; the owner merges.

## Historical three-feature comparison and library-first check

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

**Historical three-feature dependency check: PASS.** With cargo-deny 0.20.2, both `cargo deny check` on the baseline
and the same check on a disposable full workspace with both chosen dependencies connected to
`orch-infra` exited 0: `advisories ok, bans ok, licenses ok, sources ok`.
The repository's `deny.toml` was unchanged, including its Linux target, all-features setting,
licence allow-list and three existing owner-approved advisory exceptions. The baseline's 13
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

An async timeout does not stop a running `spawn_blocking` closure. For R127, interrupt a pooled
read only when its snapshot is older than 30 seconds and measured uncheckpointed WAL backlog
exceeds 256 MiB; WAL file length alone is not that backlog. Use a SQLite interrupt handle and
a retained job handle; wait for rollback before returning that connection to the pool. The separate
R130 diagnostic-query connection has its own 5-second progress-handler bound. Backpressure and shutdown
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

*Status at `8df7ba9a0` (2026-11-28).* The shipped ledger has two columns, `version` and `applied_at`, and a start
checks that the version numbers are exactly 1 to `user_version`; it stores and checks no identity or checksum of a
migration's SQL. The stronger ledger described above is not built, and whether to build it is open (the spec's "What
the stored format does not version (issue 485)").

Use a dedicated read-only source connection and separate destination for incremental online backup.
The probe pins a WAL read snapshot, commits a writer update, and backs up the old snapshot correctly.
Production backup also needs deadlines/retry bounds and coordination of blobs/repos; SQLite's API
alone does not prove the phase's complete restorable-point requirement. Long read snapshots retain
WAL pages, so bound backup/read duration and monitor WAL growth.

## Historical three-feature cost and repository verification

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
The chosen probe additionally built and passed on Linux (Docker linux/amd64, emulated),
Rust 1.99.1: 59.321 s / 2,517,536 bytes, with no system SQLite linkage. The repository-built
image was `sha256:9bd963cbaa28a8fe95d96d57da858372d6d82a6c0a8f35a10788d3b97459f885`.
These are dependency probes, not Linux CI or a product build with the proposed dependencies.

The chosen probe also checks STRICT rejects a wrong type, the partial unique index rejects a
conflict, rowid access fails, the expression query works, settings are effective, writes through
a read-only handle fail, the reader sees 10 while the writer commits 11, backup sees 10 and
passes `integrity_check`, then a new reader snapshot sees 11. Counts remain one entity row and
one FTS row; malformed inserts are rejected. No real project data was used.

[Probe sources, exact locks and reproduction commands](experiments/sqlite-library/README.md)
are separate from the product workspace. The schema, port and readers are unchanged; no migration
is run against an existing database. No human migration action is needed for this recommendation.

Historical repository validation was **blocked**, separately from the dependency check above. The
historical `fast --base origin/main` run tested the source now reachable as
[rebased entry `74f91f53`](https://github.com/yuxhuang/r-orch/blob/74f91f53c69a5f48114c7adc2bc7d82e691cce27/docs/sqlite-library.md),
before rebase, on the original `fb6a1e1` baseline: fmt, clippy, crate-graph, build and hygiene checks
passed, but three existing `xtask` tests failed. Each failed again in one exact rerun on that base
(0 passed / 1 failed / exit 101 per rerun):

- `gate::target::qualification::production_holder_commands_and_discriminators_on_scratch_filesystem`
- `gate::target::qualification::production_native_build_script_test_and_child_descriptor_propagation`
- `gate::target::tests::native_all_state_leaf_kinds_are_checked_without_touching_canary`

The first two fixture paths fail the canonical-root check on macOS; the third exceeds the
Unix socket path-length limit. Follow-up `tsk_M3Pl2T_` owns their test-only correction.
No gate rule, fixture-root override or test skip was used. No PR is opened while this is red;
the `pr` tier and PR publication remain blocked. The recommendation is posted to the architect.

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
