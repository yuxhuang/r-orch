# Frozen trait surface (end of WP-0)

Quick index of everything `orch-core` exports, for the five packages compiling
against it. Changing anything here needs an explicit amendment — it breaks
`orch-infra`, `orch-engine`, `orch-cli` and `orch-tui` at once.

Everything returns `orch_core::Result<T>` = `Result<T, orch_core::Error>` unless
stated otherwise. Every trait is `Send + Sync` and takes `&self`, never
`&mut self`: stores are shared as `Arc<dyn …>` and own their interior
mutability, exactly like the shared TS class instances.

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
**raw merged value**, not through `OrchestratorConfig`. The TS store deep-merges
plain objects, so unknown keys in `config.yml` survive a `config set`. Use
`orch_domain::{deep_merge, get_by_path, set_by_path}` on a
`serde_json::Value`, and deserialize the typed struct only for consumers.
`FakeConfigStore` does exactly this and has the test for it.

## 3. `orch_core::adapter`

```rust
pub enum AgentEventKind { Output, FileChange, Command, ToolCall, Error, Done }
impl AgentEventKind { fn as_str(); fn to_run_event_type(self) -> RunEventType }  // L0 mapping

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
    fn list(&self) -> Vec<&Arc<dyn Adapter>>;  // registration order — L0
    fn kinds(&self) -> Vec<&'static str>;      // registration order — L0
    fn len(&self) -> usize;  fn is_empty(&self) -> bool;
}
```

`execute` is sync on purpose: the orchestrator needs the PID to write
`state.running` before it consumes events, and the lifecycle listeners must be
attached before the stream is polled. Stream errors are `Err` **items**, so the
TS distinction between "an `error` event occurred" (continue) and "the
generator threw" (run failed) survives — the thrown case is one `Err` item
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
pub struct SpawnedChild { pid: u32, stdout, stderr, stdin, exit: ExitSignal }

pub trait ProcessManager {
    fn is_alive(&self, pid: u32) -> bool;
    fn kill(&self, pid: u32, signal: Signal);
    async fn kill_with_grace(&self, pid: u32, grace: Duration);
    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild>;
}

pub const DEFAULT_KILL_GRACE: Duration;   // 10 s, the TS default
pub const KILL_POLL_INTERVAL: Duration;   // 200 ms
```

`stdout`/`stderr`/`stdin` are `tokio::process::Child*` handles.

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
pub fn format_skills_block(&[(String, String)]) -> String; // the L0 output format, §9.6
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
| `make_task()`, `make_agent()`, `make_run()` | The TS `helpers.ts` defaults exactly. Override with struct-update syntax: `Task { status: Done, ..make_task() }`. `make_agent()` is `approval_policy: auto`, `stall_timeout_ms: 300_000`. |
| `Recorder<C>` | Replaces `vi.fn()` call assertions: `calls()`, `first()`, `last()`, `len()`, `matching(pred)`, `count_matching(pred)`, `contains(&c)`, `count_of(&c)`, `clear()`. Cloning shares the log. |
| `Faults` | Replaces `mockRejectedValue`: `fail_once`, `fail_next(n, msg)`, `fail_always`, `fail_once_with(DomainError)`, `clear()`, `is_armed()`. Queued failures beat the persistent one. |
| `FakeTaskStore`, `FakeAgentStore`, `FakeRunStore`, `FakeStateStore`, `FakeConfigStore`, `FakeContextStore`, `FakeMessageStore`, `FakeGoalStore`, `FakeTeamStore`, `FakeGlobalConfigStore` | Clone on read **and** write. Public `calls` + `faults`. `seed_*`/`peek_*`/`snapshot()` do not record. `saved()`/`written()` give the write history, which is what the orchestrator tests assert on. |
| `FakeProcessManager` | pid 12345, `is_alive → true`; `set_dead(pid)`, `set_all_dead(bool)`, `with_exit(ExitOutcome)`; `killed()`, `killed_with_grace()`, `spawned()`. `stdout`/`stderr`/`stdin` are `None` — real handles cannot be fabricated without a process. |
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

## Deviations from ARCHITECTURE §5, and why

1. **One `Error` type across the trait surface** instead of a per-module
   infrastructure enum (§5.7). Five packages share one `Result`, so nobody
   invents conversions, and the exit-code semantics are unchanged (`Domain`
   keeps its code, everything else is 1 — which is what the TS catch-all does).
   The `source` chain is kept for `ORCHESTRY_DEBUG`.
2. **`AdapterRegistry` is an `IndexMap`, not a `Vec`** (§5.4). The TS registry
   is a `Map` keyed by `kind`: re-registering a kind **replaces** it while
   `listKinds()` still yields registration order. A `Vec` would duplicate.
   Order is still L0 — `doctor` probes in it.
3. **`ProcessManager::kill` takes `orch_core::process::Signal`, not
   `nix::sys::signal::Signal`** (§5.5). Keeps `orch-core` platform-free;
   `orch-infra` maps it behind a `cfg(unix)` boundary (A3).
4. **`SpawnedChild::exit` is an `ExitSignal`, not a `BoxFuture`** (§5.5). §5.5
   itself requires an outcome that can be awaited more than once; a
   `BoxFuture` is consumed on first poll. `ExitSignal` is a `watch` receiver
   with `wait()`/`try_get()`.
5. **A 10th storage trait, `GlobalConfigStore`.** The TS has a concrete class
   and no interface, but the TUI persists the palette and the activity filter;
   without a seam, WP-6's tests would write to the developer's real
   `~/.orchestry/`.
6. **`orch-domain` depends on `indexmap` and `serde_json`**, not only
   serde/thiserror (§3.1). `serde_json::Value` is unavoidable —
   `RunEvent.data` is TS `unknown`, and the tolerant `state.json` read and the
   config deep-merge are value-tree operations. `indexmap` gives JS `Set`/object
   iteration order for `OrchestratorState.{claimed,running}` and
   `AgentConfig.env`; both are pure, dependency-light and no-I/O, so the
   "trivially testable, fast to compile" property is intact.
7. **`iso_from_epoch_ms` and `format_skills_block` live in `orch-core`**, not in
   `orch-infra`. Both are L0 formats with a test vector; one implementation
   shared by the real component and the fake beats two.
8. **The three binaries are three one-line shims over `orch_cli::run()`**, not
   three `[[bin]]` targets sharing one `main.rs` (§3.2). Cargo warns on every
   build about the shared path and compiles it three times; §3.2 already
   offered this as the fallback.
9. **`ContextEntry` lives in `orch_core::storage`**, mirroring its TS home
   (`storage/interfaces.ts`) rather than moving into `orch-domain`, whose
   module list in §3.3 has no `context`.

## Fakes that are better than their TS counterparts

The TS mocks stub these to `[]`; the Rust fakes implement the real behaviour,
because a fake that lies costs more than it saves. **WP-4: do not assert on the
old empty returns.**

- `FakeRunStore::list_for_task` / `list_for_agent` — real filtering.
- `FakeRunStore::read_events_tail` — real tail of the stored events.
- `FakeRunStore::stream_events` — yields the stored events and ends (empty for
  an already-cancelled token).
- `FakeMessageStore::list` / `list_pending` — real sorting and filtering, minus
  the TTL-expiry check, which needs a clock. `purge_expired` returns 0 for the
  same reason.
- `FakeContextStore` — real key-ascending `list`, real `created_at`
  preservation across overwrites; entries never expire.

## Evidence handed to WP-1

`crates/orch-domain/tests/yaml_emission.rs` pins what the frozen types actually
emit through `serde_yaml_ng` — the "before" half of V8/R1:

- **Every** timestamp field across `Task`, `Goal` and `Message` (`created_at`,
  `updated_at`, `expires_at`, `delivered_at`) comes out **unquoted**, so the
  TypeScript binary re-parses it as a `Date` and `MessageStore.list()` throws.
  The guard is genuinely required, and the test names the exact field set it
  must cover.
- A user-supplied `description: 2026-10-13` is the same hazard, which is why
  the guard keys off the scalar grammar, not field names.
- `yes`/`no`/`on`/`off`/`true`/`false`/`null`/`123`/`1.5` as string values
  already round-trip correctly; the guard does **not** need to widen to them.
- `AgentConfig.env` round-trips in insertion order, and YAML key order matches
  the TS interface declaration order.

## Test port status for `test/unit/domain/**`

`cargo test -p orch-domain -p orch-core` is green: **204** domain + **136**
core unit cases, plus 12 integration cases (4 catalogue-parity, 8 YAML
emission).

Everything in `test/unit/domain/**` that is domain-pure is ported. The
remainder belongs to other packages, because the TS file reaches outside the
domain layer:

| TS file | Not ported here | Owner |
|---|---|---|
| `run-token-usage.test.ts` | the `extractTokens` half (`infrastructure/adapters/utils.ts`) | WP-2 |
| `agent-shop.test.ts` | `getShopWizardSteps`, `applyShopTemplate`, `agentWizardToInput` (`src/tui/wizardConfigs.ts`) | WP-6 |
| `agent-shop-skills.test.ts` | the library-file integrity half (needs `skills/library/` on disk) | WP-3 |

V9 (exhaustive 7×7 transition table), V10 (retry arithmetic, overflow
included), V12 (`ScopeIndex` ≡ pairwise over a fixed corpus), V13
(`classifyAdapterError` vectors), V28 (catalogue vs the TS source, via
`fixtures/shop/`) and V29 (id shape) all have tests.

## Spec issues found while porting

1. **`PARITY.md §4.3` points at "the `state.json` schema in §6.5"**; the state
   schema is **§6.6** (§6.5 is `config.yml`). Cosmetic.
2. **`PARITY.md §6.4` says every `?` field needs
   `skip_serializing_if`, with `Message.to_agent_id` as the sole exception.**
   True for persisted records, but it must **not** apply to `PromptContext`,
   where ARCHITECTURE §4.3 requires the opposite. The two documents do not
   contradict each other, but the rule reads as universal. Flagged so WP-3 does
   not "fix" `PromptContext`.
3. **`scope.test.ts` has one case that cannot be ported as written.** It builds
   strings containing a **lone UTF-16 high surrogate** (`'\ud83d' + 'x'`),
   which a Rust `String` cannot hold. The ported test exercises the same
   radix-split-inside-a-surrogate-pair path with U+1F600 / U+1F601, which share
   the high surrogate `0xD83D` and are valid UTF-8. The trie is implemented
   over UTF-16 code units as the TS one is, so behaviour is identical for every
   input Rust can represent.
4. **`ARCHITECTURE §5.4`'s `AgentEventKind` naming differs from
   `RunEventType`** and the doc does not say so. The mapping is in PARITY §8.1
   and is now `AgentEventKind::to_run_event_type`.
5. **Nobody owns `skills/library/`.** PARITY §9.6 requires the 26 `.md` files
   to ship with the binary, byte-identical to
   `parity/orch-ts/skills/library/`, and `agent-shop-skills.test.ts` asserts
   every non-MCP template skill resolves against it. WP-3 owns the loader but
   the asset copy is unassigned.
6. **`fixtures/shop/` was unassigned**, so WP-0 created it (plus the two
   generator scripts and a README) to make V28 a real test rather than a
   transcription. `fixtures/{prompts,adapters,yaml}` remain WP-2/WP-3's.
