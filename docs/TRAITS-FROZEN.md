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

**Three rules every implementation owes the contract**, all added by the
amendments and all replicated by the fakes — so a fake-backed test will *not*
catch a store that skips one:

1. **`save` removes by id, then pushes to the end of `_index.json`**
   (PARITY.md §6.10). Index order is *last-write* order. It is observable:
   `findBestAgent` breaks score ties by position in `AgentStore::list()`, and
   §14.4 was **reversed to REPLICATE**, so an in-place update that preserves
   position silently changes which agent runs a task. Applies to the five
   index-backed stores (task, agent, goal, context, message); `TeamStore` and
   `RunStore` have no index and scan their directories, so their order is
   filesystem enumeration order and is **not** contract.
2. **Sorts over index order must be stable.** `slice::sort_unstable`,
   `sort_unstable_by` and `sort_unstable_by_key` are **denied workspace-wide**
   in `clippy.toml` with that reason attached; the lint is verified to fire.
   Override only with an `#[allow]` and a one-line justification.
3. **Every YAML read must call `Value::apply_merge()`.** js-yaml resolves `<<`;
   `serde_yaml_ng` leaves it as a literal `"<<"` key (PARITY.md §6.4.2, V38).
   Forgetting it makes a hand-authored `config.yml` silently fall back to
   `DEFAULT_CONFIG` for everything the merge supplied — while both trees still
   compare equal under the Rust parser. See §Evidence below for the
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

pub const DEFAULT_KILL_GRACE: Duration;   // 10 s, the TS default
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
| `arboard` | `3.6` | clipboard (WP-3); degrade to "empty" on any failure, as the TS does |
| `semver` | `1.0` | version comparison in the update check (WP-5) |
| `similar`, `pretty_assertions` | `3.2`, `1.4` | diffs in the harness and test failures (dev-only) |
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
- `FakeProcessManager` — since Amendment 1, real in-memory pipes: scripted
  stdout/stderr and a captured stdin, so a real adapter can be driven by a
  fake process. The TS mock passes `{} as any` for the child and can drive
  nothing.

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

`crates/orch-domain/tests/yaml_emission.rs` also pins the **merge-key
bifurcation** (PARITY.md §6.4.2, V38), measured not assumed:

- `serde_yaml_ng` leaves `<<` as a **literal key**: `scheduling.max_turns` and
  `scheduling.adapter` are simply **absent** from the parse.
- `Value::apply_merge()` reproduces js-yaml's result exactly (99 / `codex` / 5).
- `apply_merge()` is a **no-op** on every document ORCH itself writes, so WP-1
  can call it unconditionally on every read rather than sniffing for `<<`.

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
| `agent-shop-skills.test.ts` | *(nothing — now ported in full)* | — |

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
5. ~~**Nobody owns `skills/library/`.**~~ **Closed** — the team lead shipped
   the 26 `.md` files in `60a259f`, verified byte-identical to
   `parity/orch-ts/skills/library/`.
   `crates/orch-domain/tests/shop_skill_references.rs` now runs the
   reference-integrity half of `agent-shop-skills.test.ts`, and additionally
   cross-checks that the `agt_creator` role prompt advertises exactly the 26
   skills that exist — a check the TS suite does not have.
6. **`fixtures/shop/` was unassigned**, so WP-0 created it (plus the two
   generator scripts and a README) to make V28 a real test rather than a
   transcription. `fixtures/{prompts,adapters,yaml}` remain WP-2/WP-3's.
7. **Cancellation is missing from the adapter spawn contract — WP-4 read
   this.** Every TS adapter passes `signal: params.signal` to
   `ProcessManager.spawn`, which makes **node itself** send the child a
   `SIGTERM` when the `AbortController` fires. Neither PARITY.md §8.2 nor
   §8.4 mentions it, and `docs/parity/adapters.md` §4.1's table of kill call
   sites omits it too — it is only visible in the `spawn` options of all eight
   adapters. It matters twice over:

   - a `CancellationToken` does nothing of the kind, so a cancelled run whose
     child is still alive would await `ExitSignal` **forever**; and
   - it means cancelling reaps the agent process even on a path where the
     orchestrator does not itself call `kill_with_grace`.

   Reproduced in `orch-infra`'s shared adapter spawn path
   (`process/spawn.rs::kill_on_cancel`), so every adapter inherits it and a
   new adapter cannot forget it. The signal goes through
   `ProcessManager::kill`, which tries the process **group** first; node kills
   only the pid, and the group is what the orchestrator's own grace kill would
   reach a moment later anyway. **The orchestrator's grace-period kills are
   still required** — this path sends one `SIGTERM` and never escalates to
   `SIGKILL`.

8. **`PARITY.md §3` says a bare command group prints its help to stdout — it
   prints to stderr.** Measured against the vendored oracle in a hermetic
   tmpdir, for three groups:

   | argv | stdout | stderr | exit |
   |---|---|---|---|
   | `orch task` | 0 B | 735 B | 1 |
   | `orch agent` | 0 B | 654 B | 1 |
   | `orch config` | 0 B | 407 B | 1 |

   The first stderr line is `Usage: orchestry task [options] [command]`. This
   is Commander's `help({ error: true })` path, which is what a subcommand
   without an `.action()` falls into. The exit code in §3 is right; only the
   stream is wrong. `fixtures/scenarios/cmd-exit-codes-groups.toml` encodes the
   measured behaviour, and `harness.rs::is_group_help` documents it where the
   lint needs to recognise it. **WP-5: write to stderr.**

9. **`config global get` cannot read a nested key, and that is L0.** In the
   same tree, `config global show --json` prints
   `{"tui":{"palette":"amber",…}}` while `config global get tui.palette --json`
   prints `{"key":"tui.palette"}` — no `value` at all, exit 0. `config global
   get tui` is likewise empty. The project-level `config get` *does* walk dot
   paths (`config get defaults.agent.max_turns` → `{"key":…,"value":50}`), so
   the two commands differ. Replicated as measured in
   `fixtures/scenarios/cmd-config.toml`: the CLI contract is byte parity, not
   sanity. **WP-5: do not "fix" this.**

10. **`task edit` and `agent edit` take no options whatsoever.** §2.2 lists
    them beside flag-taking verbs, which invites the assumption that they
    accept `--title`/`--description`/`--model`. They do not: each serializes
    the entity into a `---` frontmatter temp file, spawns `$EDITOR` (then
    `$VISUAL`, then `vi`) on it, and parses what comes back, writing only the
    fields that changed. Four scenarios were originally written with invented
    flags and would have asserted nothing; `cargo xtask lint-scenarios` now
    catches that class of mistake, and `fixtures/fake-editors/` makes the two
    commands reachable from a scenario at all. The editor protocol itself is
    L0 surface nobody had enumerated: the frontmatter keys per entity, the
    "No changes." path when nothing is edited, and `Editor exited with code N`
    on a non-zero editor exit — all four are now scenarios.

11. **One `msg send` can write a message file that `serde_json` cannot parse —
    this needs an architect decision.** Measured, and reachable from the CLI
    with a single command.

    `MessageService.send` truncates with JS `.slice()`, which counts **UTF-16
    code units**: `subject.slice(0, 200)` and `body.slice(0, 4000)`. A non-BMP
    character is two code units, so a body of 3999 ASCII characters followed by
    `😀` (4001 code units) is cut **between the surrogates**, and the stored
    value ends in a **lone high surrogate**. What the oracle then does with it
    differs by output path:

    | path | bytes | valid UTF-8 |
    |---|---|---|
    | `.orchestry/messages/<id>.json` | `…aaa\ud83d` (the 6-byte escape) | yes |
    | `msg list --json` on stdout | `…aaa\ud83d` (same escape) | yes |
    | `msg inbox` human output | `…aaa` + `ef bf bd` (U+FFFD) | yes |

    Both renderings are byte-reproducible from Rust only deliberately, because
    a Rust `String` cannot hold a lone surrogate at all. The **read** path is
    the harder half: `serde_json` rejects a lone surrogate escape outright —
    `from_str(r#"{"body":"aaa\ud83d"}"#)` fails with `unexpected end of hex
    escape`, whatever follows it — so our storage layer cannot read back a
    file the oracle wrote. `msg list` would fail entirely rather than diverge
    in one field.

    Three options, none free, hence the escalation:

    - **Model the body as UTF-16.** Byte-exact in every path, and poisons the
      domain type for every consumer.
    - **Replace lone surrogates with U+FFFD on read.** Keeps `String`, matches
      the human path, and breaks byte parity on re-save (the escape becomes
      `ef bf bd`), so a round-trip through r-orch silently rewrites the file.
    - **Declare it L2 above the limit** and record the deviation. Honest, and
      concedes a documented L0 field.

    Not a hypothetical about hostile input: it needs one `msg send` with a long
    body ending in an emoji. The harness now reports a file only one side can
    parse as a failure instead of aborting the run, which it previously did.

    **Ruled 2026-10-13 (option 3, plus a read-side repair).** Write side:
    truncate at the last whole character within the cap — which WP-4's
    `truncate_utf16` already did, so no behaviour changed, and it is now
    covered over every alignment of a two-unit character against both caps.
    L2 above the limit. Read side: `fs_utils::read_json` repairs unpaired
    surrogate escapes to U+FFFD for **every** `.json` store, never failing a
    listing, and leaves the file alone — a re-save for another reason (drain
    marks delivered) may then write the replacement character, which is
    accepted. A valid pair is passed through untouched, so no well-formed file
    changes value.

    Pinned by: `message_service.rs` truncation tests;
    `fs_utils::repair_lone_surrogates` unit tests, including the adjacency
    case where a lone surrogate precedes a valid pair and the escaped-backslash
    case `\\ud83d`, which must not be read as an escape; and
    `cross_compat.rs::ts_written_lone_surrogate_does_not_break_the_rust_listing`,
    where the **oracle's own `MessageStore`** writes the record and r-orch
    lists it. That last one is the real guarantee and was verified to fail
    without the repair — before it, one such message made `msg list` return
    nothing at all rather than one mangled body.

    Scenario split accordingly: `cmd-msg-truncation` keeps the cases that cut
    between characters and is fully compared; `cmd-msg-truncation-mid-pair`
    carries the licensed divergence, with `@exit-only:parity-14-lone-surrogate`
    on its steps and `excluded_tree_paths = ["messages/"]`. Both exclusions are
    named in the file, and a tree exclusion that matches nothing is a failure.

12. **`msg broadcast --team <nonexistent>` fans out to every agent.**
    `message-service.ts` resolves the team and filters only `if (team)`, with
    no else branch, so an unknown team id leaves the recipient list unfiltered.
    Measured in a four-agent tree: `--team <real two-member team>` → 2
    messages, `--team team_nosuch` → 4, and `team_id: "team_nosuch"` is
    persisted on all four, leaving a dangling reference in the store. An
    implementation that validates the team id sends strictly fewer messages
    than the oracle. Replicated in `cmd-msg-broadcast-unknown-team.toml`.

13. **A message body is validated trimmed and stored untrimmed.**
    `send` rejects on `!input.body.trim()` and then stores
    `input.body.slice(0, 4000)` — the original. So `'   '` is an error while
    `' padded '` is accepted and keeps both spaces on disk. The subject is
    neither trimmed nor validated: empty becomes the literal `(no subject)`.
    Trimming before storing passes every rejection test and diverges on every
    message anyone sends with a trailing newline.
    Covered by `cmd-msg-body-trimming.toml`.

14. **The oracle's `run` and `serve --once` do not dispatch anything from the
    CLI — the process exits first.** `orch run <id>` and
    `orch serve --once` print their header and exit **0** having done nothing:
    the task stays `todo`, no run record is written, and
    `.orchestry/orchestry.lock` plus a stale `claimed` entry are left behind.
    Reproduced outside the harness, inside a git repo and outside one, in
    `worktree` and `shared` workspace modes, on node 24.21.0. The same
    invocation under a `node -r <preload>` that holds the event loop open with
    an interval timer completes the task correctly, so every await does
    resolve — the process simply exits before they do.

    The mechanism is `ProcessManager.spawn` (`process-manager.ts:63-77`),
    which `unref()`s every child on the stated assumption that "pipes
    (stdout/stderr) still hold refs while being read", plus the callers that
    never read them —`WorkspaceManager.spawnAndWait`
    (`workspace-manager.ts:171-181`) awaits `close` with no `data` listener.
    Seven lines reproduce it with no ORCH code at all:

    ```js
    const p = spawn('sh', ['-c', 'sleep 1'],
      { stdio: ['ignore', 'pipe', 'pipe'], detached: true });
    p.unref();
    new Promise((r) => p.on('close', r)).then((c) => console.log('close', c));
    // → node exits 0 immediately; `close` never fires
    ```

    The oracle's own vitest suite cannot see this: it drives the orchestrator
    in-process, where vitest keeps the loop alive. `unref()` has these
    semantics on every supported node, so an older runtime is unlikely to
    change it.

    Consequence for the corpus: `v7-run-single-task` and all nine `o7-*`
    scenarios cannot be compared against the oracle as written —
    r-orch does the work and the oracle does nothing, so every artifact
    differs. `v7-run-cross-compat` is unaffected, because there the oracle
    only *reads* what r-orch wrote. Needs a decision: patch the vendored
    oracle (recorded as such), convert those ten to rust-only snapshots, or
    leave them red. Not something r-orch can or should reproduce.

15. **`review` is not terminal, so `serve --once` waits forever for a task
    that lands there.** Both implementations' `waitForAllTerminal` poll until
    every task `isTerminal`, and `review` is not — by design, since a task
    without `review_criteria` is auto-approved only when its agent's
    `approval_policy` is `auto`. `o7-concurrency-cap` dispatches six tasks to
    agents that mostly lack that policy, so it cannot terminate in either
    implementation; it needs `task approve` steps or auto-policy agents. The
    harness now reports a timed-out step as a scenario failure rather than
    aborting the whole run.

16. **`Orchestrator::stop()` aborted the task it was running in, so `serve`
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
    stops future ticks, which is exactly what `clearInterval(this.timer)`
    does in the reference, and the arm that called `stop()` breaks
    immediately after. The abort was cancelling the *tick* as well, and a
    tick cancelled between `atomic_write`'s temp file and its rename left
    `.orchestry/.state.json.<hash>.tmp` behind — litter the reference cannot
    produce, since `clearInterval` cannot interrupt a running callback. That
    one surfaced as a flaky tree difference in `o7-serve-once-exit-codes`.

    Covered end to end by `crates/orch-cli/tests/watch_shutdown.rs`, at the
    only level where it is real: SIGINT to `run --watch`, SIGTERM to `serve`,
    both asserting the process exits and the lock file is gone.

17. **`heap_mb` has no Rust counterpart.** The `orchestrator:tick` log record
    carries `heap_mb = +(process.memoryUsage().heapUsed / 1_048_576)
    .toFixed(1)` (PARITY §16.4). A Rust process has no V8 heap, and reporting
    RSS under that name would be a different metric wearing this one's label,
    so r-orch emits `0.0` — the field stays, because the record *shape* is
    contract — and the harness masks the value the way it masks pids and
    durations (§1.4). Decide in §14 whether the honest zero or an RSS figure
    is wanted; nothing downstream reads it.

18. **The tick stream is a wall-clock heartbeat, not a sequence.** How many
    `orchestrator:tick` records appear before a run finishes, and which tasks
    are running when one fires, differ between two independently-timed
    processes: measured as a `running: 2` tick on one side against
    `running: 1` on the other (`o7-concurrency-cap`), and one extra tick
    before a stall was detected (`o7-stall-detection`). The scenarios that
    compare records therefore carry `@ticks-vary:<reason>`, which drops tick
    records and keeps every other one — dispatch, completion, retries,
    stalls, shutdown. Separately, human output stamped with
    `toLocaleTimeString` (`run`, `run --watch`, `logs`) normalizes to
    `<TIME>`: the two binaries start seconds apart.

19. **`kill(pid 0)` signals the caller's own process group.** Both
    `kill(-0)` and `kill(0)` do, in C and in node, so a stale or absent pid
    in `state.json` would have had the orchestrator SIGTERM itself along with
    every agent it started. `SystemProcessManager::kill` refuses pid 0 and
    `is_alive(0)` is false. Not replicated from the reference, which has the
    same hazard: a process that kills itself because a pid field was missing
    is not behaviour, and nothing observable depends on it. The test can only
    fail by killing the test runner.

20. **`orch tui` outlived its own terminal, at 100 % of a core.** Closing the
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
