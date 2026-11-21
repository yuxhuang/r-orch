# SQLite storage for release 1.1

status: proposed
owner: architect; the lead turns the task table into tasks; the owner approves at the gate after the entry steps
date: 2026-11-20
spec: [../specs/2026-11-20-sqlite-storage.md](../specs/2026-11-20-sqlite-storage.md)
decision: [ADR 0017](../decisions/0017-sqlite-storage.md) (proposed)

## Goal

Release 1.1 keeps all state in one SQLite database per daemon, reached by the engine only through storage port
v2, with a one-way import from 1.0 and the `orch db` commands. Every PR on the way leaves `main` green and
working, and the file backend works on `main` until the PR that removes it.

## Non-goals

Everything the spec lists as out of scope and as a non-goal. In this plan specifically: no tag and no version
bump; no change to the dashboard beyond what the new protocol forces; no product code before the owner's
approval gate (T7).

## Context

- 1.0 state is files; the facts are in the spec's "Facts this spec rests on" and ADR 0017's Context.
- [ADR 0012](../decisions/0012-daemon-owned-state.md) made the daemon the only owner of state. That is what
  makes a single writer possible, and it deliberately stopped short of transactions.
- `docs/TRAITS-FROZEN.md` §2 freezes the ten 1.0 storage traits. ADR 0017 is the amendment that replaces them.
- orch 2.0 (workers, fenced leases, landing) stands on this storage; the spec keeps the columns it needs.

## Design

**Shape of the work: change the port first, the backend second, the default last.**

```
 entry steps        port v2 on files         SQLite beside files        switch       removal
 (no product) ──▶  engine moves to v2  ──▶  backend, blobs, import, ──▶ daemon   ──▶ files leave
 T1 … T7           T8, T12 … T20            db: built, not wired         opens        the product
                   product still on files   T9 … T11, T21 … T23          SQLite T24   T25
```

1. Port v2 lands in `orch-core` beside the 1.0 traits, with a file adapter that implements all of it. The
   adapter keeps 1.0's file formats for what 1.0 has (leases and retries stay in `state.json`) and adds plain
   JSON documents for what it lacks (epochs, request keys, intents, attention). A unit of work on files buffers
   its writes and applies them at the end: it rolls back on an error, and it is not atomic across a process
   stop, exactly as 1.0 is not.
2. The engine moves to port v2 operation by operation while the product still runs on files. Each PR is a
   refactor with the suite as its net.
3. The SQLite adapter, the data directory, the blob store, the importer and `orch db` are built and tested
   beside the running product, wired to nothing a user reaches.
4. One PR switches the daemon to SQLite (T24). From then on a 1.0 project needs `orch import`, which exists and
   is tested by then. The next PR (T25) takes the file backend and the 1.0 traits out of the product.

Every engine and daemon test added by this stack runs on both adapters until T25, so the switch changes which
adapter the daemon builds and nothing else.

**Crates and files.**

| Crate | What changes |
|---|---|
| `orch-core` | New `store` module: `Storage`, `UnitOfWork`, `ReadSnapshot`, `Caller`, paging, claims, events, the blob-store and scheduler ports. The 1.0 `storage` module stays until T25. |
| `orch-infra` | New `sqlite/` (schema, migrations, connections, writer, readers, adapter), `blob/`, `data_dir/`; `storage/` gains the v2 file adapter and leaves in T25. `storage/remote` stays (orch-peer's files). |
| `orch-engine` | Operations as units of work; leases, retries, recovery by generation; the intent performer; the scheduler behind its port; the maintenance loop; attention; usage; the facade for `orch-peer`. |
| `orch-proto` | Protocol 3: keys, revisions, paging, two error codes, twelve operations. |
| `orch-daemon` | The data directory and its lock; the authorization function in the dispatcher; the new operations; discovery by pointer file. |
| `orch-client`, `orch-cli` | Resend with a key; `orch import`, `orch db`, `orch attention`; list flags. |
| `orch-peer` | The two changes of the spec's "orch-peer" section, in T24, if the owner approves them. |
| `orch-file-reference` (new in T25, dev-only) | The file backend and its v2 adapter, as the conformance reference. |
| `xtask` | `fixture-gen`, `import-verify`; the `crate-graph` rule for the reference crate. |

**Alternatives rejected.**

- Move the engine to port v2 and to SQLite in one PR: far over 3,000 lines, and no step at which `main` runs on
  a tested half.
- Switch the daemon to SQLite as soon as the adapter exists: `main` would refuse every 1.0 project until the
  importer landed.
- Give the file reference only the 1.0 subset of the port: the engine could not move before SQLite, and gate
  line 1 would compare less than the engine uses.
- Keep the 1.0 traits and add transactions to them: every multi-entity operation would still be several calls
  with nothing joining them.

The lasting choices are in ADR 0017. Limits and numbers live in the spec and in the tests it names.

## Tasks

A task that grows past about 3,000 changed lines is split in its own PR series and this table is edited in the
first of them. "Suite" is `cargo test -p orch-engine --test storage_conformance`.

### Entry steps: no product code

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Spec, plan and ADR 0017 | R1 | `docs/specs/`, `docs/plans/`, `docs/decisions/` | — | `./xtask/gate.sh fast --base origin/main` | 1 |
| T2 | Conformance suite on the file backend with its synthetic corpus; the pinned 1.0 contracts and the order-dependent reads list | R2, R3 | `crates/orch-engine/tests/storage_conformance.rs`, `crates/orch-engine/tests/storage_contract/`, `fixtures/storage-v1/`, `docs/storage-v1-conformance.md`, `docs/storage-order-dependent-reads.md` | — | Suite | 2 |
| T3 | Choose the SQLite library and its migration runner: the §4.8 check and the report, with its probes. ADR 0017 decision 19 carries the choice; no dependency enters the product workspace before T9 | R4 | `docs/sqlite-library.md`, `docs/experiments/sqlite-library/` | — | `cargo deny check` on a whole workspace with the candidates connected; the probes on macOS and Linux | 3 |
| T4 | Measure batched event append against the claim-latency budget, as a standalone experiment on macOS and native Linux | R5, R63 | `docs/sqlite-event-throughput.md`, `docs/experiments/sqlite-events/` | T3 | The report states the budget declared before the run, the figures and a verdict for each platform | 4 |
| T5 | Prototype "claim with a fenced lease" as a standalone workspace, across a suspend and resume and across a restore; report its result | R6 | `prototypes/fenced-lease/`, `docs/specs/2026-11-20-fenced-lease-prototype.md`, `docs/plans/2026-11-20-fenced-lease-prototype.md`, `docs/decisions/0018-fenced-lease-prototype.md` | T3 | The report states what a suspended machine and a restore do to a lease, and whether ADR 0017 decision 16 holds (spec AP11) | 5 |
| T6 | Security review of the spec and red-team of the plan (import cutover, single writer); their findings become spec rows or plan edits | all | `docs/specs/`, `docs/plans/`, `docs/decisions/0017-sqlite-storage.md` | T1 | Each finding is answered in PR 6 as accepted, rejected or modified | 6 |
| T7 | Approval gate: the owner rules on O1 to O10 and AP1 to AP13; spec `approved`, plan `active`, ADR 0017 `accepted` | — | status lines; `docs/decisions/README.md` | T2 to T6 | The owner's approval recorded in PR 6 | 6 |

### Product: built on files, then beside files

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T8 | Port v2 in `orch-core`, its fakes, and the v2 file adapter; the suite extended to all of port v2 | R7 to R15, R17, R97 | `crates/orch-core/src/store*`, `crates/orch-infra/src/storage/v2*`, the suite | T7 | Suite (file adapter) | 7 |
| T9 | Data directory, its lock, the 1.0 owner lock, the network-filesystem check; schema, migrations, connection settings | R18, R19, R21 to R23, R29 to R33 | `crates/orch-infra/src/sqlite/`, `crates/orch-infra/src/data_dir/`, `crates/orch-daemon/tests/daemon_data_dir.rs`, `.cargo/config.toml` | T7 | `cargo test -p orch-infra --test sqlite_schema --test sqlite_migrations --test sqlite_connection`; `cargo test -p orch-daemon --test daemon_data_dir --test daemon_legacy_lock` | 8 |
| T10 | SQLite adapter of port v2: the writer with lanes, the read pool, intents | R7 to R15, R24 to R28, R98 | `crates/orch-infra/src/sqlite/` | T8, T9 | Suite (SQLite); `cargo test -p orch-infra --test sqlite_writer --test sqlite_crash` | 9 |
| T11 | Blob store on disk; prompts by hash; attachments as blobs | R58, R59 | `crates/orch-core/src/store/blob.rs`, `crates/orch-infra/src/blob/` | T9 | `cargo test -p orch-infra --test blob_store` | 10 |
| T12 | Engine on port v2, part 1: claims, leases, retries, start and finish a run; the `orch-peer` facade | R35, R36, R40, R20, R96 | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/peer_facade*`, `crates/orch-infra/src/process/` | T8, T10 | `cargo test -p orch-engine --test transactions --test leases --test retries --test task_statuses`; `cargo test -p orch-peer` | 11 |
| T13 | Engine on port v2, part 2: cancel, pause, failure cascade; generations and recovery; the intent performer | R37 to R39, R41, R42, R28 | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/intents*` | T12 | `cargo test -p orch-engine --test transactions --test intents --test leases` | 12 |
| T14 | Scheduler port with today's scheduler behind it; `metrics.get` with tick time and open tasks | R55 to R57 | `crates/orch-core/src/store/scheduler.rs`, `crates/orch-engine/src/scheduler*`, `crates/orch-daemon/src/ops/` | T12 | `cargo test -p orch-engine --test scheduler --test order_conformance`; `cargo test -p orch-daemon --test ops_metrics` | 13 |
| T15 | Maintenance loop and its jobs; saved dispatch state | R43, R44, R60 | `crates/orch-engine/src/maintenance*`, `crates/orch-daemon/src/lifecycle.rs` | T13 | `cargo test -p orch-engine --test maintenance`; `cargo test -p orch-daemon --test daemon_dispatch` | 14 |
| T16 | Run events through the port: numbering, batches, gaps, tail reads, pruning; the file fallback if T4 requires it | R14, R61 to R63 | `crates/orch-engine/src/orchestrator/collect*`, `crates/orch-infra/src/sqlite/events*` | T10, T13, T4 | `cargo test -p orch-engine --test run_events --test maintenance` | 15 |
| T17 | Protocol 3: revisions, paging, request keys, the two error codes; client resend; CLI list flags | R45 to R51 | `crates/orch-proto/`, `crates/orch-daemon/src/ops/`, `crates/orch-client/`, `crates/orch-cli/` | T12 | `cargo test -p orch-proto`; `cargo test -p orch-daemon --test ops_request_keys --test ops_revisions --test ops_paging --test daemon_wire`; `cargo test -p orch-cli --test list_paging` | 16 |
| T18 | Authorization matrix in the dispatcher, with test principals | R52 to R54, R93 | `crates/orch-daemon/src/ops/`, `crates/orch-proto/src/ops/` | T8, T17 | `cargo test -p orch-daemon --test ops_authorization`; `./xtask/gate.sh full` (security-relevant) | 17 |
| T19 | Attention queue with its two kinds; the disk floor | R81 to R85 | `crates/orch-engine/src/attention*`, `crates/orch-engine/src/disk_floor*`, `crates/orch-cli/` | T15, T17 | `cargo test -p orch-engine --test attention --test disk_floor`; `cargo test -p orch-cli --test attention` | 18 |
| T20 | Usage for every run, with the model and the agent CLI version | R86, R87 | `crates/orch-engine/src/orchestrator/`, `crates/orch-infra/src/adapters/` | T12 | `cargo test -p orch-engine --test usage` | 19 |
| T21 | `orch import` from a 1.0 directory: dry run, bounds, resume; the seeded fixture generator | R64 to R70, R90, R99 | `crates/orch-infra/src/import/`, `crates/orch-cli/src/commands/import*`, `xtask/src/fixture_gen.rs`, `fixtures/storage/` | T10, T11, T2 | `cargo test -p orch-cli --test import`; `cargo xtask fixture-gen --check` | 20 |
| T22 | `orch db` with the daemon running: status, verify, query, prune | R72 to R74, R79 | `crates/orch-daemon/src/ops/`, `crates/orch-cli/src/commands/db*` | T10, T16, T17 | `cargo test -p orch-cli --test db_commands` | 21 |
| T23 | Backup, restore, vacuum, export, and import from an export file | R75 to R80, R100 | `crates/orch-infra/src/sqlite/backup*`, `crates/orch-cli/src/commands/db*` | T21, T22 | `cargo test -p orch-cli --test db_backup_restore --test db_export_import --test restore_fencing --test db_offline` | 22 |

### Switch, removal, proof

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T24 | The switch: the daemon opens the data directory and SQLite; `orch init` and the pointer file; discovery; the two `orch-peer` changes; the README upgrade notes and the RUNBOOK entries for the switch. The file adapter stays selectable by tests only | R34, R89, R92 | `crates/orch-daemon/src/lifecycle.rs`, `crates/orch-daemon/src/discovery.rs`, `crates/orch-client/src/discovery.rs`, `crates/orch-cli/src/commands/init*`, `crates/orch-peer/src/`, `README.md`, `docs/RUNBOOK.md` | T12 to T23 | `cargo test --workspace`; `./xtask/gate.sh full`; `cargo test -p orch-cli --test project_pointer` | 23 |
| T25 | Removal: the file backend and the 1.0 traits move to `orch-file-reference`; TRAITS-FROZEN §2 rewritten; the `crate-graph` rule | R16, R17 | `crates/orch-core/src/storage.rs`, `crates/orch-infra/src/storage/`, `crates/orch-file-reference/`, `docs/TRAITS-FROZEN.md`, `xtask/` | T24 | `cargo xtask crate-graph`; suite on both backends; the `git grep` of R16 | 24 |
| T26 | Documentation: manual, RUNBOOK, ARCHITECTURE, README upgrade notes, CHANGELOG, `docs/storage.md` | R88 | `docs/`, `README.md`, `CHANGELOG.md` | T24 | Fact check against the code at the candidate commit; `cargo test -p xtask` | 25 |
| T27 | Proof: the importer on a copy of the live project; the peer check on a second host; the cold gate; the spec's Verification table; spec and plan `done` | R71, R89, R91, G1 to G9 | `docs/specs/`, `docs/plans/`; records outside the repository | T25, T26 | `cargo xtask import-verify --source <copy> --record <dir>`; `./xtask/gate.sh full` on macOS and native Linux | 26 |

Docs travel with their change: each product PR adds its CHANGELOG line and the RUNBOOK entry for any new
message; T26 writes what spans the stack.

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | implemented in PR 1; `fast` on macOS |
| T2 to T27 | pending |

## Test plan

- The spec's acceptance checks, by name, are the proof of each requirement. The gate lines map to them in the
  spec's "Gate lines" table.
- The conformance suite runs on the file adapter from T2 and on both adapters from T10 to the end. Engine and
  daemon tests added by this stack run on both adapters until T25.
- Crash points: T10's `sqlite_crash` stops a child process between statements and at commit; T12 and T13 inject
  a stop before and after every statement of each multi-entity operation and assert all-or-nothing on SQLite.
- Lane budgets are tested with a fake clock (T10) and measured under load (T4). A green suite is not a
  performance result; the record of T4 is.
- The importer is tested on generated projects of the live project's shape and on a hand-written corpus of
  damaged input (T21), then once on a copy of the live project (T27).
- `./xtask/gate.sh full`: T18 and T24 (security-relevant and process handling), and once for the goal in T27,
  on macOS and on native Linux. Every PR: `fast` after each commit, the pre-PR check before opening, Linux CI
  green at the final head.
- No dashboard behaviour changes, so no new tmux test; the existing ones must stay green through T17 and T24.

## Rollback / compatibility

- **A user's data.** `orch import` never writes `.orchestry/`, so going back to 1.0 is: stop the 1.1 daemon,
  install the 1.0 binary, start it. Work done under 1.1 is not carried back. The 1.1 daemon holds the 1.0
  owner lock while it runs, so the two cannot run on one project at once.
- **The stack.** T8 to T23 can each be reverted alone: until T24 the product runs on files. T24 is the point
  of no easy return for a developer's project on `main`; it is reverted together with T25 or not at all.
- **Protocol.** T17 raises `LOCAL_PROTOCOL` to 3: clients and daemon must be the same build, as today; the
  stop intent still crosses the skew.
- **Peers.** The peer protocol does not change; a 1.0 peer and a 1.1 daemon are expected to interoperate, and
  T27 records the check.
- **Schema.** Forward-only. A newer database refuses an older binary. The backup taken before an upgrade is the
  way back.

## Docs to update

README (upgrade notes: one import per project, the data directory, rollback), RUNBOOK (lock holder, unfinished
import, disk floor, backup and restore, stale revision, the removed file procedures), ARCHITECTURE (§1, §2.1,
§3.1 and §3.3, §4, §5.1 to §5.3, §6.1), `docs/TRAITS-FROZEN.md` §2, the manual (storage, import, `orch db`,
attention, the plain statement about `preapproval`), a new `docs/storage.md` (schema, data directory, export
format), CHANGELOG, ADR 0017, `docs/decisions/README.md`.

## Open questions

- The spec's O1 to O10 and AP1 to AP13, each with a recommendation; the owner rules at T7.
- T5 may replace ADR 0017 decision 16 (how a generation and an epoch are formed), decides whether a restore
  lineage is carried beside the epoch (spec AP11), and settles how a process is held until its pid is committed
  (R96). Nothing before T12 depends on any of the three.
- T4 decides whether T16 ships events in SQLite or the file fallback. Its result missed the budget on macOS
  (spec AP13), so T16 is planned on the fallback unless the owner rules otherwise, and the fallback is itself
  measured against the budget before T16 merges. T16's scope is written for either.
- T24 and T25 could be one PR if the wiring is small. They are planned as two so that the switch is reviewed
  as behaviour and the removal as a move.
