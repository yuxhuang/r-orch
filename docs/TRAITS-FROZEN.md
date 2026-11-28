# Frozen trait surface (end of WP-0)

Quick index of everything `orch-core` exports, for the five packages compiling
against it. Changing anything here needs an explicit amendment — it breaks
`orch-infra`, `orch-engine`, `orch-cli` and `orch-tui` at once.

> **Revised for the amendments in commit `aea84ca` / `946da84`.** Trait
> *signatures* are unchanged; three behavioural rules and the library-first
> dependency set were added. If you branched before this revision, the items
> that affect you are **§2's three storage rules** (a `save()` that preserves
> position is now a defect), the **`clippy.toml` stable-sort denial**, and the
> new **`[workspace.dependencies]` entries** in §10. Nothing else moved.
>
> **Amendment 1 changes one type.** `SpawnedChild`'s three pipes are now boxed
> `AsyncRead`/`AsyncWrite` trait objects instead of `tokio::process::Child*`
> handles — see [Amendment 1](#amendment-1--spawnedchild-stdio-is-boxed-wp-2)
> below. Build with `SpawnedChild::from_tokio(..)` and nothing else changes.
>
> **Amendment 3 removes seven `Send` bounds** from the additive v2 write side: `UnitOfWork` and its six write
> traits. Nothing else in `orch_core::store` moved — see
> [Amendment 3](#amendment-3--the-unit-of-work-stays-on-the-writers-thread) below.

Everything returns `orch_core::Result<T>` = `Result<T, orch_core::Error>` unless
stated otherwise. Every trait is `Send + Sync` and takes `&self`, never
`&mut self`: stores are shared as `Arc<dyn …>` and own their interior
mutability. The additive v2 write traits (`UnitOfWork` and the six it extends,
§2) are the exception: their changes take `&mut self` (some of their reads take `&self`), they were `Send` and not
`Sync`, and since Amendment 3 they are not `Send`.

---

## 1. `orch_core::error`

```rust
pub type Result<T> = std::result::Result<T, Error>;

pub enum Error {
    Domain(DomainError),                                  // keeps its exit code + hint
    Io     { context: String, path: Option<PathBuf>, source: std::io::Error },
    Serde  { context: String, path: Option<PathBuf>, source: Box<dyn Error + Send + Sync> },
    Command{ command: String, status: String, output: String },
    Adapter(AdapterError),
}

impl Error {
    pub fn exit_code(&self) -> i32;            // Domain → its code; everything else → 1
    pub fn hint(&self) -> Option<String>;
    pub fn path(&self) -> Option<&Path>;
    pub fn io(context, path, source) -> Error;
    pub fn serde(context, path, source) -> Error;
    pub fn other(message) -> Error;            // DomainError::Other, exit 1
    pub fn invalid_arguments(message) -> Error; // exit 2
}
```

`From<DomainError>` and `From<AdapterError>` are implemented, so `?` works on
domain functions and adapter calls.

## 2. `orch_core::storage` — 10 traits

**Three rules every implementation owes the contract**, all added by the
amendments and all replicated by the fakes — so a fake-backed test will *not*
catch a store that skips one:

1. **`save` removes by id, then pushes to the end of `_index.json`.**
   Index order is *last-write* order. It is observable:
   `findBestAgent` breaks score ties by position in `AgentStore::list()`, so an
   in-place update that preserves position silently changes which agent runs a
   task. Applies to the five
   index-backed stores (task, agent, goal, context, message); `TeamStore` and
   `RunStore` have no index and scan their directories, so their order is
   filesystem enumeration order and is **not** contract.
2. **Sorts over index order must be stable.** `slice::sort_unstable`,
   `sort_unstable_by` and `sort_unstable_by_key` are **denied workspace-wide**
   in `clippy.toml` with that reason attached; the lint is verified to fire.
   Override only with an `#[allow]` and a one-line justification.
3. **Every YAML read must call `Value::apply_merge()`.** YAML merge keys (`<<`)
   are part of the config format, but `serde_yaml_ng` leaves `<<` as a literal
   `"<<"` key.
   Forgetting it makes a hand-authored `config.yml` silently fall back to
   `DEFAULT_CONFIG` for everything the merge supplied — while both trees still
   compare equal under the Rust parser. See "YAML emission evidence" below for the
   ready-made regression tests.

```rust
pub struct TaskFilter { pub status: Option<TaskStatus>, pub goal_id: Option<String> }
impl TaskFilter { fn with_status(..); fn with_goal(..); fn matches(&self, &Task) -> bool }

pub struct GoalFilter { pub status: Option<GoalStatus> }
impl GoalFilter { fn with_status(..); fn matches(&self, &Goal) -> bool }

pub struct ContextEntry { key, value, created_at, updated_at, ttl_ms?, expires_at? }
```

| Trait | Methods |
|---|---|
| `TaskStore` | `list(&TaskFilter) -> Vec<Task>`, `get(&str) -> Option<Task>`, `save(&Task)`, `delete(&str)` |
| `AgentStore` | `list() -> Vec<Agent>`, `get`, `get_by_name`, `save(&Agent)`, `delete` |
| `RunStore` | `save(&Run)`, `get`, `list_all`, `list_for_task`, `list_for_agent`, `append_event(&str, &RunEvent)`, `read_events`, `read_events_tail(&str, usize)`, **sync** `stream_events(&str, CancellationToken) -> BoxStream<'static, Result<RunEvent>>`, **sync** `close_run_events(&str)` |
| `StateStore` | `read() -> OrchestratorState`, `write(&OrchestratorState)` |
| `ConfigStore` | `read() -> OrchestratorConfig`, `write(&OrchestratorConfig)`, `get(&str) -> Option<Value>`, `set(&str, Value)` |
| `ContextStore` | `get(&str) -> Option<ContextEntry>`, `set(&str, &str, Option<u64>)`, `delete`, `list() -> Vec<ContextEntry>`, `get_all() -> IndexMap<String, String>` |
| `MessageStore` | `save(&Message)`, `get`, `list`, `list_pending(&str)`, `mark_delivered(&str)`, `delete`, `purge_expired() -> usize` |
| `GoalStore` | `list(&GoalFilter) -> Vec<Goal>`, `get`, `save(&Goal)`, `delete` |
| `TeamStore` | `save(&Team)`, `get`, `get_by_name`, `list`, `delete` |
| `GlobalConfigStore` | `read() -> GlobalConfig`, `write(&GlobalConfig)` |

**WP-1, read this:** `ConfigStore::get`/`set`/`write` must round-trip through the
**raw merged value**, not through `OrchestratorConfig`. The store deep-merges
plain objects, so unknown keys in `config.yml` survive a `config set`. Use
`orch_domain::{deep_merge, get_by_path, set_by_path}` on a
`serde_json::Value`, and deserialize the typed struct only for consumers.
`FakeConfigStore` does exactly this and has the test for it.

### Additive v2 event and lease contracts (ADR 0017 decision 3)

The existing v1 signatures remain active until T25. The additive `orch_core::store`
port is governed by the storage spec's "Storage port v2" and ADR 0017; it returns
`store::Result<T>`. Its `ReadSnapshot` and `UnitOfWork` supertraits are listed in
"Additive v2 port: entities, paging and operational families" below, which also
records the other families; this section holds the event and lease signatures.
All operations use the enclosing caller's project; authorization belongs to the
dispatcher. These operations are not yet wired into the running daemon.

```rust
// events::EventRead (async, &self)
read_events(run: &str, after_seq: u64, limit: usize) -> Result<Vec<SequencedEvent>>;
read_events_tail(run: &str, count: usize) -> Result<Vec<SequencedEvent>>;
read_event_gaps(run: &str) -> Result<Vec<EventGap>>;
// events::EventWrite (&mut self)
append_events(run: &str, lineage: &str, epoch: u64, batch: &[SequencedEvent]) -> Result<AppendResult>;
record_event_gap(run: &str, after_seq: u64, dropped: u64) -> Result<()>;
prune_events(run: &str, max_rows: usize) -> Result<PruneResult>;
// leases::LeaseRead (async, &self)
read_lease(task: &str) -> Result<Option<Lease>>;
read_agent_lease(agent: &str) -> Result<Option<Lease>>;
read_leases() -> Result<Vec<Lease>>;
// leases::LeaseWrite (&mut self for changes; &self for reads)
claim_lease(new: &NewLease, now_ms: i64) -> Result<ClaimOutcome>;
renew_lease(task: &str, lineage: &str, epoch: u64, now_ms: i64) -> Result<Lease>;
release_lease(task: &str, lineage: &str, epoch: u64) -> Result<Lease>;
get_lease(task: &str) -> Result<Option<Lease>>;
get_agent_lease(agent: &str) -> Result<Option<Lease>>;
list_leases() -> Result<Vec<Lease>>;
```

`SequencedEvent` carries `seq` (positive, collector-assigned) and `RunEvent`.
`AppendResult` holds `stored` and `skipped`; first stored bytes win on duplicates.
`EventGap { after_seq, dropped }` names `(after_seq, after_seq + dropped]`.
Its key is `(project_id, run_id, after_seq)`: identical `dropped` replays are
no-ops; conflicting counts return `InvalidEvents` without changing any row.
`PruneResult { deleted, complete }` reports whether any event rows remain, retaining
gaps. Read/prune bounds are 1..=1000; a tail of zero is empty.
`NewLease { task, agent, run }` receives a backend-assigned lineage and epoch in
`Lease { task, agent, run, lineage, epoch, expires_at }`. Lease clocks use UTC
milliseconds from `Clock::now_ms()`; claims/renewals store checked
`now_ms + LEASE_MS`, where `LEASE_MS: i64 = 120_000`. A late renewal is allowed until explicit release
(AP12). `ClaimOutcome` is `Granted(Lease)` or `Held(Lease)`; an occupied task, agent
or run is checked in that order and burns no epoch. Renew, release and append
refuse `StaleEpoch` for a missing lease, stale lineage/epoch or mismatching task
principal. Invalid event arguments return `InvalidEvents(String)`. Operation errors
leave the operation unchanged; a callback error rolls back the entire unit of work.

### Additive v2 port: entities, paging and operational families (ADR 0017 decision 3; ADR 0020)

Recorded from `crates/orch-core/src/store*` as of `main` at `07b5f967` plus the three additions of issue 109
(ADR 0020, "Added for issue 109" below): every public trait, trait method, type,
constant and free function, and every public inherent method (the shared validators an adapter calls for parity
are in the listings below, as one-line comments or as `fn` lines). The `test-support` fakes (`store::*::fake`) are
not listed; they are the memory reference, not the contract. This is the contract of the
spec's "Storage port v2", the amendments in it and ADR 0020. The v1 traits of the table above stay active, with
their three rules, until T25 removes the product file adapter; nothing in this section is wired into the running
daemon. Every operation below uses the enclosing caller's project (a key cannot select another), and none of them
authorizes: the dispatcher does.

Traits with `async fn` use `#[async_trait]`; the listings drop `pub` on traits and the `async_trait` attribute.

```rust
// store (store.rs)
pub struct Caller { pub principal: Principal, pub project: String }
pub enum Principal {                          // serde: tag "kind", snake_case
    Owner  { identity: String },
    Worker { identity: String },
    Task   { identity: String, task: String, run: String, lineage: String, epoch: u64 },
}
pub enum Lane { Claims, Entities, Events, Maintenance }      // choosing a lane grants nothing
pub type Result<T> = std::result::Result<T, StoreError>;
pub type WriteWork = Box<dyn FnOnce(&mut dyn UnitOfWork) -> Result<()> + Send + 'static>;   // owned: invoked once, on the writer

// The only entry points (async_trait). Backends serialize writers.
trait Storage: Send + Sync {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>>;
    async fn write(&self, caller: Caller, lane: Lane, work: WriteWork) -> Result<()>;
}
// Blanket-implemented for every Storage: the typed result is returned only after commit.
trait StorageExt: Storage {
    async fn transaction<T, F>(&self, caller: Caller, lane: Lane, work: F) -> Result<T>
        where T: Send + 'static, F: FnOnce(&mut dyn UnitOfWork) -> Result<T> + Send + 'static;
}

// One committed state for every read through it, repeated page reads included.
trait ReadSnapshot: RetryTextRead + AttentionRead + EventRead + IntentRead + LeaseRead + RetryRead + Send + Sync {
    fn caller(&self) -> &Caller;
    async fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>>;
    async fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>>;
}
// Synchronous, inside one transaction. An error from the callback discards every operation.
// Not `Send` (Amendment 3): lent to the callback as `&mut dyn UnitOfWork`, used on the thread that runs it.
trait UnitOfWork: AttentionWrite + EventWrite + IntentWrite + LeaseWrite + RequestKeyWrite + RetryWrite {
    fn caller(&self) -> &Caller;
    fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>>;
    fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>>;
    fn create(&mut self, entity: Entity) -> Result<Record>;
    fn update(&mut self, entity: Entity, expected: Revision) -> Result<Record>;
    fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<()>;
    fn move_to_end(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record>;
}
```

`ReadSnapshot` has no request-key read: a stored result is read only by the writer, in the transaction of its change
(R74). `update`, `delete` and `move_to_end` take the `expected` revision the caller read; any other stored value,
or no row, is `StaleRevision { kind, id, expected, actual }` (`actual` is `None` for a missing row) and changes
nothing. `create` stores revision 1 and the next `position` of its project and family, `update` adds one to the
revision and keeps the position, `move_to_end` adds one to the revision and gives the next position, and `delete`
never lets a position be reused. A second `create` of an id is `AlreadyExists`, and a task id with the same ASCII
fold key as another is `IdAlias` (R117); every lookup is exact to the byte.

```rust
// store::entity
pub struct Revision(NonZeroU64);              // serde refuses 0; Revision::ONE; get() -> u64; next() -> Result<Revision>
pub enum EntityKind { Task, Goal, Agent, Team, Run, Message, Context, Config, Template }   // snake_case
pub struct ConfigEntry { pub key: String, pub value: String }        // project_config; the key is the id
pub struct TemplateEntry { pub name: String, pub body: String }      // templates; the name is the id
pub enum Entity {                             // serde: tag "kind", content "value", snake_case
    Task(Box<Task>), Goal(Box<Goal>), Agent(Box<Agent>), Team(Box<Team>), Run(Box<Run>),
    Message(Box<Message>), Context(ContextEntry), Config(ConfigEntry), Template(TemplateEntry),
}                                             // Entity::kind() -> EntityKind, Entity::id() -> &str
pub struct Record { pub entity: Entity, pub revision: Revision, pub position: u64 }

// store::page
pub struct Cursor(String);                    // Cursor::new refuses empty or over 512 bytes; as_str()
pub enum Sort { Default, Id, Position, Name, Priority, CreatedAt, UpdatedAt, Attempt }   // snake_case; Default is the default
pub enum Filter {                             // serde: tag "kind", snake_case
    Tasks { status: Option<TaskStatus>, goal: Option<String> },
    Goals { status: Option<GoalStatus> },
    Agents,
    Teams,
    Runs { task: Option<String>, agent: Option<String> },
    Messages { recipient: Option<String> },
    PendingMessages { recipient: String, now: String },    // serde kind "pending_messages"; kind() = Message
    Context,
    Config,
    Templates,
}                                             // Filter::kind() -> EntityKind
pub struct PageRequest { pub limit: usize, pub cursor: Option<Cursor>, pub sort: Sort, pub with_count: bool }
                                              // default: 100, no cursor, Sort::Default, no count; validate(): limit in 1..=1000
pub struct Page<T> { pub items: Vec<T>, pub next_cursor: Option<Cursor>, pub total: Option<u64> }
                                              // total counts rows matching the filter, before the cursor

// store::blob (independent of the entity transaction; publish bytes first, then register the hash in a unit of work)
trait BlobStore: Send + Sync {
    type Hash: Clone + Eq + Display + FromStr + Send + Sync;
    type Error: std::error::Error + Send + Sync + 'static;
    fn put(&self, bytes: &[u8]) -> Result<Self::Hash, Self::Error>;       // durable on return; equal bytes deduplicate
    fn get(&self, hash: &Self::Hash) -> Result<Vec<u8>, Self::Error>;     // verifies the hash; missing or corrupt is an error
    fn remove(&self, hash: &Self::Hash) -> Result<bool, Self::Error>;     // idempotent; the caller proves no reference remains
}
// Three provided methods and `BlobStreamHasher` were added for the bounded retry read: see "Added for the bounded retry read".
```

The sorts each family offers are the spec's table: tasks `Position`, `Priority`, `CreatedAt`, `UpdatedAt`; goals
`CreatedAt`; agents and teams `Name`; every family `Default` and `Id`; `Attempt` only for runs of one task (below);
a backend refuses any other combination as `InvalidPage`. A cursor is opaque, holds no snapshot, and is bound to its project, filter and sort.

```rust
// store::retries (RetryRead and RetryTextRead on ReadSnapshot, RetryWrite on UnitOfWork)
pub const MAX_DUE_RETRIES: usize = 1_000;
pub const MAX_RETRY_ERROR_BYTES: usize = 64 * 1024;       // 65,536: the inline threshold, not a limit on the text
pub struct Retry { pub task: String, pub attempt: u32 /* >= 1 */, pub due_at: i64 /* UTC ms, >= 0 */, pub error: String }
                                              // Retry::validate(&self) -> Result<()>: task not empty, attempt >= 1, due_at >= 0
pub struct RetryCursor { pub due_at: i64, pub task: String }   // RetryCursor::after(&Retry); validate()
pub struct RetryPage { pub rows: Vec<Retry>, pub next: Option<RetryCursor> }
pub fn check_limit(limit: usize) -> Result<()>;           // 1..=MAX_DUE_RETRIES
trait RetryRead: Send + Sync {
    async fn read_retry(&self, task: &str) -> Result<Option<Retry>>;
    async fn read_due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>>;
    async fn read_retries(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage>;
}
trait RetryWrite {
    fn put_retry(&mut self, retry: &Retry) -> Result<()>;                 // inserts or replaces the whole row
    fn delete_retry(&mut self, task: &str) -> Result<bool>;               // a missing row is not an error
    fn get_retry(&self, task: &str) -> Result<Option<Retry>>;
    fn due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<Retry>>;
    fn list_retries(&self, after: Option<&RetryCursor>, limit: usize) -> Result<RetryPage>;
}
```

Retries: one row per task and project, no cap and no drop. A due row has `due_at <= now_ms`. Due and complete reads
return earliest deadline first, ties by task id compared as UTF-8 bytes; that is a read order and never dispatch
order. A read that would return a row whose long `error` cannot be resolved fails (below); a row it does not select is not
resolved, and the three async `RetryRead` methods of the file reference and the disk-backed fake read blobs on the
blocking boundary, not on the executor (behaviour, not a signature). The complete read follows `next` (exclusive keyset cursor; `Some` exactly when a row follows, so a walk never
ends on an empty page); pages of one snapshot see one committed state, and a unit of work sees its own writes. Put
replaces, where 1.0's enqueue kept the first row: a consumer that needs first-wins calls `get_retry` first, in the
same unit of work. An unparsable 1.0 deadline is mapped to due by the importer, not by the port.

```rust
// store::intents (IntentRead, IntentWrite)
pub const DONE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;   pub const MAX_ID_BYTES: usize = 128;   pub const MAX_ROWS: usize = 1_000;
pub enum IntentKind { ProcessStart, ProcessStop, WorkspaceRemove, BlobDelete }   // as_str(): "process.start" "process.stop" "workspace.remove" "blob.delete"; repeats_after_restart(): false for ProcessStart only
pub enum IntentPayload {                      // serde tag "kind", deny_unknown_fields
    ProcessStart { run: String }, ProcessStop { run: String }, WorkspaceRemove { task: String }, BlobDelete { hash: String },
}                                             // kind(), run(), validate()
pub fn check_bound(value: usize) -> Result<()>;           // 1..=MAX_ROWS; the bound both backends apply to reads and deletions
pub struct Intent { pub id: u64, pub payload: IntentPayload, pub created_at: i64, pub done_at: Option<i64> }
pub enum FinishOutcome { Finished, AlreadyDone }
trait IntentRead: Send + Sync {
    async fn read_intent(&self, id: u64) -> Result<Option<Intent>>;
    async fn read_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<Intent>>;
}
trait IntentWrite {
    fn add_intent(&mut self, payload: &IntentPayload, now_ms: i64) -> Result<Intent>;
    fn finish_intent(&mut self, id: u64, now_ms: i64) -> Result<FinishOutcome>;
    fn delete_finished_intents(&mut self, now_ms: i64, max_rows: usize) -> Result<usize>;
    fn get_intent(&self, id: u64) -> Result<Option<Intent>>;
    fn list_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<Intent>>;
}

// store::request_keys (RequestKeyWrite only)
pub const KEY_TTL_MS: i64 = 3_600_000;   pub const MAX_RESULT_BYTES: usize = 64 * 1024;   pub const MAX_ROWS: usize = 1_000;
pub const MIN_KEY_BYTES: usize = 16;   pub const MAX_KEY_BYTES: usize = 128;   pub const MAX_OP_BYTES: usize = 128;
pub struct RequestScope { /* identity, project, run, epoch, key */ }       // RequestScope::of(&Caller, key) -> Result<Self>
                                              // identity() -> &str, project() -> &str, run() -> &str ("" for owner or worker),
                                              // epoch() -> u64 (0 for owner or worker, otherwise 1..=i64::MAX), key() -> &str,
                                              // check_caller(&Caller) -> Result<()>: refuses a scope built for another caller
// The shape checks both backends make before they read or change anything:
pub fn check_key(key: &str) -> Result<()>;                // MIN_KEY_BYTES..=MAX_KEY_BYTES of A-Z a-z 0-9 _ -
pub fn check_request(op: &str, req_sha256: &str, now_ms: i64) -> Result<()>;   // op 1..=MAX_OP_BYTES of A-Z a-z 0-9 . _ -; 64 lowercase hex; check_now
pub fn check_now(now_ms: i64) -> Result<()>;              // UTC milliseconds, not before the epoch
pub fn expires_at(now_ms: i64) -> Result<i64>;            // checked now_ms + KEY_TTL_MS
pub fn check_bound(value: usize) -> Result<()>;           // 1..=MAX_ROWS, the bound on one expiry call
pub enum KeyCheck { New, Replay(String), ReplayWithoutResult, Mismatch }
trait RequestKeyWrite {
    fn check_request_key(&self, scope: &RequestScope, op: &str, req_sha256: &str, now_ms: i64) -> Result<KeyCheck>;
    fn store_request_key(&mut self, scope: &RequestScope, op: &str, req_sha256: &str, result: &str, now_ms: i64) -> Result<()>;
    fn delete_expired_request_keys(&mut self, now_ms: i64, max_rows: usize) -> Result<usize>;
}

// store::attention (AttentionRead, AttentionWrite)
pub const MAX_ATTENTION_PAGE: usize = 1_000;   pub const MAX_ATTENTION_DELETE: usize = 1_000;
pub enum AttentionKind { DiskFloor, DispatchOffWithWork }   pub enum Severity { Blocking, Warning }
pub enum DiskSubject { DataDirectory, ProjectWorkspaces { project: String } }
pub enum AttentionFacts {
    DiskFloor { subject: DiskSubject, free_bytes: u64, floor_bytes: u64 },
    DispatchOffWithWork { waiting_tasks: u64, oldest_waiting_at: i64 },
}                                             // kind() -> AttentionKind, validate(&self, project: &str, at: i64) -> Result<()>
                                              // (scope and facts, checked before even a duplicate-open lookup),
                                              // subject_id() -> String (the uniqueness key within kind and scope),
                                              // severity() -> Severity, title() -> &'static str,
                                              // detail() -> Result<String>, remedy() -> Result<&'static str> (both recheck ids)
pub struct Acknowledgement { pub identity: String, pub at: i64 }
pub enum ResolutionReason { DiskSpaceRecovered, DispatchEnabled, NoDispatchableWork }
                                              // validate(self, kind: AttentionKind) -> Result<()>: refuses a reason of another kind
pub struct Resolution { pub reason: ResolutionReason, pub at: i64 }
pub struct Attention { pub id: u64, pub facts: AttentionFacts, pub opened_at: i64,
                       pub acknowledgement: Option<Acknowledgement>, pub resolution: Option<Resolution>, pub revision: Revision }
                                              // check_transition_time(&self, at: i64) -> Result<()>: refuses a time before the last of opened, acknowledged, resolved
pub enum AttentionFilter { All, Open, Resolved }   // matches(self, &Attention) -> bool
pub struct AttentionCursor { /* project, filter, last id */ }   // after(project: &str, filter, id: u64) -> Self; check(&self, project, filter) -> Result<u64>
pub fn check_id(id: &str) -> Result<()>;                  // 1..=128 of A-Z a-z 0-9 _ -
pub fn check_time(at: i64) -> Result<()>;                 // UTC milliseconds, not before the epoch
pub fn check_limit(limit: usize) -> Result<()>;           // 1..=MAX_ATTENTION_PAGE (store::attention's; retries has its own)
pub struct AttentionPage { pub items: Vec<Attention>, pub next: Option<AttentionCursor> }
trait AttentionRead: Send + Sync {
    async fn read_attention(&self, id: u64) -> Result<Option<Attention>>;
    async fn read_attention_page(&self, filter: AttentionFilter, after: Option<&AttentionCursor>, limit: usize) -> Result<AttentionPage>;
}
trait AttentionWrite {
    fn get_attention(&self, id: u64) -> Result<Option<Attention>>;
    fn attention_page(&self, filter: AttentionFilter, after: Option<&AttentionCursor>, limit: usize) -> Result<AttentionPage>;
    fn open_attention(&mut self, facts: &AttentionFacts, at: i64) -> Result<Attention>;
    fn ack_attention(&mut self, id: u64, expected: Revision, at: i64) -> Result<Attention>;
    fn resolve_attention(&mut self, id: u64, expected: Revision, reason: ResolutionReason, at: i64) -> Result<Attention>;
    fn expire_attention(&mut self, cutoff: i64, limit: usize) -> Result<usize>;
}

// Errors (thiserror)
pub enum StoreError {
    StaleRevision { kind: EntityKind, id: String, expected: Revision, actual: Option<Revision> },
    AlreadyExists { kind: EntityKind, id: String },   IdAlias { existing: String, requested: String },
    InvalidPage(String),  InvalidCursor,  StaleEpoch,  InvalidEvents(String),  InvalidIntent(String),
    InvalidRetry(String), InvalidRequestKey(String),  InvalidAttention(String),
    StaleAttentionRevision { id: u64, expected: Revision, actual: Option<Revision> },
    Unrepresentable,                          // issue 441: a unit variant, no payload (see "Added for issue 441")
    CounterExhausted,  Backend(orch_core::Error),
}
```

The families' rules (time is always the caller's UTC milliseconds, the one exception being the ISO string that
`PendingMessages` carries below; bounds 1..=1,000; the first stored bytes win; a refusal caught and committed leaves
nothing) are in the spec's amendments for leases,
intents, attention and request keys. `orch_core::store::fake::FakeStorage` (feature `test-support`) is the memory
reference, and `orch_infra::storage::v2` the file reference; both run the same shared conformance suites, and neither
is wired into the daemon. The only `BlobStore` adapter in 1.1 is `orch_infra::blob::DiskBlobStore`.

The file reference also fixes what a persisted request-key document may hold (the spec's T8c request-keys amendment).
It adds no signature. A stored row's epoch is at most `i64::MAX`, the ceiling `RequestScope::of` applies, and its
`expires_at` is exactly `created_at + KEY_TTL_MS`, so a change to `KEY_TTL_MS` must migrate or reconcile the stored
rows in the same change. A row denies an unknown field and the collection that holds the rows ignores one. A negative
`now_ms` is refused on `check_request_key` as on `store_request_key`. The tie order of rows with an equal expiry in
`delete_expired_request_keys` is not observable through the port; T10 proves it on SQLite under issue 127.

#### Added for issue 441 (ADR 0020 decision 8)

One error variant and two inherent methods, all in `orch_core::store`. They are additive: no earlier signature
changes and nothing is removed. `StoreError` is not `#[non_exhaustive]`, so a consumer that matches it with no wildcard
arm must add the variant. The `test-support` fakes are not part of the contract and are not listed. The change lands
on the memory and file references and on SQLite's public converters; nothing is wired into the daemon, and no caller
in the engine matches the variant yet.

```rust
// store (store.rs)
impl Caller { pub fn validate_context_provenance(&self) -> Result<()>; }   // Err(StoreError::Unrepresentable)
// store::entity
impl Entity { pub fn validate_storage_input(&self) -> Result<()>; }        // Err(StoreError::Unrepresentable)
// StoreError::Unrepresentable            #[error("input cannot be represented by storage")]
```

- **The variant.** `StoreError::Unrepresentable` has no field. Its `Display` is exactly `input cannot be represented by
  storage`, its `Debug` is exactly `Unrepresentable`, and `std::error::Error::source` is `None`: it holds no input, no
  field name, no bound and no backend text. It says that the caller's value is outside the storage contract, so
  retrying the same value cannot succeed. A consumer matches the variant, never the message, does not reschedule that
  input, and does not infer permanence from `Backend` text or from any other variant.
- **What `Entity::validate_storage_input` refuses.** Only a `Task`; every other kind returns `Ok`.
  - `id`, `assignee` (when present), `goal_id` (when present), `created_at` and `updated_at` are at most 65,536 UTF-8
    bytes each. The unit is bytes: 32,768 two-byte characters are exactly at the bound. An empty value is valid.
  - `created_at` and `updated_at` must also be the canonical form `YYYY-MM-DDTHH:MM:SS.sssZ` (24 bytes, UTC, three
    fraction digits), from `0000-01-01T00:00:00.000Z` to `9999-12-30T22:00:00.999Z` inclusive. A string is accepted
    only if the core clock codec parses it and formats the instant back to the same bytes, so second precision, an
    offset, an expanded year, a month 13, a day that does not exist (`2026-02-30`), the next millisecond
    (`9999-12-30T22:00:01.000Z`) and `9999-12-31T23:59:59.999Z` are refused, and `2024-02-29T00:00:00.000Z` is accepted.
  - `title`, `description`, `feedback` and every other field have no new bound: text over 65,536 bytes still round
    trips (goal decision 7, R97).
- **What `Caller::validate_context_provenance` refuses.** The identity of an `Owner`, a `Worker` or a `Task` principal,
  and the `run` of a `Task` principal, over 65,536 UTF-8 bytes. The `task`, `lineage` and `epoch` of a `Task` principal
  are not persisted as Context provenance and are not checked. The check runs on `create` of a `Context` entity only.
  `update` never consults the caller for it, so an update keeps the persisted provenance as stored, whoever makes it
  and however long that caller's identity is. This check validates a value that is about to be stored; it grants no
  authority and does not replace the dispatcher's authorization (R132).
- **Where the memory and file references apply them.** `create` runs `validate_storage_input` for every kind, then
  `validate_context_provenance` for a Context, after the `AlreadyExists` and `IdAlias` checks and before the position is
  taken. `update` runs `validate_storage_input` after the stale-revision check and before the revision is advanced.
  `delete` and `move_to_end` take no entity and have no new refusal.
- **A refusal changes nothing.** The row, its revision and its position are as they were, and a refused `create` takes
  no position: the next `create` gets the position after the last stored one. Inside a unit of work the callback sees
  the error and may catch it; the refused operation wrote nothing, and other work in that unit commits as usual. When
  the callback returns the error, the whole unit rolls back, as for any other error.
- **SQLite.** Both public converters, `port` in `sqlite/storage/mod.rs` and `port` in `sqlite/entity_router.rs`, map
  `SqliteError::Unrepresentable(_)` to `StoreError::Unrepresentable` and drop its text. The mapping runs after the
  failure state has observed the native error, and that classifier treats `Unrepresentable` as an ordinary class
  that latches nothing (`sqlite/writer/failure.rs`), so a refusal does not retire the writer. Every other arm is as it
  was: a `SqliteError::Store` passes through, and any other native error becomes `StoreError::Backend` with a fixed
  message. **The mapping is wider than the reference adapters' checks:** it covers every native `Unrepresentable`,
  which also arises for entity encodings of the other families, for a message outside the schema's bounds on write, for
  a retry diagnostic whose stored document is over its bound (issue 546), for cursor shapes, and for the identity of a
  claimed Run (held, below). Parity of the three adapters is stated and tested for the Task and Context rules above and
  for nothing else.
- **Faults and damaged data stay distinct.** A fault of the backend is `Backend(orch_core::Error)`: an I/O failure of
  the file reference is `Backend(Error::Io)`, a statement failure on SQLite is `Backend` with the message `SQLite storage
  operation failed` and latches nothing, and a retry of the same call after the fault clears succeeds. Persisted data
  that does not decode is not an input refusal: the file reference answers `Backend(Error::Serde)` and leaves the
  document unchanged, and SQLite's stored-data damage is the fatal logical-corruption cause that fails the writer and
  answers `Backend` with the message `storage_unavailable`.
- **Not part of this record.** The identity of a claimed Run is not covered: a Run update that changes `task_id` or
  `agent_id` of a persisted claim is refused by SQLite's native guard (`Unrepresentable`, "claimed run identity") and by
  neither reference, so the three adapters differ there. That parity belongs to T12's claim-allocation task
  (decision of 28 November 2026) and is not claimed here. Which refusal wins when more than one applies is not part of
  the contract, except that all three answer `AlreadyExists` and `IdAlias` before a representability refusal on `create`
  and a stale revision before it on `update`; on `update` of a row at the revision ceiling SQLite answers the exhausted
  counter first and the references the refusal (issue 545). The bound constants are duplicated between
  `orch-core` and the SQLite codec (issue 544).

#### Added for issue 109 (ADR 0020)

These three changes are implemented by the change that carries this record, on the memory and file references. Each is
an addition: no earlier signature changes, and nothing is removed. On `main` at `07b5f967` neither variant existed and
a retry `error` over 65,536 bytes was refused. The `test-support` fakes are not part of the contract and are not
listed.

```rust
// store::page
pub enum Filter { /* ..., */
    PendingMessages { recipient: String, now: String },    // serde kind "pending_messages"; Filter::kind() = Message
}
pub enum Sort { /* ..., */ Attempt }                       // serde "attempt"
```

- `Filter::PendingMessages { recipient, now }` selects messages with status `pending`, `to_agent_id == Some(recipient)`
  byte for byte, and `expires_at` absent or `expires_at >= now`, compared as strings. 1.0's `is_expired` is
  `expires_at < now`, so equality is pending, a broadcast (no recipient) is never pending, and an `expires_at` that is
  not a date is compared as text and never parsed. `now` is the caller's captured `Clock::now_iso()`; the port reads
  no clock and adds no validation. The filter runs before the count, the cursor and the page bound, so a page fills
  with matches and `total` counts matches. A cursor binds the recipient, `now`, the sort and the project. Order is the
  messages' default (`created_at`, `position`, id); the other sorts are those of `Filter::Messages` (`Default` and `Id`).
- `Sort::Attempt` is offered only for `Filter::Runs { task: Some(_), .. }`. It orders by `Run.attempt` numerically,
  descending, then run id bytes ascending. For `Runs` with no task, for any other family and for any other
  filter it is `InvalidPage`, before any paging work. The `Sort::Default` of a run list whose filter names a
  task (alone or beside an `agent`) is the same key (PR #225; the spec's "Run and goal default order"), so the default and
  `Attempt` list the same rows in the same order and are two sorts for a cursor. For a run list with no task,
  `Sort::Default` is `started_at` text descending, then id. The spec's earlier `run_no` is this sort: `Run` has no such
  field.
- `Retry.error` is the whole logical text. `MAX_RETRY_ERROR_BYTES` (65,536 UTF-8 bytes) is the inline
  threshold and `Retry::validate` no longer refuses longer text; the other checks stay. Longer text is published to the
  blob store before the row that carries its hash is committed, and a read returns the original text byte for byte. A
  failed publication, or a missing or corrupt blob when writing, fails the write and keeps the previous row. A
  reference that does not resolve on read is an error from `read_retry`, `read_due_retries`, `read_retries`,
  `get_retry`, `due_retries` and `list_retries` whenever the call would return that row, and never a shortened row.
  Rows, bounds, orders and first-wins handling are unchanged.
- Above one blob object the text is chunked. `DiskBlobStore` holds at most 67,108,864 bytes (64 MiB) per object, and
  that bound stays. Text of 65,537 to 67,108,864 bytes inclusive is one object named by the row's hash. Longer text keeps the SHA-256 of the whole value in the row, which no single object holds,
  plus an ordered mapping to chunk objects of at most 67,108,864 bytes each, with one entry per position (identical
  chunks repeat). A chunk may split a UTF-8 character: the joined bytes are verified against the whole-value hash
  and only then decoded. A read verifies every object and the whole value; a missing, corrupt or reordered chunk is
  an error. A write publishes every object before it commits the row and mapping together, and a failure on a later
  chunk keeps the previous row readable even if the caller catches the error and commits other work. Objects an
  interrupted write leaves follow the blob store's lifecycle. The mapping's physical encoding is the adapters'
  (`Retry`, `RetryRead` and `RetryWrite` keep the shape listed above). The reference adapters record it as
  `{sha256, chunks}`: the file reference's retry row holds an inline string, `{"sha256": H}` for one object, or
  `{"sha256": H, "chunks": [...]}` above 67,108,864 bytes (hashes are 64 lower-case hex digits, and chunks are cut at
  67,108,864 bytes); a stored mapping with any other field is refused when the document loads. No public type carries it, so this record has no new type to list; the types that hold it are
  private to each adapter, and T10 may choose another encoding that holds the same facts.

#### Added for the bounded retry read (ADR 0020 decision 9)

One trait, one supertrait edge, one error enum, one constant and a small set of types and helpers in
`orch_core::store::retries::stream` (a public module of `store::retries`), plus one trait and three defaulted methods
in `orch_core::store::blob`. They are additive: no earlier method signature changes, `RetryRead` keeps the six reads
listed above, and nothing is removed. One edge is not additive for an implementor of `ReadSnapshot`: it now has
`RetryTextRead` as a supertrait, so every implementation must provide the stream (the memory fake, the file reference
and SQLite do). The `test-support` fakes are not part of the contract and are not listed, except the two
`test-support` methods named below. Signatures read at commit `bdf99d96c` (tree `9c41bb6f`), which carries the three adapters
and the SQLite disk-fault observation described under "Errors on a stored object"; none of the signatures below differs from the
earlier integration commit `b85df7a1a`, because the repair added crate-private items only.

```rust
// store::retries::stream
pub const RETRY_STREAM_WINDOW_BYTES: usize = 65_536;         // the most bytes one sink callback receives

#[derive(Debug, thiserror::Error)]
pub enum RetryStreamError {                                   // every message is content-free
    Resource,                                                 // admission or allocation refused; never damage
    Cancelled,
    Corrupt,                                                  // stored text failed verification
    Store(StoreError),                                        // #[error(transparent)] #[from]; any ordinary store failure
}
pub type Result<T> = std::result::Result<T, RetryStreamError>;

#[derive(Debug, Clone, Copy)]
pub struct RetryTextRow<'a> { pub task: &'a str, pub attempt: u32, pub due_at: i64, pub bytes: u64 }  // bytes = logical UTF-8 length
pub trait RetryTextSink: Send + 'static {                     // called on the adapter's blocking boundary only
    fn begin(&mut self, row: RetryTextRow<'_>, admission: &RetryStreamAdmission) -> Result<()>;
    fn write(&mut self, bytes: &[u8], admission: &RetryStreamAdmission) -> Result<()>;   // <= RETRY_STREAM_WINDOW_BYTES; may split a UTF-8 character
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryTextPage { pub rows: usize, pub bytes: u64, pub next: Option<RetryCursor> }  // returned only after everything verified
#[async_trait]
pub trait RetryTextRead: Send + Sync {
    async fn read_retry_text(&self, after: Option<&RetryCursor>, limit: usize,
                             sink: Box<dyn RetryTextSink>, cancellation: CancellationToken) -> Result<RetryTextPage>;
}

#[derive(Clone)] pub struct RetryStreamAdmission(/* private */);   // clones share one counter
impl RetryStreamAdmission {
    pub fn new(bytes: usize) -> Self;
    pub fn reserve(&self, bytes: usize) -> Result<RetryStreamPermit>;     // checked; refusal is Resource and leaves no charge
    pub fn buffer(&self, bytes: usize) -> Result<RetryStreamBuffer>;      // fallible allocation; charges the Vec's real capacity
    #[cfg(any(test, feature = "test-support"))] pub fn used_bytes(&self) -> usize;            // fixture introspection
    #[cfg(any(test, feature = "test-support"))] pub fn fail_next_buffer_allocation(&self);    // the next `buffer` returns Resource
}
impl Default for RetryStreamAdmission { /* new(64 * 1024 * 1024) = 67,108,864 bytes */ }
pub struct RetryStreamPermit(/* private */);                  // not Clone; releases its bytes once, on drop
pub struct RetryStreamBuffer(/* private */);                  // fixed length; holds its charge until dropped
impl RetryStreamBuffer { pub fn as_mut_slice(&mut self) -> &mut [u8]; }

// Helpers the three adapters share (public so each adapter crate can call them)
pub fn copy_text(value: &str) -> Result<String>;              // fallible copy; the caller has charged its capacity
pub fn check_cancel(token: &CancellationToken) -> Result<()>; // Cancelled when the token has fired
pub struct RetryStreamSelection<'a> { pub keys: Vec<(&'a str, i64)>, pub more: bool, /* private permit */ }
pub fn select_retry_keys<'a>(rows: impl Iterator<Item = (&'a str, i64)>, after: Option<&RetryCursor>, limit: usize,
        admission: &RetryStreamAdmission, cancel: &CancellationToken) -> Result<RetryStreamSelection<'a>>;
#[derive(Default)] pub struct RetryStreamUtf8 { /* four carried bytes */ }
impl RetryStreamUtf8 { pub fn feed(&mut self, bytes: &[u8]) -> Result<()>; pub fn finish(&self) -> Result<()>; }
pub fn verify_retry_objects<'a, B: BlobStore + ?Sized>(store: &B, whole: &str,
        objects: impl Iterator<Item = (&'a str, u64)> + Clone, expected: u64, window: &mut [u8],
        cancel: &CancellationToken, consume: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()>;

// store (ReadSnapshot gains one supertrait; the order below is the declaration order)
pub trait ReadSnapshot: retries::stream::RetryTextRead + AttentionRead + EventRead + IntentRead + LeaseRead
    + RetryRead + Send + Sync { .. }

// store::blob (additive: three provided methods and one trait; `put`, `get`, `remove` and `Hash`/`Error` are unchanged)
pub trait BlobStreamHasher { fn update(&mut self, bytes: &[u8]); fn matches(&self, expected: &str) -> bool; }
pub trait BlobStore: Send + Sync {
    // ... Hash, Error, put, get, remove as listed in section 2 ...
    fn stream_len(&self, hash: &Self::Hash) -> retries::stream::Result<u64> { /* unsupported */ }
    fn read_windowed(&self, hash: &Self::Hash, window: &mut [u8], cancel: &CancellationToken,
                     consume: &mut dyn FnMut(&[u8]) -> retries::stream::Result<()>) -> retries::stream::Result<()> { /* unsupported */ }
    fn with_stream_hasher(&self, work: &mut dyn FnMut(&mut dyn BlobStreamHasher) -> retries::stream::Result<()>)
        -> retries::stream::Result<()> { /* unsupported */ }
}
```

- **Selection.** `read_retry_text(after, limit, ..)` selects the rows, the order and the cursor that
  `RetryRead::read_retries(after, limit)` selects from the same snapshot: earliest deadline first, ties by task id as
  UTF-8 bytes, `limit` in 1..=1,000, `next` exactly when a row follows. A row it does not select is not read; the row
  that decides `next` is looked at only far enough to know that it exists.
- **Provisional output.** `begin` and `write` receive bytes that are provisional until the call returns
  `Ok(RetryTextPage)`, including bytes of rows before one that fails. On any `Err`, on cancellation and on a dropped
  future the caller discards every byte of the page and keeps its cursor. There is no end-of-row callback and none is
  needed: `begin` carries the row's declared length (`RetryTextRow::bytes`), and the page result is provisional as a
  whole. `rows` is the number of rows delivered, `bytes` the checked sum of their logical lengths and `next` the page's
  next cursor.
- **Bytes, not `str`.** A window can end inside a UTF-8 character. The adapter validates UTF-8 across windows and
  objects; the sink must not assume a window is valid text.
- **Admission.** Each adapter owns one `RetryStreamAdmission` and keeps it across snapshots and concurrent reads;
  a call cannot supply its own. Each adapter creates it with `Default`, so the capacity of the counter is 64 MiB
  (67,108,864 bytes) per adapter instance, shared by every read on it; no setting changes it, and it is a bound on live
  working memory, never on the length of a text. It charges the live windows, selected-row metadata and the bytes a sink
  retains, and the adapter passes the same counter to both callbacks; a sink that retains bytes allocates them with
  `buffer`, or holds a `reserve` permit for as long as it holds the bytes. `buffer` charges the capacity the allocator
  returned, which can exceed the request. The counter does not cover allocator bookkeeping, what the database or the
  existing snapshots already hold, or the control allocations of a call, and the `Arc` of `new` is not allocated
  fallibly. It is a count of charged payload bytes, not process memory (RSS).
- **Recovering from `Resource` (issue 554).** `Resource` says that this call's working memory was refused, never that the
  text is damaged, and an adapter may refuse a `limit` that another serves: SQLite charges each selected row eight times
  its encoded document plus its task key plus 4,096 bytes, so 1,000 inline rows of 65,536 bytes are refused there (the
  reviewed probe: limit 126 served, 128, 200 and 1,000 refused) and served by the file reference. The admission is
  unchanged and the port never retries. A consumer recovers as the spec's "Recovering from `Resource`" and ADR 0020
  decision 9 state: only `Resource` permits another call, with `floor(limit / 2)`, on the same snapshot and with the
  unchanged `after` cursor, with a fresh provisional sink; it discards the whole failed page; it does not sleep, wait,
  reopen the snapshot, reset the counter or advance the cursor; a `Resource` at a limit of 1 is terminal (at most
  `1 + floor(log2(limit))` calls, 10 for 1,000) and is returned as `Resource`; every other error, `Cancelled` included,
  propagates at once; and it acts or advances only after `Ok(RetryTextPage)`. `Resource` itself does not retire a snapshot;
  the existing retirement rules still apply (expiry, a panic of the read operation, a fresh fatal or native SQL error, and
  a disk fault, below), and a later call on a retired snapshot fails with `Store(Backend)`. The signatures above do not
  change. The producer's public matrix shows the policy on a test-side loop on all four registrations
  (`all_adapters_limit_1000_large_inline_bounded_resource_recovery`: 1,001 inline rows, SQLite serves a page of 125;
  `all_adapters_resource_recovery_discards_provisional_page_and_stops_at_one`: a sink `Resource` after bytes, 8 then 4, and
  a terminal `Resource` at limit 1 after 1,000 down to 1). No production consumer implements the policy yet, and the
  consumer's own proof and the controller's acceptance are owed.
- **Cancellation.** The adapter derives a child token of `cancellation`, holds its drop guard for the whole await,
  checks it before every read, before every callback and before it returns success, and keeps the snapshot and the
  admission charged until its worker has exited. A latched sink error is never turned into success.
- **Blob capability and refusal.** The three `BlobStore` methods have provided bodies that return
  `RetryStreamError::Store(StoreError::Backend(..))` whose message is `blob store does not support bounded streaming`.
  A store that implements only `put`, `get` and `remove` therefore compiles unchanged and refuses the stream; the stream
  never falls back to the whole `get`. `stream_len` must not fetch payload. `read_windowed` fills the caller's admitted
  window (1 to 65,536 bytes), checks cancellation before every read, before every callback and before it returns, and
  verifies the object's hash and length at its end. `with_stream_hasher` lends stack-owned incremental state for the
  whole-value hash, so no boxed hasher and no whole-value buffer is allocated. `orch_infra::blob::DiskBlobStore`
  implements all three; the memory fake in its disk-backed form calls whichever store it was given, so with a `DiskBlobStore`
it reads through that one.
- **Errors on a stored object.** With `DiskBlobStore`, a selected object that is missing, is a symbolic link, is not a
  regular file, has the wrong length or has the wrong hash is `Corrupt`; any other I/O error on it is
  `Store(Backend)` whose text is the constant `retry object access failed`. Neither carries a path, a hash or text.
  That is the disk helper's own error, and the file reference and the disk-backed fake return it as it is. On SQLite the
  same I/O error is also an observation of the snapshot's reader, and the reply the caller sees differs (no signature
  changes):
  - *A native I/O fault on a stored object* (any I/O kind except `NotFound`, which is `Corrupt`, raised while the bounded
    read opens the object, asks for its length or reads a window) is raised to the snapshot as a SQLite I/O error. The
    snapshot classifies it with the failure policy of the legacy retry reads (an ordinary, transient class that latches
    nothing), retires that snapshot's reader connection and answers the call with `Store(Backend)` whose text is the
    snapshot's ordinary retired-reader text, `SQLite storage operation failed`; if a fatal observation was made since the
    snapshot was requested the text is `storage_unavailable` instead. The disk helper's text above is replaced, not
    passed on. A later call on that snapshot is refused with `Store(Backend)` text `snapshot_expired`, the text of a snapshot
    whose worker has left. A snapshot taken after the fault clears reads the page.
  - *A sink's own error* is returned to the caller as the sink made it, including a `Store(Backend)` of the same class as
    a disk fault. It does not retire the snapshot, and the next read on that snapshot succeeds.
  - The origin is recorded privately and only at the disk operation, for one selected row's operation; the public error
    alone cannot tell the two apart. Cancellation, `Resource`, the `Corrupt` cases and interrupted-read retries are not
    observed and do not retire the snapshot.
  The test that shows reader retirement, recovery on a fresh snapshot and reuse after a sink error asserts the class of
  each error (`Store`) and the sequence of outcomes; the literal texts above are established by the source and by a one-off
  probe recorded in the plan's "Bounded retry text read", not by an assertion of that test.
- **Compatibility.** The whole-value reads of `RetryRead` and `RetryWrite` keep their signatures, orders and errors. The
  trait does not change what is stored: a row is the version-1 document and the chunk mapping of ADR 0020 decision 4.
  No format, schema or setting changes.

Implemented by: `orch_core::store::fake::FakeStorage` (the logical form and the form with a retry blob store), the
`orch_infra::storage::v2::FileStorage` reference and `orch_infra::sqlite::SqliteStorage`, each on its `ReadSnapshot`
type. No production caller exists; the plan's "Bounded retry text read" records what evidence exists and what releases
the first read.

## 3. `orch_core::adapter`

```rust
pub enum AgentEventKind { Output, FileChange, Command, ToolCall, Error, Done }
impl AgentEventKind { fn as_str(); fn to_run_event_type(self) -> RunEventType }  // exact mapping

pub struct AgentEvent { kind, timestamp: String, data: Value, tokens: Option<TokenUsage>, error_kind: Option<AdapterErrorKind> }
pub struct AdapterTestResult { ok: bool, version?, error?, error_kind?, details? }
pub struct ExecuteParams { prompt, system_prompt?, workspace: PathBuf, env: BTreeMap<String,String>, config: AgentConfig, cancel: CancellationToken }
pub struct ExecuteHandle { pid: u32, events: BoxStream<'static, Result<AgentEvent, AdapterError>> }
pub struct AdapterError { message: String, kind: AdapterErrorKind, exit_code: Option<i32> }

pub trait Adapter {
    fn kind(&self) -> &'static str;
    async fn test(&self) -> AdapterTestResult;
    fn execute(&self, ExecuteParams) -> Result<ExecuteHandle, AdapterError>;   // NOT async
    async fn stop(&self, pid: u32) -> Result<()>;
}

pub struct AdapterRegistry;                    // Default + Clone
impl AdapterRegistry {
    fn register(&mut self, Arc<dyn Adapter>);  // replaces by kind, keeps position
    fn get(&self, &str) -> Option<&Arc<dyn Adapter>>;
    fn require(&self, &str) -> Result<&Arc<dyn Adapter>, DomainError>;
    fn has(&self, &str) -> bool;
    fn list(&self) -> Vec<&Arc<dyn Adapter>>;  // registration order (exact)
    fn kinds(&self) -> Vec<&'static str>;      // registration order (exact)
    fn len(&self) -> usize;  fn is_empty(&self) -> bool;
}
```

`execute` is sync on purpose: the orchestrator needs the PID to write
`state.running` before it consumes events, and the lifecycle listeners must be
attached before the stream is polled. Stream errors are `Err` **items**, so the
distinction between "an `error` event occurred" (continue) and "the
generator threw" (run failed) is kept — the thrown case is one `Err` item
followed by the end of the stream.

## 4. `orch_core::process`

```rust
pub enum Signal { Term, Kill, Int, Hup }
pub enum StdioMode { Ignore, Pipe }              // Ignore is the default
pub struct SpawnOptions { cwd: PathBuf, env: BTreeMap<String,String>, stdin: StdioMode }
pub struct ExitOutcome { code: Option<i32>, signal: Option<i32> }  // fn success()
pub struct ExitSignal;                           // Clone; fn wait().await, fn try_get()
pub struct ExitNotifier;                         // fn notify(ExitOutcome) — first call wins
impl ExitSignal { fn channel() -> (ExitNotifier, ExitSignal); fn resolved(ExitOutcome) -> ExitSignal }
pub type ChildReader = Box<dyn AsyncRead + Send + Unpin>;    // Amendment 1
pub type ChildWriter = Box<dyn AsyncWrite + Send + Unpin>;   // Amendment 1
pub struct SpawnedChild { pid: u32, stdout: Option<ChildReader>,
                          stderr: Option<ChildReader>, stdin: Option<ChildWriter>,
                          exit: ExitSignal }
impl SpawnedChild { fn from_tokio(pid, stdout, stderr, stdin, exit) -> SpawnedChild }

pub trait ProcessManager {
    fn is_alive(&self, pid: u32) -> bool;
    fn kill(&self, pid: u32, signal: Signal);
    async fn kill_with_grace(&self, pid: u32, grace: Duration);
    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild>;
}

pub const DEFAULT_KILL_GRACE: Duration;   // 10 s
pub const KILL_POLL_INTERVAL: Duration;   // 200 ms
```

`stdout`/`stderr`/`stdin` are **boxed trait objects** (Amendment 1).
`SpawnedChild::from_tokio` wraps the real `tokio::process::Child*` handles, so
a real implementation is a one-line change from the original shape.

`spawn` does **not** handle cancellation — see the spec issue below.

## 5. `orch_core::workspace`

```rust
pub struct PrepareResult { path: String, branch: Option<String> }
pub enum MergeResult { Success, Conflict { conflict_info: String } }   // fn success(), fn conflict_info()

pub trait WorkspaceManager {
    async fn prepare(&self, &Task, &Agent, &OrchestratorConfig) -> Result<PrepareResult>;
    async fn merge_back(&self, branch: &str) -> Result<MergeResult>;
    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()>;
    fn validate(&self, workspace_path: &Path, project_root: &Path) -> Result<()>;   // sync
    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>>;
}
```

## 6. `orch_core::template`

```rust
pub trait TemplateEngine {
    async fn render(&self, template: &str, context: &PromptContext) -> Result<String>;
}

pub struct PromptContext { project, task, agent, agents, attempt: Option<i64>,
                           workspace_path, retry, feedback, shared_context, messages, goal }
// plus ProjectContext, TaskContext, AgentContext, AgentInfo, RetryContext,
// MessageContext, GoalContext, SharedContextEntry = (String, String)

pub const DEFAULT_RENDER_TIMEOUT_MS: u64 = 5_000;
pub const MAX_CONTEXT_ENTRIES: usize = 15;
```

**WP-3, read this.** `PromptContext` carries **no `skip_serializing_if`** — that
is ARCHITECTURE §4.3 mitigation 1 encoded in the type: every documented
variable is always present, and an absent optional is an explicit `nil`, which
Liquid treats exactly as liquidjs treats `undefined`. `shared_context` is an
**ordered `Vec<(String, String)>`**, which is mitigation 2: it renders
identically through `entry[0]`/`entry[1]` and pins the iteration order. Both
have tests in `template.rs`.

## 7. `orch_core::skills`

```rust
pub trait SkillLoader {
    async fn load_skills(&self, skill_names: &[String]) -> Result<String>;
    async fn list_available(&self) -> Result<Vec<String>>;
}

pub fn is_valid_skill_name(&str) -> bool;                  // ^[a-z0-9-]+$ — traversal defence
pub fn is_mcp_skill(&str) -> bool;                         // contains ':'
pub fn format_skills_block(&[(String, String)]) -> String; // the exact output format, §9.6
```

## 8. `orch_core::clock` / `orch_core::ids`

```rust
pub trait Clock { fn now_ms(&self) -> i64; fn now_iso(&self) -> String; }
pub fn iso_from_epoch_ms(ms: i64) -> String;   // `Date.toISOString()`, verified against node

pub trait IdGenerator { fn generate(&self, prefix: &str) -> String; }
pub fn is_well_formed_id(id: &str, prefix: &str) -> bool;
pub const ID_LENGTH: usize = 7;
pub const ID_ALPHABET_LEN: usize = 64;
```

## 9. `orch_core::test_support` (feature `test-support`)

Take it as `orch-core = { workspace = true, features = ["test-support"] }` under
`[dev-dependencies]` — the skeleton manifests already do.

| Item | Notes |
|---|---|
| `make_task()`, `make_agent()`, `make_run()` | Stock defaults. Override with struct-update syntax: `Task { status: Done, ..make_task() }`. `make_agent()` is `approval_policy: auto`, `stall_timeout_ms: 300_000`. |
| `Recorder<C>` | Replaces `vi.fn()` call assertions: `calls()`, `first()`, `last()`, `len()`, `matching(pred)`, `count_matching(pred)`, `contains(&c)`, `count_of(&c)`, `clear()`. Cloning shares the log. |
| `Faults` | Replaces `mockRejectedValue`: `fail_once`, `fail_next(n, msg)`, `fail_always`, `fail_once_with(DomainError)`, `clear()`, `is_armed()`. Queued failures beat the persistent one. |
| `FakeTaskStore`, `FakeAgentStore`, `FakeRunStore`, `FakeStateStore`, `FakeConfigStore`, `FakeContextStore`, `FakeMessageStore`, `FakeGoalStore`, `FakeTeamStore`, `FakeGlobalConfigStore` | Clone on read **and** write. Public `calls` + `faults`. `seed_*`/`peek_*`/`snapshot()` do not record. `saved()`/`written()` give the write history, which is what the orchestrator tests assert on. |
| `FakeProcessManager` | pid 12345, `is_alive → true`; `set_dead(pid)`, `set_all_dead(bool)`, `with_exit(ExitOutcome)`; `killed()`, `killed_with_grace()`, `spawned()`. **Since Amendment 1 it also drives the child's streams**: `with_stdout(bytes)`, `with_stdout_lines([..])`, `with_stderr(bytes)`, `set_stdout`/`set_stderr`, and `stdin_bytes()` / `stdin_text()` / `stdin_written().await` for what the adapter wrote. `stdin` is `Some` only for `StdioMode::Pipe`, as with the real manager. |
| `FakeAdapter` | `with_events([...])`, `failing_after([...], err)`, `with_kind`, `with_pid`, `set_events`, `set_execute_error`, `set_test_result`; `execute_calls`, `stop_calls`. Helpers `output_event`, `done_event`, `error_event` build canonical `data` shapes. |
| `FakeWorkspaceManager` | `prepare → { path: "/tmp/ws" }`, `merge_back → Success`; `set_prepare_result`, `set_merge_result`, `set_changed_files`; `merged()`, `cleaned()`. |
| `FakeTemplateEngine` | Renders `"rendered prompt"`; `returning(..)`, `set_output`, `contexts()`. |
| `FakeSkillLoader` | `with_skills([(name, content)])`; applies the real rules (MCP skipped, invalid names skipped, missing skipped) and formats through `format_skills_block`. |
| `FixedClock` | Starts at `FIXED_NOW_MS` (`FIXED_NOW_ISO`); `at(ms)`, `advance_ms(delta)`, `set_ms`. |
| `SeqIdGenerator` | `tsk_0000001`, `agt_0000001`, … — one counter per prefix; `starting_at(n)`, `issued(prefix)`. |

**`build_deps` / `TestOrchestrator` are not here.** They need
`OrchestratorDeps`, which lives in `orch-engine`; compose them there from the
parts above. Per ARCHITECTURE §8.1 that builder should default
`poll_interval_ms` to `100_000` so the tick loop never self-fires and tests
drive ticks explicitly.

---

## 10. `[workspace.dependencies]` — library-first set (ARCHITECTURE §4.8)

Verified on 2026-10-13 to resolve on rustc 1.94.1 **and** to `cargo check`
clean in one graph alongside our exact pins (ratatui 0.30.2, crossterm 0.29,
tokio 1.53, liquid 0.26.11 + kstring 2.0.2, nix 0.31, serde_yaml_ng 0.10,
jiff 0.2), with a **single** ratatui and a single crossterm. Declared but
unused, so the owning package does not have to touch the root manifest.

| Crate | Version | For |
|---|---|---|
| `process-wrap` | `10` (`tokio1`) | process groups / session spawn + group kill (WP-2). Keep `nix` for `kill(pid, 0)` liveness. |
| `tui-input` | `0.15` | single-line editing: command bar, `text` wizard steps (WP-6) |
| `unicode-segmentation` | `1.13` | grapheme segmentation, replacing `Intl.Segmenter` (WP-6) |
| `unicode-width` | `0.2` | display width (WP-6) |
| `arboard` | `3.6` | clipboard (WP-3); degrade to "empty" on any failure |
| `semver` | `1.0` | version comparison in the update check (WP-5) |
| `pretty_assertions` | `1.4` | diffs in test failures (dev-only) |
| `proptest` | `1.12` | property tests, if a package wants them (dev-only) |

### `tui-textarea` is rejected — **WP-6 read this**

`tui-textarea` 0.7.0, its latest release, **hard-depends on `ratatui 0.29.0`
and `crossterm 0.28.1`**:

```
tui-textarea 0.7.0 → ratatui 0.29.0, crossterm 0.28.1, unicode-width 0.2.0
```

Adding it puts two incompatible ratatui versions in the graph (measured: the
lockfile resolves `ratatui 0.29.0` *and* `0.30.2`), and a `TextArea` from it
cannot be rendered into a 0.30 `Frame` — the widget traits are different
types. It is therefore **not** in `[workspace.dependencies]`.

`tui-input` 0.15.5 *is* ratatui-0.30 compatible (verified: single ratatui
0.30.2 in the graph) but is **single-line only**. So the multi-line `textarea`
wizard steps have no off-the-shelf option on ratatui 0.30: either wrap
`tui-input` with your own line handling, or hand-roll the editor. §4.8 says to
say which in a code comment — this is the "no maintained crate fits" case, and
it is a finding, not a licence to skip the search.

## Amendments to the frozen surface

### Amendment 1 — `SpawnedChild` stdio is boxed (WP-2)

```rust
// before
pub struct SpawnedChild { pid: u32, stdout: Option<ChildStdout>,
                          stderr: Option<ChildStderr>, stdin: Option<ChildStdin>,
                          exit: ExitSignal }
// after
pub struct SpawnedChild { pid: u32, stdout: Option<ChildReader>,
                          stderr: Option<ChildReader>, stdin: Option<ChildWriter>,
                          exit: ExitSignal }
pub type ChildReader = Box<dyn AsyncRead + Send + Unpin>;
pub type ChildWriter = Box<dyn AsyncWrite + Send + Unpin>;
```

**Why.** `tokio::process::Child{Stdout,Stderr,Stdin}` cannot be constructed
without a process, so `FakeProcessManager` could only ever return `None` for
all three. That made two things untestable with an in-memory double: every
adapter's **event stream** (the parsing is the part under test, and
`FakeAdapter` bypasses it) and the two **stdin protocols** — codex's single
write of the combined prompt, and pi's one JSONL frame with stdin left open.
WP-2 worked around it inside `orch-infra` and the adapter suites do not need
this, but WP-4's orchestrator tests do: they want a real adapter driven by a
fake process.

**What it costs.** One `Box` per pipe per spawned child, and a `Debug` impl
written by hand rather than derived. Nothing else: `AsyncRead`/`AsyncWrite`
are the only things any consumer ever used these for.

**What changes for you.** Build the struct with
`SpawnedChild::from_tokio(pid, stdout, stderr, stdin, exit)`, which takes the
concrete handles and boxes them — a `ProcessManager` implementation is a
one-line change. Anything that *reads* a pipe is unaffected. `orch-infra`'s
`spawn_child` no longer boxes anything itself, and
`orch_infra::process::{ChildReader, ChildWriter}` are re-exports of these.

**Landed** on `wp2` together with the `FakeProcessManager` scripting support
and `crates/orch-infra/tests/fake_process_manager.rs`, which is the pattern
WP-4 should copy.

### Amendment 2 — `Adapter::prepare_effort` and the eco prompt context (token eco mode)

Proposed in [ADR 0010](decisions/0010-prompt-mode-and-effort-contract.md).

```rust
// added to orch_core::adapter
pub struct EffortPreparation { config: AgentConfig, diagnostics: Vec<String> }

pub trait Adapter {
    // kind, test, execute (still NOT async), stop: unchanged
    async fn prepare_effort(&self, params: &ExecuteParams)
        -> std::result::Result<EffortPreparation, AdapterError>;  // default: identity, no I/O
}

// added to orch_core::template
pub struct PromptContext { /* … as above … */, eco: Option<EcoContext> }
// plus EcoContext, EcoRosterRow, EcoRetry, EcoGoalTasks, GoalTaskRow;
// RetryContext.source_run_id, GoalContext.task_rows, TaskContext.depends_on
```

**Why.** OpenCode needs a bounded version and help check before it may
append `#<level>` to a model, and that check spawns processes, so it cannot
run inside synchronous `execute` without hiding work in the event stream.
The eco renderer needs inputs the normal templates never see.

**Rules.** The default method clones `params.config` unchanged and returns no
diagnostics; only OpenCode overrides it. The returned config may differ from
the input only by a cleared `effort`. Callers await it before `execute` and
pass the returned config on; the engine's dispatch is the only production
caller. A hook error goes through the same cleanup as a spawn error.

**Departure from the WP-3 rule.** `PromptContext.eco` is
`#[serde(default, skip_serializing_if = "Option::is_none")]`, and the new
`source_run_id`, `task_rows` and `depends_on` fields are `#[serde(skip)]`, so
the context a normal render sees is the same as before. Every variable the
normal templates use is still always present.

### Amendment 3 — the unit of work stays on the writer's thread

Decided in [ADR 0023](decisions/0023-unit-of-work-on-the-writer-thread.md); the contract and its obligations are the
storage spec's "Amendment 3: the unit of work stays on the writer's thread".

```rust
// before: each of the seven was Send
pub trait UnitOfWork: AttentionWrite + EventWrite + IntentWrite + LeaseWrite
    + RequestKeyWrite + RetryWrite + Send { .. }
pub trait AttentionWrite:  Send { .. }
pub trait EventWrite:      Send { .. }
pub trait IntentWrite:     Send { .. }
pub trait LeaseWrite:      Send { .. }
pub trait RequestKeyWrite: Send { .. }
pub trait RetryWrite:      Send { .. }
// after: none of the seven is Send
pub trait UnitOfWork: AttentionWrite + EventWrite + IntentWrite + LeaseWrite
    + RequestKeyWrite + RetryWrite { .. }
pub trait AttentionWrite  { .. }   // likewise EventWrite, IntentWrite, LeaseWrite,
                                   // RequestKeyWrite and RetryWrite
```

**Not changed.** `WriteWork` (`Box<dyn FnOnce(&mut dyn UnitOfWork) -> Result<()> + Send + 'static>`), `Storage: Send + Sync`,
`StorageExt` (`T: Send + 'static`, `F: … + Send + 'static`), `ReadSnapshot: … + Send + Sync` and every read trait
(`AttentionRead`, `EventRead`, `IntentRead`, `LeaseRead`, `RetryRead`, `RetryTextRead`: `Send + Sync`). No method of any trait changes.
The cursor rules (spec R10, the `Cursor` row and the Task cursor exception of issue 277) are not touched.

**Why.** The SQLite adapter's unit of work borrows the writer's open transaction, which cannot be `Send` (`rusqlite`'s
`Connection` is `Send` and not `Sync`). Nothing needs a unit of work to move between threads: the callback is the owned,
`Send` part of a write, and the unit is only lent to it.

**What it costs.** `dyn UnitOfWork` is no longer `Send`, so `&mut dyn UnitOfWork` is not `Send` either, and a consumer
can never hand the unit it was lent to another thread. That is the one thing the compiler enforces about the borrowed
view. Data borrowed from the unit, such as the `&Caller` that `caller()` returns, can cross threads, so rule 2 below is
kept by review for it. The reference types `Session` and `View` stay `Send` as concrete types; a callback does not see
them.

**Rules for a callback.**

1. It is an owned `Send + 'static` closure, invoked once on the writer when admitted, never replayed, and never
   invoked when the backend refuses admission. A retry by the writer of its own `BEGIN` or `COMMIT` does not invoke it
   again (spec R128, record P); work that must be redone is a new request. The SQLite adapter's `RetainedWrite`
   (`SqliteStorage::retained_write`, outside these traits) is how a producer that kept its input submits that new
   request after a confirmed noncommit; `Storage::write` itself is one attempt and has no retry of the callback. The first
   such producer is the SQLite event producer (`sqlite/events/producer.rs`), which is outside these traits too and changes
   none of them.
2. The `&mut dyn UnitOfWork` it receives, and anything borrowed from it, is used on the thread that runs it, and not
   after it returns. No thread, task or scope receives it.
3. Its body depends on no Tokio runtime: no `Handle::current()`, no `block_on`. `oneshot::Sender::send` needs none.
4. A panic in it is contained by the SQLite adapter: the transaction rolls back, the awaiting caller observes the
   panic, the writer thread survives and latches nothing, and the next write commits. The catch surrounds the
   invocation of the callback and nothing else. A leaf's failed savepoint cleanup is not a callback panic: it is
   observed under record L, operation and cleanup separately, and raised again, and the writer ends: `WriterClosed` and
   no latch when both causes are ordinary, a latch and `storage_unavailable` when either is fatal (spec "Amendment 3",
   records N and O).

**What changes for you.** Nothing at a call site that compiled before. Code that needs `UnitOfWork: Send` stops
compiling and needs a design review, not the bound back. The workspace compile check that the PR carrying the
seven-bound diff must pass is what shows that nothing on `main` is such code. That diff is the only source change.

---

## Deviations from ARCHITECTURE §5, and why

1. **One `Error` type across the trait surface** instead of a per-module
   infrastructure enum (§5.7). Five packages share one `Result`, so nobody
   invents conversions, and the exit-code semantics are unchanged (`Domain`
   keeps its code, everything else is 1).
   The `source` chain is kept for `ORCHESTRY_DEBUG`.
2. **`AdapterRegistry` is an `IndexMap`, not a `Vec`** (§5.4). It is keyed by
   `kind`: re-registering a kind **replaces** it while `kinds()` still yields
   registration order. A `Vec` would duplicate.
   Order is still exact — `doctor` probes in it.
3. **`ProcessManager::kill` takes `orch_core::process::Signal`, not
   `nix::sys::signal::Signal`** (§5.5). Keeps `orch-core` platform-free;
   `orch-infra` maps it behind a `cfg(unix)` boundary (A3).
4. **`SpawnedChild::exit` is an `ExitSignal`, not a `BoxFuture`** (§5.5). §5.5
   itself requires an outcome that can be awaited more than once; a
   `BoxFuture` is consumed on first poll. `ExitSignal` is a `watch` receiver
   with `wait()`/`try_get()`.
5. **A 10th storage trait, `GlobalConfigStore`.** The TUI persists the palette and the activity filter;
   without a seam, WP-6's tests would write to the developer's real
   `~/.orchestry/`.
6. **`orch-domain` depends on `indexmap` and `serde_json`**, not only
   serde/thiserror (§3.1). `serde_json::Value` is unavoidable —
   `RunEvent.data` is free-form JSON, and the tolerant `state.json` read and the
   config deep-merge are value-tree operations. `indexmap` gives insertion
   order for `OrchestratorState.{claimed,running}` and
   `AgentConfig.env`; both are pure, dependency-light and no-I/O, so the
   "trivially testable, fast to compile" property is intact.
7. **`iso_from_epoch_ms` and `format_skills_block` live in `orch-core`**, not in
   `orch-infra`. Both are exact formats with a test vector; one implementation
   shared by the real component and the fake beats two.
8. **The three binaries are three one-line shims over `orch_cli::run()`**, not
   three `[[bin]]` targets sharing one `main.rs` (§3.2). Cargo warns on every
   build about the shared path and compiles it three times; §3.2 already
   offered this as the fallback.
9. **`ContextEntry` lives in `orch_core::storage`** rather than in
   `orch-domain`, whose module list in §3.3 has no `context`.

## Fakes implement real behaviour

The fakes do not stub these to empty returns, because a fake that lies costs
more than it saves. Do not assert on empty returns.

- `FakeRunStore::list_for_task` / `list_for_agent` — real filtering.
- `FakeRunStore::read_events_tail` — real tail of the stored events.
- `FakeRunStore::stream_events` — yields the stored events and ends (empty for
  an already-cancelled token).
- `FakeMessageStore::list` / `list_pending` — real sorting and filtering, minus
  the TTL-expiry check, which needs a clock. `purge_expired` returns 0 for the
  same reason.
- `FakeContextStore` — real key-ascending `list`, real `created_at`
  preservation across overwrites; entries never expire.
- `FakeProcessManager` — since Amendment 1, real in-memory pipes: scripted
  stdout/stderr and a captured stdin, so a real adapter can be driven by a
  fake process.

## YAML emission evidence

`crates/orch-domain/tests/yaml_emission.rs` pins what the frozen types actually
emit through `serde_yaml_ng`:

- **Every** timestamp field across `Task`, `Goal` and `Message` (`created_at`,
  `updated_at`, `expires_at`, `delivered_at`) comes out **unquoted**, so a YAML 1.1
  reader re-parses it as a date and `MessageStore.list()` fails. The guard is
  genuinely required, and the test names the exact field set it
  must cover.
- A user-supplied `description: 2026-10-13` is the same hazard, which is why
  the guard keys off the scalar grammar, not field names.
- `yes`/`no`/`on`/`off`/`true`/`false`/`null`/`123`/`1.5` as string values
  already round-trip correctly; the guard does **not** need to widen to them.
- `AgentConfig.env` round-trips in insertion order, and YAML key order matches
  struct declaration order.

`crates/orch-domain/tests/yaml_emission.rs` also pins the **merge-key
bifurcation**, measured not assumed:

- `serde_yaml_ng` leaves `<<` as a **literal key**: `scheduling.max_turns` and
  `scheduling.adapter` are simply **absent** from the parse.
- `Value::apply_merge()` resolves them (99 / `codex` / 5).
- `apply_merge()` is a **no-op** on every document r-orch itself writes, so WP-1
  can call it unconditionally on every read rather than sniffing for `<<`.


## Behaviour notes

Findings from the early work that still shape the code. Each is pinned by a test.

1. **`AgentEventKind` is not `RunEventType`.** The mapping is `AgentEventKind::to_run_event_type`.
2. **Not every `?` field is `skip_serializing_if`.** The rule holds for persisted records (with
   `Message.to_agent_id` as the sole exception) but must **not** apply to `PromptContext`, where ARCHITECTURE §4.3
   requires the opposite.
3. **`scope` is implemented over UTF-16 code units.** A Rust `String` cannot hold a lone surrogate, so its test
   exercises the radix-split-inside-a-surrogate-pair path with U+1F600 / U+1F601, which share the high surrogate
   `0xD83D`.
4. **`crates/orch-domain/tests/shop_skill_references.rs`** checks that every skill a shop role names exists in
   `skills/library/`, and that the `agt_creator` role prompt advertises exactly the skills that exist.
5. **Cancellation is part of the adapter spawn contract.** A cancelled run whose child is still alive would
   await `ExitSignal` **forever** unless something signals the child. `process/spawn.rs::kill_on_cancel` in
   `orch-infra`'s shared adapter spawn path sends it, so every adapter inherits it and a new adapter cannot
   forget it. The signal goes through `ProcessManager::kill`, which tries the process **group** first. **The
   orchestrator's grace-period kills are still required:** this path sends one `SIGTERM` and never escalates to
   `SIGKILL`.
6. **`task edit` and `agent edit` take no options.** Each serializes the entity into a `---` frontmatter temp
   file, spawns `$EDITOR` (then `$VISUAL`, then `vi`) on it, and parses what comes back, writing only the fields
   that changed. Nothing edited prints "No changes."; a non-zero editor exit reports `Editor exited with code N`.
   Tests reach both by setting `$EDITOR` to a script they write into a temp directory.
7. **`msg send` truncates by UTF-16 code units, and the file stays readable.** Subject is capped at 200 and body
   at 4000 code units. A two-unit character cut between its surrogates would store a lone surrogate, which
   `serde_json` cannot parse. The write side therefore truncates at the last whole character within the cap, so
   a message may be one unit shorter than the cap. The read side, `fs_utils::read_json`, repairs unpaired
   surrogate escapes to U+FFFD for **every** `.json` store, never fails a listing, and leaves the file alone; a
   re-save for another reason (drain marks delivered) may write the replacement character. A valid pair is
   passed through untouched. Pinned by the `message_service.rs` truncation tests and the
   `fs_utils::repair_lone_surrogates` unit tests, including a lone surrogate preceding a valid pair and the
   escaped-backslash case `\\ud83d`, which must not be read as an escape.
8. **`msg broadcast --team <nonexistent>` fans out to every agent** and persists the unknown `team_id` on each
   message. An implementation that validates the team id sends strictly fewer messages; changing this is a
   contract change.
9. **A message body is validated trimmed and stored untrimmed.** `'   '` is an error while `' padded '` is
   accepted and keeps both spaces on disk. The subject is neither trimmed nor validated: empty becomes the
   literal `(no subject)`.
10. **`review` is not terminal.** `serve --once` waits for every task to be terminal, and a task in `review` is
    not, because a task without `review_criteria` is auto-approved only when its agent's `approval_policy` is
    `auto`. A test that dispatches to agents without that policy needs `task approve` steps or auto-policy
    agents.

11. **`Orchestrator::stop()` aborted the task it was running in, so `serve`
    and `run --watch` ignored Ctrl+C.** `stop()` takes the stored watch-loop
    `JoinHandle` and aborts it — but both the signal arms and the tick circuit
    breaker call `stop()` **from inside that very task**, so it cancelled
    itself at the next await that actually pends. In the real binary that is
    the state save, which happens *before* the lock release and before
    `stopped.send(true)`, so `wait_for_stop()` never returned: `orch run
    --watch` and `orch serve` printed `Shutting down…` and then sat there
    until something killed them, leaving `.orchestry/orchestry.lock` behind.

    Invisible to the orchestrator's own tests, which is why it survived: with
    the in-memory fakes nothing inside `stop()` ever pends, so the task runs
    to completion even while cancelled, and `tests/resilience.rs` drives
    `handle_signal` from the *test* task rather than from the loop.

    Fixed by not aborting the watch task at all: `shutdown.cancel()` already
    stops future ticks, and the arm that called `stop()` breaks
    immediately after. The abort was cancelling the *tick* as well, and a
    tick cancelled between `atomic_write`'s temp file and its rename left
    `.orchestry/.state.json.<hash>.tmp` behind.

    Covered end to end by `crates/orch-cli/tests/watch_shutdown.rs`, at the
    only level where it is real: SIGINT to `run --watch`, SIGTERM to `serve`,
    both asserting the process exits and the lock file is gone.

12. **`kill(pid 0)` signals the caller's own process group.** Both
    `kill(-0)` and `kill(0)` do, in C and in node, so a stale or absent pid
    in `state.json` would have had the orchestrator SIGTERM itself along with
    every agent it started. `SystemProcessManager::kill` refuses pid 0 and
    `is_alive(0)` is false. A process that kills itself because a pid field was missing
    is not behaviour, and nothing observable depends on it. The test can only
    fail by killing the test runner.

13. **`orch tui` outlived its own terminal, at 100 % of a core.** Closing the
    terminal sends SIGHUP, which `crates/orch-cli/src/tui/command.rs` catches
    and turns into the `stop` flag the draw loop checks once a frame — so
    far, correct. But the loop was waiting for input in
    `crossterm::event::poll`, and crossterm's unix event source answers both
    `poll` and `read` by looping on the tty fd: a read of **0 bytes**, or an
    error that is neither `WouldBlock` nor `Interrupted`, neither produces an
    event nor breaks that loop (crossterm-0.29.0
    `src/event/source/unix/mio.rs`, the `TTY_TOKEN` arm). On a hung-up pty the
    fd is readable forever and every read is one of those two, so `poll` never
    returned at all. Measured: the `orch-tui` thread pinned one core at 101 %,
    the process re-parented to init, `.orchestry/orchestry.lock` still held
    and the agents it started orphaned — with the SIGHUP already recorded in a
    flag nobody would read again.

    Fixed by doing the *waiting* ourselves: `crates/orch-tui/src/tty.rs` is a
    `poll(2)` that reports `POLLHUP`, and crossterm is entered only once the
    terminal has data and has not hung up. Reading and parsing stay
    crossterm's, per ARCHITECTURE §4.8. The same check runs before every
    re-entry into crossterm inside the per-frame drain, and after a failed
    ioctl or frame write, so a terminal that goes away mid-frame is reported
    as the hangup it is rather than as a dashboard error.

    Two second-order details the fix has to carry. `POLLNVAL` means "fd not
    open", and which fd it is decides what that implies. The poll runs on
    **stdin** whenever stdin is a terminal — every terminal, every tmux pane,
    and `orch web`'s pty child — and on `/dev/tty` only when it is not,
    mirroring crossterm's own `tty_fd`. On stdin it means the terminal is gone,
    so it is treated as a hang-up. On `/dev/tty` it may instead be the
    documented macOS limitation that `poll(2)` does not work on that file, so
    only there does detection turn off for the session and the wait revert to
    crossterm's — the behaviour being replaced, not a new way to fail. The
    split is what keeps dead-tty detection working on the path a macOS user
    actually takes. And ratatui's `Terminal::drop`
    retries `show_cursor` and `eprintln!`s the failure, which on a closed
    stderr *panics*: that turned a closed terminal into "the dashboard
    panicked" and a non-zero exit, so `TerminalGuard::drop` skips that drop
    once it knows the terminal is gone.

    Invisible to every test above the binary: `TestBackend` never goes near
    crossterm's event source and a pipe has no hangup semantics. Covered by
    `crates/orch-cli/tests/tui_hangup.rs`, which runs the real binary on a
    real pty, lets it park in its wait, closes both master fds, and asserts it
    exits with status 0 and no lock left behind. Reverting
    `crates/orch-tui/src/runner.rs` fails it with "outlived its terminal by
    10s and had to be killed".

    `orch web` needed no separate fix: its pty child *is* `orch tui`, and its
    teardown (`crates/orch-cli/src/web/pty.rs`) sends SIGHUP while a reader
    thread still holds a dup of the master, so the child's tty never hangs up
    before it exits and the flag path was always reached. That path is now
    pinned by a test of its own in the same file.
