# SQLite library check probes

These standalone packages are entry-step experiments, excluded from the product workspace.
They use only synthetic data in a directory supplied by the caller and refuse an existing probe DB.
See [the recommendation](../../sqlite-library.md) for the recorded results and their limits.
The code is original project code; crate licences are checked through cargo-deny.

From the repository root, with the pinned toolchain selected:

```sh
cargo fetch --locked --manifest-path docs/experiments/sqlite-library/rusqlite/Cargo.toml
cargo build --release --locked --manifest-path docs/experiments/sqlite-library/rusqlite/Cargo.toml
sqlite_probe_data=$(mktemp -d)
docs/experiments/sqlite-library/rusqlite/target/release/rusqlite-check "$sqlite_probe_data"
cargo fetch --locked --manifest-path docs/experiments/sqlite-library/sqlx/Cargo.toml
cargo build --release --locked --manifest-path docs/experiments/sqlite-library/sqlx/Cargo.toml
sqlite_sqlx_data=$(mktemp -d)
docs/experiments/sqlite-library/sqlx/target/release/sqlx-check "$sqlite_sqlx_data"
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
