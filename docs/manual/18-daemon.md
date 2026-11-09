# The orch daemon

<!-- Plan task W1 (PR 1). Written from the approved spec and design D8 ahead of the code; the facts board
checks every command, flag, exit code and path against the frozen PR 1 commit. No example below is captured
output: W4 replaces the placeholders with real output from the frozen commit. PR 2's writer (W2) removes the
developmental notice and turns each "Planned (PR 2)" into current behaviour. -->

> **Developmental in this build (PR 1 of the daemon work; the package version is still 0.13.0): requires `ORCH_DAEMON_DEVELOPMENTAL=1`; no client uses the daemon until PR 2.**
> In this build only the four `orch daemon` commands talk to the daemon. Every other command, the dashboard,
> `orch web` and `orch mcp` still read and write `.orchestry/` directly, as in orch 0.13.0, so a running daemon
> does **not** keep them out: the start banner says that legacy writers are not excluded. Try the daemon on a
> test project only. Sections marked **Planned (PR 2)** describe behaviour that this build does not have yet.

The **daemon** is the one orch process that owns a project. It reads and writes everything under `.orchestry/`,
starts and stops the agents, and runs the orchestrator when dispatch is on. Clients ask it to do the work over a
private Unix socket. You always start it yourself: no orch command, client, retry or error path ever starts one.
The normative contract is the [daemon-owned state spec](../specs/2026-11-09-daemon-owned-state.md); this chapter
is the operator's view of it.

## In this chapter

- [Why a daemon](#why-a-daemon)
- [Command reference](#command-reference)
- [Start the daemon](#start-the-daemon)
- [Check, toggle dispatch and stop](#check-toggle-dispatch-and-stop)
- [Exit codes](#exit-codes)
- [When no daemon answers](#when-no-daemon-answers)
- [Sockets, project selection and `daemon.json`](#sockets-project-selection-and-daemonjson)
- [Version skew](#version-skew)
- [Recovery](#recovery)
- [How the existing commands map (planned, PR 2)](#how-the-existing-commands-map-planned-pr-2)

## Why a daemon

Up to orch 0.13.0, every orch process wrote the project's files on its own: the orchestrator, each one-off
command, the dashboard, `orch web` and every `orch mcp` server. Two of them changing the same thing at the same
time could lose an update: a field could go back to an older value, a renewed context key could disappear, or a
task could exist as a file but be missing from `orch task list` (see
[Concurrent writers](17-mcp-server.md#concurrent-writers) in the MCP chapter). Once every client goes through the
daemon (planned, PR 2) there is only one writer, so these races cannot happen between processes. Your files keep
their format: tasks, agents and goals are still plain YAML in `.orchestry/`.

Inside the process that dispatches, this build already makes three guarantees, whether that process is the
daemon or `orch serve`, `orch run --watch` or the dashboard running the orchestrator:

- A task linked to a paused goal does not start. The orchestrator rereads the task and its goal just before it
  starts an agent, and that check is ordered with the pause.
- Cancelling a task in that process stops its running agent before the cancel reports success.
- Reads never delete expired context entries; `orch context get`, `orch context list` and the MCP context tools
  only hide them. The orchestrator removes expired entries on each of its checks.

A pause or a cancel sent from *another* process, such as `orch goal status <id> paused` or `orch mcp` next to
`orch serve`, is not ordered with dispatch until the clients move to the daemon (planned, PR 2): a task can still
start just before such a pause is saved, and is then cancelled; such a cancel marks the task, and the
orchestrator stops its agent at its next check, as in orch 0.13.0.

## Command reference

All four commands refuse to run unless `ORCH_DAEMON_DEVELOPMENTAL=1` is set in their environment (exit 2).

| Command | What it does | Exit codes |
|---|---|---|
| `orch daemon start [--detach] [--dispatch on\|off] [--from-agent]` | Starts the daemon for the project, in the foreground unless `--detach`. Dispatch is **off** unless `--dispatch on`. Peer-link listener options are start options too; `orch daemon start --help` lists them. | 0 after a clean stop (foreground) or after readiness (`--detach`); 2, 3, 4 on refusal |
| `orch daemon status` | Asks the running daemon for its live status. | 0; 7 when no compatible daemon answers |
| `orch daemon stop` | Asks the daemon to shut down cleanly and waits until its process has exited, for at most 30 s. Works across a version mismatch. | 0 on a completed clean stop; 7 when no daemon answers, the outcome is unknown or the wait times out (with a message of its own) |
| `orch daemon dispatch on\|off` | Turns dispatching of future work on or off in the running daemon. | 0; 7 when no compatible daemon answers |

| Flag | Meaning |
|---|---|
| `--detach` | Run in the background in a session of its own. The command returns only after the daemon is ready, and fails if the daemon fails before that. |
| `--dispatch on\|off` | Dispatch state at start. Default `off`, so you can queue work before anything runs. |
| `--from-agent` | Allow the start from an agent's environment (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set). Without it the start refuses with exit 2. Use it only when you, the owner, deliberately start a daemon from such a shell. |

`orch daemon` also needs a project: run it in the project or a subdirectory, or set `ORCH_PROJECT_ROOT` (see
[Project selection](#sockets-project-selection-and-daemonjson)). Without an initialized project it exits 3.

## Start the daemon

```sh
ORCH_DAEMON_DEVELOPMENTAL=1 orch daemon start
```

The daemon first checks the project and your user, takes ownership of the project, recovers the state a previous
orchestrator left (the same reconciliation `orch serve` does at start, and a rebuild of any missing or corrupt
`_index.json`, the context index included), removes expired context entries, binds the socket, adds any missing
[runtime entries](#runtime-files-and-gitignore) to `.orchestry/.gitignore` and writes `.orchestry/daemon.json`.
A failed context expiry does not stop the start. Only then does it print its readiness line, exactly
`listening <socket>`, followed by its dispatch state. The banner on stderr says that the daemon is developmental
and that legacy writers are not excluded.

If recovery cannot save a change to a task, run or agent, for example because a store directory is not
writable, the start fails with exit 1 and names the write that failed. Nothing listens, so `orch daemon status`
exits 7, and the state the next start needs to finish the recovery is kept. Fix the cause and start again.

A foreground daemon runs until you stop it. `Ctrl+C` (SIGINT), SIGTERM and SIGHUP, which a terminal sends when
you close its window or tmux pane, all stop it cleanly, like [`orch daemon stop`](#stop).

**`--detach`** starts the daemon in a session of its own with stdin from `/dev/null`, and sends its stdout and
stderr to `.orchestry/daemon.log` (mode 0600). The command returns success only after the daemon has published
its socket; if the daemon fails first, the command fails too. A detached daemon keeps running when you close
the terminal or tmux pane you started it from. A reboot ends it, and orch does not test what happens at logout;
afterwards run `orch daemon status` and start it again yourself if no daemon answers.

**Log size.** `daemon.log` never grows past 10 MiB, and orch keeps exactly one backup, `.orchestry/daemon.log.1`
(mode 0600), of at most 10 MiB: 20 MiB in all. The limit is applied as the daemon writes, not by a periodic
check, and covers everything written to the detached daemon's output:

- Before a write would take the log past its limit, the daemon renames the log to `daemon.log.1`, replacing any
  older backup, and starts a new log.
- A detached start that finds a log over 10 MiB moves it to the backup first, and cuts a backup over 10 MiB
  (left by an earlier build) to 10 MiB.
- If a rotation, or opening or writing the log, fails, the daemon drops that output rather than let the log grow,
  and writes one fixed notice that names the failed step and the error kind, never a path or the dropped text:
  `orch daemon: cannot <rotate|open|write> daemon.log (<error kind>); output is dropped to keep it under 10 MiB`.
  Once a write succeeds again, the log starts with a line that counts the bytes dropped.

At the default log level a detached start and stop write only the one-line developmental banner (175 bytes).
With `RUST_LOG` or `ORCHESTRY_DEBUG` set, the log fills faster and rotates more often, within the same limit. To
reclaim the space, stop the daemon and delete `daemon.log.1`.

**One daemon per project.** Ownership is decided by a lock that the daemon holds for its whole life
(`.orchestry/.daemon-owner.lock`, never deleted) and by the project's existing orchestrator lock, which it
refreshes at least every 15 s even with dispatch off. If two starts race, one becomes the daemon and the other
exits 4 naming the winner's PID; if the winner died before it could write its PID, the loser reports the lock
conflict without a PID. A start also exits 4 when another live process holds the orchestrator lock, such as
`orch serve` or `orch run --watch` from this or an older build. The daemon never removes or takes over a live
holder, however old its lock file is: the message gives a `ps -p <pid> -o command=` hint so you can see what the
process is and stop it yourself. Because the daemon holds that same lock, `orch serve`, `orch run` and the
dashboard of this build cannot become the orchestrator while it runs.

**Refusals.** A start refuses, and changes nothing, when:

| Situation | Exit |
|---|---|
| `ORCH_DAEMON_DEVELOPMENTAL=1` is not set | 2 |
| `ORCH_TASK_ID` or `ORCH_AGENT_ID` is set (an agent's environment) and `--from-agent` is not given | 2 |
| Your effective user is not the owner of `.orchestry/`, including `root` on a project another user owns | 2 |
| `.orchestry/` is writable by group or others, or a pre-existing socket directory is not private to you | refuses; orch never changes the mode |
| `ORCH_SOCKET` or the computed socket path is empty, relative, contains a NUL byte or is over the [100-byte budget](#sockets-project-selection-and-daemonjson) | refuses with a short `ORCH_SOCKET` hint; the path is never truncated |
| A socket exists at the path and connecting to it fails with permission denied or any error other than connection refused, so orch cannot prove it stale | 2, with a diagnostic; the socket is left in place |
| No initialized project is found | 3 |
| Another daemon or another live orchestrator owns the project | 4 |
| The lock cannot be taken for any reason other than another holder (for example a file system without locking) | fails; it never falls back to a PID-only lock |

**Environment.** Before it starts any thread, the daemon takes a snapshot of its environment and removes
`ORCH_AGENT_ID`, `ORCH_AGENT_NAME`, `ORCH_TASK_ID`, `ORCH_TUI_CLIENT`, `ORCH_WEB_PASSWORD`, `ORCH_SOCKET`,
`ORCH_PROJECT_ROOT`, `DEBUG`, `PWDEBUG`, and `NODE_OPTIONS` when it contains `--inspect`. Every agent it starts
gets that snapshot, the agent's configured environment and the project root and socket that orch sets for it;
no request can add environment variables, a command or a working directory. The diagnostic programs the daemon
runs for the model catalog and `orch doctor` checks get the same cleaned environment, and their stderr never
reaches `daemon.log`. Credentials therefore come from the shell you started the daemon in: after you refresh a token in your shell, stop the daemon and start it again.
`orch --json daemon status` lists the names it removed, never their values.

## Check, toggle dispatch and stop

### Status

`orch daemon status` asks the running daemon, never a file, so it cannot report a daemon that has died. The plain
output shows the daemon's PID, whether dispatch is on or off, its socket path, its instance id, how many tasks it
is running, whether it still owns the project, the executable it was started from and when, its orch and protocol
versions, and the number of client sessions and the cap. Ownership is one line:

```text
ownership: held
ownership: LOST — stop and restart the daemon
```

<!-- W1b: the plain capture taken at b1216ce4 was removed because ruling 8.12 adds lines to it; re-capture the
full plain output from the I1b frozen commit (task tsk_bCS2Fyj owns the exact lines). -->

The plain output does **not** list the ids of the running tasks or the environment names the daemon removed. Ask
for JSON or YAML to see them, with the global flag before or after the command (`orch --json daemon status`,
`orch daemon status --yaml`):

```console
$ orch --json daemon status
{
  "pid": 13566,
  "project_root": {
    "utf8": "/private/tmp/twn-v0vgwbe_/p"
  },
  "socket": {
    "utf8": "/tmp/twn-v0vgwbe_/o.sock"
  },
  "protocol": 1,
  "binary": "0.13.0",
  "instance": "daemon_hPQDKpz",
  "executable": {
    "utf8": "/…/target/debug/orch"
  },
  "started_at": "2026-11-09T08:44:11.673Z",
  "dispatch": {
    "dispatch_on": false,
    "batch_active": false
  },
  "running_tasks": [],
  "sessions": 1,
  "session_cap": 64,
  "busy_rejections": 0,
  "ownership": "held",
  "removed_env": [
    "DEBUG",
    "ORCH_SOCKET"
  ]
}
```

| Field | Meaning |
|---|---|
| `socket`, `project_root`, `executable` | Paths, each as `{"utf8": "…"}`, or `{"hex": "…"}` for a path that is not valid UTF-8 |
| `instance` | This daemon's id; a restarted daemon has a new one |
| `running_tasks` | Ids of the tasks whose agents the daemon is running |
| `busy_rejections` | Connections refused with `busy` since start, because the handshake or session capacity was full |
| `ownership` | `held`, or `lost` when the daemon no longer owns the orchestrator lock |
| `removed_env` | Names, never values, removed from the agents' environment |

**Lost ownership.** The daemon re-checks the orchestrator lock (`.orchestry/orchestry.lock`) every 10 seconds,
whether dispatch is on or off. If the file is gone or names another PID, the daemon sets `ownership` to `lost`,
turns dispatch off and refuses `orch daemon dispatch on` with `daemon ownership was lost` (exit 1). It does not
recreate the lock. Plain `orch daemon status` then prints `ownership: LOST — stop and restart the daemon`;
`--json` and `--yaml` show `ownership` as `lost`. To recover, stop the daemon and start it again.

### Dispatch on and off

`orch daemon dispatch on` lets the daemon start waiting tasks, as `orch run --watch` does; `orch daemon dispatch
off` stops it starting new ones. Turning dispatch off does **not** stop agents that are already running, release
the project or stop the daemon's other work: it still answers requests, removes expired context entries and talks
to remote peers. A daemon always starts with dispatch off unless you pass `--dispatch on`, also after a restart.

### Stop

`orch daemon stop` asks the daemon to shut down and returns only after the daemon's process has exited, so
`orch daemon stop` followed by `orch daemon start` works. The wait is at most 30 s; if the process is still
there after that, the command says `daemon accepted stop but PID <pid> is still running after 30 s` and exits 7. Shutting down, the daemon
stops taking requests and dispatching, stops its running agents (each gets the usual 10 s grace before it is
killed), records their runs as cancelled and settles their tasks' retries, removes its own socket and
`daemon.json`, and releases ownership last. SIGTERM, SIGINT and SIGHUP do the same.

- The stop works even when the daemon and this `orch` are different versions (see [Version skew](#version-skew)).
  It is checked against your user and the project, and does nothing else.
- If cleanup fails, the command reports the failure instead of a clean stop, and the daemon keeps the evidence
  needed for recovery.
- If the daemon accepted the stop but the connection closed before it confirmed that it finished, the outcome is
  unknown: the message says that the stop may have been applied and to inspect state before retrying, and the
  command exits 7 (see [Reply lost](#when-no-daemon-answers)). `orch daemon stop` never kills, restarts or
  retries anything; run `orch daemon status` to see whether the daemon is still there.

## Exit codes

orch's exit codes, with the one this work adds:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error |
| 2 | Invalid arguments; for `orch daemon`: developmental gate not set, agent context without `--from-agent`, a user who does not own the project, or a socket that `orch daemon start` cannot prove stale |
| 3 | Not initialized: no project found |
| 4 | Lock conflict: another daemon or live orchestrator owns the project |
| 5 | Adapter error |
| 6 | Workspace error |
| **7** | **No usable daemon**: none answers, a stale socket, a refused connection, a failed handshake, a version mismatch, the daemon is busy (`daemon_busy`) or belongs to another project (`wrong_project`); also a request whose reply was lost, so its outcome is unknown, and an `orch daemon stop` whose wait for the process to exit timed out |

In this build only `orch daemon status`, `stop` and `dispatch` can exit 7. From PR 2 (planned) every command that
reads or changes project state exits 7 the same way. The [MCP chapter](17-mcp-server.md#exit-codes-and-startup-errors)
lists `orch mcp`'s own startup exit codes.

## When no daemon answers

When `orch daemon status`, `stop` or `dispatch` find no daemon, or one they cannot use, they change nothing,
start nothing, and print one message that names the project and what went wrong, then exit 7:

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon start
```

The detail says why: no socket, a stale socket left by a daemon that exited, connection refused, the handshake
failed, permission denied or the connection was lost before the request was sent. A client never deletes a
stale socket or `daemon.json` and never takes over the project. Only the next explicit `orch daemon start`
replaces a stale socket, and only when connecting to it is refused (connection refused). When the connect fails
in any other way, such as permission denied, the start cannot tell whether a daemon is listening: it leaves the
socket alone and exits 2 with a diagnostic.

**In an agent's environment** (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set) the same failure never tells the agent to
start a daemon:

```text
  ✕ The orch daemon for <root> is not running. Do not start it; report this to the owner and stop.
    Detail: <safe detail, including both versions on skew>
```

Three cases are not "no daemon" and say so:

- **Reply lost.** The connection broke after the request was sent, so orch cannot know whether the daemon applied
  it. The message says that the request may have been applied and that you should inspect state before retrying,
  instead of the plain no-daemon message; exit 7. Look at what the request would have changed before you send it
  again.

- **Busy.** All client sessions are in use. The message is
  `daemon_busy (orch daemon PID <kernel peer pid or unavailable> is running; N/N sessions)`, exit 7, with no start
  hint. Close some clients and try again. `orch daemon stop` does
  not need a session slot, so it still gets through.
- **Wrong project.** The daemon that answered belongs to another project than the one you selected:
  `wrong_project`, exit 7, naming the expected and the actual project paths. orch does not tell you to delete or
  kill anything; check `ORCH_SOCKET` and `ORCH_PROJECT_ROOT`.

**Planned (PR 2):** every other command, the dashboard, `orch web` and `orch mcp` use this message. The CLI and
`orch web` at start exit 7; the dashboard shows the message and offers `r` to retry and `q` to quit (exit 7);
`orch web` answers HTTP 503 with a banner if the daemon goes away while it serves (unauthenticated requests see
only `orch daemon unavailable`); `orch mcp` stays running and returns the message, with `exit_code` 7, as the
error of `initialize` and of every tool call, so the first call of a new MCP session fails until you start the
daemon. `orch doctor` reports the message plus what it can safely read from the discovery file, and exits 7.
`orch --help`, `orch --version` and `orch init` never need a daemon.

## Sockets, project selection and `daemon.json`

### Which project

A command selects its project first, then the daemon to talk to:

| Priority | Input | What happens |
|---|---|---|
| 1 | `ORCH_SOCKET` (an absolute path) | Talk to exactly this socket. The project is still selected by rows 2–3, and the daemon must belong to it, or the command fails with `wrong_project`. |
| 2 | `ORCH_PROJECT_ROOT` | Use that project, from any directory, including an agent's task worktree. An invalid or uninitialized root fails; orch does not fall back to the current directory. |
| 3 | Current directory | The nearest directory upward that contains `.orchestry/`. Two separately initialized checkouts of one repository are two projects. |

A symbolic link to the project root works; a symbolic link in place of `.orchestry/` itself is refused. orch
identifies a project by the device and inode of its `.orchestry/` directory, not by its path. The daemon gives
the agents it starts `ORCH_PROJECT_ROOT` and `ORCH_SOCKET` explicitly, so a shell setting cannot redirect them.
One host and one `.orchestry/` per project only: sharing a project's state between machines is unsupported, and a
project on a network file system (NFS, SMB) is unverified.

### Where the socket is

- **Default:** `<project root>/.orchestry/run/orch.sock`, when that absolute path is at most 100 bytes (macOS
  allows 104 bytes in a socket path, so orch keeps a margin; the same rule applies on Linux).
- **Long project paths:** `$TMPDIR/orch-<first 16 hex digits of the SHA-256 of the project root>/orch.sock`
  (the platform temporary directory if `TMPDIR` is unset). This path must fit the same budget.
- **`ORCH_SOCKET`** overrides both, for tests and unusual layouts. It must be absolute and fit the budget.

The daemon creates `run/` (or the temporary directory) with mode 0700 and binds the socket with mode 0600. A
pre-existing directory must be owned by you with no group or other access; the daemon refuses it otherwise and
never changes its mode. `.orchestry/` itself must be owned by you and not writable by group or others. Both ends
check the other's user with the kernel before any byte is trusted: a client never sends a byte to a daemon of
another user, and the daemon refuses a client of another user.

### `daemon.json`

The daemon writes `.orchestry/daemon.json` (schema 1, mode 0600, at most 16 KiB) after it binds the socket. It
records `project_root`, `project_id`, `socket`, `pid`, `binary` (orch version), `protocol`, `executable` (the
binary the daemon was started from), `started_at` and `instance`. Clients read it once per connection attempt to
find the socket; it is never proof that a daemon is alive. Do not edit or delete it, or the socket, while a
daemon runs.

### Runtime files and `.gitignore`

`run/`, `daemon.json`, `daemon.log` and `daemon.log.1` in `.orchestry/` are runtime files. `orch init` lists them
in `.orchestry/.gitignore` under `# Daemon runtime`, and every `orch daemon start` appends whichever of these
lines are missing, so a project created with orch 0.13.0 gets them at its first start. Existing lines are never
rewritten or reordered. If the start cannot update the file (for example it is a symbolic link or not yours), it
logs a warning and starts anyway: the entries never block readiness.

### Who can use the socket

Anything that runs as your user can use the socket with your full authority, including a container or sandbox
that can see `.orchestry/` or the socket, and remote principals that `orch web` or a peer link admit. An agent
identity (`--agent`, `ORCH_AGENT_ID`) is attribution, not authorization. Do not expose `.orchestry/` or the
socket to an environment you would not trust as yourself.

## Version skew

A client and a daemon talk only when they are the same orch version **and** the same local protocol version
(`LOCAL_PROTOCOL`, 1 in this build). When they differ, in either direction, the client stops before any request
is decoded, so nothing in your project changes. The message names the daemon's and this orch's versions and
protocols in `<detail>` and changes the hint:

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon stop, then orch daemon start
```

`orch daemon stop` works across the mismatch, with either binary, even if the old executable has been deleted.
Stop the old daemon, then start the new one. orch never restarts the daemon for you.

For developers: two builds with the same orch version and the same protocol version cannot tell each other
apart. Stop the daemon before you switch builds and start the binary you mean; `orch daemon status` shows the
executable path and start time of the one that runs.

## Recovery

This table follows the spec's
[normative recovery table](../specs/2026-11-09-daemon-owned-state.md#normative-recovery-table). No row lets a
client start, restart or repair a daemon; every recovery is something you do.

| Situation | What you see | What to do | Applies |
|---|---|---|---|
| The daemon crashed or was killed (`kill -9`) | The next `orch daemon status` reports no daemon (exit 7). The stale socket and `daemon.json` stay until the next start. | Start it again. The new daemon has dispatch off, clears stale claims, records runs whose agent died as cancelled and their agents as idle, and leaves remote work as unknown until the peer syncs. Check tasks and runs before you retry anything or turn dispatch on. | now |
| A foreground daemon's terminal or pane closed | SIGHUP: a clean stop, the same as `orch daemon stop`; its agents are stopped. | Start it again in a new pane, or use `--detach`. | now |
| A detached daemon after a reboot (logout is untested) | No daemon answers. | Start it again. orch supplies no launchd job, watchdog or restart. | now |
| Recovery at start cannot save a task, run or agent change | The start exits 1 and names the write that failed; `orch daemon status` exits 7. | Fix the cause (permissions, disk space), then start again; the state needed to finish the recovery is kept. | now |
| You upgraded orch and the old daemon still runs | Version skew message, exit 7. | `orch daemon stop` (either binary), then start the new one. Running agents are stopped and reconciled; this is not a transparent upgrade. | now |
| The daemon hangs, or its socket was removed | Commands time out or report no daemon; status never claims liveness from files. | Read the PID in `daemon.json` as an untrusted hint. Only if `ps -p <pid> -o command=` shows an orch daemon for this project, send `kill -TERM <pid>`. If you cannot verify the process, do not signal it. | now |
| orch crashed between writing an entity and its `_index.json` | One writer does not make two file writes one transaction. Start-up rebuilds a missing or corrupt index of tasks, agents, runs, goals, teams, messages or context, but cannot detect a stale index that is still valid. | Offline repair only: stop the daemon, remove the affected `_index.json`, start it. Never remove an index while a daemon runs. | now |
| Re-running `orch init` | It does not overwrite the project and prints how to start the daemon. It never starts one. | Start the daemon yourself. | now |
| An agent loses the daemon mid-task | Exit 7 with the agent-context message. The outcome of a change whose reply was lost is unknown. | The agent stops and reports to you. Start the daemon, inspect the task, run and state, then decide on a retry. A restart does not resume the agent's conversation. | planned (PR 2) |
| An MCP client's first call fails | The `initialize` error with the no-daemon message; the `orch mcp` process stays up. | Start the daemon, then reconnect, re-enable or restart the MCP session; not every MCP client retries by itself. | planned (PR 2) |
| A bootstrap script or a cron job | `orch init` never starts a daemon; state commands fail visibly when none runs. | Start the daemon explicitly in the script, then check it (example below). In cron, let the failure exit non-zero with its stderr. | planned (PR 2) |
| Upgrading from orch 0.13.0 | Old `orch` processes, and `orch mcp` servers held open by IDE or Claude sessions, write without the daemon's locks. | Stop every old orch process, including MCP servers; `pgrep -fl orch` must show none before the first start. Then restart those sessions on the new build. | planned (PR 2) |

Owner-written bootstrap from the spec, for when clients use the daemon (planned, PR 2):

```sh
set -eu
orch init
orch daemon start --detach
orch daemon status  # detach returns success only after readiness
orch task list
```

## How the existing commands map (planned, PR 2)

In this build `orch serve`, `orch run` and the dashboard behave exactly as in orch 0.13.0. From PR 2 dispatch is
a mode of the daemon, not a separate process:

| Command | From PR 2 (planned) |
|---|---|
| `orch serve`, `orch run --watch` | Start the daemon in the foreground with dispatch on. If a daemon already runs, exit 4 with `Use: orch daemon dispatch on`. |
| `orch run --once` | One scheduling check in the running daemon, with that check's events; does not stop the daemon or change dispatch. |
| `orch run <task-id>` | Start that task in the running daemon and follow it until it ends. Disconnecting does not stop the run. |
| `orch run --all` | One finite batch of the currently eligible tasks in the running daemon. |
| `orch serve --once` | Run until every tracked task is finished, in the running daemon; needs dispatch off (exit 2 with `dispatch is ON; serve --once needs dispatch OFF` otherwise; a second batch exits 4). A later `dispatch on` or `off` ends the batch; agents keep running. |
| Dashboard start and stop watch | `dispatch on` and `dispatch off`; never starts or stops a process. |
| `orch tui --observe`, `orch tui --listen` | Removed: the dashboard is always a client; peer listening is a daemon start option. |
| `orch doctor` | A client: with no daemon it prints the no-daemon message and discovery facts and exits 7. |
| `orch init`, `--help`, `--version` | Need no daemon. |

The [plan](../plans/2026-11-09-daemon-owned-state.md#tasks) lists what lands in each PR.
