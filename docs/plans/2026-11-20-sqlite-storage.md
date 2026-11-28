# SQLite storage for release 1.1

status: proposed
owner: architect; the lead turns the task table into tasks; the owner approves at the gate after the entry steps
date: 2026-11-20
spec: [../specs/2026-11-20-sqlite-storage.md](../specs/2026-11-20-sqlite-storage.md)
decision: [ADR 0017](../decisions/0017-sqlite-storage.md) (proposed); [ADR 0023](../decisions/0023-unit-of-work-on-the-writer-thread.md) (accepted)

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

A task that grows past about 5,000 changed lines (normally a PR stays under about 4,000) is split in its own PR series
and this table is edited in the first of them. "Suite" is `cargo test -p orch-engine --test storage_conformance`.

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
| T10 | SQLite adapter of port v2: the writer with lanes, the read pool, intents | R7 to R15, R24 to R28, R98, R126 (the backlog kept from the commit callback and the checkpoint's result), R127, R128 (with the failed top-level rollback), R130 (the query connection and its authorizer), R137 (the outcome of a checkpoint call), R140 (a failure on any connection), R97 (SQLite row hash and blob bytes) | `crates/orch-infra/src/sqlite/` | T8, T9 | Suite (SQLite): `cargo test -p orch-engine --test storage_conformance -- sqlite_` (the eight registrations, 135 cases; require 135 passed, 0 failed, 0 ignored); the writer, pool and storage library tests, `cargo test -p orch-infra --lib -- sqlite::writer:: sqlite::read_pool:: sqlite::storage::` (require a nonzero count, 0 failed, 0 ignored); the integration targets `sqlite_connection`, `sqlite_crash`, `sqlite_storage_crash`, `sqlite_public_writer` and `sqlite_writer_policy`, each `cargo test -p orch-infra --test <target>` (a crash target's one `ignored` test is its private child process, which the other cases run). Counts at `56ecd762e` are in "T10 verification selections" below | 9 |
| T11 | Blob store on disk, on the confined-directory module: a blob is reached only by a parsed hash; prompts by hash; attachments as blobs | R58, R59, R101, R102 (blobs) | `crates/orch-core/src/store/blob.rs`, `crates/orch-infra/src/blob/` | T9 | `cargo test -p orch-infra --test blob_store` | 10 |
| T12 | Engine on port v2, part 1: claims, leases, retries, start and finish a run; the workspace found by task id and the branch by its derived name, never by a stored path; workspace ownership: a directory is reused or entered, and a branch merged, only when a listing holds it under exactly the task's name, a new workspace is an exclusive create, and a task id that folds onto an existing one is drawn again; the `orch-peer` facade | R35, R36, R40, R20, R96, R106, R117 (`task.create`), R118 (prepare, reuse, merge), R101 and R129 (the spool is written under the finish file's name and removed after the commit, for any run id: the finish-file vectors of the spec's "Filesystem names", as a name and through both operations, with an outside canary; and what happens when the spool cannot be written), R132 (an earlier lineage is refused and changes no row) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/peer_facade*`, `crates/orch-infra/src/process/`, `crates/orch-infra/src/workspace/`, `crates/orch-infra/src/data_dir/spool*` (the finish spool, on the confined-directory module of T9), `crates/orch-infra/tests/fs_names.rs` | T8, T10 | `cargo test -p orch-engine --test transactions --test leases --test retries --test task_statuses --test task_ids`; `cargo test -p orch-infra workspace`; `cargo test -p orch-infra --test fs_names`; `cargo test -p orch-peer` | 11 |
| T13 | Engine on port v2, part 2: cancel, pause, failure cascade; generations and recovery; the intent performer, whose intents hold ids and hashes and never a path, and whose `workspace.remove` and branch delete leave a directory or a branch that a listing holds under another spelling | R37 to R39, R41, R42, R28, R106 (intents), R118 (remove, delete, cleanup), R120, R101 and R129 (recovery applies the spool and removes it, reaching a file only under the finish file's name, for any run id: the same vectors and canary) | `crates/orch-engine/src/orchestrator/`, `crates/orch-engine/src/intents*`, `crates/orch-infra/src/data_dir/spool*` (the read and the removal at recovery) | T12, T4c | `cargo test -p orch-engine --test transactions --test intents --test leases` | 12 |
| T14 | Scheduler port with today's scheduler behind it; `metrics.get` with tick time and open tasks | R55 to R57 | `crates/orch-core/src/store/scheduler.rs`, `crates/orch-engine/src/scheduler*`, `crates/orch-daemon/src/ops/` | T12 | `cargo test -p orch-engine --test scheduler --test order_conformance`; `cargo test -p orch-daemon --test ops_metrics` | 13 |
| T15 | Maintenance loop and its jobs; saved dispatch state | R43, R44, R60, R126 and R127 (the checkpoint job, its two triggers on time and on backlog, and the backlog's ceiling), R137 (which calls move the timer), R139 (the order and the bound of a stop), R140 (the reports for failing checkpoints and a backlog that stays) | `crates/orch-engine/src/maintenance*`, `crates/orch-daemon/src/lifecycle.rs` | T13 | `cargo test -p orch-engine --test maintenance`; `cargo test -p orch-daemon --test daemon_dispatch --test daemon_storage_failure` | 14 |
| T16 | Run events through the port: numbering, batches, gaps, tail reads, pruning, stored in SQLite as rows, by the batched design of the spec's "Writer lanes" (spec R63). No event fallback, no event file and no setting for another event path is built, and nothing is measured as a condition of this PR (the owner's decision of 2026-11-21, the spec's record I). No test of an event file is due in 1.1, the test of its name included: the run-id vectors of the spec's "Filesystem names" are proved on the finish file by T12 and T13 (spec R101, R129). The wiring builds one `EventProducer` per writer and meets the six obligations of "One retained producer per writer" (issue 448); a second retained producer needs a new design first. As it stood until 2026-11-21: Run events through the port: numbering, batches, gaps, tail reads, pruning; the path that the owner's ruling on T4b's result selects, SQLite rows or the file fallback, measured as built by the method of the spec's "Event measurement" before this PR merges; if the fallback, each event file named by the whole hash of the spec's "Filesystem names" and reached through the confined-directory module on append, read, tail read and prune | R14, R26 (one event transaction for all runs), R61 to R63, R139 (events handed over and not committed when a stop ends) | `crates/orch-engine/src/orchestrator/collect*`, `crates/orch-infra/src/sqlite/events*` | T10, T11, T13 | `cargo test -p orch-engine --test run_events --test maintenance` | 15 |
| T17 | Protocol 3: revisions, paging, request keys, the four error codes; client resend; CLI list flags | R45 to R51, R134 | `crates/orch-proto/`, `crates/orch-daemon/src/ops/`, `crates/orch-client/`, `crates/orch-cli/` | T12 | `cargo test -p orch-proto`; `cargo test -p orch-daemon --test ops_request_keys --test ops_revisions --test ops_paging --test daemon_wire`; `cargo test -p orch-cli --test list_paging` | 16 |
| T18 | Authorization matrix in the dispatcher, with test principals | R52 to R54, R93, R133 (the operations) | `crates/orch-daemon/src/ops/`, `crates/orch-proto/src/ops/` | T8, T17 | `cargo test -p orch-daemon --test ops_authorization`; `./xtask/gate.sh full` (security-relevant) | 17 |
| T19 | Attention queue with its two kinds; the disk floor | R81 to R85, R131 | `crates/orch-engine/src/attention*`, `crates/orch-engine/src/disk_floor*`, `crates/orch-cli/` | T15, T17 | `cargo test -p orch-engine --test attention --test disk_floor`; `cargo test -p orch-cli --test attention` | 18 |
| T20 | Usage for every run, with the model and the agent CLI version | R86, R87 | `crates/orch-engine/src/orchestrator/`, `crates/orch-infra/src/adapters/` | T12 | `cargo test -p orch-engine --test usage` | 19 |
| T21a | The seeded fixture generator split out of T21 as a preparatory slice: `cargo xtask fixture-gen` scripts the real 1.0 file backend in a private temporary directory from a fixed seed, with a deterministic clock and ids; `--check` compares exact file sets and bytes with the checked-in `fixtures/storage/generated/` and changes nothing. One small scenario is checked in; large corpora for R66 and the importer's results stay in T21 | R90 | `xtask/src/fixture_gen*`, `xtask/src/main.rs`, `xtask/Cargo.toml`, `xtask/src/crate_graph.rs`, `xtask/tests/fixture_gen.rs`, `fixtures/storage/generated/` | T2 | `cargo xtask fixture-gen --check`; `cargo test -p xtask --test fixture_gen` | 20 (first slice) |
| T21 | `orch import` from a 1.0 directory: dry run, bounds, resume; the source read by listing through the confined-directory module, the id rule, and nothing done outside the database; the seeded fixture generator's further scenarios (T21a owns `xtask/src/fixture_gen.rs` and its first scenario; T21 extends it) and the hand-written hostile-source corpus (links, special files, traversal-shaped ids and names, a live canary process, a canary child left in the process group of a leader that has gone, two task ids that differ only in case, and a workspace with a canary under another spelling of a task's id). The check, before the first write, that no two task ids of the source have one fold key and that no name listed in `workspaces/` folds onto a task's id without being it; `workspaces/` is listed for its names alone. The source-budget pass of the spec's "Import bounds": every document measured for bytes, depth, nodes, aliases, expansion and tags before anything is built from it, on 1.0's own guarded reader (`orch_core::definition_yaml`) where that already holds the limit; event lines read as a bounded stream; the check of the whole source before the first write; and the hostile-document corpus (the review's depth and alias documents, merge-key bombs, tags, each limit and one over), run in a child process under a time and a memory ceiling. The six cases of `config.yml` and the classes of an `_index.json` that the spec's "Import rules" tabulate, with the statements of what 1.0 did with a refused file and the vectors C-V1 to C-V14 and X-V1 to X-V14 as tests; neither path gets a caller before the owner has merged the contract text that carries them ("T21 config and index contract") | R64 to R70, R90, R99, R103 to R105, R109 to R112, R116 (a 1.0 source), R117 (a 1.0 source), R119, R121 (the record), R122, R123, R125, R97 (imported text over 64 KiB) | `crates/orch-infra/src/import/`, `crates/orch-cli/src/commands/import*`, `xtask/src/fixture_gen.rs`, `fixtures/storage/`, `crates/orch-infra/tests/source_budget.rs`, `crates/orch-cli/tests/import_hostile.rs` | T10, T11, T2 | `cargo test -p orch-cli --test import --test import_hostile`; `cargo test -p orch-infra --test source_budget`; `cargo xtask fixture-gen --check` | 20 |
| T22 | `orch db` with the daemon running: status, verify (including that every `task_epochs.next_epoch` is at least 1), query, prune | R72 to R74, R79, R121 (status), R130 | `crates/orch-daemon/src/ops/`, `crates/orch-cli/src/commands/db*` | T10, T16, T17 | `cargo test -p orch-cli --test db_commands` | 21 |
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
passing tests, zero failed and zero ignored. Since T10 (issue 450) the expected registrations include the SQLite ones:
the spec's "The eight SQLite registrations of the conformance suite" lists them, and
`cargo test -p orch-engine --test storage_conformance -- sqlite_` selects all of them (135 cases at `56ecd762e`; 133 when #385 registered them, 135 since #454 added two lease cases). Cargo exits successfully for zero
matches; that is a failed verification. `--exact` requires the full listed test
name, for example `file_leases_v2::one_of_many_concurrent_claims_wins`, and must
execute exactly one test. Do not prefix a filter with `storage_conformance::`.

| Requirements | Bare filters | Expected registrations per filter |
|---|---|---|
| R8, R11 | `revisions_and_positions_for_every_entity_family`, `stale_revision_is_refused_and_changes_nothing`, `deletion_never_reuses_a_position` | 4: `memory_v2`, `queued_memory_v2`, `file_v2`, `sqlite_v2` |
| R9 | `page_bounds_defaults_and_optional_count` | Same 4 entity registrations |
| R10 | `cursor_walk_is_stable_under_inserts_and_deletes`, `cursor_is_bound_to_project_filter_and_sort` | Same 4 entity registrations |
| R12 | `one_of_many_concurrent_claims_wins`, `an_agent_holds_at_most_one_active_lease` | 3: `memory_leases_v2`, `file_leases_v2`, `sqlite_leases_v2` |
| R13 renew/release | `epochs_only_grow`, `a_stale_epoch_is_refused`, `release_ends_the_lease_and_keeps_the_epoch_counter` | Same 3 lease registrations |
| R13 append, R14 | `stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing`, `duplicate_counts_first_bytes_and_range_tail_order`, `transaction_error_rolls_back_append_gap_prune_and_entities` | 3: `memory_events_v2`, `file_events_v2`, `sqlite_events_v2` |
| R15 | `caller_selects_the_project_and_keeps_the_principal`, `settings_and_templates_belong_to_their_project` | Same 4 entity registrations |
| R97 whole text | `text_over_64_kib_round_trips_through_the_blob_store` | Same 4 entity registrations; this alone cannot observe blob backing |

The Task cursor exception of R10 (issue 277, option A) is proved by two `orch-infra`
unit tests, not by the conformance filters above, so they have no registrations in
`storage_conformance`. Each selection must execute one passing test:

| Requirement | Exact selection (append `--exact`) | Proves |
|---|---|---|
| R10 refusal | `cargo test -p orch-infra --lib -- sqlite::task_adapter::tests::a_deleted_row_is_resumed_at_the_shortest_fallback_and_without_one_it_is_refused` | A deleted anchor resumes at its shortest fallback (past the keys, from a prefix of the next row, or from the next row itself); with the anchor and its successor both deleted, a long-ID cursor whose IDs part beyond the carried prefix is refused as `InvalidCursor`, never resumed at a guessed position, and the surviving rows stay listed |
| R10 continuation | `cargo test -p orch-infra --lib -- sqlite::task_adapter::tests::a_walk_returns_every_row_once_when_the_row_each_page_ends_on_is_deleted` | For every Task sort, with tied and untied integer keys, a walk that deletes the row each page ends on (or the whole page) before the next page returns every remaining row exactly once, in order |

These two names cover only the Task cursor on SQLite. Every other obligation of
R10, including the filter and sort binding and the walk for the other entity
families, stays with the bare filters in the R10 row above. The cursors of the
eight families other than Task (R10 as the owner's rulings of 2026-11-26 changed it)
are proved by the vectors of "Non-Task cursors" below, which are named tests of the
page PR and are not built yet: they have no registration and no result.

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
T10 registered SQLite in PR #385 (squash `964e1144a`); the change that closes issue 450
updates this table's expected registrations: the entity, event and lease rows above count `sqlite_v2`, `sqlite_events_v2`
and `sqlite_leases_v2`, and the other five suites (operational, intent, request key, retry, attention) are
registered on SQLite as well. T10 must still assert
actual SQLite row hashes plus disk bytes for R97. Separate SQLite unit scenarios
are useful but do not replace those registrations. T12 owns stale-epoch finish
and runtime restart/renewal proof; T16 consumes SQLite event conformance. T21 owns
R97 import backing (the proposed import test does not exist yet). T27 records
those actual named checks and rejects empty or ignored selections. T25 retains
the file reference as a dev-only dependency. The name alignment for R7, R117
and the caller part of R132, the frozen-trait record and the pending-message,
run-order and retry-text decisions belong to issue 109 and are in "T8 consumer
contracts (issue 109)" below; this name reconciliation changes none of them. Full
T8 acceptance still requires the accepted operational integration and issue
109's consumer dispositions. macOS is tracked in After 2.0 under the owner's
Linux-only direction through 2.0.

### T10 verification selections (issue 450)

The T10 row's "Verify" column names these selections. Each one was listed and run on Linux at `56ecd762e` (code of `main`
after #480; the documentation change on top touches no code) and executed a nonzero number of tests with 0 failed. A selection
that matches zero tests or ignores one fails verification, as in the crosswalk above. `sqlite_writer` is not a test target
(its earlier appearances as one, in the T10 row, in R24's test cell and in the foundation note, were corrected under issue 450); the writer's tests are library tests.

| Selection (all `cargo test --locked`) | Requirements | Result at `56ecd762e` |
|---|---|---|
| `-p orch-engine --test storage_conformance -- sqlite_` | R7 to R15 and the operational port on SQLite (the eight registrations) | 135 passed, 0 failed, 0 ignored (346 filtered out); `--list` names the same 135 (481 in the whole target) |
| `-p orch-infra --test sqlite_connection -- read_connections_cannot_write --exact` | R24 | 1 passed |
| `-p orch-infra --lib -- sqlite::writer::tests::writes_run_one_at_a_time_on_one_connection --exact` | R24 | 1 passed (it runs its body in a child process and waits for it) |
| `-p orch-infra --lib -- sqlite::read_pool::tests::a_read_snapshot_does_not_see_a_later_commit --exact` | R25 | 1 passed |
| `-p orch-infra --lib -- sqlite::writer::tests::claims_precede_queued_work_and_other_lanes_use_deadlines --exact` | R26 (the order only; four of the five names the spec listed bear no test) | 1 passed |
| `-p orch-infra --lib -- sqlite::read_pool::tests::exhaustion_waits_until_deadline_and_never_opens_an_extra_reader --exact` and `... a_waiting_reader_uses_the_released_slot_before_its_deadline --exact` | R98 | 1 passed each |
| `-p orch-infra --lib -- sqlite::writer::` | writer foundation, lanes, retained writes | 62 passed |
| `-p orch-infra --lib -- sqlite::read_pool::` | R25, R98 | 13 passed |
| `-p orch-infra --lib -- sqlite::storage::` | the composed `Storage` adapter, containment, prior latch, task cleanup | 95 passed |
| `-p orch-infra --test sqlite_connection` | connection settings, read-only readers, private files; R24 | 8 passed |
| `-p orch-infra --test sqlite_writer_policy` | R128 | 23 passed |
| `-p orch-infra --test sqlite_public_writer` | R128, the public writer | 11 passed |
| `-p orch-infra --test sqlite_crash` | process kill of the production writer at statement, body-return and receipt boundaries, on synthetic tables | 3 passed, 1 ignored (the private child process, `writer_crash_child`, which the three cases start) |
| `-p orch-infra --test sqlite_storage_crash` | process kill through the composed `Storage` at callback and receipt boundaries, on synthetic config and template rows only (its header) | 5 passed, 1 ignored (`storage_crash_child`, likewise) |

Neither crash target simulates an interruption inside `COMMIT` or a power loss (both headers say so). The whole-adapter review
found no process-kill test through `Storage` for any family other than config and templates, nor for events, leases or request
keys (issue 450). Issue 149, which asked for the crash coverage of the combined adapter, was closed by PR #442 with the callback,
cleanup and receipt boundaries of "Crash points" and "The callback boundary" below; PR #442 added no such kill, so issue 149 does not
own them. They stay a residual of the T10 acceptance and are carried by the crash gate of T27, which must kill a process through
`Storage` for each of the three families or record why it does not. A passing selection above is a result of that selection;
it is not the T10 acceptance, which stays with its own task.

### T8 consumer contracts (issue 109)

[ADR 0020](../decisions/0020-storage-consumer-contracts.md) records the decisions, and the spec's "T8 consumer-contract
amendment (issue 109)" states the contract. They add `Filter::PendingMessages { recipient, now }`,
`Sort::Attempt` (per task only) and whole `Retry.error` text through the blob store (as ordered chunks above one 64 MiB
object), and
`docs/TRAITS-FROZEN.md` records the landed v2 signatures beside them. `main` at `07b5f967` has none of the three. The
change that carries this section implements them on the memory fake and the file reference and registers the shared
tests on both; the SQLite representation and every runtime caller stay with the owners in the table at the end of
this section. A test result is a receipt of that change's PR, not a statement of this plan.

**Verification names for R7, R117 and the caller part of R132.** Use the same procedure as the crosswalk above: list
the bare filter, require every registration, run it, and require that many passing tests with none failed or
ignored. Each of the four entity registrations is `memory_v2`, `queued_memory_v2`, `file_v2` and `sqlite_v2` (the first three when this section was written; #385 added `sqlite_v2`).

| Requirement | Bare filters (actual test names) | Expected registrations per filter |
|---|---|---|
| R7 | `unit_of_work_keeps_nothing_after_an_error`, `a_failed_unit_of_work_keeps_no_setting_or_template_row_or_position` | 4 entity references |
| R7, writer process crash | `cargo test -p orch-infra --test sqlite_crash -- kill_before_commit_keeps_nothing` (also `kill_at_body_return_commit_boundary_keeps_nothing` and `kill_after_commit_receipt_keeps_all_changes`) | one test each; this is the production writer's process-crash boundary, not the SQLite adapter's port-level run, which T10 registers |
| R117 | `two_task_ids_that_differ_only_in_case_are_refused`, `task_ascii_aliases_are_refused_without_rewriting_ids`, `task_lookups_stay_exact_to_the_byte`, `only_task_ids_fold_and_only_the_ascii_letters` | 4 entity references |
| R117, schema | `cargo test -p orch-infra --test sqlite_schema -- task_ids_are_unique_under_ascii_case_only_within_a_project` | one test |
| R132, caller fields | `caller_selects_the_project_and_keeps_the_principal` (a task principal's lineage and epoch come back unchanged from both entry points) | 4 entity references |
| Consumer contracts (ADR 0020 decisions 1 and 2) | `pending_messages_filter_status_recipient_and_lexical_expiry`, `pending_messages_page_matches_before_count_cursor_and_bound`, `run_attempt_order_is_numeric_task_scoped_and_keeps_default`, `run_attempt_pages_keep_snapshot_and_bind_every_query_field`, `run_attempt_sort_refuses_every_unscoped_or_other_family_filter` | 4 entity references each, 20 in all |
| Whole retry text, conformance (decisions 3 and 4) | `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run`, `retry_whole_text_replacement_rolls_back_and_snapshots_keep_original`, `retry_chunked_text_survives_restore_without_a_run` | 4 retry registrations each: `memory_retries_v2`, `blob_memory_retries_v2`, `file_retries_v2`, `sqlite_retries_v2` (the SQLite one since #385; 3 when the row was written) |

The importer-side R117 rule has an existing unit test in `orch-infra`'s library:
`import::preflight::tests::two_task_ids_that_differ_only_in_case_are_refused_before_any_write`. The command-level import, export and restore tests that R117 names, and the lineage tests
of R132 (`leases`, `restore_fencing`), do not exist yet and stay with T21, T12, T13 and T23.

The R7, R117 and R132 rows above are the 12 selections the integration task lists (`--list`) and then runs, each
required to select its expected count with none failed or ignored: the seven conformance filters (3 registrations
each when they ran; #385 later added a SQLite registration to every shared suite), the three `sqlite_crash` selections, the `sqlite_schema` selection and the importer's unit test (one test
each). The integration task ran them once on Linux, before the head was frozen, and each selected its expected count
and exited 0; the receipt is private to the task, the PR's manifest names the head it belongs to, and a reviewer or
the gate repeats them there. The spellings were also checked against the test sources and against a listing of the
conformance suite on `07b5f967`. A selection that matches zero tests, or ignores one, fails verification.

The last row of the table above is three scenarios of the retry suite. The suite has 21 scenarios on each of its registrations: 63 tests on the three that existed when this was written, and 84 with `sqlite_retries_v2`, which #385 added; the three new ones, `retry_chunked_text_survives_restore_without_a_run`,
`retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run` and
`retry_whole_text_replacement_rolls_back_and_snapshots_keep_original`, are the first three it registers in
`storage_contract/retries_v2.rs`, and `blob_memory_retries_v2` is a registration that did not exist on `main`. The
#130 crosswalk table above does not list the retry suite, so its inventory is unchanged.

**Retry text above one blob object (ADR 0020 decision 4).** The reference implementation meets this mapping and is
accepted only with these focused Linux checks on both references. Each check is one named test; the integration task
records its commit, count and platform, and an empty or ignored selection fails it. The tests are in two places:
the retry suite of `storage_conformance` (`-p orch-engine --test storage_conformance -- <filter>`, 4 registrations
each since #385, 3 when this was written), the `orch-infra` library (`-p orch-infra --lib -- storage::v2::tests::<name> --exact`, which must execute
one test) and one private unit test of `orch-core`
(`-p orch-core --features test-support --lib -- store::retries::fake::tests::reordered_retry_chunks_fail_every_memory_retry_read --exact`,
which must execute one test; the module exists only with the `test-support` feature). "Both" in the last column
means the file reference and the disk-backed fake. The memory fake is *logical* by default (whole strings) and
*disk-backed* with `FakeStorage::with_retry_blob_store`; a check about publication or damage is carried only by the
disk-backed form and the file reference.

| Check | What it shows | Test | References |
|---|---|---|---|
| 65,536 and 65,537 bytes | Inline, then one blob object named by the row's hash | `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run` (conformance); `retry_text_boundary_is_backed_before_the_hash_row_and_reopens_whole` (backing) | the 4 retry registrations; file only |
| 67,108,864 and 67,108,865 bytes | One object at the limit; chunks and a whole-value hash one byte above it | `retry_object_limit_is_not_a_diagnostic_cap_and_chunk_order_is_verified` | both |
| A character split across a chunk boundary | The joined bytes decode only after the whole-value hash matches | same test (the 67,108,865-byte text ends in a two-byte character cut by the boundary) | both |
| A retry with no run | The whole text is the retry's own, never the run's | `retry_whole_text_round_trips_at_the_utf8_boundary_without_a_run` | the 4 retry registrations |
| A text above one blob object, then a restore | A fixture lineage switch (`restore_to_lineage`: leases end, counters stay) leaves the 67,108,865-byte text whole on `read_retry`, `read_due_retries` and a complete walk; it is not a backup copy | `retry_chunked_text_survives_restore_without_a_run` | the 4 retry registrations (the logical fake keeps a whole string) |
| Failure on a later chunk after an earlier chunk was published | The previous row reads back whole, also after the caller catches the error and commits other work | `retry_later_chunk_failure_keeps_prior_row_and_earlier_published_bytes` | both |
| A publication refused or damaged at read-back | The previous row stays whole when the caller catches the error and commits | `retry_publication_refusal_preserves_prior_rows_even_when_caught_on_both_references` | both |
| A missing and a corrupt blob after commit | An error on `read_retry`, `read_due_retries`, `read_retries`, `get_retry`, `due_retries` and `list_retries`; the document is unchanged | `retry_missing_and_corrupt_blobs_fail_all_read_paths_on_both_references` | both |
| A missing and a corrupt chunk of a text above one object | The tail chunk damaged, then removed: an error on the same six reads each time, with the file restored between | `retry_object_limit_is_not_a_diagnostic_cap_and_chunk_order_is_verified` | both |
| A reordered chunk mapping | An error ("reconstructed retry does not hash") on the same six reads when two chunk entries of the file reference's document are swapped; for the disk-backed fake the private mapping is swapped and `get`, `due` and `page` of its private retry state, which every port read calls, each fail | the object-limit test (file reference only); `store::retries::fake::tests::reordered_retry_chunks_fail_every_memory_retry_read` (disk-backed fake's private state, with a reversible text-name double for its blob store: it proves the whole-value hash guard, not hashing or damage) | as stated |
| Identical chunks at two positions | The mapping keeps both positions (the file reference's has three entries) | `retry_repeated_chunks_reconstruct_every_occurrence_on_both_references` | both |
| Replacement, long and short | A rolled-back replacement and a held snapshot keep the original; then long to short and short to long each read back | `retry_whole_text_replacement_rolls_back_and_snapshots_keep_original` | the 4 retry registrations |
| Rollback of a unit that published | The earlier row stays, the rolled-back task reads as none, and the published 65,537-byte object is still on disk at its hash path with no row naming it (the sweep is not exercised) | `retry_rollback_keeps_published_bytes_without_a_reference_on_both_backed_references` | both |
| An existing single-object row | A hand-made `{"sha256": H}` row (65,538 bytes of two-byte characters; no earlier release wrote one) reads whole on `read_retry`, `read_due_retries` and `read_retries` after a reopen, the reads leave the document's bytes unchanged, an extra field in the request-key collection wrapper does not prevent the reopen, and a stored request key still replays | `retry_existing_single_object_encoding_and_wrapper_extensions_remain_readable` | file only |
| Selection before resolution | A healthy row plus a damaged row (blob removed) with a later deadline or past the limit: due reads at a fixed `now` and at limit 1 and the first page succeed on the snapshot and the unit of work; the damaged row's point read and the page that reaches it fail; on a current-thread runtime a blob-store guard fails a snapshot read that reads a blob on the executor thread (the three async reads run on the blocking boundary) | `retry_selection_does_not_resolve_excluded_damaged_rows_on_both_references` | both |
| An unknown field in a stored text | A hand-edited single or chunked mapping with an extra field fails the document's load as a parse error and its bytes are unchanged | `retry_stored_text_unknown_fields_are_refused_for_single_and_chunked_objects` | file only |
| Physical backing | The objects exist, and the file reference's document holds `{"sha256": H}` or `{"sha256": H, "chunks": [...]}`; the disk-backed fake's objects are read from the blob store, its mapping is private | the boundary, object-limit, later-chunk and repeated-chunk tests above | as stated |

Selections and expected counts. Each conformance filter above must select 3 tests and each `-- <name> --exact`
selection of `orch-infra` or `orch-core` exactly 1, with none failed or ignored. When the whole suites are run at the
head that completes this change, the expected totals are 332 tests for `storage_conformance` (the retry suite is 63 of
them) and 55 for `cargo test -p orch-infra --lib -- storage::v2::tests`; a later test changes these numbers. These are
the numbers to expect, not results: a result is a receipt of the PR, which names its head, and the runs of a head
that was not the final one prove nothing about it.

Not proved by any test of this change: the SQLite representation (T10); the blob store's sweep (the rollback test
shows that published bytes survive, not that the sweep later removes them, and its object is a single object, not a
chunk); a missing or corrupt chunk on the logical fake, which has none; a crash between publication and commit; a
restore that copies files, because the restore of the chunked scenario is the fixture's lineage switch (T23); and a 1.0
import (T21). The single-object row is a documented-form fixture and not a row an earlier release wrote. The executor guard covers the
three async snapshot reads only; it says nothing of a daemon path. Mutation checks to run on the final head, whose results
are the PR's receipts: resolving every row before the filter and the limit in either reference fails the selection test;
removing the unknown-field refusal of the stored mapping fails the unknown-field test; calling a blocking retry read
directly in an async method fails the guard.

This is reference-port proof only. It gives no measurement above 64 MiB beyond these boundaries and accepts none of
T10, T12, T21 or T23. T10 repeats the physical checks on SQLite. T21 rebuilds an imported 1.0 error into the same
representation. T23 carries every reachable chunk, in order, with the whole-value hash, through backup, restore,
export and import. The memory fake comes in two forms because `orch-core` cannot depend on `orch-infra`: the default
`FakeStorage` is logical (whole strings, no blob, no claim about backing or publication), and
`FakeStorage::with_retry_blob_store` takes the real blob port (the disk adapter in the conformance fixture) so that
publication, a missing blob and a corrupt blob can be exercised. The logical fake proves the port's logical rules;
only the disk-backed fake and the file reference prove publication and damage.

**What each consumer keeps.** The reference tests prove the port, not the runtime, and no row below is accepted by
them.

| Owner | Obligation | Source |
|---|---|---|
| T10 | SQLite representation: the retry blob and chunk mapping (publish first, whole text, whole-value hash and ordered chunks above 64 MiB, unchanged row on a missing or corrupt blob or chunk), SQLite parity for the physical checks above, the pending-message statement (`status`, exact recipient, `expires_at IS NULL OR expires_at >= ?now`, text comparison, filter before count and cursor), an index for `Sort::Attempt` (attempt descending, id ascending; it also serves the default of a run list whose filter names a task, which is the same key; the existing task page helper under `crates/orch-infra/src/sqlite/` handles tasks only and answers `Sort::Attempt` with `InvalidPage`), adapter registration of the retry, message and run scenarios (the five consumer scenarios and the three retry scenarios), the full-scope tie order of `delete_expired_request_keys` for rows that share an expiry (equal-expiry rows are not ordered by any reference test; tracked in issue 127), and an independent due-read test: after the enlarged complete-read restore, compare the first 1,000 expected rows of a due read at `i64::MAX` through its own SQL statement (the reference queries share a loader, so the walk does not prove it). Update the crosswalk's expected registrations when SQLite joins | ADR 0020; issue 109 (N-a); issue 127 (PR 171 N1); crosswalk |
| T17, T18 | Derive a principal's kind from its persisted identity: the request-key scope has no kind field, so an owner and a worker session of one identity and project share a scope and a task principal differs from them only by its run and epoch | The spec's request-keys amendment (issue 109, PR 171 N4) |
| T12 | `put_retry` after `get_retry` (1.0 enqueue was first-wins) in one unit of work (the shared scenario only models two enqueue calls with that guard); a finish that writes the retry row, the task status and the lease release together; lease-bound finish and restart fencing (R36, R132, run through issue 110); a finish of a long error whole | ADR 0020; R35, R36 |
| T13 | Generations and recovery (R41) and the Linux sleep and wake fencing check (R42, issue 110, pending) | R41, R42 |
| T14 | `retry_pending` gating per task; dispatch in candidate order and agent rotation, never due-row order | retries module |
| T21 | A 1.0 retry deadline that does not parse becomes due, and the dry run counts it; the whole 1.0 error is imported (R97), rebuilt into the same chunk representation when it exceeds one blob object | R65, R69, R97 |
| T23 | Durable restore with a new lineage, with the open point AP14 (a backup's workspace names); a whole retry export and import round trip including blob-backed text and every reachable chunk, its order and the whole-value hash, through backup, restore, export and import | R117, R132, R138 |
| T27 | The second-host peer check on Linux (R89) and the recorded named checks of the crosswalk, rejecting an empty or ignored selection | R89 |

Follow-ups kept with their owners from the review of #191 (none is a contract waiver): N-a above (T10), N-b the
move of the retry entry into `CHANGELOG.md`'s existing "Storage groundwork" section with the statement that no daemon
consumer is activated (not done in this change: a pull request no longer edits `CHANGELOG.md`, so it stays
open for the release PR's hand edit of the legacy `[Unreleased]` text; this change adds no fragment, because the inactive port contracts
change nothing a user can notice), N-c `docs/ARCHITECTURE.md`'s store module inventory names `blob` (done in this change).

Request-key follow-ups of PR 171 (issue 109 comment of 2026-11-23), dispositions in the spec's request-keys amendment:
N1, the tie order of rows with equal expiry on SQLite, stays with T10 and issue 127. N2 is met in the reference: the
persisted epoch ceiling is the `i64::MAX` that `RequestScope::of` applies, and the collection wrapper ignores an unknown
field while a row denies one. N3 is a writer's note, not a change: the persisted check needs the one-hour TTL, so a
change of `KEY_TTL_MS` migrates or reconciles earlier rows in the same change. N4 is met by a corrected test comment
and the T17 and T18 obligation above. N5 is met: a negative time is refused on the lookup path as on the store path.

### Reference order

The default orders of runs and goals are settled in the spec's "Run and goal default order": a run list whose filter
names a task (alone or beside an agent) is ordered by numeric `attempt` descending, every other run list by `started_at`
text descending, then the id; a goal list by status rank, presence of `updated_at` (a present empty text before an
absent one), the original `updated_at` text descending, `position`, then the id.

PR #225 (squash `af0d43acd`, 2026-11-23) carried the reference code and the first text of this paragraph together. The
memory fake, its queued variant and the file reference meet the orders: the shared scenarios
`message_and_run_filters_stay_within_their_family` and `declared_secondary_sorts_use_values_then_tie_breakers` are
registered once and run as `memory_v2`, `queued_memory_v2` and `file_v2` (three registrations each at that squash; `sqlite_v2` joined them with #385), and hosted Linux
check and test succeeded on the head of PR #225 and on the cumulative `main` push. The goal rule changed code in the
file reference only, because the memory fake already told a present empty `updated_at` from an absent one.

The explicit sort `Sort::Attempt` (T8 consumer contracts, above) is a separate addition of the change that carries this
revision, not of PR #225. For a run list whose filter names a task it orders by the same key as the default, so the two
list the same rows in the same order; with no task it is refused and the default is the `started_at` key.

At that squash none of this showed SQLite. T10 registered the same two scenarios, with the five consumer scenarios, on SQLite
(`sqlite_v2`, #385); the orders are shown there by those registrations passing (all eight pass on Linux at `56ecd762e`: 135 of 135, spec "The eight SQLite registrations"), a result of the suite and not an acceptance of T10.

The two shared scenarios are `message_and_run_filters_stay_within_their_family` and
`declared_secondary_sorts_use_values_then_tie_breakers`, in
`crates/orch-engine/tests/storage_contract/v2_paging.rs`. Each is registered under `memory_v2`, `queued_memory_v2`
and `file_v2` and is run by the crosswalk's rule (list, then run with
`cargo test -p orch-engine --test storage_conformance -- <filter>`, three passing registrations each at the time of PR #225).
T10 registered both on SQLite after that PR landed (`sqlite_v2`, #385), so each now has four passing registrations, and the
crosswalk's table counts them (issue 450).

### Non-Task cursors

The owner's rulings of 2026-11-26 (the spec's "Cursors of the eight families other than Task" under "Storage port v2",
which quotes them; [ADR 0024](../decisions/0024-non-task-cursors.md)) settle how Agent, Team, Run, Message, Context,
Config, Template and Goal lists page when the anchor does not fit a 512-byte token. The contract is written. Steps 1 to 4
are on `main`, steps 2 to 4 private and unwired there (no SQLite list on `main` returns a non-Task cursor). Step 5's wire is
in the draft composition PR 385 (`sqlite/storage/page.rs`); its named vectors, mutants and physical corpus are not claimed.
Task keeps its cursor and the issue 277 exception.

| Step | What | Owner task | Verify |
|---|---|---|---|
| 1 | This documentation: R10, the `Cursor` row, the spec section with its vectors, ADR 0024 and this section. It lands before any public paging and wires nothing | the documentation writer | Links resolve; `cargo xtask crate-graph`; `cargo test -p orch-infra --test import_index_reference`; `./xtask/gate.sh fast --base origin/main` (not qualifying) |
| 2 | The inline arm, inactive: encode and decode the token, expiry and binding, the capacity table of the spec confirmed against the encoder (a difference is a spec edit in the same PR). **Built** in PR #376 (`78846fed8`, `crates/orch-infra/src/sqlite/non_task_cursor`); the nine capacities are confirmed and the spec needed no correction | T10 | Vectors C7, P-B1 (inline half), I-T1 to I-T4, Q7′, P-S2, P-X2, C3 (inline kind), P-X3, tested at the module's own interface (22 tests in `cargo test -p orch-infra --lib non_task_cursor`); no page exercises them |
| 3 | A key-taking entry in the three leaves (`sqlite::agent_team_run`, `sqlite::cctg`, `sqlite::retry_message::message`) that resumes after an ordering tuple and does not check the anchor against the filter; the spec's as-built note for Context, Config, Template and Goal is edited when it lands. **Built** in PR #377 (`110002d86`) as `select_after` beside the unchanged record-taking `select`; the spec's as-built notes now say so | T10 | P-F1, P-F2, Q8, N3a, N3b: the **leaf half** of P-F1, P-F2, Q8 and N3b only (resume after the original tuple, anchor out of the filter, in the selecting view); N3a is not tested; each is due in full at step 5 (issue 370) |
| 4 | The adapter's table of retained entries and entries by lookup: bounds, nonce, monotonic age injected at construction, poison-recovering lock, the unfiltered identity scan with UTF-8 byte lengths, the tuple comparison | T10 | P-B1 to P-B3, Q1, Q9, D3-A, D2-T, P-L1, P-L2, C1 to C4, P-X1, P-S1, P-T1 to P-T3, P-R1 to P-R4, Q4, Q5, D1-S, D2-L. **Built** in PR #383 (`non_task_cursor/held.rs`), private and unwired: 1,024 entries, 8 MiB and 24 hours, a nonce made at construction, a monotonic clock injected at construction, a poison-recovering lock released before the identity scan, a 34-byte held token (version 2 or 3, sort, binding, nonce, handle) with no expiry field, and a lookup that scans identities unfiltered and then asks a caller-supplied hydrator, in the resuming view, for the original tuple. Its tests use real SQLite identity quartets and a synthetic hydrator; native blob decoding and every integrated vector are step 5's |
| 5 | The page PR: wires steps 2 to 4 into `Storage` on SQLite for the eight families after the SQLite write path exists (the owner's D1: the unit of work on the writer's thread, with a contained panic). It names every vector as a test, runs the shared cases `cursor_walk_is_stable_under_inserts_and_deletes` and `cursor_is_bound_to_project_filter_and_sort` on SQLite for all eight families, and executes the mutants of Q10. It also carries the integrated half of P-F1, P-F2, Q8, N3a and N3b, and the leaf conditions of issue 378 (see "As built, steps 2 and 3") | T10 | The vectors, with the results recorded in that PR; P-W2, P-X4, C8′ and Q2 need the write path |
| 6 | The walker half of P-E4: a full CLI, TUI or web walk buffers its pages, restarts from page one on `InvalidCursor` a small fixed number of times and then ends with a defined error | T17 | P-E4 (walker half), `cargo test -p orch-cli --test list_paging` |
| 7 | Follow-ups, not conditions: a count of evictions of unexpired entries, and a per-principal limit on the table before a caller other than the owner can list (release 1.3) | after the page PR | An issue each |

As built on `main`, steps 2 and 3 (PRs #376 and #377; both private, no `Storage` caller). Where the page PR must not trust them:

- The encoder's `Layout`, `Tuple` and `Component` and each leaf's `OrderKey` are separate types with separate comparators
  (`Layout::compare` and three `cmp_key`). The page PR bridges them (owned keys, the message key, the goal's absent
  `updated_at` as presence 1 and an empty text on one side and an `Option` on the other) and keeps an all-shapes
  equivalence test, or unifies the comparators; no test runs one against the other today (issue 378).
- Issue 378 also holds two leaf test conditions: positive second pages over stored Config and Template rows (a mutation that
  disabled them survived the suite), and `agent_team_run/key_tests.rs`, which `include!` hides from `cargo fmt`, is not
  rustfmt-formatted.
- The page enters the leaves through the router in the order `ensure_committable`, `read`, `port`, and an integration
  test exercises that route so a direct leaf `select_after` call cannot bypass the transaction guard (issue 378).
- The page authorizes the caller and takes the project from it, then validates project, generation, family, the whole
  filter and the sort against the token before any table or row access, and derives the leaf's shape from that
  validated request. A tuple of the right shape is not a binding, and `key_of`'s identity shape does not check family.
- The lookup arm scans identities unfiltered, in the project and family, in the resuming view, then compares every
  component of the tuple and only then calls `select_after`. The message leaf's narrowing query (status `pending`,
  recipient) serves the page and must not serve the scan.
- The leaf key tests use short texts. A tuple over the inline capacity, and rows whose texts are stored behind blob
  objects, are exercised at the table of step 4 (PR #383, with a synthetic hydrator) and in the page PR; the leaves compare the hydrated original bytes through the
  same page code as `select`.

Order. Step 1 first. Public paging, the wire of step 6 and every list that returns a non-Task cursor waited for the owner's
landing of step 1, which has landed (PR #369); they now wait for step 5 (step 4 is built, PR #383). Steps 2 and 3 were independent of each
other and of the table. The numbers of the table (1,024 entries,
8 MiB, 24 hours) are the owner's starting bounds and are unmeasured; a change goes to the owner.

Rollback. Nothing here touches stored data, a protocol or a file format: the table has no durable form and the tokens
end at every restart, so reverting a later step needs no migration.

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
`read_pool/tests/mod.rs`. When this foundation was written they were not the `sqlite_writer` and `sqlite_crash` integration
tests that T10's "Verify" column then named. `sqlite_writer` was never a test target (issue 450): T10's "Verify" column now names
the targets that exist, and the SQLite run of the conformance suite is built (spec "The eight SQLite registrations"). Every receipt wait in the
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
(R28); retry that replays safely and one shared fatal policy by failure class (R128, R140), which includes the latch of a
fatal cause that a callback catches, the classification of both causes of a failed rollback, the failed top-level
rollback that leaves the connection inside a transaction (a cause of its own, the spec's record L), and proven `Task` corruption
(R140, R141); the checkpoint
connection, its backlog, outcomes and the expiry of long reads (R126, R127, R137); the authorizer and query
connection (R130); and shutdown integration (R139). The review conditions of the foundation that belong to the
adapter are tracked in [issue 127](https://github.com/yuxhuang/r-orch/issues/127): detecting transaction control
in a callback, non-blocking receipt waits on async threads, bounded claim production, and fatal-state handling
that keeps queue locks free of callbacks. PR 154 delivers two items of that list, the recheck of a request's expiry
after `BEGIN IMMEDIATE` and writer tests that fail in bounded time. Both landed on `main` in `8afcbe02` (2026-11-22). The retry case of
R128, in which a request answered `overloaded` cannot commit later, is still owed at the adapter integration. The
daemon is wired to this executor only after all of these exist.

**The writer failure policy and its two PRs.** Two open, unwired PRs build the policy, and the owner merges them in
one train, bottom to top. PR 249 is the bottom: the shared failure state, the classes of R128 and R140, and no
wiring. PR 188 is above it: the executor's commit and rollback hooks, resynchronised on PR 249. The two rewrite
the same lines of the writer, so the resync is a build in its own right and takes a delta review and a fresh Linux
check and test on its head. The owner's ruling of 2026-11-24 (the spec's record L) adds the failed top-level rollback
to R128's fatal class, and the resynchronised PR 188 delivers it:

- When an explicit top-level rollback fails and the connection is still inside a transaction, the cause is latched
  in PR 249's shared state, as `storage_unavailable`, before the executor stops serving the connection. The writer
  may end its thread or stay to refuse; either way every queued and later request is answered `storage_unavailable`,
  never `WriterClosed` and never a SQLite error. A rollback error with the connection outside a transaction is not this
  cause and latches nothing for it; its result is classified as any other (R128, R140).
- The latch is checked before `TransactionAborted` is returned.
- The commit hook is armed only around the executor's own COMMIT, so that PR 249's commit is not refused, and both
  hooks are removed before the connection is dropped (a freed hook closure crashes the process when SQLite rolls
  back an open transaction at close, as PR 188 reproduced).
- No panic is mapped to `storage_unavailable` and the connection is not reopened (record L).
- On the combined head, PR 188's five tests and PR 249's policy tests pass, and so do the tests the spec's R128 and
  R140 name for this cause, which are listed in "Test plan".

Before any adapter is wired or T10 is activated, the hooks and the shared latch are both on `main`: the lead's decision
of 2026-11-23, because a statement that SQLite autocommits after it ended the transaction was durable behind a
refusal when only the latch was present.

**Lease adapter contracts (issue 127 N1 to N4).** #211 and #232 merged the damaged-epoch error, the lane
mapping and the optional request deadline of the internal SQLite lease adapter, and the spec's "Lease
adapter contracts" records them with their tests. They are unwired: only the module's tests call the
adapter. What stays owed, by owner: the engine's start (lane 1) and finish (lane 2) transactions, which
call the composed lease helpers (T12), and its cancel, pause and permanent-failure transactions, which call
them on lane 2 (T13); `leases.expire` on lane 1 (T15);
the `next_epoch` check of `orch db verify` (T22); the structured fatal classification of R128 and R140,
and the registration of the shared lease suites on SQLite (T10). Open: whether a damaged counter should
fail the writer, which neither R128 nor R140 says today. Issue 127 stays open until the owner lands these
rows and the pieces it still names.

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

### T10 task mapping (format v1)

The inactive SQLite adapter holds goals, leases, run events, request keys and task paging. A task needs the format of the
spec's "Task format v1 (SQLite)" before its mapping is built: the first migration has no place for a whole `Task`, and
its R97 text rule has no discriminator for a referenced task text. The lead chose a numbered migration that adds
`task_details` and `task_blob_refs` over a versioned envelope in a reserved column (ADR 0017 decision 26). This
section is the plan for that slice and for what other tasks owe it. It changes no row of the task table above.

**Order.** This documentation lands, reviewed, before product code: the spec, this plan and ADR 0017 are the only files
of its PR. The mapping slice then branches from `main` with the documentation in its history and builds the
code and tests named in R141. It stays one bounded PR; if it would pass about 3,000 changed lines the split is
(a) the codec, the migration and inline text, then (b) the references and the physical R97 tests, each leaving `main`
working. The public cursor wiring and the other entity families keep their own slices, and the mapping hands the
hydration seam to the slice that wires the public task page, whose tests check the real boundary.

**Obligations, by task.**

| Task | Owes | Check |
|---|---|---|
| T10, task mapping | Create, update, delete and move for `Task`; the migration (next free number); hydration in the selecting snapshot or unit of work; the physical R97 mapping; the creator, `landing_state` and attachment-row rules of the spec; the structured `SqliteError::TaskCorrupt`, fatal under R140, classified before any public conversion; the shared entity suites registered on SQLite for tasks | R141's named tests (the corruption ones among them); `cargo test -p orch-infra --test sqlite_task_format --test sqlite_migrations`; the shared suites on the three references and SQLite, none holding a text over 67,108,864 bytes (the file reference refuses one; the split of K >= 2 is checked only by `sqlite_task_format`) |
| T10, retry mapping | Its own mapping of `retry_queue.error` with the same thresholds and split, and none of this format's tables | The issue 109 tests; independent of R141 |
| T11 | One change in T11's own file, `crates/orch-infra/src/blob/references.rs`: row-only registration of an object that is already published, callable from `crate::sqlite` (a `pub(crate)` method of `BlobReferences`, or `register` split so that its upsert is `pub(crate)` and the `put` stays out of it). The `blobs` row is the one `register` writes for a prompt (`storage_location = 'disk'`, the object's own size, `unreferenced_since` cleared, refused when an existing row disagrees on size or location), and the method never calls `DiskBlobStore::put`. `register` itself, `store_prompt`, `store_attachment` and the two loaders keep their behaviour. At `07bc81124` `register` is a private method (visible only inside the `references` module) that calls `put` before its upsert, so the mapping cannot call it and could only reach it by publishing every object a second time after the spec's step 2 (a second hash and sync of each object, interleaved with row writes). The blob adapter (`DiskBlobStore::put`, `get`) is otherwise used as it is. The owner is T11; which PR carries the change is open (below) | the existing `blob_store` tests unchanged, plus one new test, not yet written, that row-only registration writes the same row as a prompt's, publishes nothing, and refuses a size or location that disagrees |
| T12 | Nothing new: a consumer reads a `Task` through the port and never through these tables | the T12 tests |
| T15 | `blobs.sweep` counts `task_blob_refs.sha256` as a naming row | `maintenance::a_referenced_blob_is_never_deleted`, extended for a task text and a split text |
| T21 | One codec for every task row; list order, repeats and `Option` presence as the 1.0 document says; the decision and the record for a non-canonical timestamp; `import` and NULL as the creator of a legacy task; 1.0's recorded proof as the task's `proof` in the detail, with `legacy_proof` left NULL; text over 64 KiB as references; no path or hash made from an attachment name | an importer test per case (one asserts that an imported proof reads back through the port and that `legacy_proof` is NULL), and the R97 importer test the spec still lists as pending |
| T22 | `orch db verify` runs the format's read checks for every task and finds orphan references and details; a live task proved corrupt is `TaskCorrupt` and fails the writer, a corrupt backup or export file refuses the command alone; the views `diag_task_details` (without `detail`) and `diag_task_blob_refs` | the schema's view-listing test; `db_commands` |
| T23 | A backup and a restore carry every object a reference row names, in one point; an export carries both tables and the objects; an import reconstructs every text and checks its hash; a backup or an export of a live source that proves a task corrupt fails the writer, a corrupt artifact refuses the command and leaves the writer usable | `db_backup_restore`, `db_export_import`; a restore that finds a missing, short or reordered chunk refuses |
| T27 | Records the named checks as run, with counts, on Linux, and rejects an empty or ignored selection | the verification table |

**Holds that stay.** The file runtime stays until the switch (T24). Nothing here releases a writer-dependent slice, wires
the daemon or the public cursor, or consumes an unmerged slice: the #109 retry mapping, the attention migration and the
public cursor slices are used only after they have merged. The independent code review and, for the
covered surfaces, the security review, the exact-head hosted Linux check and test with a clean merge state, the
lead's recommendation and the owner's merge apply to the mapping PR as to every PR.

**Settled before code.** The lead decided three write rules on 2026-11-23, and the owner of the attachment path (T11)
concurred in the third; the spec's section states them and the mapping slice implements them with the tests R141
names. (1) `create` derives `created_by_kind` and `created_by_run_id` once from the caller's `Principal`: `Owner` is
`owner`, `Worker` is `daemon`, `Task` is `agent` with the authenticated, fenced run; the importer (T21) records `import`
separately. (2) A write that leaves `done` preserves `landing_state` byte for byte; completion sets `skipped` only
over NULL. (3) A revision-checked `delete` removes the task's `task_attachments` rows for the exact project and task in
its own savepoint, and keeps the `blobs` registrations and object bytes for the sweep (R60). The same text corrects the
presence rule: `proof` and `workspace_mode` are optional, and `files_changed` is required only inside a present `proof`.
The mapping slice consumes this correction before it writes any of these columns. Three points remain open: the owner's
approval of the two tables (O2), given by the merge; the structured classification of the other entities, which issue 127 holds;
and the carrier of the T11 change in the table above: the mapping's scope is the SQLite source and tests, and
`crates/orch-infra/src/blob/references.rs` is outside it. Either a small T11 PR with the row-only registration lands first, or the lead
widens the mapping's scope by that one file and its `blob_store` test. The mapping's references slice (b) cannot write its
`blobs` rows until one of the two exists. The rule for 1.0's recorded proof is settled as documentation groundwork: the spec's
"Import rules" sends it to the detail and no longer to `legacy_proof`, the lead ruled on 2026-11-23 that this amendment of the
approved rule stands, and the owner's merge of this text approves it; the T21 implementation and live-copy access stay held.

**Task corruption, settled before code.** The lead decided on 2026-11-23 what "the typed stored-data error of R140" is. The mapping
adds `SqliteError::TaskCorrupt { task, check, locator }`: the id the caller asked for, a bounded check code and a bounded
structural locator, never a value, a raw stored key, a path or a backend message. It is classified before any public
conversion or diagnostic, and proven logical corruption of live `Task` data is fatal under R140: a present row whose detail is
missing, an unsupported stored version, an invalid encoding or identity, projection or membership drift, or a proven
missing or invalid referenced object, found by a live `get`, a page or list, a snapshot or unit-of-work read, or a live
verification, query, backup or export that validates tasks, returns the failure, discards partial results and trips the
shared writer failure state until a restart. An absent task, a caller's validation or revision refusal and an admission
timeout stay ordinary, a transient read or object-access failure alone does not prove corruption (the connection rules of
R140 apply), and corruption found only in an external import, backup or export artifact refuses that operation and leaves
the writer usable. On the writer, a fatal cause is latched even if the callback catches it and returns success, and the
enclosing transaction aborts; an `OperationRollback` is classified in both of its causes, a fatal one on either side wins,
any failed savepoint rollback forbids the enclosing commit, and an ordinary failure rolled back to its savepoint may be
caught while unrelated work commits. The writer on `main` does not yet classify or latch (issue 127), so the mapping's
error integration waits for the shared failure state of the T10 integration and this text releases no code.

### T10 entity format (Agent, Team, Run, Context, Config, Template, Goal)

The task mapping above settles how a `Task` is stored. The other seven families that the entity port holds have the
same kind of gap (the spec's "Entity format v1 (SQLite)" lists it family by family), and the lead decided on
2026-11-23 to extend the task format to them as option A of two, in two decisions: Agent, Team and Run, then Context,
Config, Template and Goal (ADR 0017 decisions 27 and 28). The rejected option wrote a versioned envelope into columns
that already have a meaning (`agents.config`, `runs.local`, the `value` and `body` columns) and still needed
metadata, a migration and a second reader. This section is the plan for those two slices. It changes no row of the
task table above and no line of "T10 task mapping (format v1)".

**Order.** The sequence is fixed and each step waits for the one before it:

1. The task format's documentation, then the task mapping with the shared codec and its migration (the plan's "T10 task
   mapping (format v1)").
2. This documentation (the spec, this plan and ADR 0017), written before any code of steps 3 and 4. The first slice's
   pull request carries it in one commit after the codec's, because the documentation was integrated onto the frozen code.
3. Agent, Team and Run, with the first migration of the format (the two tables and the rebuild of `agents`, `teams`,
   `team_members`, `team_tasks` and `runs`). It reuses the verified codec of step 1 and does not copy it. Its first
   delivered slice is an inactive byte codec for the three families, with the R97 tags, the chunked texts and the root
   reference, and with no table, migration or SQL ("First slice: the byte codec" below). Its second is migration 004,
   which stages the schema only, with no SQL operation ("Second slice: migration 004, schema only" below); the native
   create, update, delete and move, the hydration and the paging are the next slice. It is not the split named below,
   because the codec already covers the references.
4. Context, Config, Template and Goal: one slice, with the second migration (the rebuild of four tables, no new table),
   reusing the codec and the tables of step 3, and the goal adapter's existing helpers once, without replaying them.
   It was first drafted as three stacked pull requests: PR 314 (migration 005, schema only, with the lower time bound of the landed Agent, Team and Run helper), PR 317 (a crate-private native driver for the four families) and PR 318 (the goal adapter rewritten as a wrapper of the driver), together 13 files and 4,785 changed lines (+3,659/-1,126). The reviews of 2026-11-26 were answered by repairs inside the three layers, carried forward by the successor pull requests PR 323 (migration 005; 7 files, +722/-12), PR 324 (the driver) and PR 326 (the goal adapter). PR 324 and PR 326 are replaced by one combined pull request, PR 338, whose code is byte-identical to PR 326's and which sits on PR 323 (12 files, +3,725/-1,164, 4,889 changed lines at its first head `4056bc073`; the text this amendment adds to this plan and the spec makes it a few lines more, about 4,900 in all). The proposed landing is two squashes, in this order: migration 005 (PR 323), then PR 338. Neither is merged yet; the owner merges, only on a green main, after Linux CI has succeeded on each exact head with a CLEAN merge state, the independent reviews are complete and the lead has recommended the head in writing. PR 338 is within the project's limit of about 5,000 changed lines (AGENTS.md, step 4) but above the usual 4,000. The whole repaired change is 19 files, +4,447/-1,176, 5,623 changed lines against main at `8a3a96f1`, measured at PR 338's first head, which is over the limit, so the work is two pull requests and not one. PR 338 is not split further, so that it stays at about 4,900 lines, for two reasons: under single-parent squash merges the driver layer and the goal-adapter layer cannot land as separate squashes, because the upper one re-adds the driver files that the lower one added (`cctg/mod.rs` and `cctg/tests.rs`); and the driver's integrity tests must land with the implementation they check. Migration 005 stays its own pull request: it needs no driver code, so a later migration can be built on it alone. PRs 314, 317, 318, 324 and 326 are kept open as history and are not to be merged; PR 336 and PR 339 are qualification drafts of the combined chain and are not to be merged either.

One owner holds the migration registry across steps 1, 3 and 4. A number is allocated on the verified integration base in
sequence and never reserved by a document; earlier migrations stay byte for byte. Each slice is one bounded PR, normally under about
4,000 changed lines; if it would pass about 5,000, the split is the codec and migration with inline text first, then
the references and the physical R97 tests, each leaving `main` working. A stack is made only for a real code dependency
and the whole stack is qualified. The public cursor wiring is not part of either slice: it belongs to its own slice, after PRs 186 and 208 have
landed. The retry mapping (issue 109) and the attention slice (issue 183) are consumed only after a positive
owner-landing receipt for each exists.

**Obligations, by task.**

| Task | Owes | Check |
|---|---|---|
| T10, Agent, Team and Run | Create, update, delete and move for the three families; the migration; ordinal-keyed members; a numeric `attempt` order for one task's runs; the claim tuple left to the claim path; the prompt slot as a tag, and the prompt helper changed to store and load a prompt through the tag and chunk rows (the helper at the base stores one object of at most 64 MiB); hydration in the selecting snapshot or unit of work; the physical R97 mapping; the shared entity suites registered on SQLite | R142's named tests; `cargo test -p orch-infra --test sqlite_agent_team_run_format --test sqlite_migrations`; the shared suites on SQLite and both references |
| T10, Context, Config, Template and Goal | The same for the four families; context provenance bound from the caller; the lexical orders on original bytes across indirect keys; positions for context, config and templates; the goal's integration and publication columns kept | R143's named tests, which are unit tests of the library because the driver is crate-private: `cargo test -p orch-infra --lib sqlite::cctg::tests::<name> -- --exact` (one test each) and `sqlite::goal_adapter::tests`, with `cargo test -p orch-infra --test sqlite_migrations --test sqlite_schema`; the shared suites on SQLite and both references, which wait for a `Storage` adapter |
| T11 | Nothing new: the blob adapter (one object of at most 64 MiB) is used as it is; its prompt helper is changed by T10, not here | `blob_store` unchanged |
| T12 | The claim path is the only writer of a run's claim tuple, and consumes a run through the port | the T12 tests |
| T15 | `blobs.sweep` counts `entity_blob_refs.sha256`; `context.expire` is a separate job | `maintenance::a_referenced_blob_is_never_deleted`, extended for each family |
| T21 | One codec for every entity, never rows inserted by hand; list order, repeats and presence as 1.0 says; the claim tuple of an imported run stays the importer's own rule; position high-water marks initialized and a larger one kept; a time kept as it is | an importer test per case, and the R97 importer test the spec still lists as pending |
| T22 | `orch db verify` runs the read checks for every entity and finds orphan references, member rows and details; views of the two tables | the schema's view-listing test; `db_commands` |
| T23 | A backup and a restore carry every object a reference row names, in one point; an export carries both tables, the counters and the positions of emptied families; an import reconstructs every text and checks its hash | `db_backup_restore`, `db_export_import`; a restore that finds a missing, short or reordered chunk refuses |
| T27 | Records the named checks as run, with counts, on Linux, and rejects an empty or ignored selection | the verification table |

**Holds that stay.** The file runtime stays until the switch (T24). Nothing here wires the daemon or the public cursor,
releases a slice that depends on the writer, or consumes an unmerged slice. Every hold of the task mapping carries over:
the typed stored-data error of R140 and issue 127, issues 149, 152, 166, 167, 203 and 209, the signed, time, revision and cursor
bounds, the writer policy, checkpoint and backlog rules, the obligations of Amendment 3 (the unit of work stays on the writer's thread, a callback needs no runtime, a panic in it is contained, and the read side keeps `Send + Sync`), the shared conformance suites on SQLite
and on both references, and the combined crash proof. The first complete frozen storage head goes to the combined crash proof of
issue 149 and to the check of transaction-local intent reads and retention of issue 167 before any writer-dependent
work is released. Independent code review
and, for the covered surfaces, security review, the exact-head hosted Linux check and test with a clean merge state, the
lead's recommendation and the owner's merge apply to each PR. No release.

**Evidence before code.** The verified documentation commit and its base; the actual codec and the migration head that the
step builds on, by fetching and checking them, not by a task's status; positive owner-landing receipts for any slice it
consumes.

**Open before code.** The list "Derived, to be confirmed" and the open points of the spec's "Entity format v1
(SQLite)". The migration owner confirmed the ten derived items on 2026-11-24, and migration 004 settles the new column
names and the digest columns for an indirect key (the spec's "Physical schema of migration 004"). Still open: how a page
merges inline and indirect rows for Agent, Team and Run (settled for the other four by the driver of PR 317, which
hydrates the family and sorts in Rust); the serialized size of `runs.local`, whose member names `pid` and
`workspace_path` are settled; the size of a prompt (whether one over 64 MiB can occur; if not, it is refused instead of
chunked); and what T21 keeps of `local`'s record of a repeated 1.0 attempt. None changes the format. The run and goal orders are settled; see "Reference
order" above.

**First slice: the byte codec (Agent, Team and Run).** The first slice of step 3 is implemented, inactive, in
`crates/orch-infra/src/sqlite/agent_team_run` (on the corrected head `cae0ca499`: a 502-line module, a 730-line test module
with 22 tests and a six-line registration in `sqlite/mod.rs`; on the first head `5c3ff60eb`: 561 lines, 568 lines with 15
tests and the same six lines), in PR 251, which the owner squashed as `33fd6fcbf` on the Task codec of PR 226 (`f181be7af`). Nothing calls it, and the daemon still uses file storage. What it is:

- `encode_agent`, `encode_team` and `encode_run` turn the typed domain value into the Task codec's `Detail` record (the
  JSON inline when it is at most 65,536 bytes, otherwise the same JSON behind root references at locator `''`), and
  `decode_agent`, `decode_team` and `decode_run` read one back. It reuses `Detail` and the Task codec's text helpers
  (`publish`, `validate_chunks`, `reconstruct`) and adds no dependency. The strict duplicate-key JSON reader is not
  written twice: the lead approved on 2026-11-23 (option A) that this module use the Task codec's `UniqueValue`, whose
  declaration is widened to `pub(in crate::sqlite)` and nothing else of the Task codec changes. The reader keeps
  integer and floating-point numbers apart, represents `null` explicitly and refuses a repeated key at every depth. What
  stays in each family is its own driver: the Agent, Team and Run decoder still refuses `null`, a fraction and an
  exponent spelling (an integer-valued float included) at every place that takes a number, and checks the typed values
  before it reads a slot object. The approval covers the shared reader only while the family drivers stay separate;
  a generic driver would be a new decision. The corrected head `cae0ca499` carries this; the module shrank by 59 lines because the local reader was removed.
- Every string of the three families is an R97 slot: identities, names, times, nested strings, list elements and the
  names and values of `env`. Numbers and booleans are not. `env` is an array of `[name, value]` pairs. A run's `prompt` is
  always a tag, an empty one included, naming one zero-byte object. Encoding publishes the objects it needs to the blob
  store and returns the chunk rows in the `Detail`; it installs no reference row and no `blobs` row, so those objects stay
  unreferenced, and follow R60, until a caller's transaction registers them.
- The decoder refuses an unknown or missing key, a `null`, a duplicate key, a wrong type, an integer outside its type, a
  fraction or an exponent, an inline text over 65,536 bytes, an inline prompt, a reference with inconsistent metadata,
  rows at a locator that has no tag, and an `env` name that repeats, also when one of the two was behind a tag. It checks
  the typed inline values and every reference's metadata before it reads any slot object, after it has reconstructed a
  detail that is itself behind root references, so a damaged inline value or damaged metadata is `Invalid` even when an
  object is missing or unreadable. An object that cannot be read for an I/O reason (permission denied, for one) is
  `Access`, not corruption. Neither error holds a value or a path; both name a locator, and an `Invalid` one a check. The classification of either
  as the typed stored-data error of R140 belongs to the mapping and to issue 127; the module's `FormatError` is internal.
- The SQL caller owns what the codec does not: the format number, the native projections, the project-scoped reference and
  `blobs` rows, revision and position, and every check that needs a transaction or a snapshot.

**What was run.** Linux, by the author, from their logs, in this order. The tests first failed to compile (23 errors)
before the module existed. A first run of the module tests then passed 3 tests; a focused build failed on a borrow error
in a test, and its fix gave 11 passed. The author's focused run, `cargo test -p orch-infra --lib sqlite::agent_team_run
--offline -- --test-threads=1`, then passed 15 tests, 0 failed, 0 ignored. After it, a first `cargo clippy` run failed on
three collapsible `if` statements, they were fixed in the module, and a second `cargo clippy` run on `orch-infra` with
warnings denied finished without errors. The fix was committed as `5c3ff60eb`. The author's focused run is not logged
after the lint fix, so the 15 passed of that run cover the tree before it, not `5c3ff60eb`; the head's own results follow. `cargo fmt --check` and `git diff --check`
are reported by the author to pass, and their logs are not among the records read for this text. The author's `fast` run on the exact head
`5c3ff60eb` (tree `d3e10509`, clean, unmoved) then passed: 5,470 passed, 0 failed, 7 ignored, all eight steps passing,
among them the 15 module tests of this slice and the 19 `task_format` tests. Its process exit was 3 by design, because the
evidence a local `fast` run exports is never qualifying (`qualifying: no`); that is not a failed test and not a gate
result. The 15 module tests also passed (0 failed, 0 ignored) on `5c3ff60eb` in two focused runs by others: the writer
of this text ran them with a private home and a build output inside the checkout, and the code reviewer ran them on the
same head. The writer also ran `import_index_reference` (22 passed, 0 failed) and the changelog check (exit 0) on the
documentation's tree. The 15 tests are those of `5c3ff60eb`. They are module tests of the library, which R142 does not name:
`complete_values_preserve_absence_empty_false_order_and_unsigned_limits`,
`empty_capabilities_are_omitted_and_empty_prompt_is_one_zero_byte_object`,
`strict_agent_decoder_rejects_unknown_duplicate_null_missing_and_nonintegers`,
`every_nested_string_uses_original_byte_limits_and_oversized_ids_roundtrip`,
`aggregate_detail_is_indirect_even_when_every_string_is_small`, `team_and_run_nested_slots_preserve_exact_values`,
`malformed_detail_and_references_are_refused_with_bounded_diagnostics`,
`inline_enum_and_counter_damage_is_proven_before_missing_slot_access`,
`environment_pairs_refuse_duplicates_wrong_shapes_null_and_unknown_nested_fields`,
`team_and_run_reject_unknown_missing_null_and_wrong_nested_types`,
`prompt_sizes_short_inline_limit_and_one_more_always_name_verified_objects`,
`split_utf8_prompt_and_exact_object_limit_reconstruct_from_the_manifest`,
`repeated_slot_hashes_keep_distinct_locators_and_env_names_are_checked_after_reconstruction`,
`permission_errors_stay_ordinary_until_metadata_or_inline_types_prove_corruption` and
`all_statuses_empty_and_present_zero_tokens_remain_distinct`. The split at 67,108,864 bytes is exercised on the prompt only.

The corrected head `cae0ca499` (tree `d54e3c69`, parent `3a7434527`) keeps these 15 unchanged and adds seven, 22 in all, in
answer to the code review's I1 (test sensitivity): `short_prompt_manifest_damage_is_refused`,
`oversize_inline_text_inside_indirect_root_is_refused`, `aggregate_exact_boundary`, `inline_prompt_without_refs_is_refused`,
`oversized_inline_root_is_refused_even_when_all_slots_fit`, `tokens_unknown_fields_are_refused` and
`shared_reader_keeps_null_fraction_exponent_and_nested_duplicate_refusals`. On Linux, by the fact-checker of this
documentation, focused runs on that head passed: `agent_team_run` 22 passed, 0 failed, 0 ignored; `task_format` 19 passed, 0
failed, 0 ignored; and `import_index_reference` 22 passed, 0 failed, 0 ignored. The code's author reports the same 22 and 19, a
wider `sqlite::` filter at 150 passed and 0 failed, clippy with warnings denied, `cargo fmt --check` and `git diff --check`
clean, and a `fast` run still in progress when the head was frozen; the author's runs are the author's own. No `fast` or hosted
result of `cae0ca499` is recorded in this text; the head that landed, `271518ac3`, is the one that carried the hosted Linux check and test.

**Second slice: migration 004, schema only (Agent, Team and Run).** The second slice of step 3 stages the physical
schema and nothing that uses it. It is one bounded change of five files, 1,119 lines added and 3 removed (tasks
`tsk_tGrSRMx` and `tsk_YgPU5Hq`; PR 268, branch head `ed4a96795`, tree `875e6bcb4`, on base `ee8d56ae4`, which holds the landed codec). It is
inactive: the daemon uses file storage until T24, no code reads or writes a rebuilt column, and the migration runs on no real database. What it is:

- `crates/orch-infra/src/sqlite/migrations/004_agent_team_run.sql` (232 lines) creates `entity_details` and
  `entity_blob_refs` for the seven families and rebuilds `agents`, `teams`, `team_members`, `team_tasks` and `runs` with
  explicit text forms, nullable millisecond and counter projections, original time texts, ordinal-keyed members, the
  payload `attempt`, the independent `tokens_total`, and the claim tuple that is absent together or complete. The
  spec's "Physical schema of migration 004" records every name, check and index and is the reference for them.
- `crates/orch-infra/src/sqlite/migrations.rs` (+42, -2) raises `SCHEMA_VERSION` to 4, registers the step and adds
  `refuse_lossy_entities`: a count of the five predecessor tables, made before the disk-floor check, the R124 copy and
  any intermediate migration, and made again inside the migration's own transaction. A populated predecessor is
  refused with the five counts and no row content, and a refusal writes no copy.
- Tests (all in the library or its integration tests): `sqlite_migrations.rs` +242/-1, four new tests and the version in
  the Task migration's test raised from 3 to 4; `sqlite_schema.rs` +523, eight new tests and the table list (37 tables);
  and eight permanent optional-null regressions and one value-equivalent detail test in the codec's module tests
  (`agent_team_run/tests.rs`, +80), issue 252.
- Unchanged byte for byte against the base (the Git object ids of the blobs are equal): `schema.sql`, `002_attention_facts.sql`,
  `003_task_format.sql`, `agent_team_run/mod.rs` and the three `task_format` files. The slice adds no dependency and
  changes no product code outside `migrations.rs`, the SQL file and the codec's test module.

**What was run for the second slice.** Linux. The slice has two heads: the first, `012b1df3e` (tree `1baa29d5d`), and
the corrected head, `ed4a96795` (tree `875e6bcb4`), which is the slice's current one.

*First head.* The migration owner's logs, in order: a mistaken exact test filter selected zero tests first
(15:42:33 UTC) and was corrected; the registry regression
`agent_team_run_registry_reaches_four_and_repeats_without_changes` then failed (3 against the expected 4) before the
migration was registered (15:42:41); a test compile failed on an unsupported `usize` read (`FromSql`) and was corrected to
`i64` (15:45:25); then the focused runs passed: the codec's module tests
31 passed, `sqlite_migrations` 19 passed, `sqlite_schema` 11 passed, 0 failed and 0 ignored in each, and the
blob store's reader tests 27 passed. The null guard was removed as a control and all eight `optional_null_*` tests
failed (0 passed, 8 failed); the decoder was restored byte for byte. The migration owner's exact-head local `fast` run on
`012b1df3e` passed (tree `1baa29d5d`, clean tree, head unmoved): 5,705 passed, 0 failed, 8 ignored, and its format,
clippy, crate-graph, build and test steps passed; its own log labels it `qualifying: no: local iteration; never merge
evidence`. A hosted Linux run on that head succeeded. The independent security review's preliminary finding on that head
was that every digest column accepted 64 lowercase hexadecimal characters followed by a NUL (CWE-20; no code used the
columns).

*Corrected head.* `ed4a96795` adds one byte-length term to each of the 24 digest checks of `004_agent_team_run.sql` and
four tests to `sqlite_schema.rs` (a pure append: the earlier tests are an exact prefix); no other file differs from the
first head. The migration owner's records: the new bound-NUL test failed against the first head's SQL (exit 101) and
passes against the correction; `sqlite_schema` 15, `sqlite_migrations` 19, the blob store's reader tests 27 and the codec's
module tests 31 passed, 0 failed; 38 of 38 compiled mutants were killed (the 14 that survived the code review of the first
head, and each of the 24 byte terms removed alone); and the exact-head local `fast` run on `ed4a96795` passed (clean tree,
head unmoved): 5,709 passed, 0 failed, 8 ignored, with its format, clippy, crate-graph, build, test and three isolation
steps passed. Its log carries the same `qualifying: no: local iteration; never merge evidence` label, so it is development
evidence only. The writer of this text re-ran, on a documentation commit over the corrected head (its crates equal
`ed4a96795`'s), the four focused runs with the same counts and 0 failed; ran the four new tests against the first head's
SQL (14 passed, 1 failed, and the one failure is the bound-NUL test); removed two of the 24 byte terms one at a time
(`runs.error_sha256` and `entity_blob_refs.sha256`), and the bound-NUL test failed each time; and probed throwaway
in-memory databases with Python's `sqlite3` (SQLite 3.46.0): the first head's SQL accepts `'a' * 64 + NUL + '/../../canary'`
in all 20 native projections and in the four positions of the shared pair, and the corrected SQL refuses it in every one.
The permanent tests apply the shipped file with the product's bundled SQLite (3.54.1). The writer's focused runs used a
private checkout, and the logs and probe scripts are kept with the team's private receipts.

Pending for the corrected head: the delta code, security and resilience reviews (the first head's reviews read the
earlier bytes), a fact-check of this text, hosted Linux check and test SUCCESS and CLEAN on that exact head (the first
head's hosted run does not cover it), the lead's recommendation and the owner's merge. No `fast` run other than the
migration owner's is claimed here.

**What is not done, and still held.** No `sqlite_agent_team_run_format.rs`; no SQL operation in this format: no native
create, update, delete or move, no projection, revision or position written, no claim tuple allocated, no hydration in
the selecting snapshot or unit of work, no paging and no merge of inline and indirect rows; no prompt-helper change (the
helper looks a run up by id without its form and stays restricted to runs of form 0, which nothing produces yet); no
Storage method; no shared entity suite registered on SQLite; no importer, backup or export work; no
SQL round trip of the oversized-identity, name, task-pool and maximal-counter cases of R142; nothing for Context,
Config, Template or Goal, and no second migration. The slice releases none of the holds listed above; it is not a
Storage acceptance and gives no credit for R142, R140, issue 127 or any gate line.

### Amendment 3: the unit of work on the writer's thread

The owner's ruling of 2026-11-26 (the spec's record M, answer D1; [ADR 0023](../decisions/0023-unit-of-work-on-the-writer-thread.md))
removes the `Send` bound from `UnitOfWork` and its six write traits. The contract is the spec's "Amendment 3: the unit of
work stays on the writer's thread" under "Storage port v2"; `docs/TRAITS-FROZEN.md` carries the signatures.

| Step | What | Owner task | Verify |
|---|---|---|---|
| 1 | One PR carries this documentation and the seven-bound source change in `crates/orch-core/src/store*`: `Send` removed from `UnitOfWork`, `AttentionWrite`, `EventWrite`, `IntentWrite`, `LeaseWrite`, `RequestKeyWrite` and `RetryWrite`, one bound in each of seven files and no other line of `crates/`. No adapter and no caller is added | the T8 port | A3-W1: `cargo check --workspace --all-targets` on the PR's head, exit 0; `git diff --shortstat -- crates` reads 7 files changed, 6 insertions, 7 deletions; the PR's own Linux check and test |
| 2 | The SQLite `Storage::write` runs the callback once, on the writer's thread, against the borrowed unit; it contains a panic of the caller's callback, and only that (the catch surrounds the callback's invocation; building the unit, its read of the `Clock` and the check after a successful callback stay outside it): roll back, resume it on the awaiting caller, keep the thread, latch nothing; it never invokes the callback of a request it refuses | T10 | A3-W2, A3-W6, A3-W10, named tests of the adapter's PR |
| 2a | A leaf's failed savepoint cleanup raises the typed `CleanupUnwind` (the lease, intent, retry and message leaves) and the catch observes it under record L and raises it again, so that `Writer::run` ends the worker and closes the running and queued receipts (policy U, record N; the receipts and the latch follow the operation's and the cleanup's causes taken separately, record O). A failed outer rollback after a callback panic keeps the payload and the payload reaches the caller. Step 2a lands with or before step 2: the composition may not call the catch of step 2 without it | T10: the lower PR (issue 379) for the writer-layer helper and the leaves, then the composition for the call through `Storage::write`; issue 149 for the crash path | A3-W7 to A3-W9, first at the helper, then through `Storage::write`; issue 149's cases (the Test plan's "callback boundary" bullet) |
| 3 | The `compile_fail` doctest that shows a callback cannot move its unit to another thread, with a control that compiles | T10 | A3-W3 |
| 4 | Every engine callback of T12 and later keeps its unit on its thread and its body free of a Tokio runtime | T12 to T24 | The shared suites on SQLite; review of each callback |

Order. Step 1 comes first and needs no other slice. Steps 2 and 3 are part of the PR that makes SQLite a `Storage` and
may not land without their vectors; nothing in step 1 wires a daemon path or the public cursor. Step 4 applies to
each later PR.

What this plan does not decide, and leaves with its owner: the error representation of a refused or expired read (the
spec's "Reader snapshots on SQLite"; how the adapter otherwise serves a `ReadSnapshot` is the owner's ruling of 2026-11-27,
record N); how R128's "tried again" reads for a failure inside a callback; the `Cursor` and R10 wording for the families
other than Task (record M, D2). The cursor text is a separate revision and lands before public paging.

Rollback. Step 1 is source-level: it touches no stored data, protocol or file format, and reverting it restores the
bounds. Once the SQLite adapter lends a borrowed unit, restoring them would first need a different way to run the
callback, which a new design review would have to supply (ADR 0023, "Consequences").

### Reader snapshots on SQLite (the owner's ruling of 2026-11-27)

The owner's ruling of 2026-11-27 (the spec's record N) approved the wording of the reader's snapshot contract, with its
two numbers (200 ms, 5 s) as starting values. The contract is the spec's "Reader snapshots on SQLite" under "Writer lanes"
and the spec's paragraph on a pooled read after a writer latch (issue 389, option B as signed). The steps below are the
obligations of the PR that makes SQLite a `Storage` (T10); the table after them reads PR 385 at `dd196f952` (on #400, `eb3e0061e`). The published head of PR 385 is `d58dd15e4`; the
commits it adds to `dd196f952` change only `cfg(test)` code, tests and documentation (`3f5405194`, the completion probe and
its test; `8c4b534ea`, a test of bounded fanout through the public snapshot constructor), none of it in the reader path, so
the table is read again at no other head
against the contract. It is a reading of that head's source and tests, not a result of this plan and not a review.

| Step | What | Owner task | Verify |
|---|---|---|---|
| 1 | Admission and pin: a 200 ms admission budget for an open; a cancelled or expired queued open is skipped before it enters the pool; the deadline is checked again before the view is pinned; expiry is overload; the view is pinned by a real read before the open is acknowledged | T10, the composition (PR 385) | Tests of that PR: expiry while every connection is busy answers overload; a cancelled queued open pins nothing; a snapshot that is opened and never used sees the state at its open |
| 2 | Calls, idle release and stop: valid calls wait for bounded queue capacity and are not refused (independent review item I2); stop and expiry are rechecked after the wait; a handle with no call in flight for 5 s releases its transaction and its connection though it leaked, and a later call is `snapshot_expired`; dropping the last `Storage` client or closing storage stops the readers independently of the idle timer | T10, the composition | The review's 40-call case completes; an idle leaked handle returns its connection and its next call is `snapshot_expired`; a stop with a live snapshot does not wait for a long idle bound |
| 3 | Issue 389, option B: a latch set before the request permits the read; a fatal observation on any live connection between the request and the result, including while the read waits for a connection, discards the result, ends the snapshot and returns `storage_unavailable`; writes, claims and checkpoints stay refused | T10, the composition; proof by the consumer task of issue 389 | The lead's three cases: a fatal observation while the read waits for a connection; fresh damage after an earlier latch; the writer's refusal |
| 4 | The error representation: decide whether `overloaded`, `snapshot_expired` and `storage_unavailable` from a read are typed variants or fixed messages, and write it in the spec before activation | The owner of the R140 and T17 contract; this plan's writer records it | The spec's statement matches the code on the activation head |
| 5 | R127 and T15 stay apart: the interruption of an active snapshot older than 30 s over 256 MiB, and the checkpoint job, are not discharged by the idle release. The adapter's interruption has a development implementation at `6706afcbc` ("As built at `6706afcbc`" below); the checkpoint job and its schedule stay T15's | T10 (the interruption), T15 (the job) | R127's tests on the interruption (named below), and on the checkpoint job when it exists |

| Contract | As built at `dd196f952` |
|---|---|
| Open budget 200 ms; expiry is `overloaded` | Met. `Storage::read` queues the open with a 200 ms deadline; a full admission queue and an expired deadline both answer `StoreError::Backend` with the text `overloaded`. A queued open that its caller cancelled, or whose deadline passed, is skipped before it enters the pool (review item I5), and the deadline is checked again inside the pool just before the pin. Tests: `a_queued_snapshot_is_refused_at_admission_deadline`, `cancelled_and_expired_opens_do_not_retire_a_healthy_connection`, `cancelled_open_never_enters_pool_or_resets_its_failure_streak` |
| The view is pinned by a real read before the open is acknowledged | Met: `SELECT count(*) FROM sqlite_schema` runs before the acknowledgement (`owned_snapshot_pins_at_open_and_stops_with_last_storage_handle`) |
| Calls wait for capacity | Met (review item I2). A call awaits one of 16 permits without blocking its task, checks again that storage has not stopped, and holds the permit until its job has run, so a caller with many calls in flight waits and is not refused. `forty_snapshot_calls_wait_for_bounded_capacity` builds its handle with the test helper `worker_snapshot`, which takes the call capacity from the same constant as `Storage::read` (`SNAPSHOT_CALL_CAPACITY`); `public_snapshot_fanout_waits_for_the_production_capacity` (`8c4b534ea`, issue 418) goes through `Storage::read`, the handle that production builds: it stores 200 teams, then in each of five rounds opens a snapshot and runs 40 concurrent `list` calls on it, and fails if any call is refused. *Observation, not a bound:* the delta review of `44bae17a7` reported about 23 seconds for the five rounds; one run of that test alone (one test thread, the development profile, Linux, at `07fecf1ed`, which includes `8c4b534ea`) passed in 25.3 seconds, and another at `60a989256` (the same conditions, on a host at a load average of about 60) passed in 24.2 seconds. The control is kept as it is, because no shorter version has been shown to catch the capacity error; none was tried |
| 5 s idle release; a later call is `snapshot_expired` | Met for the default (`StorageOptions::default()` sets `snapshot_idle` to 5 s): the reader leaves its loop after that time with no job, and a later call answers `snapshot_expired` (`idle_snapshot_releases_the_only_reader_and_reports_expiry`). The same text answers a call whose reader channel closed for another reason, such as a view retired after a fatal read, so it does not tell them apart |
| Stop independent of the idle timer | Met: the last client's drop and the explicit close set the stop flag and interrupt the readers, and the reader loop checks the flag at least every 50 ms. `close_stops_idle_snapshot_independently_of_its_long_idle_bound` (a 60 s bound), `close_interrupts_a_statement_that_is_already_running`, `last_storage_drop_releases_lock_while_snapshot_metadata_remains` |
| Option B: a read after an earlier latch is served | Met (issue 389, option B as signed). Only a stop refuses a read (`Shared::readable`); a snapshot opened after a latch serves entity and operational reads, and a fatal observation since the request discards the result, retires that view and answers `storage_unavailable`. Writes stay refused. `a_prior_latch_serves_a_new_snapshot_entity_and_operational_reads`, `a_fatal_observation_since_a_snapshot_was_requested_refuses_its_calls`, `a_read_that_finds_damage_after_a_prior_latch_returns_no_value_and_is_counted`, `a_prior_latch_refuses_every_write_and_never_runs_its_callback`, `fresh_fatal_observation_while_queued_refuses_snapshot_open` |
| A typed or fixed error representation | Still open. All names are message text in `StoreError::Backend`, which has no variant for them (`snapshot_expired` is also the text of R127's expiry, which is a typed `SqliteError::SnapshotExpired` inside the adapter only): `overloaded`, `snapshot_expired`, `storage_unavailable`, `SQLite writer is closed`, `SQLite reader closed`, `SQLite reader operation panicked` (a panic of the read operation retires that view and keeps the worker) and `SQLite storage operation failed`. The texts `SQLite read admission overloaded` and `snapshot call queue is full` of the earlier draft no longer exist |

### Writer retry policy and the first event submission (the owner's ruling of 2026-11-27)

The owner approved on 2026-11-27 (the spec's record P, with record O for the leaf cleanup) the writer's retry policy that the
lead's writer-producer proposal called option 1, permission for the first real event submission before the T10 acceptance,
and the milestones below. The contract is the spec's R128, R27, "When a transaction fails" and the last bullet of
"Amendment 3". What is built, and what is not, is stated by "As built at `2a7cb83e4`" and "As built: the event producer, first form" after the vectors
below: step 1 is built: its source landed on `main` in PR #412 as `53136a969` on 2026-11-27 (PR #411 is a separate test PR).
Step 2 has a development implementation on the draft train (PR 420's event member, in its later encoded form) that is
not in PR 385 or on `main`; the callers of steps 3 to 5 are not built, and neither implementation is wired to the daemon.
Read the step table as an order of work and the as-built tables as the state.

What the policy says, in the owner's words where the record gives them: `BEGIN` is retried within the original deadline;
the callback is invoked at most once (`FnOnce`); `COMMIT` is retried only where SQLite documents that as safe, on the same
transaction, which is an exception to R128's earlier wording; the backoff is 50 ms doubling to 2 s for the retryable
class; a second consecutive `IOERR`, or 60 s without a commit on the retried transaction, latches, and idle time does not
count. Besides those, the proposal that the owner approved says that a storage fault inside an arbitrary callback body is
surfaced after its cleanup and not retried by the writer; that only a producer holding the original logical operation
rebuilds work, after a confirmed rollback; that an unknown commit outcome is reconciled against the database before
anything is submitted again; that a retry episode is one episode per logical request across submissions, with its deadline
and the suppression of claims; that expired withdrawable work gets one terminal answer and never commits; and that the
writer counts the actual rows and elapsed time (the `COMMIT` included) of each transaction once, with no double count on a
retry, so that a transaction over 1,000 rows or 20 ms is counted once in `writer_txn_over_bound_total`, an ordinary divisible
batch that the producer cut by time among them (I2, "As built: the event producer, first form"). A leaf's failed savepoint cleanup is
no part of the retry: the writer retries only its own `BEGIN` and `COMMIT`, and a cleanup that fails with a transient
result ends the worker as the nonfatal pair of record O (the spec's Amendment 3, "A transient cleanup", vector WP11).

| Step | What | Owner task | Verify |
|---|---|---|---|
| 1 | The common writer policy on the composed `Storage` path: the retained request across `BEGIN` retries with its original deadline, the callback run once, the same-transaction `COMMIT` retry where documented safe, the backoff, the two latches, one retry episode across submissions, the one terminal answer, the actual-work accounting. Its result is a frozen writer-policy commit of the adapter, which is a development input and not the T10 freeze | T10, the successor of the hardening task | WP1 to WP8, WP10 and WP11 below, with the rows and the receipts both checked |
| 2 | The first production submission of run events: compatible pending batches of several runs in one event transaction with per-run order and caller and project isolation, at most one waiting event transaction globally, the producer's retained input, and the cut at 1,000 rows or 20 ms checked between statements with the uncommitted tail kept. It needs step 1's frozen commit and is permitted before the T10 acceptance | T16, the first slice | WP9 |
| 3 | The named proofs of issue 127 on a head that integrates the commits of steps 1 and 2: the actual rows and their order, the transaction grouping, the 1,000 and 1,001 boundaries, the 20 ms boundary, the oversize count, real lock contention, `SQLITE_FULL` exhaustion, deadline withdrawal, consecutive `IOERR` and an idle control | issue 127, its proof task | Each vector, on rows and on terminal receipts |
| 4 | The T10 acceptance at its checkpoint. Steps 1 to 3 do not release it, and the freeze, the physical R142 corpus and issues 149, 167, 286 and 368 stay as the Status table lists them | T10 | The original acceptance conditions |
| 5 | The callers, each with the proof of its own work at its milestone, and no credit for a caller that is not implemented: bounded claims and retained finishes (T12); the remaining event collection and pruning (T16); scheduled expiry (T15); the importer's resumable chunks (T21, after the T10 landing) | T12, T16, T15, T21 | The tests named in those rows |

The vectors are proposed and are numbered for this plan only; the PR that builds each names its tests (the next table).

| Vector | Input | Must |
|---|---|---|
| WP1 | A second connection holds the write lock; a request is admitted | Its callback has not run while `BEGIN` is retried; it commits once the lock is released; the callback's invocation counter is exactly 1 |
| WP2 | As WP1, and the request's deadline passes during the wait; the lock is then released | One terminal answer, `overloaded`; the counter is 0; no row of the request ever appears, including after the lock is released |
| WP3 | A `COMMIT` fails where SQLite documents a retry as safe and the transaction is still active | The same transaction is committed on a later attempt; its rows appear once and the counter is 1 |
| WP4 | A `COMMIT` fails where the transaction is no longer active, or where no retry is documented as safe | The transaction is rolled back, the request ends with its classified outcome, the counter is 1, no row appears and nothing is resubmitted by the writer |
| WP5 | A commit whose outcome is unknown, and a producer that holds its input | The producer reconciles against the database before it submits again; the stored rows are the same as after one commit |
| WP6 | `SQLITE_FULL` on every attempt, a fake clock; and an idle writer | The attempts wait 50 ms, doubling to 2 s; no claim is granted meanwhile; at 60 s without a commit the writer fails; an idle writer never does |
| WP7 | A first `SQLITE_IOERR`, then a second in a row | The first is retried; the second latches; the first fatal cause stays, later mutations are `storage_unavailable` |
| WP8 | The same logical request submitted again by its producer during one episode | The episode continues: neither the 60 s count nor the suppression of claims starts over, and the original deadline applies |
| WP9 | Pending batches of several runs, 1,000 and 1,001 rows, and a 20 ms limit, with a real writer | One transaction holds the batches in per-run order and isolation; the cut falls at the bound and the tail is kept and then committed; rows, grouping and receipts match |
| WP10 | An indivisible transaction over the bound, retried | `writer_txn_over_bound_total` rises once; rows and work time are counted once |
| WP11 (issue 410) | Through `Storage::write`, a leaf's savepoint cleanup fails with a result of R128's retryable class (`SQLITE_FULL`, `SQLITE_BUSY` or `SQLITE_LOCKED`), injected at the leaf's cleanup call as the draft injects the corrupt one, the operation's cause being ordinary or the same result; the callback swallows the error it gets; a second request is queued behind it | Neither cause is fatal, so the pair is the nonfatal one of record O, and the writer retries nothing for it (it retries only its own `BEGIN` and `COMMIT`): the worker ends, the running and the queued request are answered `WriterClosed`, nothing is latched, no backoff or retry episode starts, the callback's counter is 1 and the queued callback's is 0, a later submission is refused, and after reopening neither request's rows are present and a new write commits. A cleanup or operation result of `SQLITE_IOERR` waits for the open question of the spec's Amendment 3 ("A transient cleanup") |

**As built at `2a7cb83e4`** (the source of PR #412, which landed as `53136a969` on 2026-11-27; the landing order was #400, #385,
#406, #411 (a separate test PR) and then #412; the signed spec and plan text were its first two commits; `2a7cb83e4` corrects the first head `f262da952`, which is superseded and is not
described here). Read from the source and run on Linux: the writer module 56 passed, 0 failed;
`sqlite::storage` 52 passed, 0 failed; the five public SQLite targets (`sqlite_public_writer`, `sqlite_storage`,
`sqlite_storage_bridge`, `sqlite_storage_crash`, `sqlite_task_cursor`) 24 passed, 0 failed, 1 ignored (the private child
process that the crash cases start). Test names are the library's; a "probe" is a throwaway test that was run and not kept.
No vector is accepted: nothing is wired to the daemon, no engine caller exists, and, when this paragraph was written, the head had no independent review, no
whole `fast` run that had passed and no hosted run recorded here (the first `fast` run on `f262da952` was interrupted to
make the corrections; a run on the corrected head was running when the builder recorded it, with no result known here).
"Status on 2026-11-27" below says what has happened since.
Five of the permanent tests below each failed (a behavioural red) on the first head's behaviour and pass after the
correction (the producer's private logs show the five failures; the five pass on the corrected head; they are the tests of the expiry of a
known noncommit, the writer-wide `SQLITE_IOERR` streak, the caught clean leaf refusal, the logical refusal after a caught
result and the streak's break by another SQL failure). No red is recorded for two controls that were kept with them,
`unknown_commit_remains_fenced_past_the_original_deadline` and
`a_known_commit_breaks_the_writer_ioerr_streak_between_requests`.

**Status on 2026-11-27 (issue 428; after #412 landed).** The text above and the table below were written at `2a7cb83e4`.

- *Landing.* PR #412 landed as `53136a969` (squash, 2026-11-27, sole parent `8e4b39223`, which holds #411's squash `a84ac78a6`
  and #414). The `crates/` tree of the squash, `e4a40d17b732f7f393b2c093196128b7fb40370f`, equals the `crates/` tree of
  `2a7cb83e4` and of the replay head `1406466ed`, so the as-built text and the tests it names describe the landed source.
  PR #411 landed separately as `a84ac78a6` (`test(sqlite): pin composed reader refusal guards and intent parity`) and carries no
  writer code; earlier revisions of this plan that say "PR 411's writer member" mean the writer that #412 landed.
- *Review and CI.* One independent code review of `2a7cb83e4` passed with conditions (the permanent-test gaps are issue 417, covered by
  PR 421, open when this was written) and one independent security review passed. A delta review of the replay onto
  `main` and of the as-built documentation followed. The hosted Linux `check` and `test` passed on the tested merge of
  #412's head `030af4b6c` (run 45969062931; `test` 9,243 passed, 0 failed, 11 ignored). Run again on the landed source on
  Linux for this revision: `cargo test -p orch-infra --lib -- sqlite::writer sqlite::storage::retained
  sqlite::storage::containment_tests` 68 passed, 0 failed; `--test sqlite_public_writer` 9 passed, 0 failed;
  `--test sqlite_writer_policy` 23 passed, 0 failed.
- *Lanes.* While one logical operation is retained, all four lanes wait, not only `Claims`: the spec's "While one logical
  operation is retained" under "Writer lanes" gives the rule, the source and the test that PR #443 added (issue 429; PR #455, issue 445, added its finite-future-deadline case). It is the
  built behaviour of `Writer::run` at `53136a969`, not a wording that R128 asks for and not a ruling of the
  owner, and it carries no latency guarantee.
- *Tests of R128.* The four names that R128 listed (busy, disk full, corrupt, idle) are mapped to the tests that exist, and
  to the missing coverage, in the spec's "R128's tests". PR #443 (landed as `7351003e3`) adds WP6's `SQLITE_FULL` exhaustion on a
  real event producer and a real idle control (`sqlite::events::tests::producer::a_full_disk_is_retried_and_then_fails_the_writer`),
  and the waiting of each non-claim lane (`sqlite::storage::retained::tests::every_nonclaim_lane_waits_for_retirement_and_keeps_its_original_deadline`).
  The `SQLITE_FULL` is injected and no disk is filled; PR #455 (issue 445) added the finite-future-deadline case (3,600 s) to the lane test, and no test advances a clock across the original deadline; and the issue 127 closure,
  the T10 acceptance and the daemon activation stay open.
- *Tests of R140 and of the rows that shared the prefix (issue 430).* The spec's "R140's tests" maps the fourteen names of R140: three exist by
  exact name, eleven are proposals, and the checkpoint, `Tool` and `Artifact` sources are exercised by an observation that the test supplies
  (no test makes a real checkpoint call fail, and no query, backup or export connection exists). The three daemon tests and the two CLI tests
  are not written: nothing wires the writer to the daemon, and there is no `orch db` command. R25, R26, R27, R98, R126 and R137 name real paths
  in place of `sqlite_writer::`; four of the five names of R26, six of the seven of R126 and all five of R137 bear no test, and the spec says so.
- *Still open, and not decided by this status:* daemon activation (nothing wires the writer to the daemon); the T10
  acceptance (the genuine freeze, the physical R142 corpus, and issues 167 and 286; issues 149 and 368 were open when this status was written and are closed since, by PR #442 and PR #427, and the process kill through `Storage` for events, leases and request keys, which neither covers, is carried by the crash gate of T27); WP9 (not built at `53136a969`; the event producer that builds it landed afterwards in PRs #420, #425 and #427 and is on `main`, "As built: the event producer, first form" and "As built: the event producer, encoded form" below, and nothing in the daemon calls it); the
  `SQLITE_IOERR` question of a leaf's cleanup and its count toward the consecutive-`SQLITE_IOERR` streak (issue 410); the
  untested reuse of a token after an admission refusal; and the tests of issues 417 and 408 (PRs 421 and 413, open when
  this was written).

| Vector | Tests at that head | State |
|---|---|---|
| WP1 | `sqlite::writer::tests::begin_busy_retains_the_original_callback_until_the_lock_clears` | Built. A second connection holds the lock, the writer's busy timeout is 0 and its clock is fixed; the callback runs once and its row is stored once |
| WP2 | `a_withdrawn_request_is_never_committed` (a real lock, the deadline passes inside `BEGIN`, `overloaded`, the callback never runs, no row, a later request commits), `a_deadline_that_passes_while_begin_waits_is_rechecked_before_the_body`, and `sqlite::storage::retained::tests::no_new_deadline_or_callback_after_one_terminal_withdrawal` | Covered by tests that were written before the policy and pass with it, and by one new test of the retained write; no vector-specific test |
| WP3 | `busy_commit_retries_the_same_transaction_and_counts_over_bound_once` | Built with `SQLITE_BUSY` injected at the commit through a test-only probe: three failures, waits of 50, 100 and 200 ms, one callback, 1,001 rows stored once. No real `COMMIT` busy is reached |
| WP4 | `unsafe_commit_failure_is_rolled_back_and_never_replays_callback` | Built for `SQLITE_FULL` at `COMMIT` with the transaction open: `RolledBack`, no row, callback counter 1, nothing latched. A `SQLITE_BUSY` `COMMIT` after the transaction has ended has no test |
| WP5 | `commit_error_after_real_commit_is_unknown_until_producer_reconciliation`, `sqlite::storage::retained::tests::unknown_real_commit_requires_same_caller_snapshot_reconciliation` | Built. The `COMMIT` really runs and the probe then reports `SQLITE_IOERR`: the outcome is `Unknown`, a resubmission is refused until a snapshot of the bound caller finds the row. The check in the test reads an entity; the event producer's own lookup of a run's sequence is not built |
| WP6 | `sqlite::writer::policy::tests::backoff_caps_and_sixty_second_episode_excludes_idle_time`, `retained_rebuild_uses_the_running_slot_before_a_full_queue_of_claims`, `expired_known_noncommit_releases_claims_between_producer_submissions`, `unknown_commit_remains_fenced_past_the_original_deadline` | Partly. The backoff (50 ms to 2 s), the latch at 60 s and no latch for an idle writer are shown on the episode with a supplied clock, not at this head by `SQLITE_FULL` on every attempt of a real writer (PR #443 adds that proof on a real event producer); a queued claim waits for the retained write's commit and then runs. Every lane waits while one logical operation is retained (as built: the daemon's work stays queued until the outcome is known or a known noncommit is withdrawn at its original deadline); at this head a permanent test shows only the `Claims` lane waiting (PR #443 adds the three other lanes, issue 429, and PR #455 their finite future deadline, issue 445), and the unit test of the episode's expiry gates claims alone with `Schedule::pop_allowed(false)`, which the worker does not use; an `Unknown` outcome stays fenced past that deadline |
| WP7 | `sqlite::writer::policy::tests::consecutive_io_errors_span_submissions_but_busy_breaks_the_streak`, `consecutive_io_errors_of_separate_logical_requests_still_latch`, `a_different_sql_failure_breaks_the_consecutive_ioerr_streak`, `sqlite::writer::tests::a_known_commit_breaks_the_writer_ioerr_streak_between_requests`, `sqlite::storage::retained::tests::native_observation_survives_port_conversion_and_spans_reconstructed_callbacks`, `caught_clean_leaf_refusal_may_commit_unrelated_work_despite_retry_class`, `a_callback_logical_error_is_not_retried_for_an_earlier_caught_storage_error` | Partly. The first `SQLITE_IOERR` permits a rebuild, the second latches `ConsecutiveIoErrors` and a first cause stays. The streak is the writer's: it survives the end of a logical request and is broken by a known commit, by another retryable code or by a non-retryable SQLite failure. A callback that catches a clean leaf refusal commits its unrelated work, and a later logical refusal is not retried for the earlier caught result. No test submits a mutation after this latch; the older tests show the refusal for other causes |
| WP8 | the same retained-write test, `expiry_withdraws_before_another_attempt_and_drop_releases_claims`, `expired_known_noncommit_releases_claims_between_producer_submissions`, `no_new_deadline_or_callback_after_one_terminal_withdrawal` | Partly. One episode across two rebuilt callbacks keeps the suppression of claims; a withdrawn token gets one terminal answer and a second submission is refused; the worker retires a known-noncommit token at its original deadline without another submission. A token whose `attempt` is refused at admission with `overloaded` stays usable while its original deadline is ahead or it has none (read from the source; no test). No test spans 60 s of submissions |
| WP9 | none | Not built at this head. The event producer is built on the next member ("As built: the event producer, first form") |
| WP10 | `busy_commit_retries_the_same_transaction_and_counts_over_bound_once`; a probe | Partly. A retried 1,001-row transaction raises `writer_txn_over_bound_total` once and reports 1,001 rows. A probe (not kept) found a 1,001-row transaction that was then rolled back counted once with 1,001 rows and none stored. At this head the 20 ms bound has no test (the event producer's tests of it are below). The vector's wording, "an indivisible transaction", is not narrower than the counter: the writer counts any transaction over the bound, a divisible event batch cut by time included (I2, below) |
| WP11 | `sqlite::storage::containment_tests::retryable_leaf_cleanup_ends_writer_without_starting_a_retry_episode`, `typed_leaf_cleanup_ends_writer_without_becoming_a_caller_panic` | Built for `SQLITE_BUSY`, `SQLITE_LOCKED` and `SQLITE_FULL` (and the `SQLITE_AUTH` pair of record O): the real Message leaf, its savepoint's `RELEASE` and `ROLLBACK TO` denied by an authorizer, and the cleanup result replaced at the leaf's typed cleanup boundary after the denial; all the assertions of the vector hold. An `SQLITE_IOERR` on either side is not exercised: the open question of the spec's Amendment 3 stands and the record-O default (nothing latches) is what the source does |

| Step | State at `2a7cb83e4` |
|---|---|
| 1 | Built as a development input: retained `BEGIN`, the same-transaction `BUSY` `COMMIT` retry, the classified outcomes, the retained write with its reconciliation, the one episode with its two latches and the suppression of claims, and the actual accounting. It is not the genuine T10 freeze. The spec's "As built: the retained writer" lists what it does, the three readings it builds and the one open question (`SQLITE_IOERR` in a leaf's cleanup) |
| 2 | Not built at this head. The event producer (coalescing of several runs, the cut at 1,000 rows or 20 ms with the kept tail, the reconciliation of an unknown commit) is the first slice of T16 and is the first caller of the retained write; it is built in "As built: the event producer, first form", below |
| 3 | Not done: it needs the integrated head of steps 1 and 2, and issue 127's proofs stay open |
| 4 | Held. The freeze, the physical R142 corpus and issues 149, 167, 286 and 368 stay as the Status table lists them, and the deterministic admission fixtures of issue 408 are not changed by this member |
| 5 | Not started; no credit goes to a caller that is not implemented |

**As built: the event producer, first form** (step 2 of the table above; the landing order is #400, #385, #406, #411,
#412, #413, then this member). This section states what the producer's source does and which tests show it. It is a
development member on the integrated head `07fecf1ed` (tree `a6fed4bed`): the event commit `9b55edbf4` (a new module and
its tests, all in `orch-infra`) on the corrected writer `2a7cb83e4`, the deterministic admission fixtures of issue 408
(`881dfa473`: `cfg(test)` options of the storage and tests, and a rewrite of `sqlite_public_writer`), the as-built writer
documentation of the section above (`bf65624a3`) and the landed #400 and PR 385 composition (`44bae17a7`). The traits, the
schema, authentication and provenance are unchanged: the frozen port is untouched, and the producer is a SQLite-only type
that submits through `SqliteStorage::retained_write` on the `Events` lane with no deadline. It is **not** the genuine T10
freeze, not the acceptance of issue 127, and not wired to the daemon or to a collector. The commit hashes in this section
and in "As built: the event producer, encoded form" record the development heads at which the source was read and the
tests were run; they are the evidence of those runs, and a section is found by its name.

Run on Linux at `07fecf1ed` in a private checkout with its default target, synthetic data and no network:
`cargo test -p orch-infra --lib sqlite::` 650 passed, 0 failed, 1 ignored (62 of them in `sqlite::events`: 7 planner tests
in `producer::tests` and 23 against a real writer in `tests::producer`); the six public SQLite targets 28 passed, 0 failed,
1 ignored (`sqlite_event_producer` 2, `sqlite_public_writer` 11, `sqlite_storage` 1, `sqlite_storage_bridge` 4,
`sqlite_storage_crash` 5 with its 1 ignored child process, `sqlite_task_cursor` 5); `storage_conformance` of `orch-engine`
475 passed, 0 failed. A whole `fast` run of the integrator on the same head **failed**: 6,654 passed, 2 failed and 10
ignored, the two being `remote_peers` cases of `orch-cli`
(`s19_failed_status_save_and_socket_loss_block_new_dispatch_until_recovery`, "Address already in use", and
`s23_sigterm_while_offline_keeps_the_remote_run_and_the_restart_resumes_it`, "timed out waiting for w1 online") on a host
that the gate recorded at a load average above 150 and up to 88 foreign builds; no SQLite test failed. That red stays on
record and no rerun is recorded here. No passed whole `fast` run, no hosted run and no independent code or security review
of this head is recorded here, and no vector below is accepted.

The producer in six parts. `EventProducer::new(storage, options)` starts one task that owns every numbered batch until the
writer has answered for it. `submit(EventBatch)` queues one run's batch and returns a `Submission` to await (dropping it
cancels nothing). `flush()` waits until what was submitted before the call has been answered. `close()` stops intake, lets
what waits be answered and ends the task.

| Part | What the source does | Tests at `07fecf1ed` |
|---|---|---|
| Coalescing | The batches that wait, of any number of runs, go into the next transaction together in arrival order, and a run's events keep their order. A transaction has one caller, so batches of another caller or project wait for a later one, and a later batch of a run does not overtake an earlier batch of that run that is still waiting. One producer has at most one transaction in flight, so the event lane holds the one transaction that R26 allows. An empty batch takes a turn for its lease check (one statement over no event). A batch whose lease is stale (`StaleEpoch`) or whose events are invalid is answered alone; it wrote nothing and the others still commit | `producer::tests::the_waiting_batches_of_several_runs_share_one_plan_up_to_the_bound`, `another_caller_waits_and_its_run_is_not_overtaken`, `a_lone_replan_takes_the_first_batch_only`, `an_empty_batch_takes_one_turn_for_its_lease_check`; against a real writer `waiting_event_batches_share_one_transaction`, `batches_of_another_caller_never_share_a_transaction_and_runs_are_not_overtaken`, `a_stale_run_is_refused_alone_and_the_others_in_its_transaction_commit`, `an_empty_batch_still_checks_its_lease`; public `collector_batches_of_several_runs_reach_storage_exactly_once_and_in_order` |
| 1,000 rows | A transaction is planned to hold at most 1,000 rows, counted from each event's stored form: one row, or two when the event's JSON is over the inline limit and its payload goes behind a blob reference. The cut falls inside a batch when it must, and exactly 1,000 rows are one transaction | `the_cut_falls_at_exactly_1000_rows_and_the_tail_is_kept`, `an_event_with_a_blob_costs_two_rows_and_the_cut_counts_them`; against a real writer `bulk_work_is_cut_at_1000_rows` (1,001 events become transactions of 1,000 and 1 rows, as the writer counted them), `exactly_1000_rows_is_one_transaction`, `events_with_a_blob_are_planned_by_the_rows_they_change`; public `a_batch_over_the_row_bound_commits_in_order_with_its_tail_in_the_next_transaction` |
| 20 ms | Time is read between statements, never inside one, from the producer's clock (`EventProducerOptions::now`, the system clock by default). A statement holds up to 32 events, or one event that has a blob, so that is the grain of the cut, and one statement always runs, so the tail keeps moving. A statement that runs long is not interrupted: the writer's own measure of the actual rows and elapsed time, the `COMMIT` included, counts the transaction once in `writer_txn_over_bound_total`, and the producer counts nothing. A time cut is taken at the start of a statement after at least 20 ms of statements, and the `COMMIT` follows, so a transaction that the producer cut by time is also counted by the writer (I2, below) | With a clock that moves 7 ms at every reading, `the_twenty_millisecond_cut_falls_between_statements_and_keeps_the_tail` (200 events become transactions of 96, 96 and 8 events, the first two cut by time); `a_cut_always_lets_one_statement_run_so_the_tail_keeps_moving` (a clock that jumps an hour: 32, 32 and 6 events); `an_atomic_operation_over_the_bound_runs_whole_and_is_counted` (a `COMMIT` that is busy twice: one increment of the counter for the transaction, not three); `the_real_clock_may_cut_anywhere_and_every_event_still_commits_once_in_order`. Where the real clock cuts is not tested |
| Retained tail | What is not committed stays in the producer, in order, and is planned again after the transaction that did commit; a batch is released from memory only on a known commit or a definitive refusal. What waits is bounded by three independent limits (at `9b55edbf4` only the first existed): 16,384 events (`queue_limit`, also the most one batch may hold), 4,096 batches (`batch_limit`, an empty batch counting as one) and 256 MiB of accounted bytes (`byte_limit`, metadata included). A batch that would pass one of them only because other batches wait is returned whole to its collector as `Rejected::Full` to be submitted again; a batch that could never fit, over the event limit or over the byte limit with nothing waiting, is returned whole as `Rejected::TooLarge`. An empty queue accepts no batch over a limit. (The earlier reading, that an empty queue accepts a batch of any size, is not what the source at `4e80198f0` does; "As built: the event producer, encoded form", below.) | `a_consumed_prefix_is_not_planned_again`, `runs_are_cut_together_in_order_and_each_keeps_its_own_tail`, `too_many_waiting_events_return_the_batch_to_its_collector`, `closing_answers_what_waits_and_refuses_more`; at `4e80198f0` also those of the "As built: the event producer, encoded form" table below |
| Rebuild | After a confirmed noncommit with a retryable cause (`retryable()` of the attempt), the same transaction is built again from the retained input as a new request of the same retry episode, so the episode's backoff and its suppression of claims carry over. A submission that the writer's queue refuses with `overloaded` (found by that text, because the port gives the answer no type) ran nothing and is tried again after 25 ms, doubling to 250 ms, without a limit. A failure that is not retried, while the writer has not failed, answers one batch alone: a coalesced transaction is planned again with its first batch alone, so that one batch's failure does not answer for the others | `a_known_rollback_is_rebuilt_from_the_retained_input_and_commits_once` (`SQLITE_FULL` injected once at `COMMIT`: two attempts, one receipt, rows once, claims not suppressed afterwards), `a_batch_behind_another_connections_write_lock_commits_once_it_is_released`, `an_admission_the_writer_refuses_is_kept_and_committed_later`, `a_batch_the_store_cannot_hold_fails_alone_and_the_runs_beside_it_commit` |
| Reconcile | After an unknown commit the producer reads the database before it submits anything again: with a snapshot of the bound caller it pages through the sequence range that each stored step carried and compares every event with its input (at `4e80198f0` it reads each stored row by its sequence number and compares its time, type and payload bytes with the frozen encoding of the input). All found and equal: the commit happened and the batch is answered from the attempt. None found: a rebuild if the cause was retryable, and if not a failure handled as in the row above. Some found, a stored event that differs, or a read that fails are errors: the check is tried up to five times (20 ms, doubling between tries) and the producer then fails closed. A producer that fails closed, or whose writer has failed, answers every waiting batch with its own counts, refuses later submissions (`Rejected::Closed`), and sets `SubmitFailure::outcome_unknown` on the batches of the transaction whose commit it could not establish, so a collector must not make a gap row of them on that answer | `an_unknown_commit_that_was_durable_is_reconciled_and_never_replayed` (one attempt, rows once), `an_unknown_commit_that_was_lost_is_rebuilt_only_after_the_database_says_so` (the commit loses its rows: two attempts), `reconciliation_answers_for_all_none_or_neither_of_the_expected_rows`, `a_commit_that_cannot_be_reconciled_fails_closed_and_says_its_outcome_is_unknown`, `a_second_consecutive_io_error_fails_the_writer_and_answers_every_waiting_batch` |

Those failures are injected through the writer's test-only commit probe (a `SQLITE_FULL`, a `SQLITE_BUSY` and a
`SQLITE_IOERR` result at `COMMIT`, and a commit that really runs and is then reported as failed); no real `SQLITE_FULL` or
`SQLITE_IOERR` is provoked. The reconciliation compares every row of each step that stored something, including a
duplicate that an earlier commit had already stored. A step that mixes such duplicates with new rows has no test, and how
that case is answered is a reading of the source that the review of this member has been asked about, not a finding.

**Time cuts and the over-bound counter (I2; decided 2026-11-27: the writer's accounting is kept and documented).** The
independent review of the integrated head (finding I2) recorded that every time-cut transaction raised
`writer_txn_over_bound_total` (11 of 11 in its probe). That follows from the two predicates. The producer cuts at the start
of the next statement once `now() - started >= 20 ms` (`producer.rs`, `MAX_WORK`), so the transaction has already run at
least 20 ms of statements; the writer's `rows > 1000 || work > 20 ms` (`writer/policy.rs`, `accounting`) takes `work` from
the end of `BEGIN` to the end of the attempt, the `COMMIT` included (`writer.rs`, `execute_episode`), less its own backoff
waits. At `07fecf1ed`, on Linux, a probe with the system clock (three runs of three batches of 2,000 events, test code
not kept) counted 8 of 8 time-cut transactions in each run, from 20.2 to 21.5 ms and 640 to 736 rows each, and counted the
uncut last transaction in one run of three (28.1 ms; the others took 8.9 and 13.7 ms and were not counted). The decision is
to keep the measured elapsed time, `COMMIT` included, so the counter means "a transaction that cost more than the bound,
whatever ended it" and cannot tell an ordinary divisible batch from an indivisible operation; cutting earlier by an estimate
would not make the count exact and would change the metric's contract. What this documentation now says: R27 and "The cut
(R27)" in the spec, the spec's "What the counter does not tell (I2)", ARCHITECTURE's "Retry and retained writes" and "Event producer" bullets
and the two rows above. What remained for source owners when this was written: the producer's module comment (`producer.rs`, the
"Actual work" paragraph) said that only an indivisible event over the bound is counted by the writer (it was corrected in
`4be221c11`, which `c51daaea9` and `4e80198f0` contain: it now says that the writer also counts a transaction that the
producer cut by time); the helper
`assert_counted_by_time_alone` compares the counter with the writer's own flag, so no test asserts the count of a real-clock
time cut either way (still so at `4e80198f0`); and AP8's phrase "lane waits are budgeted only while that count is zero" predates this reading.

The public surface is exported from `orch_infra::sqlite` and is used from outside the crate by the integration test
`sqlite_event_producer`: `EventProducer`, `EventProducerOptions` (`now`, `observer`, `queue_limit`; and, from `4e80198f0`, `batch_limit` and `byte_limit`), `EventBatch` (caller,
run, lineage, epoch and the numbered events), `Submission`, `SubmitFailure` (the error, the counts committed before the
failure, the number not stored and `outcome_unknown`), `Rejected` (`Full`, `Closed` and, from `4e80198f0`, `TooLarge`) and `TransactionReport` (the runs, the
events, whether time cut it, the attempts and the writer's accounting of the transaction that committed). None of them is
part of the frozen traits of `docs/TRAITS-FROZEN.md`.

| Step | State at `9b55edbf4` |
|---|---|
| 1 | As at `2a7cb83e4` above; the integrated head adds the producer, the admission fixtures and the landed #400 and PR 385 composition |
| 2 | Built as a development member and not wired: the six parts above. Outside it, by the producer's own header: the numbering of an event at its handover and the counters at start and stop (R61, R139), the collector's buffer and its gaps (R63), pruning, an export of the counter, and every caller in the daemon (T16's remaining work) |
| 3 | Not done. This member has a test close to several of step 3's cases (rows and their order, the grouping, the 1,000 and 1,001 boundaries, the 20 ms boundary by a fake clock (the count of a real-clock time cut is asserted by no test, I2 above), the oversize count, a real lock, a second consecutive `IOERR`). It has none for `SQLITE_FULL` on every attempt, for a deadline withdrawal (the producer's retained write has no deadline) or for an idle writer, and issue 127's proofs stay open |
| 4 | Held. At this head R127's rule (a read older than 30 s while the backlog is over 256 MiB) and the checkpoint job have no code (`6706afcbc` adds the rule's adapter side: "As built at `6706afcbc`" below); the physical R142 corpus, the final proofs of issues 149, 167, 286 and 368, R140, every method of the port, the landings and a current merge CI run all remain, as the Status table lists them |
| 5 | Not started; no credit goes to a caller that is not implemented |

**As built: the event producer, encoded form** (the producer's admission, retained form and reconciliation, step 2 of the table above, read
at a later development head than the first form). Source read: `4e80198f0ca62b45572ad7d86a7216f3eb500a40`, tree
`6db7dc8eb6d1636f1b9397b409773b0ecdba6cd7`, on `c3d009ab9` (test-only: the principal capacity refusal tests and a longer timeout in
one storage test), on the integrated candidate `c51daaea9` that this plan's earlier rows read. Two production commits follow
`c3d009ab9`: `d169de765` (the producer retains each event as its bounded encoded payload) and `4e80198f0` (the producer's vacant
queue allocations are released; one file, 7 lines). A third production commit follows `4e80198f0`: `c33037e98` (tree
`b8abd8e1bb169589d198222c00e8836b5e99a17e`, "refund encoded admission before terminal receipts"), which changes `producer.rs`
(18 insertions, 1 deletion) and the producer's tests (20 insertions, one new test) and nothing else; the `crates/` tree of
`c33037e98`, `d2c1c4086`, is the `crates/` tree of the draft train at `60a989256`, so the table below is read at the
sources of that tree, and the head `4e80198f0` names the representation and the admission that the correction leaves as
they were. From `c51daaea9` to `4e80198f0`: 9 paths under
`crates/orch-infra/src/sqlite/`, 703 insertions and 156 deletions, and no manifest, lock file, schema, authorization or trait
change. It is a *usable* development implementation and not the full candidate freeze (its freeze record says
`genuine_full_freeze: false` and `readiness: false`), it has no independent review, no hosted run and no whole `fast` run, and it
is read here, not accepted. It implements the contract that the lead decided on 2026-11-27 (decision I3, option 2): under the
defaults, one otherwise valid indivisible event whose encoded payload is at most 64 MiB, submitted to an empty open producer with
admissible metadata, fits whatever its JSON shape, while occupied capacity, closure, the event and batch limits and the metadata
charges keep their ordinary meanings. The spec's "As built: the event producer" states the behaviour; this section lists the
tests and the runs. A name in the table without a path is a real-writer test in `sqlite::events::tests::producer`; a name that
starts with `producer::tests::` is a planner or unit test in `sqlite::events::producer::tests`.

| Part | What the source does | Tests at `4e80198f0` |
|---|---|---|
| Encoded form | `events/encoded.rs` (`EncodedEvent`): the sequence number, the time in milliseconds, the static type and one `Box<str>` that holds exactly the whole-event JSON that `serde_json` writes for the physical encoder, kept with no fresh parse and no decoded tree. `Measure` serializes the borrowed original through a bounded writer: a checked size sum against the bytes still free, a fallible exact reservation that grows by at least 64 KiB at a time and never past the remaining allowance, and a shrink to the exact length. The charge of an event is that length plus a fixed record; the reserve for compact object entries and the charge of an array's capacity are gone | `producer::tests::every_shape_retains_exactly_its_serialized_payload_without_value_allocations` (a 160-deep nesting with escaped text, the largest `u64` and a negative fraction: the bytes equal the encoder's, an exact limit fits and one byte less refuses), `producer::tests::an_event_is_encoded_only_as_far_as_the_limit_and_the_limit_is_exact`, `producer::tests::the_blob_threshold_is_pinned_at_equality_and_the_size_is_the_encoded_one`; `encoded_dense_shapes_fit_the_default_producer_and_store` (an array of 100,000 objects and an array of four million numbers: each admitted by the default producer, stored, its text read back equal to the encoder's, and nothing retained afterwards) |
| The 64 MiB ceiling | An event of at most 67,108,864 encoded bytes is admitted by an empty producer with admissible metadata, as a scalar or in a container of any density; one byte more still fits the 256 MiB and fails alone at its statement. The default byte limit is four times `MAX_BLOB_BYTES` | `encoded_boundaries_store_exact_physical_bytes_and_isolate_plus_one` (exactly 67,108,864 bytes as a string and, in a second pass, as the last element of an array of four million numbers: stored as one blob whose file has that size and the hash of the encoder's bytes, the row's time and type those of the encoder, the rows in order 1 and 2; one byte more fails alone, `outcome_unknown` false and one event not stored, while the batch beside it commits), `producer::tests::the_default_envelope_holds_a_whole_blob_with_room_beside_it` (compile-time bounds), `a_full_queue_refuses_a_large_batch_without_encoding_it_and_the_default_envelope_takes_a_blob` |
| Independent limits | 16,384 events (also the most one batch may hold), 4,096 batches (an empty batch counts) and the byte limit are checked separately; the cheap checks precede any encoding and the walk stops at the first event that does not fit | `a_first_batch_over_the_event_limit_is_returned_whole_and_unencoded` (exactly the limit is admitted on an empty queue, one more is `TooLarge` and nothing is encoded), `empty_batches_saturate_their_own_limit_and_the_capacity_comes_back` (a limit of three batches, the fourth is `Full`, all capacity returns), `the_byte_limit_is_exact_and_a_batch_past_it_is_returned_whole` (a byte limit equal to what a batch holds admits it, one byte less is `TooLarge`), `an_event_that_does_not_fit_stops_the_encoding_of_the_rest`; `producer::tests::a_batch_is_measured_until_the_first_event_that_does_not_fit` |
| Metadata | The strings of the caller, the principal, the run and the lineage are charged at their capacities, eight times, and one run name for each of the first 1,000 events; a fixed amount per batch and per event. An empty batch has a charge of its own | `metadata_alone_counts_against_the_byte_limit`, `producer::tests::metadata_is_counted_at_its_capacity_even_with_no_events`, `producer::tests::metadata_reserves_eight_copies_and_only_1000_statement_names`, `metadata_spares_are_refused_and_payload_spares_are_released` (nine shapes: an oversized spare capacity in the run, the lineage, the project or the principal is refused whole as `TooLarge`, the returned batch equals what was sent and the run string keeps its pointer and capacity; a spare inside an event's time or data is not retained and the event is stored), `principal_spare_fields_are_returned_whole_before_encoding` |
| Ownership of a refusal | The caller's batch is owned until the final admission under the lock; every refusal, the concurrent recheck included, returns it whole, and its allocations are dropped only after admission | `same_batch`, the helper of the tests above (the returned batch equals what was sent); `concurrent_submissions_never_exceed_the_byte_limit` (six rounds of 16 threads against room for one: exactly one is admitted and the held bytes never pass the limit) |
| Release | One charge per batch, shared by the queue entry, the plans, the attempt copies and the reconciliation's rows; it is returned when the last owner drops. The queue's vacant slots are shrunk when batches leave. At `c33037e98` the answer of a batch follows the refund: an answer whose outcome is known is kept in the charge and sent once, by the last owner, after the bytes are returned; an answer that says the outcome is unknown (`outcome_unknown`) is sent at once, with the bytes still charged while a callback or reconciliation owns part of the batch | `capacity_returns_after_completion_after_a_refusal_and_exactly_fills_again`, `encoded_budget_stays_charged_through_in_flight_ownership` (a plan whose commit is complete but whose observer has not returned still holds the bytes and the next batch is `Full`; after the release the producer holds `(0, 0, 0)` and admits it), `a_panic_in_the_clock_answers_every_held_batch_and_closes_intake`, `a_panic_in_the_observer_answers_what_it_completed_and_then_everything_held`, `observer_panic_on_the_last_commit_closes_intake`, `a_waiting_flush_is_released_when_the_task_panics`; from `c33037e98`, `an_answered_encoded_batch_has_already_returned_its_exact_budget` (a byte limit equal to what one batch holds; 1,000 rounds of submit, answer and submit again, each admitted, with `(0, 0, 0)` retained after every answer; the 1,000-round regression failed before the correction, 0 passed and 1 failed, with 2,437 bytes still charged at an early success receipt, as the correction's logs record) |
| Reconcile | The frozen bytes of the input are compared with the stored row, one row at a time, through the original read view; a blob is loaded and checked by `DiskBlobStore`; no decoded tree and no callback replay. All, none or a divergence, as before | `encoded_unknown_commit_reconciles_escaped_ordered_numbers_and_accepted_nesting` (a commit that ran and was reported as failed: one attempt and no replay; the stored time, type and payload equal the encoder's for a 160-deep nesting with escaped text, the largest `u64`, `-0.0` and `1.25e100`, a map of two keys in insertion order and a timestamp with an offset), `an_unknown_commit_that_was_durable_is_reconciled_and_never_replayed`, `an_unknown_commit_that_was_lost_is_rebuilt_only_after_the_database_says_so` |

The shared retained path is one source change and not a second implementation: `RetainedWrite::attempt` and the producer's
typed `attempt_unit` run the same code in the same episode, the producer's callback reaches the database through the same
`event_operation` guard and the same single `append_rows` statement implementation as the earlier `append_with_blobs`, and a
reconciliation reads through the `SqliteStorage::snapshot` helper that `Storage::read` also uses. The writer and storage tests
that cover those paths are in the count below. Between `c51daaea9` and `4e80198f0` the only change to a test outside
`sqlite::events` is the longer timeout (3 s to 30 s) in `an_idle_owned_snapshot_expires_and_releases_backlog_before_its_idle_timeout`.

Runs, on Linux in the producer's private checkout, with its default target, synthetic data and no network, as its logs record
them for the tree `6db7dc8eb`: the focused library run of `sqlite::events`, `sqlite::storage`, `sqlite::writer` and
`sqlite::read_pool` of `orch-infra`, **225 passed, 0 failed** (92, 61, 59 and 13), `cargo clippy` on `orch-infra` and the
formatting check exit 0, no output. The new dense-shapes test, run against the producer of `c3d009ab9` before the change, failed
at its first shape, the array of 100,000 objects (`dense shape 0 below 64MiB must fit`). The correction `c33037e98` has its own
record: **226 passed, 0 failed** in the same focused set on its tree `b8abd8e1b`, clippy and the formatting check exit 0, and the
new regression test 0 passed and 1 failed on the producer of `4e80198f0` and 1 passed on the corrected one. This revision ran the
focused set again on the `crates/` tree `d2c1c4086` (a clean worktree of `60a989256`, Linux, synthetic data, offline, private
`HOME`, `TMPDIR` and `XDG` directories, default target on the task's volume): **226 passed, 0 failed** with `--test-threads=16`.
Two earlier runs of the same set with the default thread count, on a host at a load average of 60 to 90, failed: 225 passed and 1
failed (`waiting_event_batches_share_one_transaction`, whose assertion is that a transaction of 12 rows took less than 20 ms of
wall time) and 216 passed and 10 failed (each answered `overloaded`: nine event-producer tests at their own lease claim, a
`Claims` transaction, `events/tests/producer.rs:95`, and one retained-writer test at a reconciliation read,
`storage/retained.rs:640`). The first test passed alone straight after. These are failures of host load in tests that depend on wall time, recorded and not hidden; none is
a failure of an assertion about the receipt or the charge. The earlier counts stay with their
heads and are not restated here as results of this one: 650 passed, 0 failed, 1 ignored in the SQLite library and 28 passed in
the six public targets at `07fecf1ed` (above, with that head's failed whole `fast` run), and 160 passed, 0 failed in the
`sqlite::events::`, `sqlite::read_pool::` and `sqlite::storage::` modules at the test-only `c3d009ab9`, a development result and
not a qualification. The regression tests of the retained writer that `6b75babbe` added are preserved: the test suffix of
`storage/retained.rs` has the same SHA-256 (`fccaac74…9268`) as at `6b75babbe`, although the production part of that file
changed, and `writer/tests/mod.rs`, `writer/tests/retained_bounds.rs` and the changelog fragment
`sqlite-retained-writer-regressions.fixed.md` are the same blobs. Not run: a whole `fast` gate, a hosted run, `cargo xtask
changelog check`. Held and unchanged: an independent code review and a targeted security review of the changed representation,
the physical R142 corpus, the final proofs of issues 149, 167, 286 and 368, R140, the T15 schedule of the checkpoint job and of
pruning, the first Task-authenticated production call (issue 397, deferred), the full candidate freeze and the T10 acceptance.
The producer's own comments still describe the earlier representation in places (the figure of `QUEUE_LIMIT_BYTES`, the charge
of a batch's strings, the field comment of `Pending::consumed` and the claim that capacity is all returned when the producer
fails, among others); the corrected comment text is supplied to the builder as a comment-only patch (regenerated against the
`producer.rs` of `c33037e98`, which also states the receipt order) and is not applied by this documentation.
A later source head, `7776c83b9` (tree `68fe13f1e`, the builder's), changes only the two fixed amounts of the charge and
adds tests. At that head the per-batch amount (`BATCH_FIXED_BYTES`) and the per-event amount (`EVENT_FIXED_BYTES`) are a fixed
reserve that does not depend on the encoded size of any event. It covers the spare capacity of the vectors that a batch keeps
alive at the same time (the queue, the plan, the steps and the expected rows), because a vector that is filled by pushes can
hold more slots than it has elements (257 elements give a capacity of 512). The permanent test
`metadata_reserve_covers_simultaneous_vector_capacities` is the test of that reserve. The limits are unchanged: the byte limit of
256 MiB and the allowance of 64 MiB for one indivisible event, whatever its JSON shape, are as described above, and the
statements of this section about the metadata charged at capacity and the answer that follows the refund hold as written. The
counts and runs of this section stay with `4e80198f0` and `c33037e98`, and no run of `7776c83b9` is recorded here. A second
comment-only patch, against that head, corrects the comments that the first one left describing the earlier amounts.

| Step | State at `4e80198f0` |
|---|---|
| 1 | As at `2a7cb83e4` above |
| 2 | Built as a development member and not wired: the six parts of "As built: the event producer, first form", with the admission, the retained form and the reconciliation of the table above. Outside it, unchanged: the numbering of an event at its handover and the counters at start and stop (R61, R139), the collector's buffer and its gaps (R63), pruning, an export of the counter and every caller in the daemon |
| 3 | Not done, as at `9b55edbf4`; issue 127's proofs stay open |
| 4 | Held, as at `9b55edbf4` |

**As built at `6706afcbc`** (R127's adapter side and the physical R142 corpus: one commit on `07fecf1ed`, a development
input and not the T10 freeze; the landing order of the members above is unchanged and is not decided here). It is read from
the source and from this writer's own runs on Linux (a private checkout, synthetic data). It is not a review, not a whole
`fast` run, not a hosted run and not an acceptance, and it changes no trait, schema or authorization (Amendment 3's
runtime obligations are unchanged, and no first Task-authenticated call is credited to issue 397).

| Part | State at `6706afcbc` |
|---|---|
| R127, adapter side | Built, development, unwired (the spec's "As built: active readers and the passive checkpoint"). One passive checkpoint on a connection of its own (`ReadPool::checkpoint`, `SqliteStorage::checkpoint`) takes the backlog from its result, (frames in the log minus frames copied) times the page size, never from the file's length. Only a read that is both older than 30 s and under a backlog over 256 MiB is interrupted, each once (both limits strict). The interruption stops the running SQL, refuses SQL that begins afterwards (a progress handler), retires that one owned snapshot and answers `snapshot_expired`; a fatal observation made first is answered as `storage_unavailable`. It is a typed `SqliteError::SnapshotExpired` inside the adapter and the same message text at the port, so the error representation of record N stays open |
| R127, not built | The checkpoint job's schedule (the 5-minute timer and the 64 MiB size trigger), R137's outcomes and its repeat report, `wal_bytes` and `wal_file_bytes`, `orch db status`, the commit callback's running count of the backlog (R126) and every daemon call. T15 owns these, and the daemon does not call `checkpoint()` |
| Permanent tests of R127 | `read_pool::active::tests::exact_limits_do_not_expire_and_interruption_counts_once`; `read_pool::tests::active::actual_checkpoint_backlog_and_age_are_both_required_and_a_long_file_is_not_backlog`, `::both_limits_interrupt_current_sql_and_caught_interrupt_cannot_publish_a_snapshot` and `::sql_started_after_checkpoint_expiry_cannot_escape_the_progress_guard`; `storage::tests::active_checkpoint_expiry_interrupts_owned_sql_and_expires_that_same_snapshot`. R127's requirement row still names `sqlite_pool.rs` tests that do not exist; these are the tests that carry the rule. Run here on Linux (`cargo test -p orch-infra --lib`, the development profile): `sqlite::read_pool::` 11 passed, 0 failed; `sqlite::storage::` 57 passed, 0 failed (the 11 include the four tests of `read_pool`, the 57 the one of `storage`) |
| What those tests do not show | Each one makes a read older by a test-only call (`ActiveReads::age_for_test`) or injects a clock, so none waits 30 s and none shows the 30 s on a real clock. The 256 MiB is real: they write 257 or 258 MiB. The checkpoint's other outcomes (busy, partial copies) and a non-WAL database are not tested. The two numbers stay proposals that no measurement backs (R127) |
| Physical R142 corpus | In the source as test code only (`cfg(test)`, crate-internal): `r142_harness.rs`, a borrowed bridge that drives `EntityWrite`, `EntityRead` and `NativeSql` on one transaction with the shared `FailureState`, and `r142_physical.rs`, the retained corpus of 22 tests (about 1,600 lines) that reads stored-data damage as the typed `SqliteError::EntityCorrupt`. It compiles, and all 22 tests passed in this writer's Linux run in the development profile (one filtered `cargo test -p orch-infra --lib` run of the `sqlite::read_pool`, `sqlite::r142_physical` and the one `storage` test above: 34 passed, 0 failed, in 2,513 s; the longest test, `every_variable_slot_and_environment_name_crosses_the_physical_disk_boundary`, which writes 64 MiB and 128 MiB values through every text slot, also passed alone in the release profile in 85.5 s). No public `Storage` adapter registers, and the whole current qualification of the corpus (a whole `fast` run, a hosted run, the review) is not recorded here; no count of the corpus beyond that run is claimed |
| Held | Unchanged: the freeze, the final proofs of issues 149, 167, 286 and 368, R140, every method of the port, the landings, the checkpoint job and its schedule (T15), a current merge CI run and the T10 acceptance. This member does not change the event producer or its held items |

### One retained producer per writer (issues 448, 456 and 461; before T16 wires the event producer)

The contract is in the spec's "One retained producer per writer", first read at `77376bc6c` (`main`) and at `fd81eacc3`, the
frozen head of PR #459, and read again on `main` at `23ffdc838` (after #459, #466, #486 and #488), which is the state this
subsection describes. This subsection is what T16's PR does with it. It is not a ruling of the owner, and no code
changes with this documentation.

**T16 must (the wiring PR's acceptance, each with a named test or a recorded check):**

| # | Obligation | Check |
|---|---|---|
| 1 | Construct one `EventProducer` for each `SqliteStorage` and name its owner in the wiring (the daemon object that opens the storage). A second construction on the same storage is a defect of the wiring: the PR either makes it impossible by construction or refuses it, and a test or the type shows that | The named test or the type; a grep of the PR shows one production call of `EventProducer::new` |
| 2 | Give no other owner a `RetainedWrite` that rebuilds after a rollback. Ordinary writers use `Storage::write` or `Writer::submit`, one attempt each. A second rebuilding owner needs the design that the spec names (its own slot or a bounded queue, and the meaning of R26, of the sixty seconds and of the claim suppression for two open episodes) in a spec, plan and ADR change before its code | Review of the PR's callers of `retained_write` and `submit_episode`. On `23ffdc838` the non-test callers of `retained_write` are `EventProducer::drive` (the one retained producer) and `Storage::write`, which attempts once and drops its token; `submit_episode` is called by `RetainedWrite::attempt_unit` and `WriterHandle::submit_after` |
| 3 | Keep the producer's retained write at `deadline = None`. A deadline would make the episode withdrawable at it, and a refusal after `Episode::prepare` leaves its outcome `NotStarted`, which is a known noncommit | The producer's call (`drive`); no change to `retained_write(.., Lane::Events, None)` |
| 4 | Reconcile an unknown COMMIT only through `reconcile_encoded`. The generic `RetainedWrite::reconcile` has no `commit_attempted` guard (issue 456 N1, now issue 465, open): a first production caller of it adds that guard, or an equivalent enforced precondition, in the same PR, and the guard is tested with a preexisting durable identity and a callback that never reached COMMIT, and with a genuine uncertain COMMIT that still reconciles | A test of the guard if the generic reconcile gains a caller; otherwise a grep that it has none (on `23ffdc838` its three callers are tests in `storage/retained.rs`) |
| 5 | Treat `overloaded` from `attempt_unit` (an admission refusal, no callback run) as pressure while no failure is latched, and anything else as the answer. Do not read an `overloaded` inside a receipt as pressure: it is the deadline withdrawal and never commits | `an_admission_the_writer_refuses_is_kept_and_committed_later` stays green; no new retry arm |
| 6 | Leave the failure rules as they are: a failure that the producer cannot answer per batch (`fail_all`) closes its intake and answers every held batch, and a writer that has latched a failure ends the producer the same way (the spec's "Reconcile") | Issue 456's tests, on `main` since #466 and green: `sqlite::events::tests::producer::two_uncertain_commits_reconciled_absent_report_storage_unavailable_and_close_intake` (M6, M7) and `sqlite::storage::retained::tests::refused_commit_does_not_mark_the_next_callback_only_failure_as_committed` (M2) |

**What this contract does not prove, and who owns each part (status on 2026-11-28, `main` at `23ffdc838`):**

| Open point | Owner | State |
|---|---|---|
| Issue 456: a refused COMMIT must not make the next callback-only failure reconcile as committed (M2); two uncertain commits reconciled absent report `storage_unavailable` with the right counts and cause and close intake (M6, M7) | Landed (#466, `4d415afb9`; issue 456 closed) | The three tests are on `main` and pass; with `p.commit_attempted = false;` commented out of `Episode::prepare` the selection of `sqlite::writer`, `sqlite::storage::retained` and `sqlite::events::tests::producer` fails that one test (146 of 147 pass; 136 of 136 passed without the line at `fd81eacc3`, before #466). With the fatal branch of `Reconciled::NotStored` disabled (M6) the producer test fails at its intake-closed assertion, and with the latch check removed from the `Rebuild` guard (M7) at its public-error assertion |
| Issue 465: the generic `RetainedWrite::reconcile` has no `commit_attempted` guard (issue 456 N1) | The SQLite goal lead routes the guard to the writer builder before the first production caller is admitted (T16 obligation 4) | Open. No production caller: the three callers are tests of `storage/retained.rs`, and the event producer uses `reconcile_encoded` |
| Issue 461 item 5 (the uncertain-COMMIT wording after a refusal) | This documentation, from the spec's "One retained producer per writer"; the comment in the header of `writer/policy.rs`, which still lacks the qualification, goes to the writer builder before a retained caller depends on a resubmission | Answered by the code and one throwaway test, repeated on `main`: the refusal cannot reach an unreconciled `Unknown`, and after a reconciliation that established absence the outcome was already a known noncommit. Issue 461 stays open: items 1 and 2 (the prior-latch expiry test and the snapshot comment), 3 (the comment of the occupied-slot test) and 4 (the vestigial queue of a policy test) are test and comment work that changes no sentence of this contract |
| Issue 457: three writer cleanups that #459 split off | The After 1.1 backlog | Not part of this contract |
| A second retained producer | Nobody; unsupported | New design, not scheduled |

**The throwaway test (not kept, not a permanent proof).** At `fd81eacc3`, and again on `main` at `23ffdc838` (the patch applied
unchanged and printed the same states), in `sqlite::writer::tests`, one test built an
`Episode` with a 200 ms deadline, called `mark_commit_attempted`, set `WriteOutcome::Unknown` with `RetryCode::Io` and
registered a failure; a second episode was registered and submitted into the continuation slot with `submit_episode`.
Results, from the run in the task's private checkout: after the deadline `check_retry_timeout` kept the unknown episode
registered; after the outcome was set to `RolledBack` (what `reconcile(false)` does) a `submit_episode` of it returned
`Overloaded`, its snapshot was `(NotStarted, None, zero accounting)`, `commit_attempted` was false and it was still registered; after the
deadline it was withdrawn (`retrying()` false) while `claims_suppressed()` stayed true for the other episode. The test
drives the writer's `submit_episode`, not a `RetainedWrite`: the token's own gate is read from `attempt_unit` and the existing
`authoritative_absence_releases_an_unknown_token_after_its_original_deadline` shows it for an unknown outcome. The reuse
of a token after a slot refusal has no test.

**Runs for this subsection.** On Linux, in a private checkout, with synthetic fixtures only.

- At `fd81eacc3` (before #466): `cargo test -p orch-infra --lib -- sqlite::writer sqlite::storage::retained
  sqlite::events::tests::producer` ran 136 tests; in the first run one failed
  (`incomplete_unknown_callback_preserves_its_failure_and_releases_every_charge`, an `overloaded` where the test expected
  success, with the host's load average near 277); it passed alone three times and the same set passed 136 of 136 with the
  reset removed. That red stays on record.
- On `main` at `23ffdc838`, the same command: 147 passed, 0 failed, 0 ignored. With `p.commit_attempted = false;` commented
  out of `Episode::prepare` (M2): 146 passed, 1 failed, `refused_commit_does_not_mark_the_next_callback_only_failure_as_committed`.
  The producer test `two_uncertain_commits_reconciled_absent_report_storage_unavailable_and_close_intake` alone: passes; with
  the fatal branch of `Reconciled::NotStored` disabled (M6) it fails at its intake-closed assertion, and with the latch check
  removed from the `Rebuild` guard of `EventProducer::reconcile` (M7) at its public-error assertion. Every mutant was reverted.
- The throwaway test above, applied to `main`: 1 passed.
- The first head of this documentation (`582b1d388`) had a local `fast` run that failed one test of 3,840 (`concurrent_submissions_never_exceed_the_byte_limit`,
  the fixture of issue 435, which passed alone three times), and a hosted `test` job that failed (run 46055545712:
  `daemon_dispatch_wake::writes_during_the_wake_tick_cause_at_most_one_follow_up_tick` and
  `daemon_engine::maintenance_reconciles_adopted_death_without_dispatch_or_idle_publication`, two tests of the daemon on
  the file backend that a documentation-only head does not touch). Both reds stay on record; the pull request records the
  disposition of the hosted failure and the runs of each later head.
- No review of the code is claimed here: the contract is read from the source, and nothing in it is accepted for T10.

## Status

| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T1 | first-review text (`f5637805`) implemented in PR 1, `fast` on macOS, and merged to `main` by the owner on 2026-11-21; this corrected text, with the review corrections and the owner's answers, implemented in a follow-up PR that is pending its reviews and the owner's merge. The spec is not approved by either. Since then: the corrected text (`34b3b4d`) reached `main` on 2026-11-22 (UTC) through that follow-up PR, and the deferral of 2026-11-21 (the spec's record I) is implemented in a further documentation PR that is pending its review and its merge. This revision changes no document's status (`draft`, `proposed`) |
| T4 | dropped as an entry step by the owner on 2026-11-21. The first run of 2026-11-20 is recorded in [issue 98](https://github.com/yuxhuang/r-orch/issues/98) and is not delivered to `main` |
| T4b | deferred by the owner on 2026-11-21; not run |
| T10, first slice (writer and read pool) | implemented as an unwired foundation in PR 122 (`b67dbfa44`, merged 2026-11-22), with its module tests; the follow-ups of issue 127 are open. When this row was written the rest of T10 was pending behind T8b and T8c ("T10 split"). The deadline recheck and the bounded writer tests landed in PR 154 (`8afcbe02`, 2026-11-22). Status on 2026-11-28, read from `git log` of `main` at `23ffdc838` and not a result of this plan: the rest of the writer side is on `main` as well, still unwired and not accepted for T10 (the daemon uses files): the bounded writer test cleanup (PR 169, `eb79206d3`), the executor's hooks (PR 188, `9a68f1498`) and the shared failure state (PR 249, `8d79976df`), the lease adapter (PRs 211, 232 and 245), the composed `Storage` adapter (PR 385, `964e1144a`), the retained writer (PR 412, `53136a969`) and the event producer (PRs 420 and 427). Issue 127 and the T10 acceptance stay open |
| Linux-only scope (the spec's record K) | recorded on 2026-11-23 by one documentation PR that is pending its review and the owner's merge: the spec, this plan, ADR 0017, the lease prototype's spec, the spec TEMPLATE, ARCHITECTURE §13 and AGENTS.md carry it. Nothing was verified on a platform: no code and no CI changed. The first independent review blocked its first head on three points: the Linux sleep/wake check of R42 (issue 110) was withdrawn with the macOS one, R16, R88 and R136 still named macOS, and the branch conflicted with `main`. The text was corrected for them and re-synchronised with `main` keeping the rows of both sides; the corrected text awaits a second review |
| T10, internal lease adapter (issue 127 N1 to N4) | The damaged-epoch error, the lane mapping and the optional request deadline are merged (#211, #232) and their tests pass on Linux at `af0d43acd` (38 module tests). The spec's "Lease adapter contracts" records them. Unwired: only the module's tests call the adapter, and the daemon uses files. This documentation landed as PR 245 (`65b82036d`, 2026-11-23), and PR 385 registered the shared lease suite on SQLite (`sqlite_leases_v2`, 20 cases). Pending behind it, as of 2026-11-28: T12's start and finish transactions, T13's cancel, pause and permanent-failure transactions, T15's lease expiry, T22's `orch db verify` and the R128 and R140 classification. Issue 127 is not closed |
| T10, internal goal adapter | Goal reads, creates, updates, deletes and explicit moves use the executor. Module tests cover transactions, snapshots, reopen and file-reference behavior for supported inputs. Public conformance still requires timestamp and counter compatibility, imported position high-water marks, and text/blob handling. PR 318 (open, draft, on PR 317; kept as history, its repaired content carried by the combined PR 338, which is not merged yet) rewrites the adapter as a thin wrapper over the native driver of the four families (step 4 of "T10 entity format"): the staged helper `entity.rs` and its tests are removed, both timestamps are kept as their original strings, and the module's 15 tests cover the lifecycle against the file reference and a reopen, original timestamps, snapshots, rollback and its poisoning, counters, a drifted projection, a zero stored and a zero surviving position, an unsupported detail format, and a caught failure inside a move and inside a delete. On its first head `730b72c4e` the module had 10 tests, which passed; the five last named are registered in the source of `e45545321` (branch `feature/sqlite-goal-adapter-repaired-public`) and the module's 15 tests passed in the integrator's Linux run on a private head whose code is byte-identical to that source (counts in the row below). What it still lacks is listed in the row below. |
| T10, task format v1 | defined in the spec, this plan and ADR 0017 decision 26 by one documentation PR, with the write rules and the task-corruption rules (`TaskCorrupt`, fatal under R140). PR 226 implements only a first slice: the strict, complete `Task` byte and detail codec (`crates/orch-infra/src/sqlite/task_format`, with the R97 text chunk helper) and the guarded migration 003 (`task_details`, `task_blob_refs`). It is inactive: nothing calls it and the daemon still uses file storage. It is not the split that "T10 task mapping" names under (a) and (b): the codec already covers the reference tags and the root reference, and every SQL operation waits for the next slice. The codec validates the typed task (priority, status, attempts, canonical timestamps) before it reads any slot object, so a damaged inline value is `Invalid` even when a slot's object is unreadable (review finding I4); a detail that is itself behind root references is reconstructed first, because its typed fields cannot be read before that. Verified on Linux by the author's focused runs, whose regression test `typed_damage_is_checked_before_any_slot_object_access` failed before that fix (0 passed, 1 failed) and passes after it: the `task_format` module tests (19 passed, 0 failed, 0 ignored), `sqlite_migrations` (15 passed), `sqlite_schema` (7 passed), `cargo clippy` on `orch-infra` with warnings denied, `cargo fmt --check` and `git diff --check`. Earlier results stay on record: the first head's `fast` run was interrupted (exit 130) after 1,587 passes and 36 failures had been observed, from the umask 0002 fixture of issue 137, and counts as failed and incomplete; a migration test run failed 14 passed and 1 failed, two compile errors (a `usize` type error and a `HookError` error) and a clippy non-octal literal were fixed on the way; the hosted run on the first head was cancelled by the corrective push; the head `eec91899b` passed hosted Linux check and test and was CLEAN, which does not cover any later head. Pending for the final head of PR 226: the terminal delta code and security reviews, hosted Linux check and test SUCCESS and CLEAN on that exact head, the lead's renewed recommendation and the owner's merge; the inactive codec is not a Storage acceptance. Of the tests that R141 names, the two migration tests and the two codec tests on `proof` and `workspace_mode` exist; the codec ones are module tests of the library, not yet in `tests/sqlite_task_format.rs`. Not implemented, built or tested: create, update, delete and move; the `tasks` row, membership and `blobs` projections and their checks on read; hydration in the selecting snapshot or unit of work; `SqliteError::TaskCorrupt`, its classification and the writer latch (issue 127); the Storage port; the shared entity suites on SQLite; the importer and backup obligations. T11 owes one row-only registration change in `blob/references.rs` whose carrier is open, so that each object is published once before its `blobs` row is registered. 1.0's recorded proof is imported into the detail and `legacy_proof` stays NULL, which the lead ruled on 2026-11-23 to stand as documentation groundwork; the T21 implementation and live-copy access stay held. Backend equality is claimed only for texts of at most 67,108,864 bytes; the split of a larger text is checked on SQLite alone (R141). Whether decode shall refuse non-canonical bytes whose length and hash match is open in issue 227; two minor review conditions, the transaction-hook coverage of migration 003 and typed preflight errors under issue 127 and R140, are tracked in issue 229. |
| T10, entity format v1 (Agent, Team, Run, Context, Config, Template, Goal) | defined in the spec, this plan and ADR 0017 decisions 27 and 28 by the documentation commits that PR 251 carried (`67f64741b` and `3a7434527`, between the codec's commit `5c3ff60eb` and the code correction `cae0ca499`). The byte and detail codec of the Agent, Team and Run families landed inactive as PR 251 (squash `33fd6fcbf`, on the squash `f181be7af` of PR 226; "First slice: the byte codec" above), and migration 004 stages the schema ("Second slice: migration 004, schema only" above): on Linux, the author's `fast` run on its first head `5c3ff60eb` passed (5,470 passed, 0 failed, 7 ignored, including the 15 module tests), and the writer of this text and the code reviewer each ran the 15 module tests on that head (15 passed, 0 failed). On the corrected head `cae0ca499` the module has 22 tests, and the fact-checker's focused Linux runs passed 22 (`agent_team_run`), 19 (`task_format`) and 22 (`import_index_reference`), 0 failed. Earlier results stay on record: 23 compile errors before the module existed, a borrow error in a test, and three collapsible-`if` clippy errors, all fixed. On `5c3ff60eb` the independent code review and the covered security review each passed with conditions (no product defect; minor conditions tracked in issue 252). Two of the code review's conditions, I1 (test sensitivity) and I2 (the shared duplicate-key reader), were addressed on the corrected head `cae0ca499`: seven added tests and the Task codec's reader reused. PR 251's reviewed head `271518ac3` had hosted Linux check and test SUCCESS and CLEAN, and the owner squashed it on 2026-11-24, per the team's landing receipt. Migration 004 (`entity_details`, `entity_blob_refs` and the rebuild of five tables) is the second slice, schema only and inactive; no Storage method and no wiring exists for any family. For Agent, Team and Run, a private native adapter and selector reached `main` after this row was written (PRs 284, 288 and 296); this row is not yet reconciled with them (pending: a tracked follow-up task). For Context, Config, Template and Goal the work was first drafted as three stacked pull requests: PR 314 (migration 005, schema only, refusing every populated predecessor row), PR 317 (the crate-private driver) and PR 318 (the goal adapter on the driver), all inactive. It landed as two squashes on 2026-11-26: migration 005 (PR 323, `452e6f622`) and one combined driver and goal-adapter pull request (PR 338, `7aaaff7b2`, 4,899 changed lines, within the limit of about 5,000; step 4 of "T10 entity format" gives the size and the reason). PRs 314, 317 and 318 stay open as history and are not to be merged; PRs 324 and 326 were closed unmerged. The first drafts were checked on Linux at `ae16886f5`, `4e4947d17` and the stack top `730b72c4e` (the 20 `sqlite::cctg::tests`, the 10 `sqlite::goal_adapter::tests`, `sqlite_migrations` 23 and `sqlite_schema` 16 passed, 0 failed; 13 of the 20 driver tests existed at `4e4947d17`), and the reviews of 2026-11-26 (code, security, resilience) passed them with conditions, which were repairs and not defects of the format. The repaired stack ends in three layers at `2d1b9eb30` (PR 314's successor, PR 323), `5c5ff9922` (PR 317's successor, PR 324) and the source head `e45545321` (PR 318's successor, branch `feature/sqlite-goal-adapter-repaired-public`, PR 326 at `d818a4f97`, whose only difference from `e45545321` is in the plan and the spec; PR 338 replaces PRs 324 and 326 with the same code), and the stack's documentation commit changes no code. The repairs, each with its test: the 005 preflight keeps the SQLite error category as PR 306 gave 003 and 004's (`settings_format_preflight_statement_error_keeps_its_sqlite_category`); the native time projections are bounded below at year 0000, in the Agent, Team and Run helper and in the driver's (`native_time_projection_is_bounded_below_by_year_zero`, `native_run_time_column_is_null_below_year_zero_and_text_stays_authoritative` and five added cases of `opaque_and_equivalent_instant_times_are_kept_and_project_only_when_exact`; the Agent, Team and Run helper was later replaced by the shared `rfc3339_millis` (row "T10, RFC 3339 time projection" below), and the driver's helper was replaced by it in turn by PR 354, squashed on 2026-11-26 as `3a8909977`; see the row "T10, RFC 3339 time projection of Context and Goal" below); the driver's provenance accessor, bind-count check and exact-bytes lookup (the four tests of `sqlite::cctg::driver_regression`); the Goal regression tests (a zero stored position, a zero surviving position, an unsupported detail format and a caught failure inside a move and inside a delete) and the annotation of the caught-corruption test as pre-R140 scaffolding; and the repeated-chunk test, the former chunk test having been renamed `a_text_over_one_object_is_ordered_chunks_with_a_whole_hash` because it never repeated a chunk (`repeated_chunks_keep_every_ordinal`). Linux results of the integrator's runs, executed on a private integrated head whose code is byte-identical to the public source head `e45545321`, and on the two lower layers: at the top, `sqlite::cctg` 25, `sqlite::goal_adapter` 15 and `sqlite::migrations` 6 passed (46, 0 failed), and `import_index_reference` 22, `sqlite_migrations` 23 and `sqlite_schema` 16 passed (61, 0 failed); at PR 323's layer `2d1b9eb30`, `sqlite::agent_team_run` 68 and `sqlite::migrations` 6 passed (74, 0 failed), then `sqlite_migrations` 23 and `sqlite_schema` 16; at PR 324's layer `5c5ff9922`, `sqlite::cctg` 17 passed (0 failed). On the top, `cargo clippy` on `orch-infra` for all targets and `cargo fmt` exited 0 and the changelog check read 24 fragments. None of these ran on `e45545321` itself. The publication receipt and successor PR bodies record the final documentation check and the local fast result; local fast is development evidence, never the hosted merge gate. Each repaired head still needs its delta reviews and its hosted Linux check and test, and the successor pull requests need their own receipts. Two schema bytes are kept on purpose: `goals.created_at_text DEFAULT ''` and the `position >= 0` check of migration 005. Two holds remain: the combined adapter's authenticated caller binding for the Task principal, a prerequisite of composing Context that the lead decides, and the R140 observation of caught corruption, owed by tracked follow-up tasks. R142's and R143's remaining tests (the shared entity suites on SQLite, the public cursor, authorization and the observation of the typed corruption error under R140), the importer and backup obligations, and every hold above are pending. The documentation's own checks were the DDL of the new tables and of the claim check on SQLite 3.45 and a throwaway probe of the domain's serde, on Linux. |
| T10, reader snapshots on SQLite (the spec's record N) | The owner approved the wording on 2026-11-27 with 200 ms and 5 s as starting values; it is written in the spec ("Reader snapshots on SQLite", the paragraph on a pooled read after a latch, R51 and R127) and in this plan's subsection of that name. The error representation is open and must be written before activation. The source of PR 385 at `dd196f952` implements every other row of that subsection's table (admission and stale opens, the pin, waiting calls, idle release, independent stop, option B), each with a named test that this plan has not re-run as acceptance; the idle release does not satisfy R127's active-reader age and the WAL, and PR 385 has no checkpoint job and no age-based interruption; a development member adds the interruption (`6706afcbc`, "As built at `6706afcbc`" above), not part of PR 385. Nothing is activated, and no review, freeze, qualification or acceptance is claimed |
| T10, failed top-level rollback (the spec's record L) | The contract is in the spec (R128, "Writer lanes", record L) and this plan, and it is built and unwired. PR 249 (the shared failure state, `8d79976df`) and PR 188 (the executor's hooks, `9a68f1498`) landed on 2026-11-24, so the writer on `main` classifies and latches: `FatalCause::RollbackLeftTransactionOpen` exists, and the writer tests `a_failed_top_level_rollback_that_leaves_the_transaction_open_fails_the_writer` and `a_rollback_error_that_leaves_the_connection_outside_a_transaction_is_not_that_cause` pass on Linux at `23ffdc838` (in the 147-test selection of "One retained producer per writer" below). This row said, when it was written on 2026-11-23, that both PRs were open and that the writer on `main` neither classified nor latched; it does not say so now |
| T10, RFC 3339 time projection (issue 335) | One function, `rfc3339_millis` in `crates/orch-infra/src/sqlite/time_projection.rs`, fills the native time columns of the Team, Run and Message mappings (`teams.created_at` and `updated_at`, `team_members.joined_at`, `runs.started_at` and `finished_at`, `messages.created_at`, `expires_at` and `delivered_at`); the Agent adapter fills none. Issue 341 then adopted it for the Context and Goal columns (next row). Each mapping used a `jiff` parse of its own that accepted wider ISO 8601 spellings than RFC 3339 (a space separator, no seconds, an offset without a colon) and silently folded a leap second onto `:59`. The spec's "What is an RFC 3339 `date-time` (issue 335)" states the rule: `T`/`t` and `Z`/`z`, `-00:00` accepted as UTC, a space separator and a leap second NULL, a fraction of any length when its digits past the third are all `0`, and the range of "Time and unsigned integers"; it also lists which columns are filled and which reads compare them (every point read and every page selection of a team, a run, a message, and, since issue 341, a context entry or a goal). It was written as draft PR 337 (code head `a672e2ced`, on the Message mapping's PR 333 at `bdd32621a`; PRs 333 and 337 were closed unmerged) and was delivered by PR 350, which the owner squashed on 2026-11-26 as `372361041` (tree `af1bb485c`, the same as that of its earlier head `ad0a65a21`), on the replayed Message mapping, PR 349, squashed as `356e37666`; PR 351 (the Message boundary regression controls) followed as `87aae416f`. It is inactive: the daemon uses files. On Linux its author ran the `time_` tests (15 passed, 0 failed; the writer of this text re-ran them on that head with the same result) and `./xtask/gate.sh fast` three times: the first run failed the `orch-engine` `token_eco_preparation` guard on non-literal `.execute(` calls in the new test files (fixed), the second failed one unrelated `orch-daemon` timing test, and the third, on the same head as the second, passed (6,006 passed, 0 failed, 8 ignored). Its independent code review passed with conditions (this wording, and the tracking of the driver below) and its security review found nothing, both of the code at that head. Its hosted Linux check and test passed on PR 350's exact head `992032738` (tree `af1bb485c`) before the owner's squash. It did not change the Context, Config, Template and Goal driver (R143), which kept its own `jiff` helper until PR 354 (next row); the Task rule and the events' time are other contracts. The importer (T21) and `orch db verify` (T22) must call the same function and keep no parser of their own; neither has confirmed it yet. |
| T10, RFC 3339 time projection of Context and Goal (issue 341) | The Context, Config, Template and Goal driver's time helper (`time` in `crates/orch-infra/src/sqlite/cctg/mod.rs`) is now a wrapper of `rfc3339_millis`, so `create`, `update` and the strict comparison of every read (point read and page selection) use the rule of the spec's "What is an RFC 3339 `date-time` (issue 335)". It fills `context_entries.created_at`, `updated_at` and `expires_at` and `goals.created_at` and `updated_at`; Config and Template have no time column, and the goal adapter, a wrapper over the driver, has no rule of its own. The driver's former `jiff` parse and its `TIME_MIN_MS` check are removed, and its tests use the shared `MIN_MILLIS`. It was PR 354, 4 paths and +166/-13 lines (the driver, its two test files and a changelog fragment, no schema, importer or verifier change), which the owner squashed on 2026-11-26 as `3a8909977` (tree `37ca3f29e`, sole parent `8b3ca7c24`). Its one regression test, `sqlite::cctg::time_tests::every_cctg_time_slot_follows_the_spelling_table_on_create_update_and_read`, runs the shared `SPELLINGS` table (84 rows) through the five columns: both families, each optional time present and absent, a `create` and an `update`, the storage class of each column, an exact read in a new transaction, and each column damaged by the kinds that apply to it (a wrong integer or a NULL where it holds an integer, an integer where it holds NULL) (672 stored records); it failed against the previous helper and passed with the change. On Linux the independent code review ran the `sqlite::cctg::` module (26 passed, 0 failed), `sqlite::goal_adapter::` (15 passed) and `sqlite::time_projection::` (2 passed), and four mutants (the old helper, a reader that skips the comparison, an update that keeps the old columns, a NULL written as 0) were each killed; its security review passed. The hosted Linux check and test passed on the combined merge of the replacement head with its base, and on the owner's squash on `main` (8,871 passed, 0 failed, 10 ignored or skipped). The previous helper projected spellings that the rule leaves NULL, and the reverse for two long zero fractions: on a disposable database that it wrote (336 synthetic rows), the new reader accepted 272 and refused 64 as `Corrupt` with check `projection`, in 128 time columns across 16 spellings. The disposition, in the spec's "Development databases written before issue 341", is to recreate an affected development database from its authoritative source before the mapping is consumed; no migration, repair, importer or verifier change; no real database was assessed. This lifts only the time-rule condition on consuming the four-family mapping. The caller binding of Context provenance, the R140 observation of caught corruption, the combined Storage adapter with R142 and R143's shared suites, the activation by T24, and the importer (T21) and `orch db verify` (T22) calling the same function, all stay pending, and the driver stays inactive: nothing outside the crate's SQLite module constructs it. |
| T10 and T8, Amendment 3 (the unit of work on the writer's thread) | The contract is written by the documentation of one PR (ADR 0023, the spec's "Amendment 3" and record M, `docs/TRAITS-FROZEN.md` and this plan), which also carries the seven-bound source change and nothing else. The owner's ruling of 2026-11-27 (record N) adds the boundary of the contained panic: the catch surrounds the callback only, and a leaf's failed savepoint cleanup is observed and raised again, not contained (policy U). The vectors A3-W2, A3-W3, A3-W6 and A3-W7 to A3-W10 are not accepted here, no engine caller exists, and the daemon uses files. As built, read from the source and tests of PR 385 at `d58dd15e4` (on #400's head `eb3e0061e`, which landed on `main` as `7a2fb42e9`; the `crates/` trees of the two are the same; it includes `3f5405194`, the completion seam below, and `8c4b534ea`, the public-constructor fanout test, and its last commit is a documentation commit) and not a result of this plan: `Storage::write` calls the SQLite-private `contain` (`writer/contain.rs`) around the caller's `WriteWork` alone, after `Unit::new` (with its one `Clock` read) and before `ensure_committable`, which run outside it. The lease, intent, retry and message leaves raise the typed `CleanupUnwind` when their savepoint cleanup fails; it is observed under record L and raised again, so `Writer::run` ends the worker. An ordinary callback panic is rolled back and resumed on the caller, and a failed outer rollback keeps its payload for the caller. The broad `WriterHandle::submit_contained` is `cfg(test)` and private to `sqlite`. Tests: `typed_leaf_cleanup_ends_writer_without_becoming_a_caller_panic`, `failed_outer_rollback_keeps_the_callback_payload_for_the_caller`, `callback_panic_is_resumed_and_the_following_write_commits`, `failed_cleanup_prevents_native_list_and_commit_even_when_callback_swallows_it`, `adapter_setup_panic_is_outside_the_user_callback_catch` (A3-W10's first case: a `Clock` that panics) and `adapter_completion_panic_is_outside_the_user_callback_catch` (A3-W10's second case, the check after a callback that returned success). The second case has an injection seam: `StorageOptions::completion_probe`, a `cfg(test)` `Option<Arc<dyn Fn() + Send + Sync>>` that `Unit::ensure_committable` calls first, before its router and native checks (`storage/view.rs`; the unit's own methods call it too, and the final check after the callback is the last call). The test's probe panics only once the callback has set a flag, so the only call that panics is that final check. The caller gets an error and not a resumed callback panic (the awaited `tokio::spawn` is `Ok`, not a `JoinError`), the failure state shows no cause, `close` returns an error because the worker is gone, a reopen on the same data directory lists no team, and a following write commits. The test asserts an error, not which error; the spec's `WriterClosed` for A3-W10 is not asserted by it. The producer's raw logs (Linux, from its private checkout): the test passed 1/0 with the probe; a mutant that moved only the completion check inside the catch failed it 0/1 with a `JoinError::Panic` (a test failure, not a compile error); the restored `sqlite::storage` run was 40 passed, 0 failed; formatting and Clippy were clean. No runtime line changed: the probe and its two fields are `cfg(test)`. The producer's combined fast gate passed on the earlier #406 head `e57880fb0` (6,614 passed, 0 failed, 10 ignored: 6,591 test events in the gate's JSON output and 23 text doctests) before this test existed; after it the checks were scoped, on Linux: at `d58dd15e4`, the `sqlite::storage` module (41 passed, 0 failed), `cargo fmt --all --check` and Clippy on `orch-infra`, `orch-core` and `orch-engine` with warnings denied (both clean); at the #406 head `1c406dd8a`, the same module (41 passed, 0 failed) and the five public `orch-infra` test binaries that PR carries (24 passed, 0 failed, 1 ignored). No full fast gate has run on a head that carries the completion seam or the fanout test. A failed cleanup with no fatal cause answers `WriterClosed` and latches nothing, and that is the contract: the owner ruled on 2026-11-27 (the spec's record O, landed with #400) that the operation's cause and the cleanup's cause are classified separately, that two ordinary causes end the worker with `WriterClosed` and no latch, and that either cause being fatal latches and answers `storage_unavailable`. The ordinary pair through `Storage::write` with a reopen already has a development proof on this head: `typed_leaf_cleanup_ends_writer_without_becoming_a_caller_panic` (`crates/orch-infra/src/sqlite/storage/containment_tests.rs`, lines 20 to 129, the same text at `dd196f952`, `3f5405194`, `d58dd15e4` and the #406 head `1c406dd8a`). It runs a real `Storage::write` on a writer whose authorizer denies, for the `orch_message` savepoint only, `RELEASE` (the operation's failure) and `ROLLBACK` (the cleanup's). Both are `SQLITE_AUTH` denials, which `classify_sql` classes `Transient`: not fatal, so the pair is the first row of the spec's by-cause table, but not one of R128's retryable codes (the spec's Amendment 3, "A transient cleanup", and the vector WP11 below name that case). The callback creates a team, swallows the message leaf's error and reports success, and a second write is admitted behind it. It asserts that both receipts carry the `WriterClosed` text (`SQLite writer is closed`), that the queued callback never ran, that the failure state holds no fatal cause and shows the shared abort, that `close` reports the unwound worker, and that after reopening on the same data directory neither the team nor the message `m` is present and a following write commits. That is a development proof on the published head and is not the final qualification: issue 149 still owes, on its frozen candidate, the qualification of the ordinary pair (the Test plan's "callback boundary" bullet), the two cases of a fatal side, and its candidate checks, and nothing here waives any of them. The test asserts the error text and not a typed variant, and it submits nothing to the closed writer, so A3-W7's refusal of a later submission is not shown by it. Issue 149's crash cases on the combined adapter are not accepted here. The PR is pending its independent review, its successful Linux check and test on the exact head, a CLEAN merge state, the lead's recommendation and the owner's merge. It records no result: a result is a receipt of the PR |
| T10, what the composition does not decide (state read from PR 385 at `d58dd15e4`, not a result) | (1) The nonfatal cleanup question is settled by the owner (the spec's record O: classify by cause); the source keeps the original cause and invents no fatal one, which is what the ruling asks, and a development proof of the ordinary pair with a reopen exists on the published head (the Amendment 3 row names it), so what remains is issue 149's qualification of that case, of the two fatal sides and of its candidate checks on the frozen candidate. (2) Context provenance: the owner's ruling permits owner-authenticated Context consumption and defers the first Task-authenticated Context write to issue 397; no identity or provenance schema is authorized, and a test `Caller` is not production authentication. (3) R26, R27 and R128 production coalescing, chunk accounting and retry policy are incomplete: the owner approved the writer's retry policy (option 1 of the writer-producer proposal) and the first real event submission on 2026-11-27 (the spec's record P, "Writer retry policy" below). The writer's policy was built at `2a7cb83e4` and landed in PR #412 as `53136a969` on 2026-11-27 (not part of PR 385; PR 411 is a separate test PR: retained `BEGIN`, the same-transaction `COMMIT` retry, the outcomes, the retained write, the actual accounting), independently reviewed (the code review with conditions) but not accepted for T10 and not wired to the daemon; the coalescer, the chunking and the event producer are not built in that source. The first real event producer (issue 127) may start on a frozen writer-policy commit, and issue 127's final proofs and the T10 acceptance remain held. A development event producer now exists on PR 420's member (`9b55edbf4`; "As built: the event producer, first form" above), which is not part of PR 385 either; its admission and its retained form were changed afterwards, and the development head `4e80198f0` holds them ("As built: the event producer, encoded form" below), which is not part of PR 385 either and is not the full candidate freeze. (4) R127's active-reader interruption has a development implementation at `6706afcbc` ("As built at `6706afcbc`" above), which is not part of PR 385; its checkpoint job and its daemon schedule (T15) are separate and missing. (5) The full candidate freeze, the physical R142 corpus, the final proofs of issues 149, 167, 286 and 368, independent review, qualification and the T10 acceptance remain. T10 is not complete |
| T10, writer retry policy and the first event submission (the spec's record P) | Approved by the owner on 2026-11-27 and written in the spec (R128, R27, "When a transaction fails", "Amendment 3") and in "Writer retry policy" above, by one documentation change. Step 1 is built: its source landed in PR #412 as `53136a969` on 2026-11-27 (it is `2a7cb83e4`, the correction of `f262da952`; PR 411 is a separate test PR): the vectors WP1 to WP8, WP10 and WP11 each have a test, some of them partly (the table "As built at `2a7cb83e4`" says which and how), WP9 does not exist at that head, and "Status on 2026-11-27" above records the reviews and the hosted run that the head at `2a7cb83e4` lacked when the table was written. The spec's "As built: the retained writer" states the behaviour, three readings that the source builds (every lane waits while one logical operation is retained, the writer-wide `SQLITE_IOERR` streak, a caught clean leaf refusal may commit) and one open question, `SQLITE_IOERR` in a leaf's cleanup, which stays the owner's and unchanged. The first event submission is T16's first slice, and it has a development implementation on PR 420's member, the event commit `9b55edbf4` on that writer (integrated head `07fecf1ed`; "As built: the event producer, first form" above): coalescing, the 1,000-row and 20 ms cuts, the retained tail, the rebuild after a confirmed noncommit and the reconciliation of an unknown commit, each with its tests, 650 passed, 0 failed and 1 ignored in the SQLite library and 28 passed, 0 failed and 1 ignored in the six public targets on Linux. A whole `fast` run of that head failed two `orch-cli` `remote_peers` cases and has no rerun recorded; no review, hosted run, daemon or collector wiring exists for it. Issue 127's final proofs (R127's age-and-backlog rule has an adapter-side development implementation at `6706afcbc`, and its checkpoint job has no code), the T10 acceptance, activation and release stay held. The two other options put to the owner were not chosen |
| T10, event producer: encoded payloads and the admission limits (development head `4e80198f0`, receipt order `c33037e98`) | Read from the source and tests of `4e80198f0` (tree `6db7dc8eb`, on `c3d009ab9` on `c51daaea9`) and of the correction `c33037e98` that sends a known answer after the batch's bytes are returned (the `crates/` tree of the draft train at `60a989256`): the producer keeps each event as its exact encoded JSON, charges that length and its metadata, admits an event of at most 67,108,864 encoded bytes into an empty producer whatever its JSON shape, and bounds what waits by 16,384 events, 4,096 batches and 256 MiB of accounted bytes, each checked on its own (`Rejected::TooLarge` for a batch that can never fit, `Rejected::Full` for one that must wait, both returning it whole). The earlier statement that an empty queue accepts a batch of any size no longer describes the source. It adds no trait, schema, authorization or dependency. Development runs on Linux: 225 passed, 0 failed in the focused library run of `sqlite::events`, `sqlite::storage`, `sqlite::writer` and `sqlite::read_pool`; Clippy and the formatting check clean. No whole `fast` run, hosted run, independent review or targeted security review of this head is recorded here, it is not the full candidate freeze, and nothing is accepted: the plan's "As built: the event producer, encoded form" has the tests, the runs and what stays held |
| T10 and T16, one retained producer per writer (issues 448, 456, 461 and 465) | Written as a contract in the spec's "One retained producer per writer" and in this plan's subsection of that name, from `main` at `77376bc6c` and from PR #459 at `fd81eacc3`, and read again on `main` at `23ffdc838` after #459 (`930d32585`), #466 (`4d415afb9`), #486 and #488: one retained producer (the event producer), refusal at a taken continuation slot or a queued `Events` entry as `Overloaded` after `Episode::prepare`, the token's retry eligibility after it, and the uncertain-COMMIT boundary of issue 461 item 5 checked against the code and one throwaway test. T16's acceptance carries six obligations, and obligation 6's tests of issue 456 are on `main`. Held, not changed by this text: the first caller of the generic `reconcile` (issue 465, open), the rest of issue 461, the T10 acceptance and every wiring. Whether the daemon wires the producer once per writer is not shown by any code yet: `EventProducer::new` has no production caller |
| T10, active readers (R127) and the physical R142 corpus (development member `6706afcbc`) | One commit on `07fecf1ed`, not part of PR 385 and not wired: the adapter side of R127 (one passive checkpoint on its own connection, the backlog from its frame counts, interruption of a read that is older than 30 s while the backlog is over 256 MiB, `snapshot_expired` for that one owned snapshot) and the physical R142 corpus as `cfg(test)` code on the borrowed entity driver and the shared failure state. Development runs on Linux: `sqlite::read_pool::` 11 passed, `sqlite::storage::` 57 passed, 0 failed (the plan's "As built at `6706afcbc`" has the tests; the 22 physical tests passed in the same development run). Not done: the checkpoint job and its schedule (T15), a whole `fast` run, a hosted run, an independent review, the corpus's whole current qualification, the freeze and the acceptance; nothing is activated |
| T2 to T27, except T4 and T4b | pending, as this table stood before 2026-11-21; this revision brings no other row up to date |

### Train 14: size of the replacement for PR 324 and PR 326

The owner ruled on 2026-11-26 that a pull request normally stays under about 4,000 changed lines and is split when it
is over about 5,000, unless the plan explains why it cannot be split; the review-slice rule of about 800 lines is
unchanged. The documentation PR 340 (merged 2026-11-26) amended step 4 of the "Workflow" of AGENTS.md from "about 3,000" to
the same two figures. For this goal's pull requests from that day this rule replaces the earlier limit of about 3,000
changed lines. Older
sentences of this plan still quote that limit for the slices they describe (the "Tasks" introduction, the alternatives
list, and the T8 and T10 split rules); they are the history of those splits and are not rewritten here.

The replacement for the overlapping PR 324 (the native driver for Context, Config, Template and Goal) and PR 326 (the
goal adapter rewritten as a wrapper of that driver) is a single change on PR 323. Its first commit,
`4056bc073e8fcd27771e8523a8f95d045e80ee3b` on PR 323's head `2d1b9eb30253f46576c6b21e1ac92461f39ff325` (tree
`04201deba85f0cec34cf496038d2442d81d9a4f0`, equal to PR 326's), has 12 files, 3,725 lines added and 1,164 removed: 4,889
changed lines. It is PR 338, which the owner squashed on 2026-11-26 as `7aaaff7b2` from the head `d77185b0b`, whose code
is that of `4056bc073` (the two differ in the spec and the plan only): 12 files, 3,731 lines added and 1,168 removed,
4,899 changed lines. That is above the usual 4,000 and under the 5,000 at which a split is required, so it is inside
the rule and not an exception; the owner confirmed the size on the same day. Where this plan describes the three-layer split of
PR 323, PR 324 and PR 326 as the way the work lands, this note supersedes the last two of those layers.

- *Why one change.* The three ordered owner squashes of PR 328, PR 323 and PR 324 replay cleanly on main, but PR 326
  then conflicts with the file additions that squashing PR 324 left behind (add/add in the driver and its tests), and
  retargeting PR 326 cannot restore the ancestry the squash removed. One replacement keeps the driver and the integrity
  tests that the review of the lower change required to be qualified with it in one squash, and the tree that the
  squashes give is the one that the earlier combined train run on PR 336 (45753065806) already passed.
- *Not chosen.* Splitting the change again, with both heads frozen and qualified together, would keep the change
  smaller but needs new construction and a new qualification; a merge commit for PR 324 alone would keep its ancestry
  but departs from the squash convention.
- *Conditions that stand.* They come from the decision to replace two reviewed changes with one, not from the size. The
  whole new delta gets independent code, security, resilience and documentation review. The replacement and a freshly
  built combined train each need successful hosted Linux check and test on the exact head, CLEAN, with the executed
  parents and trees verified; each single-parent squash is rehearsed; and the lead writes a separate recommendation
  naming the qualified heads before the first squash. PR 324, PR 326, the draft train PR 336 and every failed or
  cancelled run stay on record. The owner alone merges, one member per green main, and drift stops the sequence for
  requalification.
- *What it does not do.* It changes no merge method and lifts none of the holds on Storage composition, issue 127 and
  R140, issue 149 and issue 167, cursors and authorization, garbage collection, import, export, backup and restore, the
  two reference suites, the writer release, full T10 and T24, and it does not release the conditions that the reviews of
  these drivers set for their consumption. The corrected wording of D1 (the caller-binding prerequisite of the
  four-family driver, in the spec's as-built paragraph and its open item 6) is not touched by this note or by the time
  rule above.
- *State when this was written.* PR 338 was a draft and its own review and hosted Linux check and test were pending;
  the fresh combined train that qualified it was the draft PR 339. Nothing of it was qualified or merged, and the file
  backend served the daemon. Since then PR 338 has landed as `7aaaff7b2` (see above); the corrected combined train that
  qualified the landed tree was the draft PR 346, and the file backend still serves the daemon.

## Test plan

- The spec's acceptance checks, by name, are the proof of each requirement. The gate lines map to them in the
  spec's "Gate lines" table.
- The conformance suite runs on the file adapter from T2 and on both adapters from T10 to the end. Engine and
  daemon tests added by this stack run on both adapters until T25.
- Crash points (process kill; issue 149). T10's two crash test files each start a child process that reaches a
  boundary, sends one marker byte over a private socket pair and blocks; the parent waits for that marker, requires
  the expected one, sends `SIGKILL`, requires the child's status to be signal 9, reopens the data directory and
  compares the complete rows. `sqlite_crash` does this to the production writer with two tables: (S) between two
  statements, (B) with both statements run and the callback not yet returned to the writer's commit (both before
  `COMMIT`, so nothing survives), and (C) after the successful receipt, which the writer sends only after its commit
  returned (both rows survive). `sqlite_storage_crash` does it through the public `Storage::write` with a config
  and a template: (S) the config is created, the template is not and the callback is blocked; (B) both are created
  and the callback has not returned; neither family survives either. (C) is a successful receipt and both records
  survive; (N) is a following write that has created both of its records and has not returned, and only the earlier
  receipted records survive. The marker shows that the child reached the phase. The signal rules out a normal exit.
  Neither shows that the transaction was still active at the instant of the kill: the child's wait for the release
  is 10 seconds, so a parent delayed past it lets the callback start unwinding before the signal arrives, and the
  parent's marker wait is bounded by the same 10 seconds. Not verified, and no case claims them: an interruption
  inside SQLite's `COMMIT` call and a power loss (with `synchronous = FULL`, hardware or file system recovery).
  T12 and T13 inject a stop before and after every statement of each multi-entity operation and assert
  all-or-nothing on SQLite.
- Sensitivity of the crash cases to timing. A deliberately premature receipt was caught in 31 of 31 macOS trials.
  That is history, recorded from issue 149; further macOS timing and recovery measurements are issue 181 in the
  After 2.0 milestone, and none of it is a release gate. On Linux, at `24c40bcf4`, one development measurement exists: a
  temporary change to the writer alone made it send its receipt from inside the callback, before `COMMIT`, with no
  added delay, and the public case (C), `public_kill_after_receipt_preserves_both_families`, failed in 31 of 31
  separate runs at its complete-reopened-records check (exit status 101 each; the change is not in the tree). It is
  a timing-dependent sample from one host. It does not guarantee that every premature receipt is caught.
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
- Task corruption and the writer (spec R140, R141): each check of the format's read is planted in turn, and a get, a page,
  a snapshot, a unit of work and a live backup or verification each return `TaskCorrupt` with no partial page and fail the
  writer. An absent task, a stale revision, a refused value and an admission timeout do not. A callback that catches a fatal
  cause still fails the writer and aborts the transaction; a failed operation rollback is classified in both causes and a
  fatal one wins; a failed savepoint rollback forbids the commit; an ordinary failure rolled back successfully may be
  caught and unrelated work commits. A corrupt backup, export or import file refuses its command and leaves the writer
  usable.
- A failed top-level rollback (spec R128, record L): a connection whose `ROLLBACK` is refused (a test authorizer
  stands for it) ends a request in an error. The cause is latched; the next request on each lane, a claim and a
  checkpoint are answered `storage_unavailable`; the report names the writer connection and the cause; and after the
  process stops the file holds no row of the failed request. A rollback that returns an injected ordinary (non-fatal)
  error while the connection is outside a transaction latches nothing for this cause, and the writer serves the next
  request. With the latch set and the transaction ended by SQLite (a `RAISE(ROLLBACK)` trigger), the answer is
  `storage_unavailable` and not `TransactionAborted`, and a write made after the latch is not durable. A callback
  panic is not mapped.
- The callback boundary (spec Amendment 3, records N and O; issue 149): five cases, each kept apart, none standing for
  another.
  (1) A callback that panics: rolled back, the panic resumed on the caller, nothing latched, and the worker serves the
  queued request and the next write (A3-W2). (2) A leaf whose savepoint cleanup fails with no fatal cause, the
  operation's cause being ordinary too (the ordinary pair, decided by record O): the worker ends, the running and the
  queued receipt are `WriterClosed`, nothing is latched and a later submission is refused (A3-W7). (3) The same with
  `SQLITE_CORRUPT` or `SQLITE_NOTADB` on the operation's side, which a successful outer rollback would not have shown,
  and with a fatal result on the cleanup's side (the other side ordinary in each case): the cause is latched before the
  unwind is raised again, and the receipts are `storage_unavailable` (A3-W8). (4) A callback panic with a refused outer
  rollback: the payload reaches the caller, the `RollbackLeftTransactionOpen` latch is set and the worker ends (A3-W9).
  (5) A panic of the adapter's own code outside the callback (A3-W10). Issue 149 carries (1) to (4) separately on the
  combined adapter, and each ends with a reopen that shows no row of the failed request (for (1), none of the panicking
  callback; the queued request's row is present). Case (2), the ordinary pair, and its reopen are evidence that issue 149
  owes through `Storage::write`: the receipts are `WriterClosed`, no cause is in the failure state, a later submission is
  refused, and after reopening on the same data directory neither request's rows are present and a new write commits.
  A real `SQLITE_CORRUPT` on the cleanup statement itself cannot be produced through SQL while `rusqlite`'s `functions`
  feature is off, so the draft injects that one directly at the leaf's cleanup call; the operation-side corruption is a
  damaged page.
- Other connections (spec R137, R140): a result is injected at the checkpoint, read, query and backup
  connections in turn. Corruption fails the writer from each; other failures are counted and reported at
  their thresholds and do not fail it; each outcome of a checkpoint call is produced from injected counts,
  and a running total repeated by the next call credits no second copy. As of 2026-11-27 (issue 430) this is not built as written: results are injected at the shared
  state for the checkpoint and tool sources and at a pooled reader, no real checkpoint call, query or backup connection is made to fail, and
  R137's outcomes of a checkpoint call have no code (T15). The spec's "R140's tests" lists what exists.
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
- **Amendment 3.** A source-level change with no on-disk effect; see "Amendment 3: the unit of work on the writer's thread".
- **Schema.** Forward-only. A newer database refuses an older binary. The copy the daemon makes before each
  migration (spec R124) is the way back; a scheduled backup is not in 1.1 (AP9).

## Docs to update

README (upgrade notes: one import per project, the data directory, rollback and what it loses, keeping the
1.0 binary), RUNBOOK (lock holder, unfinished import, a 1.0 state that changed after the import, disk floor,
backup and restore, the copy before a migration, a failed writer, never writing to `orch.db`, stale
revision, the removed file procedures), ARCHITECTURE (§1, §2.1,
§3.1 and §3.3, §4, §5.1 to §5.3, §6.1), `docs/TRAITS-FROZEN.md` §2, the manual (storage, import, `orch db`,
attention, the plain statement about `preapproval`), a new `docs/storage.md` (schema, data directory, export
format), CHANGELOG, ADR 0017, ADR 0023, `docs/decisions/README.md`.

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

Status: the owner merged the contract text (PR 158), which ratifies the drafted rows and the proposal of
T21-O1 as the plan above says. The read-side wiring slice below gives the `config.yml` path and the index
reconciliation their first caller, the whole-source check, and the record of point 4 is brought up to date
there.

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
blocking pool of one is enough. A task id of `.` or `..` stops the check (R104); a template stem is stored as it is when it
is UTF-8 and stops the check when it is not (issue 192). The check on `main` does not yet refuse one, nor a template
body that is not UTF-8; the first importer caller of issue 192 adds both refusals, and the spec's R99 names its test.

Where the check goes beyond the spec's wording, it is stricter and says so: a `running` set of `state.json`
that 1.0 would read as empty because one entry does not decode stops the check, because that set is what
R119 asks about.

Not in this slice: the `config.yml` and `_index.json` classes (C-V1 to C-V14, X-V1 to X-V14), which the
read-side wiring slice below adds; every write (rows, blobs, `.orch-project`, the manifest), `orch import` and
its dry-run report, `--resume`, the second reading of each file at the load, and any use of a copy of a live
project. The check has no caller outside its tests.

### T21 reader compatibility evidence (issue 146)

The inactive reader now pins ordinary config, blank event lines and damaged indexes
against the real file readers in `import::tests::compatibility`. It retries interrupted
buffered event reads and propagates terminal I/O failures. The document-count rule,
physical gap semantics and index disposition are unchanged. JSON's effective depth
ceiling remains 64, even when a caller requests more.

The effective legacy config reader supplies defaults for missing, empty, space/newline-only,
comment-only and explicit-null config; the bounded reader refuses the three zero-document
streams, and explicit null passes the budget check but is not a typed entity. Multi-document
config is refused by both readers. The contract that reconciles this with R109 is the one of
"T21 config and index contract" above, and the read-side wiring slice below applies it. See
[the compatibility record](../internal/import-reader-compatibility.md) for the table and the
individually tracked conditions. Full T21 still requires its actual writer, reporting, dry run,
resume and acceptance evidence.

### T21 read-side wiring slice (issue 146)

The whole-source check now classifies `config.yml` and the five `_index.json` files by the contract of
"T21 config and index contract", on the bytes it already measured, and still writes nothing. Nothing outside
its tests calls it, and there is still no `orch import` command.

- `config.yml`: a missing file is the empty mapping; a stream of no document is refused under `documents` and a
  null, scalar or sequence document as `unreadable`, each saying that 1.0 read the file as its defaults; a stream
  that 1.0's scanner rejects, such as one with a tab at the start of a line (C-V5), is refused as `unreadable`
  without that statement and is never counted as a stream of no document (the bounded reader checks the scan
  before it refuses for the count, PR 239); two
  documents are refused under `documents` without that statement; the mapping is merged over a private copy
  of 1.0's defaults that no later default changes. A key the defaults do not hold is noted as `unknown_config`
  by its bounded dotted path, its value never reported, and the compact JSON of those keys above 65,536 bytes
  is refused under `legacy_config_bytes`. The report carries the effective `run_cap` (6 when the file does not
  set it, with a `config_default` note that says so) and `preapproval_requires_confirmation`, true when an
  agent is `auto` or inherits an `auto` project default. That flag, with its note, is a preview: input never
  grants trust (R93, R133), and nothing sets a policy.
  A numeric or boolean mapping key reads as the string 1.0 reads (`200` and `true`), at the top and in a nested
  mapping, and a null key is refused as `unreadable`, as 1.0 refuses it.
- `_index.json`: the array is examined whole, so a bad entry anywhere is `index_entry` before any
  `index_duplicate`, which holds for bare identities and for whole entities alike. A missing, unparsable,
  object or `null` index is a note `index_unreadable` and the files go in id order; the budget, file-kind and
  I/O stops stay terminal and are never `index_unreadable`. `index_unlisted`, `index_missing_file` (an entry with
  no file, never opened as a path; a present file that is refused gets its own refusal and is not also reported
  as missing) and `index_differs` are notes, and the file's content wins. The report's `order` previews at most
  1,000 positions for all stores together and counts the rest in `more`; it is not a load manifest.
  Files with no index entry are ordered by id, which is the file name without its document extension, in all five
  directories, so `a` precedes `a-b`, `a.b` and `a_b` although `a-b.json` sorts before `a.json` as a file name. A
  field of an index entry that the check does not read stays raw JSON: a number outside the range of a JSON value
  (`1e999`) in an ignored field neither refuses the entry nor changes the whole-array precedence, and the comparison
  with the file for `index_differs` is structural (objects by member, arrays in order, such a number by its text).
  A file that the typed reader accepts is accepted whatever such an ignored field holds.
- Tests: `crates/orch-infra/tests/import_preflight_compat.rs`, named in the compatibility record. The same
  test that pins C-V11 runs 1.0's task, agent, goal and team readers on a separately created zero-byte file
  for each, and X-V5 and X-V7 run 1.0's `IndexManager` on a second, disposable source for each of the five
  directories. X-V13 is the unit test `an_index_read_error_stops_and_an_interrupted_read_is_retried`, which
  injects the read fault into the method the whole-source check reads every index through. The tests also
  pin that a JSON message or context document with a repeated field the entity type knows is refused as before, and that a YAML
  entity keeps an ignored YAML-only key. C-V5 is the unit test `a_config_that_does_not_scan_stops_before_its_documents_are_counted`
  (PR 239), which runs six tab-only streams through 1.0's `FileConfigStore::read_merged` beside the bounded reader. Four later
  regression vectors of the caller are named in the same test file (issue 270, `docs/internal/import-reader-compatibility.md`):
  `numeric_and_boolean_yaml_keys_follow_the_effective_legacy_reader`,
  `prefix_id_order_is_independent_of_the_document_extension_in_every_store`,
  `opaque_ignored_json_numbers_preserve_acceptance_order_and_structural_comparison` and
  `opaque_index_members_do_not_change_whole_array_precedence_in_any_store`. All of them run on synthetic sources.

Held, and not claimed by this slice: the writer, the importer and its command, integrated dry run, resume and
zero-write acceptance (issue 146), and the lock held across the check and the cancellation of a blocked read
(issue 192). The reader-level `import::index::reconcile` is unwired, is kept as historical evidence and still
names a repeated identity `index_entry`; it is not the preflight's classifier, and it takes every entity
document of a directory at once, which the check does not gather. No changelog fragment is added: nothing a user can run or notice changes, and
the check has no caller outside its tests (AGENTS.md, "Changelog fragments").

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
The reference's larger-error refusal does not satisfy or weaken whole-text R97:
[ADR 0020](../decisions/0020-storage-consumer-contracts.md) decides that a retry keeps its whole `error`
text; the references of `main` at `07b5f967` refused it, and the change that carries this plan changes them to meet it
(see "T8 consumer contracts (issue 109)").
T10 owns the SQLite representation of the blob mapping. T12 retains first-wins enqueue, T21
legacy invalid-deadline handling, and T14 dispatch/rotation order. The historical
builder's due-only limitation is superseded by the reviewed module contract.

T8 reference acceptance is separate from runtime acceptance, and each runtime check has one owner:

- T12 owns lease-bound finish and restart lease fencing (R36, R132; the runtime proof is issue 110).
- T13 owns generations and recovery (R41) and the Linux sleep and wake fencing check (R42, issue 110: a real
  suspend and resume of an isolated Linux host that the owner has authorized; pending).
- T23 owns durable restore, including a new lineage, and the owner's open point AP14 on a backup's workspace
  names.
- T27 owns the second-host peer check on Linux (R89).

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
