# 0017. SQLite is the only storage backend from release 1.1

- Status: proposed
- Date: 2026-11-20
- Spec: [SQLite storage for release 1.1](../specs/2026-11-20-sqlite-storage.md);
  plan: [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md)
- Would supersede, on acceptance: the clauses of [0012](0012-daemon-owned-state.md) decision 1 and decision 9 that
  place project state, `daemon.json` and the default socket under `<project>/.orchestry/`; the clause of 0012
  decision 8 and its consequence that the operation boundary is "not a transaction upgrade"; and the storage
  section of `docs/TRAITS-FROZEN.md` (§2: the ten storage traits and their three rules). Everything else in 0012
  stands: one daemon owns state, no client opens it, nothing starts a daemon on its own, exit 7.
- Decisions 1 to 6 and 14 restate decisions the owner already took in the goal and are binding. Decisions 7 to 13
  and 15 to 21 are the design and stay proposed until the owner's approval gate. The numbers in decision 9 are
  the owner's.
- The owner's answers so far (2026-11-20): decision 16 follows the answer to the spec's AP11, decision 17 the
  answer to O1 and decision 18 the answer to AP1. Every other point of the spec's "Open questions" is pending,
  and this ADR stays proposed until the owner has answered them. Decisions 22 to 25 answer the security review
  and the red team; the owner has not seen them.

## Context

orch 1.0 keeps a project's state as files under `.orchestry/`: one YAML or JSON document per entity, a
per-store `_index.json`, one `state.json`, and per-run `runs/<id>.json` and `runs/<id>.jsonl`. Facts read from
the code at `fb6a1e1` (v1.0.0):

- Every store offers get, save, list and delete on one entity (`crates/orch-core/src/storage.rs`). Nothing spans
  two entities, nothing checks a version, and every list returns everything.
- `save` removes the entity from `_index.json` and appends it, so index order is last-write order. That order is
  behaviour: `AgentService::find_best_in` sorts by score with a stable sort and takes the first agent, so ties go
  to the agent written longest ago. This is why ties between agents rotate.
- `state.json` is one document holding the running set, the claimed set, a retry queue capped at 100 entries
  that drops the oldest (`MAX_RETRY_QUEUE_SIZE`), and totals.
- A run event has no sequence number; a run records its tokens but neither the model nor the agent CLI version.
- ADR 0012 gave the daemon one operation boundary and said plainly that it is not a transaction: a crash between
  two file writes leaves the stores disagreeing.
- On the owner's live project (2026-11-19) run events take about 700 MB for 2,538 runs and task documents 68 MB
  for 2,979 tasks.

orch 2.0 needs things files cannot give without rebuilding a database by hand: a claim that two dispatchers
cannot both win, a lease with an epoch that fences a superseded run, multi-entity changes that are all-or-nothing
across a crash, lists that do not load everything, and a retried request that runs once. ADR 0012 rejected
"SQLite now" only because it would have combined two migrations; that reason ended when 1.0 shipped.

## Decision

1. **SQLite is the only backend from release 1.1.** The file backend leaves the product in the last PR of the
   stack. It stays in the test tree for one release as the conformance reference: a crate that only
   dev-dependencies may name, checked by `cargo xtask crate-graph`.

2. **Import is one-way.** `orch import` reads `<project>/.orchestry/` and never writes to it: it opens every
   source file read-only and creates no file there. The way back to 1.0 is that untouched directory with the
   1.0 binary. There is no export to the 1.0 format and no dual-write period.

3. **The engine reaches state only through storage port v2.** The port has units of work (one transaction),
   a revision on every entity that is checked on save, cursor-paged filtered lists, an explicit ordering column,
   claim operations, a batched event append, and a caller (principal and project) on every call. It no longer
   has the whole-document state store, the YAML merge-key rule, or "save moves the entity to the end of the
   index". The spec's "Storage port v2" section is the contract; `docs/TRAITS-FROZEN.md` §2 is rewritten from
   it in the PR that lands the port, and this ADR is the amendment that file requires.

4. **The schema is multi-project and 1.1 runs one project per daemon.** Every table except the daemon-wide ones
   has `project_id` in its primary key. Tables are `STRICT`. Times are UTC milliseconds. Ids keep their 1.0
   form.

5. **One data directory per daemon, mode 0700, held by an exclusive lock.** A second daemon pointed at it, and
   any offline command, refuses and names the holder. The directory is on a local disk; a network filesystem is
   refused at start.

6. **One writer, many readers.** One task performs every write, one transaction at a time, each opened with
   `BEGIN IMMEDIATE`. A pool of read-only connections serves reads, each read from one consistent snapshot.
   Connection settings: WAL journal, `synchronous = NORMAL` (`FULL` is a setting), foreign keys on, a busy
   timeout of 5 s. The busy timeout is a safety net: with one writer in one process it should never be reached,
   and reaching it is counted and reported.

7. **Migrations are numbered, forward-only and run at daemon start.** `schema_migrations` records each one.
   A database whose version is newer than the binary refuses to open and names both versions. There is no down
   migration. Before each pending migration the daemon copies the database, checks the copy and keeps the
   newest two (the spec's R124); the way back from a failed or a wrong migration is that copy. It is not a
   scheduled backup, which 1.1 does not have.

8. **1.1 creates 33 of the sketch's 50 tables.** A whole table that only a later phase uses arrives by a
   migration in that phase (17 tables and the FTS5 index). A column whose later addition could not be filled in
   truthfully, or would force a table rebuild, is present from 1.1: `runs.model` and `runs.adapter_version`,
   the landing columns and their `CHECK`s on `tasks`, the published and integration columns on `goals`,
   `task_epochs`, `position`, and `project_id` everywhere. The spec lists the tables and every difference from
   the sketch.

9. **The writer serves four lanes: claims first, the rest by deadline.** The lanes and the budgets for their waits are
   claims and lease work 50 ms, entity changes 200 ms, events 1 s, pruning and other maintenance 5 s. A queued
   claim always goes first. Among the other lanes the writer takes the earliest deadline (arrival plus the
   lane's wait), so none of them starves, and a bulk job queues one transaction at a time, so overdue bulk work
   cannot pile up ahead of anything. A claim therefore waits behind at most the one transaction in progress.
   That is an order and not a time: a running statement and a commit are not cut short, so whether a wait is
   met is a measured result (decision 12) and never a consequence of the lanes. The waiting event batches of
   all runs share one transaction, so the event lane puts at most one transaction ahead of an entity change.
   Bulk work (event batches, pruning, expiry, import) is cut into transactions of at most 1,000 rows or 20 ms.
   An operation that must be atomic is never cut; if it exceeds the bound it runs whole and is counted.

10. **Each multi-entity operation is one transaction, and its side effects are intent rows.** Starting a run,
    finishing a run, cancelling a task, pausing a goal, and a permanent failure with its cascade each commit
    once or not at all. Anything outside the database (start or stop a process, remove a workspace, delete a
    blob file) is written as a row in `intents` in the same transaction and performed after the commit. A
    stopped daemon finds unfinished intents at the next start. Every intent kind states whether it is repeated
    (idempotent ones) or resolved by recovery. Starting a process is never repeated: the process is created
    held, its pid is committed, and only then is it released, so startup recovery always knows what exists and
    treats it as 1.0's startup recovery does.

11. **Bytes are written before the row that names them.** A blob is written to `blobs/tmp/`, synced, renamed to
    `blobs/sha256/ab/cd/<hash>`, and only then referenced in a transaction. A crash leaves at worst an
    unreferenced file, which the sweep removes. Text up to 64 KiB may live in a column; larger or non-text
    bytes go behind the blob-store port, whose only adapter in 1.1 is files on disk.

12. **Run events go into SQLite, in batches, unless the measurement says otherwise.** A collector numbers its
    run's events from 1 and hands batches to the events lane; the append ignores a sequence number already
    stored, so a repeated batch is harmless. If the measured throughput cannot meet the claim-latency budget
    (deliverable D), events are kept as per-run files in the data directory, each named by a hash (decision 20),
    behind the same port operations and nothing else in this ADR changes. The measurement has since missed the
    budget on macOS and met it in a Linux container on the same machine; no figure from native Linux exists
    yet. Its report does not establish the cause, so the spec's point AP13 asks the owner to let a declared
    diagnosis run first (automatic checkpointing off, one transaction for all waiting batches) and keeps the
    fallback as the ruling if that run misses. The path that ships is measured against the same budget before
    its PR merges.

13. **A retried change runs once.** A request may carry a key. The key, scoped by (identity, project, run,
    epoch, key), is stored with a hash of the request and the result in the same transaction as the change, and
    kept for one hour. The same key with the same request returns the first result; with a different request
    it is refused. A result over 64 KiB is not kept: the replay says the change was made and returns no body.

14. **The scheduler only decides.** It sits behind an interface that maps open tasks to agents. The engine's
    claim transaction is what prevents a double dispatch, so a scheduler that is wrong or raced costs a wasted
    claim and never a second run. 1.1 ships one scheduler: today's in-memory logic with the same rules.

15. **Order is explicit.** `position` is a per-project integer that only grows. Creating an entity gives it the
    next value; an update leaves it alone; the port operation `move_to_end` gives it the next value. The engine
    calls `move_to_end` at exactly the call sites where 1.0's `save` reordering is observable (the list is
    deliverable B). Agent rotation is therefore the same as 1.0 and is now a line of engine code that a test
    names.

16. **A restore starts a new generation and a new lineage; no pair of lineage and epoch is issued twice.** The
    owner decided this on 2026-11-20 (the spec's AP11) and amended gate line 9 to match. The lineage is a
    random 128-bit id made with the database, replaced by every restore, kept by every other start, stored in
    `daemon_config` and held by every lease beside its epoch. Every lease-bound call carries both and is
    refused unless both equal the lease's (R77, R132). A task's next epoch comes from `task_epochs` alone. The
    generation is a counter of starts, one more than the highest that the database and the file `generation`
    record; it orders the starts of one lineage and promises nothing about numbers. An epoch's number may
    repeat under another lineage, and nothing compares epochs across lineages. The first draft of this
    decision took a floor for epochs from the clock (`generation * 2^20`, with the generation at least the
    UTC second) and concluded that a number was never issued twice. That does not hold: a backup restored
    onto a directory with no earlier state, while the clock is behind an earlier start, issues the same
    number again. The floor is withdrawn. The lease prototype (deliverable E) is still to confirm the
    mechanics. A restore also refuses while a recorded process lives (R100).

17. **The project is named by a pointer file from 1.1.** `orch init` and `orch import` write `.orch-project`
    in the project directory, holding the project id and nothing else. The daemon's data directory is created as
    `<data home>/<name derived from the project directory>/` and recorded under the project id in
    `<data home>/projects/`, so a moved project directory still finds its state; the data home is
    `~/.orchestry/daemons` unless `ORCH_DATA_HOME` says otherwise, and `ORCH_DATA_DIR` names one directory
    outright. `daemon.json`
    and the socket move into the data directory's `run/`. `--project` and `ORCH_PROJECT`, which name a project
    without a directory, arrive when one daemon serves many projects (1.5). The frozen handshake envelope of
    0012 decision 6 does not change. This is open point 1 of the goal; the owner decides it at the gate.

18. **orch-peer keeps its wire protocol and its own files, and loses its direct reads of the 1.0 stores.**
    Decision 15 of the goal says orch-peer is not changed, or the spec says exactly what breaks. `orch-peer`
    runs inside the daemon on the engine. It reads `tasks/_index.json` and `messages/_index.json` itself, builds
    a `FileMessageStore`, and reads `state.json` through the state store; none of these survives decision 1.
    The spec names each place and asks the owner to approve adapting those places and nothing else. Its other
    storage calls keep compiling against a facade with the 1.0 trait shapes, kept in `orch-engine` for
    `orch-peer` alone until 2.0. The peer protocol, the `remotes` files and `peer-id` stay as they are.

19. **The SQLite library is `rusqlite`, and `rusqlite_migration` runs the migrations.** `rusqlite` 0.41.1
    with the features `bundled`, `backup`, `cache`, `limits` and `hooks` and its default features off, and
    `rusqlite_migration` 2.7.0, both pinned exactly and used from `orch-infra` only. `limits` and `hooks`
    were added after the report, for decision 21: they are the library's safe calls for run-time limits, the
    authorizer and the progress handler. Both are parts of the crate and bring no other crate (in 0.41.1's
    manifest `limits = []`, `hooks = ["wal"]`, `wal = []`), so the dependency graph the report checked is
    unchanged. `load_extension` stays off. The report of deliverable C
    (`docs/sqlite-library.md`) holds the §4.8 check and its evidence: both crates maintained; a declared MSRV
    of 1.89 under the pinned toolchain; MIT and Apache-2.0, inside `deny.toml`'s allow-list; `cargo deny check`
    passing on the whole workspace with the two crates connected and no new exception; the bundled engine
    SQLite 3.54.1 running STRICT tables and FTS5 on macOS and Linux; and a safe online-backup API. 0.41 is not
    `rusqlite`'s newest line: the maintained migration runners stop at it, and a maintained runner is
    preferred to a hand-written one (ARCHITECTURE §4.8). Each migration is applied in its own transaction,
    which also writes its `schema_migrations` row. The versions and the advisories are checked again in the
    PR that adds the dependency.

20. **No value from outside the daemon names a file.** An id, a key, a name or a path that came from a request,
    a 1.0 source, an export file, a backup's manifest or a row of an imported or restored database is data in a
    column. Below the data directory, in a backup and in the data home's `projects/`, a path component is a
    fixed name of the layout, 64 hex digits of a SHA-256 the daemon computed (or its first two, as a shard
    directory), or a name the daemon generated; an export is one file and holds no path at all. A run's event
    file (decision 12) is named by the SHA-256 over a fixed label, the project id and the run id, each id
    preceded by its length; a project's record by the SHA-256 of its id. One module in `orch-infra` reaches
    every such file, and writes the pointer file in the project directory: it opens the root once and then goes
    through directory descriptors, one name at a time, refusing links and special files, judging an entry by the
    descriptor it opened, and creating exclusively. It refuses a file with more than one hard link in a tree
    from elsewhere (an import source, a backup being restored) and before any append; reads below the data
    directory rely on the hash check instead, so that a hard-link snapshot of the directory does not stop the
    daemon. The importer reads its source the same way, by listing, and opens nothing that a document names. A
    pid, a workspace path or a project directory read from a 1.0 source or an export is never acted on: an
    import writes no lease and no intent and signals nothing. A restore does not act on a backup's rows either: it refuses while a
    process that the backup records is alive, and stops, when asked, only the processes that the replaced
    state of this directory records (R100; AP14 puts that to the owner). The spec's "Filesystem names" is the contract and holds the test vectors. The one id
    that still names a directory is `workspaces/<task id>` in the project directory, 1.0's layout, with 1.0's
    reject-never-rewrite check; the spec's point AP14 puts it to the owner. A check of the id shows that it is
    one name, not that the directory is one task's: the review's second reading found that `tsk_Case` and
    `tsk_case` are two rows and, on a filesystem that folds case, one workspace and one branch. So the kept
    layout comes with an ownership rule. No two tasks of a project have ids that differ only in the case of
    ASCII letters: one rule on every platform, whatever the filesystem tells apart, kept by a unique index
    under SQLite's `NOCASE` collation, with the exact id still the key and no id ever lower-cased or
    rewritten. An import, from a 1.0 directory or from an export, and a restore refuse such a pair before
    they publish anything, together with a directory in `workspaces/` that the tasks they bring may not own.
    And before every reuse, merge, delete and cleanup a listing must hold the directory, or the branch,
    under exactly the task's name; a new workspace is an exclusive create. A filesystem that folds a name
    keeps the spelling it was created under, so what is reached through another spelling is seen as another
    name's and is never adopted or removed. This decision answers the security
    review's finding S2, which showed the first layout (`blobs/events/ab/<run id>`) leaving the data directory
    for a run id shaped like a path. The descriptor calls exist, without `unsafe`, in `nix`, which the workspace
    already depends on and already uses this way (`crates/orch-daemon/src/ops/transport.rs`); `orch-infra` needs
    its `fs` and `dir` features turned on, which adds no crate. The PR that adds the module does the §4.8 check
    on a capability-style crate such as `cap-std` before it settles on `nix`, and the contract is the same
    either way.

21. **Input from outside is measured before anything is built from it.** A 1.0 source file and an export
    file are content from elsewhere, whatever directory the owner named. Every limit on them is checked on
    the bytes or on a parser's events, by code whose memory and stack do not depend on what the input says
    about itself, and no limit is a deadline. For a 1.0 document the limits are bytes, nesting depth, node
    count, aliases, and the nodes and bytes of the document as expanded, with one budget for every
    expansion whether an alias stands as a value or under a merge key; an explicit YAML tag is refused. The
    numbers are those of 1.0's own guarded reader where it has them (1 MiB, 64 aliases, 200,000 expanded
    nodes), so that a document 1.0 wrote always passes and aliases and merge keys are still resolved as 1.0
    resolved them; three rules are new and stricter (tags, depth 64, expanded bytes 4 MiB) and the spec's
    point AP15 puts them to the owner. An import checks its whole source, as its dry run does, before it
    writes anything. An export is a SQLite file and is read as a hostile one, with the settings SQLite
    documents for that case: its header is checked before SQLite opens it; the connection is its own,
    read-only and immutable, in defensive mode, with the schema untrusted, triggers, views and extension
    loading off, lowered run-time limits, a heap limit, an authorizer that allows only a fixed list of
    statements and a progress handler; `quick_check` runs first; the file's schema is compared with the one
    this binary writes and never adopted; rows are copied by fixed statements into a database made by the
    binary's own migrations, so the destination's constraints judge every row; blob bytes arrive in chunks of
    at most 1 MiB. That database is built under a scratch name and renamed into place only when everything
    has passed, so an import from an export is all or nothing. The spec's "Import bounds" is the contract and
    holds the numbers and the test vectors. This decision answers the security review's finding S1, which
    showed a 40,001-byte document of depth 20,000 and a 412-byte document standing for 100,000,000 scalars
    inside the first draft's byte bound.

22. **A cutover refuses while 1.0 work may still run, and a 1.0 run after it is detected.** 1.0's own recovery
    keeps a local entry whose pid is alive and a remote entry whose peer is registered, and a 1.0 stop leaves
    both behind. Closing them would send their tasks out a second time. So the import refuses while a local
    process of a run exists or any remote run is recorded, and names it (R119); it does not trust the peer
    registry, since removing a peer there stops nothing on the peer. It asks only whether the pid exists and never signals: a 1.0 entry has no
    start time, so the pid may be another program's. The owner ends the work with the 1.0 daemon or names the
    run as ended. A run the import closed is never attached again (R120). The import keeps the size and hash
    of `state.json` and of each index, and a 1.1 start refuses when they have changed, because a 1.0 daemon
    ran meanwhile (R121). Where an index and an entity's file differ the file wins, since 1.0 writes it first
    (R122). The importer still writes nothing under `.orchestry/`.

23. **The writer never checkpoints, fails by class, and a finish is on disk before it is queued.** Automatic
    checkpointing is off on the writer's connection, and a second connection runs passive checkpoints, so no
    commit carries one (R126); a long pooled read is interrupted once the log is large (R127). A failure from
    outside a transaction's logic is retried with nothing dropped, or fails the writer, which then refuses
    mutations and reports itself without the database (R128). A run's finish is written to a spool file and
    synced before it waits for the writer, and recovery applies it before it judges the run, so a restart
    behind a locked or failing database does not turn a finished run into a cancelled one (R129).

24. **`orch db query` reads fixed views under an authorizer; a backup stays in the data directory.** The
    command has its own read-only connection, an authorizer that denies by default, and run-time limits; it
    reads views that show no identity, token, stored request result or daemon setting (R74, R130). These are
    the `hooks` and `limits` features that decision 19 already names. `orch db backup` writes only into a
    directory it creates below `backups/`, mode 0700 with files 0600 (R75). Before a migration the daemon
    makes its own copy (decision 7).

25. **Trust is turned on by named paths only, and an attention item holds no outside text.** `preapproval`
    is turned on by `policy.set`, `orch init`, the import of a 1.0 project whose agents use `auto`, and an
    import or a restore whose data has it on; every import and restore that turns it on names it in what the
    owner confirms, and none works in agent context, where `orch init` leaves it off (R133, following the owner's answer to AP5). An attention item's words
    are fixed for its kind (R131). A request key never applies to a credential operation, and authorization
    comes before a stored result is looked up (R134).

## Alternatives considered

- **Keep files and add indexes, a journal and file locks.** Each 2.0 need (atomic claim, fencing, paging,
  idempotent requests) would be a hand-built piece of a database, against ARCHITECTURE §4.8.
- **Both backends for a release, kept in step.** Two writers of truth, a sync that can diverge, and double the
  test matrix, to buy a rollback that the untouched `.orchestry/` already gives.
- **A reversible import (export back to 1.0 files).** 1.1 records facts 1.0 has no place for (revisions,
  epochs, usage by model); a round trip would be lossy and would have to be tested as a product feature.
- **An embedded key-value store (redb, sled, LMDB).** No SQL for `orch db query`, no FTS5 for 1.2, and
  secondary indexes and filtered paging written by hand.
- **SQLx as the library.** Its asynchronous pool does not replace the priority writer orch needs in any
  case, and the checked driver has no safe wrapper for an online backup: that would take the raw handle and
  `unsafe`, which the workspace forbids.
- **Postgres now.** A second process to install, start and back up on a single-machine install that must work
  from one start command. It is listed after 2.0.
- **Several writer connections relying on the busy timeout.** Writers would wait in an order SQLite chooses,
  so a claim could wait behind a 5-second prune. One writer makes the order ours and makes lane budgets
  testable with a fake clock.
- **One database per project.** A claim and its lease would still be one database, but daemon-wide tables
  (identities, attention, request keys) would need a second file and cross-file transactions in 1.5.
- **Run events kept as JSONL for good.** No transaction with the run row, a second pruning and backup path,
  and no tail read by index. Kept only as the measured fallback of decision 12.
- **An in-place conversion of `.orchestry/`.** It destroys the only rollback.
- **A reorder on every save, kept inside the port.** It is the 1.0 rule the owner removed: it makes every write
  an ordering side effect that no caller asked for and no index can serve.
- **Epochs as a plain counter, raised by a constant at restore.** It guesses how many epochs were issued after
  the backup; decision 16 does not need to know.
- **Keep ids in file names and validate them.** A check on every path builder holds only until one caller
  forgets it; 1.0's wire and domain id types check a length and nothing else. A name the daemon computes
  cannot be shaped by its input, whatever a later caller passes.
- **Hash only a directory prefix of the id.** The first layout: the basename was still the id, and it escaped.
- **A generated event-file name stored in a column.** As safe, but a column, a lookup before every append and a
  second thing for backup and export to keep in step; the derived hash needs none.
- **Resolve and compare paths (`canonicalize`, then a prefix test).** It races with a link planted between the
  check and the use, and it follows the link it was meant to refuse. Descriptor-relative calls do neither.
- **Name workspaces by a hash, with a mapping for the ones that exist.** No id would be a path component, and
  no project would be refused for two ids that differ only in case. But every agent would see a new path,
  the branch `orchestry/<task id>/...` would keep the same alias unless it changed too, and each mapped old
  name would still need the ownership rule that decision 20 now has. The rule alone is the smaller change;
  the spec's point AP14 keeps this alternative before the owner.
- **Refuse a pair of ids only where the filesystem folds case.** A project with such a pair would import on
  Linux, and its database, backup or export would then be refused on macOS: whether stored state is valid
  would depend on the disk under it, and a moved project directory would meet the alias later.
- **Lower-case every task id.** It rewrites ids that messages, branches, results and agents' own notes
  already hold, and two ids would still become one.
- **Record each workspace's device and inode as its owner.** It binds a task to one object, and breaks when
  the project directory is copied, restored from a file backup or moved to another disk, which changes every
  inode and no name. The listing answers the question that matters, under which name the object was made,
  and survives all three.
- **Rely on the parser libraries' own limits.** `serde_yaml_ng` stops at 128 nested collections, `serde_json`
  at 128, and both refuse the review's depth document today. Those are properties of two versions of two
  crates, not rules of this project, and neither counts what a document expands to.
- **Refuse every alias and merge key.** The simplest rule, and 1.0 resolves merge keys: a `config.yml` that
  a person wrote with `<<` would stop its project's import. The expansion is bounded instead.
- **A time limit on the parse.** 1.0 had one and removed it: it could not stop the work it timed, so a slow
  failure became one that never ended.
- **Run the parsers in a child process under operating-system limits.** A second defence worth having some
  day, and not a portable one: the review could not set a data-size limit on macOS. The budgets hold by
  construction on both platforms.
- **An export that is not a SQLite file (rows as JSON lines in an archive).** No SQLite on hostile bytes, at
  the price of a second serialisation of every table that must keep integers, nulls and blobs exact for gate
  line 4. SQLite documents how to read a file from elsewhere, and decision 21 follows it.
- **Trust an export because its manifest's hashes agree.** The file's author wrote the manifest too. It finds
  damage; it is not a reason to skip a check.
- **Attach the export to the writer and copy with `INSERT ... SELECT`.** Fast and short, and it puts a
  hostile schema on the connection that owns the destination.
- **Load an export into `orch.db` directly, with a resume.** A refusal at the last row would leave a database
  that is neither a project nor nothing. A scratch name and one rename need no resume.
- **Close in-flight 1.0 state without looking at any process.** The first answer to the security review: no
  value of a source then chooses a process to examine. A run still at work on this machine or on a peer is
  then dispatched again, in the same workspace or with a second merge. Refusing costs a blocked import when
  a pid was reused or a peer is gone for good, which the owner clears by naming the run.
- **Stop surviving 1.0 processes from the import.** A 1.0 entry holds a pid and no start time, and the file
  that holds it is input. Nothing shows the process to be the run's.
- **An epoch floor taken from the clock.** The first draft of decision 16. It needs no new field, and its
  promise about numbers fails on a fresh directory with the clock set back. The owner chose the lineage.
- **A release between the engine's move and the switch, or SQLite chosen for each project in 1.1.** The red
  team's "path not taken". Neither is the two-backends-in-step design rejected above: each project would
  have one writer of truth. The first separates an engine regression from an import defect at the cost of a
  release; the second gives field use of the importer and two tiers of guarantees, and changes a binding
  decision of the goal. This ADR keeps one release; the spec's AP20 puts both to the owner.
- **Automatic checkpointing left on.** SQLite's default, and what the first measurement ran with. The
  checkpoint then runs inside whichever commit crosses the threshold, a claim's included.
- **Results kept in memory until they commit.** No file, and a daemon restarted behind a locked database
  loses the result and cancels the run.
- **`orch db query` on every table, guarded by reading the SQL.** A filter on text is a parser to keep
  correct; the authorizer sees what the statement does.
- **Skip or set aside a source file the import cannot read.** The import always finishes, and a task can be
  missing from 1.1 unnoticed. The spec's AP17 puts it to the owner.

## Consequences

- A multi-entity operation is atomic across a crash, which 1.0 never was. The crash-point tests become possible
  and are required.
- 1.1 is a breaking release: every project is imported once. The upgrade notes, the dry run and the resume
  carry that cost.
- State is no longer greppable files. `orch db query` (read-only SQL) and `orch db export` replace reading
  the directory, and the RUNBOOK's file procedures are rewritten.
- A load-bearing dependency arrives: the SQLite library, chosen by deliverable C with the §4.8 check. It must
  bundle SQLite 3.37 or later (STRICT tables; `RETURNING` and `VACUUM INTO` are older) with FTS5 available for 1.2, and must
  let the workspace keep `unsafe_code` forbidden. Decision 19 names it.
- All writes share one thread of work. The budgets of decision 9 are promises that must be measured
  (deliverable D) and watched (`metrics.get`); the fallback of decision 12 exists because the promise might not
  hold for events.
- `docs/TRAITS-FROZEN.md` §2, ARCHITECTURE §1, §2.1, §5.1 to §5.3 and §6.1, and the RUNBOOK's storage
  procedures change. The stable-sort rule stays: the scheduler still sorts in memory.
- Backups become a product duty: database, blobs and repositories as one restorable point.
- `orch-peer` is touched at the places decision 18 names, if the owner approves it, and nowhere else.
- Files in the data directory can no longer be found by an id with `ls`: an event file and a project record
  are named by hashes, which the storage reference (`docs/storage.md`) must explain how to compute. A copy
  of a 1.0 project or of a backup made with hard links (some snapshot tools) is refused by the importer and
  by the restore and must be copied again as plain files; a backup never shares files with the live blob
  store by hard links. A hard-link snapshot of the data directory itself does not stop reads, but the daemon
  refuses to append to an event file that has a second link, since the append would change the snapshot.
- A 1.0 document that a person edited to hold an explicit tag, to nest deeper than 64 or to expand past 4 MiB
  was readable by 1.0 and stops the import, by name and before anything is written; the owner changes the
  document or keeps the project on 1.0. An import reads its source twice. An import from an export that is
  interrupted starts again. An export cannot be read by a build of another schema version, and reading one
  rests on SQLite's own handling of a hostile file under the settings of decision 21, which this project
  configures and does not prove.
- Two task ids that differ only in the case of ASCII letters cannot live in one project, on any platform. 1.0's
  generated ids make that about 7 in 100,000 for a project of 3,000 tasks, and only a project kept on a
  filesystem that tells case apart can hold such a pair; its import is refused, naming both, until the owner
  removes one task with the 1.0 binary. An export is not imported for a project directory that already holds
  a workspace under one of its tasks' names. Preparing and cleaning up a workspace list `workspaces/` once
  each, which is a few thousand names at most.
- The data directory lives outside the project, so moving a project directory no longer moves its state. The
  pointer file and the record under the data home are what let the daemon find it again instead of offering a
  second import. `<project>/.orchestry/` must stay in place: it is still the handshake's project identity.
