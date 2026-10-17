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

### Installing a release

Releases are tags on `master` (`vX.Y.Z`). v0.5.0 is the first release with
[remote agents](#remote-agents); they were previewed in the pre-release tags `v0.5.0-alpha.1` and
`v0.5.0-alpha.2`. v0.5.1 makes remote task state truthful and reconnection reliable. The Homebrew formula pins the latest release tag (and `--HEAD` follows `master`).
`orch update` does not self-update: it only prints how to install. To build a specific release from source:

```bash
git clone <this repo> r-orch && cd r-orch
git checkout v0.5.1
cargo install --locked --path crates/orch-cli
orch --version      # 0.5.1
```

Keep the clone: the library skills are found in `skills/library` of the checkout the binary was built from.
Before upgrading a project that runs v0.4.0, read
[Upgrading from v0.4.0 and rolling back](#upgrading-from-v040-and-rolling-back).

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

**Messaging an agent from the dashboard.** On the Agents tab, select an agent and press **`m`**: the
command bar opens prefilled `/msg "Code Reviewer" `, ready for the body. Or type `/msg ` and pick from
the agent list — Tab or a double-click fills the name, already quoted. The message is queued exactly as
`orch msg send` queues it; the recipient reads it on its **next run**, so start `/watch` if nothing is
dispatching.

```
/msg reviewer the signup test is flaky on CI      # bare name
/msg "Code Reviewer" please look at #12           # a name with spaces
/msg agt_7fK2mQ1 ping                             # by id, always unambiguous
/msg all standup in five                          # broadcast — asks for a second Enter
```

Names are quoted the way a shell quotes them, because agent names may contain anything but whitespace
alone:

| Name | Write it as |
|---|---|
| `reviewer`, `名前` | `reviewer`, `名前` |
| `Code Reviewer`, `🚀 deploy` | `"Code Reviewer"`, `"🚀 deploy"` |
| `say "hi"` | `"say \"hi\""` |
| `C:\path` | `"C:\\path"` |
| `don't` | `"don't"` |
| `all` (an agent actually named that) | `"all"`, or its id |

The body is everything after the name, verbatim — its own quotes, `#` and `/` are not interpreted, so
`/msg reviewer /quit now` sends that text and does not quit.

Autocomplete offers prefix matches, but **sending never guesses**: a prefix, an ambiguous name or an
unquoted name that could be read two ways (`Code` when `Code Reviewer` also exists, including
`/msg Code Rev hi`) is refused with the exact form to use. `all` broadcasts only as the bare lowercase
word and only after a confirming second Enter, pressed straight after the first — any edit, any other
key and leaving the bar all make it ask again. `/msg ALL …` is **not** a broadcast: `ALL`, `All` and
`@all` are read as names, so they reach an agent actually called that and are otherwise refused with
`No agent matches`. The chip in the command bar names the recipient while you type.

A queued message **expires after 24 hours** if the recipient never runs; nothing reports that it did,
so for anything time-sensitive check that something is dispatching (`/watch`, or `● WATCHING` in the
header). Long bodies: paste is bracketed, so newlines are kept and shown as `⏎` rather than submitting
the message early — but one paste is capped at **64 KiB** (with a line in the feed when it is cut), and
the engine cuts the body itself at **4000 characters**, which the TUI reports as an advisory. In a
terminal or multiplexer *without* bracketed paste (Windows legacy console, tmux with the mode off) a
pasted body still arrives as raw keystrokes: the first newline submits and the rest is typed into the
dashboard. Paste one line, or paste into `orch msg send` instead.

Two things that will bite you on first use, both covered in the [runbook](docs/RUNBOOK.md):

- **`approval_policy: auto`** above is deliberate. The default `agt_creator` agent uses `suggest`, which
  leaves finished tasks in `review` — and `serve --once` waits for `review` forever.
- **Keep API keys in your environment, not in an agent's `env:` map.** Agent files are world-readable and
  are *not* gitignored by `init`.

---

## Remote agents

Peers use plain TCP on a trusted LAN, **without authentication or encryption**. Listening is off by
default. Only the process that owns the project engine opens peer connections; a TUI observing an
existing owner cannot enable listening.

> [!WARNING]
> **A listening worker runs whatever a connecting peer sends it.** Any host that can reach the port can
> list the worker's agents and submit tasks to them, including `shell` agents that run arbitrary
> commands. There is no password, token or allowlist. Only expose the listener on a network where you
> trust every host: never on a public interface, shared Wi-Fi or a VPN with strangers on it, and
> preferably behind a firewall that admits only your controller's IP.

A bare `--listen` and `orch remote listen on` bind **`127.0.0.1:7690`**, which only accepts
connections from the same machine. To accept another machine you must give an address explicitly,
either a specific LAN IP of the worker (preferred) or `0.0.0.0:7690` (all interfaces). For any
address other than loopback, `orch remote listen on`, `orch serve` and `orch run --watch` print a
warning on stderr, and `orch remote status` and the TUI header show the address as
`(EXPOSED, no auth)`.

On the worker, start an engine with a unique display name and an explicit listen address:

```bash
orch serve --listen 192.168.1.20:7690 --peer-name worker-linux   # the worker's own LAN IP
# Or every interface: orch serve --listen 0.0.0.0:7690 --peer-name worker-linux
# Also supported: orch run --watch --listen ... or orch tui --listen ...
```

To persist the preference for future engine starts, use `orch remote listen on 192.168.1.20:7690`
(without an address it saves `127.0.0.1:7690`); `orch remote listen off` disables it on the next
start. A `--listen` flag applies to that invocation.
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
Remote runs do not use the controller's local slot cap (`scheduling.max_concurrent_agents`): the
worker enforces its own, and each remote agent runs one task at a time.

A remote counts as online only while its link is live **and** the controller can record that state in
`.orchestry/remotes/`. If that directory is unwritable or the disk is full, or the socket drops, the
remote reports `offline` in `orch remote status`, the TUI and `orch agent list`, even if a cached
snapshot file still says online, and `orch task show` reports `remote w1 offline` for tasks waiting on
it. Nothing new is dispatched to it until the status can be saved again. The engine prints a
non-fatal `Write peer status for w1 … Check permissions and free space` (or `Peer w1 disconnected`)
error without `RUST_LOG`. Fix the permissions or free space; once the link reconnects and saves its status, the remote is online
again and waiting tasks dispatch. Local agents keep running throughout, including after an engine restart.

**Remote agents are leaf executors using worker-local resources.** There is no repository, input,
file, or artifact synchronization. Paths refer to the worker, which runs its own agent command, role,
skills, workspace integration and review criteria. Prepare required resources there before assigning
work. Hand results back in summaries or direct messages. Remote agents must not orchestrate the
controller's graph with `orch task add`, `orch goal`, or `orch context`: those commands operate on the
worker's own project. Task proofs record the remote alias, worker branch and changed files. Worker
review failures and merge conflicts put the controller task in `review` and keep dependents blocked.

### Peer names and mutual links

Every instance announces a peer name (`--peer-name`, `peer.name` in `.orchestry/config.yml`, default: the
short hostname). A worker rejects an incoming controller whose peer name equals one of **its own outbound
aliases** (`orch remote list`). This is deliberate: it keeps `<id>@<alias>` routing unambiguous.

The natural mutual setup trips it. Two hosts, `mini` and `studio`, each listen and each register the other
under its hostname:

```bash
# on studio:  orch remote add mini <mini-ip>        # outbound alias "mini"
# on mini:    orch remote add studio <studio-ip>    # outbound alias "studio"
```

mini connects to studio announcing itself as `mini`, which is studio's alias for it, so studio refuses, and
studio's link to mini is refused the same way. Both sides keep retrying and report, verbatim:

```
$ orch remote status      # on mini
  Listen: 127.0.0.1:17690
  Controller: none
  studio: offline (Inbound controller name collides with a configured outbound alias)
```

Fix: give each side a `--peer-name` that is not an alias on the other side, or use different aliases:

```bash
orch serve --listen 192.168.1.20:7690 --peer-name mini-ctl      # on mini
orch serve --listen 192.168.1.21:7690 --peer-name studio-ctl    # on studio
```

After that, `orch remote status` on both shows the peer `online` and the other side as `Controller:`.
A one-directional setup (one controller, one worker) never hits this.

### Environment of remote runs

A run started on behalf of a remote controller does **not** inherit credential-shaped variables from the
worker's `orch` process. Local runs are unaffected. A name is dropped when it matches, case-insensitively,
one of these:

- an underscore-separated segment equal to `KEY`, `KEYS`, `APIKEY`, `TOKEN`, `TOKENS`, `SECRET`, `SECRETS`,
  `PASSWORD`, `PASSWD`, `PASS`, `PAT`, `AUTH`, `OAUTH`, `CREDENTIAL`, `CREDENTIALS` or `COOKIE`;
- the substring `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `APIKEY`, `CREDENTIAL` or `PRIVATEKEY` anywhere.

So `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GITHUB_TOKEN`, `AWS_ACCESS_KEY_ID` and `SSH_AUTH_SOCK` are
stripped, and so is the harmless `TOKENIZERS_PARALLELISM` (it contains `TOKEN`). `GIT_AUTHOR_NAME` and
`PLAIN_VAR` are kept. Matching is by name only: a secret under an innocent name, such as `DATABASE_URL`, is
not caught.

**Consequence:** if a worker's `claude` or `codex` authenticates through an environment variable, or git
needs `SSH_AUTH_SOCK`, remote-submitted tasks fail on authentication while the same agent works locally.
Re-admit variables explicitly, by exact name (no wildcards):

```bash
# On the worker, for the orch process that owns the engine (serve, run --watch or tui):
ORCH_REMOTE_ENV_PASSTHROUGH=ANTHROPIC_API_KEY,SSH_AUTH_SOCK orch serve --listen 192.168.1.20:7690
```

The list is separated by commas or whitespace and is read each time a run starts. Variables set in the
agent's own `config.env` are always passed, filtered or not. Checked with a `shell` agent: with
`FAKE_API_KEY`, `SSH_AUTH_SOCK`, `TOKENIZERS_PARALLELISM`, `PLAIN_VAR` and `GIT_AUTHOR_NAME` set on the
worker, a remote run saw only `PLAIN_VAR` and `GIT_AUTHOR_NAME`; with
`ORCH_REMOTE_ENV_PASSTHROUGH=FAKE_API_KEY,TOKENIZERS_PARALLELISM` it also saw those two, and still not
`SSH_AUTH_SOCK`.

### Approval on the worker

Remote submits honour the **worker** agent's approval policy. Only an agent whose policy is `auto` runs a
remote task straight away; `suggest`, `manual` and an agent with no policy stored hold it. On a fresh
project `orch agent add` stores `auto` for `shell` agents and the project default (`defaults.agent.approval_policy`,
`auto` after `orch init`) for the others, but the `Agent Creator` agent from `orch init` is `suggest`. Check
with `orch agent show <id>` (it prints `auto` for an agent with no policy, so a hand-edited agent file
without one still holds remote tasks). A held task sits in `todo` on the worker with the `awaiting-approval` label, while the
controller shows it `in_progress` (the worker reports it as queued). Approve it **on the worker**, using the
worker's own task id (the controller's id differs):

```bash
orch task list            # on the worker: find the mirror task
orch task approve tsk_05O0U7_
# ✓ Approved tsk_05O0U7_ → released to run
```

Until then nothing runs there, so a remote task can wait indefinitely for someone at the worker.

### Upgrading from v0.4.0 and rolling back

Upgrade
- Run **one version on every peer**; mixed builds have not been tested.
- Upgrade the engine owner (`orch serve`, `orch run --watch` or the TUI) and the CLI on the same host
  together: processes of one project coordinate through lock files that only this build takes.
- Back up first: `cp -R .orchestry .orchestry.bak` in each project, then stop the old engine, install v0.5.0
  (`brew upgrade r-orch`, or from source as in [Installing a release](#installing-a-release)) and start it.
- Changes you may notice since v0.4.0:
  - the listener binds `127.0.0.1:7690` by default; give `0.0.0.0:7690` or a LAN IP explicitly to accept
    other hosts. There is still no authentication;
  - the entity indexes under `.orchestry` (tasks, agents, messages, context) take a cross-process writer lock
    (`.index.lock` in each folder), which fixes lost updates between concurrent CLI and engine writes. A writer that cannot get it within 5 seconds
    fails with `Timed out after 5000 ms waiting for index writer lock …; retry the operation`;
  - a remote counts as online only while the controller's engine holds a lock on
    `.orchestry/remotes/<alias>.health`: after `kill -9` of the engine, `orch remote status` showed the remote
    `offline` at once;
  - remote tasks wait for worker approval (above) and run without credential-shaped variables (above);
  - `.orchestry/peer-id` and `.orchestry/remotes/*.json` are written readable by the owner only;
  - `claude` agents get the system prompt through a private file (`--system-prompt-file`, in a 0700
    per-user directory: `$XDG_RUNTIME_DIR/orch`, else `<temp dir>/orch-<uid>`) and the task prompt on
    stdin, instead of as command-line arguments. This fixes `Failed to spawn process: claude` on Linux
    when the prompt exceeded the 128 KiB single-argument limit. A blank system prompt now counts as none.

Roll back
1. Stop every v0.5.0 engine on the project and make sure **no remote runs are in flight**: the old binary
   cannot resume remote execution entries and will cancel them.
2. Go back to the v0.4.0 binary: reinstall it from its tag (the steps in
   [Installing a release](#installing-a-release) with `git checkout v0.4.0`), or switch back to a v0.4.0
   install you kept. If anything looks wrong, stop the engine and restore `.orchestry` from
   `.orchestry.bak`.

v0.4.0 has no remote support, so the `remote` commands and the `Controller` and `Listen` status lines
disappear when you roll back.

### Upgrading from v0.5.0 and rolling back

Upgrade every peer to v0.5.1 together: the fixes below are mostly on the worker, so a v0.5.1 controller
talking to a v0.5.0 worker still sees the v0.5.0 failures. Changes you may notice:
- a task file containing Markdown emphasis no longer fails with `contains N YAML aliases, over the limit
  of 64` (the alias guard now counts real aliases from the parser, not `*` characters);
- a remote task shows where it really is: `dispatched → <alias>`, `queued @<alias>` (with
  `awaiting approval: orch task approve <id> on <alias>` when the worker holds it), `RUN @<alias>` only once
  the worker runs it, and `RUN? (<alias> offline)` while the link is down;
- one unreadable task file on a worker no longer drops every connection; `orch remote status` on the
  worker shows why the last session ended, and on the controller the retry countdown and attempt;
- a controller restart, graceful or not, keeps remote runs and reattaches to them instead of cancelling;
- a submit the worker never acknowledges fails after 5 minutes online; a task queued for an hour is
  reported hourly but never failed;
- a worker cancels leftover task copies of a controller it has not seen for 24 hours (todo copies only).

Rolling back to v0.5.0 cancels remote runs in flight (stop with none in flight first), and v0.5.0 never
cleans up leftover task copies on a worker. See [docs/RUNBOOK.md](docs/RUNBOOK.md) for each message.

### Known gaps in v0.5.1

- The automated multi-instance tests run every instance on one machine; no automated test covers two
  hosts or a physical network (v0.5.1 was verified by hand between a macOS controller and a Linux worker
  on a LAN).
- Messages and task results between peers are plain TCP: no authentication, no encryption.
- If a stdin write to `claude` fails after the first 32 KiB, claude can run with a truncated prompt. This
  is logged at `warn` but not reported as a run error.
- `cursor`, `opencode`, `agy` and `grok` pass the prompt as a command-line argument, and `pi` passes the
  system prompt as one, so they can still hit the Linux 128 KiB single-argument limit. `claude` and
  `codex` do not.

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
