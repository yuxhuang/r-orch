# SQLite storage for release 1.1

status: draft
owner: architect; requirements approval: the owner, at the approval gate that follows the entry steps
date: 2026-11-20
plan: [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md)
decision: [ADR 0017](../decisions/0017-sqlite-storage.md) (proposed)

This spec is longer than the page that [the specs README](README.md) asks for. The goal requires it to fix
names, bounds and the schema split before any code, so those are kept here as reference sections after the
requirements, as the daemon-owned-state spec did for its protocol. All names of operations, commands, settings
and metrics are proposals (source item O9) until the owner approves them.

## Source

Verbatim excerpts of the owner's request as recorded in the goal "orch 2.0 phase 1: SQLite storage (release
1.1)". Labels are added for reference and change no wording: S1 and S2 for context, B1 to B15 for the binding
decisions, A to R for the deliverables, G1 to G9 for the gate, X for out of scope, O1 to O10 for the open
points, DL for delivery. Checkout instructions and tracker metadata are not requirements and are not reproduced.

### S1. Where this sits

> orch 2.0 is reached by one release per phase. Each release leaves a single-machine install working from one start command.
>
> - 1.1 storage (this goal): SQLite, a one-way import, `orch db`.
> - 1.2 evidence as stored records, text search (SQLite FTS5), live log follow.
> - 1.3 HTTP/2 over TLS on loopback, with a token.
> - 1.4 authentication: enrolment, client certificates, a project list per identity.
> - 1.5 workers with fenced leases, a managed git hub, daemon-side landing, one daemon serving many projects (opt-in).
> - 2.0 orch-peer is removed.
>
> What 2.0 looks like, so that 1.1 does not paint it into a corner: one central daemon owns all state; workers on any machine dial it, take leases and run agents; every lease carries an epoch that fences a superseded run; a task is done only when the daemon has merged its accepted run ("done means landed"); the agent records a claim and the daemon sets the verdict. None of that is built here. 1.1 lays the storage those phases stand on.
>
> 1.1 is one of two breaking releases: every project must be imported once, one way.

### S2. What 1.0 does today

> - State is files under `.orchestry/`: one YAML per entity, a per-store `_index.json`, `state.json` (running, claimed, a retry queue capped at 100 entries that drops the oldest, stats), `runs/run_*.json` and `runs/run_*.jsonl`, `config.yml`, messages, context, templates, attachments.
> - One daemon per project directory owns that state; every client talks to it over a Unix socket (protocol version 2, 115 typed operations: 48 read, 43 mutate, 11 destructive, 13 control). No client ever starts a daemon; with none answering a client exits 7.
> - Each store offers get, save, list and delete on one entity. Nothing spans entities, nothing checks versions (last writer wins), and lists return everything.
> - Order is behaviour: saving an entity moves it to the end of the index, and agent selection depends on index position. That is why ties between agents rotate.
> - Size on the owner's live project (2026-11-19): about 700 MB of run events for 2,538 runs (about 279 KB per run), and 68 MB of task YAML for 2,979 tasks.

### B. Decisions that bind this goal

> 1. SQLite is the only backend from 1.1. The file backend is removed from the product and kept in the test tree for one release as the conformance reference.
> 2. Import is one-way. The importer never writes to `.orchestry/`, so the way back is that untouched directory with the 1.0 binary.
> 3. The engine reaches state only through a storage port with transactions (units of work), a revision on every entity, and paged lists. Removed from the port: the whole-document state store, the YAML merge-key rule, and "save moves the entity to the end of the index".
> 4. The schema is multi-project (a `project_id` on every table except the daemon-wide ones), but 1.1 still runs one project per daemon.
> 5. One data directory per daemon, mode 0700, with an exclusive lock. A second daemon pointed at it refuses to start and names the holder.
> 6. One task performs every write, one transaction at a time; a pool of read-only connections serves reads from a consistent snapshot.
> 7. Text up to 64 KB may be stored in the database; larger or non-text bytes go behind a blob-store adapter (files on disk now, object storage later).
> 8. `orch db export` writes a file the importer can read back. Every `orch db` command and `orch import` is for the owner, over the local socket only.
> 9. The scheduler sits behind an interface that only decides which task goes to which agent; the engine's claim transaction is what prevents a double dispatch. 1.1 ships one backend: today's in-memory logic, with the same rules and the same rotation.
> 10. Raw run output is pruned after 90 days for successful runs and 365 for failed ones by default; "never" is an explicit setting. Results are never pruned.
> 11. There are no per-project storage quotas. The daemon refuses new work when free disk falls under 5 GiB or 5%, whichever is larger.
> 12. Storage defaults: WAL journal, `synchronous = NORMAL` (FULL is a setting), foreign keys on, `BEGIN IMMEDIATE` for writes, STRICT tables, a busy timeout of about 5 s as a safety net, local disk only.
> 13. Every completed task is recorded as having nothing to land (`landing_state = 'skipped'`) until workers arrive in 1.5. The statuses `proposed`, `landing` and `needs_attention` exist in the schema but nothing enters them before 1.5.
> 14. A project whose agents use the 1.0 `auto` approval policy is imported with a per-project pre-approval policy on, so it keeps running unattended. Only the owner can set that policy. This is implicit trust of the agents and the manual must say so plainly.
> 15. orch-peer is not changed by this goal. It must keep working beside a 1.1 daemon, or the spec must say exactly what breaks and get the owner's approval.

### A to R. Deliverables

> Entry steps, done before any product code:
>
> - A. The spec, the plan and an ADR in the repository, carrying the conditions in this description.
> - B. A storage conformance suite written against the file backend, and the list of every read whose result depends on index order.
> - C. The choice of database library, with the dependency check AGENTS.md requires.
> - D. A measurement of batched event throughput against a stated claim-latency budget.
> - E. A prototype of "claim with a fenced lease", including what happens across sleep and wake.
>
> Then the product:
>
> - F. Storage port v2: units of work, revisions, cursor-paged filtered lists, an explicit ordering column, claim operations, batched event append, a principal on every call.
> - G. The SQLite backend: schema, numbered forward-only migrations, the single writer with priority lanes, the read pool, intent rows for side effects.
> - H. The engine on the new port: each multi-entity operation is one transaction; leases and a retry table replace `state.json`; the maintenance jobs that belong to 1.1.
> - I. Request keys, so a repeated change runs once; revisions and cursors in the API; a protocol version bump.
> - J. The authorization matrix, checked in one place in the operation dispatcher.
> - K. The scheduler interface with today's scheduler behind it, and metrics for tick duration and open tasks.
> - L. The blob store on disk; rendered prompts stored once by hash; attachments as blobs.
> - M. Run events in SQLite, with pruning.
> - N. `orch import`: one-way, bounded, with a dry run and a resume.
> - O. `orch db`: status, verify, query, backup, restore, prune, vacuum, export.
> - P. The attention queue with its first two kinds: disk floor, and dispatch off with work waiting.
> - Q. Usage recorded for every run, whatever its outcome, with the model and the agent CLI version.
> - R. Documentation: manual, RUNBOOK, ARCHITECTURE, README upgrade notes, CHANGELOG.

### G. The gate to leave this phase

> Every line needs a named test or a recorded check. The goal is not done while any line is unproven.
>
> 1. The conformance suite is green on SQLite and on the file-backend reference, including list order and agent rotation.
> 2. The importer is verified on a copy of the owner's live project, with a dry run and a resume.
> 3. A backup followed by a restore round-trips the database, the blobs and the repositories as one restorable point. (No bare repository exists in the data directory before 1.5, so in 1.1 that part is an empty directory; the code path must already cover it.)
> 4. An export followed by an import of one project reproduces every row and hash.
> 5. Test principals for a worker and for a task are refused what they must be refused.
> 6. A second daemon on one data directory refuses and names the holder.
> 7. Measured event throughput meets the claim-latency budget, or the file fallback for events ships.
> 8. A dropped and repeated change runs once; the same key with a different body is refused.
> 9. A restore starts a new daemon generation: no epoch is reused, and the restore lists the identities and tokens it re-enables and asks for confirmation.

### X. Out of scope

> Evidence records, quarantine, uploads and text search (1.2). HTTP/2, TLS and tokens (1.3). Enrolment and certificates (1.4). Workers, the worker protocol, the git hub, landing, proposals, many projects on one daemon (1.5). Removing orch-peer (2.0). Postgres, an indexed scheduler, object storage, token budgets, any outbound notification (after 2.0). Windows.

### O. Open points the spec must settle and the owner must approve

> - (O1) How a client finds the daemon and its data directory in 1.1, while there is still one daemon per project directory. The target is that a project is named explicitly (`--project`, `ORCH_PROJECT`, or a pointer file `.orch-project` that holds a project id and nothing else) and never guessed; which release introduces that is not fixed.
> - (O2) Which tables 1.1 creates. The full 2.0 sketch has 50 tables. Columns whose later addition would be a one-way migration must be there from 1.1; whole tables for later phases may arrive by migration.
> - (O3) Whether `orch import` runs with the daemon stopped, and how long the 1.0 files are kept after import.
> - (O4) Whether the daemon starts with dispatch as it was when it stopped (proposed) or always off (1.0).
> - (O5) Whether the maintenance loop is one timer with per-job due times or one timer per job.
> - (O6) How long resolved attention items are kept, and whether a blocking item makes `orch doctor` fail.
> - (O7) Unknown keys in a 1.0 `config.yml`: proposed to be stored in a size-capped legacy object that the daemon never reads, and listed by the dry run. `max_concurrent_agents`: proposed to become an optional per-project run cap.
> - (O8) Request keys are kept for an hour, so their expiry job must ship with them in 1.1.
> - (O9) Names of new operations, commands, settings and metrics. All names in this goal are proposals.
> - (O10) How the fixture corpus for the conformance suite and the importer avoids private data.

### DL. Delivery

> tier=mid, stance=0.5, create PRs. This is far more than one PR: deliver an ordered series of stacked PRs against main, each green and each leaving main fully working. The file backend keeps working on main until the PR that removes it. The owner merges. No tag and no version bump in this goal; cutting 1.1.0 is a separate decision the owner takes when the gate evidence is in.
>
> Approval gate: after the entry steps, present the spec, the measurement, the prototype result, the library choice and the open points above with recommendations, then pause this goal and wait for the owner. Build nothing in the product before that.
>
> A task in this goal that turns out to be larger than one PR of about 3,000 changed lines is split: deliver the first slice, and add follow-up tasks to this goal with their dependencies stated.
>
> Tests never touch a real project's daemon or the developer's real `~/.orchestry`. Anything measured or verified on the owner's live project is done on a copy outside the repository, and no private content from it enters the repository, a PR or a fixture.

## Facts this spec rests on

Read from the code at `fb6a1e1` (v1.0.0) unless marked unverified.

| # | Fact | Status |
|---|---|---|
| F1 | The catalogue has 111 operations: 47 read, 42 mutate, 10 destructive, 12 control (`crates/orch-proto/src/ops/mod.rs`, the class table in its tests). S2 says 115 (48, 43, 11, 13); each class is one lower in the code. This spec counts from the code; see added point AP2. | verified |
| F2 | `LOCAL_PROTOCOL` is 2. The wire error codes are `general`, `invalid_arguments`, `not_initialized`, `lock_conflict`, `agent_adapter`, `workspace`, `not_found`, `conflict`, `not_permitted`, `overloaded`, `shutting_down`, `unimplemented`, with exits 1, 2, 3, 4, 5, 6, 1, 1, 2, 7, 7, 1 (`crates/orch-proto/src/error.rs`). | verified |
| F3 | Five stores keep last-write order in `_index.json` (task, agent, goal, context, message). Agent choice breaks ties by that order (`AgentService::find_best_in`); every agent save, including a status change, moves the agent to the end. Team order is directory enumeration and is not a contract. | verified |
| F4 | A run event has a timestamp, a type and data, and no sequence number. Event data is cut at 8,192 characters and a stored line at about 4 KiB (the cut counts characters, so multi-byte text can exceed it). A run records tokens, a pid, its workspace path and the rendered prompt inline; it records neither the model nor the agent CLI version. Nothing prunes runs or events. | verified |
| F5 | Dispatch on or off lives only in memory and is off after every start. `orch doctor` exits 0 when its checks fail and a daemon answers. There is no metrics facility. | verified |
| F6 | The approval policy `auto` (the default) moves a task from `review` to `done` after a successful run without a person. It is not passed to the agent CLI. | verified |
| F7 | `orch-peer` runs inside the daemon on the engine. It reads `tasks/_index.json` and `messages/_index.json` directly and builds a `FileMessageStore` (`crates/orch-peer/src/worker.rs` near line 805, `crates/orch-peer/src/lib.rs` near lines 299 to 316), reads `state.json` through the state store (`worker.rs` near line 868), and keeps its own files `remotes.yml`, `remotes/` and `peer-id` under `.orchestry/`. | verified |
| F8 | The 1.0 daemon takes `flock` on `.orchestry/.daemon-owner.lock` and then writes the PID file `.orchestry/orchestry.lock`. Agent workspaces are under `.orchestry/workspaces/`. | verified |
| F9 | An exclusive `flock` taken through a read-only descriptor conflicts with the 1.0 daemon's lock on macOS and Linux and changes nothing in the file. | unverified; R31's test settles it |
| F10 | Suspending the machine does not, by itself, end a local agent process or the daemon's supervision of it. | unverified; deliverable E settles it |

## Requirements

Test names are the names the implementing PR creates. A PR may choose another name only by editing this table in
the same PR. "Both" means macOS and Linux CI. Requirements marked "(O*n*)" or "(AP*n*)" depend on that open point
and change with the owner's answer.

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | The spec, the plan and ADR 0017 shall be in the repository in one documentation-only PR before any product code. | A | must | The three files exist on `main`; `./xtask/gate.sh fast --base origin/main` classifies the PR as documentation only; macOS |
| R2 | A storage conformance suite shall run every scenario against a backend chosen by the test, and shall pass on the file backend before the SQLite backend exists. | B | must | `cargo test -p orch-infra --test storage_conformance`; both |
| R3 | The list of every read whose result depends on index order shall be in the repository, each entry naming the function, the order it relies on and the scenario that covers it. | B | must | `docs/internal/order-dependent-reads.md` exists and `crates/orch-engine/tests/order_conformance.rs::every_listed_read_has_a_scenario` passes; both |
| R4 | The SQLite library shall be chosen with the ARCHITECTURE §4.8 check (maintenance, MSRV of the pinned toolchain, licence through `deny.toml`), recorded in its own ADR, bundle SQLite 3.37 or later with FTS5 available, and leave `unsafe_code` forbidden in the workspace. | C | must | The ADR exists; `cargo deny check` passes on the PR that adds it; `crates/orch-infra/tests/sqlite_build.rs::bundled_sqlite_has_strict_tables_and_fts5`; both |
| R5 | Batched event throughput shall be measured against the claim-latency budget in "Writer lanes", on macOS and on Linux, with the commit, the hardware class and the raw figures recorded. | D, G7 | must | `cargo xtask storage-bench --record <dir>` and its record linked from the approval-gate PR; macOS and native Linux |
| R6 | A prototype shall show a claim with a fenced lease, including a suspended and resumed machine, and its result shall confirm or replace ADR 0017 decision 16 and R39 before the engine PR starts. | E | must | The prototype's report linked from the approval-gate PR; macOS with a real suspend, Linux |
| R7 | When the work of a unit of work returns an error or the process stops before the commit, the storage shall keep none of its changes. | F, B3 | must | `storage_conformance::unit_of_work_keeps_nothing_after_an_error`, `sqlite_crash::kill_before_commit_keeps_nothing`; both |
| R8 | Every entity shall carry a revision that is 1 at creation and goes up by one on every update, and an update or delete whose expected revision is not the stored one shall fail with `StaleRevision` and change nothing. | F, B3 | must | `storage_conformance::revision_starts_at_one_and_goes_up_by_one`, `::stale_revision_is_refused_and_changes_nothing`; both |
| R9 | A list shall return at most 100 rows by default and at most 1,000 on request, shall refuse a larger limit, shall return a cursor while more rows remain, and shall count the matching rows only when asked. | F | must | `storage_conformance::list_defaults_to_100_rows`, `::list_refuses_a_limit_over_1000`, `::count_is_absent_unless_requested`; both |
| R10 | A walk through a list by cursor shall return every row whose sort values did not change during the walk exactly once, whatever is inserted or deleted meanwhile, and a cursor used with another filter or sort shall be refused. | F | must | `storage_conformance::cursor_walk_is_stable_under_inserts_and_deletes`, `::cursor_from_another_filter_is_refused`; both |
| R11 | Creating an entity shall give it the next `position` of its project and table, an update shall leave `position` unchanged, and `move_to_end` shall give it the next `position`. | F, B3 | must | `storage_conformance::update_keeps_position`, `::move_to_end_takes_the_next_position`; both |
| R12 | When several callers claim a lease for one task at once, exactly one shall be granted and the others shall be told who holds it; no agent shall hold two active leases. | F, B9 | must | `storage_conformance::one_of_many_concurrent_claims_wins`, `::an_agent_holds_at_most_one_active_lease`; both |
| R13 | Every lease shall carry an epoch larger than every epoch issued before for that task, and a renew, release, event append or finish that names an older epoch shall be refused and change nothing. | F, S1 | must | `storage_conformance::epochs_only_grow`, `::a_stale_epoch_is_refused`; both |
| R14 | An event append shall store a batch in one transaction and skip every sequence number already stored for that run, reporting how many it stored and how many it skipped. | F, M | must | `storage_conformance::repeated_event_batch_stores_nothing_twice`; both |
| R15 | The port shall be entered only through `Storage::read` and `Storage::write`, each taking a caller (principal and project), and a call shall see and change only rows of the caller's project. | F, B4 | must | `storage_conformance::a_caller_sees_only_its_project`, `::a_write_to_another_project_is_refused`; both |
| R16 | After the engine PR, `orch-core` shall export no whole-document state store, no merge-key rule and no implicit reorder on save, and `docs/TRAITS-FROZEN.md` §2 shall describe port v2. | B3 | must | `git grep -nE 'trait StateStore\|apply_merge' crates/orch-core crates/orch-engine/src` prints nothing; reviewer check of TRAITS-FROZEN §2 against the port; macOS |
| R17 | The file backend shall implement port v2, so that the conformance suite runs whole on both backends; after the removal PR it shall live in a crate that only dev-dependencies may name, for one release. Atomicity across a process stop is required of SQLite only. | B1 | must | `cargo xtask crate-graph` fails when a product crate depends on it (`xtask` test `file_reference_is_dev_only`); both |
| R18 | A new database shall have exactly the tables listed in "Schema for 1.1", all `STRICT`, with `project_id` on every table outside the daemon-wide group and in the primary key wherever the sketch has it there. | G, B4, O2 | must | `crates/orch-infra/tests/sqlite_schema.rs::new_database_matches_the_schema_snapshot`, `::every_project_table_carries_project_id`; both |
| R19 | The schema shall contain, from the first migration, every column named in "Columns kept for later phases". | G, O2 | must | `sqlite_schema::kept_columns_are_present`; both |
| R20 | No 1.1 operation shall put a task in `proposed`, `landing` or `needs_attention`, and every task that becomes `done` shall get `landing_state = 'skipped'`. | B13 | must | `crates/orch-engine/tests/task_statuses.rs::no_operation_enters_a_1_5_status`, `::done_is_recorded_as_skipped`, `sqlite_schema::done_without_landing_state_is_rejected`; both |
| R21 | Migrations shall be numbered, applied in order inside one transaction each, recorded in `schema_migrations`, and never reversed; a failed migration shall leave the database at the earlier version and stop the start. | G | must | `crates/orch-infra/tests/sqlite_migrations.rs::migrations_apply_in_order_and_are_recorded`, `::failed_migration_leaves_the_earlier_version`; both |
| R22 | When the database's schema version is newer than the binary's, the daemon and every offline command shall refuse to open it and name both versions. | G | must | `sqlite_migrations::newer_schema_is_refused_with_both_versions`; both |
| R23 | Every connection shall run with WAL journal, `synchronous = NORMAL` unless `storage.synchronous` is `full`, foreign keys on and a busy timeout of 5,000 ms. | B12 | must | `crates/orch-infra/tests/sqlite_connection.rs::connections_have_the_required_settings`, `::synchronous_full_is_a_setting`; both |
| R24 | Exactly one connection shall write, every write shall start with `BEGIN IMMEDIATE`, and every other connection shall be read-only. | B6, B12 | must | `sqlite_connection::a_read_connection_refuses_writes`, `crates/orch-infra/tests/sqlite_writer.rs::writes_run_one_at_a_time_on_one_connection`; both |
| R25 | A read shall see one consistent snapshot from its first statement to its last, whatever commits meanwhile. | B6 | must | `sqlite_writer::a_read_snapshot_does_not_see_a_later_commit`; both |
| R26 | The writer shall always take a queued claim or lease transaction first, and otherwise the queued transaction with the earliest deadline, where the deadline is arrival plus 200 ms for entity changes, 1 s for events and 5 s for maintenance; a bulk job shall have at most one transaction queued at a time. A claim shall therefore wait behind at most the one transaction in progress, within its 50 ms. | G | must | `sqlite_writer::a_claim_runs_before_everything_queued` (fake clock), `::earliest_deadline_runs_first_among_the_other_lanes`, `::overdue_maintenance_does_not_delay_a_claim`, `::no_lane_starves_under_a_flood_of_another`; both |
| R27 | Bulk work (event batches, pruning, expiry, import) shall be cut into transactions of at most 1,000 rows or 20 ms of work; an atomic operation over that bound shall run whole and be counted in `writer_txn_over_bound_total`. (AP8) | G | must | `sqlite_writer::bulk_work_is_cut_at_1000_rows`, `::an_atomic_operation_over_the_bound_runs_whole_and_is_counted`; both |
| R28 | A side effect outside the database shall be written as an `intents` row in the transaction that decides it, performed after the commit, and marked done; an intent left by a stopped daemon shall be handled at the next start as its kind states in "Transactions and intents". | G, H | must | `crates/orch-engine/tests/intents.rs::intent_is_written_with_its_change`, `::pending_intent_is_handled_after_restart`, `::a_repeated_intent_has_no_second_effect`; both |
| R29 | The data directory shall have the layout and modes in "Data directory"; the daemon shall create it with those modes and refuse to start when it exists with group or other access or another owner. | B5 | must | `crates/orch-daemon/tests/daemon_data_dir.rs::layout_and_modes_are_as_specified`, `::a_directory_open_to_others_is_refused`; both |
| R30 | When a daemon or an offline command is pointed at a data directory that another holds, it shall change nothing, exit 4 and print the holder's kind, pid and start time. | B5, G6 | must | `crates/orch-daemon/tests/daemon_data_dir.rs::second_daemon_refuses_and_names_the_holder`, `crates/orch-cli/tests/db_offline.rs::offline_command_refuses_while_a_daemon_holds_the_directory`; both |
| R31 | While a 1.1 daemon or `orch import` runs for a project directory, a 1.0 daemon shall not be able to start on it, and a 1.1 daemon shall refuse to start while a 1.0 daemon holds it; neither shall create or write a 1.0 file. Where the 1.0 lock file does not exist (no 1.0 daemon ever ran there), or if F9 proves false, 1.1 shall only refuse while a 1.0 daemon answers, and the manual shall say that the exclusion is then a procedure and not a lock. | B2, F9 | must | `crates/orch-daemon/tests/daemon_legacy_lock.rs::legacy_owner_lock_is_held_without_writing`, `::start_is_refused_while_a_legacy_daemon_holds_the_project`, `::an_absent_legacy_lock_file_is_not_created`; both |
| R32 | The daemon shall refuse to start when the data directory is on a network filesystem. | B12 | must | `daemon_data_dir::network_filesystem_is_refused` (injected filesystem type); both |
| R33 | The data directory shall be `ORCH_DATA_DIR` when set (absolute, else exit 2), otherwise the directory that `<data home>/projects/<project id>` records, which `orch init` and `orch import` write when they create `<data home>/<name>`; the data home is `ORCH_DATA_HOME` or `~/.orchestry/daemons`; the repository's cargo configuration shall set `ORCH_DATA_HOME` inside `target/`. (O1) | B5, DL | must | `daemon_data_dir::default_name_is_derived_from_the_project_directory`, `::the_project_record_names_the_directory`, `::relative_override_is_refused`; `./xtask/gate.sh pr` `real-home` step unchanged; both |
| R34 | `orch init` and `orch import` shall write `.orch-project` (the project id and a newline) in the project directory; a client shall find the project by `ORCH_SOCKET`, then `ORCH_PROJECT_ROOT`, then the nearest ancestor holding `.orch-project`; a directory with 1.0 state and no `.orch-project` shall get exit 3 and the hint `orch import --dry-run`. When the project directory is not the one the database records, the daemon shall adopt the new path if the recorded one no longer holds that project id (a move) and refuse, naming both paths, if it still does (a copy). (O1) | O1 | must | `crates/orch-cli/tests/project_pointer.rs::client_finds_the_project_by_pointer`, `::unimported_project_gets_the_import_hint`, `::a_moved_project_keeps_its_state`, `::a_copied_project_is_refused_with_both_paths`; both |
| R35 | Starting a run shall be one transaction: the lease, the epoch, the run row, the task's status and attempts, the removal of its retry row and the intent to start the process. | H | must | `crates/orch-engine/tests/transactions.rs::start_run_is_all_or_nothing` (a stop injected before and after each statement); both |
| R36 | Finishing a run shall be one transaction: the run's outcome and usage, its result row, the task's next status, its retry row or its `landing_state`, the release of the lease, the agent's totals and position, and the intents to clean up. A finish that names a stale epoch shall change only that run's row. | H | must | `transactions::finish_run_is_all_or_nothing`, `::finish_with_a_stale_epoch_changes_only_the_run`; both |
| R37 | Cancelling a task shall be one transaction that also ends its run, releases its lease, removes its retry row and writes the intent to stop its process; the process shall be stopped after the commit and again after a restart if it still runs. | H | must | `transactions::cancel_task_is_all_or_nothing`, `intents::stop_intent_is_repeated_until_the_process_is_gone`; both |
| R38 | Pausing a goal shall be one transaction with the cancellations it implies, and no claim for a task it cancelled shall commit after it. | H | must | `transactions::pause_goal_is_atomic_with_dispatch` (claims and pause raced 1,000 times); both |
| R39 | A permanent failure shall be one transaction with its whole cascade. | H | must | `transactions::permanent_failure_and_its_cascade_are_one_transaction`, `::a_cascade_over_1000_rows_is_still_one_transaction`; both |
| R40 | Leases and the retry table shall replace `state.json`; a retry shall never be dropped for lack of room. (AP10) | H | must | `crates/orch-engine/tests/leases.rs::running_and_claimed_are_leases`, `crates/orch-engine/tests/retries.rs::the_101st_retry_is_kept`; both |
| R41 | At every start the daemon shall take a new generation and treat every lease of an earlier generation as 1.0's startup recovery treats a running entry (`docs/internal/orchestration.md`, "Startup recovery"): a run whose recorded process is gone is ended and its task handled as 1.0 handles it; a run whose recorded process is alive keeps its lease until the process exits; a run still `preparing` is ended. It shall never start a process for an intent of an earlier generation. | H, G9 | must | `leases::recovery_matches_1_0_for_dead_alive_and_preparing_runs`, `intents::a_start_intent_of_an_earlier_generation_is_not_performed`; both |
| R42 | A lease shall not be taken from a live local run because the machine was suspended: after a resume the daemon shall renew the leases of processes it still supervises before it expires any. (F10) | E | must | `leases::a_clock_jump_does_not_expire_a_supervised_run` (clock seam); both. A real suspend: recorded check on macOS |
| R43 | The 1.1 maintenance jobs in "Maintenance jobs" shall run from one timer with a due time per job, stored so that a restart neither skips a due job nor runs every job at once. (O5) | H, O5 | must | `crates/orch-engine/tests/maintenance.rs::each_job_runs_when_due`, `::due_times_survive_a_restart`; both |
| R44 | The daemon shall start with dispatch as it was when it last stopped, and off after an import, a restore or a start that applied a migration; `--dispatch on\|off` shall override; the start shall print the state and its reason. (O4) | O4 | must | `crates/orch-daemon/tests/daemon_dispatch.rs::dispatch_resumes_as_it_was`, `::dispatch_is_off_after_import_restore_and_migration`; both |
| R45 | When a mutate or destructive request whose change is a database transaction carries a key already stored for its scope with the same request hash, the daemon shall make no change and return the first result marked as a replay. | I, G8 | must | `crates/orch-daemon/tests/ops_request_keys.rs::a_repeated_change_runs_once`, `::a_reply_lost_before_delivery_is_replayed`; both |
| R46 | When a request carries a key already stored for its scope with a different request hash, the daemon shall refuse it with `conflict`, execution `not_executed`. | I, G8 | must | `ops_request_keys::same_key_with_a_different_body_is_refused`; both |
| R47 | A key shall be stored in the same transaction as its change, kept for one hour and then deleted by the `request_keys.expire` job; a result over 64 KiB shall not be kept, and its replay shall say so. | I, O8 | must | `ops_request_keys::key_and_change_commit_together`, `::a_key_expires_after_an_hour`, `::a_result_over_64_kib_is_not_kept`; both |
| R48 | Every entity returned by the API shall carry its `revision`; a change that names `expected_revision` shall fail with `stale_revision` when it is not the stored one; a change that names none shall apply as in 1.0. | I | must | `crates/orch-daemon/tests/ops_revisions.rs::entities_carry_a_revision`, `::stale_expected_revision_is_refused`, `::a_change_without_one_applies`; both |
| R49 | Every list operation shall accept `limit`, `cursor`, `sort` and `with_count` with the bounds of R9. CLI list commands shall return all rows in their 1.0 shape and order unless `--limit` or `--cursor` is given: they walk the pages by the immutable sort `id`, so that every row present throughout the walk is returned exactly once, and order the result as 1.0 did. (AP4) | I | must | `crates/orch-daemon/tests/ops_paging.rs::list_operations_page`, `crates/orch-cli/tests/list_paging.rs::default_output_is_unchanged`, `::a_row_changed_during_the_walk_is_returned_once`, `::limit_returns_one_page_and_a_cursor`; both |
| R50 | `LOCAL_PROTOCOL` shall be 3, and a client and a daemon of different protocol versions shall refuse each other as in 1.0, with the frozen handshake and the stop intent unchanged. | I | must | `crates/orch-proto` test `protocol_is_3`, `crates/orch-daemon/tests/daemon_wire.rs::protocol_2_client_is_refused_by_a_protocol_3_daemon`, `::stop_intent_crosses_the_skew`; both |
| R51 | The error codes `stale_revision` and `quota_exceeded` shall be added with exit 1; the twelve existing codes and exits 1 to 7 shall keep their meanings. | I | must | `crates/orch-proto` test `existing_error_codes_and_exits_are_unchanged`, `::new_codes_exit_1`; both |
| R52 | The dispatcher shall decide whether a principal may run an operation in one function with an exhaustive match and no default arm, before any admission or storage call. | J | must | `crates/orch-daemon/tests/ops_authorization.rs::every_operation_has_a_row_for_every_principal_kind`; a reviewer confirms no other check site; both |
| R53 | A test principal of kind worker and one of kind task shall be refused every operation the matrix denies them, with `not_permitted` and no change, including any call for another project and any lease-bound call with a stale epoch. | J, G5 | must | `ops_authorization::worker_is_refused_what_the_matrix_denies`, `::task_is_refused_what_the_matrix_denies`, `::another_project_is_refused`; both |
| R54 | Every `db.*` operation, `attention.ack`, `policy.set`, `orch import`, `orch db restore` and `orch db vacuum` shall be refused over anything but the local socket or the local lock, to an MCP session of any mode, and in agent context. (AP3) | B8, B14 | must | `ops_authorization::owner_only_operations_are_refused_to_mcp_and_agent_context`, `crates/orch-cli/tests/db_offline.rs::offline_commands_refuse_in_agent_context`; both |
| R55 | The scheduler shall be an interface that receives a view of open tasks, agents and leases and returns task-to-agent pairs; it shall not write, and a pair that loses its claim shall start no run. | K, B9 | must | `crates/orch-engine/tests/scheduler.rs::scheduler_has_no_write_access`, `::a_raced_assignment_starts_one_run`; both |
| R56 | The 1.1 scheduler shall choose the same task and agent as 1.0 for every scenario of the order list, including the rotation of tied agents. | K, B9, G1 | must | `crates/orch-engine/tests/order_conformance.rs` on both backends (`::tied_agents_rotate`, `::dispatch_order_matches_1_0`); both |
| R57 | The daemon shall record the duration of every scheduler tick and the number of open tasks by status, and return them from `metrics.get`. | K | must | `crates/orch-daemon/tests/ops_metrics.rs::tick_duration_and_open_tasks_are_reported`; both |
| R58 | The blob store shall write a blob to `blobs/tmp/`, sync it, rename it to `blobs/sha256/ab/cd/<hash>` and only then let a row name it; a read shall fail when the bytes do not hash to the name. | L, B7 | must | `crates/orch-infra/tests/blob_store.rs::bytes_exist_before_the_row`, `::a_corrupted_blob_is_detected_on_read`; both |
| R59 | A rendered prompt shall be stored once per content hash and named by `runs.prompt_sha256`; a task attachment shall be a blob named by `task_attachments.sha256`. | L | must | `blob_store::equal_prompts_are_stored_once`, `crates/orch-engine/tests/attachments.rs::attachment_is_a_blob`; both |
| R60 | The `blobs.sweep` job shall delete a blob only when no row has named it for 7 days, and shall remove files in `blobs/tmp/` older than 24 hours. | L | must | `maintenance::unreferenced_blob_is_deleted_after_seven_days`, `::a_referenced_blob_is_never_deleted`; both |
| R61 | Run events shall be stored with a per-run sequence number from 1, read by range and by tail from an index, and a batch the daemon had to drop shall leave a row in `run_event_gaps`. | M | must | `crates/orch-engine/tests/run_events.rs::events_are_numbered_from_one`, `::tail_read_returns_the_last_n`, `::a_dropped_batch_leaves_a_gap`; both |
| R62 | The `events.prune` job shall delete the events of a run that finished successfully more than `retention.events.success_days` (90) ago or otherwise more than `retention.events.failed_days` (365) ago, shall skip a class set to `never`, shall record `events_pruned_at` and `events_pruned_count` on the run, and shall never delete a run, a result or a blob named by one. | M, B10 | must | `maintenance::events_are_pruned_by_outcome_and_age`, `::never_disables_pruning`, `::results_survive_pruning`, `::an_interrupted_prune_resumes`; both |
| R63 | The measured throughput shall meet the budget of "Writer lanes", or events shall be stored as per-run files in the data directory behind the same port operations. | G7, D | must | The record of R5; if the fallback ships, `storage_conformance` event scenarios pass on it; macOS and native Linux |
| R64 | `orch import` shall open every source file read-only and create, change or delete nothing under the source `.orchestry/`. | N, B2 | must | `crates/orch-cli/tests/import.rs::source_tree_is_byte_identical_after_import` (content and metadata snapshot; source made read-only); both |
| R65 | `orch import --dry-run` shall write nothing anywhere and shall report the counts per entity, every file it cannot read, every unknown key, every dangling reference, every field over 64 KiB, the policy it would turn on, the in-flight state it would close and the space it needs. | N, O7 | must | `import::dry_run_writes_nothing`, `::dry_run_report_lists_every_problem_class` (pathology corpus); both |
| R66 | `orch import` shall read its source in bounded memory and bounded transactions, refuse a source file over the bounds in "Import rules", and commit an entity and its `import_manifest` row together. | N | must | `import::memory_stays_bounded_on_a_large_project` (generated corpus of the live project's shape), `::oversize_source_file_is_refused_by_name`; both |
| R67 | When an import was stopped part-way, `orch import --resume` shall continue from the manifest, shall refuse when a source file it already imported has changed, and the daemon shall refuse to start on an unfinished import. | N | must | `import::resume_completes_an_interrupted_import` (stops injected at every batch boundary), `::resume_refuses_a_changed_source`, `daemon_data_dir::unfinished_import_is_refused`; both |
| R68 | `orch import` shall refuse, changing nothing, when a 1.0 or 1.1 daemon holds the project or the data directory, when the data directory already holds a finished project, or when free space is under the disk floor plus the estimate. (O3) | N, O3 | must | `import::refuses_while_a_daemon_runs`, `::refuses_a_second_project`, `::refuses_without_space`; both |
| R69 | An imported project shall keep its ids, its order (`position` from each `_index.json`), its retry queue, its totals and its runs; state that was in flight shall be closed as 1.0's reconcile would close it; every task's next epoch shall be above every epoch given to its imported runs. | N | must | `import::order_and_rotation_survive_import` (the order scenarios run on the imported database), `::in_flight_state_is_closed`, `::epochs_continue_after_import`; both |
| R70 | A known `config.yml` key shall be imported to its setting; unknown keys shall be stored in the `legacy.config` object, at most 64 KiB, which the daemon never reads; `scheduling.max_concurrent_agents` shall become the project's optional `run_cap`; a project with an agent whose policy is `auto` shall be imported with `preapproval` on. (O7) | N, O7, B14 | must | `import::unknown_config_keys_go_to_the_legacy_object`, `::max_concurrent_agents_becomes_run_cap`, `::auto_policy_turns_preapproval_on`; both |
| R71 | The importer shall be verified on a copy of the owner's live project, outside the repository: a dry run, an import stopped and resumed, `orch db verify`, and counts equal to the dry run's; the record shall hold counts, sizes, durations and hashes only. | G2, DL | must | `cargo xtask import-verify --source <copy> --record <dir>`; the owner's machine, with the owner's permission |
| R72 | `orch db status` shall report the path, sizes, schema version, generation, journal and sync modes, free space and floor, row counts, the last run of each maintenance job and the import record. | O | must | `crates/orch-cli/tests/db_commands.rs::status_reports_the_listed_fields`; both |
| R73 | `orch db verify` shall run SQLite's integrity and foreign-key checks and the invariants in "`orch db` commands", check every blob's size and hash with `--blobs`, and exit 1 naming each failure. | O | must | `db_commands::verify_passes_on_a_healthy_database`, `::verify_names_each_planted_fault`; both |
| R74 | `orch db query` shall run one read-only statement on a read-only connection, refuse anything that writes or attaches, and stop at its row and time limits. | O | must | `db_commands::query_refuses_writes_and_attach`, `::query_stops_at_its_limits`; both |
| R75 | `orch db backup` shall write, while the daemon runs, one directory holding a consistent copy of the database, every blob that copy names, the repositories and a manifest with hashes. | O, G3 | must | `crates/orch-cli/tests/db_backup_restore.rs::backup_is_consistent_under_concurrent_writes`; both |
| R76 | `orch db restore` shall run with the daemon stopped, verify the backup against its manifest before changing anything, keep the replaced state aside, and leave the database, the blobs and the repositories as they were at the backup. | O, G3 | must | `db_backup_restore::backup_then_restore_round_trips_database_blobs_and_repos`, `::a_damaged_backup_is_refused_before_any_change`; both |
| R77 | A restore shall start a new generation; no epoch issued before it, by the replaced state or by the backup, shall be issued after it. | O, G9 | must | `crates/orch-cli/tests/restore_fencing.rs::no_epoch_is_reused_after_restore`, `::generation_grows_even_onto_an_empty_directory`; both |
| R78 | A restore shall list every identity and token that the backup holds enabled, mark those the replaced state had disabled or revoked, and proceed only on a typed confirmation or `--yes`; without a terminal and without `--yes` it shall exit 2. | O, G9 | must | `restore_fencing::restore_lists_reenabled_identities_and_tokens`, `::restore_without_confirmation_changes_nothing`; both |
| R79 | `orch db prune` shall run the event pruning now, with `--dry-run` and `--older-than`, and `orch db vacuum` shall rebuild the database with the daemon stopped. | O | must | `db_commands::prune_dry_run_deletes_nothing`, `crates/orch-cli/tests/db_offline.rs::vacuum_runs_offline_and_keeps_every_row`; both |
| R80 | `orch db export` shall write one file holding one project's rows and blob bytes; `orch import --from <file>` into an empty data directory shall reproduce every exported row and every blob hash. | O, B8, G4 | must | `crates/orch-cli/tests/db_export_import.rs::export_then_import_reproduces_every_row_and_hash`, `::a_truncated_export_is_refused`; both |
| R81 | Only the daemon shall write the attention queue; at most one open item shall exist per project, kind and subject; the owner shall be able to list, show and acknowledge items. | P | must | `crates/orch-engine/tests/attention.rs::one_open_item_per_subject`, `crates/orch-cli/tests/attention.rs::owner_lists_and_acknowledges`; both |
| R82 | While free space is under the floor, a blocking `disk_floor` item shall be open; it shall resolve by itself when free space is back above the floor plus 1 GiB. | P, B11 | must | `attention::disk_floor_opens_and_resolves` (disk probe seam); both |
| R83 | While dispatch is off and a task that could be dispatched has waited 10 minutes, a warning `dispatch_off_with_work` item shall be open; it shall resolve when dispatch turns on or no such task remains. | P | must | `attention::dispatch_off_with_work_opens_and_resolves` (fake clock); both |
| R84 | Resolved attention items shall be deleted after `attention.resolved_retention_days` (90); `orch doctor` shall list open items and exit 1 while a blocking item is open. (O6) | P, O6 | must | `maintenance::resolved_attention_expires`, `crates/orch-cli/tests/attention.rs::doctor_fails_on_a_blocking_item`; both |
| R85 | While free space on the filesystem of the data directory or of the project's workspaces is under the larger of 5 GiB and 5% of that filesystem, the daemon shall start no run and shall refuse operations that add work with `quota_exceeded`, and shall keep finishing, cancelling, reading and pruning. | B11 | must | `crates/orch-engine/tests/disk_floor.rs::no_run_starts_under_the_floor`, `::new_work_is_refused_with_quota_exceeded`, `::running_work_still_finishes`; both |
| R86 | Every run shall get its usage recorded when it ends, whatever its outcome: the five token counts, each empty when the agent CLI reported none, never zero in its place. | Q | must | `crates/orch-engine/tests/usage.rs::usage_is_recorded_for_every_outcome`, `::unreported_tokens_are_null_not_zero`; both |
| R87 | Every run shall record the model that ran and the agent CLI's version, each empty when it cannot be learnt, and a failure to learn either shall not fail the run. | Q | must | `usage::model_and_adapter_version_are_recorded`, `::a_failed_version_probe_does_not_fail_the_run`; both |
| R88 | The manual, the RUNBOOK, ARCHITECTURE, the README upgrade notes, the CHANGELOG and a storage reference under `docs/` shall describe 1.1 as built, and the manual shall say plainly that `preapproval` is implicit trust of the agents. | R, B14 | must | Fact check of each document against the code at the candidate commit; `cargo test -p xtask` (ARCHITECTURE checks); macOS |
| R89 | Remote peers shall keep working beside a 1.1 daemon with the peer protocol unchanged, at the cost of the source changes listed in "orch-peer". (AP1) | B15 | must | `cargo test -p orch-peer`; a 1.0 peer and a 1.1 daemon exchange a task and its result in both directions (recorded check); both, and a second host |
| R90 | No fixture, test, PR or record shall contain content from a real project; fixtures shall be generated from a seed or written by hand. (O10) | O10, DL | must | `cargo xtask fixture-gen --check` reproduces every generated fixture byte for byte; reviewer check of hand-written ones; both |
| R91 | No test shall reach a real project's daemon, the real `~/.orchestry` or a data directory outside its own temporary root. | DL | must | `./xtask/gate.sh pr` (`home-hygiene`, `real-home`); both |
| R92 | Every PR of the stack shall leave `main` green and working, with the file backend working until the PR that removes it. | DL, B1 | must | CI at each PR's final head; `cargo test --workspace` at each merge; Linux CI |
| R93 | Only an owner session shall set `preapproval`; an agent's own `approval_policy` of `auto` shall have effect only while the project's `preapproval` is on. | B14 | must | `crates/orch-engine/tests/preapproval.rs::auto_needs_the_project_policy`, `ops_authorization::only_the_owner_sets_preapproval`; both |
| R94 | — | X | won't | Evidence, quarantine, uploads, text search, HTTP, TLS, tokens, enrolment, certificates, workers, the hub, landing, proposals, many projects per daemon, removing orch-peer, Postgres, an indexed scheduler, object storage, token budgets, outbound notification and Windows are out of scope (X). |
| R95 | — | DL | won't | No tag and no version bump: cutting 1.1.0 is the owner's separate decision. |
| R96 | An agent process shall not run its agent unless the run's pid and start time were committed first: the daemon shall create the process held, commit, then release it, and a held process whose daemon is gone shall exit without running the agent. | review finding (a stop between starting a process and recording it left an untracked agent on a workspace that recovery would hand to a second run) | must | `crates/orch-engine/tests/transactions.rs::a_stop_between_spawn_and_commit_runs_no_agent`, `crates/orch-infra` process test `held_process_exits_when_its_parent_is_gone`; both |
| R97 | A text value over 64 KiB, written through the API or imported, shall be stored in the blob store, with the row holding its hash, and shall be returned whole on read. | B7; review finding | must | `storage_conformance::text_over_64_kib_round_trips_through_the_blob_store`, `import::text_over_64_kib_becomes_a_blob`; both |
| R98 | Reads shall be served by a pool of `storage.read_connections` connections (default 4); a read that finds none free shall wait up to the admission deadline and then fail with `overloaded`, never open another connection. | B6; review finding | should | `sqlite_writer::reads_wait_for_a_free_connection_then_fail_overloaded`; both |

## Storage port v2

The contract of deliverable F. Rust signatures are fixed by the port PR and recorded in `docs/TRAITS-FROZEN.md`.

| Name | Meaning |
|---|---|
| `Caller` | `{ principal, project }`, passed to both entry points. `Principal` is `Owner { identity }`, `Worker { identity }` or `Task { identity, task, run, epoch }`. |
| `Storage::read(caller)` | Returns a `ReadSnapshot`: every read through it sees the same committed state. |
| `Storage::write(caller, lane, work)` | Runs `work` on a `UnitOfWork` in one transaction on the writer. An error from `work` rolls everything back. `lane` is `Claims`, `Entities`, `Events` or `Maintenance`. |
| `Revision` | Positive integer on every entity. `create` stores 1. `update(entity)` and `delete(id, revision)` compare with the stored value and fail with `StaleRevision { kind, id, expected, actual }`. |
| `PageRequest` | `{ limit, cursor, sort, with_count }`. `limit` defaults to 100; 0 or more than 1,000 is refused. |
| `Page<T>` | `{ items, next_cursor, total }`. `next_cursor` is absent on the last page. `total` is present only when `with_count` was set. |
| `Cursor` | Opaque text of at most 512 bytes: the sort, the last row's sort values and id, and a hash of the filter. It holds no snapshot. |
| `position` | Explicit order (R11). `move_to_end(kind, id)` is its only other writer. |
| `claim_lease(new)` | Inserts a lease only if the task has none and the agent has no active one, and gives it the task's next epoch. Returns `Granted(lease)` or `Held(lease)`. |
| `renew_lease`, `release_lease` | Take `(task, epoch)`; refuse with `StaleEpoch` unless the epoch is the lease's. |
| `append_events(run, epoch, batch)` | Inserts with "ignore if `(run, seq)` exists". Returns `{ stored, skipped }`. `record_event_gap(run, after_seq, dropped)` records a loss. |
| `read_events(run, after_seq, limit)`, `read_events_tail(run, count)`, `prune_events(run, max_rows)` | Range read, tail read, and a bounded delete that reports whether the run is finished. |
| `put_retry`, `due_retries(now, limit)`, `delete_retry` | The retry table; one row per task; no cap. |
| `add_intent(kind, payload)`, `pending_intents(limit)`, `finish_intent(id)` | Side effects (R28). |
| `check_request_key(scope, op, hash)`, `store_request_key(scope, op, hash, result)` | Return `New`, `Replay(result)`, `ReplayWithoutResult` or `Mismatch`. |
| `open_attention`, `ack_attention`, `resolve_attention` | The attention queue; `open` is a no-op when an open item exists for the subject. |

Each list has a fixed set of sorts. The default sort of a list is its 1.0 order, served by an index, and
deliverable B confirms each line. Every list also offers the sort `id`, whose values never change (R49):

| List | Default order | Other sorts |
|---|---|---|
| tasks | status rank, `updated_at` descending, `position` | `position`, `priority`, `created_at`, `updated_at` |
| agents | `position` | `name` |
| goals | status rank, `updated_at` descending, `position` | `created_at` |
| messages | `created_at`, `position` | none |
| context entries | `key` | none |
| teams | `created_at`, `id` (1.0's order came from the filesystem and was not a contract) | `name` |
| runs | `started_at` descending, `id` | per task: `run_no` |

The engine calls `move_to_end` only at the call sites deliverable B lists as observable. Until that list
exists the rule is: wherever 1.0 saved a task, agent, goal or message.

Removed with port v1: `StateStore` (replaced by leases, retries, counters and project settings), the rule that
every YAML read resolves merge keys (only the importer reads YAML), and the reorder inside `save`.

## Schema for 1.1

The starting point is the 2.0 sketch attached to the goal: 50 tables and one FTS5 index. Times are UTC
milliseconds; the API keeps ISO-8601 strings. Recommendation for O2: 1.1 creates 33 tables.

| Group | Created in 1.1 |
|---|---|
| Projects and settings | `projects`, `project_sources`, `project_config`, `templates` |
| Work | `goals`, `tasks`, `task_dependencies`, `task_labels`, `task_scopes`, `task_attachments` |
| Agents and teams | `agents`, `teams`, `team_members`, `team_tasks` |
| Execution | `runs`, `task_epochs`, `leases`, `retry_queue`, `intents`, `request_keys`, `results` |
| Raw output | `run_events`, `run_event_gaps` |
| Blobs | `blobs` |
| Messages and context | `messages`, `context_entries` |
| Housekeeping, per project | `attention` (its `project_id` is empty for a daemon-wide item), `counters`, `import_manifest` |
| Daemon-wide | `daemon_config`, `identities`, `identity_tokens`, `schema_migrations` |

| Arrives by migration in | Tables |
|---|---|
| 1.2 | `evidence`, `quarantined_hashes`, `evidence_bodies`, `uploads`, `evidence_tags`, `evidence_refs`, `search_docs`, and the FTS5 index `search` |
| 1.3 and 1.4 | `auth_failures`, `identity_projects`, `identity_certs`, `enrolment_codes` |
| 1.5 | `source_log`, `landings`, `source_push_grants`, `lease_declines`, `workers`, `worker_adapters` |

`identities` and `identity_tokens` are created now because the restore of gate line 9 must already list them.
In 1.1 `identities` holds two built-in rows, the local owner and the local worker, and `identity_tokens` is
empty outside tests.

**Columns kept for later phases** (R19), all present and unused or constant in 1.1:

- `runs`: `model`, `adapter_version`, `worker_id`, `epoch`, `run_no`, `attempt_no`, `counted`, `branch`,
  `base_sha`, `head_sha`, `head_verified_at`, `push_window_until`, `events_pruned_at`, `events_pruned_count`,
  and the five token columns.
- `tasks`: `accepted_by`, `accepted_at`, `accepted_run_id`, `landing_state`, `landed_commit`, `landed_into`,
  `landed_at`, `landing_attempts`, `landing_conflict_paths`, `attention_reason`, `attention_since`,
  `created_by_kind`, `created_by_run_id`, `outputs`, `legacy_proof`, `position`, with the sketch's `CHECK`s.
- `goals`: `integration_branch`, `integration_head`, `published_at`, `published_commit`, `published_by`.
- `task_epochs` as a table, never deleted from and never reset.
- `project_id` in every primary key outside the daemon-wide group.
- `leases`: `worker_id`, `epoch`, `state`, `offered_at`, `branch`, `base_sha`, `fetch_ref`.

**Differences from the sketch**, each for the owner's approval with O2:

| # | Difference | Reason |
|---|---|---|
| 1 | `tasks`: the check `landing_state IS NULL OR accepted_run_id IS NOT NULL` becomes `landing_state IS NULL OR landing_state = 'skipped' OR accepted_run_id IS NOT NULL`. | B13 marks every completed task `skipped`, and a 1.0 task can be done with no run (approved by hand, or its runs are gone). "Nothing to land" needs no run in any release. |
| 2 | `leases`: `branch`, `base_sha` and `fetch_ref` are nullable. | 1.1 has no hub. The table is small and short-lived, so 1.5 can rebuild it to tighten them. |
| 3 | `runs` gains `local TEXT`: a JSON object holding the pid and the workspace path. | 1.0 records both and reconcile needs the pid. They are facts of this daemon's machine, so they stay out of named columns and go when workers own processes. |
| 4 | `goals` and `messages` gain `position INTEGER NOT NULL`. | Both stores are in last-write order in 1.0 (F3). The order cannot be recovered once the index files are no longer read. |
| 5 | `runs.status` and `tasks.status` get no `CHECK`. | As in the sketch; the port's types validate them, and 1.5 adds run statuses. |
| 6 | Constants 1.1 writes into columns that only later phases vary: `leases.worker_id` and `runs.worker_id` are the built-in local worker identity; `leases.state` is `active`; `runs.counted` is 1 with `attempt_no` set; `tasks.created_by_kind` is `owner`, `agent` (a session in agent context; `created_by_run_id` set when that task holds a lease) or `daemon`, and `import` for imported tasks. | The sketch declares these `NOT NULL` or checks them, so 1.1 must say what it stores. `import` is a fourth value of `created_by_kind`: 1.0 did not record who created a task. |

Rows the daemon keeps without a table of their own: the generation and the saved dispatch state in
`daemon_config`; each maintenance job's times under `maintenance/<job>` in `daemon_config`; 1.0's totals in
`counters`; `run_cap`, `preapproval`, the retention settings and `legacy.config` in `project_config`.

## Transactions and intents

| Operation | Lane | In the one transaction | Intents written with it |
|---|---|---|---|
| Start a run (R35) | Claims | Check the disk floor flag and the run cap; `claim_lease`; next epoch; insert the run as `preparing`; task to `in_progress`, attempts; delete its retry row; request key. | `process.start` |
| Finish a run (R36) | Entities | Epoch check; run outcome, usage, model, adapter version; result row; task to `review`, `done` with `landing_state = 'skipped'`, `retrying` with a retry row, or `failed`; release the lease; agent totals and `move_to_end`; counters. | `workspace.remove` when 1.0 would remove it |
| Cancel a task (R37) | Entities | Task to `cancelled`; run to `cancelled`; release the lease; delete its retry row; request key. | `process.stop`, `workspace.remove` |
| Pause a goal (R38) | Entities | Goal to `paused`; cancel the tasks 1.0's pause cancels, each as above. | `process.stop` per cancelled run |
| Permanent failure (R39) | Entities | Task to `failed`; the cascade over its dependents and its goal, as 1.0 decides it; release the lease. | `workspace.remove` |
| Prune a run's events (R62) | Maintenance | Delete at most 1,000 events of the run and add the number to `events_pruned_count`; the last transaction sets `events_pruned_at`. | none |

| Intent kind | Performed | After a restart |
|---|---|---|
| `process.start` | Prepare the workspace, store the prompt blob, create the agent process held, commit the run as `running` with its pid and start time in a second transaction, then release the process (R96). | Never repeated. Startup recovery handles the run as R41 says: `preparing` is ended; `running` is kept while its process lives. |
| `process.stop` | Stop the process group named in the run's `local` data, checked against its start time. | Repeated until the process is gone or was never this run's. |
| `workspace.remove` | Remove the task's workspace. | Repeated; a missing workspace is success. |
| `blob.delete` | Delete a blob file after its `blobs` row is gone. | Repeated; a missing file is success. |

Change notifications to subscribers are not intents: they stay in memory, and a subscriber that missed one gets
the resync that protocol 2 already defines. Done intents are deleted after 7 days.

## Writer lanes

| Lane | Work | Longest wait |
|---|---|---|
| 1 `Claims` | Claims, renewals, releases, lease expiry | 50 ms |
| 2 `Entities` | Every entity change and the multi-entity operations above | 200 ms |
| 3 `Events` | Event batches and gap rows | 1 s |
| 4 `Maintenance` | Pruning, expiry, sweeps, checkpoints | 5 s |

Lane 1 always goes first; among lanes 2 to 4 the earliest deadline wins, and a bulk job queues its next
transaction only when the last has committed (R26). Lane 1 cannot starve the others: its work is bounded by
the number of tasks and agents. One transaction is bounded to 1,000 rows or 20 ms of work (R27). A collector hands over a batch when it holds
256 events or the oldest is 250 ms old. Its buffer holds at most 4,096 events or 4 MiB per run; past that it
drops the oldest raw output and records a gap. Results and usage travel on lane 2 and are never dropped.

**Claim-latency budget** (deliverable D, gate line 7). Under an event load of the larger of 2,000 events a
second and ten times the busiest minute found on the copy of the owner's project, with 1 KiB events, for at
least 10 minutes, with `synchronous = NORMAL`:

- a claim's wait in the queue is at most 50 ms at the 99th percentile;
- a claim commits within 70 ms of being queued at the 99th percentile and within 250 ms at the worst;
- no event batch waits more than 1 s, and no event is dropped.

If either platform misses a line, the file fallback of R63 ships. The record states the figures; a green test
suite is not a performance result.

## Request keys

- A request frame may carry `key`: 16 to 128 characters of `A-Z a-z 0-9 _ -`. The CLI makes one per
  invocation and resends the request once with the same key when the connection drops after sending.
  `--request-key` and `ORCH_REQUEST_KEY` let a script supply its own. Keys apply to mutate and destructive
  operations whose change is a database transaction. A key on any other operation is ignored, and the CLI
  does not resend those: every read and control operation, and `db.backup`, `db.export` and `db.prune`, whose
  effect is a file or a bounded series of transactions.
- Scope: `(identity, project, run, epoch, key)`. The daemon takes run and epoch from the caller's lease, never
  from the request; they are empty and 0 for an owner session.
- The stored hash is SHA-256 over the operation name and its arguments in canonical JSON (sorted keys, no
  insignificant space).
- The row is written in the transaction of the change, with `expires_at` one hour later. A replay returns the
  stored result with `"replayed": true`. A result over 64 KiB is stored as `result_truncated = 1` with no body,
  and its replay returns `"replayed": true, "result": null, "result_truncated": true`.
- A replay never fails with `stale_revision`: the first outcome stands.
- `request_keys.expire` deletes expired rows every 5 minutes, at most 1,000 per transaction.
- `state = 'in_flight'` stays in the schema for 1.5; 1.1 writes only `done`.

## Protocol 3 and errors

`LOCAL_PROTOCOL` becomes 3 in the PR that first changes the wire (R50). Changes from 2:

| Change | Detail |
|---|---|
| Request frame | Optional `key`. |
| Terminal frame | Optional `replayed`, `result_truncated`. |
| Entities | Every entity object gains `revision`. |
| Changes | Optional `expected_revision` on every operation that changes one entity. |
| Lists | `limit`, `cursor`, `sort`, `with_count` in; `items`, `next_cursor`, `total` out. |
| Welcome | Optional `project` (the project id) and `generation`. The frozen fields, including `project_id {dev, ino}` of `<project>/.orchestry`, are unchanged. |
| New operations | `db.status`, `db.verify`, `db.query`, `metrics.get`, `attention.list`, `attention.get`, `policy.get` (read); `db.backup`, `db.export`, `attention.ack`, `policy.set` (mutate); `db.prune` (destructive). |
| Kept operations | All 111 stay. `state.get` returns the same fields as before, assembled from leases, retries and counters. |

| Code | Exit | When |
|---|---|---|
| `stale_revision` | 1 | `expected_revision` is not the stored revision. Execution `not_executed`. The message names the entity and both revisions. |
| `quota_exceeded` | 1 | The disk floor refuses new work (R85). Execution `not_executed`. Reserved for later storage limits. |

A reused key with another body is `conflict`; an operation the matrix denies is `not_permitted`. Exits 1 to 7
keep their meanings: 1 general, 2 invalid arguments, 3 not initialized, 4 lock conflict, 5 adapter, 6
workspace, 7 no usable daemon. No exit code is added.

## Data directory

```
<data dir>/                      0700   one per daemon
  orch.lock                      0600   exclusive lock; never deleted; holds the holder's record
  orch.db  orch.db-wal  orch.db-shm   0600
  generation                     0600   the highest generation this directory has used; written and synced at every start
  blobs/                         0700
    sha256/ab/cd/<hash>          0600   ab and cd are the first four hex digits of the hash
    tmp/                         0700
  repos/                         0700   empty before 1.5
  backups/                       0700   one directory per backup, files 0600
  run/                           0700
    orch.sock                    0600
    daemon.json                  0600
```

- The lock is taken before anything else is opened. The holder's record is JSON: kind (`daemon`, `import`,
  `restore`, `vacuum`), pid, start time, binary version and executable path. A refusal prints it (R30).
- Default location at creation: `<data home>/<name>/`, with `<name>` the project directory's base name in lower case,
  characters outside `a-z 0-9 . _ -` replaced by `-`, cut to 32 bytes, then `-` and the first 12 hex digits of
  the SHA-256 of the canonical project path. `orch init` and `orch import` record it in
  `<data home>/projects/<project id>` (0600, one line), and from then on the project is found by its id, so
  moving the project directory does not lose its state (R34). `ORCH_DATA_DIR` names the directory outright.
- The socket follows 0012's rule: `run/orch.sock` when its absolute path is at most 100 bytes, otherwise the
  short path under `$TMPDIR`, recorded in `run/daemon.json`. `ORCH_SOCKET` still overrides.
- In the project directory 1.1 writes `.orch-project` (0644, at most 64 bytes) and, as 1.0 did,
  `.orchestry/workspaces/` and orch-peer's own files. It writes no other path under `.orchestry/` (AP6).
  The directory `.orchestry/` itself must stay: its device and inode are the project identity of the frozen
  handshake. `orch init` creates it for a new project.
- A data directory that names another project id than `.orch-project` does is refused with both values
  printed. A project directory other than the recorded one is a move or a copy, handled as R34 says.

## Authorization matrix

One function in the dispatcher (R52). In 1.1 every real session is the local owner (AP3); worker and task
principals exist for tests and for the port's caller checks. The security review of this spec confirms the
task column before the dispatcher PR.

| Operations | Owner | Worker | Task |
|---|---|---|---|
| Reads of the caller's project in the 1.0 catalogue; `subscribe`, `unsubscribe`, `request.cancel` | allow | deny | allow |
| `task.create`, `message.send`, `message.broadcast`, `message.mark_read`, `context.set` | allow | deny | allow; the row is stamped with the task's run |
| Every other mutate operation of the 1.0 catalogue | allow | deny | deny |
| Destructive operations | allow, when the session mode allows them | deny | deny |
| `dispatch.*`, `daemon.stop`, `transfer.*` | allow | deny | deny |
| `db.*`, `metrics.get`, `attention.*`, `policy.*`, any configuration write | allow | deny | deny |
| Port calls bound to a lease the caller holds, with its current epoch (append events, renew, release, finish) | allow | allow | deny |
| Anything for a project that is not the caller's | deny | deny | deny |
| Anything bound to a lease with an older epoch | deny | deny | deny |

Session modes (`read_only`, `allow_destructive`) and the MCP allowlist of 1.0 still narrow the owner column.
The new operations are in no MCP allowlist.

## Import rules

- `orch import [--from <project directory or export file>] [--dry-run] [--resume] [--json]`. It runs with both
  daemons stopped, under the data directory's lock and the 1.0 owner lock (R31, R68). It is offline: it does
  not use the socket (AP7).
- Bounds (R66): an entity document at most 16 MiB, an event line at most 64 KiB, an attachment at most 64 MiB,
  at most 1,000 rows per transaction, and memory that does not grow with the project. A file over a bound is
  reported by its path relative to the project and the import stops; nothing is cut short silently.
- Order: `position` follows each `_index.json`. An entity on disk that its index does not list is appended in
  id order and reported; an index entry with no file is reported and skipped.
- Runs: `run_no` and `epoch` count from 1 per task in start order; `task_epochs.next_epoch` is one more than
  the largest. `attempt_no` is 1.0's attempt number where it is unique within the task; a run that would
  repeat one is stored uncounted with the 1.0 number in `local`, and the dry run counts such runs. The rendered prompt becomes a blob. Events get `seq` from their line number; a line that does
  not parse becomes a gap. Model and adapter version are empty. Every event is imported; the pruning job
  applies the retention afterwards.
- `state.json`: the retry queue becomes retry rows; running and claimed entries are closed as 1.0's reconcile
  closes them; totals become counters.
- Tasks that are `done` get `landing_state = 'skipped'`; 1.0's recorded proof goes to `legacy_proof`.
- `config.yml`: R70. If the unknown keys exceed 64 KiB the import stops and names them by size.
- Text over 64 KiB in a description, message or context value becomes a blob (R97) and is listed by the dry
  run.
- A finished import writes `.orch-project` and records its time and source in `daemon_config`. orch never
  deletes a 1.0 file. `orch db status` shows that the 1.0 files are still present and how large they are (O3).
- An export file (R80) is a SQLite database of the same schema version holding one project's rows from the
  project tables, without `leases`, `intents` and `request_keys`, plus a table of blob bytes and a manifest
  with a hash of every table and blob.

## `orch db` commands

| Command | Daemon | Notes |
|---|---|---|
| `orch db status [--json]` | running | R72. |
| `orch db verify [--quick] [--blobs]` | running | `integrity_check` (or `quick_check`), `foreign_key_check`, and: every lease names a run of its task with the same epoch; every epoch is below its task's `next_epoch`; no two rows of a table share a `position`; every `done` task has a landing state; every named blob has a `blobs` row. |
| `orch db query '<statement>' [--limit n] [--json]` | running | One statement; 1,000 rows by default, 100,000 at most; 5 s. |
| `orch db backup [--to <directory>]` | running | Default `backups/<UTC time>-g<generation>/`. The blob sweep waits while it runs. |
| `orch db prune [--older-than <days>] [--dry-run]` | running | R79. |
| `orch db export --to <file>` | running | The file must not exist; created 0600. |
| `orch db restore <backup> [--yes]` | stopped | R76 to R78. The replaced state goes to `backups/pre-restore-<UTC time>/`. |
| `orch db vacuum` | stopped | Needs free space of the database's size above the floor. |

There are no scheduled backups in 1.1; the manual tells the owner to take one before every upgrade (AP9).

## Attention queue, disk floor, usage

- **Attention.** Kinds in 1.1: `disk_floor` (blocking, daemon-wide) and `dispatch_off_with_work` (warning, per
  project). `orch attention list|show|ack`; `orch status` prints the number of open items. The daemon opens
  and resolves items; the owner can only acknowledge, which records who and when and resolves nothing.
- **Disk floor** (R85). Free space is what the filesystem offers an unprivileged process. It is read every
  30 s and before a claim, an import, a backup, an export and a vacuum. "Operations that add work" are
  `task.create`, `goal.create`, attachment staging, `db.backup`, `db.export` and `orch import`.
- **Usage** (R86, R87). The agent CLI's version is probed once per adapter per daemon generation and cached.
  The model is the one the adapter was asked to use, replaced by the one the CLI reports when it reports one.
  1.0's per-agent total stays as `agents.tokens_used`.

## Maintenance jobs

One timer; each job has a period and a stored due time (R43). All run on lane 4 except lease expiry.

| Job | Period | Does |
|---|---|---|
| `leases.expire` | 10 s | Renews supervised runs, then expires what is left past its time (lane 1). |
| `intents.perform` | at start, then 30 s | Handles pending intents; deletes done ones after 7 days. |
| `attention.evaluate` | 30 s | Disk floor and dispatch-off-with-work. |
| `request_keys.expire` | 5 min | R47. |
| `context.expire`, `messages.expire` | as in 1.0 | 1.0's expiry steps, now jobs. |
| `wal.checkpoint` | 5 min, or when the WAL passes 64 MiB | Passive checkpoint. |
| `events.prune` | 24 h | R62. |
| `blobs.sweep` | 24 h | R60. |
| `attention.expire` | 24 h | R84. |
| `db.optimize` | 24 h | `PRAGMA optimize`. |

## orch-peer

What breaks, exactly (B15), at `fb6a1e1`:

| Place | What it does today | In 1.1 |
|---|---|---|
| `crates/orch-peer/src/worker.rs`, `list_tasks` (near line 805) | Reads `tasks/_index.json` to predict an index rebuild. | The read is removed; there is no index to rebuild. |
| `crates/orch-peer/src/lib.rs` (near lines 299 to 316) | Reads `messages/_index.json` and builds a `FileMessageStore` to reject a remote message. | Calls the message service through the port. |
| `crates/orch-peer/src/worker.rs` (near line 868) and its other calls on `engine.tasks`, `engine.agents`, `engine.messages`, `engine.state` | Use the 1.0 store traits. | Unchanged source: `orch-engine` keeps a facade with those shapes over port v2 for `orch-peer` alone, removed with it in 2.0. |
| `orch-peer`'s tests that write index and state files | Arrange 1.0 files by hand. | Rewritten to arrange through the facade. |
| `remotes.yml`, `remotes/`, `peer-id` | orch-peer's own files under `.orchestry/`. | Unchanged, and therefore not in the database, a backup or an export. |

The peer wire protocol does not change, so a 1.0 peer and a 1.1 daemon are expected to interoperate; R89's
recorded check proves it. The owner is asked to approve the first two rows (AP1).

## Non-goals

Everything in X. Also: scheduled backups; a paged dashboard (the TUI and `orch web` still fetch every page);
moving agent workspaces out of the project directory; a usage report command (`orch db query` serves until one
is asked for); a metrics endpoint other than `metrics.get`; deleting 1.0 files; any change to the global
configuration file or to organization templates, which stay files.

## Constraints

- macOS and Linux; the pinned toolchain; one project per daemon; a local disk.
- The frozen handshake, the stop intent, the no-daemon contract and exit 7 of ADR 0012 are unchanged.
- CLI and `--json` output shapes of 1.0 commands are unchanged apart from the added `revision` field (R48,
  R49). The peer protocol is unchanged.
- 1.1 is a breaking release for stored state only: one import per project.
- "64 KB" in B7 is read as 64 KiB (65,536 bytes) everywhere in this spec.

Security checklist:
- Default bind and exposure: the Unix socket in `run/`, 0600 in a 0700 directory, same-uid check as in 1.0.
  Nothing listens on a network. `orch db` and `orch import` have no remote form.
- Bounds on input size, count and rate: the frame and admission limits of protocol 2 stand. Added: 4 read
  connections; list limit 1,000; key 128 characters; cursor 512 bytes; kept result 64 KiB; `db.query` 100,000 rows and 5 s; import
  bounds in "Import rules"; `.orch-project` 64 bytes; `legacy.config` 64 KiB.
- Permissions of files it writes: the data directory 0700 and files 0600; backups and exports 0600;
  `.orch-project` 0644 (it holds an id, no secret).
- Secrets and environment passed to children: unchanged. `identity_tokens` holds hashes only. `db.query` can
  read every table, which is why it is owner-only and absent from MCP.
- Untrusted text rendered to a terminal or passed to a shell: `db.query` output, attention titles that quote
  agent text, the dry-run report and holder records are escaped as 1.0 escapes diagnostics. `db.query` takes
  SQL from the owner only and runs it on a connection that cannot write, attach or load an extension. Source
  files of an import are untrusted input: bounded, never executed, and paths in them are never followed.
- Same-uid limit, stated plainly: any process of the owner's user can open the socket or the database file.
  The matrix is not a defence against such a process in 1.1; it becomes one with authentication in 1.4.

## Verification environment

- Developer machine (macOS) and Linux CI: every test named above.
- Native Linux and macOS hardware, by the performance engineer: R5 and R63.
- A real suspend and resume on a macOS laptop: R6 and the recorded part of R42.
- The owner's machine and permission, on a copy outside the repository: R71.
- A second host for the peer check: R89.
- Not needed: a browser, a native terminal profile, tmux (no dashboard behaviour changes).

Asked for in the first approval request: permission to copy the live project for R71 and R5's load figure, and
the second host for R89.

## Open questions

Each goal open point with a recommendation, then the points this spec found. The owner approves each at the gate.

| # | Point | Recommendation | Main alternative |
|---|---|---|---|
| O1 | How a client finds the daemon and its data directory | Introduce `.orch-project` in 1.1 (R34), keep the 1.0 search order, name the data directory from the project directory when it is created and find it by project id afterwards (R33). `--project` and `ORCH_PROJECT` arrive in 1.5, when a project can exist without a directory. Reason: the import is the one moment every project is touched, and without the pointer a moved project directory looks unimported and invites a second import of stale files. | No pointer in 1.1: `.orchestry/` stays the marker and the data directory is found by the derived name alone. Less change now, the move hazard stays, and the pointer arrives in a later breaking step. |
| O2 | Which tables 1.1 creates | The 33 tables and six differences in "Schema for 1.1". | Create all 50 now: no later table migrations, but 17 untested empty tables and constraints nobody has exercised. |
| O3 | Import with the daemon stopped; how long 1.0 files are kept | Stopped, both daemons (R68). orch never deletes 1.0 files; the manual advises keeping them until release 1.2 is running and one backup has been restored in a test, and lists the 1.0 state files that may then be removed by hand. The directory `.orchestry/` itself, its `workspaces/` and the peer files stay in use and are never on that list. | Import into a running 1.1 daemon: no stop, but the import shares the writer with live work and its resume is harder to reason about. |
| O4 | Dispatch at start | As it was when the daemon stopped; off after import, restore and a migrating start; the flag overrides (R44). The `dispatch_off_with_work` item covers a forgotten "off". | Always off, as 1.0: simplest, and every restart needs a second command. |
| O5 | Maintenance loop | One timer with a due time per job (R43): one place for order, shutdown and the lane, and due times that survive a restart. | One timer per job: independent, but eleven tasks to stop and no shared order. |
| O6 | Resolved attention items; doctor | Keep 90 days. A blocking item makes `orch doctor` exit 1; a warning does not (R84). Doctor's exit for its existing checks does not change. | Doctor stays exit 0 and only prints: no script breaks, and a blocked daemon looks healthy to one. |
| O7 | Unknown config keys; `max_concurrent_agents` | As proposed (R70), with a hard stop over 64 KiB. | Refuse an import with any unknown key: strict, and blocks projects that carry extension keys. |
| O8 | Request-key expiry | Ships in 1.1 as `request_keys.expire` (R47). | None; the goal requires it. |
| O9 | Names | Approve the names in this spec as a set: operations in "Protocol 3 and errors", commands in "`orch db` commands" and "Import rules", settings `storage.synchronous`, `retention.events.success_days`, `retention.events.failed_days`, `attention.resolved_retention_days`, `run_cap`, `preapproval`, `legacy.config`; variables `ORCH_DATA_DIR`, `ORCH_DATA_HOME`, `ORCH_REQUEST_KEY`; metrics `scheduler_tick_seconds`, `scheduler_open_tasks`, `writer_wait_seconds`, `writer_txn_seconds`, `writer_txn_over_bound_total`, `writer_busy_timeouts_total`, `events_dropped_total`, `disk_free_bytes`, `disk_floor_bytes`. | Rename any of them at the gate; a rename after the protocol PR costs a protocol bump. |
| O10 | Fixtures without private data | Generate them (R90): a seeded generator drives the 1.0 file backend through scripted scenarios, so the format is real and the content invented; its sizes follow counts and percentiles taken from the live copy, which are numbers; a hand-written corpus covers damaged input. The live copy is used only for R71 and R5, outside the repository. | Scrub a real project: any scrubber can miss something, and the result cannot be regenerated. |
| AP1 | orch-peer cannot stay byte-for-byte unchanged (F7) | Approve the two source changes in "orch-peer"; the wire protocol and peer files stay. | Turn remote peers off in 1.1 until their removal in 2.0: no change to `orch-peer`, and remote agents stop working for four releases. |
| AP2 | The catalogue has 111 operations, the goal says 115 (F1) | Treat the catalogue's own class test as the authority; 1.1 adds 12 operations. | The owner names the four operations the goal counted, if they are meant to exist. |
| AP3 | Whether an agent's session becomes a task principal in 1.1 | No: it stays an owner session for the 1.0 catalogue, so no agent workflow changes in the storage release; the new owner-only operations are refused in agent context (R54). The task column becomes real with authentication. | Enforce the task column now: safer on paper, but same-uid makes it advisory, and it changes what agents can do today. |
| AP4 | CLI lists | Return all rows in the 1.0 shape by default; `--limit` and `--cursor` opt in to one page (R49). Scripts and agent prompts that parse `--json` keep working. | Page by default: honest about cost, breaks every consumer of the current output. |
| AP5 | `preapproval` for a project created by `orch init` in 1.1 | On, as 1.0's default policy is `auto`; `orch init` prints the trust statement. Revisit in 1.5, when the daemon sets the verdict. | Off: safer default, and every new project stops at `review` until the owner turns it on. |
| AP6 | What 1.1 writes under `.orchestry/` | Only `workspaces/` and orch-peer's files, as today; never a 1.0 state file. B2 binds the importer; this states the same for the daemon. | Move workspaces into the data directory: `.orchestry/` is then never written, but every agent CLI sees new paths and may ask for trust again. |
| AP7 | "Over the local socket only" (B8) for commands that need the daemon stopped | Read it as "owner, local, never remote": import, restore and vacuum run offline under the directory lock; the other `orch db` commands use the socket. | Run all of them through a daemon in a maintenance mode: one path, and a daemon that must swap its own database while serving. |
| AP8 | An atomic operation larger than 1,000 rows | It runs whole and is counted (R27); lane waits are promised only while that count is zero. Deliverable D reports the largest cascade on the live copy. | Cut cascades into steps: keeps the bound, exposes half-finished cascades to readers. |
| AP9 | No scheduled backups in 1.1 | Accept; a job is a small follow-up once restore has field use. | Add a daily `db.backup` job with rotation now. |
| AP10 | The 100-entry retry cap goes away (R40) | Accept: a table needs no cap, and 1.0's drop silently stranded a task. | Keep the cap for exact parity. |

## Gate lines

| Gate line | Proved by |
|---|---|
| G1 Conformance on both backends, with order and rotation | R2, R3, R56: `cargo test -p orch-infra --test storage_conformance` and `cargo test -p orch-engine --test order_conformance`, each run for SQLite and for the file reference; both platforms |
| G2 Importer on a copy of the live project | R71: `cargo xtask import-verify --source <copy> --record <dir>`; recorded check on the owner's machine |
| G3 Backup then restore as one restorable point | R75, R76: `db_backup_restore::backup_then_restore_round_trips_database_blobs_and_repos` (the repositories directory is empty and still compared) |
| G4 Export then import reproduces every row and hash | R80: `db_export_import::export_then_import_reproduces_every_row_and_hash` |
| G5 Worker and task principals are refused | R53: `ops_authorization::worker_is_refused_what_the_matrix_denies`, `::task_is_refused_what_the_matrix_denies` |
| G6 A second daemon refuses and names the holder | R30: `daemon_data_dir::second_daemon_refuses_and_names_the_holder` |
| G7 Throughput meets the budget, or the fallback ships | R5, R63: the `cargo xtask storage-bench` record on macOS and native Linux |
| G8 A repeated change runs once; a changed body is refused | R45, R46: `ops_request_keys::a_repeated_change_runs_once`, `::same_key_with_a_different_body_is_refused` |
| G9 Restore starts a new generation, reuses no epoch, lists and confirms | R77, R78: `restore_fencing::no_epoch_is_reused_after_restore`, `::restore_lists_reenabled_identities_and_tokens`, `::restore_without_confirmation_changes_nothing` |

## Verification

Filled in by the PR that completes the work.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |

Defects found outside the requirements:
- ...
