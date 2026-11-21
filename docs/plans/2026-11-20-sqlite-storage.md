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
| `orch-infra` | New `sqlite/` (schema, migrations, connections, writer, readers, adapter), `blob/`, `data_dir/`; `storage/` gains the v2 file adapter and leaves in T25. `storage/remote` stays (orch-peer's files). `data_dir/` also holds the one confined-directory module (spec R102) that the blob store, the event files, backup, restore and the importer all use to reach files. |
| `orch-engine` | Operations as units of work; leases, retries, recovery by generation; the intent performer; the scheduler behind its port; the maintenance loop; attention; usage; the facade for `orch-peer`. |
| `orch-proto` | Protocol 3: keys, revisions, paging, four error codes, twelve operations. |
| `orch-daemon` | The data directory and its lock; the authorization function in the dispatcher; the new operations; discovery by pointer file. |
| `orch-client`, `orch-cli` | Resend with a key; `orch import`, `orch db`, `orch attention`; list flags. |
| `orch-peer` | The two changes of the spec's "orch-peer" section, in T24, which the owner approved (spec AP1). |
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
- Let each of the blob store, the event files, backup and the importer build its own paths: four places to
  get link and name handling right. One confined-directory module lands in T9, before any of them, and they
  take a directory handle from it and never a path string (spec "Filesystem names", ADR 0017 decision 20).
- Let the importer find a hostile document while it loads: one read of the source, and a refusal part-way
  leaves an unfinished import. T21 runs the dry run's checks first, and T23 builds an export's database under
  a scratch name (spec "Import bounds", ADR 0017 decision 21).

The lasting choices are in ADR 0017. Limits and numbers live in the spec and in the tests it names.

## Tasks

A task that grows past about 3,000 changed lines is split in its own PR series and this table is edited in the
first of them. "Suite" is `cargo test -p orch-engine --test storage_conformance`.

### Entry steps: no product code

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Spec, plan and ADR 0017 | R1 | `docs/specs/`, `docs/plans/`, `docs/decisions/` | — | `./xtask/gate.sh fast --base origin/main` | 1 (first-review text, merged), then a follow-up PR (the corrected text) |
| T2 | Conformance suite on the file backend with its synthetic corpus; the pinned 1.0 contracts and the order-dependent reads list | R2, R3 | `crates/orch-engine/tests/storage_conformance.rs`, `crates/orch-engine/tests/storage_contract/`, `fixtures/storage-v1/`, `docs/storage-v1-conformance.md`, `docs/storage-order-dependent-reads.md` | — | Suite | 2 |
| T3 | Choose the SQLite library and its migration runner: the §4.8 check and the report, with its probes. ADR 0017 decision 19 carries the choice: `rusqlite` 0.41.1 with five features and `rusqlite_migration` 2.7.0. The report is revised for the two features the reviews added (`limits`, `hooks`) with probes of the limits, the authorizer, the progress handler, the interrupt and the heap limit; no dependency enters the product workspace before T9 | R4 | `docs/sqlite-library.md`, `docs/experiments/sqlite-library/` | — | `cargo deny check` on a whole workspace with the candidates connected; the probes on macOS and Linux | 3 |
| T4 | Measure batched event append against the first claim-latency budget, as a standalone experiment on macOS and native Linux | R5, R63 | `docs/sqlite-event-throughput.md`, `docs/experiments/sqlite-events/` | T3 | The report states the budget declared before the run, the figures and a verdict for each platform | 4 |
| T4b | The revised event measurement that the owner directed at AP13: extend the standalone harness to the method of the spec's "Event measurement" (`ap13-realistic-v1`), write `protocol.json` from that section and record its SHA-256 with the spec's commit before the first repetition, and run the seven mandatory cells five times and the diagnostics three times on macOS hardware and on native Linux hardware. Every T4 file and figure is kept byte for byte, and the new results are a new data set. The proposed numbers and the results go back to the owner together; nothing in them is accepted before the owner answers. Each repetition runs under the host lease, held exclusively, with the host sampled, and on a platform whose class is recorded (the spec's "The host" and "Platforms and limits"). With no native Linux host the result is incomplete, a miss that macOS has shown stays a failure meanwhile, and a container on the macOS machine does not stand for a host | R5, R63, R26, R126 | `docs/sqlite-event-throughput.md`, `docs/experiments/sqlite-events/` | T4, T6 (the frozen method), T3's delivery for the library features | The harness's own checks and its timing check; the verifier's verdict for each platform and for both, by the one rule of the spec's "The verdict" (four axes; `PASS`, `FAIL`, `FAIL, INCOMPLETE`, `INCOMPLETE` or `CORRECTNESS_DEFECT`, with its exit), with each repetition's checkpoint calls and their outcomes, its host record and its overlap counts listed and the claims around each checkpoint reported apart; the cases of the spec's table under "What a miss selects", given by the verifier as written; the report names the path T16 takes only as the outcome the owner then rules on | 4 |
| T4c | The number of rows in the largest goal pause and failure cascade on the live copy, counts only (AP8). The owner answered AP8 as recommended; this use of the copy still needs the owner's word, which neither that answer nor the answer D6 gives; it is the third thing the owner holds, outside the answers | R27 | records outside the repository; the count alone in `docs/sqlite-event-throughput.md` | T7 | The count and how it was taken, with no content of the copy | 4 |
| T5 | Prototype "claim with a fenced lease" as a standalone workspace, across a suspend and resume and across a restore; report its result | R6 | `prototypes/fenced-lease/`, `docs/specs/2026-11-20-fenced-lease-prototype.md`, `docs/plans/2026-11-20-fenced-lease-prototype.md`, `docs/decisions/0018-fenced-lease-prototype.md` | T3 | The report states what a suspended machine and a restore do to a lease, and whether ADR 0017 decision 16 holds (spec AP11) | 5 |
| T6 | Security review of the spec and red-team of the plan (import cutover, single writer); their findings become spec rows or plan edits | all | `docs/specs/`, `docs/plans/`, `docs/decisions/0017-sqlite-storage.md` | T1 | Each finding is answered in PR 6 as accepted, rejected or modified | 6 |
| T7 | Approval gate. The owner has answered O1 to O10, AP1 to AP12, AP14 to AP20 and D1 to D8 as recommended, and AP13 in its direction; the spec's "Owner's answers" has a row for each id. Still the owner's: the numbers and results of T4b; the fifth point of AP14; outside the answers, the word on using the live copy for T4c (AP8); and the merge of the follow-up PR that carries the corrected spec, which by the answer D8 follows the validation and the PRs of the entry steps and the reviewers' conditions, and by the answer D1 a re-review at the exact head; the merge of PR 1 on 2026-11-21 carried the first-review text and is not that approval. Then spec `approved`, plan `active`, ADR 0017 `accepted` | — | status lines; `docs/decisions/README.md` | T2 to T6, T4b | The owner's approval recorded in PR 6 | 6 |

### Product: built on files, then beside files

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T8 | Port v2 in `orch-core`, its fakes, and the v2 file adapter; the suite extended to all of port v2, with the rule that no two task ids of a project differ only in case, kept by the file adapter's own comparison | R7 to R15, R17, R97, R117 (the port and the file adapter), R132 (the caller's lineage in the port) | `crates/orch-core/src/store*`, `crates/orch-infra/src/storage/v2*`, the suite | T7 | Suite (file adapter) | 7 |
| T9 | Data directory, its lock, the 1.0 owner lock, the network-filesystem check; the confined-directory module and the project record named by hash; schema, migrations, connection settings; the unique index on `tasks` over the project and the id without regard to ASCII case | R18, R19, R21 to R23, R29 to R33, R77 (the lineage column and row), R102, R107 (the record), R117 (the index), R124 (the copy before a migration), R126 (the writer's settings, the sync settings read back on every connection that can write) | `crates/orch-infra/src/sqlite/`, `crates/orch-infra/src/data_dir/`, the workspace `Cargo.toml` and `Cargo.lock` and `crates/orch-infra/Cargo.toml` (`rusqlite` and `rusqlite_migration` exactly as ADR 0017 decision 19 names them, all five features of `rusqlite` at once, with the versions and advisories checked again; the `fs` and `dir` features of `nix`), `crates/orch-infra/tests/confined_dir.rs`, `crates/orch-daemon/tests/daemon_data_dir.rs`, `.cargo/config.toml` | T7 | `cargo test -p orch-infra --test sqlite_schema --test sqlite_migrations --test sqlite_connection --test confined_dir`; `cargo test -p orch-daemon --test daemon_data_dir --test daemon_legacy_lock` | 8 |
| T10 | SQLite adapter of port v2: the writer with lanes, the read pool, intents | R7 to R15, R24 to R28, R98, R126 (the backlog kept from the commit callback and the checkpoint's result), R127, R128, R130 (the query connection and its authorizer), R137 (the outcome of a checkpoint call), R140 (a failure on any connection) | `crates/orch-infra/src/sqlite/` | T8, T9 | Suite (SQLite); `cargo test -p orch-infra --test sqlite_writer --test sqlite_crash` | 9 |
| T11 | Blob store on disk, on the confined-directory module: a blob is reached only by a parsed hash; prompts by hash; attachments as blobs | R58, R59, R101, R102 (blobs) | `crates/orch-core/src/store/blob.rs`, `crates/orch-infra/src/blob/` | T9 | `cargo test -p orch-infra --test blob_store` | 10 |
| T12 | Engine on port v2, part 1: claims, leases, retries, start and finish a run; the workspace found by task id and the branch by its derived name, never by a stored path; workspace ownership: a directory is reused or entered, and a branch merged, only when a listing holds it under exactly the task's name, a new workspace is an exclusive create, and a task id that folds onto an existing one is drawn again; the `orch-peer` facade | R35, R36, R40, R20, R96, R106, R117 (`task.create`), R118 (prepare, reuse, merge), R129 (the spool is written, and what happens when it cannot be), R132 (an earlier lineage is refused and changes no row) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/peer_facade*`, `crates/orch-infra/src/process/`, `crates/orch-infra/src/workspace/` | T8, T10 | `cargo test -p orch-engine --test transactions --test leases --test retries --test task_statuses --test task_ids`; `cargo test -p orch-infra workspace`; `cargo test -p orch-peer` | 11 |
| T13 | Engine on port v2, part 2: cancel, pause, failure cascade; generations and recovery; the intent performer, whose intents hold ids and hashes and never a path, and whose `workspace.remove` and branch delete leave a directory or a branch that a listing holds under another spelling | R37 to R39, R41, R42, R28, R106 (intents), R118 (remove, delete, cleanup), R120, R129 (recovery applies the spool) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/intents*` | T12, T4c | `cargo test -p orch-engine --test transactions --test intents --test leases` | 12 |
| T14 | Scheduler port with today's scheduler behind it; `metrics.get` with tick time and open tasks | R55 to R57 | `crates/orch-core/src/store/scheduler.rs`, `crates/orch-engine/src/scheduler*`, `crates/orch-daemon/src/ops/` | T12 | `cargo test -p orch-engine --test scheduler --test order_conformance`; `cargo test -p orch-daemon --test ops_metrics` | 13 |
| T15 | Maintenance loop and its jobs; saved dispatch state | R43, R44, R60, R126 and R127 (the checkpoint job, its two triggers on time and on backlog, and the backlog's ceiling), R137 (which calls move the timer), R139 (the order and the bound of a stop), R140 (the reports for failing checkpoints and a backlog that stays) | `crates/orch-engine/src/maintenance*`, `crates/orch-daemon/src/lifecycle.rs` | T13 | `cargo test -p orch-engine --test maintenance`; `cargo test -p orch-daemon --test daemon_dispatch --test daemon_storage_failure` | 14 |
| T16 | Run events through the port: numbering, batches, gaps, tail reads, pruning; the path that the owner's ruling on T4b's result selects, SQLite rows or the file fallback, measured as built by the method of the spec's "Event measurement" before this PR merges; if the fallback, each event file named by the whole hash of the spec's "Filesystem names" and reached through the confined-directory module on append, read, tail read and prune | R14, R26 (one event transaction for all runs), R61 to R63, R101, R102 (event files), R139 (events handed over and not committed when a stop ends) | `crates/orch-engine/src/orchestrator/collect*`, `crates/orch-infra/src/sqlite/events*`, `crates/orch-infra/src/blob/events*`, `crates/orch-infra/tests/event_files.rs` | T10, T11, T13, T4, T4b | `cargo test -p orch-engine --test run_events --test maintenance`; `cargo test -p orch-infra --test event_files`; the record of R63 for the path that ships, on macOS and native Linux hardware | 15 |
| T17 | Protocol 3: revisions, paging, request keys, the four error codes; client resend; CLI list flags | R45 to R51, R134 | `crates/orch-proto/`, `crates/orch-daemon/src/ops/`, `crates/orch-client/`, `crates/orch-cli/` | T12 | `cargo test -p orch-proto`; `cargo test -p orch-daemon --test ops_request_keys --test ops_revisions --test ops_paging --test daemon_wire`; `cargo test -p orch-cli --test list_paging` | 16 |
| T18 | Authorization matrix in the dispatcher, with test principals | R52 to R54, R93, R133 (the operations) | `crates/orch-daemon/src/ops/`, `crates/orch-proto/src/ops/` | T8, T17 | `cargo test -p orch-daemon --test ops_authorization`; `./xtask/gate.sh full` (security-relevant) | 17 |
| T19 | Attention queue with its two kinds; the disk floor | R81 to R85, R131 | `crates/orch-engine/src/attention*`, `crates/orch-engine/src/disk_floor*`, `crates/orch-cli/` | T15, T17 | `cargo test -p orch-engine --test attention --test disk_floor`; `cargo test -p orch-cli --test attention` | 18 |
| T20 | Usage for every run, with the model and the agent CLI version | R86, R87 | `crates/orch-engine/src/orchestrator/`, `crates/orch-infra/src/adapters/` | T12 | `cargo test -p orch-engine --test usage` | 19 |
| T21 | `orch import` from a 1.0 directory: dry run, bounds, resume; the source read by listing through the confined-directory module, the id rule, and nothing done outside the database; the seeded fixture generator and the hand-written hostile-source corpus (links, special files, traversal-shaped ids and names, a live canary process, a canary child left in the process group of a leader that has gone, two task ids that differ only in case, and a workspace with a canary under another spelling of a task's id). The check, before the first write, that no two task ids of the source have one fold key and that no name listed in `workspaces/` folds onto a task's id without being it; `workspaces/` is listed for its names alone. The source-budget pass of the spec's "Import bounds": every document measured for bytes, depth, nodes, aliases, expansion and tags before anything is built from it, on 1.0's own guarded reader (`orch_core::definition_yaml`) where that already holds the limit; event lines read as a bounded stream; the check of the whole source before the first write; and the hostile-document corpus (the review's depth and alias documents, merge-key bombs, tags, each limit and one over), run in a child process under a time and a memory ceiling | R64 to R70, R90, R99, R103 to R105, R109 to R112, R116 (a 1.0 source), R117 (a 1.0 source), R119, R121 (the record), R122, R123, R125 | `crates/orch-infra/src/import/`, `crates/orch-cli/src/commands/import*`, `xtask/src/fixture_gen.rs`, `fixtures/storage/`, `crates/orch-infra/tests/source_budget.rs`, `crates/orch-cli/tests/import_hostile.rs` | T10, T11, T2 | `cargo test -p orch-cli --test import --test import_hostile`; `cargo test -p orch-infra --test source_budget`; `cargo xtask fixture-gen --check` | 20 |
| T22 | `orch db` with the daemon running: status, verify, query, prune | R72 to R74, R79, R121 (status), R130 | `crates/orch-daemon/src/ops/`, `crates/orch-cli/src/commands/db*` | T10, T16, T17 | `cargo test -p orch-cli --test db_commands` | 21 |
| T23 | Backup, restore, vacuum, export, and import from an export file; a backup is written and read through the confined-directory module, its manifest's names are checked against the layout and never opened, and an export's names, ids and paths are stored as data. Before an export's database is published and before a restore changes anything, the check of the spec's R117: no two task ids of one fold key, no directory in the project's `workspaces/` that the tasks brought may not own, and a backup's database refused when it lacks the index. The export format of the spec's "Import bounds" (blob bytes in chunks, a manifest, tables only) and its hostile reader: the header checked before SQLite opens the file, a read-only immutable connection with the schema untrusted, triggers, views and extensions off, run-time limits, a heap limit, an authorizer and a progress handler, `quick_check` first, the schema compared and never adopted, rows copied by fixed statements into a database built by the product's own migrations under a scratch name and published by a rename; and the hostile-export corpus, made by altering the bytes of an export the product wrote, run in a child process under the same ceilings. A restore replaces the state as the spec's R138 says: the whole state staged under a scratch name with its new lineage and generation and its leases ended, a synced marker, renames in a fixed order, `--continue` and `--abandon`, a refusal to start anything while the marker exists, and a check of free space that counts the backup; tested by stopping a child at every step | R75 to R80, R100, R108, R101 and R102 (backups), R105 and R107 (export), R113 to R115, R116 (an export), R67 (no resume for an export), R117 (an export, a restore), R124 (restoring the copy), R125 (an export), R132 (a restore), R133 (the confirmation), R138 | `crates/orch-infra/src/sqlite/backup*`, `crates/orch-infra/src/import/export*`, `crates/orch-cli/src/commands/db*`, `crates/orch-cli/tests/export_hostile.rs` (the `limits` and `hooks` features of `rusqlite` that this task uses are on since T9, ADR 0017 decision 19) | T21, T22 | `cargo test -p orch-cli --test db_backup_restore --test db_export_import --test export_hostile --test restore_fencing --test db_offline`; `./xtask/gate.sh full` (security-relevant) | 22 |

### Switch, removal, proof

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T24 | The switch: the daemon opens the data directory and SQLite; `orch init` and the pointer file, read without following a link and accepted only as one project id; discovery; the two `orch-peer` changes; the README upgrade notes and the RUNBOOK entries for the switch. The file adapter stays selectable by tests only | R34, R89, R92, R107 (the pointer), R121 (the check at a start), R133 (`orch init`), R135 | `crates/orch-daemon/src/lifecycle.rs`, `crates/orch-daemon/src/discovery.rs`, `crates/orch-client/src/discovery.rs`, `crates/orch-cli/src/commands/init*`, `crates/orch-peer/src/`, `README.md`, `docs/RUNBOOK.md` | T12 to T23 | `cargo test --workspace`; `./xtask/gate.sh full`; `cargo test -p orch-cli --test project_pointer` | 23 |
| T25 | Removal: the file backend and the 1.0 traits move to `orch-file-reference`; TRAITS-FROZEN §2 rewritten; the `crate-graph` rule | R16, R17 | `crates/orch-core/src/storage.rs`, `crates/orch-infra/src/storage/`, `crates/orch-file-reference/`, `docs/TRAITS-FROZEN.md`, `xtask/` | T24 | `cargo xtask crate-graph`; suite on both backends; the `git grep` of R16 | 24 |
| T26 | Documentation: manual, RUNBOOK, ARCHITECTURE, README upgrade notes, CHANGELOG, `docs/storage.md` | R88, R136 | `docs/`, `README.md`, `CHANGELOG.md` | T24 | Fact check against the code at the candidate commit; `cargo test -p xtask`; the command that installs 1.0 beside 1.1 run as written (recorded check, spec R136) | 25 |
| T27 | Proof: the importer on a copy of the live project; the peer check on a second host; the cold gate; the spec's Verification table; spec and plan `done` | R71, R89, R91, G1 to G9 | `docs/specs/`, `docs/plans/`; records outside the repository | T25, T26 | `cargo xtask import-verify --source <copy> --record <dir>`; `./xtask/gate.sh full` on macOS and native Linux | 26 |

Docs travel with their change: each product PR adds its CHANGELOG line and the RUNBOOK entry for any new
message; T26 writes what spans the stack.

### Security review and red-team findings (T6)

Each finding of T6's two reviews gets a row here when its correction lands; the requirement rows carry the
substance. Findings of the earlier independent review are the spec rows that name it as their source.

| Finding | Answer | Where |
|---|---|---|
| Security S2, blocking: an imported run id became the event file's name, and other source-supplied names could select files | Accepted, and widened to every name, id and path from outside the daemon, to the pointer file and the project record, and to process and branch facts read from a source. Modified in two places, both put to the owner as AP14: the 1.0 workspace layout `workspaces/<task id>` is kept, with the id checked and links refused; and what a restore does about the processes its backup records (R100), which the re-review's condition S2-C2 then narrowed to a refusal and a report, never a signal | Spec R101 to R108, R41, R99, "Filesystem names", "Import rules", the backup layout in "Data directory", AP14; ADR 0017 decision 20; T9, T11, T12, T13, T16, T21, T23, T24 |
| Security S1, blocking: the import bounds limited a file's bytes and nothing inside it (depth, alias and merge expansion, tags), and an export file had no validation contract at all | Accepted. Every 1.0 document is measured before anything is built from it, with 1.0's own reader's limits where it has them, so aliases and merge keys keep working; an export is read as a hostile SQLite file and published only when all of it has passed. Modified in one place, put to the owner as AP15: three rules are stricter than 1.0's reader for a document edited by hand (tags refused, depth 64, expanded bytes 4 MiB). Widened in two: the whole source is checked before the first write, and an import from an export is all or nothing | Spec R109 to R116, R65 to R67, R80, facts F12 and F13, "Import bounds" with its vectors, "Import rules", AP15; ADR 0017 decisions 19 and 21; T21, T23 |
| Security S2 re-review, finding S2-R1, blocking: with the kept layout `workspaces/<task id>`, the ids `tsk_Case` and `tsk_case` are two rows with two leases and, on a filesystem that folds case, one workspace and one branch, so one task read another's files and its cleanup would have removed them | Accepted. The layout and every id are kept, and nothing is lower-cased or rewritten. No two tasks of a project may have ids that differ only in ASCII case, by one rule on every platform, kept by the schema; an import and a restore refuse such a pair, and a workspace that the tasks they bring may not own, before they publish anything; and before every use a listing must hold the directory or the branch under exactly the task's name, so nothing is adopted or deleted through another spelling. Widened in two places: an export is refused where a directory already stands under one of its tasks' names, and the same listing rule covers branches. The compatibility cost and the alternative of hash-named workspaces go to the owner as the fourth point of AP14 | Spec R117, R118, R104, R106, R108, R65, fact F14, "Filesystem names" (workspace ownership and its vectors), "Import rules", "Import bounds", difference 7 of "Schema for 1.1", AP14; ADR 0017 decision 20; T8, T9, T12, T13, T21, T23 |
| Red team I1, blocking: a 1.0 run still at work at the import, on this machine or on a peer, was closed and its task sent out again | Accepted, modified. The import and its dry run refuse and name each local entry whose process exists and every remote entry, whatever the peer registry says. Modified: the check only asks whether the pid's process or its process group exists and nothing is signalled or stopped, because a 1.0 entry has no start time; the owner names a run that has ended. This replaces the second point of AP14 as S2's correction left it | Spec R119, R120, R65, R69, fact F15, "Import rules", AP14, O3; ADR 0017 decision 22; T13, T21 |
| Red team I2, major: nothing stops or detects a 1.0 daemon on the stale files after the import; the way back loses value with time and the 1.0 binary may be gone | Modified. (a) accepted as detection: the import keeps size and hash, a 1.1 start refuses on a change, the owner accepts by an option; no `--replace` and no modification time, which a copy changes. (b) accepted as documentation. (c), a 1.0.1 that refuses, rejected for this goal and put to the owner | Spec R121, R136, AP16; ADR decision 22; T21, T22, T24, T26 |
| Red team I3, major: an index entry and its file differing in content was unspecified | Accepted: the file wins, the index gives order and membership, and both new classes are reported | Spec R122, R65, fact F16, "Import rules"; T21 |
| Red team I4, major: hard stops with no override, repaired only in the copy kept for the way back | Modified. Accepted: every class says whether the import stops, an unreadable document stops, and the notes say to keep the 1.0 binary and an untouched copy. Rejected for 1.1: an option that skips or sets aside a file; put to the owner as AP17 | Spec R123, R136, "Import problem classes", AP17; T21, T26 |
| Red team I5, major: no backup before a migration | Accepted: the daemon copies and checks the database before every pending migration. Scheduled backups stay out of 1.1; the two are now told apart | Spec R124, R21, AP9, "`orch db` commands"; ADR decision 7; T9, T23 |
| Red team I6, minor: a stop between "finished" and the pointer write wedged 1.1 | Accepted: running the import again writes the missing pointer | Spec R125; T21, T23 |
| Red team W1, major: the 20 ms bound left out commit and checkpoint, and automatic checkpointing was unspecified | Accepted: no checkpoint on the writer, a second connection for it, a ceiling on the log with long reads interrupted, a metric for commit time, and the lanes stated as an order with measured budgets | Spec R26, R126, R127, "Writer lanes", "Maintenance jobs"; ADR decisions 9 and 23; T9, T10, T15 |
| Red team W2, major: the fallback was chosen before the cause was known | Accepted at first as a recommendation to diagnose at the same budget. The owner then decided otherwise at AP13: revise the workload and the budget and measure anew, with the fallback if the revised budget is missed. The first run's FAIL is kept, and no number of the revised budget is accepted yet | Spec R5, R63, AP13, "Writer lanes", "Event measurement"; ADR decision 12; T4b, T16 |
| Red team W3, major: no failure policy for the writer; results held in memory were lost on a restart | Accepted, modified: failure classes with retry or a failed writer, and a spool file for each finish. The failed writer is reported from memory, since the attention queue needs the writer | Spec R128, R129, R41, R51, "Writer lanes", AP18; ADR decision 23; T10, T12, T13 |
| Red team W4, minor: the lane waits did not add up with 24 runs of events | Accepted: the waiting batches of all runs share one transaction | Spec R26, "Writer lanes"; T16 |
| Red team W5, minor: AP8 cited a cascade figure that deliverable D does not hold | Accepted: the citation is removed and the count is a task | Spec AP8; T4c |
| Security S2 re-review, condition S2-C2: a pid and a start time in a backup do not show who wrote it | Accepted: a restore never signals a process because a backup records it; `--stop-runs` covers the replaced state only | Spec R100, AP14, "`orch db` commands"; ADR decision 20; T23 |
| Security S3, major: `orch db query` could read every table | Accepted: fixed views, its own connection, an authorizer that denies by default, limits | Spec R74, R130, "Diagnostic views", AP19; ADR decision 24; T10, T22 |
| Security S4, major: a backup destination outside the private directory | Accepted: the option is removed and modes are fixed. A destination outside is put to the owner as the alternative of AP19 | Spec R75, R136, AP19; ADR decision 24; T23 |
| Security S5, major: attention items had no rule for agent text or for the action offered | Accepted: no outside text in a 1.1 item, fixed remedy text, and the rule a later kind must meet | Spec R81, R131; ADR decision 25; T19 |
| Security S6, major: no reuse of a numeric epoch did not follow from the clock | Accepted, and settled by the owner's answer to AP11: lineage beside the counter on every lease-bound call, gate line 9 amended, the clock floor withdrawn | Spec R77, R132, AP11, gate line G9; ADR decision 16; T12, T23 |
| Security S7, major: `orch init` and other paths could turn `preapproval` on in agent context | Accepted, following the owner's answer to AP5: the paths are named, and none works in agent context | Spec R133, R93; ADR decision 25; T18, T23, T24 |
| Security S8, minor: request keys had no rule for credential operations and no isolation tests | Accepted | Spec R134, "Request keys"; T17 |

The two closure reviews read the answers above at the frozen head `55188964` and both passed with conditions. Their conditions, and one from the review of the lease prototype's corrections:

| Condition | Answer | Where |
|---|---|---|
| Security C1 (S6), medium: R36 let a finish of an earlier lineage change its run's row while R132 refused it | Accepted. An earlier lineage is refused and changes no row of any table; a stale epoch of the current lineage still changes its own run's row, and only the daemon's supervisor calls it. A test compares every row before and after | Spec R36, R132, R41, R129, the finish row of "Transactions and intents"; ADR decision 16; T12 |
| Security C2 (S2-R1), medium: adopting a workspace at a task's exact name assumes the backup is this project's own | Not decided here: it is the owner's choice. Both options, their costs and their tests are the fifth point of AP14, and R117's clause for a backup is provisional. The owner's answer to AP14 of 2026-11-21, "as recommended", chooses neither, because the row recommended neither | Spec AP14, R117, "Filesystem names"; ADR decision 20; T23 waits for the ruling |
| Security N1, low: three places still said a restore acts on a backup's process records | Accepted: they now say refusal and report only, and the workspace question is stated apart | Spec AP14, "Constraints", "Filesystem names"; this table; ADR decision 20 |
| Red team W2-R1 (its condition C1), major: a 12 s cell with automatic checkpointing off contains no checkpoint | Accepted, and carried into the method the owner directed: every repetition is 660 s and holds two timer checkpoints by the production rule, one cell starts with 64 MiB of backlog, and the claims around each checkpoint are reported and judged apart | Spec R63, "Event measurement", AP13; ADR decision 12; T4b |
| Red team W2-R2 (its condition C2), major: the task graph still encodes "build the fallback" | For the lead: this plan's T4b, T4c and T16 rows are what the graph must match before a product task is released. No graph change is made by this document | T4b, T4c, T16 |
| Red team condition C3: the command that installs 1.0 beside 1.1 must be shown to work | Carried as a recorded check; without it finding I4 goes back to the owner | Spec R136, AP17; T26 |
| Red team condition C4: R31's test settles F9 | Carried: if F9 is false, AP16's recommendation becomes the 1.0.1 alternative | Spec F9, AP16; T9 |
| Red team I1-R1, minor: the liveness check asked about the leader's pid alone | Accepted: the import also asks whether the recorded process group has a member, by the same test that delivers nothing | Spec R119, R105, F17, "Import rules"; ADR decision 22; T21 |
| Red team W1-R2, minor: a trigger on the length of the log's file latches | Accepted: every rule acts on the backlog, the frames not yet checkpointed, and the file's length is only shown | Spec R126, R127, "Writer lanes", "Maintenance jobs"; ADR decision 23; T10, T15 |
| Red team W3-R1, minor: a failed spool write and an idle writer were unspecified | Accepted: an unspooled finish is kept, retried and reported, and lost only if the daemon is stopped first; the 60 s counts one transaction being retried | Spec R128, R129, AP18; ADR decision 23; T10, T12 |
| Lease prototype corrections review, N1: a waiting period after a restore works only where a holder stops itself | Accepted: 1.1 has no such holder (AP12, answered), so R100's refusal while a recorded process lives is the only ground for sending a restored task out again | Spec R100, AP12; ADR decision 16; T23 |

Four independent reviews read revision `196f9087` on 2026-11-21 (security, red team, resilience, facts), and
each passed with conditions. A challenge of the method's machine-readable draft, read against the same
revision, found the method not ready. The owner then ruled on what a miss selects and allowed the
corrections below to be written in one pass; that word accepts none of them. Each row says what this
revision does with a finding, and none is closed by it.

| Finding | Answer | Where |
|---|---|---|
| Method challenge MC1, red team O1 and P1, blocking: any miss selected the fallback, a miss with no events included | Accepted, by the owner's ruling of 2026-11-21, which amends the direction's sentence. Every miss is kept; only a miss attributable to events selects the fallback; a miss without events or on one platform alone selects no event path and goes back to the owner. Modified: the red team's test (C0 of the same block must pass) and its added cell are not adopted, and no test of cause is added. Three readings where the ruling's words stop are marked as this spec's | Spec R63, "What a miss selects", AP13, record E of "Owner's answers", G7; ADR decision 12; T4b |
| Method challenge MC2, blocking: what completes a checkpoint for the timer was not said, and calls of the size rule can leave no timer checkpoint in 660 s | Accepted. A call has a recorded outcome, and any call that succeeded moves the timer. Incomplete coverage is a result of its own with a reason, never a pass, a miss or evidence about events. Modified: the coverage rule is unchanged, so a valid run of the production rule can be incomplete; changing it needs a new reviewed revision | Spec R137, "Checkpoints in every repetition"; T10, T15, T4b |
| Method challenge MC3, red team P4, blocking: no total verdict; a miss on one platform with the other unrun had two answers | Accepted. One rule, four axes, a headline by a fixed order and one exit for each; a miss with an absent platform is `FAIL, INCOMPLETE`; a recorded wrong outcome and an absent record are kept apart | Spec "The verdict" and its table of cases; T4b |
| Method challenge MC8, red team O2 and P2, resilience F4, major: "a busy machine is valid" against ADR 0013 decision 22, and no platform class | Accepted. The method follows decisions 21 and 22: the lease held exclusively, the host sampled, a rule for idle written before the run. A met budget on a host that was not idle is no pass, and a miss there stands and selects no path. Platform classes are defined. The limits and the classes are proposals | Spec "The host", "Platforms and limits", R63; ADR decision 12; T4b |
| Red team O4 and P3, minor: B24 was said to meet the timer checkpoints | Accepted. The purpose no longer says so; the overlap is counted for every checkpoint call and a zero is written as zero; nothing is arranged | Spec "Cells", "What is reported about overlap" |
| Resilience F1, major: a restore had no defined state when stopped | Accepted. Staging, a synced marker, a fixed order of renames, `--continue` and `--abandon`, a refusal to start while the marker exists, free space that counts the backup, and tests that stop at every step | Spec R138, "How a restore replaces the state", "Data directory"; ADR decision 16; T23 |
| Resilience F2, major: the bounded shutdown of O5 and D4 was in no requirement | Accepted. An order, a bound with proposed times, and queued events committed or left as a gap from a spooled record | Spec R139, "Stopping", "Data directory"; ADR decision 23; T15, T16 |
| Resilience F3, major: only the writer's failures had a class | Accepted. Corruption on any connection fails the writer; other failures are counted where they occur; failing checkpoints and a backlog that stays are reported. The counts and times are proposals | Spec R140, "When another connection fails"; ADR decision 23; T10, T15 |
| Facts F1, moderate: the first run was said to hold no checkpoint | Accepted. It ran with `wal_autocheckpoint = 1000`, inside the writer's commits, which is not this spec's rule | Spec "Checkpoints in every repetition" |
| Facts F2 and F4, minor: the summaries of what the owner holds left out T4c's use of the live copy; the lane numbers were called the owner's | Accepted | Spec "Requirements", "Owner's answers"; this plan's T4c, T7 and "Open questions"; ADR status lines |

Carried, and not answered by this revision: the method challenge MC4 to MC7 and MC9 and the harness's
part of MC8, which are the performance engineer's and stand before any measurement; red team O3 (what the
small windows and the burst demand, for the owner), O6 (a drain before the import and work that left the
process group), O7 (the task graph), O8 (the spool removed before its commit is synced) and O9 (a long
export under load); resilience F5 and F6; the security review's C2 (the fifth point of AP14) and its
conditions before publication and before the product; facts F3, which is the publisher's. No independent
review has yet read this revision. The answer D1 requires a re-review at the exact head before the spec is
approved, and every condition above stays open until that review closes it.

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | first-review text (`f5637805`) implemented in PR 1, `fast` on macOS, and merged to `main` by the owner on 2026-11-21; this corrected text, with the review corrections and the owner's answers, implemented in a follow-up PR that is pending its reviews and the owner's merge. The spec is not approved by either |
| T2 to T27 | pending |

## Test plan

- The spec's acceptance checks, by name, are the proof of each requirement. The gate lines map to them in the
  spec's "Gate lines" table.
- The conformance suite runs on the file adapter from T2 and on both adapters from T10 to the end. Engine and
  daemon tests added by this stack run on both adapters until T25.
- Crash points: T10's `sqlite_crash` stops a child process between statements and at commit; T12 and T13 inject
  a stop before and after every statement of each multi-entity operation and assert all-or-nothing on SQLite.
- The lanes' order is tested with a fake clock (T10). Their budgets are only measured, under load: by T4
  against the first budget, by T4b against the revised one, and by T16 on the path that ships. Every counted
  repetition of T4b and T16 holds checkpoints by the production rule. A green suite is not a performance
  result, and neither is a run on a container that stands in for a host.
- A real suspend and resume on an isolated macOS machine (spec R42) is a recorded check that is still due;
  the clock-seam test does not replace it.
- Processes (spec R100, R119): every test that could signal uses a fake process table that records each call,
  and asserts that a pid read from a source or a backup is never signalled.
- A restore's steps (spec R138): a child process is stopped before and after every copy, sync and rename,
  and so are `--continue` and `--abandon`. After each stop the test asserts one of three states and no
  other: the live state unchanged; the marker present and every start refused; or the restore complete.
  A continued restore is compared with an uninterrupted one, and an abandoned one with the replaced state
  byte for byte. Free space is set one byte under the need, and a copy is made to fail while staging.
- A stop (spec R139): with a second connection holding the write lock and a fake clock, the stop ends at
  its bound; each step is recorded in order; events handed over and not committed are a gap row after the
  next start, or are named by the stop when their record could not be written.
- Other connections (spec R137, R140): a result is injected at the checkpoint, read, query and backup
  connections in turn. Corruption fails the writer from each; other failures are counted and reported at
  their thresholds and do not fail it; each outcome of a checkpoint call is produced from injected counts,
  and a running total repeated by the next call credits no second copy.
- The verdict (spec "The verdict", "What a miss selects"): the verifier of T4b gives every row of the
  spec's table of cases as written, from recorded inputs made for the purpose. No repetition of the
  measurement is shortened or run again to make such an input.
- The importer is tested on generated projects of the live project's shape and on a hand-written corpus of
  damaged input (T21), then once on a copy of the live project (T27).
- Names and paths (spec R101 to R108, R117, R118): every test that plants a hostile name, link or special file
  also plants a canary outside its root and asserts that the canary's bytes and metadata are unchanged and that
  its contents appear in no row, blob or report. The vectors of the spec's "Filesystem names" are tested as
  written, by T9 for the confined directory, T11 for blobs, T16 for event files, T21 for a 1.0 source and T23
  for backups and exports.
- Two ids on one workspace (spec R117, R118): the pair `tsk_Case` and `tsk_case` and a foreign directory and
  branch under the other spelling, each with a canary, are tested as the spec's vectors say: by T8 and T9
  for the storage, T21 for a 1.0 source, T23 for an export and a backup, T12 and T13 for prepare, reuse,
  merge, cancel, failure and cleanup. R117's tests do not depend on the filesystem. R118's folding cases run
  on a filesystem that folds case where the test has one (macOS) and through an injected listing on every
  platform, and each says which it ran.
- Hostile input (spec R109 to R116): the vectors of the spec's "Import bounds" are tested as written, each
  limit at its value and at one over. T21 owns the documents of a 1.0 source and T23 the export files. Every
  entry runs in a child process and must end with its stated refusal within 30 s and 512 MiB resident, with
  the source byte-identical and the destination as the spec says for that refusal. The product's own export
  must pass under the same limits that refuse the hostile ones.
- `./xtask/gate.sh full`: T18, T23 and T24 (security-relevant and process handling), and once for the goal in T27,
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
- **Schema.** Forward-only. A newer database refuses an older binary. The copy the daemon makes before each
  migration (spec R124) is the way back; a scheduled backup is not in 1.1 (AP9).

## Docs to update

README (upgrade notes: one import per project, the data directory, rollback and what it loses, keeping the
1.0 binary), RUNBOOK (lock holder, unfinished import, a 1.0 state that changed after the import, disk floor,
backup and restore, the copy before a migration, a failed writer, never writing to `orch.db`, stale
revision, the removed file procedures), ARCHITECTURE (§1, §2.1,
§3.1 and §3.3, §4, §5.1 to §5.3, §6.1), `docs/TRAITS-FROZEN.md` §2, the manual (storage, import, `orch db`,
attention, the plain statement about `preapproval`), a new `docs/storage.md` (schema, data directory, export
format), CHANGELOG, ADR 0017, `docs/decisions/README.md`.

## Open questions

- The owner has answered all 38 ids of the spec (O1 to O10, AP1 to AP20, D1 to D8): 37 as recommended, and
  AP13 in its direction. No id is pending. The spec's "Owner's answers" gives the record for each and
  what each answer does not settle. An answer is not a proof, a review or a delivery.
- A third thing is the owner's and lies outside the answers: the word on using the live copy for T4c (AP8).
- Two things are still the owner's inside those answers. The numbers and results of AP13 (T4b). And the
  fifth point of AP14, whether a backup is trusted as this project's own state for a workspace that
  already stands: the row recommended neither option, so the answer chose none, and T23 does not build
  restore's handling of an existing workspace before it is ruled.
- By the answer D8 the spec is approved only after the validation and the PRs of the entry steps (T2 to
  T5) and the reviewers' conditions, and its PR stays a draft until then. That answer was given while PR 1
  was an open draft. The owner merged PR 1 on 2026-11-21 at its first-review text (`f5637805`), so `main`
  holds the spec, this plan and ADR 0017 as they stood before the review corrections and the answers, and
  this corrected text reaches `main` only through a follow-up PR. The record does not say that the merge
  approves the spec, changes D8 or releases a product task, and this plan reads it as none of those: the
  spec stays `draft`, this plan `proposed` and ADR 0017 proposed, D8's conditions are unmet, and D8 is read
  as standing for the follow-up PR. Whether that PR is opened as a draft, and when it merges, is the
  owner's to rule; nothing in this plan depends on which. Showing the owner the corrected text does not
  wait for the entry steps.
- The owner has answered AP11: a restore lineage is carried beside the epoch (ADR 0017 decision 16, spec R77
  and R132). T5 still has to confirm the mechanics and settles how a process is held until its pid is
  committed (R96). Nothing before T12 depends on either.
- T4 missed the first budget on macOS and has no figure from native Linux; that result is kept. The owner
  directed a revised workload and budget (spec AP13), and T4b measures it by the method the spec states. The
  numbers of that budget are proposed and not accepted: T4b's results and the numbers go to the owner
  together, with what the spec proposes for the three numbers that do not agree with them (lane 1's 50 ms,
  lane 3's 1 s, the collector's 256 events or 250 ms), and the owner's ruling chooses T16's path. By the
  owner's ruling of 2026-11-21 every miss is kept, a miss selects the fallback only when it is attributable
  to events, and a miss without events or on one platform alone selects no event path and goes back to the
  owner (the spec's "What a miss selects"). The path that ships is measured as built before T16 merges.
  T16's scope is written for either.
- This revision proposes, and nobody has yet reviewed or approved: the limits for an idle host and the
  platform classes; the times of a stop (10 s, 5 s, 20 s); the counts and times of the reports for other
  connections (three in a row, 5 minutes); and three readings under the spec's "What a miss selects".
  The coverage rule of the measurement is unchanged, and a valid run of the production rule can leave a
  repetition without it; whether the rule changes is a question for a reviewed revision of the method.
- No task yet owns a native Linux host: the owner's answer D6 allows the evidence, the spec's "Verification
  environment" lists the host as not yet named, and until one exists T4b is incomplete and R5 and R63
  cannot be met.
- The task graph does not yet match this table (red-team condition C2): it has no task for T4b or T4c, its
  events task does not wait for T4b, and its qualification task still names the fallback. The lead repairs
  it before any product task is released; this plan changes no task.
- Two proofs are carried from the closure reviews and are not yet made: the command that installs 1.0
  beside 1.1 (T26, spec R136), and the read-only lock against a 1.0 daemon on macOS and Linux (T9, spec
  F9 and R31).
- T24 and T25 could be one PR if the wiring is small. They are planned as two so that the switch is reviewed
  as behaviour and the removal as a move.
