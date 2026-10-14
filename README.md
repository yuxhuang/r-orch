# r-orch

An AI agent orchestrator: a CLI, a terminal dashboard, and a runtime that drives external AI coding CLIs
(`claude`, `codex`, `cursor`, `opencode`, `pi`, `grok`, `agy`) as child processes, moves tasks through a state
machine, and keeps everything in plain files under `.orchestry/`.

No database, no daemon to install, no server. One process per project, guarded by a PID lock file.

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

Requires a Rust toolchain; the version is pinned in `rust-toolchain.toml`.

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

Two things that will bite you on first use, both covered in the [runbook](docs/RUNBOOK.md):

- **`approval_policy: auto`** above is deliberate. The default `agt_creator` agent uses `suggest`, which
  leaves finished tasks in `review` — and `serve --once` waits for `review` forever.
- **Keep API keys in your environment, not in an agent's `env:` map.** Agent files are world-readable and
  are *not* gitignored by `init`.

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
  update itself at all, and makes no network calls; `orch update` reports your version and how to upgrade.
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
./parity/setup.sh                 # clones the pinned commit, applies parity/patches/, npm ci && npm run build
cargo xtask parity                # runs every scenario against both binaries
```

`parity/orch-ts/` is gitignored. See [ARCHITECTURE.md §8](docs/ARCHITECTURE.md) for the test strategy and
[§9](docs/ARCHITECTURE.md) for how the work is split across crates.

---

## License

MIT, as the original.
