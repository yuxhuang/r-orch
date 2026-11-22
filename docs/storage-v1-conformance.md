# Storage 1.0 reference contract

Entry B of the SQLite-storage goal freezes observable **current** behaviour before
the storage port changes. It changes no product code and adds no dependency.
Run `cargo test -p orch-engine --test storage_conformance`; the ordinary workspace
suite, including the `pr` tier, discovers this integration binary automatically.

`crates/orch-engine/tests/storage_contract/mod.rs` contains the reusable scenarios.
They accept a `Stores` bundle of today's trait objects and a controllable clock.
`storage_conformance.rs` registers a fresh file backend for every scenario with
`storage_suite!`. A later backend registers another factory returning that bundle;
the scenarios and their expected results stay unchanged. A v2-to-reference adapter
will have to collect pages and translate the removed document operations. This
suite does not require v2 to retain the old method signatures or file mechanics.

`storage_contract/file.rs` alone constructs file stores and copies corpus inputs.
Every copy is temporary; the suite never opens a daemon or changes environment
variables. Global preferences use an explicit temporary `global.yml` path.
No live project or home directory is an input.

## What order means

| Store | Create / update / delete observations |
|---|---|
| Tasks | Status rank (in progress, retrying, review, todo, done, failed, cancelled), then updated time descending; exact ties preserve last-write index order. Save replaces the payload and moves the record to the end among ties. Delete removes it and is idempotent. |
| Goals | Active, paused, achieved, abandoned; updated time descending with missing time last; ties preserve last-write index order. Same replacement/deletion semantics. |
| Agents | Unsorted last-write index order; name lookup reads that order. Local capabilities are supplied at read time. Same replacement/deletion semantics. |
| Messages | Creation time ascending, then last-write index order for ties. Delivery also saves and rotates ties. Pending lists exclude other recipients, broadcasts, delivered and expired records. Delete is idempotent. |
| Context | Key ascending, including `get_all` map iteration. Updates preserve creation time and refresh update time; delete is idempotent. Expiry is strict (`expires_at < now`), and reads do not delete expired records. |
| Teams | No index and **no defined enumeration order**. Capture each list and check complete membership at all lifecycle stages. Do not bless a particular filesystem's enumeration order. |
| Runs | Started time descending; update replaces metadata without last-write rotation. Equal-start-time ties inherit unspecified filesystem enumeration, so the portable suite uses distinct times. There is no delete method. |
| Run events | Append order, including duplicates and timestamps out of order; tail keeps that order. Stream is a finite read to EOF, not live follow. There is no update/delete method. File readers have existing oversize/line bounds; this suite uses small valid records and does not redefine those bounds. |
| Config / global preferences | Single documents, no list/delete operation. Check read defaults, replacement, extension preservation through dot-path writes, and independence of project/global documents. |
| Running / claimed / retries / stats | StateStore replaces the whole document. Running and claimed preserve insertion order (updating a value does not move its key), retries preserve vector order, all stats survive. Removal is a document rewrite. The retry cap of 100 belongs to engine enqueue logic, not storage. |
| Project prompt templates | Plain files, not a storage trait in 1.0. The corpus pins `templates/default.md` bytes. Template strings inside config are part of ConfigStore. No template list/save/delete port exists to run a backend-neutral lifecycle against. |

The trait suite includes 103 records per collection to expose accidental adoption
of v2's proposed default page size of 100. It also records six equal-score
selections using the real `AgentService::find_best_agent` (and its `find_best_in` scorer): B, A, C, B, A, C after
run lifecycle saves. No process or adapter is executed. This isolates storage and
selection; existing orchestrator tests cover lifecycle orchestration itself.

`docs/storage-order-dependent-reads.md` inventories the consumers that must choose
explicit ordering, query/filter operations or complete pagination during the port.
The goal removes save-to-tail as a **port primitive**; it still requires preserving
scheduler rotation. An adapter must not accidentally make the scheduler alphabetical.

## Shared synthetic corpus

`fixtures/storage-v1/<case>/.orchestry/` is input shared with future importer tests:

| Case | Deliberate contents |
|---|---|
| `empty` | Only a marker file so Git retains the empty `.orchestry` directory; missing documents use defaults. |
| `small` | Two tied tasks in B/A index order, one agent, partial config, default state. |
| `every-kind` | Task, goal, agent, team, run, two events, message, context, config, state, prompt template and synthetic attachment. References are storage inputs, not a claim that all business validation has run. |
| `in-flight` | Two running task/run/agent records, an extra claimed task, two pending retries and nonzero stats. |
| `index-disagrees` | A stale title in the index, an index-only ghost and a file-only orphan. List trusts the valid index; get trusts the entity file. This is intentionally not repaired by a read. |
| `merge-config` | YAML anchor/merge in scheduling plus unknown scalar/object/array extension keys. Set preserves merged unknown values. |

All values were authored synthetically for these tests. IDs, titles, timestamps,
output, messages and preferences are invented. JSON-formatted `.yml` files are
valid YAML and keep the corpus straightforward to inspect; `merge-config` uses
YAML syntax deliberately. There are no credentials, executable agent commands,
real machine locations, sockets, logs or captured external data. The test checks
case names, rejects symlinks and scans for common private-location markers; this
scan supplements review and is not a general secret detector.

Importer tests must copy the corpus before opening a file store: even a list can
rebuild and publish an absent index. Keep the disagreement fixture unchanged and
test the importer's reconciliation policy separately. Do not infer that importing
the index alone preserves all entity files.

## Boundaries

This is an entry-step conformance reference, not SQLite implementation or importer
acceptance. It does not add v2 transactions, revisions, authorization, durability,
crash consistency or concurrent claims. Existing file-backend tests retain their
lock, corruption and process-race coverage. Organization template bundles live
outside the project storage port (`storage::org_templates`); they are not a new
database entity in this suite. A future port must settle any new template storage
interface explicitly rather than silently claiming coverage through TemplateEngine,
which only renders strings.

## Negative controls

During development, 16 deliberate faults were applied separately and restored.
Every one of the 24 scenarios (17 trait scenarios and seven file/corpus checks)
failed in at least one control run. Each source control ran the integration
binary with `cargo test -p orch-engine --test storage_conformance`; focused corpus
controls used the same command with a scenario-name filter. These are manual
mutation checks, not faults retained in product code or a new mutation runner.

| Deliberate fault | Guard demonstrated |
|---|---|
| Prepend saves instead of append | Task/goal/agent/message lifecycle order, complete-list order and six-run rotation |
| Reverse task status rank | Task status/time and filtered-order scenario |
| Reverse goal status rank | Goal status/time scenario |
| Return no teams | Team lifecycle and collection completeness |
| Reverse run start-time ordering | Run list order and collection completeness |
| Drop stored events on read | Event append/tail/stream scenario and every-kind corpus |
| Reverse context keys | Context lifecycle and collection completeness |
| Ignore authored config and return defaults | Config replacement/extensions, merge-key corpus and every-kind corpus |
| Discard stored state | Running/claimed/retry/stats scenario, in-flight corpus and malformed-field check |
| Discard global preference writes | Global preference replacement scenario |
| Select delivered instead of pending messages | Recipient/expiry scenario and collection completeness |
| Give missing state a nonzero run count | Empty stores and empty/small corpus |
| Alter the stale index title | Index/entity disagreement oracle |
| Alter a small-project index ID | Small-project list oracle |
| Insert a synthetic private-key header marker | Corpus privacy scan |
| Alter the project prompt bytes | Template corpus oracle |
