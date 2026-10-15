# r-orch

An AI agent orchestrator: a CLI, a terminal dashboard, and a runtime that drives external AI coding CLIs
(`claude`, `codex`, `cursor`, `opencode`, `pi`, `grok`, `agy`) as child processes, moves tasks through a state
machine, and keeps everything in plain files under `.orchestry/`.

No database or daemon to install. One engine process per project, guarded by a PID lock file; optional
peer connections let it delegate work to other installations.

This is a **Rust reimplementation of [`oxgeneral/ORCH`](https://github.com/oxgeneral/ORCH)** (npm
`@oxgeneral/orch`), built to operate on the same projects as the original — see
[Relationship to the TypeScript original](#relationship-to-the-typescript-original).

---

## What it does

You define **agents** (an AI CLI plus a model, a role and some skills) and **tasks** (work with a priority,
dependencies, and optionally a file scope). The orchestrator matches tasks to idle agents, prepares a
workspace, renders a prompt, spawns the agent, streams its output to an append-only log, and moves the task
through `todo → in_progress → review → done` with retries and backoff on failure.

Beyond that: **goals** that drive autonomous agents, **teams** with a lead, inter-agent **messaging**, a
shared **context** store, git **worktree** isolation per task with automatic merge-back, and automatic
review criteria (`typecheck`, `lint`, `test_pass`).

---

## Install

From source, which requires a Rust toolchain; the version is pinned in `rust-toolchain.toml`.

```bash
git clone <this repo> && cd r-orch
cargo build --release
```

The binaries land in `target/release/`. Put them on your `PATH`, or:

```bash
cargo install --path crates/orch-cli
```

### The three binary names

`orch`, `orchestry` and `ao` are **the same program** — three names for one binary, matching the original's
npm `bin` map. Use whichever you prefer; `orch` is shortest and is what the docs use.

### Agent CLIs are not bundled

r-orch spawns AI CLIs; it does not ship them. Install whichever you intend to use and check what is visible:

```bash
orch doctor
```

The `shell` adapter needs nothing but `bash` and runs any command you give it — the easiest way to try
r-orch without an AI CLI, and what the test suite uses.

---

## Quick start

```bash
cd your-project
orch init                                   # creates .orchestry/
orch agent add builder --adapter claude --model claude-sonnet-4-6 --approval-policy auto
orch task add "Add input validation to the signup form" --assignee builder
orch run --watch                            # or: orch serve      (headless)
orch                                        # the TUI dashboard
```

Bare `orch` launches the dashboard. In a directory with no `.orchestry/`, it initialises one first.

**Mouse.** Click tabs, list rows, `+ add …` rows, filter chips and the footer hint buttons;
double-click a row to open its detail; the wheel moves the selection. Everything is also a key —
the mouse adds nothing you cannot type.

Capture holds the pointer, which takes click-drag **text selection** away from the terminal. To get
it back: hold **Shift** while dragging (most terminals), or press **F2** to release the pointer for
the session. **Under tmux, Shift+drag is not the answer** — tmux holds the outer terminal's mouse
mode, so it selects across pane borders; use `F2` (or `/mouse off`), which also restores tmux's own
drag-to-copy. `prefix [` copy mode always works. Under tmux you also need `set -g mouse on`, or no
clicks reach the dashboard at all.

To start with the mouse off: `ORCH_TUI_MOUSE=0 orch`. `/mouse status` says which it is.

Two things that will bite you on first use, both covered in the [runbook](docs/RUNBOOK.md):

- **`approval_policy: auto`** above is deliberate. The default `agt_creator` agent uses `suggest`, which
  leaves finished tasks in `review` — and `serve --once` waits for `review` forever.
- **Keep API keys in your environment, not in an agent's `env:` map.** Agent files are world-readable and
  are *not* gitignored by `init`.

---

## Remote agents (alpha)

Peers use plain TCP on a trusted LAN, **without authentication or encryption**. Listening is off by
default. Only the process that owns the project engine opens peer connections; a TUI observing an
existing owner cannot enable listening.

On the worker, start an engine with a unique display name and an explicit listen address:

```bash
orch serve --listen 0.0.0.0:7690 --peer-name worker-linux
# Also supported: orch run --watch --listen 0.0.0.0:7690 --peer-name worker-linux
# Or: orch tui --listen 0.0.0.0:7690 --peer-name worker-linux
```

To persist the preference for future engine starts, use `orch remote listen on 0.0.0.0:7690`;
`orch remote listen off` disables it on the next start. A `--listen` flag applies to that invocation.
Use `--peer-name` or `peer.name` in `.orchestry/config.yml` to distinguish installations.

On the controller, register the worker and run the engine:

```bash
orch remote add w1 worker-host:7690
orch serve --peer-name controller
# In another terminal:
orch remote status
orch agent list
orch task add "Run the worker's checks" --assignee 'agt_x@w1'
orch msg send 'agt_x@w1' "Please include the test results in your summary"
# On the worker, reply to the controller's CLI:
orch msg send 'cli@controller' "Checks passed"
```

Replace `agt_x` with an agent ID from the worker. Remote agents appear as `<id>@<alias>`, with
`linux` or `macos` host capabilities in the CAPS column. Add `--labels cap:linux` to a task to require
Linux; unsupported capabilities leave the task waiting. The TUI header and `orch remote status` show
the effective listen address, connected controller, and each configured remote's online state.

A controller can connect to several workers. Each worker accepts one controller at a time. Registry
entries persist across restarts; links reconnect with backoff until `orch remote remove w1`. Offline
agents remain visible, and new tasks assigned to them wait in `todo`. During a link outage an active
remote task waits for its outcome without a retry: the worker owns execution and stall detection.
Cancelling the task or removing the remote ends that wait. A controller restart cancels its active
remote tasks; reconnecting reconciles and cancels their worker mirrors. Messages queue while offline
and deduplicate when delivered. Broadcast messages stay local.

**Remote agents are leaf executors using worker-local resources.** There is no repository, input,
file, or artifact synchronization. Paths refer to the worker, which runs its own agent command, role,
skills, workspace integration and review criteria. Prepare required resources there before assigning
work. Hand results back in summaries or direct messages. Remote agents must not orchestrate the
controller's graph with `orch task add`, `orch goal`, or `orch context`: those commands operate on the
worker's own project. Task proofs record the remote alias, worker branch and changed files. Worker
review failures and merge conflicts put the controller task in `review` and keep dependents blocked.

This feature is developed on `feature/remote-peers`; no Homebrew formula or stable release is changed.
To try the local checkout explicitly, build it with `cargo build --release -p orch-cli` and invoke
`./target/release/orch`. Before rolling back to v0.3.1, stop the alpha engine with **no remote runs in
flight**. The old binary cannot resume remote execution entries and will cancel them.

---

## Documentation

| Document | For |
|---|---|
| **[docs/RUNBOOK.md](docs/RUNBOOK.md)** | **Operators.** Recovery procedures and the gotchas that are inherited rather than fixed — corrupt locks, hung `--once`, index rebuilds, state recovery, debugging, secrets |
| [docs/PARITY.md](docs/PARITY.md) | The parity contract: every command, flag, exit code, `--json` shape and on-disk field, graded L0/L1/L2, plus every known bug and whether it was replicated or fixed |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | The Rust design: crate boundaries, traits, concurrency model, error model, test strategy |
| [docs/parity/](docs/parity/) | Per-area appendices behind the contract — CLI, adapters, infrastructure, TUI, tests |

Start with the **runbook** if something is wrong, and **PARITY.md** if you are wondering whether a behaviour
is intentional. It usually is, and it usually says why.

---

## Relationship to the TypeScript original

r-orch is a reimplementation of ORCH at pinned commit `c066dc0` (v1.0.34), not a fork or a wrapper. No
TypeScript is executed at runtime.

### Cross-compatibility

The remote-peer extension described above is specific to r-orch. The historical parity documentation
and differential harness describe the original command set; they do not promise parity for remote
agents, peer metadata, or the extended prompts.

**Both binaries can operate on the same project directory, in either order.** That is a design goal, not a
side effect, and it is enforced by a differential test harness that runs the same commands against both
implementations in parallel temporary directories and compares stdout, stderr, exit codes and the entire
resulting `.orchestry/` tree — including a step that re-parses every YAML file **in Node with js-yaml** to
catch bytes that Rust writes and TypeScript would misread.

What that buys you concretely:

- the `.orchestry/` format is identical — field names, types, ID formats, timestamp format, JSON indentation;
- the CLI surface is identical — command names, flags, exit codes and `--json` shapes;
- prompts rendered from the default templates are **byte-identical**, so prompt caching and agent behaviour
  carry over.

What is deliberately *not* identical:

- **internals** — tokio, trait-based adapters, no promise chains;
- **the TUI** is a `ratatui` reimplementation matching views, keybindings and wizards rather than pixels;
- **`--help` output** is clap's layout;
- **no self-update.** The original updates itself via npm after almost every command. r-orch does not
  update itself at all; `orch update` reports your version and how to upgrade.
  This is a deliberate scope cut on security grounds, not an omission.

A handful of original bugs are **replicated on purpose**, because `--json` output and on-disk format are a
contract that something downstream may depend on. Each is listed in [PARITY.md §14](docs/PARITY.md) with the
reasoning, and the operator-visible ones are in the [runbook](docs/RUNBOOK.md).

### If you are migrating from the npm package

Point r-orch at an existing project and it should just work. Before you rely on it: make sure no orchestrator
is running under the old binary, then `orch status` and `orch task list` to confirm r-orch reads your state
as expected. The two binaries must not dispatch simultaneously — the PID lock prevents two *orchestrators*,
but one-shot commands do not take it.

---

## Development

```bash
cargo test --workspace            # unit, integration and TUI tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo xtask crate-graph           # asserts the inter-crate dependency edges
```

The differential harness needs the reference implementation built once:

```bash
./parity/setup.sh                 # clones the pinned TS reference, applies parity/patches/, builds it (needs Node and npm)
cargo xtask parity                # runs every scenario against both binaries
```

`parity/orch-ts/` is gitignored. See [ARCHITECTURE.md §8](docs/ARCHITECTURE.md) for the test strategy and
[§9](docs/ARCHITECTURE.md) for how the work is split across crates.

---

## License

MIT, as the original.
