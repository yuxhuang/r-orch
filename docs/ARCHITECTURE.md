# r-orch Architecture

Design for the Rust rewrite of ORCH. Read [PARITY.md](PARITY.md) first — it is the specification; this
document is how we build to it.

---

## 1. Context

ORCH is an AI-agent orchestrator: a CLI, a TUI, and a long-running engine that spawns external AI coding CLIs
(`claude`, `codex`, `cursor`, …) as child processes, streams their JSONL output, drives a task state machine,
and persists everything as plain files under `.orchestry/`. ~26k LOC of TypeScript, layered DDD with hand-rolled
dependency injection, ~2085 tests.

The rewrite target: a Rust implementation that is **drop-in compatible at the `.orchestry/` and CLI boundary**
(either binary can operate on the same project, in either order), idiomatic inside, and better tested —
specifically, deterministic where the original is timing-dependent.

**No distributed system here.** One process per project, guarded by a PID lock file, with a filesystem as the
database. The hard problems are (a) child-process lifecycle and cancellation, (b) exact serialization
compatibility, and (c) not regressing a state machine that has had many bug fixes layered into it.

---

## 2. Assumptions

Load-bearing, stated so they can be challenged:

| # | Assumption | If wrong |
|---|---|---|
| A1 | **Scale is small.** Tens of agents, hundreds of tasks, thousands of run events per run. The TS code's caps (16 KB lines, 8 KB events, 1024-event buffer, 500-entry LRU) are generous. | Nothing in the design changes; the file-per-entity store would need an index rework only at ~10⁵ tasks. |
| A2 | **Single *orchestrator*, multiple concurrent mutators.** The PID lock serializes *orchestrators* only. One-shot commands never take it, and some "reads" write. See A2-detail below. | This is **already true in the reference implementation** — not a hypothetical. The exposure is now advertised, so it must be documented rather than assumed away. Verified by V37. |
| A3 | **POSIX only.** The TS code uses process groups (`kill(-pid)`), `bash -lc`, `rsync`, and `O_APPEND` atomicity. Windows support is already partial at best. | Windows would need a separate process-group strategy (job objects) and no `bash`. Out of scope; keep platform code behind a `cfg` boundary so it can be added. |
| A4 | **The TS binary is available as a test oracle** at `parity/orch-ts/dist/cli.js` with node 24. | The differential harness is the main parity guarantee; without it we fall back to the written spec alone, which is materially weaker. |
| A5 | **Latency budget is human-scale.** Tick interval 10 s; CLI commands should feel instant (<100 ms). The TS container tiering exists purely to keep read-only commands fast. | Rust startup is ~1 ms either way, so the tiering becomes an optional optimization rather than a requirement. |
| A6 | **Prompt text must be byte-exact** because agents are prompt-sensitive and the system prompt is a cache key. | If approximate were acceptable, the Liquid risk (§4.3) would mostly evaporate. It is not acceptable. |
| A7 | **No external Rust consumer of the library crates yet.** | Public API stability is not a constraint; we are free to pick idiomatic signatures. |

### 2.1 A2 in detail — the concurrency story is weaker than "single writer"

An earlier revision of this document asserted "single writer … this is a pre-existing property, not a
regression." That was wrong on both counts, and [PARITY.md §1.1](PARITY.md)'s promise that "both binaries must
operate on the same project directory interchangeably, in either order" converts an accident into an
**advertised feature**, multiplying the exposure. Three facts from the source:

1. **One-shot commands never take the lock.** `src/infrastructure/storage/lock.ts`'s own header: "Only one
   `orch run --watch` can run at a time. One-shot commands do not acquire the lock." So `orch task add` while
   the orchestrator runs is a second mutator — and that is the *normal* workflow, not an edge case.
2. **Some reads write.** `IndexManager.readIndex` falls through to `rebuildIndex` when the index is missing or
   corrupt, and `rebuildIndex` calls `ensureDir` and **writes `_index.json`**
   (`index-manager.ts:79-124`). `orch task list` on a fresh project therefore creates directories and writes
   a file.
3. **The mutex is in-process only.** It is a JavaScript promise chain — invisible to a second node process,
   and certainly to a Rust one.

**Lost-update mechanism, stated plainly.** Two processes interleaving the `_index.json` read-modify-write can
drop entries: `tasks/tsk_X.yml` exists on disk but is absent from the index, so `task list`, `status` and
**dispatch** do not see the task, while `task show tsk_X` does. It self-heals only when the index is deleted or
corrupted into a rebuild.

**This cannot be fixed unilaterally.** Any lock r-orch takes around its index writes is invisible to the TS
binary, so serialization is unachievable while interchangeable operation is a goal. The honest response is to
**document the behaviour and verify that it is what we documented** (V37), not to claim a guarantee we cannot
provide. Two consequences:

- The `--allow-foreign-project` valve (§11, Rollback) stays **default-on** until V32 is green, not merely V8.
- Concurrent mutation while an orchestrator is running is a **known, documented hazard inherited from the
  original**, and belongs in user-facing docs — not only here.

---

## 3. Crate layout

```
r-orch/
├── Cargo.toml                 # workspace root, shared [workspace.dependencies]
├── rust-toolchain.toml        # pin 1.94.1, edition 2024
├── crates/
│   ├── orch-domain/           # pure types + pure functions. NO I/O, NO async.
│   ├── orch-core/             # the port traits (storage, adapter, process, workspace, clock, ids)
│   ├── orch-infra/            # implementations of orch-core traits
│   ├── orch-engine/           # services + orchestrator + event bus + container
│   ├── orch-cli/              # clap command tree, human + JSON rendering, serve mode. bins live here.
│   └── orch-tui/              # ratatui dashboard
├── xtask/                     # dev tooling: parity harness runner, fixture generation
├── tests/                     # workspace-level differential + cross-compat harness
├── fixtures/                  # golden prompts, adapter stdout recordings, scenario definitions
├── skills/library/            # shipped skill markdown (byte-identical to the TS package)
└── parity/orch-ts/            # vendored reference, gitignored
```

### 3.1 Dependency direction

```
            orch-domain                     (no deps beyond serde/thiserror)
                 ▲
                 │
             orch-core                      (traits only; depends on orch-domain)
                 ▲
        ┌────────┴────────┐
    orch-infra ◄───── orch-engine           (engine → infra: assembly + two faithful
        ▲                 ▲                  couplings — see below)
        └────────┬────────┘
                 │
        ┌────────┴────────┐
    orch-cli          orch-tui              (both depend on engine; tui never on cli)
```

**`orch-engine` does depend on `orch-infra`.** An earlier revision of this diagram said "engine depends on
core, NOT on infra", which was wrong. The edge is real and comes from three places:

| Site | What it names | Why it is acceptable |
|---|---|---|
| `container.rs` | every concrete infra type it assembles — stores, adapters, `LiquidTemplateEngine`, `GitWorkspaceManager`, `SystemProcessManager`, `NanoIdGenerator`, `SystemClock` | Assembly has to name implementations somewhere. This is the TS `container.ts` in Rust form. |
| `orchestrator.rs` | `CachedTaskStore`, `CachedAgentStore`, `CachedGoalStore` | Faithful: `orchestrator.ts` likewise constructs the tick-scoped cache wrappers itself (`new CachedTaskStore(deps.taskStore)`). |
| `orchestrator/dispatch.rs` | `DEFAULT_SYSTEM_TEMPLATE`, `DEFAULT_USER_TEMPLATE`, `build_prompt_context`, `BuildPromptOptions` | Faithful: `orchestrator.ts` imports exactly these from `template-engine.ts`. |

**What actually holds is the narrower, more useful property: every *service* takes only `orch-core` traits.**
Concrete types appear at assembly and in the two orchestrator modules where the TypeScript couples the same
way. So `orch-engine` remains unit-testable against `test_support` fakes, and `orch-infra` is still swappable
wholesale — which is what the boundary was for.

If the edge ever needs to be severed, the moves are known: lift `storage::cached` into `orch-core` (it is
generic over the traits already) and the two template constants into `orch-domain`. Neither is worth doing now
— both would diverge from the TS layering for no behavioural gain.

**Enforced invariant: `orch-engine` has zero dependency on `orch-cli` or `orch-tui`.** This mirrors the TS
layering ("the core engine has zero dependencies on CLI/TUI layers").

`xtask crate-graph` asserts the **whole edge set** against a table, not just that one forbidden edge —
including build- and dev-dependencies, since a test-only edge still compiles the crate. A single
forbidden-edge check would pass a workspace in which `orch-infra` had started depending on `orch-engine`,
inverting the layering while satisfying the letter of the rule.

**Why `orch-core` exists as a separate crate from `orch-domain`.** The TS code puts storage interfaces under
`src/infrastructure/storage/interfaces.ts` and the adapter interface under
`src/infrastructure/adapters/interface.ts`, yet `src/application/orchestrator.ts` imports both — so
"application depends on infrastructure interfaces". In Rust that is a dependency cycle unless the traits live
in their own crate. `orch-core` holds **only** trait definitions and the types that appear in their
signatures. It is what lets `orch-engine` be unit-testable with in-memory fakes and lets `orch-infra` be
swapped wholesale.

**Why `orch-infra` is one crate, not five.** Splitting storage / adapters / process / workspace / templates
into separate crates would add five `Cargo.toml` files and compile-graph edges to buy an isolation we already
get from the `orch-core` traits. One crate with clear modules (`storage`, `adapters`, `process`, `workspace`,
`template`, `skills`, `clipboard`, `models`) is the right granularity at 26k LOC. If adapter development
becomes a bottleneck we can extract `orch-adapters` later — the trait boundary makes that a mechanical move.

### 3.2 Binaries

All three names are the same program, matching the npm `bin` map:

```toml
# crates/orch-cli/Cargo.toml
[[bin]] name = "orch"      path = "src/bin/main.rs"
[[bin]] name = "orchestry" path = "src/bin/main.rs"
[[bin]] name = "ao"        path = "src/bin/main.rs"
```

(A single `main.rs` shared by three `[[bin]]` targets compiles three times. If that becomes annoying, make
`orchestry` and `ao` thin shims that `pub use orch_cli::main`.) The clap program name stays `orchestry` so
`--help` matches.

### 3.3 Module map per crate

| Crate | Modules |
|---|---|
| `orch-domain` | `task`, `agent`, `run`, `goal`, `team`, `message`, `state`, `config`, `global_config`, `events`, `error`, `transitions`, `scope`, `model_tiers`, `shop` (agent-shop + org-shop + default-agents) |
| `orch-core` | `storage` (9 traits), `adapter` (`Adapter`, `AgentEvent`, `ExecuteParams`, `Registry`), `process` (`ProcessManager`), `workspace` (`WorkspaceManager`), `template` (`TemplateEngine`), `skills` (`SkillLoader`), `clock` (`Clock`), `ids` (`IdGenerator`) |
| `orch-infra` | `storage::{paths, fs_utils, index, task, agent, run, state, config, global_config, context, message, goal, team, cached, lock}`, `adapters::{claude, codex, cursor, opencode, pi, grok, antigravity, shell, util, buffer, registry}`, `process`, `workspace::{manager, merge}`, `template`, `skills`, `clipboard`, `models` |
| `orch-engine` | `events` (bus), `task_service`, `agent_service`, `run_service`, `goal_service`, `team_service`, `message_service`, `agent_factory`, `review_runner`, `doctor_service`, `orchestrator`, `container` |
| `orch-cli` | `commands::{task, agent, status, logs, config, context, msg, goal, team, org, run, doctor, tui, serve, init, update}`, `output`, `context`, `editor`, `shop_picker`, `update_check`, `serve::{logger, once, observer}` |
| `orch-tui` | `app`, `state`, `views::{goals, tasks, agents, logs}`, `widgets::*`, `command_bar`, `wizard`, `theme`, `keys` |

---

## 4. Dependencies

All versions verified present on crates.io via `cargo search` on 2026-10-13 against cargo 1.94.1 / rustc 1.94.1.
Pin minors in `[workspace.dependencies]`; let patches float.

### 4.1 Core

| Crate | Version | Why |
|---|---|---|
| `tokio` | `1.53` | The async runtime. Needed for: concurrent child-process stdout/stderr draining (the TS shell adapter deadlocks without it), `select!` for cancellation, and timers. Features: `rt-multi-thread`, `macros`, `fs`, `io-util`, `process`, `signal`, `sync`, `time`. |
| `futures` | `0.3` | `Stream`/`StreamExt` for the adapter event streams. |
| `async-trait` | `0.1` | Async methods in the `orch-core` traits. Native AFIT cannot produce `dyn`-compatible traits, and the container stores `Arc<dyn ...>` — so `async-trait` is required, not stylistic. |
| `serde` | `1.0` | derive. |
| `serde_json` | `1.0` | JSON + JSONL. **Enable the `preserve_order` feature** so `Value` round-trips key order; and prefer typed structs over `json!` for anything persisted (see §5.2). |
| `thiserror` | `2.0` | The error enum in `orch-domain`. |
| `anyhow` | `1.0` | Only in `orch-cli`/`xtask` top-level glue, never in library error signatures. |
| `clap` | `4.6` | derive. Matches Commander's surface: subcommands, aliases, `required`, defaults, global args. |
| `nanoid` | `0.5` | `nanoid::alphabet::SAFE` is the **same 64-character set** as JS nanoid's `urlAlphabet` (verified), so IDs are format-compatible with no custom alphabet. |

### 4.2 YAML — the one genuinely awkward choice

`serde_yaml` is **archived and deprecated**; its last release is literally versioned `0.9.34+deprecated`.
Three maintained options:

| Candidate | Version | Assessment |
|---|---|---|
| **`serde_yaml_ng`** | `0.10.0` | A direct, actively maintained continuation of `serde_yaml` with the same API. **Chosen.** |
| `serde_norway` | `0.9.42` | Also a maintained `serde_yaml` fork, same API. Verified to read and emit identically to `serde_yaml_ng` on our data. A drop-in fallback if `_ng` stalls. |
| `saphyr` / `yaml-rust2` | `0.2.1` / `0.13.0` | Full YAML 1.2 engines, but **no serde integration** — we would hand-write (de)serialization for 7 entity types. Rejected as a default; see §5.3 for where a raw emitter would be needed anyway. |

**Decision: `serde_yaml_ng = "0.10"`**, with `serde_norway` named as the pre-vetted escape hatch. Both were
tested against real TS-written YAML (including the folded-scalar `>-` 3 KB `agt_creator` role) and read it
correctly.

### 4.3 Templates — the highest-risk dependency

| Crate | Version | Notes |
|---|---|---|
| `liquid` | `0.26.11` | The only maintained Liquid implementation for Rust. **Requires pinning `kstring = "2.0.2"`** — `liquid` 0.26.11 resolves `kstring` 2.0.5, whose MSRV (1.96.0) exceeds our toolchain (1.94.1). |

I measured `liquid` 0.26.11 against `liquidjs` 10.27.2 on the exact constructs ORCH uses
([PARITY.md §9.5](PARITY.md)). Summary:

- **Whitespace handling is byte-identical**, including the `\n\n\n` runs the real prompts produce. This was
  the biggest unknown and it came out in our favour.
- Truthiness, `.size`, `!=`, `>`, `default:`, inline `if`, and `for`-over-objects all match.
- Two real divergences, both mitigable in the *context builder* rather than the engine:
  1. Missing keys error instead of rendering empty → **always emit every documented variable, using explicit
     `nil` for absent optionals.** Verified identical once done.
  2. Object iteration order is unspecified → **pass `shared_context` as an ordered array of `[k, v]` pairs**,
     which renders identically through `entry[0]`/`entry[1]` and fixes ordering. Verified.
- One accepted divergence: unknown filters fail at parse time instead of passing through. Only reachable via
  user-supplied templates in `config.prompt.*`.

**The gate is golden fixtures, not reasoning.** Before WP-3 is accepted, the Rust renderer must reproduce
TS-generated prompt output byte for byte across the full branch matrix (§8.3). If it cannot, the contingency is
a ~400-line purpose-built renderer for the 7 constructs actually used — a bounded, well-specified fallback,
not open-ended work.

### 4.4 TUI

| Crate | Version | Notes |
|---|---|---|
| `ratatui` | `0.30` | Includes `TestBackend`, which is what makes the TUI tests deterministic. |
| `crossterm` | `0.29` | Backend + key events. Note `ratatui` 0.30 re-exports a crossterm; use the re-export to avoid a version clash. |

### 4.5 Support

| Crate | Version | Why |
|---|---|---|
| `jiff` | `0.2` | Timestamps. Chosen over `chrono` because we need exactly one thing — RFC 3339 with **millisecond precision and a `Z` suffix** — and `jiff` makes that unambiguous without timezone-database surprises. `chrono 0.4.45` is a fine fallback if `jiff` friction appears. |
| `nix` | `0.31` | `kill(2)` with negative PIDs (process groups), `setsid`. The one place we need real POSIX semantics; `std::process` cannot express group kill. |
| `tempfile` | `3.27` | Atomic writes (temp + rename) and test temp dirs. |
| `dirs` | `7.0` | `~/.orchestry/`. |
| `which` | `8.0` | Adapter binary probing (`cursor-agent` → `agent` fallback). |
| `shell-words` | `1.1` | Parsing/quoting in diagnostics only — **never** for building adapter argv (we pass argv vectors directly, no shell). |
| `globset` | `0.4` | Available if scope matching ever becomes real glob matching. **Not used for `scopesOverlap`**, which is deliberately raw string-prefix logic ([PARITY.md §7.11](PARITY.md)) and must be ported as such. |
| `tracing` + `tracing-subscriber` | `0.1` / `0.3` | Internal diagnostics. **Must not** write to stdout — stdout is the CLI contract. Default to stderr at `warn`, controlled by `ORCHESTRY_DEBUG`/`RUST_LOG`. |
| `indexmap` | `2.14` | Insertion-ordered maps where TS object order matters. |
| `toml` | `1.1` | Only for our own tooling/config, not for `.orchestry/`. |

### 4.6 Dev-dependencies

| Crate | Version | Why |
|---|---|---|
| `insta` | `1.49` | Snapshot tests for adapter argv, rendered prompts, human CLI output. |
| `assert_cmd` | `2.2` | Running our own binary in integration tests. |
| `predicates` | `3.1` | Assertions on command output. |
| `tempfile` | `3.27` | Isolated project dirs per test. |
| `proptest` | *(add when needed)* | For the scope-overlap equivalence property (V12) and the ID-format property (V29). |

### 4.7 Deliberately not used

- **`git2`** (`0.21`) — binds libgit2. [PARITY.md §10](PARITY.md) specifies *exact git command lines*
  (`git worktree add … -b …`, `git worktree prune`, `git merge --no-ff … -m …`, `git merge --abort`), and
  parity depends on their exact behaviour and exit codes, including `worktree prune` semantics libgit2 models
  differently. **Shelling out to `git` is the correct choice here** and it also removes a C dependency from
  the build. Decision recorded because it looks backwards at first glance.
- **`notify`** (`9.0.0-rc.5`) — only a release candidate, and the serve-mode disk observer polls in the
  original. Keep polling; parity first.
- **`sysinfo`** — overkill. PID liveness is `kill(pid, 0)` via `nix`.
- **`libc` 1.0** — still alpha (`1.0.0-alpha.5`). `nix` 0.31 pulls a stable `libc` 0.2; let it.

---

### 4.8 Library-first policy (user directive, 2026-10-13)

**Where a well-supported third-party crate covers a complex feature, use it instead of hand-rolling.**
Hand-roll only when (a) no maintained crate fits, or (b) a parity requirement in PARITY.md rules the crate
out — and say which in a code comment. Candidates for areas that would otherwise be hand-written (verify
version compatibility with `ratatui` 0.30 / rustc 1.94.1 before adopting; record the choice in
`[workspace.dependencies]`):

| Area | Crate(s) | Note |
|---|---|---|
| Process groups / session spawn + group kill (tokio) | `process-wrap` 10 (tokio feature) | Prefer over hand-rolled `setsid` + `kill(-pid)`; keep `nix` for `kill(pid, 0)` liveness if `process-wrap` lacks it |
| TUI multi-line/single-line text editing (wizards, command bar, undo) | `tui-textarea` / `tui-input` (or a maintained ratatui-0.30-compatible fork) | Parity of key semantics is L1 — wrap the widget, don't rewrite an editor |
| Grapheme segmentation / display width (`text-cursor` parity) | `unicode-segmentation`, `unicode-width` | Replaces `Intl.Segmenter` + width logic |
| Clipboard text/image (TUI paste) | `arboard` (with image support) | TS shells out to osascript/xclip/PowerShell; TUI is L2 here, so the mechanism may differ — degrade to "empty" on any failure, as TS does |
| Diffs in the parity harness / test failures | `similar`, `pretty_assertions` | |
| Version comparison (update check) | `semver` | |
| Templates | `liquid` (already chosen) | The ~400-line fallback renderer is a last resort only, after escalation |

Things that **stay hand-written because parity requires it**: the PID lock-file format, `scopesOverlap`'s raw
string-prefix logic, the line reader's 16 KB cap/empty-line semantics, the retry backoff formula, the
`--since` duration regex, the table renderer's exact padding, and the TS-compatible atomic-write temp naming.

---

## 5. Key traits and types

Signatures below are the contract that WP-0 freezes. Illustrative but intended to be close to final.

### 5.1 Storage

```rust
// orch-core/src/storage.rs
#[async_trait]
pub trait TaskStore: Send + Sync {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>>;
    async fn get(&self, id: &str) -> Result<Option<Task>>;
    async fn save(&self, task: &Task) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[derive(Default, Clone)]
pub struct TaskFilter { pub status: Option<TaskStatus>, pub goal_id: Option<String> }

#[async_trait]
pub trait AgentStore: Send + Sync {
    async fn list(&self) -> Result<Vec<Agent>>;
    async fn get(&self, id: &str) -> Result<Option<Agent>>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Agent>>;
    async fn save(&self, agent: &Agent) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait RunStore: Send + Sync {
    async fn save(&self, run: &Run) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<Run>>;
    async fn list_all(&self) -> Result<Vec<Run>>;
    async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>>;
    async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>>;
    async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()>;
    async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>>;
    async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>>;
    /// Reads the JSONL file to EOF and ends — **not** a tail-follow. Waits up to 30 s
    /// for the file to appear. Cancellable. Has no production caller (tests only);
    /// `logs --follow` subscribes to the EventBus instead. See PARITY.md §6.9.
    fn stream_events(&self, run_id: &str, token: CancellationToken)
        -> BoxStream<'static, Result<RunEvent>>;
    /// Release the cached O_APPEND handle for this run.
    fn close_run_events(&self, run_id: &str);
}

// Plus: StateStore, ConfigStore, ContextStore, MessageStore, GoalStore, TeamStore
// — same shapes as the TS interfaces in src/infrastructure/storage/interfaces.ts.
```

Notes:
- `&self`, not `&mut self`: stores are shared as `Arc<dyn TaskStore>` and own their interior mutability
  (the index mutex). This matches the TS classes, which are shared instances.
- `TaskFilter` as a struct rather than two `Option` parameters, so adding a filter dimension is not a
  signature break.
- `close_run_events` is **not** async in TS and should stay sync — it is a cache eviction.

### 5.2 Serialization helpers (the cross-compat crux)

```rust
// orch-infra/src/storage/fs_utils.rs

/// temp file + fsync + rename. Temp name: `.{basename}.{8 hex}.tmp` in the same directory.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()>;

/// `serde_json::to_string_pretty` + '\n'. 2-space indent, trailing newline — matches
/// `JSON.stringify(v, null, 2)` + console.log.
pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()>;

/// js-yaml-compatible emission. See `quote_ambiguous_scalars`.
pub async fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()>;

/// Compact JSON + '\n' appended through a cached O_APPEND handle, with the
/// PIPE_BUF truncation rule from PARITY.md §6.9.
pub async fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<()>;
```

Two rules every entity struct must follow, or cross-compat breaks silently:

1. **Every `Option` field** that the TS type marks `?` carries
   `#[serde(skip_serializing_if = "Option::is_none")]`. TS `undefined` is *omitted*, not null. The sole
   exception is `Message::to_agent_id`, which is `string | null` and must emit `null`.
2. **Never use `serde_json::json!` for a persisted record.** `serde_json::Map` defaults to `BTreeMap`, so
   `json!` **alphabetizes** keys — a JSONL event came out `{"data":…,"timestamp":…,"type":…}` in testing
   instead of `{"timestamp":…,"type":…,"data":…}`. Derived structs emit declaration order. Use typed structs,
   and enable `preserve_order` as belt-and-braces.

### 5.3 The YAML timestamp guard

This is the one place where correctness required measurement. js-yaml's default schema includes the YAML 1.1
`timestamp` type, so an **unquoted** `created_at: 2026-10-13T16:37:58.959Z` deserializes in TypeScript to a
JavaScript `Date`, not a `String` — and `MessageStore.list()` then calls `.localeCompare()` on it and
**throws**. Both `serde_yaml_ng` and `serde_norway` emit such scalars unquoted.

```rust
/// Post-emit pass over serialized YAML: force single-quoting on any plain scalar that
/// js-yaml's DEFAULT_SCHEMA would resolve as a YAML 1.1 timestamp or date.
/// Operates on the emitted document, keyed off the scalar grammar — not on field names,
/// so a user-supplied `description` that happens to look like a date is also protected.
fn quote_ambiguous_scalars(yaml: &str) -> String;
```

Implemented inside `write_yaml` so no caller can forget it. **Mandatory test (V8):** write every entity type
from Rust, then read it back *in the TS process* and assert `typeof value === 'string'` for every timestamp
field. If the post-emit pass proves leaky in practice, the escalation is a hand-written emitter over
`serde_norway::Value` with explicit scalar styles (~150 lines) — which is also where `saphyr` would earn its
place.

### 5.4 Adapters

```rust
// orch-core/src/adapter.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind { Output, FileChange, Command, ToolCall, Error, Done }

#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub kind: AgentEventKind,
    pub timestamp: String,               // ISO-8601, minted at parse time
    pub data: serde_json::Value,
    pub tokens: Option<TokenUsage>,
    pub error_kind: Option<AdapterErrorKind>,
}

pub struct ExecuteParams {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: PathBuf,
    pub env: BTreeMap<String, String>,
    pub config: AgentConfig,
    pub cancel: CancellationToken,
}

/// Returned synchronously: the PID is available before any event is pulled.
pub struct ExecuteHandle {
    pub pid: u32,
    pub events: BoxStream<'static, Result<AgentEvent, AdapterError>>,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn test(&self) -> AdapterTestResult;
    /// NOT async: mirrors the TS contract where execute() returns { pid, events }
    /// synchronously. Lifecycle listeners must be attached before the stream is polled.
    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError>;
    async fn stop(&self, pid: u32) -> Result<()>;
}
```

`execute` being sync is a deliberate contract choice, not an oversight: the TS code attaches `close`/`error`
and stderr listeners *eagerly* precisely so a fast-failing child is not missed, and the orchestrator needs the
PID to write `state.running` before it starts consuming events. Spawning is itself non-blocking, so sync costs
nothing.

Stream errors are modelled as `Result` items rather than terminating the stream, because the TS generator
distinguishes "an `error` event occurred" (continue) from "the generator threw" (run failed). The thrown case
is the stream yielding `Err` and then ending.

```rust
pub struct AdapterRegistry { adapters: Vec<Arc<dyn Adapter>> }  // Vec, not HashMap — order is contract
impl AdapterRegistry {
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn Adapter>>;
    pub fn require(&self, kind: &str) -> Result<&Arc<dyn Adapter>, DomainError>;
    pub fn kinds(&self) -> Vec<&'static str>;   // registration order — L0
}
```

### 5.5 Process management

```rust
#[async_trait]
pub trait ProcessManager: Send + Sync {
    fn is_alive(&self, pid: u32) -> bool;
    fn kill(&self, pid: u32, signal: Signal);
    async fn kill_with_grace(&self, pid: u32, grace: Duration);
    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild>;
}

pub struct SpawnOptions {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,   // overlaid on the inherited environment
    pub stdin: StdioMode,                // Ignore by default; Pipe for codex and pi
}

pub struct SpawnedChild {
    pub pid: u32,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    pub stdin: Option<ChildStdin>,
    pub exit: BoxFuture<'static, ExitOutcome>,  // resolves once, shareable
}
```

Implementation requirements ([PARITY.md §8.4](PARITY.md)):

- `pre_exec(|| { setsid(); Ok(()) })` to create a new process group — the Rust equivalent of node's
  `detached: true`. This is what makes `kill(-pid)` reap the whole tree.
- `kill` tries the **group first** (`nix::sys::signal::kill(Pid::from_raw(-(pid as i32)), sig)`), falling back
  to the direct PID, swallowing errors.
- `kill_with_grace`: return if dead; `SIGTERM`; poll `is_alive` every **200 ms** to the deadline; `SIGKILL`.
- `is_alive` = `kill(pid, None)` is `Ok`, **or** `Err(EPERM)`.
- `exit` must be subscribable before the output streams are read, and resolvable more than once by the
  consumer — use a `tokio::sync::watch` or a shared `OnceCell`, not a plain oneshot.

### 5.6 Injected seams for determinism — a deliberate addition

The TS test suite has **no clock seam and no ID-generator seam**: timestamps come from real `new Date()` and
tests assert ID *prefixes* rather than values. That is a large part of why ~500 TUI/timing cases are flaky
([PARITY.md §14.9](PARITY.md)).

```rust
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    /// ISO-8601 UTC, exactly 3 fractional digits, 'Z' — PARITY.md §6.3
    fn now_iso(&self) -> String;
}
pub trait IdGenerator: Send + Sync {
    fn generate(&self, prefix: &str) -> String;   // "<prefix>_<nanoid(7)>"
}
```

Real implementations in `orch-infra`; `FixedClock`/`SeqIdGenerator` in a `test-support` module. Both are
threaded through the container and reachable by every service. This is **not** parity-neutral bookkeeping — it
is what lets the differential harness assert on exact values, lets retry/backoff and stall-detection tests run
instantly instead of sleeping, and removes a whole class of flake. It is the one place where I am deliberately
going beyond a transliteration of the original.

### 5.7 Error model

TS `OrchestryError` carries `(message, exitCode, hint?)` and subclasses specialize it.

```rust
// orch-domain/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Not initialized")]                                    NotInitialized,
    #[error("{0}")]                                                InvalidArguments(String),
    #[error("Orchestrator already running (PID: {0})")]            LockConflict(u32),
    #[error("Agent adapter \"{adapter}\" not available")]          AgentAdapter { adapter: String, detail: String },
    #[error("No agents configured")]                               NoAgents,
    #[error("Task not found: {0}")]                                TaskNotFound(String),
    #[error("Agent not found: {0}")]                               AgentNotFound(String),
    #[error("Task {task_id} is already running (run: {run_id}, agent: {agent})")]
                                                                   TaskAlreadyRunning { task_id: String, run_id: String, agent: String },
    #[error("Invalid transition for {task_id}: {from} → {to}")]     InvalidTransition { task_id: String, from: TaskStatus, to: TaskStatus },
    #[error("Goal not found: {0}")]                                GoalNotFound(String),
    #[error("Cannot mark goal {id} as achieved: {count} task(s) still pending — {summary}")]
                                                                   GoalHasPendingTasks { id: String, count: usize, summary: String },
    #[error("Team not found: {0}")]                                TeamNotFound(String),
    #[error("Message not found: {0}")]                             MessageNotFound(String),
    #[error("{message}")]                                          Workspace { message: String, hint: Option<String> },
    #[error("{0}")]                                                Other(String),
}

impl DomainError {
    pub fn exit_code(&self) -> i32 { /* PARITY.md §3 */ }
    pub fn hint(&self) -> Option<String> { /* PARITY.md §3.1 — exact strings */ }
}
```

The `Display` strings **are** the user-facing messages from [PARITY.md §3.1](PARITY.md) — writing them in
`#[error(...)]` makes the contract hard to drift from. `exit_code()` and `hint()` are the only two places the
CLI needs. `orch-cli`'s top-level handler is the single exit point: downcast to `DomainError` → print
`  ✕ <msg>` (+ hint) to stderr → exit with `exit_code()`; anything else → exit 1, and dump the chain when
`ORCHESTRY_DEBUG` is set.

Infrastructure errors (`std::io::Error`, YAML, JSON) get their own enums per module and convert **into**
`DomainError::Other` or a specific variant at the service boundary. Library crates never return `anyhow`.

---

## 6. Concurrency model

### 6.1 Replacing the promise-chain mutex

The TS orchestrator serializes state mutation with a hand-rolled promise chain (`withStateLock`), and
`IndexManager` has its own. The faithful and simpler Rust equivalent is `tokio::sync::Mutex`:

```rust
pub struct Orchestrator {
    state: Arc<Mutex<OrchestratorState>>,       // replaces stateMutex + this.state
    active_collectors: Arc<Mutex<HashSet<String>>>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,   // replaces abortControllers
    /* deps: Arc<dyn …> */
}
```

`tokio::sync::Mutex` (not `std::sync::Mutex`) because the critical sections `await` on store I/O — exactly as
the TS code does. Two properties of the original must be preserved:

1. **The lock is held across awaits**, including file writes. This is intentional: it is what makes
   tick/stop/completion mutually exclusive. Do not "optimize" it into fine-grained locking — the TS bug
   history (the `activeCollectors` guard, the early `delete state.running[taskId]`, the re-read-before-save
   patterns) is all about this serialization.
2. **`std::sync::Mutex` is correct for `IndexManager`**'s *in-memory* guard only if no await happens inside;
   it does (file I/O), so use `tokio::sync::Mutex` there too.

`Orchestrator` is `Clone`-able over `Arc`s so background tasks can hold it. One `Arc<Mutex<_>>` per concern
rather than one giant lock, with a documented acquisition order (`state` → `active_collectors` → `cancels`) to
make deadlock impossible by construction.

### 6.2 Tick loop

```rust
pub async fn start_watch(&self, opts: WatchOptions) -> Result<()> {
    self.acquire_lock().await?;                 // LockConflict on failure
    { let mut st = self.state.lock().await; /* load, cleanup stale, set pid/started_at, save */ }
    let mut signals = signal_stream()?;         // SIGINT + SIGTERM
    let mut ticker = tokio::time::interval(self.config.scheduling.poll_interval_ms);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);   // no burst catch-up
    self.tick().await?;                                          // immediate first tick
    loop {
        tokio::select! {
            _ = ticker.tick()              => self.on_tick_result(self.tick().await),
            _ = self.immediate.notified()  => { let _ = self.immediate_dispatch().await; }
            Some(sig) = signals.next()     => { self.emit_shutdown(sig); break; }
            _ = self.shutdown.cancelled()  => break,
        }
    }
    self.stop().await
}
```

- **`MissedTickBehavior::Delay`** matters. `setInterval` in node does not catch up on missed ticks; tokio's
  default `Burst` would fire several ticks back-to-back after a slow one, changing dispatch timing.
- The reactive-dispatch debounce (500 ms, max 10 retries while a tick is in flight) becomes a
  `tokio::sync::Notify` plus a debounce task — the retry loop exists to avoid contending with a tick, which
  `select!` arms already partly solve, but the **observable** 500 ms delay must be preserved because tests
  depend on it.
- Signal handling moves from process-global handlers to a `select!` arm, which removes the TS
  add/remove-listener dance entirely.

### 6.3 Child process I/O

Per run, one `tokio::spawn`ed collector task:

```rust
let (mut out_lines, mut err_tail) = (lines(child.stdout), stderr_tail(child.stderr));
loop {
    tokio::select! {
        line = out_lines.next_line() => match line { Some(l) => emit(parse(l)), None => break },
        _ = cancel.cancelled()       => { pm.kill_with_grace(pid, Duration::from_secs(5)).await; return; }
    }
}
```

- **stdout and stderr must be drained concurrently.** The TS shell adapter explicitly notes that reading them
  sequentially deadlocks on a full pipe. Two tasks, or two `select!` arms — never sequential.
- The stderr tail is a bounded **4096-byte** ring, attached before the stream is polled, used only to enrich
  an error message.
- `readLines` semantics are specified in [PARITY.md §8.5](PARITY.md) and are **not** what
  `tokio::io::AsyncBufReadExt::lines()` does: it splits on `0x0A` only (keeping `\r`), **skips empty lines**,
  yields a trailing fragment, and caps each line at 16384 characters. Write our own `line_reader`; do not
  reach for the convenience method.

### 6.4 Cancellation

`tokio_util::sync::CancellationToken` replaces `AbortController`, one per running task, stored in
`Orchestrator::cancels`. Cancelling it both stops the collector loop and triggers `kill_with_grace`.
Grace periods by call site: `cancel_task`/`force_stop_agent` 3 s, stall/abort 5 s, `stop()` 10 s.

A `Drop` guard on the orchestrator is **not** sufficient for process cleanup (async work cannot run in
`Drop`); shutdown must be explicit, which is already the shape of `stop()`.

### 6.5 Event bus

TS `EventBus` is synchronous: `emit()` runs every handler inline and swallows their exceptions. The Rust
equivalent:

```rust
pub struct EventBus { tx: tokio::sync::broadcast::Sender<Arc<OrchestratorEvent>> }
impl EventBus {
    pub fn emit(&self, event: OrchestratorEvent);              // non-blocking, lossy-on-lag
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<OrchestratorEvent>>;
}
```

`broadcast` with `Arc` payloads gives cheap fan-out to the TUI, the structured logger and the run store. One
behaviour change to be explicit about: `broadcast` **drops** messages for a lagging receiver instead of
blocking, whereas synchronous TS emission cannot lose an event. Mitigation: size the channel generously
(4096), and have subscribers that must not miss events (the run-store writer) consume on a dedicated task
that does nothing else. `RecvError::Lagged` is surfaced as a `tracing::warn`, never silently ignored. The TUI
*may* lose high-frequency `agent:output` events under load — which is exactly what the TS TUI's 80 ms batching
and 2 KB caps already accept.

---

## 7. Container and wiring

```rust
pub struct Engine {                       // the TS LightContainer
    pub paths: Paths,
    pub config: OrchestratorConfig,
    pub global_config: GlobalConfig,
    pub tasks: Arc<dyn TaskStore>,  /* … the other 8 stores … */
    pub events: Arc<EventBus>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub task_service: Arc<TaskService>,  /* … the other services … */
}

pub struct FullEngine {                   // the TS Container
    pub engine: Engine,
    pub processes: Arc<dyn ProcessManager>,
    pub adapters: Arc<AdapterRegistry>,
    pub workspace: Arc<dyn WorkspaceManager>,
    pub templates: Arc<dyn TemplateEngine>,
    pub skills: Arc<dyn SkillLoader>,
    pub doctor: Arc<DoctorService>,
    pub orchestrator: Arc<Orchestrator>,
}

pub async fn build_light(ctx: &CliContext) -> Result<Engine>;
pub async fn build_full(ctx: &CliContext) -> Result<FullEngine>;
```

The two-tier split is kept for structural fidelity and because it documents which commands may mutate state.
In Rust it buys little startup time (there are no dynamic imports to avoid), so **do not** add lazy
initialization machinery for it — just construct what each tier needs.

---

## 8. Test strategy

Four layers. Layers 1 and 2 are the acceptance gates; 3 and 4 are how individual packages prove themselves.

### 8.1 Layer 1 — ported unit and integration tests

Mapping of the TS suite (counts from [docs/parity/tests.md](parity/tests.md)):

| TS directory | Files | Cases | Rust destination | Notes |
|---|---|---|---|---|
| `test/unit/domain` | 10 | ~183 | `orch-domain` `#[cfg(test)]` modules | Pure. Ports almost mechanically. |
| `test/unit/application` | 25 + helper | ~429 | `orch-engine/tests/` + inline | Needs the in-memory fake stores (WP-0 deliverable). |
| `test/unit/infrastructure` | 38 | ~562 | `orch-infra/tests/` + inline | Mix of pure (fs-utils, index, scope) and PROCESS/IO (tempdirs, real `bash`). |
| `test/unit/cli` | 18 + helper | ~224 | `orch-cli/tests/` | Output formatting, flag parsing, serve logger, disk observer. |
| `test/unit/tui` | 30 | ~681 | `orch-tui/tests/` via `TestBackend` | **Re-expressed, not ported** — see §8.4. |
| `test/integration` | 4 | 6 | `tests/` workspace-level | |
| `test/smoke-*.ts`, `test/battle/`, `test/e2e/*.pty.ts` | 6 | — | `xtask` subcommands | Not part of `cargo test`; deliberately outside the suite in the original too. |

**No test in the reference suite requires a real AI CLI.** Verified: there are no `which` / `command -v` /
binary-presence skip guards anywhere in `test/`. Every adapter test injects a fake `IProcessManager` whose
`spawn()` returns a hand-built `EventEmitter` with `PassThrough` streams, and drives it from outside (write
bytes, emit `exit`/`error`), asserting exact argv and exact stdin writes. **No fake shell scripts are written
to disk anywhere in the TS suite.** So there is no AI-CLI boundary to stub at the unit level, and the Rust
ports inject `FakeProcessManager` the same way.

The **fake agent scripts** in §8.2 are therefore a *new* requirement specific to the differential harness, not
a port of anything: the harness drives both binaries as subprocesses, so it cannot inject a trait object
across that boundary and must instead put a replay script named `claude`/`codex`/… on `PATH`. Keep the two
mechanisms distinct — unit tests inject, the harness shims `PATH`.

**Port the helper infrastructure first** (WP-0). The TS suite rests on exactly two shared helpers; the Rust
equivalent is a `test-support` module in `orch-core` (behind a feature flag so it does not ship):

- `make_task()`, `make_agent()`, `make_run()` with the **same hard-coded defaults** as the TS factories —
  including `make_agent()`'s `approval_policy: auto`, which is why so many tests observe `review → done`
  without an explicit approve, and `stall_timeout_ms: 300_000`.
- In-memory `Mutex<HashMap<_>>`-backed fakes for all 9 stores, cloning on read and write to prevent aliasing
  (the TS fakes `structuredClone` for exactly this reason).
- `FakeProcessManager` (fixed pid 12345), `FakeAdapter` (takes a `Vec<AgentEvent>` to replay),
  `FakeWorkspaceManager`, `FakeTemplateEngine` (returns a constant), `FixedClock`, `SeqIdGenerator`.
- `build_deps(overrides)` assembling complete `OrchestratorDeps` with **real** services, event bus and
  registry — only the stores and I/O edges faked. Default `poll_interval_ms` of 100_000 so the tick loop never
  self-fires and tests drive ticks explicitly.

Rust needs a `TestOrchestrator` builder rather than the TS `overrides`-spread trick; that is WP-0's job to get
ergonomic, because five other packages will build on it.

### 8.2 Layer 2 — the differential parity harness

The primary guarantee. Lives in `tests/parity/`, driven by `cargo xtask parity`.

```
fixtures/scenarios/*.toml
  ┌─────────────────────────────────────────────────────────┐
  │ name = "task-lifecycle-shell-agent"                      │
  │ steps = [                                                │
  │   "init --name proj",                                    │
  │   "agent add shelly --adapter shell --command 'echo hi'", │
  │   "task add 'Do it' --assignee shelly --json",            │
  │   "config set defaults.agent.workspace_mode shared",      │
  │   "run ${task.0}",                                        │
  │   "task list --json",                                     │
  │   "status --json",                                        │
  │ ]                                                        │
  └─────────────────────────────────────────────────────────┘
                │                                  │
      ┌─────────▼──────────┐           ┌───────────▼───────────┐
      │ tmpdir A (TS)       │           │ tmpdir B (Rust)        │
      │ node parity/orch-ts │           │ target/debug/orch      │
      │   /dist/cli.js      │           │                        │
      └─────────┬──────────┘           └───────────┬───────────┘
                │   stdout, stderr, exit code,      │
                │   full .orchestry/ tree           │
                └────────────┬──────────────────────┘
                             ▼
                  normalize (PARITY.md §1.4)
                  then assert deep equality
```

Rules:

- **Both tmpdirs are `git init`ed identically** with one commit, because worktree mode requires a repo.
- **Hermeticity — the harness must isolate the home directory, not just the global config.**
  `ORCHESTRY_GLOBAL_CONFIG_PATH` redirects `global.yml` **alone**; `src/cli/update-check.ts:15-16,125`
  hard-codes its cache to `path.join(os.homedir(), '.orchestry')` with **no override**, and
  `src/bin/cli.ts:210-231` runs the check after every command except `tui`/`update`/`serve`, writes a
  notification to **stderr**, and spawns `npm install -g` when an update exists. Since the harness compares
  stderr and is a required merge gate, that makes the gate non-deterministic *and* gives it a side effect on
  the host. So every scenario:
  1. sets **`HOME` and `USERPROFILE`** per tmpdir,
  2. sets `ORCHESTRY_GLOBAL_CONFIG_PATH` inside it,
  3. **pre-seeds `$HOME/.orchestry/update-check.json`** with a fresh `checked_at` so the SWR cache is warm and
     no registry query happens,
  4. **asserts the oracle spawned no `npm` and opened no socket** (V31). CI runs the parity job with
     networking disabled.
  Without step 3 the gate is green in an offline container and red on a laptop — or the reverse — with no code
  change, and a parity run can leave the developer's real TUI claiming a restart is needed.
- **No real AI CLI is ever needed.** Agents use the `shell` adapter, or a **fake agent script** — a small
  `bash`/Rust binary on `PATH` under the name `claude`/`codex`/… that replays a recorded JSONL stream from a
  fixture file. This is how adapter-level behaviour (token accounting, `done` events, file-change extraction)
  gets exercised end to end.
- Comparison covers: **stdout**, **stderr**, **exit code**, and a **recursive walk of `.orchestry/`** —
  every file, parsed according to its extension (YAML/JSON/JSONL), normalized, deep-compared. Files present
  in one tree and absent in the other are failures.
- **Every YAML artifact is additionally re-parsed in the TS process with js-yaml** and deep-compared against
  the Rust parse (V32). This is a harness *step*, not a hand-written test. Without it, both trees are verified
  by a single parser that shares the writer's assumptions, and **every bug of the form "js-yaml reads r-orch's
  bytes differently" is invisible by construction** — which is precisely the failure class R1 and the
  `--allow-foreign-project` valve exist to prevent. It is ~50 lines of Node glue, it subsumes V8's timestamp
  assertion as a special case, and it catches the schema deltas in [PARITY.md §6.4.2](PARITY.md) (merge keys,
  `!!binary`, `!!timestamp` tags, complex keys) without anyone having to enumerate them correctly in advance.
- **Cross-compat scenarios** are a separate scenario kind, and they are the part that actually proves the
  shared-format claim:
  - `ts_then_rust`: TS runs steps 1..k, Rust runs k+1..n in the *same* directory.
  - `rust_then_ts`: the reverse.
  - `interleaved`: alternate per step.
  Each is run for the lifecycle scenarios, which is what catches the timestamp-quoting class of bug (§5.3) —
  it is invisible to a same-binary test.
- Scenario failures print a unified diff of the normalized artifacts, not a boolean.

**Run the TS oracle with `CI=1`** (⇒ `maxWorkers=2`) whenever the reference *suite* is invoked, per
[PARITY.md §14.9](PARITY.md). The oracle *binary* is unaffected, but the habit should be in the tooling.

### 8.3 Layer 3 — fixtures

| Fixture set | Generated by | Asserts |
|---|---|---|
| `fixtures/prompts/*.txt` | `xtask gen-prompts` — drives the TS engine over the full branch matrix of the two default templates (with/without role, autonomous, goal_id, attempt>1, retry output empty/non-empty, feedback, shared_context, messages 0/1/n, goal description/tasks/progress) | Rust template rendering, **byte-exact**. The gate for §4.3. |
| `fixtures/adapters/<adapter>/*.jsonl` + `.expected.json` | Hand-written from `test/unit/infrastructure/*-adapter.test.ts` | Each adapter's stdout → `AgentEvent` sequence mapping, token extraction, error classification. |
| `fixtures/yaml/*.yml` | Copied from TS-written artifacts, plus Rust-written ones fed back to TS | Round-trip compatibility both directions (V8). |
| `fixtures/shop/*.json` | Extracted from `src/domain/agent-shop.ts`, `org-shop.ts`, `default-agents.ts` | The catalogue data, byte-exact (V28). |

Fixture generation is a committed `xtask`, not a one-off script, so fixtures can be regenerated when the
reference commit is bumped.

### 8.4 Layer 4 — TUI tests

`ratatui::backend::TestBackend` renders into an in-memory buffer. The pattern:

```rust
let mut app = App::new(test_engine());
app.handle_key(KeyEvent::from(KeyCode::Char('A')));     // switch to agents
app.handle_key(KeyEvent::from(KeyCode::Char('N')));     // open the wizard
let mut term = Terminal::new(TestBackend::new(100, 30))?;
term.draw(|f| app.render(f))?;
assert_buffer_contains(term.backend().buffer(), "Agent name");
```

**Push the key, draw, assert — synchronously.** No sleeps. This is the structural fix for the ~500 cases whose
TS equivalents are flaky ([PARITY.md §14.9](PARITY.md)), and it means the two F1 tests that
`ink-testing-library` could not express (`KeyCode::F(1)`) become ordinary passing tests.

What to test: the key-dispatch chain order; view switching and its blocked-while-detail-open rule; wizard step
sequencing, skip predicates and validation messages; command-bar completion vectors; filter pickers. What
**not** to test: exact glyphs, colours, or pixel layout — those are L2.

### 8.5 CI shape

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                       # layers 1, 3, 4
cargo xtask parity                           # layer 2 — needs node + the vendored TS build
cargo xtask crate-graph                      # asserts the whole inter-crate edge set (§3.1)
```

`cargo xtask parity` is gated on `parity/orch-ts/dist/` existing; `parity/setup.sh` clones the pinned
commit `c066dc0` and runs `npm ci && npm run build`. The parity job is **required** for merge — the whole
design rests on it.

---

## 9. Work breakdown

Sized so one engineer agent completes a package in one session (~3–6k LOC Rust including tests).

### WP-0 — Foundation (one engineer, blocking, must land before anything else)

**Owns:** `crates/orch-domain/**`, `crates/orch-core/**`, workspace `Cargo.toml`, `rust-toolchain.toml`,
`crates/orch-core/src/test_support/**`.

**Port from:** `src/domain/*.ts` (all of it), `src/infrastructure/storage/interfaces.ts`,
`src/infrastructure/adapters/interface.ts`, `src/infrastructure/process/process-manager.ts` (trait only),
`src/infrastructure/workspace/interface.ts`, `src/infrastructure/template/template-engine.ts` (trait +
`PromptContext` only), `src/infrastructure/skills/skill-loader.ts` (trait only),
`test/unit/application/helpers.ts`, `test/unit/domain/**`.

**Delivers:**
1. Every domain type with final serde attributes — field order matching the TS interfaces,
   `skip_serializing_if` on every optional, `Message::to_agent_id` emitting `null`.
2. `transitions`, `scope` (including `ScopeIndex`), `model_tiers`, and the `shop`/`default_agents` catalogue
   data, all with their ported unit tests.
3. `DomainError` with `exit_code()` and `hint()` returning the exact [PARITY.md §3.1](PARITY.md) strings.
4. **All `orch-core` traits, frozen** — §5 is the brief.
5. `test_support`: entity factories, in-memory store fakes, `FakeProcessManager`, `FakeAdapter`,
   `FakeWorkspaceManager`, `FakeTemplateEngine`, `FixedClock`, `SeqIdGenerator`, and a `TestOrchestrator`
   builder stub.

**Acceptance:** `test/unit/domain/**` (~183 cases) ported and green; V9, V10, V12, V13, V28, V29 pass;
`cargo clippy -D warnings` clean. **The trait surface is then frozen** — later changes need an explicit
amendment, because five packages compile against it.

**Standing rule from the freeze onward:** any package that finds itself **working around** a trait **files an
amendment instead**. With six agents building in parallel against a frozen surface, the cheap local move is
always to work around it — and the result is two caches with different lifetimes over the same store, or
duplicated invalidation logic, surfacing as an intermittent invalid-transition error that no unit test
reproduces (because `test_support`'s fakes clone on every read). Working around a trait is a **defect**, not a
shortcut.

---

### WP-0b — Walking skeleton *(hard gate: WP-4 and WP-5 may not begin until this is green)*

**Owner:** the WP-0 engineer. **Cost:** days, not weeks.

**Why it exists.** An earlier revision of this plan ordered WP-5 (CLI) last and claimed "the differential
harness becomes meaningful the moment this package lands." That built the **measuring instrument** early and
the **measurable surface** last: a scenario step is an argv line executed against a binary, so until
`orch init` and `orch task list` exist end to end, *every* scenario fails at step 1 and zero rows of V1–V8,
V20, V21 can run. Six packages would ship individually green against six readings of PARITY.md, and integrate
in one week through a harness that had never been green, with no last-known-good to bisect against. The
schedule response to that is to relax the normalizers until green — which is how an L0 contract quietly
becomes an L1 one.

**Delivers:** a thin vertical slice — `orch init`, `orch agent add`, `orch task add`, `orch task list` **only**
— cutting through domain → storage → services → CLI, plus the harness skeleton (`cargo xtask parity`, the
scenario format, the normalizers of [PARITY.md §1.4](PARITY.md), the hermeticity setup of §8.2).

**Acceptance (V39):** **≥1 green differential scenario** and **≥1 green `rust_then_ts` cross-compat
scenario** over those four commands. Nothing else counts as done.

This converts the harness from a final exam into a **ratchet**: from here on, every package lands against a
gate that is already green and must stay green.

---

### WP-1 — Storage and serialization *(riskiest package — assign the strongest engineer)*

**Owns:** `crates/orch-infra/src/storage/**`.
**Port from:** `src/infrastructure/storage/*.ts` (all 14 files).

**Delivers:** `Paths` + `sanitize_id` + `find_project_root`; `fs_utils` (`atomic_write`, `write_json`,
`write_yaml` **with the timestamp guard**, `append_jsonl` with the PIPE_BUF rule, `read_jsonl`,
`read_jsonl_tail`, the cached `O_APPEND` handle map with its 10 s idle eviction); `IndexManager`; all 9 store
implementations; the tick-scoped cached wrappers; `lock.rs`.

**Acceptance:** `test/unit/infrastructure/{fs-utils,fs-endurance,indexed-store,task-store,agent-store,goal-store,state-store,context-store,message-store-index,cached-stores,concurrent-access}.test.ts` ported;
**V6, V7 and V8 pass** — including the cross-compat round trip executed through the real TS binary.

**Why riskiest:** it is the one package where being *subtly* wrong is invisible in-tree and only shows up as
corruption in a user's project. The timestamp-quoting hazard (§5.3) alone can silently break the TS binary's
message sorting. Ship V8 before anything else in this package.

---

### WP-2 — Adapters and process management

**Owns:** `crates/orch-infra/src/{adapters,process,models}/**`.
**Port from:** `src/infrastructure/adapters/*.ts` (12 files), `src/infrastructure/process/process-manager.ts`,
`src/infrastructure/models/model-discovery.ts`.

**Delivers:** `ProcessManager` (setsid process groups, group-first kill, graceful kill, `is_alive`); the
`line_reader` with [PARITY.md §8.5](PARITY.md) semantics; the bounded stderr tail; the event buffer with
backpressure; all 8 adapters with exact argv and parsing; `AdapterRegistry` preserving order; model discovery
with its parsers and fallback lists.

**Stderr draining is structural, not per-adapter.** `grok` and `antigravity` in the original never attach a
stderr consumer, so a child writing past one pipe buffer blocks forever ([PARITY.md §14.12](PARITY.md)).
Put draining in the **shared spawn path** so an adapter *cannot* forget it. Two distinct behaviours, and the
difference is observable:

- claude / codex / cursor / opencode / pi → drain **and keep** the bounded 4096-byte tail, which is appended
  to the error message and then classified.
- grok / antigravity → drain and **discard**. Do *not* attach the tail capture: appending it would change L0
  error message text, the `classifyAdapterError` result, and the persisted `agent.last_error`, diverging from
  the oracle on a change we intended to be invisible.

**Acceptance:** all 8 `*-adapter.test.ts` files plus `adapter-error-kind`, `error-paths`,
`process-endurance`, `model-discovery` ported; **V14, V15 and V40 pass**; the fake-agent scripts used by the
parity harness exist and work.

---

### WP-3 — Templates, skills, workspace

**Owns:** `crates/orch-infra/src/{template,skills,workspace,clipboard}/**`, `fixtures/prompts/**`,
`xtask gen-prompts`.
**Port from:** `src/infrastructure/template/template-engine.ts`,
`src/infrastructure/skills/skill-loader.ts`, `src/infrastructure/workspace/*.ts`,
`src/infrastructure/clipboard-service.ts`.

**Delivers:** the Liquid-backed template engine with the §4.3 mitigations (always-present globals with
explicit nils; `shared_context` as an ordered pair array); both default templates copied byte-exact;
`build_prompt_context` and `filter_relevant_context`; the skill loader; the workspace manager (worktree,
isolated, cleanup, `get_changed_files`) and the merge strategy; the clipboard service.

**Acceptance:** `template-engine`, `skill-loader`, `workspace-manager`, `merge-strategy`,
`clipboard-service` tests ported; **V16, V17, V18, V19 pass** — V16 being the golden-prompt gate.

**Do first:** generate the golden prompts and get one rendering through `liquid` byte-exact. If that fails
after the §4.3 mitigations, escalate immediately rather than working around it — the fallback (a small
purpose-built renderer) is a day of work, but only if discovered early.

---

### WP-4 — Engine: services and orchestrator

**Owns:** `crates/orch-engine/**`.
**Port from:** `src/application/*.ts` (all 12 files), `src/container.ts`.

**Delivers:** `EventBus`; `TaskService`, `AgentService`, `RunService`, `GoalService`, `TeamService`,
`MessageService`, `AgentFactory`, `ReviewRunner`, `DoctorService`; the `Orchestrator` (tick loop, reconcile,
dispatch, collect, completion/failure handling, cascade failure, autonomous seeding, startup recovery,
shutdown); `build_light` / `build_full`.

**Acceptance:** all 25 `test/unit/application/**` files (~429 cases) ported and green; **V11, V21, V22, V23,
V24, V25, V26 pass**. This is the largest package by behaviour density — `orchestrator.ts` alone is 1949 lines
with a long bug-fix history (`orchestrator-bugfixes-2026-03.test.ts`, `-resilience`, `-retry-race`,
`-parallel-race`, `-perf-opts`, `-endurance`). **Port those regression tests before the code they cover.**

If this proves too large in practice, the clean split is WP-4a (the six services + event bus + container) and
WP-4b (the orchestrator), with 4b depending on 4a. Prefer the split if the engineer's first-session estimate
exceeds ~6k LOC.

---

### WP-5 — CLI and serve mode

**Owns:** `crates/orch-cli/**`.
**Port from:** `src/bin/cli.ts`, `src/cli/**` (all of it).

**Delivers:** the full clap command tree matching [PARITY.md §2.2](PARITY.md); `output` (glyphs, ASCII
fallback, `print_table` with its exact padding, `print_key_value`, the formatters); every command module with
both human and `--json` rendering; the error boundary and exit codes; `init`; `doctor`; `serve` with the
structured logger, `--once` runner and disk observer; `update`/update-check against the r-orch channel;
`editor` and `shop_picker`.

**Acceptance:** all 18 `test/unit/cli/**` files ported; **V1, V2, V3, V4, V5, V20 pass**. The differential
harness becomes meaningful the moment this package lands — it is the integration point.

---

### WP-6 — TUI

**Owns:** `crates/orch-tui/**`.
**Port from:** `src/tui/**` (10k LOC of Ink/React), `src/cli/commands/tui.ts`.
**Spec:** [docs/parity/tui.md](parity/tui.md) is the detailed brief; [PARITY.md §12](PARITY.md) is the
acceptance checklist.

**Delivers:** the four views with their lists and detail panels; the ordered key-dispatch chain; the command
bar with completion, suggestions and history; all wizards with their skip predicates and validation messages;
help overlay; both filter pickers; onboarding; toasts/bell/tab-flash; soft delete with undo; the five palettes;
the 80 ms batching, 2 KB detail caps and 500-entry LRU.

**Acceptance:** [PARITY.md §12](PARITY.md) checklist complete; **V27 passes**; the two F1 tests that were
unskippable in Ink are green.

**Risk: medium** — on **schedule** grounds (7k LOC), with its **correctness risk unmeasured rather than low.**

An earlier revision rated this the lowest-risk package because "it is L2, has no on-disk or wire contract, and
failures are visible immediately." All three clauses were wrong in the same way. The TUI is the **default
command** (bare `orch` launches it), so it is most users' entire experience; "no wire contract" is not "no
contract" — the **wizard → entity field mapping is L0** ([PARITY.md §12](PARITY.md)) because a wizard with an
inverted skip predicate writes corrupt L0 data; and "visible immediately" means visible *to a user*, which is
the argument for catching it first, not for relaxing the gate. A shipped inverted `skip` predicate or a
no-op filter picker would leave `cargo test`, `cargo xtask parity` and every V-row green, because none of them
would look.

Hence V33: the checklist is not the gate — the **counted `TestBackend` case inventory** is.

Do **not** port the dead code listed in [PARITY.md §13](PARITY.md).

---

### Integration order and parallelism

```
WP-0  foundation — freezes the trait surface
  │
  ▼
WP-0b walking skeleton + harness  ◄── HARD GATE (V39)
  │
  ├──► WP-1 storage  ────────┐
  ├──► WP-2 adapters ────────┤
  ├──► WP-3 templates ───────┤
  └──► WP-6 TUI ─────────────┤
                             │
                   ╔═════════▼═══════════════════════════╗
                   ║ TRAIT RE-SYNC CHECKPOINT             ║
                   ║ owner: WP-0 engineer                 ║
                   ║ trigger: WP-1, WP-2, WP-3 each have  ║
                   ║   one vertical slice                 ║
                   ║ one amendment round, then re-freeze  ║
                   ╚═════════┬═══════════════════════════╝
                             │
                             ├──► WP-4 engine ──► WP-5 CLI
                             │
                             └──► WP-6 wires to the real engine
```

- **WP-0b is a hard gate**: WP-4 and WP-5 may not begin until V39 is green. WP-1/2/3/6 may begin in parallel
  right after it.
- **WP-1, WP-2, WP-3 and WP-6 are fully parallel**, over disjoint directories, because none of their
  acceptance oracles depends on CLI integration:
  - WP-2's oracle is argv snapshots + stream-parsing fixtures, both derived from the TS **unit tests**.
  - WP-6's oracle is `TestBackend` cases written from `docs/parity/tui.md`; there is no TS oracle at all.
  - WP-3's oracle is golden prompts from the TS engine — **but only if generated the right way.**
    `src/index.ts` does **not** export the template engine or the default templates, so they are unreachable
    through the package entry point, and generating prompts via `orch run` + `run.prompt` would make WP-3
    depend on the skeleton. **Generate them with `npx tsx` against
    `parity/orch-ts/src/infrastructure/template/template-engine.ts` directly** (`tsx` is already a
    devDependency and `npm run dev` uses it). `xtask gen-prompts` must do this; discovering it at fixture time
    would stall WP-3.
- **The trait re-sync checkpoint is a scheduled slot, not a risk-register aspiration.** One amendment round,
  owned by the WP-0 engineer, triggered when WP-1/2/3 each have a vertical slice. Then the surface re-freezes.
- WP-4 compiles against the WP-0 traits and tests against `test_support` fakes, so it needs WP-1/2/3 only at
  *integration* time. WP-5 needs WP-4's service surface.

| Package | Est. LOC (impl + test) | Risk |
|---|---|---|
| WP-0 foundation | 3.5k + 1.5k | Medium — a wrong trait costs five packages a rework |
| WP-0b walking skeleton | 0.8k + 0.6k | Low effort, **highest leverage** — it turns the gate into a ratchet |
| WP-1 storage | 2.5k + 2k | **Highest** — silent cross-compat corruption |
| WP-2 adapters | 3.5k + 2.5k | Medium-high — eight external CLI formats, plus the stderr deadlock fix |
| WP-3 templates | 2k + 1.5k | Medium-high — the Liquid gate |
| WP-4 engine | 5k + 4k | High — largest behaviour surface, long bug-fix history |
| WP-5 CLI | 5k + 2k | Medium — wide but shallow |
| WP-6 TUI | 7k + 3k | **Medium** — on schedule grounds; correctness risk **unmeasured** until V33 |

---

## 10. Trade-offs

**Optimizes for:** exact external compatibility, and determinism in testing. Every significant decision —
the trait crate, the injected clock/ID seams, the differential harness as a merge gate, the post-emit YAML
quoting pass — buys one of those two.

**Gives up:**

- **Some idiomatic purity.** `async-trait` and `Arc<dyn _>` everywhere instead of generics and static
  dispatch. The cost is a vtable call on I/O paths, which is free relative to the syscall; the benefit is a
  container that can be assembled at runtime and faked wholesale, exactly as the TS one is.
- **Coarse locking.** Holding a `tokio::sync::Mutex` across file I/O is not what you would design from
  scratch. It *is* what the original does, and its bug history is specifically about that serialization. At
  this scale (one process, tens of agents) there is no throughput argument for changing it, and a real one for
  not changing it.
- **Bug-for-bug compatibility in five places** ([PARITY.md §14](PARITY.md)) — including
  `status --json`'s `"claimed": {}`. Each is individually defensible and collectively a little
  embarrassing. They are all documented and reversible.
- **A node dependency in CI.** The parity harness needs the TS binary. Without it the written spec is the only
  guarantee, which is a materially weaker position.
- **Self-update, deliberately.** The TS binary updates itself via npm after almost every command; r-orch does
  not update itself at all ([PARITY.md §11.5](PARITY.md)). This is the one place the project **declines**
  functional parity outright, on security grounds: a CLI overwriting its own executable as a side effect of
  `orch task list` is a supply-chain surface, not a feature, and it is unverifiable by the harness by
  construction. Users must update r-orch the way they installed it.
- **Interchangeable concurrent operation is documented, not guaranteed.** §2.1 — the index read-modify-write
  can lose updates across binaries, and no lock r-orch takes is visible to the TS binary.
- **Shelling out to `git`** rather than linking libgit2 — slower per call, but it is the only way to match
  exact command behaviour and exit codes, and it drops a C dependency.

---

## 11. Risks

| # | Risk | Severity | Mitigation / trigger |
|---|---|---|---|
| R1 | **YAML divergence** — timestamp quoting, merge keys, or a schema construct — silently bifurcating a project between the two binaries | **High** | **V32 (generalized js-yaml cross-parse) is the real mitigation**, not V8; V8 is one special case of it. Build V32 in WP-0b alongside the harness skeleton, not in WP-1. Merge keys are already a confirmed divergence — `apply_merge()` on every read ([PARITY.md §6.4.2](PARITY.md)). Escalation trigger for a hand-written emitter: any artifact where V32 reports a parse mismatch that quoting cannot fix. |
| R1b | **The harness itself is non-hermetic** — the TS oracle queries npm and writes to the real `$HOME`, making a required gate environment-dependent and host-mutating | **High** | V31: per-tmpdir `HOME`/`USERPROFILE`, pre-seeded `update-check.json`, assert no `npm` spawn and no socket; CI runs the parity job with networking disabled. |
| R2 | **`liquid` cannot reproduce prompt bytes** despite the mitigations | Medium-high | Golden fixtures first in WP-3. Measured evidence says whitespace and truthiness already match, so the residual risk is a construct I did not enumerate. Fallback: a ~400-line renderer for 7 constructs. |
| R3 | **Orchestrator behaviour regression** — a race the TS code already fixed and I did not transcribe | **High** | Port `orchestrator-bugfixes-2026-03`, `-resilience`, `-retry-race`, `-parallel-race`, `-endurance` **before** the orchestrator code. They are the encoded institutional memory. |
| R4 | **`broadcast` event loss** under high adapter output | Medium | Dedicated consumer task for the run-store writer; 4096 capacity; `Lagged` logged as a warning, never swallowed. If a *correctness* consumer ever needs lossless delivery, give it an `mpsc` instead. |
| R5 | **WP-0's trait surface is wrong**, forcing a cross-package rework | Medium | §5 is deliberately detailed so the freeze is informed. Mitigation if it happens: the trait crate is small, and `cargo` will point at every break. Budget one re-sync checkpoint after WP-1/2/3 each have a vertical slice. |
| R6 | **Differential harness built late**, so parity is asserted only by prose | Medium | **WP-0b is the mitigation** — a hard gate requiring one green differential and one green `rust_then_ts` scenario before WP-4/WP-5 begin (V39). Building the *harness* early without the *measurable surface* satisfies this risk's letter and none of its intent. `cargo xtask parity` is required for merge from that point on. |
| R6b | **Live/streaming behaviour is prose-verified only** — `tui`, watch-mode `serve`, `run --watch` and `logs --follow` cannot be expressed as differential scenarios, so §7's timing constants rest on this document's reading of `orchestrator.ts` | Medium-high | V34–V36 pull the highest-density parts back into oracle-verified territory via `serve --once` (stall past timeout, killed mid-run, concurrency saturation). The residual gap is **stated explicitly** in [PARITY.md §15.1](PARITY.md) rather than implied. Port the regression tests named in R3 before the code. |
| R7 | **Adapter argv drift** — an external CLI changes its flags | Low (for parity) | Parity is against the pinned commit, not against the live CLIs. Snapshot tests pin our argv; real-CLI breakage is a separate, post-parity concern. |
| R8 | **`kstring` MSRV and similar toolchain friction** recurs as dependencies move | Low | `rust-toolchain.toml` pins the toolchain; `Cargo.lock` is committed; the `kstring = "2.0.2"` pin is commented with its reason. |
| R9 | **TUI scope creep** — 10k LOC of React, much of it dead | Medium (schedule) | [PARITY.md §13](PARITY.md) lists exactly what is not ported. WP-6 is checklist-driven; it is also the most deferrable package. |
| R10 | **Process-group kill behaves differently** than node's `detached` on some platform | Low | `process-endurance` tests spawn real `bash`/`sleep` and assert the tree dies. These are skipped under CI in the original; **run them everywhere** in Rust. |
| R11 | **An adapter that does not drain stderr deadlocks** on a chatty child once the pipe fills (~64 KB) | Medium | This is a live bug in the original's `grok` and `antigravity` adapters ([PARITY.md §14.12](PARITY.md)) — fixed, not replicated. Structural mitigation: make the stderr tail capture part of the **shared** spawn path in WP-2 so an adapter cannot forget it, rather than per-adapter opt-in. Add a test per adapter that writes 256 KB to stderr and asserts completion. |

### Rollback

There is nothing to roll back *to* — this is a greenfield reimplementation, and the TS binary continues to
exist and work. The meaningful failure mode is shipping something that corrupts a project a user also opens
with the TS binary, which is what R1/V7/V8 exist to prevent. Until the parity harness is green across the
cross-compat scenarios, r-orch should refuse to write to a `.orchestry/` directory it did not create, behind a
`--allow-foreign-project` flag. That is the only real safety valve and it costs almost nothing.

---

## 12. Alternatives considered

**Single crate.** Simplest build, fastest compile. **Rejected:** the brief's hard requirement is that the
engine not depend on the CLI or TUI, and nothing but a crate boundary enforces that. Within a single crate it
would be a convention that decays in a month.

**Finer crate split (`orch-storage`, `orch-adapters`, `orch-templates`, …).** More enforcement, better
incremental compiles. **Rejected for now:** six more manifests to buy isolation the `orch-core` traits already
provide. The trait boundary makes extraction mechanical if adapter work becomes a bottleneck.

**No `orch-core`; put traits in `orch-domain`.** One fewer crate. **Rejected:** `orch-domain` would then
depend on `async-trait`, `futures` and `tokio` types, which destroys the "pure types, no I/O, no async"
property that makes the domain trivially testable and fast to compile.

**`serde_yaml` (deprecated) because it is the most battle-tested.** **Rejected:** it is archived; its own
version string says `+deprecated`. `serde_yaml_ng` is the same API, maintained, and verified against our real
data.

**Hand-written YAML emitter from the start** (via `saphyr`/`yaml-rust2`). Total control over scalar styles,
which is what R1 needs. **Rejected as the default:** it means hand-writing serialization for seven entity
types and reimplementing quoting rules we would then have to get right from scratch. The post-emit quoting
pass is ~20 lines in one function with a direct test. Kept as the documented escalation.

**Write our own Liquid subset renderer immediately.** Removes R2 entirely and is genuinely small — ORCH uses
seven constructs and zero custom filters. **Rejected as the default:** measurement showed `liquid` already
matches on whitespace and truthiness, which were the hard parts, and a maintained crate handles the
constructs we have not thought of in user-supplied templates. Kept as the WP-3 contingency with a defined
trigger.

**`git2`/libgit2 instead of shelling out.** Faster, no subprocess, typed errors. **Rejected:** parity is
specified in terms of exact `git` command lines and their exit codes, including `git worktree prune` and
`git merge --abort` semantics that libgit2 models differently. Shelling out is both more faithful and one
fewer C dependency.

**`chrono` over `jiff`.** More ubiquitous, more examples. **Rejected narrowly:** we need exactly one format
(RFC 3339, millisecond precision, `Z`) and `jiff` expresses that without timezone-database coupling. This is
the decision I hold most loosely — `chrono` is a one-file change if `jiff` creates friction.

**Reuse the TS test suite via a node bridge** instead of porting ~2085 cases. Enormous effort saving.
**Rejected:** it would test the Rust code only through whatever surface the bridge exposes, give up unit-level
coverage of pure functions entirely, and leave the flakiness in place. The differential harness captures the
genuine cross-implementation value of the TS suite; the unit tests are worth porting properly.

**Port the TUI tests as-is, sleeps included.** Cheapest path through 681 cases. **Rejected:** those sleeps are
the direct cause of the 5–6 flaky failures in the reference suite. `TestBackend` makes them synchronous, which
is strictly better and no more work.
