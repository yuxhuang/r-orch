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
mutability.

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
