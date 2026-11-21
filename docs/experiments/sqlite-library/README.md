# SQLite library check probes

These standalone packages are entry-step experiments, excluded from the product workspace.
They use only synthetic data in a directory supplied by the caller and refuse an existing probe DB.
See [the recommendation](../../sqlite-library.md) for the recorded results and their limits.
The code is original project code; crate licences are checked through cargo-deny.

The current rusqlite package uses `bundled,backup,cache,hooks,limits`, defaults off, plus
`rusqlite_migration` with defaults off. `verification.json` is the unchanged historical three-feature
record at commit `1c72fef`; its source hashes refer to that commit. The current source and results
are pinned by `security-features-verification.json`. The SQLx sources/locks are unchanged.

From the repository root, with the pinned toolchain selected and a fresh synthetic HOME/config/TMPDIR
and target supplied to the process (never unset `ORCHESTRY_GLOBAL_CONFIG_PATH`; keep `global.yml`):

```sh
cargo fetch --locked --manifest-path docs/experiments/sqlite-library/rusqlite/Cargo.toml
cargo build --release --locked --manifest-path docs/experiments/sqlite-library/rusqlite/Cargo.toml
sqlite_probe_target="${CARGO_TARGET_DIR:-docs/experiments/sqlite-library/rusqlite/target}"
sqlite_probe_data=$(mktemp -d)
"$sqlite_probe_target/release/rusqlite-check" "$sqlite_probe_data"
"$sqlite_probe_target/release/rusqlite-check" --heap-limit
cargo fetch --locked --manifest-path docs/experiments/sqlite-library/sqlx/Cargo.toml
cargo build --release --locked --manifest-path docs/experiments/sqlite-library/sqlx/Cargo.toml
sqlite_sqlx_target="${CARGO_TARGET_DIR:-docs/experiments/sqlite-library/sqlx/target}"
sqlite_sqlx_data=$(mktemp -d)
"$sqlite_sqlx_target/release/sqlx-check" "$sqlite_sqlx_data"
```

Use a new `CARGO_TARGET_DIR` for a cold build, and time only `cargo build` after fetch.
Inspect linkage with `otool -L <binary>` on macOS or `ldd <binary>` on Linux.
For the recorded Linux run, the official `rust:1.99.1-trixie` arm64 image mounted the
probe sources and used a separate target per package; cargo ran `build --release --offline --locked`
and each executable ran against a fresh synthetic directory. No system SQLite was linked.

To repeat the full-workspace dependency check without changing product dependencies, export
the recorded baseline into a disposable directory, add the recommended entries to its workspace
manifest and `rusqlite.workspace = true` / `rusqlite_migration.workspace = true` to
`crates/orch-infra/Cargo.toml`, then run `cargo deny check` there. Keep `deny.toml` unchanged.
The checked RustSec revision and baseline are recorded in the recommendation. A new audit
can produce a different result and must not be treated as the historical result.

The rusqlite migration smoke test verifies the runner's transactions and version checks.
It does not implement orch's `schema_migrations` ledger, pre-migration backup hook or schema.
The SQLx probe tests SQL capability and buildability; it does not implement raw-handle backup.

The security probe uses only original synthetic tables. It checks the authorizer's actual denials and
its invocation during automatic reprepare, progress cancellation (including `quick_check`), a
handshaken cross-thread interrupt, import/query limit readbacks and refusals, defensive/untrusted
schema settings, immutable read-only access and unchanged companion canaries. `--heap-limit` must be
a separate process: SQLite's hard heap ceiling is process-wide, and SQL can lower but not raise it.
The main invocation uses the specified 256 MiB ceiling; the separate process injects 8 MiB and
requires `SQLITE_NOMEM` for a 16 MiB allocation. Use a plain absolute scratch path with no URI query
characters. Each main invocation requires a fresh directory; it refuses existing database names.

For the renewed audit, Docker used the same read-only source and registry cache, a new target per
architecture, no network, explicit synthetic HOME/config/TMPDIR and non-root uid. The arm64 image
is `sha256:8a4e90c7946b74626488b1446f712c0fe0ed1a596cbd3d871684bc9c5433ae07`;
the amd64 image is
`sha256:9bd963cbaa28a8fe95d96d57da858372d6d82a6c0a8f35a10788d3b97459f885`.
Both run `cargo build --release --offline --locked`, the main probe in a fresh directory, a separate
`--heap-limit` process and `ldd`. Native macOS uses Rust 1.99.1, then standalone fmt/clippy and
`otool -L`. Apply an external wall deadline when reproducing; the recorded main/heap subprocesses
had a 10-second parent deadline in Docker and a 180-second parent deadline on macOS. Those
watchdogs bound the experiment; they are not the product's promised 5-second query deadline.

The historical dependency-only disposable-workspace audit ran while broad repository checks were
held. Direct-main qualification is now separately authorized; historical receipts are not receipts
for the new delivery head. Compare baseline and candidate under unchanged `deny.toml`, freeze the
advisory revision for both, and retain the resolving metadata/lock. Do not treat this audit as a workspace build/test or
Pre-PR receipt. No product dependency installation, daemon, socket or real agent CLI is involved.

The direct-main reconciliation changes no probe bytes or historical JSON. In particular, the WAL
hook/checkpoint APIs required by R126 exist through `hooks`/`wal` but are not exercised here;
T4b owns their first execution. The full five-feature contract enters the product together at T9,
not incrementally at T23, and only after the entry approval and that task's own dependency check.
