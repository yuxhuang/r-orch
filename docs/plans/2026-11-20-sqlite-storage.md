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
approval gate (T7). Since the owner's decision of 2026-11-21 (the spec's record I): no measurement of event
throughput or of latency, no performance tuning and no event fallback. Since the owner's ruling of 2026-11-23
(the spec's record K): every release from 1.1 through 2.0 is developed, tested and qualified on Linux only, so no
task of this plan waits for a macOS result, and a macOS-only gap is an issue in the [After 2.0](https://github.com/yuxhuang/r-orch/milestone/20) milestone.
The Linux sleep/wake check of spec R42 (a supervised process through a real suspend and resume of an isolated,
authorized Linux host; issue 110) is not part of what is withdrawn. It is pending, and the release that ships 1.1
and each later release says in the README upgrade notes, the CHANGELOG and the release PR that it was qualified
on Linux only (spec R88 and R136).

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
| `orch-infra` | New `sqlite/` (schema, migrations, connections, writer, readers, adapter), `blob/`, `data_dir/`; `storage/` gains the v2 file adapter and leaves in T25. `storage/remote` stays (orch-peer's files). `data_dir/` also holds the one confined-directory module (spec R102) that the blob store, the finish spool, the event files, backup, restore and the importer all use to reach files. |
| `orch-engine` | Operations as units of work; leases, retries, recovery by generation; the intent performer; the scheduler behind its port; the maintenance loop; attention; usage; the facade for `orch-peer`. |
| `orch-proto` | Protocol 3: keys, revisions, paging, four error codes, twelve operations. |
| `orch-daemon` | The data directory and its lock; the authorization function in the dispatcher; the new operations; discovery by pointer file. |
| `orch-client`, `orch-cli` | Resend with a key; `orch import`, `orch db`, `orch attention`; list flags. |
| `orch-peer` | The two changes of the spec's "orch-peer" section, in T24, which the owner approved (spec AP1). |
| `orch-file-reference` (new in T25, dev-only) | The file backend and its v2 adapter, as the conformance reference. |
| `xtask` | `fixture-gen`, `import-verify`; the `crate-graph` rule for the reference crate. T21a gives `xtask` normal edges to `orch-core`, `orch-domain` and `orch-infra` (no test-support feature), so the generator can drive the real file backend. |

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

**Changed by the owner on 2026-11-21** (the spec's record I;
[issue 98](https://github.com/yuxhuang/r-orch/issues/98)). Claim latency is not a concern of release 1.1. T4's
report is not delivered to `main`, T4b is not run, T16 stores events in SQLite by the batched design and
builds no fallback, and no task measures a path as built. The rows of T4, T4b, T4c, T7, T16 and T27 say so
where they stand, and text marked "as it stood" is kept as a record. No task tests an event file: the run-id
vectors of the spec's "Filesystem names" are tested on the finish file, by T12 and T13.

### Entry steps: no product code

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T1 | Spec, plan and ADR 0017 | R1 | `docs/specs/`, `docs/plans/`, `docs/decisions/` | — | `./xtask/gate.sh fast --base origin/main` | 1 (first-review text, merged), then a follow-up PR (the corrected text) |
| T2 | Conformance suite on the file backend with its synthetic corpus; the pinned 1.0 contracts and the order-dependent reads list | R2, R3 | `crates/orch-engine/tests/storage_conformance.rs`, `crates/orch-engine/tests/storage_contract/`, `fixtures/storage-v1/`, `docs/storage-v1-conformance.md`, `docs/storage-order-dependent-reads.md` | — | Suite | 2 |
| T3 | Choose the SQLite library and its migration runner: the §4.8 check and the report, with its probes. ADR 0017 decision 19 carries the choice: `rusqlite` 0.41.1 with five features and `rusqlite_migration` 2.7.0. The report is revised for the two features the reviews added (`limits`, `hooks`) with probes of the limits, the authorizer, the progress handler, the interrupt and the heap limit; no dependency enters the product workspace before T9 | R4 | `docs/sqlite-library.md`, `docs/experiments/sqlite-library/` | — | `cargo deny check` on a whole workspace with the candidates connected; the probes on macOS and Linux | 3 |
| T4 | **Dropped as an entry step by the owner on 2026-11-21; its PR is not opened.** The first run was made on 2026-11-20, and its figures and its FAIL on macOS are kept in [issue 98](https://github.com/yuxhuang/r-orch/issues/98), which links the report and the results at the revision that holds them. As it stood: Measure batched event append against the first claim-latency budget, as a standalone experiment on macOS and native Linux | R5, R63 | `docs/sqlite-event-throughput.md`, `docs/experiments/sqlite-events/` | T3 | The report states the budget declared before the run, the figures and a verdict for each platform | — (was 4) |
| T4b | **Deferred by the owner on 2026-11-21: not run in 1.1.** No data set is authorised, and no harness, protocol file or verifier is built for it. As it stood, kept as a record: The revised event measurement that the owner directed at AP13: extend the standalone harness to the method of the spec's "Event measurement" (`ap13-realistic-v1`), write `protocol.json` from that section and record its SHA-256 with the spec's commit before the first repetition, and run the seven mandatory cells five times and the diagnostics three times on macOS hardware and on native Linux hardware. Every T4 file and figure is kept byte for byte, and the new results are a new data set. The proposed numbers and the results go back to the owner together; nothing in them is accepted before the owner answers. Each repetition runs under the host lease, held exclusively, with the host sampled, and on a platform whose class is recorded (the spec's "The host" and "Platforms and limits"). With no native Linux host the result is incomplete, a miss that macOS has shown stays a failure meanwhile, and a container on the macOS machine does not stand for a host | R5, R63, R26, R126 | `docs/sqlite-event-throughput.md`, `docs/experiments/sqlite-events/` | T4, T6 (the frozen method), T3's delivery for the library features | The harness's own checks and its timing check; the verifier's verdict for each platform and for both, by the one rule of the spec's "The verdict" (four axes; `PASS`, `FAIL`, `FAIL, INCOMPLETE`, `INCOMPLETE` or `CORRECTNESS_DEFECT`, with its exit), with each repetition's checkpoint calls and their outcomes, its host record and its overlap counts listed and the claims around each checkpoint reported apart; the cases of the spec's table under "What a miss selects", given by the verifier as written; the report names the path T16 takes only as the outcome the owner then rules on | — (was 4) |
| T4c | The number of rows in the largest goal pause and failure cascade on the live copy, counts only (AP8). The owner answered AP8 as recommended; this use of the copy still needs the owner's word, which neither that answer nor the answer D6 gives; it is the third thing the owner holds, outside the answers. Since 2026-11-21 the report that was to hold the count is not delivered (T4); where the count is recorded is not settled by this revision and is settled before T4c runs | R27 | records outside the repository; where the count alone is written is not settled (was `docs/sqlite-event-throughput.md`, which is not delivered) | T7 | The count and how it was taken, with no content of the copy | not settled (was 4) |
| T5 | Prototype "claim with a fenced lease" as a standalone workspace, across a suspend and resume and across a restore; report its result | R6 | `prototypes/fenced-lease/`, `docs/specs/2026-11-20-fenced-lease-prototype.md`, `docs/plans/2026-11-20-fenced-lease-prototype.md`, `docs/decisions/0018-fenced-lease-prototype.md` | T3 | The report states what a suspended machine and a restore do to a lease, and whether ADR 0017 decision 16 holds (spec AP11) | 5 |
| T6 | Security review of the spec and red-team of the plan (import cutover, single writer); their findings become spec rows or plan edits | all | `docs/specs/`, `docs/plans/`, `docs/decisions/0017-sqlite-storage.md` | T1 | Each finding is answered in PR 6 as accepted, rejected or modified | 6 |
| T7 | Approval gate. The owner has answered O1 to O10, AP1 to AP12, AP14 to AP20 and D1 to D8 as recommended, and AP13 in its direction; the spec's "Owner's answers" has a row for each id. Still the owner's: the numbers and results of T4b; the fifth point of AP14; outside the answers, the word on using the live copy for T4c (AP8); and the merge of the follow-up PR that carries the corrected spec, which by the answer D8 follows the validation and the PRs of the entry steps and the reviewers' conditions, and by the answer D1 a re-review at the exact head; the merge of PR 1 on 2026-11-21 carried the first-review text and is not that approval. Then spec `approved`, plan `active`, ADR 0017 `accepted`. Since 2026-11-21 (the spec's record I): T4 and T4b are not inputs of this gate, the numbers and results of T4b are deferred and nothing waits for them, and the gate presents the spec, the library choice, the prototype's result and the open points | — | status lines; `docs/decisions/README.md` | T2, T3, T5, T6 (until 2026-11-21 also T4 and T4b) | The owner's approval recorded in PR 6 | 6 |

### Product: built on files, then beside files

T8 is delivered in small additive slices: T8a supplies the entity port; T8b
supplies remaining entity families and the file reference; T8c first supplies
event/lease contracts and their memory reference, followed by retries, intents,
request keys and attention. The file reference carries the event and lease
families too (registered beside the memory reference); SQLite wiring follows
T9/T10. The T8c event and lease slice does not complete T8 or release
the engine's dependency on the full operational port. The T8c intents slice
(typed `process.start`, `process.stop`, `workspace.remove` and `blob.delete`
payloads, project-local ids, bounded pending reads, idempotent completion and
7-day retention, with the same memory and file references) follows the same
rule. The T8c request-keys slice (a whole-scope key found by identity, project,
run, epoch and key, `New`/`Replay`/`ReplayWithoutResult`/`Mismatch`, one-hour
UTC-millisecond expiry, results over 64 KiB not kept, bounded expiry deletion,
with the same two references) is stacked on the intents slice. The attention
slice is stacked on the request-key slice: fixed typed facts, unique open rows,
revisioned acknowledgements/resolutions, bounded paging and resolved retention
share both references and their transactions. Empty daemon scope and nonempty
project scope remain isolated; consumers read daemon health explicitly in a second
snapshot. Its shared suite is registered on both references; file tests also cover
legacy/reopen and scope-invalid loads. SQLite, evaluation/maintenance, authorization
and CLI remain later tasks. Retry enumeration and final combined acceptance remain
separate slices. The v1 runtime stays active.

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T8 | Port v2 in `orch-core`, its fakes, and the v2 file adapter; the suite extended to all of port v2, with the rule that no two task ids of a project differ only in case, kept by the file adapter's own comparison | R7 to R15, R17, R97, R117 (the port and the file adapter), R132 (the caller's lineage in the port) | `crates/orch-core/src/store*`, `crates/orch-infra/src/storage/v2*`, the suite | T7 | Suite (file adapter) | 7 |
| T9 | Data directory, its lock, the 1.0 owner lock, the network-filesystem check; the confined-directory module and the project record named by hash; schema, migrations, connection settings; the unique index on `tasks` over the project and the id without regard to ASCII case | R18, R19, R21 to R23, R29 to R33, R77 (the lineage column and row), R102, R107 (the record), R117 (the index), R124 (the copy before a migration), R126 (the writer's settings, the sync settings read back on every connection that can write) | `crates/orch-infra/src/sqlite/`, `crates/orch-infra/src/data_dir/`, the workspace `Cargo.toml` and `Cargo.lock` and `crates/orch-infra/Cargo.toml` (`rusqlite` and `rusqlite_migration` exactly as ADR 0017 decision 19 names them, all five features of `rusqlite` at once, with the versions and advisories checked again; the `fs` and `dir` features of `nix`), `crates/orch-infra/tests/confined_dir.rs`, `crates/orch-daemon/tests/daemon_data_dir.rs`, `.cargo/config.toml` | T7 | `cargo test -p orch-infra --test sqlite_schema --test sqlite_migrations --test sqlite_connection --test confined_dir`; `cargo test -p orch-daemon --test daemon_data_dir --test daemon_legacy_lock` | 8 |
| T10 | SQLite adapter of port v2: the writer with lanes, the read pool, intents | R7 to R15, R24 to R28, R98, R126 (the backlog kept from the commit callback and the checkpoint's result), R127, R128, R130 (the query connection and its authorizer), R137 (the outcome of a checkpoint call), R140 (a failure on any connection), R97 (SQLite row hash and blob bytes) | `crates/orch-infra/src/sqlite/` | T8, T9 | Suite (SQLite); `cargo test -p orch-infra --test sqlite_writer --test sqlite_crash` | 9 |
| T11 | Blob store on disk, on the confined-directory module: a blob is reached only by a parsed hash; prompts by hash; attachments as blobs | R58, R59, R101, R102 (blobs) | `crates/orch-core/src/store/blob.rs`, `crates/orch-infra/src/blob/` | T9 | `cargo test -p orch-infra --test blob_store` | 10 |
| T12 | Engine on port v2, part 1: claims, leases, retries, start and finish a run; the workspace found by task id and the branch by its derived name, never by a stored path; workspace ownership: a directory is reused or entered, and a branch merged, only when a listing holds it under exactly the task's name, a new workspace is an exclusive create, and a task id that folds onto an existing one is drawn again; the `orch-peer` facade | R35, R36, R40, R20, R96, R106, R117 (`task.create`), R118 (prepare, reuse, merge), R101 and R129 (the spool is written under the finish file's name and removed after the commit, for any run id: the finish-file vectors of the spec's "Filesystem names", as a name and through both operations, with an outside canary; and what happens when the spool cannot be written), R132 (an earlier lineage is refused and changes no row) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/peer_facade*`, `crates/orch-infra/src/process/`, `crates/orch-infra/src/workspace/`, `crates/orch-infra/src/data_dir/spool*` (the finish spool, on the confined-directory module of T9), `crates/orch-infra/tests/fs_names.rs` | T8, T10 | `cargo test -p orch-engine --test transactions --test leases --test retries --test task_statuses --test task_ids`; `cargo test -p orch-infra workspace`; `cargo test -p orch-infra --test fs_names`; `cargo test -p orch-peer` | 11 |
| T13 | Engine on port v2, part 2: cancel, pause, failure cascade; generations and recovery; the intent performer, whose intents hold ids and hashes and never a path, and whose `workspace.remove` and branch delete leave a directory or a branch that a listing holds under another spelling | R37 to R39, R41, R42, R28, R106 (intents), R118 (remove, delete, cleanup), R120, R101 and R129 (recovery applies the spool and removes it, reaching a file only under the finish file's name, for any run id: the same vectors and canary) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/intents*`, `crates/orch-infra/src/data_dir/spool*` (the read and the removal at recovery) | T12, T4c | `cargo test -p orch-engine --test transactions --test intents --test leases` | 12 |
| T14 | Scheduler port with today's scheduler behind it; `metrics.get` with tick time and open tasks | R55 to R57 | `crates/orch-core/src/store/scheduler.rs`, `crates/orch-engine/src/scheduler*`, `crates/orch-daemon/src/ops/` | T12 | `cargo test -p orch-engine --test scheduler --test order_conformance`; `cargo test -p orch-daemon --test ops_metrics` | 13 |
| T15 | Maintenance loop and its jobs; saved dispatch state | R43, R44, R60, R126 and R127 (the checkpoint job, its two triggers on time and on backlog, and the backlog's ceiling), R137 (which calls move the timer), R139 (the order and the bound of a stop), R140 (the reports for failing checkpoints and a backlog that stays) | `crates/orch-engine/src/maintenance*`, `crates/orch-daemon/src/lifecycle.rs` | T13 | `cargo test -p orch-engine --test maintenance`; `cargo test -p orch-daemon --test daemon_dispatch --test daemon_storage_failure` | 14 |
| T16 | Run events through the port: numbering, batches, gaps, tail reads, pruning, stored in SQLite as rows, by the batched design of the spec's "Writer lanes" (spec R63). No event fallback, no event file and no setting for another event path is built, and nothing is measured as a condition of this PR (the owner's decision of 2026-11-21, the spec's record I). No test of an event file is due in 1.1, the test of its name included: the run-id vectors of the spec's "Filesystem names" are proved on the finish file by T12 and T13 (spec R101, R129). As it stood until 2026-11-21: Run events through the port: numbering, batches, gaps, tail reads, pruning; the path that the owner's ruling on T4b's result selects, SQLite rows or the file fallback, measured as built by the method of the spec's "Event measurement" before this PR merges; if the fallback, each event file named by the whole hash of the spec's "Filesystem names" and reached through the confined-directory module on append, read, tail read and prune | R14, R26 (one event transaction for all runs), R61 to R63, R139 (events handed over and not committed when a stop ends) | `crates/orch-engine/src/orchestrator/collect*`, `crates/orch-infra/src/sqlite/events*` | T10, T11, T13 | `cargo test -p orch-engine --test run_events --test maintenance` | 15 |
| T17 | Protocol 3: revisions, paging, request keys, the four error codes; client resend; CLI list flags | R45 to R51, R134 | `crates/orch-proto/`, `crates/orch-daemon/src/ops/`, `crates/orch-client/`, `crates/orch-cli/` | T12 | `cargo test -p orch-proto`; `cargo test -p orch-daemon --test ops_request_keys --test ops_revisions --test ops_paging --test daemon_wire`; `cargo test -p orch-cli --test list_paging` | 16 |
| T18 | Authorization matrix in the dispatcher, with test principals | R52 to R54, R93, R133 (the operations) | `crates/orch-daemon/src/ops/`, `crates/orch-proto/src/ops/` | T8, T17 | `cargo test -p orch-daemon --test ops_authorization`; `./xtask/gate.sh full` (security-relevant) | 17 |
| T19 | Attention queue with its two kinds; the disk floor | R81 to R85, R131 | `crates/orch-engine/src/attention*`, `crates/orch-engine/src/disk_floor*`, `crates/orch-cli/` | T15, T17 | `cargo test -p orch-engine --test attention --test disk_floor`; `cargo test -p orch-cli --test attention` | 18 |
| T20 | Usage for every run, with the model and the agent CLI version | R86, R87 | `crates/orch-engine/src/orchestrator/`, `crates/orch-infra/src/adapters/` | T12 | `cargo test -p orch-engine --test usage` | 19 |
| T21a | The seeded fixture generator split out of T21 as a preparatory slice: `cargo xtask fixture-gen` scripts the real 1.0 file backend in a private temporary directory from a fixed seed, with a deterministic clock and ids; `--check` compares exact file sets and bytes with the checked-in `fixtures/storage/generated/` and changes nothing. One small scenario is checked in; large corpora for R66 and the importer's results stay in T21 | R90 | `xtask/src/fixture_gen*`, `xtask/src/main.rs`, `xtask/Cargo.toml`, `xtask/src/crate_graph.rs`, `xtask/tests/fixture_gen.rs`, `fixtures/storage/generated/` | T2 | `cargo xtask fixture-gen --check`; `cargo test -p xtask --test fixture_gen` | 20 (first slice) |
| T21 | `orch import` from a 1.0 directory: dry run, bounds, resume; the source read by listing through the confined-directory module, the id rule, and nothing done outside the database; the seeded fixture generator's further scenarios (T21a owns `xtask/src/fixture_gen.rs` and its first scenario; T21 extends it) and the hand-written hostile-source corpus (links, special files, traversal-shaped ids and names, a live canary process, a canary child left in the process group of a leader that has gone, two task ids that differ only in case, and a workspace with a canary under another spelling of a task's id). The check, before the first write, that no two task ids of the source have one fold key and that no name listed in `workspaces/` folds onto a task's id without being it; `workspaces/` is listed for its names alone. The source-budget pass of the spec's "Import bounds": every document measured for bytes, depth, nodes, aliases, expansion and tags before anything is built from it, on 1.0's own guarded reader (`orch_core::definition_yaml`) where that already holds the limit; event lines read as a bounded stream; the check of the whole source before the first write; and the hostile-document corpus (the review's depth and alias documents, merge-key bombs, tags, each limit and one over), run in a child process under a time and a memory ceiling. The six cases of `config.yml` and the classes of an `_index.json` that the spec's "Import rules" tabulate, with the statements of what 1.0 did with a refused file and the vectors C-V1 to C-V14 and X-V1 to X-V14 as tests; neither path gets a caller before the owner has merged the contract text that carries them ("T21 config and index contract") | R64 to R70, R90, R99, R103 to R105, R109 to R112, R116 (a 1.0 source), R117 (a 1.0 source), R119, R121 (the record), R122, R123, R125, R97 (imported text over 64 KiB) | `crates/orch-infra/src/import/`, `crates/orch-cli/src/commands/import*`, `xtask/src/fixture_gen.rs`, `fixtures/storage/`, `crates/orch-infra/tests/source_budget.rs`, `crates/orch-cli/tests/import_hostile.rs` | T10, T11, T2 | `cargo test -p orch-cli --test import --test import_hostile`; `cargo test -p orch-infra --test source_budget`; `cargo xtask fixture-gen --check` | 20 |
| T22 | `orch db` with the daemon running: status, verify, query, prune | R72 to R74, R79, R121 (status), R130 | `crates/orch-daemon/src/ops/`, `crates/orch-cli/src/commands/db*` | T10, T16, T17 | `cargo test -p orch-cli --test db_commands` | 21 |
| T23 | Backup, restore, vacuum, export, and import from an export file; a backup is written and read through the confined-directory module, its manifest's names are checked against the layout and never opened, and an export's names, ids and paths are stored as data. Before an export's database is published and before a restore changes anything, the check of the spec's R117: no two task ids of one fold key, no directory in the project's `workspaces/` that the tasks brought may not own, and a backup's database refused when it lacks the index. The export format of the spec's "Import bounds" (blob bytes in chunks, a manifest, tables only) and its hostile reader: the header checked before SQLite opens the file, a read-only immutable connection with the schema untrusted, triggers, views and extensions off, run-time limits, a heap limit, an authorizer and a progress handler, `quick_check` first, the schema compared and never adopted, rows copied by fixed statements into a database built by the product's own migrations under a scratch name and published by a rename; and the hostile-export corpus, made by altering the bytes of an export the product wrote, run in a child process under the same ceilings. A restore replaces the state as the spec's R138 says: the whole state staged under a scratch name with its new lineage and generation and its leases ended, a synced marker, renames in a fixed order, `--continue` and `--abandon`, a refusal to start anything while the marker exists, and a check of free space that counts the backup; tested by stopping a child at every step | R75 to R80, R100, R108, R101 and R102 (backups), R105 and R107 (export), R113 to R115, R116 (an export), R67 (no resume for an export), R117 (an export, a restore), R124 (restoring the copy), R125 (an export), R132 (a restore), R133 (the confirmation), R138 | `crates/orch-infra/src/sqlite/backup*`, `crates/orch-infra/src/import/export*`, `crates/orch-cli/src/commands/db*`, `crates/orch-cli/tests/export_hostile.rs` (the `limits` and `hooks` features of `rusqlite` that this task uses are on since T9, ADR 0017 decision 19) | T21, T22 | `cargo test -p orch-cli --test db_backup_restore --test db_export_import --test export_hostile --test restore_fencing --test db_offline`; `./xtask/gate.sh full` (security-relevant) | 22 |

T8 is split to keep each PR below about 3,000 changed lines. T8a introduces the
additive entity contract, the in-memory fake and backend-neutral entity scenarios
(R7–R11, R15, and the caller fields of R132); it leaves v1 traits and runtime
wiring intact. T8b depends on T8a and adds the file reference plus blob,
configuration and template coverage, including the file adapter's own R117
comparison. T8c depends on those foundations and supplies the operational port
and full conformance coverage (leases, event batches, retries, intents, request
keys and attention). Prototype-dependent lease mechanics consume accepted E
there. Full-port consumers T10 and T12 remain dependent on completed T8, not on
T8a alone. This split implements the supervising session's 2026-11-21 direction
to start independent T8/T9 work from merged PR 95; it does not approve the
remaining entry evidence or reserved owner decisions.

T8b adds two families to the port, project settings (`project_config`: key,
value, revision) and templates (`templates`: name, body, revision), listed by
their exact key in byte order like context entries, and the shared scenarios for
them, for the 64 KiB text threshold of R97 and for the task-id fold rule of R117.
The shared `text_over_64_kib_round_trips_through_the_blob_store` scenario checks
that text on both sides of the line comes back whole and replaceable. The file
reference also checks its persisted hash-backed rows against the confined disk
blob store in `storage::v2::tests`, including the UTF-8 byte boundary, reopening,
project separation and rollback. Inline rows retain their existing encoding;
external rows need the new reference reader. This reference-only encoding does
not migrate production files. R97's SQLite blob integration remains with its
adapter; the shared round trip alone does not prove that backing.

### T8 reference verification crosswalk (issue 130)

Run each bare filter in R8–R15 with
`cargo test -p orch-engine --test storage_conformance -- <filter>`.
First list it with the same command plus `--list`: require the named scenario
under every expected registration below, then run it and require that many
passing tests, zero failed and zero ignored. Cargo exits successfully for zero
matches; that is a failed verification. `--exact` requires the full listed test
name, for example `file_leases_v2::one_of_many_concurrent_claims_wins`, and must
execute exactly one test. Do not prefix a filter with `storage_conformance::`.

| Requirements | Bare filters | Expected registrations per filter |
|---|---|---|
| R8, R11 | `revisions_and_positions_for_every_entity_family`, `stale_revision_is_refused_and_changes_nothing`, `deletion_never_reuses_a_position` | 3: `memory_v2`, `queued_memory_v2`, `file_v2` |
| R9 | `page_bounds_defaults_and_optional_count` | Same 3 entity references |
| R10 | `cursor_walk_is_stable_under_inserts_and_deletes`, `cursor_is_bound_to_project_filter_and_sort` | Same 3 entity references |
| R12 | `one_of_many_concurrent_claims_wins`, `an_agent_holds_at_most_one_active_lease` | 2: `memory_leases_v2`, `file_leases_v2` |
| R13 renew/release | `epochs_only_grow`, `a_stale_epoch_is_refused`, `release_ends_the_lease_and_keeps_the_epoch_counter` | Same 2 lease references |
| R13 append, R14 | `stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing`, `duplicate_counts_first_bytes_and_range_tail_order`, `transaction_error_rolls_back_append_gap_prune_and_entities` | 2: `memory_events_v2`, `file_events_v2` |
| R15 | `caller_selects_the_project_and_keeps_the_principal`, `settings_and_templates_belong_to_their_project` | Same 3 entity references |
| R97 whole text | `text_over_64_kib_round_trips_through_the_blob_store` | Same 3 entity references; this alone cannot observe blob backing |

PR142's file-specific R97 proof is
`cargo test -p orch-infra --lib -- storage::v2::tests::text_over_64_kib_round_trips_through_the_blob_store --exact`
and the same command with the full filter
`storage::v2::tests::inline_threshold_counts_utf8_bytes_and_preserves_empty_values`.
Each exact selection must execute one passing test.
It asserts a null persisted text slot, the SHA-256 in its reference, actual disk
blob bytes, and whole values after reopening by get and list. The threshold is
65,536 UTF-8 bytes, not characters. The reference follow-up tests pin empty
references, absent/invalid pointers, non-null targets, correctly hashed non-UTF-8
bytes, invalid hydrated records, RFC6901 key escaping and array indexes above
zero. Corrupt JSON must retain `Error::Serde`, the document path and its cause;
semantic row corruption must retain the named diagnostic. The untagged inline
record decoder currently reports a variant mismatch rather than field detail;
this test records that limitation without changing the reference format.

These are reference checks on Linux, not acceptance of the SQLite backend.
T10 updates this table's expected registrations when it registers SQLite.
T10 must register the shared entity, event and lease suites on SQLite and assert
actual SQLite row hashes plus disk bytes for R97. Separate SQLite unit scenarios
are useful but do not replace those registrations. T12 owns stale-epoch finish
and runtime restart/renewal proof; T16 consumes SQLite event conformance. T21 owns
R97 import backing (the proposed import test does not exist yet). T27 records
those actual named checks and rejects empty or ignored selections. T25 retains
the file reference as a dev-only dependency. R7/R117/R132-caller name alignment
and the frozen-trait and sort decisions remain with issue 109; none is changed by this name reconciliation. Full T8
acceptance still requires the accepted operational integration and issue 109's
consumer dispositions. macOS is tracked in After 2.0 under the owner's Linux-only
direction through 2.0.

### Switch, removal, proof

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T24 | The switch: the daemon opens the data directory and SQLite; `orch init` and the pointer file, read without following a link and accepted only as one project id; discovery; the two `orch-peer` changes; the README upgrade notes and the RUNBOOK entries for the switch. The file adapter stays selectable by tests only | R34, R89, R92, R107 (the pointer), R121 (the check at a start), R133 (`orch init`), R135 | `crates/orch-daemon/src/lifecycle.rs`, `crates/orch-daemon/src/discovery.rs`, `crates/orch-client/src/discovery.rs`, `crates/orch-cli/src/commands/init*`, `crates/orch-peer/src/`, `README.md`, `docs/RUNBOOK.md` | T12 to T23 | `cargo test --workspace`; `./xtask/gate.sh full`; `cargo test -p orch-cli --test project_pointer` | 23 |
| T25 | Removal: the file backend and the 1.0 traits move to `orch-file-reference`; TRAITS-FROZEN §2 rewritten; the `crate-graph` rule | R16, R17 | `crates/orch-core/src/storage.rs`, `crates/orch-infra/src/storage/`, `crates/orch-file-reference/`, `docs/TRAITS-FROZEN.md`, `xtask/` | T24 | `cargo xtask crate-graph`; suite on both backends; the `git grep` of R16 | 24 |
| T26 | Documentation: manual, RUNBOOK, ARCHITECTURE, README upgrade notes, CHANGELOG, `docs/storage.md` | R88, R136 | `docs/`, `README.md`, `CHANGELOG.md` | T24 | Fact check against the code at the candidate commit; `cargo test -p xtask`; the command that installs 1.0 beside 1.1 run as written (recorded check on a Linux host, spec R136); the README upgrade notes and the CHANGELOG state that the release was qualified on Linux only and that macOS is untested until after 2.0 (spec R88, R136) | 25 |
| T27 | Proof: the importer on a copy of the live project; the peer check on a second Linux host; the cold gate; the spec's Verification table; spec and plan `done` | R71, R89, R91, G1 to G6, G8 and G9 (G7 was retired on 2026-11-21) | `docs/specs/`, `docs/plans/`; records outside the repository | T25, T26 | `cargo xtask import-verify --source <copy> --record <dir>`; `./xtask/gate.sh full` on native Linux (the macOS run is withdrawn: the spec's record K); the Linux sleep/wake check of spec R42 is issue 110's and is not recorded | 26 |

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
revision does with a finding, and none is closed by it. The ids in the tables of this section are each
review's own. An O with a number that is named with the red team is an item of its review of `196f9087`
and is not the spec's open point of the same name.

| Finding | Answer | Where |
|---|---|---|
| Method challenge MC1, red team O1 and P1, blocking: any miss selected the fallback, a miss with no events included | Accepted, by the owner's ruling of 2026-11-21, which amends the direction's sentence. Every miss is kept; only a miss attributable to events selects the fallback; a miss without events or on one platform alone selects no event path and goes back to the owner. Modified: the red team's test (C0 of the same block must pass) and its added cell are not adopted, and no test of cause is added. Three readings where the ruling's words stop are marked as this spec's, and so is the step that comes from "The host" (see the next table) | Spec R63, "What a miss selects", AP13, record E of "Owner's answers", G7; ADR decision 12; T4b |
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
part of MC8, which are the performance engineer's and stand before any measurement; of the red team's
review of `196f9087`, its items O3 (what the small windows and the burst demand, for the owner), O6 (a
drain before the import and work that left the process group), O7 (the task graph), O8 (the spool removed
before its commit is synced) and O9 (a long export under load), none of which is the spec's open point of
the same name; resilience F5 and F6; the security review's C2 (the fifth point of AP14) and its
conditions before publication and before the product; facts F3, which is the publisher's.

Five independent reviews then read revision `a914e78` on 2026-11-21 (method, resilience, security, facts,
red team). The method review blocked it and the other four passed it with conditions. The table below is
this revision's answer to each condition. None is closed by it. When it was written no independent review had
read that revision; five have read it since (below). The answer D1 requires a re-review at the exact head before the spec is approved, and every
condition above and below stays open until that review closes it.

| Finding at `a914e78` | Answer | Where |
|---|---|---|
| Method MC3 and red team P4, blocking: the ordered steps of "What a miss selects" had no guard for a defect. A diagnostic that stores an event twice, beside valid C24 misses on both platforms in a complete data set, reached the fallback at step 5 although the verdict says that a defect is fixed before any performance result is used | Accepted. A data set must be eligible before any step is read: correctness `ok` everywhere, no required repetition invalid or without its backend, settings, timing check or platform class, and coverage complete. Otherwise no path, with the verdict's headline and exit unchanged and every miss and axis kept. Three mixed cases are in the table of cases. No defect is renamed incomplete and no miss is left out | Spec R63, "The verdict", "What a miss selects"; ADR decision 12; T4b |
| Method MC1 and red team R1, major: "one platform alone" is judged by cell, so two different misses, one on each platform, select the fallback; and the text said that every reading sends more to the owner, which is false for this one | Accepted as the review's second option. At that revision the cell reading stayed as a proposal, the false sentence was removed, and the case (a claim at 230 ms in a checkpoint window on macOS, an event over 5,000 ms on Linux) and the limit reading were set beside it for the owner. The owner has since chosen the limit reading, called the kind reading in the record (2026-11-21; the table after this one), and the cell reading is not adopted. No cell is added, and no cause is claimed. Technical attribution stays open | Spec "What a miss selects", AP13, record F of "Owner's answers" |
| Facts N1, minor: step 4 of those steps is not a reading of the ruling | Accepted. The text says four things are not the ruling's words and that step 4 comes from "The host" | Spec "What a miss selects" |
| Method MC2 and red team R2, major: only the diagnostic was named as exposed to a size call that moves the timer, and nothing said what follows an incomplete data set | Accepted for the text. C24, H32 and B24 are named as exposed; an incomplete data set selects no path, and a new one needs the owner's direction or an approved revision of the method, never a repeat of the unchanged method. Not done: no call is forced, no repetition lengthened, no other call credited, no cell or count changed, nothing measured. Whether the method can complete stays open and is the performance engineer's | Spec "Checkpoints in every repetition", "The verdict", "Diagnostics" |
| Resilience N1, major: the marker was created and rewritten in place, so a stop between its creation and its write, or during the change of direction, left a marker that neither command could read | Accepted. Every version is published whole by a temporary file, a sync, a rename and a directory sync; a temporary file is scratch; a marker that fails its check fails closed with a manual recovery that the RUNBOOK is to give; stops at each step of the publication are tested | Spec R138, "How a restore replaces the state", "Data directory"; ADR decision 16; T23, T26 |
| Resilience N2 and N3, minor: one directory of two was synced after a rename and the `pre-restore` directory was never created; an abandonment after the staging directory was gone had nowhere to put a part | Accepted. The directory is created and synced before the marker and both directories are synced after each rename. Once the staging directory is gone the restore is complete: `--abandon` is refused and `--continue` only checks and removes the marker. That refusal was later cut down to the direction `forward` (the table for `3c50293` below) | Spec R138, "How a restore replaces the state"; T23 |
| Security S-R1381, medium: a continuation told what was left from where the staged part lay, so with an old `-wal` and no staged one a stop after the database was moved out could end with the new database beside the old log | Accepted. The marker lists, for seven fixed members, which the replaced state holds and which the staged state holds. Every replaced member is moved out before any staged member is moved in, on a continuation too. The absence of a member that was never staged means nothing. The layout, the lineage, the generation and the ended leases are checked before the marker is removed, and a layout the steps cannot have left keeps the marker and is refused. The last check was later changed to read files only (the table for `3c50293` below). This is a gap in the contract; no shipped code exists and no replay of an old log was shown | Spec R138, "How a restore replaces the state"; ADR decision 16; T23 |
| Resilience N4 and N5, minor: nothing said that events have numbers before they commit, the stop record could be removed before its gap row was synced, and a second stop could overwrite the first record | Accepted. Events are numbered at handover; a start continues above everything committed and recorded; the record is published whole and merged, and is removed only when its gap rows are durable. The same rule for a finish file is item O8 of the red team's review of `196f9087` and stays open | Spec R61, R139, "Stopping", "Data directory"; ADR decision 23; T15, T16 |
| Resilience N6, a note: a service manager's kill time must exceed the stop's bound | Carried as a duty of the RUNBOOK (T26). No service manager's default is stated, since none was checked, and the RUNBOOK is not edited by this revision | Spec "Stopping"; ADR "Consequences" |
| Resilience N7 and red team R3, minor: "idle" sees load and build processes only | Accepted as a stated limit. The record says so in words; recording the scratch device's write activity is the performance engineer's to declare. No sample, limit or harness change is made here | Spec "The host" |
| Facts N2 and N3, minor: "about ten minutes later"; the 1 s and the 4 KiB were not labelled, and the list of proposals left out the new names and the verdict's names and exits | Accepted. The time reads "recorded 14:07 UTC"; both numbers are labelled; the list below is extended | Spec record E, R137, "How a restore replaces the state", "The verdict"; this plan's "Open questions" |

Kept as the reviews of `a914e78` left them, and changed by nothing here: resilience F3 (R140) and F4 (the
idle-host rule) and the normative part of MC8; facts F1, F2 and F4; the security review's closures for
R101 to R134. Not answered here and still open: the technical question of what shows that a miss is
attributable to events; whether the method can reach checkpoint coverage; MC4 to MC7, MC9 and the
harness's part of MC8; and everything listed as carried above.

Five independent reviews read the revision that followed, `3c50293`, on 2026-11-21, and each passed it
with conditions. Two of the red team's conditions were choices for the owner, and the owner answered both
on 2026-11-21 (the spec's record F). The revision that carries this paragraph writes those two answers
into the spec, this plan and ADR 0017, and changes nothing else.

| Put to the owner from the review of `3c50293` | The owner's answer, 2026-11-21 | Where |
|---|---|---|
| Red team L1: step 3 judged "one platform alone" by cell, as a proposal, so merging the text would have chosen the cell reading without an answer. The case: on macOS a C24 claim at 230 ms in a checkpoint window, on Linux a C24 event older than 5,000 ms | "kind reading is fine": a miss is shown by both platforms only when each missed the same kind of limit in the same population of the same cell. The case selects no path and goes back to the owner. The cell reading is not adopted | Spec R63, "What a miss selects" (step 3, the text under the steps, two rows of the cases), AP13, record F; ADR decision 12 |
| Red team D1, major: the rule that stops measurement after a miss was keyed on the headline `FAIL`, so a data set with a defect and a miss, whose headline is `CORRECTNESS_DEFECT`, could be followed by a data set that fixes the defect without the owner's word | "ask first": after a data set in which a mandatory repetition that is `valid` or `host_not_idle` has missed, no further data set is started without the owner's word, whatever the headline, a data set that only fixes a defect included. No exception | Spec R63, "The verdict" (the rule and the three rows of what may follow a data set), AP13, record F; ADR decision 12 |

The two answers choose two rules. They accept no number, limit, proxy for cause, eligibility rule, method
or result, and they permit no measurement. When that revision was written no independent review had read
the text that states them, so it closed neither condition. The five reviews of `d2fe9b0` read that text
afterwards (below); what each left is its own to say, and this plan records neither condition as closed.
The other conditions of the reviews of `3c50293` are not answered by this revision and stay open as those
reviews left them: resilience N8 to N12 (a stop inside the restore's last check, the end of an
abandonment, on which the security review left a note too, the numbering claim, the order of the gap
range, the durability of the marker and of the stop record), facts N4 (the attribution in R63) and the
notes on review status. Whether the same pass may
correct them was asked of the owner apart and is not part of these two answers.

The owner's answer to that came the same day (the spec's record G). The owner's words were "yes always",
to a question about corrections for review findings on the spec. The lead's record reads them as a
standing rule for these three documents: such corrections may be written without a new question each
time, and the result still comes to the owner as a pull request. It closes no finding. The revision that
first carried this paragraph, `d2fe9b0`, is a pass that the rule allows. The table is that revision's
answer to each condition that the reviews of `3c50293` left open. None was closed by it. When it was
written no independent review had read that revision or the one before it; five have read both since
(below).

| Finding at `3c50293` | Answer | Where |
|---|---|---|
| Resilience N8, major: the restore's last check opened the database, which makes a `-wal` and a `-shm`; a stop inside the check left them at a live name beside the new database, a layout that both commands refuse, so nothing could finish the restore | Accepted, by the review's first option. The check opens no database. The staged database is closed, the staging directory is looked at for anything but staged members, and the file alone is read once more to hold the new lineage, the new generation and no active lease, with its SHA-256 the same before and after; the marker holds that hash, and the last check compares the bytes at the live name with it. Nothing opens the live database while the marker stands, so the check leaves no file and is run again after a stop. The rule that refuses a `-wal` at a live name is unchanged. No way of opening a database without companions is relied on | Spec R138, "How a restore replaces the state" (steps 2, 3 and 5, the table of stops); ADR decision 16; T23 |
| Resilience N9, major, with the security review's note on the same case: an abandonment stopped after it had removed the staging directory was refused by `--abandon`, with a message that the restore was complete, and by `--continue`; and a part of the staging directory removed did not fit the table of places | Accepted. The refusal for a staging directory that is gone holds under `forward` only. Under `back`, `--abandon` first tests whether the abandonment has reached its end (every replaced member live, no other member live, `pre-restore` empty or absent), before the table of places and before any rule on the staging directory, and then only finishes the cleanup: the rest of the staging directory unread, the `pre-restore` directory, the marker, each skipped when already done | Spec R138, "How a restore replaces the state" (the two commands, the table of stops); ADR decision 16; T23 |
| Resilience N10, minor: "No number of a run shall be given twice" does not hold for numbers lost with a killed daemon or with the power under `synchronous = NORMAL` | Accepted. The claim is cut down to numbers that are committed or recorded, and the loss is stated. No durable record of the numbers given is added, and nothing is closed: the loss stays open as an item of its own, under this finding's name (the table of `d2fe9b0` below says what was corrected in this row) | Spec R61, "Stopping" (the table); ADR decision 23 |
| Resilience N11, a nit: nothing said that the ranges of a stop are taken after the writer's last commit, so a gap row could cover an event that exists | Accepted, both ways the review offered. The ranges are taken once the writer has ended its last transaction and no event is given a number any more; and a start cuts each recorded range to the numbers that no committed event and no gap row holds | Spec R139, "Stopping"; ADR decision 23; T15, T16 |
| Resilience N12, minor: the marker and the stop record are synced by this program and not by SQLite, and on macOS a plain sync does not empty the drive's cache, so what was said of a stop held for a stop of the process only | Accepted as a stated limit, with the intended call named. The spec now says which promises are for a stop of the process and rest on no sync, and what a sync means on each platform for these files. It proves nothing about a loss of power: no such test exists, whether macOS accepts the full flush for a directory was not checked, and the outcomes after a loss of power are listed, among them one that a start does not recognise. The manual recovery stays a duty of T26 | Spec "What a sync promises", R138, R139; ADR decision 16 |
| Facts N4, moderate: in R63 the sentence "That is the owner's ruling" stood after the eligibility rule and so named a proposal as the ruling | Accepted. The attribution now follows the two sentences that state the ruling and names the owner's "1."; the eligibility rule is called this spec's proposal, on which the owner has not ruled. The rule itself is unchanged | Spec R63 |
| Red team D1, major: a data set with a defect and a miss could be followed by another without the owner's word | Accepted by the owner's answer of 2026-11-21, already written by the revision before this one and kept as it was: the rule reads the budget axis, has no exception for the fix of a defect, and has its three rows. Two questions about where a data set ends are named as not settled and are the owner's | Spec "The verdict", R63; ADR decision 12 |
| Facts' notes: the status of the reviews in these documents was true when written and goes stale; the ADR's short form of the eligibility rule left out three of its terms | Accepted. The status lines say which revision each review read; the ADR names the backend, the settings, the timing check and the platform class | Spec (the text under the title), this plan, ADR decision 12 |

Left for the owner or for a named owner by this revision, and chosen by nothing in it: the two
questions about where a data set ends (the owner's); whether a start should recognise members moved
with no marker after a loss of power (the spec states the limit and adds no check); which sync call
macOS accepts for a directory (deferred to the After 2.0 milestone, issue 181; the Linux behaviour stays with the
builder in T15, T16 and T23);
a durable record of the numbers given (an open item of its own, resilience N10 of `3c50293`, which no
other item closes); and when a finish file may be removed (item O8 of the red team's review of
`196f9087`).

Five independent reviews read revision `d2fe9b0` on 2026-11-21 (resilience, security, facts, method, red
team), with the changes of `e8e681e` and of `d2fe9b0` together, and each passed it with conditions. The
revision that carries this paragraph answers those of their conditions that are wording, in the table
below. It changes no rule, number, limit, method or step, and it closes nothing.

| Finding at `d2fe9b0` | Answer | Where |
|---|---|---|
| Resilience N14, minor: R61 and this plan counted the loss of event numbers that were given and neither committed nor recorded with "the red team's O6". That item of the red team's review of `196f9087` is the drain before an import and the work that left the process group, and nothing in it concerns event numbers, so closing it could have been read as closing the loss. The resilience review says that the error began in its own report on `3c50293` (N10) and was copied from there. O6 and O8 with no review named are also the spec's own open points, which the owner decided | Accepted. The tie is removed. The loss is an open item of its own, which no other item closes. An item of a review is named with its review and the revision it read, and the red team's O6 and O8 are told apart from the spec's. Neither the loss nor the red team's O6 or O8 is closed | Spec (the text under the title, R61, "Stopping", "What a sync promises"); this plan (the paragraphs and rows above that named those items); ADR 0017 (the alternative on the stop record) |
| Resilience N15, a nit: R61 named two ways in which a given number is lost and can be given again; the spec states two more elsewhere, and the two could be read as all | Accepted. R61 names the four and says that the list is not complete; the ADR says "for example". No promise changes | Spec R61; ADR decision 23 |
| Facts M1, minor: "the owner has not read it", of the eligibility rule, says more than the record shows. The record shows that the owner has not ruled on it | Accepted | Spec R63, AP13; this plan (the row of facts N4 above) |
| Facts M2, minor: this plan gave the three-document scope as the owner's answer. The owner's words are "yes always"; the scope is in the lead's record of them | Accepted. The spec's record G, which already said so, is unchanged | This plan (the paragraph above the table of `3c50293`) |
| Facts' condition on review status: "no independent review has read this revision" is true when written and stale as soon as a review reads it | Accepted. Each such sentence now says which revision which reviews read, as a thing of the past, and none says of the revision that carries it that it is unread | Spec (the text under the title, record F); this plan; ADR decision 12 |

Not answered by this revision, and carried as each review left it: every other condition of the five
reviews of `d2fe9b0`, among them the resilience review's that a start does not recognise members moved
with no marker after a loss of power, which stays a stated limit; and everything listed as carried or
left above.

Two independent reviews read the revision that followed, `8954cf6`, on 2026-11-21 (resilience, facts), for
its changes, and each passed it with conditions. The same day four things that the spec had put to the
owner, two as proposals and two as open questions, were answered under the owner's delegation, and the same
record allows the
follow-up PR to open as a draft (the spec's record H). The answers are not the owner's own words; the spec's
record gives the delegation word for word. The revision that carries this paragraph writes those answers
into the spec, this plan and ADR 0017. It changes no number, limit or eligibility rule, chooses no method,
permits no measurement and closes no finding.

| Put to the owner (the lead's name for it) | Answer under the owner's delegation, 2026-11-21 | Where |
|---|---|---|
| The list of kinds by which the kind reading is applied (AP13-KINDS) | Adopted, and operative: the regular claims, the claims of a checkpoint window, the claims of a burst window, an event's age, and a refusal or something still queued. The match is made on the cell, the population and the kind together; kinds named in a report beside a selection made without them do not apply the reading | Spec R63, "What a miss selects" (step 3, the text under the steps, the cases), AP13, record H; ADR decision 12 |
| That one miss of steps 2 to 4 anywhere returns the whole choice to the owner (AP13-WHOLESET) | Adopted: such a miss anywhere in a data set keeps it from selecting an event path. Not every miss stops every path: an eligible data set in which every miss is a valid miss in a population that holds events, shown by both platforms in the same cell, population and kind, still selects the fallback | Spec R63, "What a miss selects" (the text under the steps, the cases), AP13, record H; ADR decision 12 |
| Whether the second platform's block, run after a mandatory miss on the first, is a further data set (AP13-BOUNDARY-1) | It is further measurement and needs the owner's word. Until then the data set stands with a platform absent, selects no path, and nothing stands in for the block that was not run | Spec R63, "The verdict" (the paragraph on the two questions, the rows of what may follow a data set), AP13, record H; ADR decision 12 |
| Whether replacing an `invalid` or `host_not_idle` set of five, in a data set that holds a mandatory miss, starts one (AP13-BOUNDARY-2) | It is further measurement and needs the owner's word. The old set and every miss are kept. A set replaced where no mandatory miss stands is not touched by this answer | The same |
| Whether the follow-up PR for the corrected text is opened as a draft (the answer D8) | One new PR may be opened as a draft from the reviewed correction once its publication checks and its disclosure package (its statement of what it does not decide) are ready. D8's conditions stand before approval and merge; the merge is the owner's | Spec row D8 of "Owner's answers", record H; this plan ("Open questions") |

Selecting a path, permission to measure and qualification to ship are three things, and the answers give
only the first its rule: a selected fallback is measured as built only on the owner's word, since the data
set that selected it holds a mandatory miss, and it ships only when it has passed as built (spec R63). Not
answered, and still proposals: the numbers and the results, how the lanes and the collector sit beside the
proposal, the sync settings, the eligibility rule, the reading of "a burst alone", the host and platform
rules, the fifth point of AP14 and the word on the live copy for T4c. The method challenge's findings on
attribution and on whether a complete data set can be produced, and the controls it asked for, are findings
about the method and stay open as their reviews left them; an answer on a rule closes none of them.

Four independent reviews read that revision, `ec68890`, on 2026-11-21 (method, red team, facts, resilience):
one blocked it and three passed it with conditions. The revision that carries this paragraph answers them.
A row here says what the text does with a finding. It does not close the finding: that is the reviewer's
to say.

| Finding at `ec68890` | What the text does |
|---|---|
| Method DM1 (major, blocking): the text on the two answers about further measurement gave `FAIL, INCOMPLETE` or `INCOMPLETE` to a data set that also has a correctness defect, against the order of the headlines | The headline and the exit are those of the one rule, the defect first, with the coverage and the budget axis kept; two rows trace the mixed cases, each beside its control without a defect (spec "The verdict") |
| Red team R1 (major): nothing listed as open what is done with a block that is running when a mandatory miss is seen, or whether the platforms' blocks may overlap | Both are named as open with what the owner holds (spec "The verdict", and AP13 in "Open questions" and in "Owner's answers"; this plan's "Open questions"); no campaign is started before they are settled; no rule for stopping, scheduling or tearing down a block is chosen |
| Facts F-M1 and red team R2: record H gave the second answer as "misses of one kind" | Record H quotes the answer; a case shows two kinds of miss, each matched on both platforms, selecting the fallback; every case that selects nothing is kept |
| Red team R3: a selection could be read as starting the measurement of the fallback | Step 5, the AP13 row and ADR 0017 decision 12 say that it is started only on the owner's word |
| Red team R5 and the resilience review's O1: which data set a block that the owner allows belongs to | Not chosen; named as open with the two of R1 |
| Resilience N17: nothing enforces the two answers at a start, and nothing bounds a stop | Carried as a duty of the method's proposal for a campaign ("Test plan", T4b); not due before one is proposed |
| Facts m1 to m6 and m9: the count and the time of record H, and five phrases of it and of this section | Five of six answers; no time of day is given; two proposals and two questions; the limits as the record has them, with the spec's reading named as a reading; the reach of the withdrawn rule; the draft's conditions as the record has them; the reason for the sixth read is not given as the lead's words |

Why another round of design review runs. AGENTS.md asks for the reason from the third round on. This text
has been read at `196f9087` and at `a914e78`, and this revision asks for a further read. The lead's
reason, from its adjudication of the reviews of `a914e78`, is that the second read produced counterexamples
that the text did not answer, and not a request for a wider design: (1) a diagnostic that stores an event
twice, beside valid C24 misses on both platforms, reached the fallback by the ordered steps; (2) a restore
stopped between creating its marker and writing it left a marker that no command could read; (3) a restore
stopped after the old database was moved out and before its `-wal` could continue to a new database
beside the old log; (4) a stop record removed before its gap row was synced lost the gap with the power,
and a second stop overwrote the first record; (5) two different misses, one on each platform, selected the
fallback while the text said that this reading sent more to the owner. The round is limited to those
conditions and to what their corrections touch. A test cannot settle them yet, because no product code is
built before the owner's gate; the named tests of R61, R138 and R139 and the verifier's cases carry them
to T15, T16, T23 and T4b.

A further round follows the reviews of `3c50293`, which makes four reads of this text. The lead's
reason, from its disposition of those reviews, is of the same kind: counterexamples and one wrong
attribution that the text did not answer, and no request for a wider design. (1) A restore stopped inside
its last check, which opened the database, left a `-wal` at a live name that both commands refused, so
nothing could finish it. (2) An abandonment stopped after it had removed the staging directory was
refused by `--abandon`, with a message that the restore was complete, and by `--continue`. (3) "No number
of a run shall be given twice" did not hold for numbers lost with a killed daemon or with the power.
(4) Nothing said that the ranges of a stop are taken after the writer's last commit, so a gap row could
cover an event that exists. (5) The marker and the stop record are synced by this program, and on macOS
a plain sync does not empty the drive's cache, so what was said of a stop held for a stop of the process
only. (6) R63 called the proposed eligibility rule the owner's ruling. (7) A data set with a defect and a
miss could be followed by another without the owner's word, which the owner has since answered. The
round is limited to those seven and to what their corrections touch. A test still cannot settle them
before the owner's gate; the named tests of R61, R138 and R139 carry them to T15, T16 and T23.

A fifth read follows the reviews of `d2fe9b0`. The lead's reason, from its disposition of those reviews,
is one wrong attribution: the loss of unrecorded event numbers was counted with an item of the red team
that is about something else, so that closing that item could have been read as closing the loss. Three
points of wording are corrected in the same pass. The read is limited to what this revision changed after
`d2fe9b0`, which is no rule; no test settles an attribution.

A sixth read follows the answers of the spec's record H. The reason is that four things which were open
are now answered, that every changed text needs its checks and its review (the spec's record H), and that
the text which states the answers is new: the selection with its cases for and
against, the two answers on further measurement with what is kept at a stop, what a draft does and does not
decide, and whose words each answer is. The read is limited to that. No test settles it before the owner's
gate; the verifier's cases carry the selection to T4b.

A seventh read follows the correction of that text. The reason is what the sixth read found (the table of
findings at `ec68890` above): one reader blocked on a headline that had two answers, and the text that
removes it is new. The read is limited to the changes that the table names, by the four readers of the
sixth. No test settles it before the owner's gate.

An eighth read follows the owner's decision of 2026-11-21 on latency (the spec's record I). Its reason is no
finding: the owner dropped deliverable D and gate line 7, and the text that defers them is new. It is one
independent review of that change, limited to the notes of deferral, to the gate as it stands in the three
documents, and to whether the rulings on AP13 and every requirement on the correctness and the safety of
events are kept. No test settles it. The findings on the measurement's method that were open when it was
deferred are parked with it and are not closed.

That review read `3a6189a` and passed it on one condition, which this text answers. The hostile run ids
were tested on the event file's name, which 1.1 does not build, while the finish file that 1.1 writes had
one harmless vector. The vectors and the outside canary now stand on the finish file: on its name and on
the operations that write it, recover it and remove it (spec R101, R129 and "Filesystem names"; T12 and
T13). Every test of an event file waits for the fallback (T16). The security reviewer of those rows agreed
to that placement in writing before it was written, and has not read this text. Four small points of the
same review are corrected with it: two rows of the spec that named R5 without its deferral, the layout row
of the event file, the place of T4c's count, and the wording of T1's status. The same reviewer reads the
change once more, and that read is limited to it.

### T10 split: the independent writer and read-pool foundation

T10's row stays as written, and so does its completion rule: T10 is done only when its whole row is. A first slice
that needs no port v2 type is delivered as its own PR (PR 122) so that the later slices review against it. A second
executor slice, the deadline recheck after `BEGIN IMMEDIATE` and the bounded writer tests, is delivered as PR 154,
which is open and pending its review and merge.

**What the foundation is.** The writer executor and the read pool, in `crates/orch-infra/src/sqlite/writer.rs`
and `read_pool.rs`, with their module tests. It uses T9's data directory and connection setup and nothing from T8.
The internal goal adapter now uses the executor. The daemon still uses file storage. The executor’s behaviour and limits are in
[ARCHITECTURE.md §6.6](../ARCHITECTURE.md#66-sqlite-writer-and-read-pool-foundation-not-wired). Of the spec it
covers R24 (one writer, `BEGIN IMMEDIATE`, read-only readers), R25 (a snapshot per read), R98 (pool size, bounded
wait, no extra connection) and the order of R26's lanes. Of R128 it covers one case only, from PR 154: a request
whose deadline passes while `BEGIN IMMEDIATE` waits for the write reservation is refused before its body runs and
never commits. Nothing else of R26 and nothing else of R27, R28, R126 to R128, R130, R137, R139 or R140.

**Module tests.** Unit tests under `crates/orch-infra/src/sqlite/writer/tests/mod.rs` and
`read_pool/tests/mod.rs`. They are not the `sqlite_writer` and `sqlite_crash` integration tests that T10's
"Verify" column names; those and the SQLite run of the conformance suite remain T10's. Every receipt wait in the
`writer` module gives up after 10 s, and a scoped test closes admission when its body unwinds so that the join of the
worker cannot hang behind a failed assertion: a regression fails by name instead of hanging. `a_withdrawn_request_is_never_committed` covers a deadline that
passes during the `BEGIN IMMEDIATE` wait only. It is not the oracle of R128's retry case.

| Module | Tests |
|---|---|
| `writer` (11) | `writes_run_one_at_a_time_on_one_connection`, `body_and_commit_errors_keep_nothing_and_writer_continues`, `claims_precede_queued_work_and_other_lanes_use_deadlines`, `bounded_queue_and_single_pending_event_transaction`, `expired_queued_request_never_runs_and_close_drains_accepted_work`, `a_panicking_transaction_rolls_back_and_closes_the_worker`, `executor_runs_claims_first_and_dropped_receipts_still_commit`, `a_deadline_that_passes_behind_active_work_withdraws_the_request`, `dropping_the_last_handle_closes_admission_and_drains`, `a_deadline_that_passes_while_begin_waits_is_rechecked_before_the_body`, `a_withdrawn_request_is_never_committed` |
| `read_pool` (7) | `a_read_snapshot_does_not_see_a_later_commit`, `exhaustion_waits_until_deadline_and_never_opens_an_extra_reader`, `error_and_panic_release_the_slot_and_end_the_snapshot`, `a_waiting_reader_uses_the_released_slot_before_its_deadline`, `a_free_slot_is_usable_when_a_waiter_reacquires_after_its_deadline`, `a_failed_read_discards_its_connection_local_state`, `a_lease_left_in_a_transaction_is_discarded` |

**Dependencies of the full T10.** The SQLite adapter of port v2 needs T9 and the whole of T8:

- T8a, the additive entity contract and its fake (merged);
- T8b, the file reference with blob, configuration and template coverage;
- T8c complete: the operational port and conformance coverage for leases, event batches, retries, intents,
  request keys and attention. Its first slice, the events and the leases, is not enough. The time unit of the
  operational port and the database's must be reconciled deliberately.

The public SQLite port awaits accepted T8b and T8c. T12 depends on completion of T8 and T10.

**What remains of T10 after the foundation.** The adapter of the Storage v2 port and its conformance run on SQLite;
bulk identities and the 1,000-row or 20 ms cut (R27); event aggregation into one transaction (R26); intents
(R28); retry that replays safely and one shared fatal policy by failure class (R128, R140); the checkpoint
connection, its backlog, outcomes and the expiry of long reads (R126, R127, R137); the authorizer and query
connection (R130); and shutdown integration (R139). The review conditions of the foundation that belong to the
adapter are tracked in [issue 127](https://github.com/yuxhuang/r-orch/issues/127): detecting transaction control
in a callback, non-blocking receipt waits on async threads, bounded claim production, and fatal-state handling
that keeps queue locks free of callbacks. PR 154 delivers two items of that list, the recheck of a request's expiry
after `BEGIN IMMEDIATE` and writer tests that fail in bounded time. Both are pending its merge. The retry case of
R128, in which a request answered `overloaded` cannot commit later, is still owed at the adapter integration. The
daemon is wired to this executor only after all of these exist.

### Goal boundary slice and remaining allocation consumers

The inactive goal adapter checks both timestamp fields against the staged canonical range in the
spec's "Staged SQLite goal timestamp and position limits" section. Its entity allocator checks the
project-local `position:goal` counter on create and move, preserves it on delete, and refuses missing,
negative or behind counters. Focused tests cover bootstrap, caught refusals, nonpositive surviving
positions during create/move, signed exhaustion, reopen and deleted highest positions. This does not
complete the public adapter or conformance; the daemon still uses file storage.

Remaining consumer obligations, before activation:

- T21 initializes each imported ordered family's high-water mark transactionally and preserves larger
  durable marks. Import/export must carry marks for emptied families: `MAX(position)` loses deleted
  issued positions and is not counter recovery. Export integration remains with T23.
- T22 verification checks positive row positions, nonnegative family counters, counter presence for
  nonempty families, and every surviving position against its matching counter.
- T10 agrees on exact family namespaces and integrates task, agent, message and other entity families.
  This patch validates goal allocation only. Lease epochs retain T12/T13/T23 ownership and event
  sequences retain their separate adapter checks.

These consumer obligations are pending, not delivered by the goal boundary slice.

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | first-review text (`f5637805`) implemented in PR 1, `fast` on macOS, and merged to `main` by the owner on 2026-11-21; this corrected text, with the review corrections and the owner's answers, implemented in a follow-up PR that is pending its reviews and the owner's merge. The spec is not approved by either. Since then: the corrected text (`34b3b4d`) reached `main` on 2026-11-22 (UTC) through that follow-up PR, and the deferral of 2026-11-21 (the spec's record I) is implemented in a further documentation PR that is pending its review and its merge. This revision changes no document's status (`draft`, `proposed`) |
| T4 | dropped as an entry step by the owner on 2026-11-21. The first run of 2026-11-20 is recorded in [issue 98](https://github.com/yuxhuang/r-orch/issues/98) and is not delivered to `main` |
| T4b | deferred by the owner on 2026-11-21; not run |
| T10, first slice (writer and read pool) | implemented as an unwired foundation in PR 122 at `63edf0f9`, with its module tests; its Linux CI, independent reviews and the owner's merge are pending, as are the follow-ups of issue 127. The rest of T10 is pending behind T8b and T8c ("T10 split"). The deadline recheck and the bounded writer tests are in PR 154, which is open and pending its Linux CI, reviews and merge |
| Linux-only scope (the spec's record K) | recorded on 2026-11-23 by one documentation PR that is pending its review and the owner's merge: the spec, this plan, ADR 0017, the lease prototype's spec, the spec TEMPLATE, ARCHITECTURE §13 and AGENTS.md carry it. Nothing was verified on a platform: no code and no CI changed. The first independent review blocked its first head on three points: the Linux sleep/wake check of R42 (issue 110) was withdrawn with the macOS one, R16, R88 and R136 still named macOS, and the branch conflicted with `main`. The text was corrected for them and re-synchronised with `main` keeping the rows of both sides; the corrected text awaits a second review |
| T10, internal goal adapter | Goal reads, creates, updates, deletes and explicit moves use the executor. Module tests cover transactions, snapshots, reopen and file-reference behavior for supported inputs. Public conformance still requires timestamp and counter compatibility, imported position high-water marks, and text/blob handling. |
| T2 to T27, except T4 and T4b | pending, as this table stood before 2026-11-21; this revision brings no other row up to date |

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
  result, and neither is a run on a container that stands in for a host. Since 2026-11-21 none of those
  measurements is part of 1.1 (the spec's record I): T4b is not run and T16 measures nothing, so 1.1 states
  no performance result at all, and the order is what its tests prove.
- A real suspend and resume on an isolated macOS machine (spec R42) was a recorded check. The owner's ruling of
  2026-11-23 (the spec's record K) withdraws that macOS check for every release through 2.0 (the physical macOS
  observation is issue 178), and the prototype's passes on macOS arm64 stay as history. The Linux check stays: a
  supervised process through a real suspend and resume of an isolated Linux host that the owner has authorized,
  tracked in issue 110 and pending. The clock-seam test injects the clock and proves renewal ordering only; it
  does not stand for that check.
- Processes (spec R100, R119): every test that could signal uses a fake process table that records each call,
  and asserts that a pid read from a source or a backup is never signalled.
- A restore's steps (spec R138): a child process is stopped before and after every copy, sync and rename,
  and so are `--continue` and `--abandon`. After each stop the test asserts one of three states and no
  other: the live state unchanged; the marker present and every start refused; or the restore complete.
  A continued restore is compared with an uninterrupted one, and an abandoned one with the replaced state
  byte for byte. Free space is set one byte under the need, and a copy is made to fail while staging.
  The stops include each step of the marker's publication (create, write, sync, rename, directory sync),
  for `forward` and for the change to `back`, and each rename of `orch.db`, `orch.db-wal` and
  `orch.db-shm` where the replaced state has the two companions and the staged state has none. Each
  command is also run a second time. A layout that the steps cannot leave, a marker that fails its check
  and, while the direction is `forward`, an abandonment after the staging directory is gone are each
  refused with the marker kept. The stops also include each read of the last check, which opens no
  database, and each removal of an abandonment's cleanup: a part of the staging directory gone, all of
  it gone, and the `pre-restore` directory gone as well. After each of those the test asserts more than
  a refusal: `--continue`, or under `back` `--abandon`, runs to its end, and a state that both commands
  refuse fails the test unless the spec's table of stops calls it unexpected. No `-wal` or `-shm` may
  appear at a live name while the marker exists. These are stops of the process. No test cuts the
  power, and the spec's "What a sync promises" says what is therefore not proved.
- A stop (spec R139): with a second connection holding the write lock and a fake clock, the stop ends at
  its bound; each step is recorded in order; events handed over and not committed are a gap row after the
  next start, or are named by the stop when their record could not be written. An event has its number
  at handover; a start continues above the committed and the recorded numbers; a second stop keeps the
  first record's ranges; the record is removed only after its gap rows are durable (a fake reports the
  commit as not yet synced); a start that is killed and run again writes each gap once. The ranges are taken after the writer's
  last transaction, and a start writes no gap row over a committed event (a record with such a range
  is planted).
- Other connections (spec R137, R140): a result is injected at the checkpoint, read, query and backup
  connections in turn. Corruption fails the writer from each; other failures are counted and reported at
  their thresholds and do not fail it; each outcome of a checkpoint call is produced from injected counts,
  and a running total repeated by the next call credits no second copy.
- Deferred on 2026-11-21 with T4b, kept as a record and not due in 1.1, where no verifier is built.
  The verdict (spec "The verdict", "What a miss selects"): the verifier of T4b gives every row of the
  spec's table of cases as written, from recorded inputs made for the purpose, the mixed cases included
  (a defect beside a miss, an unknown correctness or a backend not shown beside a miss): a verifier that
  selects any path for them is wrong. So is one that selects a path when the two platforms miss different
  kinds of limit in one cell (the owner's answer of 2026-11-21). The kinds are operative (the spec's
  record H): the verifier reads the kind of every miss and matches on the cell, the population and the
  kind together. One that matches by cell alone is wrong; one that names the kinds in its report and
  selects without them is wrong; one that selects a path while a miss of steps 2 to 4 stands anywhere in
  the data set is wrong; and one that selects nothing for an eligible data set in which every miss is a
  valid miss in a population that holds events, shown by both platforms in the same cell, population and
  kind, is wrong as well: the fallback is selected there, for one such miss and for two kinds of miss that
  are each matched. The verifier gives the headline and the exit of every row of the spec's table of what
  may follow a data set as well: one that gives `FAIL, INCOMPLETE` or `INCOMPLETE` where a repetition has
  a defect, beside an absent platform or an `invalid` set, is wrong. No repetition of the
  measurement is shortened or run again to make such an input. The rows of what may follow a data
  set are a rule for whoever starts one under the spec's "Event measurement": after a mandatory miss none
  is started without the owner's recorded word, the fix of a defect included; nor is the second
  platform's block, nor the replacement of an `invalid` or `host_not_idle` set of five. A selection
  starts no measurement of the path it selects. A method that proposes a campaign (T4b) carries, with
  whatever it proposes for a block that is already running: a check at every start of a second block or
  of a replacement, which reads the recorded budget axis and the owner's recorded word and refuses with a
  record; and for any stop a bound within which the processes it owns are ended and the host's lease is
  given back, an end record is written or the coverage is marked incomplete, and partial output is kept,
  with a control test (the resilience review of `ec68890`, N17). That is not due before a campaign is
  proposed, and none is authorised.
- The importer is tested on generated projects of the live project's shape and on a hand-written corpus of
  damaged input (T21), then once on a copy of the live project (T27).
- Names and paths (spec R101 to R108, R117, R118): every test that plants a hostile name, link or special file
  also plants a canary outside its root and asserts that the canary's bytes and metadata are unchanged and that
  its contents appear in no row, blob or report. The vectors of the spec's "Filesystem names" are tested as
  written, by T9 for the confined directory, T11 for blobs, T12 and T13 for the finish file, T21 for a 1.0
  source and T23 for backups and exports. Since 2026-11-21 no task tests an event file: its vectors, the
  name's included, are due only with the event fallback, which 1.1 does not build (spec R63). The run ids of
  those vectors are tested on the finish file instead, by T12 through the write and the removal after the
  commit and by T13 through recovery, each with the canary (spec R129).
- Two ids on one workspace (spec R117, R118): the pair `tsk_Case` and `tsk_case` and a foreign directory and
  branch under the other spelling, each with a canary, are tested as the spec's vectors say: by T8 and T9
  for the storage, T21 for a 1.0 source, T23 for an export and a backup, T12 and T13 for prepare, reuse,
  merge, cancel, failure and cleanup. R117's tests do not depend on the filesystem. R118 is accepted on Linux
  through three paths, none inferred from another: the injected folding listing, the native exact-name,
  device-and-inode and canary checks on the Linux filesystem the test uses, and the real folding branch wherever
  that filesystem folds case. Each test says which paths it ran and which it did not and why. No folding
  filesystem is required or provisioned (the spec's record K). At the time of this revision none of the R118 tests
  exists, so every path is pending implementation in T12 and T13.
- Hostile input (spec R109 to R116): the vectors of the spec's "Import bounds" are tested as written, each
  limit at its value and at one over. T21 owns the documents of a 1.0 source and T23 the export files. Every
  entry runs in a child process and must end with its stated refusal within 30 s and 512 MiB resident, with
  the source byte-identical and the destination as the spec says for that refusal. The product's own export
  must pass under the same limits that refuse the hostile ones.
- `./xtask/gate.sh full`: T18, T23 and T24 (security-relevant and process handling), and once for the goal in T27,
  on native Linux. Every PR: `fast` after each commit, the pre-PR check before opening, Linux CI
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
- Five points were added to the spec on 2026-11-22 and are not answered: T21-C1, T21-C2, T21-X1, T21-X2
  and T21-O1 (the spec's record J). The spec drafts the first four from their defaults and the fifth from
  a proposal, and T21 builds no caller on the `config.yml` path or on the index reconciliation before the
  owner has merged that text ("T21 config and index contract").
- A third thing is the owner's and lies outside the answers: the word on using the live copy for T4c (AP8).
- Two things are still the owner's inside those answers. The numbers and results of AP13 (T4b), which the
  owner deferred on 2026-11-21 with the measurement (the spec's record I), so that nothing in 1.1 waits for
  them. And the
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
  as standing for the follow-up PR. By an answer of 2026-11-21 given under the owner's delegation (the
  spec's record H) one new follow-up PR may be opened as a draft from the reviewed correction once its
  publication checks and its disclosure package are ready. A draft is not accepted content: D8's conditions
  stand before the
  spec is approved and before that PR merges, and merging it chooses no proposal that is still pending,
  accepts no number or result and releases no product task. When it merges is the owner's to rule;
  nothing in this plan depends on when. Showing the owner the corrected text does not wait for the entry
  steps.
- The owner has answered AP11: a restore lineage is carried beside the epoch (ADR 0017 decision 16, spec R77
  and R132). T5 still has to confirm the mechanics and settles how a process is held until its pid is
  committed (R96). Nothing before T12 depends on either.
- **Deferred by the owner on 2026-11-21** (the spec's record I): this item is no longer open for 1.1. T16
  stores events in SQLite by the batched design, no fallback is built, T4b is not run and nothing is measured
  as built. The rest of the item is kept as a record; the rulings in it are not withdrawn and have nothing to
  act on until latency is taken up again.
  T4 missed the first budget on macOS and has no figure from native Linux; that result is kept. The owner
  directed a revised workload and budget (spec AP13), and T4b measures it by the method the spec states. The
  numbers of that budget are proposed and not accepted: T4b's results and the numbers go to the owner
  together, with what the spec proposes for the three numbers that do not agree with them (lane 1's 50 ms,
  lane 3's 1 s, the collector's 256 events or 250 ms), and the owner's ruling chooses T16's path. By the
  owner's ruling of 2026-11-21 every miss is kept, a miss selects the fallback only when it is attributable
  to events, and a miss without events or on one platform alone selects no event path and goes back to the
  owner (the spec's "What a miss selects"). By the owner's two answers of 2026-11-21 that followed, "one
  platform alone" is judged by kind of limit and population, so two different kinds of miss in one cell,
  one on each platform, select no path; and after a mandatory miss in a `valid` or `host_not_idle`
  repetition no further data set is started without the owner's word, whatever the headline, a data set
  that only fixes a defect included (the spec's "The verdict"). By four answers of 2026-11-21 given
  under the owner's delegation (the spec's record H) the five kinds are operative in that judgment, with
  the cell and the population kept in the match; a miss of steps 2 to 4 anywhere in a data set keeps it
  from selecting an event path, while an eligible data set in which every miss is a valid miss in a
  population that holds events, shown by both platforms in the same cell, population and kind, still
  selects the fallback; and starting the second platform's block after a mandatory miss on the first, or
  replacing an `invalid` or `host_not_idle` set of five while such a miss stands, is further measurement
  that needs the owner's word. Selecting a path is not permission to measure it and does not qualify it
  to ship. Settled by no answer, and open: what is done with a block that is already running when a
  mandatory miss is seen, and whether the blocks of two platforms may overlap or be run in turns; no
  measurement campaign is started before those two are settled. Open as well: which data set a block that
  the owner allows belongs to. The path that ships is measured as built
  before T16 merges.
  T16's scope is written for either.
- This revision proposes, and nobody has yet reviewed or approved: the limits for an idle host and the
  platform classes; the times of a stop (10 s, 5 s, 20 s); the counts and times of the reports for other
  connections (three in a row, 5 minutes); the 1 s before a failed checkpoint call is tried again; the
  4 KiB bound of the restore's marker; the names `restore-in-progress`, `restore-staging-<random hex>/`,
  `--continue`, `--abandon`, `spool/events-at-stop` and their temporary files; the SHA-256 of the
  staged database as a field of that marker; the sync call named for each platform in the spec's
  "What a sync promises"; the names of the verdict's
  axes and values, its headline words and its exits 0, 1, 2 and 3; and, under the spec's "What a miss
  selects", the eligibility rule, the reading of "a burst alone" and the step taken from "The host". The
  kind reading itself and the rule on a further data set after a miss are no longer among the proposals:
  the owner chose both. Nor are the list of kinds, the rule for the whole data set and the two questions
  on what counts as further measurement: they were answered on 2026-11-21 under the owner's delegation
  (the spec's record H).
  The coverage rule of the measurement is unchanged, and a valid run of the production rule can leave a
  repetition without it; whether the rule changes is a question for a reviewed revision of the method.
  The mandatory cells C24, H32 and B24 may be exposed to that as well as the diagnostic, so whether the
  method can produce a complete data set at all is open, and it is the performance engineer's to settle
  before the cells are frozen. An incomplete data set selects no path and is not run again without the
  owner's direction or an approved revision.
  Since 2026-11-21 the proposals of this item that belong to the measurement alone are deferred with it and
  are not due in 1.1: the limits for an idle host and the platform classes, the names of the verdict's axes
  and values with its headlines and exits, the three things under "What a miss selects", and the coverage
  rule. The other proposals of this item are unchanged.
- No task yet owns a native Linux host: the owner's answer D6 allows the evidence, the spec's "Verification
  environment" lists the host as not yet named, and until one exists T4b is incomplete and R5 and R63
  cannot be met. Since 2026-11-21 neither measurement is due in 1.1 (the spec's record I); T27's cold gate
  on native Linux is unchanged.
- The task graph does not yet match this table (red-team condition C2): it has no task for T4b or T4c, its
  events task does not wait for T4b, and its qualification task still names the fallback. The lead repairs
  it before any product task is released; this plan changes no task. Since 2026-11-21 the graph needs no
  task for T4b, and its events task waits for T10, T11 and T13 alone.
- Two proofs are carried from the closure reviews and are not yet made: the command that installs 1.0
  beside 1.1 (T26, spec R136, a recorded check on a Linux host), and the read-only lock against a 1.0 daemon on Linux (T9, spec
  F9 and R31; the macOS half is not required before 2.0, record K).
- T24 and T25 could be one PR if the wiring is small. They are planned as two so that the switch is reviewed
  as behaviour and the removal as a move.

### T21 config and index contract (issue 146)

The compatibility evidence on the bounded reader ([issue 146](https://github.com/yuxhuang/r-orch/issues/146))
left two questions to the spec owner before the reader gets a caller: what the import does with a `config.yml`
that 1.0 ran on its defaults, and what it does with an index array that holds an entry without a string
identity, or one identity twice. The spec now carries the contract for both: R65, R70, R109, R110, R122 and
R123, the tables under "Import rules" ("Order", `config.yml`, "Import problem classes"), the row of the
document count in "Compatibility with 1.0", and the vectors C-V1 to C-V14 and X-V1 to X-V14 with a test name
each. It also corrects three statements of the spec and one of ADR 0017 decision 21 that were wrong in fact
for an empty `config.yml` (the spec's fact F18).

The decision is still open in one sense, and this plan does not close it. Five points were put to the owner
on 2026-11-22 (the spec's T21-C1, T21-C2, T21-X1, T21-X2 and T21-O1) and none is answered. The text is
drafted from the default that each of the first four carried, which keeps a stop that the spec or the reader
already had, and from
a proposal for the fifth, which carried none: what a key that `config.yml` does not set imports as, and so
whether a project with no such key gets `run_cap` 6 or none. The spec owner recommends another answer than
the default on three of the four. The owner ratifies the drafted rows by merging the PR that carries them,
and an explicit answer replaces a default through a new contract PR before T21 builds on it.

What follows for T21, and nothing here is built by this text:

1. T21 gives the `config.yml` path and the index reconciliation no caller before the owner has merged that
   PR. The rest of T21 is not held by it.
2. With the wiring, T21 implements the whole-array examination of an index (the reader returns at the first
   problem today, which names the class by the order of the entries), the name `index_duplicate`, the
   statements of what 1.0 did with a file, and the tests of the C-V and X-V vectors. X-V13 is a unit test of
   the `import` module, where a reader can be injected. The statement for `index_duplicate` gives both of
   1.0's cases and claims neither (the spec's F19 and R122): 1.0 used the index as it stood only where
   every entry was a complete entity of the directory's type, and otherwise rebuilt it. The import stops
   on identities alone in both cases, and T21 adds no decoding of an entry as its entity type. X-V5's test
   runs 1.0's own reader beside the import, on disposable copies, to show the two cases.
3. Three things the vectors pin are unverified until then: whether the bounded pass counts a bare `---` as
   one document (C-V6); the bounded reader's refusal of a stream of no document for an agent, a goal and a
   team (C-V11); and what 1.0's reader does with a repeated identity in each directory that has an index,
   which is read from the code and was run once outside the repository (X-V5).
4. The evidence of issue 146 is on `main` since 2026-11-22: its tests, the section "T21 reader compatibility
   evidence" of this plan, and a record of its own, `docs/internal/import-reader-compatibility.md`. This
   contract changes none of the three. T21 brings the record and that section up to date with this
   contract, or folds the record into this plan, in the PR that does the wiring. Two of their statements
   are then out of date. They say that the spec owner must reconcile R109 with the compatibility table
   before the first caller; this section and the spec rows it names are that reconciliation, once the
   owner has merged them. And the record's row on damaged indexes says that decoded duplicates survive
   1.0's reader. That is so for the test type of one field that the test uses. In a real directory an
   array of identities alone does not decode, and 1.0 rebuilds it (the spec's F19).

### T21 bounded reader slice

The private `orch-infra::import` module compiles the read-only YAML/JSON budget pass,
streaming event decoder and index reconciliation. Its unit tests use the landed synthetic
generator and index-recovery corpus. This is document-level evidence only: whole-source
preflight, path confinement, reports, writes, resume and live-copy verification remain T21 work.
The v1 daemon remains file-backed.


### T21 whole-source check slice

`orch-infra::import::preflight` is the check that comes before every write (R112), built read-only and
without a caller. It opens the source `.orchestry/` once without following a link, lists each directory of
the "Import rules" table through the confined-directory module (a bounded listing, so the entry limit is
met before a document is read), refuses a listed entry that is not the kind the table expects before any
of its bytes is read (R103), applies the id rule (R104) and the fold-key rule for task ids and the names of
`workspaces/` (R117), measures every document on the bytes that are then decoded (R109), streams each event
file (R111), and asks of each recorded in-flight run only whether its process or process group exists
(R119, through a probe that a test can replace). It reports at most 1,000 refusals and 1,000 notes, each
path, rule and message escaped and cut, and it runs on a blocking thread that a flag can stop between reads.
It creates, changes and removes nothing. The hostile-document corpus of "Import bounds" runs against it in
a child process under a time and a resident-memory ceiling (`crates/orch-infra/tests/source_budget.rs`).

Report paths are relative to the source `.orchestry/`; the layer that prints a report adds the prefix that
the spec's "relative to the project" asks for. The check takes no lock: whoever calls it holds the 1.0 owner
lock for as long as it relies on an answer (R119 means something only while no 1.0 daemon can rewrite
`state.json`), and stops it by setting its cancel flag and awaiting the result, because dropping the future
does not stop the blocking thread. The YAML decode runs on a runtime of the check's own, so a caller's
blocking pool of one is enough. A task id of `.` or `..` stops the check (R104); template stems are not
judged, because the spec has no rule for them (tracked, not decided here).

Where the check goes beyond the spec's wording, it is stricter and says so: a `running` set of `state.json`
that 1.0 would read as empty because one entry does not decode stops the check, because that set is what
R119 asks about.

Not in this slice: the `config.yml` and `_index.json` classes (C-V1 to C-V14, X-V1 to X-V14), which keep
only the file-kind, byte, I/O and budget stops until the proof of issue 146 is accepted; every write (rows,
blobs, `.orch-project`, the manifest), `orch import` and its dry-run report, `--resume`, the second reading
of each file at the load, and any use of a copy of a live project. The check has no caller outside its tests.

### T21 reader compatibility evidence (issue 146)

The inactive reader now pins ordinary config, blank event lines and damaged indexes
against the real file readers in `import::tests::compatibility`. It retries interrupted
buffered event reads and propagates terminal I/O failures. The document-count rule,
physical gap semantics and index disposition are unchanged. JSON's effective depth
ceiling remains 64, even when a caller requests more.

Before the first runtime caller, the spec owner must reconcile R109's exactly-one-document
rule with the R110 compatibility table: the effective legacy config reader supplies
defaults for missing, empty, space/newline-only, comment-only and explicit-null config;
the bounded reader refuses the three zero-document streams. Explicit null passes the
budget check but is not a typed entity. Multi-document config is refused by both readers.
This evidence does not grant a config exception or amend AP15. See
[the compatibility record](../internal/import-reader-compatibility.md) for the decision
and individually tracked integration conditions. Full T21 still requires its actual
writer, source confinement/preflight, reporting, dry run, resume and acceptance evidence.

### T8c complete retry enumeration follow-up

The landed retry port is extended, without replacing its point/due/write contracts,
by `RetryCursor`, `RetryPage`, `read_retries` and transaction-local `list_retries`.
Both references enumerate all rows, including future rows, ordered by UTC-ms
`due_at` and UTF-8 task bytes in pages of 1..=1,000 on one snapshot. Recovery and
export must follow `next` until absent without deleting rows or capping totals.
For T10 the existing SQL names require `WHERE project_id = ?` and the exclusive
`(due_at, task_id)` key, `ORDER BY due_at, task_id COLLATE BINARY`, fetching limit+1.
No index migration is required by this reference change. The shared retry suite
covers 2,500 rows and a 2,501-row restore containing a whole 65,536-byte UTF-8 error.
The reference's larger-error refusal does not satisfy or weaken whole-text R97;
T10/T12/T21 retain blob mapping under #109. T12 retains first-wins enqueue, T21
legacy invalid-deadline handling, and T14 dispatch/rotation order. The historical
builder's due-only limitation is superseded by the reviewed module contract.

T8 reference acceptance is separate from runtime acceptance: T12 owns Linux
restart/recovery and lease-bound finish, T13 owns Linux sleep/wake fencing and
second-host peer verification, and T23 owns durable restore including a new lineage.
macOS compatibility is deferred to After 2.0 and is not a release gate.

### T10 attention mapping (#183)

Implement the spec's migration-2 facts JSON and numeric TEXT-ID queries as an
internal SQLite module; T10 composes it once into Storage. Verify reopen of both
kinds and disk subjects, scope isolation, bounded pages and strict expiry across
9/10/11. References retain the same scenarios. I2/N1/N2 reference coverage and
N3/N4 consumer follow-ups remain separately accountable under issue 183.
Reference no-op writes are tracked in [#204](https://github.com/yuxhuang/r-orch/issues/204);
T19 observation wording in [#205](https://github.com/yuxhuang/r-orch/issues/205);
T15/T19 clock handling in [#206](https://github.com/yuxhuang/r-orch/issues/206).
[ADR 0019](../decisions/0019-sqlite-storage-attention.md) records the mapping.
