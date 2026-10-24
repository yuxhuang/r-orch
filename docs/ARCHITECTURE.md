# r-orch Architecture

Design of r-orch: crate boundaries, traits, concurrency and error model, and test strategy. The README says
what users see; this document says how it is built.

---

## 1. Context

r-orch is an AI-agent orchestrator: a CLI, a TUI, and a long-running engine that spawns external AI coding CLIs
(`claude`, `codex`, `cursor`, …) as child processes, streams their JSONL output, drives a task state machine,
and persists everything as plain files under `.orchestry/`. It is a layered Rust workspace with trait seams for
every external effect, so the engine is testable deterministically without real processes, clocks or AI CLIs.
For the runtime flows (scheduling, handoffs, goals, pause, and how adapters drive each CLI), see
[docs/internal/](internal/README.md).

**No distributed system here.** (Optional peer links delegate work between installations; each project still has one engine.) One process per project, guarded by a PID lock file, with a filesystem as the
database. The hard problems are (a) child-process lifecycle and cancellation, (b) exact, stable
serialization of the on-disk and `--json` formats, and (c) not regressing a state machine that has had many bug fixes layered into it.

---

## 2. Assumptions

Load-bearing, stated so they can be challenged:

| # | Assumption | If wrong |
|---|---|---|
| A1 | **Scale is small.** Tens of agents, hundreds of tasks, thousands of run events per run. The caps (16 KB lines, 8 KB events, 1024-event buffer, 500-entry LRU) are generous. | Nothing in the design changes; the file-per-entity store would need an index rework only at ~10⁵ tasks. |
| A2 | **Single *orchestrator*, multiple concurrent mutators.** The PID lock serializes *orchestrators* only. One-shot commands never take it, and some "reads" write. See A2-detail below. | Not a hypothetical: it is true today, so it is documented rather than assumed away. |
| A3 | **POSIX only.** r-orch uses process groups (`kill(-pid)`), `bash -lc`, `rsync`, and `O_APPEND` atomicity. Windows support is already partial at best. | Windows would need a separate process-group strategy (job objects) and no `bash`. Out of scope; keep platform code behind a `cfg` boundary so it can be added. |
| A4 | **Latency budget is human-scale.** Tick interval 10 s; CLI commands should feel instant (<100 ms). The container tiering exists purely to keep read-only commands fast. | Rust startup is ~1 ms either way, so the tiering becomes an optional optimization rather than a requirement. |
| A5 | **Prompt text must be byte-exact** because agents are prompt-sensitive and the system prompt is a cache key. | If approximate were acceptable, the Liquid risk (§4.3) would mostly evaporate. It is not acceptable. |
| A6 | **No external Rust consumer of the library crates yet.** | Public API stability is not a constraint; we are free to pick idiomatic signatures. |

### 2.1 A2 in detail — the concurrency story is weaker than "single writer"

The PID lock serializes *orchestrators*, not every writer. Interchangeable use of several processes on one
project directory is a normal workflow, which multiplies the exposure. Three facts:

1. **One-shot commands never take the lock.** Only one
   `orch run --watch` can run at a time; one-shot commands do not acquire the lock. So `orch task add` while
   the orchestrator runs is a second mutator — and that is the *normal* workflow, not an edge case.
2. **Some reads write.** Reading an index that is missing or corrupt rebuilds it and **writes `_index.json`**.
   `orch task list` on a fresh project therefore creates directories and writes
   a file.
3. **The mutex is in-process only.** It is invisible to a second r-orch process.

**Lost-update mechanism, stated plainly.** Two processes interleaving the `_index.json` read-modify-write can
drop entries: `tasks/tsk_X.yml` exists on disk but is absent from the index, so `task list`, `status` and
**dispatch** do not see the task, while `task show tsk_X` does. It self-heals only when the index is deleted or
corrupted into a rebuild.

**This is not fixed by locking today.** Index writes take no cross-process lock, so the honest response is to
**document the behaviour and verify that it is what we documented**, not to claim a guarantee we cannot provide.
Concurrent mutation while an orchestrator is running is a **known, documented hazard**, written up for
operators, with the one-command recovery, in [docs/RUNBOOK.md](RUNBOOK.md#rebuilding-an-index-after-a-lost-update).

**`update` and `patch` add one narrow, new lock — not a fix for the above.** `orch agent|task|config update|patch`
([guide](file-mutations.md)) take a stable advisory lock, `.orchestry/.definition.lock`, for at most 5 seconds
(`fs_utils::mutation_lock`), so two of *those* commands cannot interleave their read-merge-write. `edit`, `agent add`,
the orchestrator and the dashboard do not take it, and agent and task records are still saved through their stores
and indexes, one write each, so the hazard in this section is unchanged for everything else.

---

## 3. Crate layout

```
r-orch/
├── Cargo.toml                 # workspace root, shared [workspace.dependencies]
├── rust-toolchain.toml        # pin 1.99.1, edition 2024
├── crates/
│   ├── orch-domain/           # pure types + pure functions. NO I/O, NO async.
│   ├── orch-core/             # the port traits (storage, adapter, process, workspace, clock, ids)
│   ├── orch-infra/            # implementations of orch-core traits
│   ├── orch-engine/           # services + orchestrator + event bus + container
│   ├── orch-peer/             # remote-peer protocol: links, mirrors, reconnect (core, domain, engine, infra)
│   ├── orch-cli/              # clap command tree, human + JSON rendering, serve mode. bins live here.
│   └── orch-tui/              # ratatui dashboard
├── xtask/                     # dev tooling: crate-graph, gate.sh, mouse-e2e
├── fixtures/                  # golden prompts, adapter stdout recordings, shop catalogue, fake agents
└── skills/library/            # shipped skill markdown
```

### 3.1 Dependency direction

```
            orch-domain                     (no deps beyond serde/thiserror)
                 ▲
                 │
             orch-core                      (traits only; depends on orch-domain)
                 ▲
        ┌────────┴────────┐
    orch-infra ◄───── orch-engine           (engine → infra: 5 sites, see below)
        ▲                 ▲
        └────────┬────────┘
                 │
             orch-peer                      (remote-peer protocol; composes engine + infra)
                 │
             orch-cli                       (top of the stack; may name anything below)
                 │
                 ▼  implements TuiBackend
             orch-tui                       (depends on orch-domain ONLY)
```

Two things in this diagram were wrong in earlier revisions, and both are now enforced rather than described.

**1. `orch-engine` does depend on `orch-infra`.** An earlier revision said "engine depends on core, NOT on
infra". The edge is real, and it is **five** non-test sites:

| Site | What it names | Why it is acceptable |
|---|---|---|
| `container.rs` | every concrete infra type it assembles — stores, adapters, `LiquidTemplateEngine`, `GitWorkspaceManager`, `SystemProcessManager`, `FileSkillLoader`, `NanoIdGenerator`, `SystemClock`, `REGISTRATION_ORDER` | Assembly has to name implementations somewhere. The composition root. |
| `orchestrator.rs` | `CachedTaskStore`, `CachedAgentStore`, `CachedGoalStore` | The orchestrator constructs the tick-scoped cache wrappers itself. |
| `orchestrator/dispatch.rs` | `DEFAULT_SYSTEM_TEMPLATE`, `DEFAULT_USER_TEMPLATE`, `build_prompt_context`, `BuildPromptOptions` | The orchestrator builds the default prompt from exactly these. |
| `task_service.rs` | `Paths`, `fs_utils` — for attachment paths | Attachment paths need the project layout. |
| `lock.rs` | `storage::lock`, re-exporting `LOCK_STALE` and `LockResult` | The lock *mechanism* is infra; `ProjectLock` is a thin wrapper over it. |

**What actually holds is the narrower, more useful property: every *service* takes only `orch-core` traits.**
Concrete types appear at assembly and in the four modules above. So
`orch-engine` remains unit-testable against `test_support` fakes, and `orch-infra` is still swappable
wholesale — which is what the boundary was for.

If the edge ever needs severing, the moves are known: lift `storage::cached` into `orch-core` (already
generic over the traits) and the two template constants into `orch-domain`. Neither is worth doing — both
would add churn for no behavioural gain.

**2. `orch-tui` depends on `orch-domain` alone — not on the engine.** An earlier revision of this diagram had
the TUI depending on `orch-engine`. WP-6 **inverted** it: everything the dashboard needs crosses a single
trait, `orch_tui::backend::TuiBackend` (`crates/orch-tui/BACKEND.md`), so the TUI names only
the domain types that pass through it. `orch-cli` implements that trait over the DI container
(`crates/orch-cli/src/tui/backend.rs`), and the TUI's own tests use an in-crate `FakeBackend`. **The real
layering is tighter than this document originally specified** — a genuine improvement, recorded rather than
quietly absorbed.

**The authoritative enumeration lives in the check, not in this prose.** `ENGINE_INFRA_SITES` in
`xtask/src/crate_graph.rs` lists each permitted site with its justification, and it is bidirectional: **a new
engine file naming `orch_infra` fails the build until it is justified, and a stale entry fails too.** The
stale-entry half is what caught the TUI inversion above — it turned "someone quietly improved the layering"
into a failure somebody had to look at, rather than a diff nobody read. The table above keeps the *rationale*;
the check keeps the *list*. If they disagree, the check is right.

**Enforced invariant: `orch-engine` has zero dependency on `orch-cli` or `orch-tui`.** The core engine has no
knowledge of the CLI or TUI layers.

`xtask crate-graph` asserts the **whole edge set** against a table, not just that one forbidden edge —
including build- and dev-dependencies, since a test-only edge still compiles the crate. A single
forbidden-edge check would pass a workspace in which `orch-infra` had started depending on `orch-engine`,
inverting the layering while satisfying the letter of the rule.

**Why `orch-core` exists as a separate crate from `orch-domain`.** The orchestrator depends on the storage and
adapter interfaces, which are implemented in infrastructure. That is a dependency cycle unless the traits live
in their own crate. `orch-core` holds **only** trait definitions and the types that appear in their
signatures. It is what lets `orch-engine` be unit-testable with in-memory fakes and lets `orch-infra` be
swapped wholesale.

**Why `orch-infra` is one crate, not five.** Splitting storage / adapters / process / workspace / templates
into separate crates would add five `Cargo.toml` files and compile-graph edges to buy an isolation we already
get from the `orch-core` traits. One crate with clear modules (`storage`, `adapters`, `process`, `workspace`,
`template`, `skills`, `clipboard`, `models`) is the right granularity at this size. If adapter development
becomes a bottleneck we can extract `orch-adapters` later — the trait boundary makes that a mechanical move.

### 3.2 Binaries

All three names are the same program:

```toml
# crates/orch-cli/Cargo.toml
[[bin]] name = "orch"      path = "src/bin/main.rs"
[[bin]] name = "orchestry" path = "src/bin/main.rs"
[[bin]] name = "ao"        path = "src/bin/main.rs"
```

(A single `main.rs` shared by three `[[bin]]` targets compiles three times. If that becomes annoying, make
`orchestry` and `ao` thin shims that `pub use orch_cli::main`.) The clap program name is `orchestry`.

### 3.3 Module map per crate

| Crate | Modules |
|---|---|
| `orch-domain` | `task`, `agent`, `run`, `goal`, `team`, `message`, `state`, `config`, `global_config`, `events`, `error`, `transitions`, `scope`, `model_tiers`, `shop` (agent-shop + org-shop + default-agents) |
| `orch-core` | `storage` (9 traits), `adapter` (`Adapter`, `AgentEvent`, `ExecuteParams`, `Registry`), `process` (`ProcessManager`), `workspace` (`WorkspaceManager`), `template` (`TemplateEngine`), `skills` (`SkillLoader`), `clock` (`Clock`), `ids` (`IdGenerator`) |
| `orch-infra` | `storage::{paths, fs_utils, index, task, agent, run, state, config, global_config, context, message, goal, team, cached, lock}`, `adapters::{claude, codex, cursor, opencode, pi, grok, antigravity, shell, util, buffer, registry}`, `process`, `workspace::{manager, merge}`, `template`, `skills`, `clipboard`, `models` |
| `orch-engine` | `events` (bus), `task_service`, `agent_service`, `run_service`, `goal_service`, `team_service`, `message_service`, `agent_factory`, `review_runner`, `doctor_service`, `orchestrator`, `container` |
| `orch-cli` | `commands::{task, agent, status, logs, config, context, msg, goal, team, org, run, doctor, tui, serve, init, update}`, `output` (human rendering and the `--json`/`--yaml` `StructuredFormat`), `context`, `document_input` (bounded JSON/YAML input and `apply_mutation` for `update`/`patch`), `editor`, `shop_picker`, `update_check`, `serve::{logger, once, observer}` |
| `orch-tui` | **`backend`** (the `TuiBackend` trait + an in-crate `FakeBackend` — the crate's only seam to the engine, §3.1), `app`, `state`, `views::{goals, tasks, agents, logs}`, `widgets::*`, `command_bar`, `wizard`, `theme`, `keys` |

### 3.4 Organization templates

A template is data plus four small layers, each in the crate that already owns that kind of work:

| Layer | Owns |
|---|---|
| `orch-domain::org_templates` | The authoring types (`OrgDefinition`, `AgentDefinition`, `Bundle`), identifier rules, the limits (1 MiB per document, 16 MiB, 128 agents, 64 rules), rule-reference parsing and role rendering (`Bundle::render_role`), and the embedded a-team files (`builtin`). Pure: no I/O. |
| `orch-infra::storage::org_templates` | Reading a directory as a bounded no-follow snapshot (`RawSnapshot`), whole-bundle validation, resolution (`resolve`), the lock and atomic publication (`LockedTemplate`), `init`. The only layer that touches template files. |
| `orch-engine::org_templates` | `resolve_inputs`: the one path from a `Bundle` to agent inputs, used by `deploy`, `show --resolved` and the shop; `preflight`; `deploy_bundle`, which drives the ordinary agent, team and goal services and reports survivors. |
| `orch-cli` | `commands::org_template` (the `org template …` commands), `commands::org` (`deploy`), `template_shop`, and `document_input`, shared with the live-definition `update`/`patch` commands. |

Data flows one way: directory or embedded files → `RawSnapshot` → `Bundle` (validated) → `resolve_inputs` →
`CreateAgentInput` → the existing services. Because preview, deploy and shop share `resolve_inputs`, they cannot
disagree about a role. A deployed agent is an ordinary record with the rules already embedded in its role; it keeps no
reference to the template.

Mutations take `.template.lock`, snapshot the files, build a candidate snapshot with one file replaced, added or
removed, validate **all** of it, and only then write that one file (temporary file and rename). Nothing about this is
transactional across files or against other writers, and `deploy` is deliberately not transactional either; see
[org-templates.md](org-templates.md#concurrency-and-limits) and [ADR 0006](decisions/0006-org-templates.md). New
parsing guards (explicit YAML tags, nesting depth) sit on top of the existing `parse_yaml_bytes` guard and
`saphyr-parser`; `json-patch` ([ADR 0005](decisions/0005-file-mutations.md)) provides RFC 7396.

---

## 4. Dependencies

All versions verified present on crates.io via `cargo search` on 2026-10-13 against cargo 1.94.1 / rustc 1.94.1.
Pin minors in `[workspace.dependencies]`; let patches float.

### 4.1 Core

| Crate | Version | Why |
|---|---|---|
| `tokio` | `1.53` | The async runtime. Needed for: concurrent child-process stdout/stderr draining (the shell adapter deadlocks without it), `select!` for cancellation, and timers. Features: `rt-multi-thread`, `macros`, `fs`, `io-util`, `process`, `signal`, `sync`, `time`. |
| `futures` | `0.3` | `Stream`/`StreamExt` for the adapter event streams. |
| `async-trait` | `0.1` | Async methods in the `orch-core` traits. Native AFIT cannot produce `dyn`-compatible traits, and the container stores `Arc<dyn ...>` — so `async-trait` is required, not stylistic. |
| `serde` | `1.0` | derive. |
| `serde_json` | `1.0` | JSON + JSONL. **Enable the `preserve_order` feature** so `Value` round-trips key order; and prefer typed structs over `json!` for anything persisted (see §5.2). |
| `json-patch` | `4.2` (`default-features = false`) | RFC 7396 JSON Merge Patch for `orch {agent,task,config} patch -f`. Only `json_patch::merge` is used, in `orch-cli`'s `document_input`. Adoption checks in [ADR 0005](decisions/0005-file-mutations.md). |
| `thiserror` | `2.0` | The error enum in `orch-domain`. |
| `anyhow` | `1.0` | Only in `orch-cli`/`xtask` top-level glue, never in library error signatures. |
| `clap` | `4.6` | derive. Subcommands, aliases, `required`, defaults, global args. |
| `nanoid` | `0.5` | `nanoid::alphabet::SAFE` is the URL-safe 64-character set the ID format uses, so no custom alphabet is needed. |

### 4.2 YAML — the one genuinely awkward choice

`serde_yaml` is **archived and deprecated**; its last release is literally versioned `0.9.34+deprecated`.
Three maintained options:

| Candidate | Version | Assessment |
|---|---|---|
| **`serde_yaml_ng`** | `0.10.0` | A direct, actively maintained continuation of `serde_yaml` with the same API. **Chosen.** |
| **`saphyr-parser`** | `0.2` | **Also adopted**, for a different job: the event-streaming parser behind the expanded-node pre-pass. Not a `serde_yaml_ng` replacement — see below. |
| `serde_norway` | `0.9.42` | Also a maintained `serde_yaml` fork, same API. Verified to read and emit identically to `serde_yaml_ng` on our data. A drop-in fallback if `_ng` stalls. |
| `saphyr` / `yaml-rust2` | `0.2.1` / `0.13.0` | Full YAML 1.2 engines, but **no serde integration** — we would hand-write (de)serialization for 7 entity types. Rejected as a default; see §5.3 for where a raw emitter would be needed anyway. |

**Decision: `serde_yaml_ng = "0.10"`.** Re-confirmed at the gate against measured alternatives rather than
left as a first-pass choice:



| Candidate | DoS behaviour on the C0 bombs | Why not |
|---|---|---|
| **`serde_yaml_ng` 0.10** — chosen | 5 s / 65 s *unguarded*; **refused in 18 ms** with the expanded-node pre-pass in front of it | — |
| `serde-saphyr` | **excellent — ~80 ms refusal** unaided | **No dynamic `Value` model and no `apply_merge`.** Adopting it would mean hand-writing merge-key resolution, the timestamp quoting guard (§5.3) and surrogate repair — three wire-contract behaviours — on top of a parser that cannot express them. Rejected on **API fit**, not performance |
| `serde_norway` 0.9 | 5 s / 65 s — same libyaml design | No advantage; retained only as a drop-in if `_ng` stalls |

Both chosen and fallback were tested against real YAML files (including the folded-scalar `>-` 3 KB
`agt_creator` role) and read it correctly.

**`saphyr-parser` 0.2 is a direct dependency** — and note which half of the saphyr family we took.
`serde-saphyr` was rejected above for having no `Value` model, but `saphyr-parser`, the **event-streaming
parser underneath it**, is exactly the right tool for the expanded-node pre-pass: it streams events without
resolving aliases, which is what makes an exact expansion count O(input) instead of O(expansion). So the
family is split by job — `serde_yaml_ng` for serde deserialisation, `saphyr-parser` for the guard in front of
it.

**`unsafe-libyaml` (RUSTSEC-2024-0449, unmaintained) is accepted**, because the expanded-node pre-pass closes the
amplification class **before the parser is reached** — refusing the shape that defeated every prior guard in
**18 ms**, while the worst legitimate 1 MiB document still parses in 161 ms. The guard is needed regardless
of crate: `serde_norway` measures identically on the same inputs, so this is not a crate-specific weakness.
**Revisit if `serde-saphyr` gains a `Value` API**, at which point the API-fit objection disappears and it
becomes the better choice on both axes.

**`--yaml` output does not go through `serde_yaml_ng`.** `orch-cli`'s `output::StructuredFormat` renders the
`serde_json::Value` the JSON path would print with a small emitter of its own, so YAML and JSON carry the same
data in the same key order. The emitter writes every key and string double-quoted (JSON string escaping, plus
`\uXXXX` for characters YAML readers treat as line breaks or BOMs) so YAML 1.1 and 1.2 readers agree on types, and
it never emits a tag. This is the §4.8 exemption "no maintained crate fits": the code comment records it and
[ADR 0007](decisions/0007-hand-written-yaml-output-emitter.md) the reasoning. Parsing, including the guards above, is unchanged.

### 4.3 Templates — the highest-risk dependency

| Crate | Version | Notes |
|---|---|---|
| `liquid` | `0.26.11` | The only maintained Liquid implementation for Rust. `kstring` floats (the 2.0.2 MSRV pin went with the 1.99 toolchain); its 2.0.3+ crate-wide deprecation is allowed in `template/lenient.rs`, whose trait impls must name `KStringCow`. |

`liquid` 0.26.11 was measured on the exact constructs the default templates use. Summary:

- **Whitespace handling is exact**, including the `\n\n\n` runs the real prompts produce. This was the biggest
  unknown and it came out in our favour.
- Truthiness, `.size`, `!=`, `>`, `default:`, inline `if`, and `for`-over-objects all behave as the templates
  expect.
- Two real quirks, both mitigable in the *context builder* rather than the engine:
  1. Missing keys error instead of rendering empty → **always emit every documented variable, using explicit
     `nil` for absent optionals.** Verified identical once done.
  2. Object iteration order is unspecified → **pass `shared_context` as an ordered array of `[k, v]` pairs**,
     which renders identically through `entry[0]`/`entry[1]` and fixes ordering. Verified.
- One accepted divergence: unknown filters fail at parse time instead of passing through. Only reachable via
  user-supplied templates in `config.prompt.*`.

**The gate is golden fixtures, not reasoning.** The renderer must reproduce the golden prompts in
`fixtures/prompts/` byte for byte across the full branch matrix (§8.3). If it cannot, the contingency is
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
| `globset` | `0.4` | Available if scope matching ever becomes real glob matching. **Not used for `scopesOverlap`**, which is deliberately raw string-prefix logic and must stay that way. |
| `tracing` + `tracing-subscriber` | `0.1` / `0.3` | Internal diagnostics. **Must not** write to stdout — stdout is the CLI contract. Default to stderr at `warn`, controlled by `ORCHESTRY_DEBUG`/`RUST_LOG`. |
| `indexmap` | `2.14` | Insertion-ordered maps where key order is part of the format. |
| `toml` | `1.1` | Only for our own tooling/config, not for `.orchestry/`. |

### 4.5.1 `orch web` (`orch-cli` only)

| Crate | Version | Why |
|---|---|---|
| `axum` | `0.8` (`http1`, `tokio`, `ws`) | Router, middleware and websocket upgrade for `orch web`. Features are minimal: no `json`, `form`, `query` or HTTP/2. Rejected: a hand-rolled server over `hyper` + `tokio-tungstenite` (more code for the same routing, middleware and upgrade hooks); `actix-web` (a second runtime model next to tokio). |
| `portable-pty` | `0.9` | Opens the pseudo-terminal `orch tui` runs in. Rejected: `nix::pty::openpty` plus hand-rolled spawn, `setsid` and `TIOCSCTTY` (this crate forbids `unsafe`, and portable-pty already does it and handles resize); `pty-process` (async API, but smaller and less used). Its I/O is blocking, so it runs on plain threads bridged by channels. |
| `base64` | `0.22` | Decodes the HTTP Basic header. Already in the tree via axum. |
| `sha2` + `subtle` | `0.10` / `2.6` | Credentials are compared as SHA-256 digests with `subtle`'s constant-time equality: fixed-length inputs, so neither content nor length leaks. `sha2` was already in the tree; `subtle` is new and tiny. Rejected: a hand-written compare (easy to get subtly wrong, and optimisers can undo it). |
| `nanoid` | `0.5` | The 128-bit access token. Already how the workspace makes ids; its default RNG is an OS-seeded `StdRng`. Rejected: adding `rand` or `getrandom` directly for one call. |
| `nix` (`signal`) | `0.31` | SIGHUP, then SIGKILL after a grace period, to the pty child. `portable-pty`'s own `kill()` escalates in about 200 ms, too fast for `orch tui` to stop its agents. |
| `tokio-tungstenite` | `0.29` (dev) | Websocket client for the bridge tests. Pinned to the `tungstenite` axum already uses, so there is one copy in the graph. |

Windows is out of scope: the cargo-deny graph is Linux-only (`deny.toml`) and `portable-pty` pulls in
Windows-only crates the gate never resolves.

### 4.6 Dev-dependencies

| Crate | Version | Why |
|---|---|---|
| `insta` | `1.49` | Snapshot tests for adapter argv, rendered prompts, human CLI output. |
| `assert_cmd` | `2.2` | Running our own binary in integration tests. |
| `predicates` | `3.1` | Assertions on command output. |
| `tempfile` | `3.27` | Isolated project dirs per test. |
| `proptest` | *(add when needed)* | For the scope-overlap equivalence property and the ID-format property. |

### 4.7 Deliberately not used

- **`git2`** (`0.21`) — binds libgit2. The workspace manager uses *exact git command lines*
  (`git worktree add … -b …`, `git worktree prune`, `git merge --no-ff … -m …`, `git merge --abort`), and
  depends on their exact behaviour and exit codes, including `worktree prune` semantics libgit2 models
  differently. **Shelling out to `git` is the correct choice here** and it also removes a C dependency from
  the build. Decision recorded because it looks backwards at first glance.
- **`notify`** (`9.0.0-rc.5`) — only a release candidate, and the serve-mode disk observer polls.
  Keep polling.
- **`sysinfo`** — overkill. PID liveness is `kill(pid, 0)` via `nix`.
- **`libc` 1.0** — still alpha (`1.0.0-alpha.5`). `nix` 0.31 pulls a stable `libc` 0.2; let it.

---

### 4.8 Library-first policy (user directive, 2026-10-13)

**Where a well-supported third-party crate covers a complex feature, use it instead of hand-rolling.**
Hand-roll only when (a) no maintained crate fits, or (b) a documented format or behaviour requirement rules the crate
out — and say which in a code comment. Candidates for areas that would otherwise be hand-written (verify
version compatibility with `ratatui` 0.30 / the pinned toolchain before adopting; record the choice in
`[workspace.dependencies]`):

| Area | Crate(s) | Note |
|---|---|---|
| Process groups / session spawn + group kill (tokio) | `process-wrap` 10 (tokio feature) | Prefer over hand-rolled `setsid` + `kill(-pid)`; keep `nix` for `kill(pid, 0)` liveness if `process-wrap` lacks it |
| TUI multi-line/single-line text editing (wizards, command bar, undo) | `tui-textarea` / `tui-input` (or a maintained ratatui-0.30-compatible fork) | Wrap the widget, don't rewrite an editor |
| Grapheme segmentation / display width  | `unicode-segmentation`, `unicode-width` | Cursor movement and column layout |
| Clipboard text/image (TUI paste) | `arboard` (with image support) | Degrade to "empty" on any failure |
| Diffs in test failures | `pretty_assertions` | |
| Version comparison (update check) | `semver` | |
| Templates | `liquid` (already chosen) | The ~400-line fallback renderer is a last resort only, after escalation |

Things that **stay hand-written because the format or behaviour is pinned**: the PID lock-file format,
`scopesOverlap`'s raw string-prefix logic, the line reader's 16 KB cap/empty-line semantics, the retry backoff
formula, the `--since` duration regex, the table renderer's exact padding, and the atomic-write temp naming.

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
    /// `logs --follow` subscribes to the EventBus instead.
    fn stream_events(&self, run_id: &str, token: CancellationToken)
        -> BoxStream<'static, Result<RunEvent>>;
    /// Release the cached O_APPEND handle for this run.
    fn close_run_events(&self, run_id: &str);
}

// Plus: StateStore, ConfigStore, ContextStore, MessageStore, GoalStore, TeamStore
// — same style: `&self`, async, `Result` return.
```

Notes:
- `&self`, not `&mut self`: stores are shared as `Arc<dyn TaskStore>` and own their interior mutability
  (the index mutex).
- `TaskFilter` as a struct rather than two `Option` parameters, so adding a filter dimension is not a
  signature break.
- `close_run_events` is **not** async and should stay sync — it is a cache eviction.

### 5.2 Serialization helpers (the on-disk format)

```rust
// orch-infra/src/storage/fs_utils.rs

/// Durable atomic write. Temp name: `.{basename}.{8 hex}.tmp` in the same directory.
/// Order: write temp -> **fsync the temp file** -> rename over the target ->
/// **fsync the parent directory**. Both fsyncs are required: without the first,
/// a power loss can make the rename durable while the data is not; without the
/// second, the rename itself may not survive. rename() alone is only safe
/// against *process* crashes, not machine crashes (sre C5).
pub async fn atomic_write(path: &Path, content: &str) -> Result<()>;

/// `serde_json::to_string_pretty` + '\n'. 2-space indent, trailing newline.
pub async fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()>;

/// YAML emission that YAML 1.1 readers parse back to the same types. See `quote_ambiguous_scalars`.
pub async fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()>;

/// Compact JSON + '\n' appended through a cached O_APPEND handle, with the
/// PIPE_BUF truncation rule: a record over the limit is truncated so one write stays atomic.
pub async fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<()>;
```

Two rules every entity struct must follow, or the on-disk format drifts silently:

1. **Every optional field** carries `#[serde(skip_serializing_if = "Option::is_none")]`: absent is *omitted*,
   not null. The sole exception is `Message::to_agent_id`, which must emit `null`.
2. **Never use `serde_json::json!` for a persisted record.** `serde_json::Map` defaults to `BTreeMap`, so
   `json!` **alphabetizes** keys — a JSONL event came out `{"data":…,"timestamp":…,"type":…}` in testing
   instead of `{"timestamp":…,"type":…,"data":…}`. Derived structs emit declaration order. Use typed structs,
   and enable `preserve_order` as belt-and-braces.

### 5.3 The YAML timestamp guard

This is the one place where correctness required measurement. The YAML 1.1 schema includes a `timestamp`
type, so an **unquoted** `created_at: 2026-10-13T16:37:58.959Z` is read by a YAML 1.1 reader as a date, not a
string — and any consumer that then treats it as text fails. Both `serde_yaml_ng` and `serde_norway` emit such scalars unquoted.

```rust
/// Post-emit pass over serialized YAML: force single-quoting on any plain scalar that
/// a YAML 1.1 reader would resolve as a timestamp or date.
/// Operates on the emitted document, keyed off the scalar grammar — not on field names,
/// so a user-supplied `description` that happens to look like a date is also protected.
fn quote_ambiguous_scalars(yaml: &str) -> String;
```

Implemented inside `write_yaml` so no caller can forget it. **Tests:** `crates/orch-domain/tests/yaml_emission.rs`
pins which scalars `serde_yaml_ng` emits unquoted (the hazard the guard exists for) and that the domain types
round-trip through `serde_yaml_ng`; the `fs_utils` unit tests pin the quoting guard against a hand-measured
YAML 1.1 date table; `crates/orch-infra/tests/store_round_trip.rs` writes every entity through the real stores
and checks the written bytes for any timestamp-shaped scalar left unquoted. No YAML 1.1 parser is involved. If the post-emit pass proves leaky in practice, the escalation is a hand-written emitter over
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
    /// NOT async: returns { pid, events } synchronously.
    /// Lifecycle listeners must be attached before the stream is polled.
    fn execute(&self, params: ExecuteParams) -> Result<ExecuteHandle, AdapterError>;
    async fn stop(&self, pid: u32) -> Result<()>;
}
```

`execute` being sync is a deliberate contract choice, not an oversight: the lifecycle and stderr listeners
are attached *eagerly* precisely so a fast-failing child is not missed, and the orchestrator needs the
PID to write `state.running` before it starts consuming events. Spawning is itself non-blocking, so sync costs
nothing.

Stream errors are modelled as `Result` items rather than terminating the stream, because the stream
distinguishes "an `error` event occurred" (continue) from "the generator threw" (run failed). The thrown case
is the stream yielding `Err` and then ending.

```rust
pub struct AdapterRegistry { adapters: Vec<Arc<dyn Adapter>> }  // Vec, not HashMap — order is contract
impl AdapterRegistry {
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn Adapter>>;
    pub fn require(&self, kind: &str) -> Result<&Arc<dyn Adapter>, DomainError>;
    pub fn kinds(&self) -> Vec<&'static str>;   // registration order is part of the contract
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

Implementation requirements:

- `pre_exec(|| { setsid(); Ok(()) })` to create a new process group — the child leads its own session
  and group. This is what makes `kill(-pid)` reap the whole tree.
- `kill` tries the **group first** (`nix::sys::signal::kill(Pid::from_raw(-(pid as i32)), sig)`), falling back
  to the direct PID, swallowing errors.
- `kill_with_grace`: return if dead; `SIGTERM`; poll `is_alive` every **200 ms** to the deadline; `SIGKILL`.
- `is_alive` = `kill(pid, None)` is `Ok`, **or** `Err(EPERM)`.
- `exit` must be subscribable before the output streams are read, and resolvable more than once by the
  consumer — use a `tokio::sync::watch` or a shared `OnceCell`, not a plain oneshot.

### 5.6 Injected seams for determinism — a deliberate addition

Time and ID generation are injected, so tests can assert exact values instead of prefixes and timing-dependent
cases do not flake.

```rust
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    /// ISO-8601 UTC, exactly 3 fractional digits, 'Z'
    fn now_iso(&self) -> String;
}
pub trait IdGenerator: Send + Sync {
    fn generate(&self, prefix: &str) -> String;   // "<prefix>_<nanoid(7)>"
}
```

Real implementations in `orch-infra`; `FixedClock`/`SeqIdGenerator` in a `test-support` module. Both are
threaded through the container and reachable by every service. This is what lets tests assert on exact values,
lets retry/backoff and stall-detection tests run instantly instead of sleeping, and removes a whole class of
flake.

### 5.7 Error model

`DomainError` carries a message, an exit code and an optional hint.

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
    pub fn exit_code(&self) -> i32 { /* per-variant exit code */ }
    pub fn hint(&self) -> Option<String> { /* per-variant hint, exact strings */ }
}
```

The `Display` strings **are** the user-facing messages — writing them in `#[error(...)]` makes them hard to
drift. `exit_code()` and `hint()` are the only two places the
CLI needs. `orch-cli`'s top-level handler is the single exit point: downcast to `DomainError` → print
`  ✕ <msg>` (+ hint) to stderr → exit with `exit_code()`; anything else → exit 1, and dump the chain when
`ORCHESTRY_DEBUG` is set.

Infrastructure errors (`std::io::Error`, YAML, JSON) get their own enums per module and convert **into**
`DomainError::Other` or a specific variant at the service boundary. Library crates never return `anyhow`.

---

## 6. Concurrency model

### 6.1 State mutex

The orchestrator serializes state mutation with a `tokio::sync::Mutex`, and the index manager has its own:

```rust
pub struct Orchestrator {
    state: Arc<Mutex<OrchestratorState>>,
    active_collectors: Arc<Mutex<HashSet<String>>>,
    finishing_collectors: Arc<Mutex<HashSet<String>>>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /* deps: Arc<dyn …> */
}
```

`tokio::sync::Mutex` (not `std::sync::Mutex`) because the critical sections `await` on store I/O . Two properties must be preserved:

1. **The lock is held across awaits**, including file writes. This is intentional: it is what makes
   tick/stop/completion mutually exclusive. Do not "optimize" it into fine-grained locking — the bug
   history (the `active_collectors` guard, the early removal of `state.running[task_id]`, the
   re-read-before-save patterns) is all about this serialization.
2. **`std::sync::Mutex` would be correct for the index manager**'s *in-memory* guard only if no await happens inside;
   it does (file I/O), so use `tokio::sync::Mutex` there too.

`Orchestrator` is `Clone`-able over `Arc`s so background tasks can hold it. One `Arc<Mutex<_>>` per concern
rather than one giant lock, with a documented acquisition order (`state` → `active_collectors` →
`finishing_collectors` → `cancels`) to make deadlock impossible by construction.

Reconcile and a run's collector meet at two guards. While a collector is registered in `active_collectors`,
reconcile skips the PID check: the child may have exited cleanly with the success handler queued for the state
lock, and the collector reports the outcome. The stall check still runs, because an open stream with no events
is what it exists to catch. Once the stream ends, the collector adds the task to `finishing_collectors`, and
reconcile skips the stall check too until the collector deregisters (removing `finishing` before `active`).
Dispatch waits for that deregistration before starting the task again: collector state and cancel tokens are keyed
by task id, so a second run alongside a draining collector would be completed by the old run's late handler.

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

- **`MissedTickBehavior::Delay`** matters. The tick loop must not catch up on missed ticks; tokio's
  default `Burst` would fire several ticks back-to-back after a slow one, changing dispatch timing.
- The reactive-dispatch debounce (500 ms, max 10 retries while a tick is in flight) becomes a
  `tokio::sync::Notify` plus a debounce task — the retry loop exists to avoid contending with a tick, which
  `select!` arms already partly solve, but the **observable** 500 ms delay must be preserved because tests
  depend on it.
- Signal handling moves from process-global handlers to a `select!` arm, which keeps handler registration out of the global state.

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

- **stdout and stderr must be drained concurrently.** Reading them
  sequentially deadlocks on a full pipe. Two tasks, or two `select!` arms — never sequential.
- The stderr tail is a bounded **4096-byte** ring, attached before the stream is polled, used only to enrich
  an error message.
- The line reader's semantics are **not** what
  `tokio::io::AsyncBufReadExt::lines()` does: it splits on `0x0A` only (keeping `\r`), **skips empty lines**,
  yields a trailing fragment, and caps each line at 16384 characters. Write our own `line_reader`; do not
  reach for the convenience method.

### 6.4 Cancellation

`tokio_util::sync::CancellationToken`, one per running task, stored in
`Orchestrator::cancels`. Cancelling it both stops the collector loop and triggers `kill_with_grace`.
Grace periods by call site: `cancel_task`/`force_stop_agent` 3 s, stall/abort 5 s, `stop()` 10 s.

A `Drop` guard on the orchestrator is **not** sufficient for process cleanup (async work cannot run in
`Drop`); shutdown must be explicit, which is already the shape of `stop()`.

### 6.5 Event bus

The event bus is a broadcast channel:

```rust
pub struct EventBus { tx: tokio::sync::broadcast::Sender<Arc<OrchestratorEvent>> }
impl EventBus {
    pub fn emit(&self, event: OrchestratorEvent);              // non-blocking, lossy-on-lag
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<OrchestratorEvent>>;
}
```

`broadcast` with `Arc` payloads gives cheap fan-out to the TUI, the structured logger and the run store. One
behaviour to be explicit about: `broadcast` **drops** messages for a lagging receiver instead of
blocking. Mitigation: size the channel generously
(4096), and have subscribers that must not miss events (the run-store writer) consume on a dedicated task
that does nothing else. `RecvError::Lagged` is surfaced as a `tracing::warn`, never silently ignored. The TUI
*may* lose high-frequency `agent:output` events under load — which the TUI's 80 ms batching
and 2 KB caps already accept.

---

## 7. Container and wiring

```rust
pub struct Engine {                       // light tier: read-only commands
    pub paths: Paths,
    pub config: OrchestratorConfig,
    pub global_config: GlobalConfig,
    pub tasks: Arc<dyn TaskStore>,  /* … the other 8 stores … */
    pub events: Arc<EventBus>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdGenerator>,
    pub task_service: Arc<TaskService>,  /* … the other services … */
}

pub struct FullEngine {                   // full tier: adds adapters, processes, orchestrator
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

The two-tier split documents which commands may mutate state. It buys little startup time (there are no dynamic imports to avoid), so **do not** add lazy
initialization machinery for it — just construct what each tier needs.

---

## 8. Test strategy

Four layers. Layers 1 and 2 are the acceptance gates; 3 and 4 are how individual packages prove themselves.

### 8.1 Layer 1 — unit and integration tests

Unit tests are inline `#[cfg(test)]` modules; integration tests live in each crate's `tests/`.

| Crate | Where | Notes |
|---|---|---|
| `orch-domain` | inline modules, `tests/` | Pure. |
| `orch-engine` | `tests/` + inline | Uses the in-memory fake stores from `test_support`. |
| `orch-infra` | `tests/` + inline | Mix of pure (fs-utils, index, scope) and process/IO (tempdirs, real `bash`). |
| `orch-cli` | `tests/` | Output formatting, flag parsing, serve logger, disk observer; the real binary on temp projects. |
| `orch-tui` | `tests/` via `TestBackend` | See §8.4. |

**No test requires a real AI CLI.** Every adapter test injects a fake `ProcessManager` whose `spawn()` returns
in-memory pipes and drives it from outside (write bytes, signal exit), asserting exact argv and exact stdin
writes. Integration tests that need an agent that really runs use the `shell` adapter or a fake agent script
(§8.2).

The shared helper infrastructure is a `test-support` module in `orch-core` (behind a feature flag so it does
not ship):

- `make_task()`, `make_agent()`, `make_run()` with fixed defaults — including `make_agent()`'s
  `approval_policy: auto`, which is why so many tests observe `review → done` without an explicit approve, and
  `stall_timeout_ms: 300_000`.
- In-memory `Mutex<HashMap<_>>`-backed fakes for all 9 stores, cloning on read and write to prevent aliasing.
- `FakeProcessManager` (fixed pid 12345), `FakeAdapter` (takes a `Vec<AgentEvent>` to replay),
  `FakeWorkspaceManager`, `FakeTemplateEngine` (returns a constant), `FixedClock`, `SeqIdGenerator`.
- `build_deps(overrides)` assembling complete `OrchestratorDeps` with **real** services, event bus and
  registry — only the stores and I/O edges faked. Default `poll_interval_ms` of 100_000 so the tick loop never
  self-fires and tests drive ticks explicitly.

### 8.2 Layer 2 — binary-level tests

`crates/orch-cli/tests/` run the real `orch` binary (`assert_cmd`) against temporary projects and assert on
stdout, stderr, exit code and the resulting `.orchestry/` tree.

- **Each project is `git init`ed** with one commit, because worktree mode requires a repo.
- **Hermeticity.** Each test gets its own temp root, a cleared or explicit environment, and
  `ORCHESTRY_GLOBAL_CONFIG_PATH` inside it; `.cargo/config.toml` points the variable at `target/test-global/`
  for every cargo command so no test touches the developer's real `~/.orchestry`. Runner variables
  (`CARGO_TARGET_DIR`, `TMPDIR`, wrapper `PATH`s) must not leak in. Do not sandbox `HOME` around `cargo`
  itself: `RUSTUP_HOME` and `CARGO_HOME` default to paths under `HOME`, so a sandbox `HOME` makes rustup
  re-download the toolchain.
- **No real AI CLI is ever needed.** Agents use the `shell` adapter, or a **fake agent script** — a small
  `bash`/Rust binary on `PATH` under the name `claude`/`codex`/… that replays a recorded JSONL stream from a
  fixture file (`fixtures/fake-agents/`). This is how adapter-level behaviour (token accounting, `done`
  events, file-change extraction) gets exercised end to end. Unit tests inject a trait object; binary-level
  tests shim `PATH`. Keep the two mechanisms distinct.
- **Every YAML artifact the store round-trip test writes is read back** and compared, and its raw bytes are
  checked against the YAML 1.1 date grammar, so a bug of the form "a YAML 1.1 reader reads r-orch's bytes
  differently" is caught without a second parser (§5.3). The grammar check is only as good as the hand-measured
  table behind `is_yaml11_timestamp`.
- **Every `--once` test needs a bound.** A `serve --once` step can hang forever on the `review` trap: `review`
  is not terminal, so the poll loop waits for ever and there is no built-in timeout
  ([docs/RUNBOOK.md](RUNBOOK.md#serve---once-hangs-on-a-task-in-review)). A test that runs `--once` must drive
  every task it creates to a terminal status (`approval_policy: auto`, `review_criteria`, or an explicit
  `task approve` step) and run under a wall-clock timeout that reports a hang as a failure, not as a slow test.

### 8.3 Layer 3 — fixtures

| Fixture set | Contents | Asserts |
|---|---|---|
| `fixtures/prompts/*.txt` | Golden rendered prompts over the full branch matrix of the two default templates (with/without role, autonomous, goal_id, attempt>1, retry output empty/non-empty, feedback, shared_context, messages 0/1/n, goal description/tasks/progress) | Template rendering, **byte-exact**. The gate for §4.3. |
| `fixtures/adapters/<adapter>/*.jsonl` + `.expected.json` | Recorded or hand-written adapter output | Each adapter's stdout → `AgentEvent` sequence mapping, token extraction, error classification. |
| `fixtures/yaml/*.yml` | Representative and hazardous YAML documents | Round-trip stability of the on-disk format (§5.3). |
| `fixtures/shop/*.json` | Golden agent-shop and org-shop catalogue data | The catalogue data, byte-exact (`crates/orch-domain/tests/shop_catalogue.rs`). |

Golden fixtures change only in a reviewed commit that says why; a test failing against one is a regression until
shown otherwise.

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

**Push the key, draw, assert — synchronously.** No sleeps. This keeps TUI tests deterministic, and
function keys (`KeyCode::F(1)`) are ordinary inputs.

What to test: the key-dispatch chain order; view switching and its blocked-while-detail-open rule; wizard step
sequencing, skip predicates and validation messages; command-bar completion vectors; filter pickers. What
**not** to test: exact glyphs, colours, or pixel layout.

### 8.5 CI shape

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                       # layers 1 to 4
cargo deny check                             # supply chain — advisories, licences, bans
cargo xtask crate-graph                      # asserts the whole inter-crate edge set (§3.1)
```

`deny.toml` carries **two accepted advisories**, both *unmaintained* rather than vulnerable, and both
transitive with no alternative:

| Advisory | Crate | Via | Why accepted |
|---|---|---|---|
| RUSTSEC-2024-0449 | `unsafe-libyaml` | `serde_yaml_ng` | No API-compatible replacement (`serde-saphyr` has no `Value` model), and the amplification class is closed by the exact expanded-node pre-pass at the `read_yaml_value` chokepoint, measured refusing the worst known shape in 18 ms. Revisit if `serde-saphyr` gains a `Value` API (§4.2) |
| RUSTSEC-2026-0319 | `anymap2` | `liquid-core` | No alternative; `liquid` is the only maintained Liquid implementation for Rust (§4.3) and prompt rendering is byte-contract |

Each accepted advisory must name its mitigation and its revisit condition. An advisory accepted with neither
is a silent waiver, which is the failure mode an allowlist invites.

**Licences:** `MPL-2.0` is allowed for **`option-ext`** (transitive via `dirs`). MPL's copyleft is
**per-file** and the crate is used **unmodified**, so it imposes no obligation on our sources. Every
workspace member is `publish = false`, so none of this is a distribution question for the crates themselves.

---

## 9. Package ownership

The workspace was built as seven packages, WP-0 to WP-6. The names survive in code comments, ADRs and
`docs/TRAITS-FROZEN.md`; each owns a fixed set of directories.

### WP-0 — Foundation

**Owns:** `crates/orch-domain/**`, `crates/orch-core/**` (including `crates/orch-core/src/test_support/**`),
workspace `Cargo.toml`, `rust-toolchain.toml`.

**Holds:**
1. Every domain type with final serde attributes — declaration-order fields, `skip_serializing_if` on every
   optional, `Message::to_agent_id` emitting `null`.
2. `transitions`, `scope` (including `ScopeIndex`), `model_tiers`, and the `shop`/`default_agents` catalogue
   data.
3. `DomainError` with `exit_code()` and `hint()`.
4. **All `orch-core` traits, frozen** — §5 is the brief and `docs/TRAITS-FROZEN.md` the index.
5. `test_support`: entity factories, in-memory store fakes, `FakeProcessManager`, `FakeAdapter`,
   `FakeWorkspaceManager`, `FakeTemplateEngine`, `FixedClock`, `SeqIdGenerator`.

**Standing rule:** any package that finds itself **working around** a trait **files an amendment instead**.
Against a frozen surface the cheap local move is always to work around it — and the result is two caches with
different lifetimes over the same store, or duplicated invalidation logic, surfacing as an intermittent
invalid-transition error that no unit test reproduces (because `test_support`'s fakes clone on every read).
Working around a trait is a **defect**, not a shortcut.

### WP-1 — Storage and serialization

**Owns:** `crates/orch-infra/src/storage/**`.

**Holds:** `Paths` + `sanitize_id` + `find_project_root`; `fs_utils` (`atomic_write`, `write_json`, `write_yaml`
**with the timestamp guard**, `append_jsonl` with the PIPE_BUF rule, `read_jsonl`, `read_jsonl_tail`, the cached
`O_APPEND` handle map with its 10 s idle eviction); the index manager; all 9 store implementations; the
tick-scoped cached wrappers; `lock.rs`.

**Why riskiest:** it is the one package where being *subtly* wrong is invisible in-tree and only shows up as
corruption in a user's project. The timestamp-quoting hazard (§5.3) alone can silently break any other YAML
reader's message sorting.

### WP-2 — Adapters and process management

**Owns:** `crates/orch-infra/src/{adapters,process,models}/**`.

**Holds:** `ProcessManager` (setsid process groups, group-first kill, graceful kill, `is_alive`); the
`line_reader`; the bounded stderr tail; the event buffer with backpressure; all 8 adapters with exact argv and
parsing; `AdapterRegistry` preserving order; model discovery with its parsers and fallback lists.

**Stderr draining is structural, not per-adapter.** A child writing past one pipe buffer blocks forever if
nothing drains stderr. Draining lives in the **shared spawn path** so an adapter *cannot* forget it. Two
distinct behaviours, and the difference is observable:

- claude / codex / cursor / opencode / pi → drain **and keep** the bounded 4096-byte tail, which is appended
  to the error message and then classified.
- grok / antigravity → drain and **discard**. Do *not* attach the tail capture: appending it would change the
  error message text, the `classifyAdapterError` result, and the persisted `agent.last_error`.

### WP-3 — Templates, skills, workspace

**Owns:** `crates/orch-infra/src/{template,skills,workspace,clipboard}/**`, `fixtures/prompts/**`.

**Holds:** the Liquid-backed template engine with the §4.3 mitigations (always-present globals with explicit
nils; `shared_context` as an ordered pair array); both default templates; `build_prompt_context` and
`filter_relevant_context`; the skill loader; the workspace manager (worktree, isolated, cleanup,
`get_changed_files`) and the merge strategy; the clipboard service.

### WP-4 — Engine: services and orchestrator

**Owns:** `crates/orch-engine/**`.

**Holds:** `EventBus`; `TaskService`, `AgentService`, `RunService`, `GoalService`, `TeamService`,
`MessageService`, `AgentFactory`, `ReviewRunner`, `DoctorService`; the `Orchestrator` (tick loop, reconcile,
dispatch, collect, completion/failure handling, cascade failure, autonomous seeding, startup recovery,
shutdown); `build_light` / `build_full`. It is the largest package by behaviour density, with a long bug-fix
history; its regression tests are institutional memory, so add the test before changing the code it covers.

### WP-5 — CLI and serve mode

**Owns:** `crates/orch-cli/**`.

**Holds:** the clap command tree; `output` (glyphs, ASCII fallback, `print_table` with its exact padding,
`print_key_value`, the formatters); every command module with human, `--json` and `--yaml` rendering; the error
boundary and exit codes; `init`; `doctor`; `serve` with the structured logger, `--once` runner and disk
observer; `update`/update-check; `editor` and `shop_picker`. It implements `TuiBackend` over the container
(§3.1).

### WP-6 — TUI

**Owns:** `crates/orch-tui/**`.

**Holds:** the four views with their lists and detail panels; the ordered key-dispatch chain; the command bar
with completion, suggestions and history; all wizards with their skip predicates and validation messages; help
overlay; both filter pickers; onboarding; toasts/bell/tab-flash; soft delete with undo; the fourteen palettes; the
80 ms batching, 2 KB detail caps and 500-entry LRU. Its tests use the in-crate `FakeBackend`.

The TUI is the **default command** (bare `orch` launches it), so it is most users' entire experience, and the
**wizard → entity field mapping is on-disk contract**: a wizard with an inverted skip predicate writes corrupt
data. The gate is therefore the counted `TestBackend` case inventory (`crates/orch-tui/TEST-COVERAGE.md`), not a
checklist: a shipped inverted `skip` predicate or a no-op filter picker would leave every other test green.

---

## 10. Trade-offs

**Optimizes for:** stable on-disk and CLI contracts, and determinism in testing. Every significant decision —
the trait crate, the injected clock/ID seams, the golden fixtures, the post-emit YAML quoting pass — buys one of
those two.

**Gives up:**

- **Some idiomatic purity.** `async-trait` and `Arc<dyn _>` everywhere instead of generics and static
  dispatch. The cost is a vtable call on I/O paths, which is free relative to the syscall; the benefit is a
  container that can be assembled at runtime and faked wholesale.
- **Coarse locking.** Holding a `tokio::sync::Mutex` across file I/O is not what you would design from
  scratch. It is long-standing behaviour, and its bug history is specifically about that serialization. At
  this scale (one process, tens of agents) there is no throughput argument for changing it, and a real one for
  not changing it.
- **Self-update, deliberately.** r-orch does not update itself at all; `orch update` reports the version and how
  to upgrade. A CLI overwriting its own executable as a side effect of `orch task list` is a supply-chain
  surface, not a feature. Users update r-orch the way they installed it.
- **Concurrent operation is documented, not guaranteed.** §2.1 — the index read-modify-write can lose updates
  across processes.
- **Shelling out to `git`** rather than linking libgit2 — slower per call, but it keeps exact command
  behaviour and exit codes, and it drops a C dependency.

---

## 11. Risks

| # | Risk | Severity | Mitigation / trigger |
|---|---|---|---|
| R1 | **YAML divergence** — timestamp quoting, merge keys, or a schema construct — making a file written by r-orch read differently by another YAML reader | **High** | Mitigated by the quoting guard, the hand-measured YAML 1.1 date table with its `fs_utils` unit tests, and the byte-level check in `store_round_trip.rs` (§5.3, §8.2); there is no independent YAML 1.1 reader in the test suite. Merge keys are resolved with `apply_merge()` on every read. Escalation trigger for a hand-written emitter: any artifact where the byte-level check finds a timestamp-shaped scalar that quoting cannot fix. |
| R2 | **`liquid` cannot reproduce prompt bytes** despite the mitigations | Medium-high | The golden prompts in `fixtures/prompts/` are the gate. Whitespace and truthiness already match, so the residual risk is a construct not enumerated. Fallback: a ~400-line renderer for 7 constructs. |
| R3 | **Orchestrator behaviour regression** — a race that was already fixed and is reintroduced | **High** | The orchestrator regression tests (`bugfixes`, `resilience`, `retry-race`, `parallel-race`, `endurance`) are the encoded institutional memory; add the test before changing the code it covers. |
| R4 | **`broadcast` event loss** under high adapter output | Medium | Dedicated consumer task for the run-store writer; 4096 capacity; `Lagged` logged as a warning, never swallowed. If a *correctness* consumer ever needs lossless delivery, give it an `mpsc` instead. |
| R5 | **The trait surface is wrong**, forcing a cross-package rework | Medium | §5 is deliberately detailed so the freeze is informed. The trait crate is small, and `cargo` points at every break; changes go through an amendment (`docs/TRAITS-FROZEN.md`). |
| R6 | **Live/streaming behaviour is under-tested** — `tui`, watch-mode `serve`, `run --watch` and `logs --follow` cannot be fully expressed as one-shot binary tests | Medium-high | `serve --once` tests cover stall past timeout, killed mid-run and concurrency saturation; PTY-based tests cover the watch-mode shutdown paths. |
| R7 | **Adapter argv drift** — an external CLI changes its flags | Low | Snapshot tests pin our argv; real-CLI breakage is found by `orch doctor` and by users, not by the suite. |
| R8 | **`kstring` MSRV and similar toolchain friction** recurs as dependencies move | Low | `rust-toolchain.toml` pins the toolchain; `Cargo.lock` is committed; the `kstring = "2.0.2"` pin is commented with its reason. |
| R9 | **Process-group kill misbehaves** on some platform | Low | `process-endurance` tests spawn real `bash`/`sleep` and assert the tree dies; run them everywhere. |
| R10 | **An adapter that does not drain stderr deadlocks** on a chatty child once the pipe fills (~64 KB) | Medium | Structural mitigation: stderr draining is part of the **shared** spawn path (WP-2), so an adapter cannot forget it. Each adapter has a test that writes 256 KB to stderr and asserts completion. |

### Rollback

A release is rolled back by reinstalling the previous version (README, "Upgrading from v0.4.0 and rolling
back"). The meaningful failure mode is shipping something that corrupts a project's `.orchestry/` data, which
the YAML round-trip and format tests exist to prevent. What remains is documentation, not a flag: the
concurrent-index hazard (§2.1) and the recovery for it are in [docs/RUNBOOK.md](RUNBOOK.md), which is where an
operator will look.

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

**Write our own Liquid subset renderer immediately.** Removes R2 entirely and is genuinely small — the default
templates use seven constructs and zero custom filters. **Rejected as the default:** measurement showed `liquid`
already handles whitespace and truthiness, which were the hard parts, and a maintained crate handles the
constructs we have not thought of in user-supplied templates. Kept as the WP-3 contingency with a defined
trigger.

**`git2`/libgit2 instead of shelling out.** Faster, no subprocess, typed errors. **Rejected:** the workspace
manager depends on exact `git` command lines and their exit codes, including `git worktree prune` and
`git merge --abort` semantics that libgit2 models differently. Shelling out is one fewer C dependency.

**`chrono` over `jiff`.** More ubiquitous, more examples. **Rejected narrowly:** we need exactly one format
(RFC 3339, millisecond precision, `Z`) and `jiff` expresses that without timezone-database coupling. This is
the decision I hold most loosely — `chrono` is a one-file change if `jiff` creates friction.

### TUI focus, snapshots and copy ownership

The dashboard keeps explicit pane focus separate from entity selection. Rendered hit maps publish actual
content rectangles, display-row navigation metrics and generations; key and mouse routing consume that frame.
Viewer and editor hooks each run once before legacy dispatch. Click-caused editor transitions invalidate stale
hits and suppress that exact footer before either hook or legacy actions; keyboard input remains independent.
All user quit routes converge on guarded exit, while signals restore the terminal without prompting.

Canonical message storage retains complete bodies under admission/count/text-allocation budgets. Live Raw
bodies use shared immutable Arcs so a pinned viewer reuses source ownership and survives record eviction.
Pending Strings charge capacity, live Arc text charges payload bytes, and IDs/previews charge String capacity.
Control blocks, incoming backend queues, domain objects, allocator arenas and temporary rendering allocations
are outside this component budget. One immutable viewer caches Human/Raw representations and only its active
row index; visible rows escape controls without altering copy bytes. These limits do not bound process RSS.

`TuiBackend` owns copy request/result contracts; orch-cli owns route selection, serialized OSC output and browser
glue; orch-infra owns the native worker. Only the viewer Tick hook drains shared results. Feedback is matched by
full origin and generation across viewer, editor and direct-field surfaces. Draft state belongs to the wizard
session and remains memory only. No frozen orch-core port, storage format or peer protocol changes.
See [ADR 0003](decisions/0003-tui-focus-viewer-and-clipboard-boundaries.md), the
[backend contract](../crates/orch-tui/BACKEND.md) and [operator guidance](RUNBOOK.md#copying-text-from-the-dashboard).
