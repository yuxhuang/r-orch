# The orch daemon

The **daemon** is the one orch process that owns a project. It reads and writes everything under `.orchestry/`,
starts and stops the agents, and runs the orchestrator when dispatch is on. Every other orch command, the
dashboard, `orch web` and `orch mcp` is a **client**: it asks the daemon to do the work over a private Unix socket
and never opens the project's state itself. **No orch command that reads or changes project state works without
a running daemon.** You always start it yourself: no orch command, client, retry or error path ever starts one.
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
- [How `serve`, `run` and the dashboard map to the daemon](#how-serve-run-and-the-dashboard-map-to-the-daemon)
- [How clients follow changes](#how-clients-follow-changes)
- [Upgrading from orch 0.13.0](#upgrading-from-orch-0130)

## Why a daemon

Up to orch 0.13.0, every orch process wrote the project's files on its own: the orchestrator, each one-off
command, the dashboard, `orch web` and every `orch mcp` server. Two of them changing the same thing at the same
time could lose an update: a field could go back to an older value, a renewed context key could disappear, or a
task could exist as a file but be missing from `orch task list` (see
[Running two writers in orch 0.13.0](17-mcp-server.md#one-writer-the-daemon) in the MCP chapter). Now every client
goes through the daemon, so there is only one writer and these races cannot happen between processes. Your files
keep their format: tasks, agents and goals are still plain YAML in `.orchestry/`.

Because dispatch, the running agents and every change are in one process, the daemon makes three guarantees for
every client, whether the request comes from the CLI, the dashboard, `orch web`, `orch mcp` or an agent:

- A task linked to a paused goal does not start. The daemon rereads the task and its goal just before it starts an
  agent, and that check is ordered with the pause, whichever client sent it.
- Cancelling a task stops its running agent before the cancel reports success, whichever client sent it.
- Reads never delete expired context entries; `orch context get`, `orch context list` and the MCP context tools
  only hide them. The daemon removes expired entries itself: at start, and on a timer, also with dispatch off.

What the daemon does **not** promise:

- Two file writes (an entity and its `_index.json`) are still not one transaction if the daemon crashes between
  them ([Recovery](#recovery)).
- A request whose reply was lost has an unknown outcome.
- Another program that edits `.orchestry/` while the daemon runs is not kept out.
- **Read times that do not depend on writes.** Reads such as `orch task list` and `orch status` share a FIFO
  operation boundary with failure-record writes: each read waits behind operations already queued to acquire
  that boundary. This orders boundary acquisition, not every request's network arrival; some external operations
  do work outside it. These store writes await file sync and attempt a best-effort parent-directory sync before
  releasing the boundary. During a failure-recording burst, reads can take longer, depending in part on
  durable-write latency. The measured finite burst below converged with working storage; this is not a bound
  on a read whose storage operation hangs. Queueing alone needs no state recovery; storage errors and hung
  operations need their own diagnosis. An ordinary request still waiting to acquire the boundary after 10 s is
  refused without being run (`Overloaded`, `NotExecuted`, exit 7); this is not a completion deadline once work
  starts. No sampled read was refused in the measurement below. What to do: [Commands are slow while many tasks fail to
  start](../RUNBOOK.md#orch-commands-are-slow-while-many-tasks-fail-to-start).

  Measured on an Apple M5 Max (macOS 27.2) with a release build of commit `a08fad1b`, on a shared machine at
  load average 12.30–18.93: an isolated project of 500 tasks and six agents whose workspaces could not be
  prepared made exactly 1,500 preparations, three attempts per task, then stopped retrying. While the tasks were
  failing, all 792 `orch task list` reads and all 792 `orch status` reads succeeded; `task list` p99/max were
  158/228 ms and `status` 129/209 ms. Afterwards, with the daemon restarted, at load 15.84–18.83 and 201 reads per
  command, `task list` p95 was 27.61 ms against 30.84 ms for an idle project measured at the same time, and
  `status` 21.00 ms against 20.05 ms. These are observations on one machine, not promised bounds.

## Command reference

`orch serve` and `orch run --watch` also start the daemon, with dispatch on; see
[How `serve`, `run` and the dashboard map to the daemon](#how-serve-run-and-the-dashboard-map-to-the-daemon).

| Command | What it does | Exit codes |
|---|---|---|
| `orch daemon start [--detach] [--dispatch on\|off] [--from-agent]` | Starts the daemon for the project, in the foreground unless `--detach`. Dispatch is **off** unless `--dispatch on`. Peer-link listener options are start options too; `orch daemon start --help` lists them. | 0 after a clean stop (foreground) or after readiness (`--detach`); 2, 3, 4 on refusal |
| `orch daemon status` | Asks the running daemon for its live status. | 0; 1 with `request cancelled before execution` when the daemon lost ownership of the project while the request was waiting ([Lost ownership](#status)); 7 when no compatible daemon answers |
| `orch daemon stop` | Asks the daemon to shut down cleanly and waits until it confirms its cleanup and has released the project (its process has exited or it has released its ownership lock), for at most 30 s. Works across a version mismatch. | 0 on a completed clean stop; 1 when the daemon reports incomplete cleanup (`daemon shutdown cleanup failed; recovery evidence was retained`); 7 when no daemon answers, the outcome is unknown or the wait times out (with a message of its own) |
| `orch daemon dispatch on\|off` | Turns dispatching of future work on or off in the running daemon. | 0; 7 when no compatible daemon answers |

| Flag | Meaning |
|---|---|
| `--detach` | Run in the background in a session of its own. The command returns only after the daemon is ready, and fails if the daemon fails before that. |
| `--dispatch on\|off` | Dispatch state at start. Default `off`, so you can queue work before anything runs. |
| `--from-agent` | Allow the start from an agent's environment (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set). Without it the start refuses with exit 2. Use it only when you, the owner, deliberately start a daemon from such a shell. `orch serve` and `orch run --watch` take it too; `orch serve --once` and `orch run` without `--watch` reject it (exit 1). |

`orch daemon` also needs a project: run it in the project or a subdirectory, or set `ORCH_PROJECT_ROOT` (see
[Project selection](#sockets-project-selection-and-daemonjson)). Without an initialized project it exits 3.

## Start the daemon

```sh
orch daemon start
```

The daemon first checks the project and your user, takes ownership of the project, recovers the state a previous
orchestrator left (the same reconciliation `orch serve` does at start, and a rebuild of any missing or corrupt
`_index.json`, the context index included), removes expired context entries, binds the socket, adds any missing
[runtime entries](#runtime-files-and-gitignore) to `.orchestry/.gitignore` and writes `.orchestry/daemon.json`.
A failed context expiry does not stop the start. Only then does it print its readiness line, exactly
`listening <socket>`, followed by its dispatch state. From then on every client of the project can use it.

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

With `RUST_LOG` or `ORCHESTRY_DEBUG` set, the log fills faster and rotates more often, within the same limit. To
reclaim the space, stop the daemon and delete `daemon.log.1`.

**One daemon per project.** Ownership is decided by a lock that the daemon holds from start until the last
step of its shutdown (`.orchestry/.daemon-owner.lock`, never deleted) and by the project's existing orchestrator
lock, which it refreshes at least every 15 s even with dispatch off. If two starts race, one becomes the daemon and the other
exits 4 naming the winner's PID; if the winner died before it could write its PID, the loser reports the lock
conflict without a PID. A start also exits 4 when another live process holds the orchestrator lock, such as
`orch serve` or `orch run --watch` from this or an older build. The daemon never removes or takes over a live
holder, however old its lock file is: the message gives a `ps -p <pid> -o command=` hint so you can see what the
process is and stop it yourself. Because the daemon holds that same lock, an `orch serve` or `orch run --watch`
of orch 0.13.0 cannot become the orchestrator while it runs; [stop every old orch process](#upgrading-from-orch-0130)
before the first start anyway, because old one-off commands and old `orch mcp` servers write without that lock.
This holds as long as `.orchestry/` and its two lock files are not deleted or replaced under a running daemon, for
example by a restore from a backup: a second daemon can then start on the same project. See
[ARCHITECTURE.md](../ARCHITECTURE.md#21-daemon-ownership-guarantees-and-limits) and, for the `wrong_project` message
that shows it, [the runbook](../RUNBOOK.md#no-orch-daemon-is-running-for-project-root).

**Refusals.** A start refuses, and changes nothing, when:

| Situation | Exit |
|---|---|
| `ORCH_TASK_ID` or `ORCH_AGENT_ID` is set (an agent's environment) and `--from-agent` is not given; the same for `orch serve` and `orch run --watch` | 2 |
| `orch serve` or `orch run --watch` is given an option of orch 0.13.0 that a daemon start cannot honour (`--tick-interval`, `--log-file`, `--verbose`, a `--log-format` other than `json`); see [below](#how-serve-run-and-the-dashboard-map-to-the-daemon) | 2, before any socket or lock is touched |
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

```console
$ orch daemon status
orch daemon PID 76126 · dispatch OFF
  executable: /…/target/debug/orch
  started: 2026-11-09T12:07:26.840Z
  version: orch 0.13.0 protocol 2
  sessions: 1/64
  ownership: held
  socket: /private/tmp/i1b-status-demo/sk/s
  instance: daemon_Fc-nomx
  running tasks: 0
```

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
  "protocol": 2,
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
whether dispatch is on or off. If the file is gone, names another PID or no longer holds a PID, the daemon sets `ownership` to `lost` and
turns dispatch off. It does not recreate the lock. Plain `orch daemon status` then prints
`ownership: LOST — stop and restart the daemon`; `--json` and `--yaml` show `ownership` as `lost`. To recover, stop
the daemon and start it again.

The daemon also writes one fixed line to its standard error when it finds its ownership lost (two spaces, then `⚠`):

```text
  ⚠ ownership lost: the project PID lock is missing or names another process; dispatch is off and this daemon is still running
```

A foreground daemon writes it to its terminal; a detached daemon (`--detach`) writes it to the capped
`.orchestry/daemon.log`. `RUST_LOG` does not suppress it. The daemon attempts it once, from a separate thread that
nothing waits for, and also when recording the loss returned an error. A recording that never completes, panics or is
cancelled is not covered: the line is written only after the recording returns. That a full standard-error pipe does
not hold the daemon up is shown at the default log level; with an explicit `RUST_LOG` the daemon's other log lines go
to the same standard error as before and are not made non-blocking, so a pipe that nobody reads can still stall
them (shown only with a standard error that is being read). It is best effort: it arrives late when standard error is
drained later, it is lost if the daemon exits before anyone reads it, and it is absent if the thread or the duplicate
of the descriptor cannot be created (a failed write is not retried). `orch daemon status` stays the authoritative
signal. The notice does not stop a second writer: the daemon disables dispatch and reports LOST, but it does not exit
by itself and it does not prevent another process from writing to the project.

Two ways to get here that are worth knowing. If you delete `.orchestry/orchestry.lock` while a daemon runs (the
[troubleshooting steps](13-troubleshooting.md#orch-daemon-already-running-pid-unavailable-and-no-daemon-is-running)
tell you to delete it only after checking that no daemon runs), the daemon reports LOST at its next check. And orch
0.13.0 treats a lock file that has not been touched for 60 seconds as abandoned even when its process is alive; the
daemon touches the file every 10 seconds, so a daemon whose lock has not been refreshed for more than 60 seconds, for
example while suspended (a laptop with its lid closed), is exposed to a 0.13.0 binary that starts. That is read from
the code, not observed. Stop every 0.13.0 process before you start the daemon.

From then until it stops, the daemon refuses the requests that switch dispatch or start a run, from every client,
with `daemon ownership was lost` (exit 1; nothing was executed):

- `orch daemon dispatch on` and `orch daemon dispatch off`;
- `orch run --once`, `orch run <task-id>`, `orch run --all` and `orch serve --once`;
- the dashboard's `/watch`, `/run` and `/run-all`, which show `Failed: daemon ownership was lost` (`/pause` has
  nothing to stop: the header shows dispatch off, and it answers `Watch mode not active`).

It still executes every other request, including the ones that change state.

A request that the daemon had accepted before it lost ownership, and had not started to execute, is refused with
`request cancelled before execution`: the command prints it on stderr and exits 1, and nothing was executed.
`orch daemon status` is one of the commands this can happen to, at the moment of the change. A diagnostic that was
already running at that moment, such as `orch doctor`, is stopped and ends with `diagnostic cancelled` (exit 1);
run it again after the restart. A command that was following its runs stops following them, and the agents keep
running: `orch run <task-id>` and `orch run --all` end with `observation interrupted; runs continue in the daemon`
(exit 1), `orch serve --once` ends its batch as `interrupted` (exit 1), and `orch logs --follow` returns without a
message (exit 0). The agents
they started keep running until you stop the daemon. orch does not retry any of these requests and does not restart anything. Repeat a
command that only reads. Before you repeat a command that changes state, stop the daemon and start it again, as
described above. A request made after the change is answered, and `orch daemon status` then prints
`ownership: LOST`.

**What may have been written before you stop it.** The daemon checks for ownership loss on a 10-second timer;
scheduling and waiting for the control operation can delay recording the loss further. Until the loss is recorded,
dispatch, if it is on, can still start tasks. After it is recorded, the requests listed above are refused, but every
other request, including writes, runs until you stop the daemon. Agents already running still commit their results,
and the daemon's own maintenance (dead-run reconciliation, context expiry) and a hosted peer runtime keep writing too.
So the daemon can have written to the project in that time. A second daemon cannot have, as long as `.orchestry/.daemon-owner.lock` was not deleted or replaced as
well: the daemon holds the lock on that file until it exits. Whether a process that is not a daemon wrote to the
project in that time is not excluded by the code, and no test covers it.

### Dispatch on and off

`orch daemon dispatch on` lets the daemon start waiting tasks, as `orch run --watch` does; `orch daemon dispatch
off` stops it starting new ones. Turning dispatch off does **not** stop agents that are already running, release
the project or stop the daemon's other work: it still answers requests and removes expired context entries. A
running daemon keeps its links to the remote peers registered with `orch remote add` whatever the dispatch state:
dispatch off stops automatic task starts only. A daemon always starts with dispatch off unless you pass `--dispatch on`, also after a restart.

### Stop

`orch daemon stop` asks the daemon to shut down and returns only after the daemon has confirmed that its cleanup
finished and has released the project: either its process has exited or it has released its ownership lock
(`.orchestry/.daemon-owner.lock`), whichever the command sees first. The lock is the last thing the daemon lets go,
so `orch daemon stop` followed by `orch daemon start` works. A success does not prove the old process is gone
from the process table: it can still be exiting for a moment after it has released the lock. The wait is at most
30 s in total; if the daemon has not confirmed its cleanup and released the project by then, the command says
`daemon accepted stop but PID <pid> is still running after 30 s` and exits 7. Shutting down, the daemon
stops taking requests and dispatching, tries to stop its running local agents (each gets the usual 10 s grace before
it is killed), records the runs it stopped as cancelled and settles their tasks' retries, removes its own socket and
`daemon.json`, and releases ownership last. Remote runs are not cancelled. SIGTERM, SIGINT and SIGHUP do the same.

- The stop works even when the daemon and this `orch` are different versions (see [Version skew](#version-skew)).
  It is checked against your user and the project, and does nothing else.
- If cleanup fails, the command reports the failure instead of a clean stop and exits 1 with
  `daemon shutdown cleanup failed; recovery evidence was retained`, and the daemon keeps the evidence needed for
  recovery. This happens, for example, when it cannot verify an agent's saved PID or cannot confirm that an agent stopped; the
  descendants of an agent can also survive a stop. Check the result before you start the daemon again.
- If the daemon accepted the stop but the connection closed before it confirmed that it finished, the outcome is
  unknown: the message says that the stop may have been applied and to inspect state before retrying, and the
  command exits 7 (see [Reply lost](#when-no-daemon-answers)). `orch daemon stop` never kills, restarts or
  retries anything; run `orch daemon status` to see whether the daemon is still there.

**After a timeout the daemon can still own the project.** The timeout ends only the wait, not the daemon. A
daemon that is finishing an operation it already accepted, for example a `git worktree add` that hangs while it
prepares a task's workspace, keeps the project until that call returns, then exits by itself. Until then:

- `orch daemon status` reports `No orch daemon is running for <root> (connection refused)` and exits 7: the daemon
  no longer answers, although its process, socket file and `daemon.json` are still there. Status cannot tell you
  that the daemon is still running; check the PID from the stop message with `ps -p <pid> -o command=`.
- `orch daemon start` exits 4 with `orch daemon already running (PID <pid>)`.
- Another `kill -TERM <pid>` does nothing; the shutdown is already under way.

Wait for the operation to finish, or end it as [Recovery](#recovery) describes.

## Exit codes

orch's exit codes, with the one this work adds:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | General error; also a request that was accepted but had not started when the daemon lost ownership of the project (`request cancelled before execution`) |
| 2 | Invalid arguments; for a daemon start (`orch daemon start`, `orch serve`, `orch run --watch`): agent context without `--from-agent`, a user who does not own the project, or a socket that the start cannot prove stale; a foreground `orch serve` or `orch run --watch` given `--tick-interval`, `--log-file`, `--verbose` or a non-`json` `--log-format`, and `--tick-interval` on any `serve` or `run`; also `orch serve --once` while dispatch is on |
| 3 | Not initialized: no project found |
| 4 | Lock conflict: another daemon or live orchestrator owns the project, including `orch serve` or `orch run --watch` while a daemon runs; a second `orch serve --once` batch |
| 5 | Adapter error |
| 6 | Workspace error |
| **7** | **No usable daemon**: none answers, a stale socket, a refused connection, a failed handshake, a version mismatch, the daemon is busy (`daemon_busy`) or belongs to another project (`wrong_project`); also a request whose reply was lost, so its outcome is unknown, and an `orch daemon stop` whose wait for cleanup and release of the project timed out |

Every command that reads or changes project state exits 7 the same way: `orch task list` as much as
`orch task add` or `orch daemon status`. `orch mcp` does not exit; it returns the message with `exit_code` 7 as an
MCP error (see [When no daemon answers](#when-no-daemon-answers)). The
[MCP chapter](17-mcp-server.md#exit-codes-and-startup-errors) lists `orch mcp`'s own startup exit codes.

## When no daemon answers

When a command finds no daemon, or one it cannot use, it changes nothing, starts nothing, and prints one message
that names the project and what went wrong, then exits 7:

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon start
```

The detail says why: `no daemon.json`, `daemon.json failed its safety checks`, `no socket`, `connection refused`
(a stale socket left by a daemon that exited), `timed out`, `permission denied`, `handshake failed`,
`connection lost` (before the request was sent) or `daemon is stopping`. A version mismatch names both versions
instead ([Version skew](#version-skew)). A client never deletes a
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

An agent should treat exit 7 as an infrastructure failure: stop the coordinated work and report it. orch cannot
make the AI session that ran the command stop; the instruction in the message is the contract. Starting the
daemon again does not resume the agent's conversation.

Three cases are not "no daemon" and say so:

- **Reply lost.** The connection broke after the request was sent, so orch cannot know whether the daemon applied
  it. Instead of the no-daemon message you see, exit 7:

  ```text
    ✕ Cannot confirm the orch request outcome for <project root> (request outcome unknown)
      the request may have been applied; inspect state before retrying.
  ```

  In an agent's environment the second line ends with ` Report this to the owner and stop.` Look at what the
  request would have changed before you send it again; orch never sends it again for you.
- **Busy.** The daemon is running but has no room for another client: all sessions are in use, or too many
  clients are connecting at the same moment. A client waits up to about 13 s for room (10 s of waiting plus one
  last connection attempt) before it gives up (no request has been sent by then, so waiting cannot run anything
  twice), then exits 7, with no start hint:

  ```text
    ✕ daemon_busy for <project root> (orch daemon PID <pid or unavailable> is running; <n>/<cap> sessions)
      Retry when a session closes.
  ```

  When the connections, not the sessions, were full the count can be well under the cap (`1/64 sessions`). Close
  some clients and try again. `orch daemon stop` does not need a session slot and never waits, so it still gets
  through.
- **Wrong project.** The daemon that answered belongs to another project than the one you selected. Exit 7:

  ```text
    ✕ wrong_project (expected <project>, daemon serves <other project>)
      Check ORCH_SOCKET and ORCH_PROJECT_ROOT.
  ```

  orch does not tell you to delete or kill anything.

**Each kind of client** shows the same text in its own way:

| Client | With no usable daemon |
|---|---|
| A CLI command | Prints the message on stderr and exits 7. |
| The dashboard (`orch`, `orch tui`) | Shows the message full-screen. `r` tries to connect again, `q` quits with exit 7. It never starts anything. |
| `orch web` | At start: prints the message and exits 7. If the daemon goes away while it serves: a new page or tab gets HTTP 503 with the message as plain text, after the usual login; with `--no-auth` it gets only `orch daemon unavailable`, never the project or socket path. A tab whose dashboard is already open shows the dashboard's own no-daemon screen. |
| `orch mcp` | Stays running. `initialize` and every tool call return the message as an MCP error with `exit_code` 7, so a new MCP session's first call fails until you start the daemon. It exits 0 when its client closes stdin. A repeated `initialize` on a session that already initialised still succeeds while no daemon runs (a known limit, accepted for a later fix). See [the MCP chapter](17-mcp-server.md#when-no-daemon-is-running). |
| `orch doctor` | Runs no check and starts no process. Prints `orch doctor · discovery only (daemon unavailable)`, the project root and `daemon discovery: available` or `unavailable`, then the message, and exits 7. Outside a project it prints `project discovery: unavailable`, then `Not initialized` and `Run: orch init`, and exits 3. See [Check your setup](07-running-and-monitoring.md#check-your-setup). |

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
never changes its mode. `.orchestry/` itself must be owned by you and not writable by group or others
(see [Permissions of `.orchestry/`](#permissions-of-orchestry)). Both ends
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

### Permissions of `.orchestry/`

New `orch init` projects create `.orchestry` with mode 0700; existing projects keep their modes, and opening the
directory later requires protecting all daemon-consumed files and directories yourself. `orch init` and
`orch daemon start` never change the mode of an existing `.orchestry`.

The daemon protects its socket and [runtime files](#runtime-files-and-gitignore), but ordinary state and store
directories follow the process umask, and startup checks only the owner and write bits of `.orchestry` itself.
Untrusted local users must not be able to modify state files or replace their directories—umask 002 can permit group
members to forge messages/context and alter runtime state despite the private socket, so use an owner-only
`.orchestry` (`chmod 700 <state-directory>`) in an owner-controlled parent, without ACL grants to untrusted users, or
treat every filesystem writer as the project owner.

**Existing projects are not migrated.** A daemon started under umask 002 on a project whose `.orchestry` is 0755
still writes group-writable state. In a check on macOS the daemon wrote `state.json` and the `_index.json` files with
mode 0664 and `messages/`, `context/` and `teams/` with mode 0775, and `.orchestry` stayed 0755. To protect such a
project, run `chmod 700 '<absolute path of .orchestry>'` as its owner, the command the refusal below prints.

**When orch refuses.** If `.orchestry` is writable by group or others, or owned by another user and readable by you,
commands such as `orch task list`, `orch daemon status` and `orch daemon start` exit 2 and change nothing. The message
names the project and the directory and says `No permissions were changed.`; when the directory belongs to the caller
and is group- or world-writable, it also prints the owner's commands with the paths shell-quoted. For a group-writable
directory, with your own paths:

```text
  ✕ Refusing project `<project root>`: `<project root>/.orchestry` is group/world-writable (mode 0775). No permissions were changed. For an owner-only project, run `chmod 700 '<project root>/.orchestry'` as its owner, keep its parent directories and ACLs under trusted control, then start explicitly with `(cd '<project root>' && orch daemon start)`. If other accounts could already write state, verify or restore it from trusted data before starting.
```

The commands that talk to the daemon add `A refused project does not prove a daemon is absent.` at the end. For a
directory owned by another user, the message says it `is owned by uid <n>, but this process runs as uid <m>` and
tells you to run as the project owner or select your project. If another user's `.orchestry` cannot be read at all (for
example the 0700 a new project gets), `orch task list` and `orch daemon status` report the project as not initialized
(exit 3), and `orch daemon start` exits 2 saying it is not usable.

## Version skew

A client and a daemon talk only when they are the same orch version **and** the same local protocol version
(`LOCAL_PROTOCOL`).
When they differ, in either direction, the client stops before any request
is decoded, so nothing in your project changes. The message names the daemon's and this orch's versions and
protocols in `<detail>` and changes the hint:

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon stop, then orch daemon start
```

`orch daemon stop` works across the mismatch, with either binary. Whether it also works when the old executable has
been deleted while its daemon runs has not been tested (unproven, waived by the owner's word of 2026-11-14).
Stop the old daemon, then start the new one. orch never restarts the daemon for you.

For developers: two builds with the same orch version and the same protocol version cannot tell each other
apart. Stop the daemon before you switch builds and start the binary you mean; `orch daemon status` shows the
executable path and start time of the one that runs.

## Recovery

This table follows the spec's
[normative recovery table](../specs/2026-11-09-daemon-owned-state.md#normative-recovery-table) and adds one
case, an `orch daemon stop` that timed out. No row lets a client start, restart or repair a daemon; every recovery
is something you do.

| Situation | What you see | What to do |
|---|---|---|
| The daemon crashed or was killed (`kill -9`) | The next command of every client reports no daemon (exit 7); nothing restarts it. The stale socket and `daemon.json` stay until the next start. | Start it again. The new daemon has dispatch off, clears stale claims, records runs whose agent died as cancelled and their agents as idle, and leaves remote work as unknown until the peer syncs. Check tasks and runs before you retry anything or turn dispatch on. Agents that survived the crash get the no-daemon message too: with `ORCH_TASK_ID` or `ORCH_AGENT_ID` set it says not to start the daemon, with neither it prints `Start it with: orch daemon start`. Only a start with one of the two variables set is refused (exit 2), in words that name `--from-agent`; a start with neither is not refused. |
| A foreground daemon's terminal or pane closed | SIGHUP: a clean stop, the same as `orch daemon stop`; its local agents are stopped (unless cleanup fails; see the stop section). | Start it again in a new pane, or use `--detach`. |
| A detached daemon after a reboot (logout is untested) | No daemon answers. | Start it again. orch supplies no launchd job, watchdog or restart. |
| Recovery at start cannot save a task, run or agent change | The start exits 1 and names the write that failed; `orch daemon status` exits 7. | Fix the cause (permissions, disk space), then start again; the state needed to finish the recovery is kept. |
| You upgraded orch and the old daemon still runs | Version skew message, exit 7. | `orch daemon stop` (either binary), then start the new one. Running local agents are stopped and reconciled, unless cleanup fails; this is not a transparent upgrade. |
| The daemon hangs, or its socket was removed | Commands time out or report no daemon; status never claims liveness from files. | Read the PID in `daemon.json` as an untrusted hint. Only if `ps -p <pid> -o command=` shows an orch daemon for this project, send `kill -TERM <pid>`. If you cannot verify the process, do not signal it. If `orch daemon stop` already timed out, SIGTERM does nothing: see the next row. |
| `orch daemon stop` timed out (exit 7) and `orch daemon start` says `already running (PID <pid>)` | The daemon is still finishing an operation it accepted, such as a hung `git worktree add`, and owns the project until it returns. `orch daemon status` reports no daemon (exit 7) and `kill -TERM` does nothing. | Wait: the daemon exits by itself when the call returns. To end it now, check that `ps -p <pid> -o command=` shows an orch daemon, list its child processes with `pgrep -P <pid>`, then `kill -KILL <pid>`. The children are not killed with it: a stuck `git worktree add` kept running under PID 1. Check each child with `ps -p <child-pid> -o command=` and stop it too, or wait until it has exited, before you start the daemon again; the crash row above then applies. |
| orch crashed between writing an entity and its `_index.json` | One writer does not make two file writes one transaction. Start-up rebuilds a missing or corrupt index of tasks, agents, goals, messages or context (runs and teams have no index), but cannot detect a stale index that is still valid. | Offline repair only: stop the daemon, remove the affected `_index.json`, start it. Never remove an index while a daemon runs. |
| Re-running `orch init` | It does not overwrite the project and prints how to start the daemon. It never starts one. | Start the daemon yourself. |
| An agent loses the daemon mid-task | Exit 7 with the agent-context message. The outcome of a change whose reply was lost is unknown. | The agent stops and reports to you. Start the daemon, inspect the task, run and state, then decide on a retry. A restart does not resume the agent's conversation. |
| An MCP client's first call fails | The `initialize` error with the no-daemon message; the `orch mcp` process stays up. | Start the daemon, then reconnect, re-enable or restart the MCP session; not every MCP client retries by itself. |
| A bootstrap script or a cron job | `orch init` never starts a daemon; state commands fail visibly when none runs. | Start the daemon explicitly in the script, then check it (example below). In cron, let the failure exit non-zero with its stderr. |
| Upgrading from orch 0.13.0 | Old `orch` processes, and `orch mcp` servers held open by IDE or Claude sessions, write without the daemon's locks. | Stop every old orch process, including MCP servers; `pgrep -fl orch` must show none before the first start. Then restart those sessions on the new build. See [Upgrading from orch 0.13.0](#upgrading-from-orch-0130). |

A bootstrap script you write starts the daemon explicitly, then checks it (from the spec's recovery table):

```sh
set -eu
orch init
orch daemon start --detach
orch daemon status  # detach returns success only after readiness
orch task list
```

A cron job does not start a daemon; it fails visibly, with its exit code and stderr, when none runs:

```sh
cd /absolute/project || exit 1
orch serve --once || { rc=$?; echo "orch batch failed (exit $rc); owner action required" >&2; exit "$rc"; }
```

## How `serve`, `run` and the dashboard map to the daemon

Dispatch is a mode of the daemon, not a separate process. `orch serve` and `orch run --watch` start a daemon; every
other way of running work asks the running daemon:

| Command | What it does now |
|---|---|
| `orch serve`, `orch run --watch` | Start the daemon in the foreground with dispatch **on**, like `orch daemon start --dispatch on`, with the same refusals. If a daemon already runs, exit 4 with `Use: orch daemon dispatch on`; a daemon that runs with dispatch off is not resumed for you. |
| `orch serve --listen`, `orch run --watch --listen` | The same, with the peer-link listener on. Against a running daemon: exit 4, with the hint `Peer options apply only at start. Use: orch daemon stop, then run this command again` (also for `--peer-name`); a listener is never added to a running daemon. To listen, `orch daemon stop`, then start again with `--listen`. |
| `orch run --once` | One scheduling check in the running daemon, with that check's events; it does not stop the daemon or change dispatch. |
| `orch run <task-id>` | Start that task in the running daemon and follow it until it ends. Disconnecting stops following, not the run. The usual eligibility, capacity and cancel checks apply, except that a `retrying` task starts at once instead of waiting out its retry delay, as in orch 0.13.0. A task that cannot start exits 1 with `task is not eligible for launch`. |
| `orch run --all` | One finite batch of the currently eligible tasks in the running daemon, with the same results as before. |
| `orch serve --once` | Run in the running daemon until every tracked task is final (below). Needs dispatch off. |
| Dashboard start and stop watch | `orch daemon dispatch on` and `off`; never starts or stops a process. |
| `orch tui --observe`, `orch tui --listen` | Removed: the dashboard is always a client; peer listening is a start option of the daemon. |
| `orch doctor` | A client: with no daemon it runs no check, prints a discovery note and the no-daemon message, and exits 7; outside a project it prints `Not initialized` and exits 3. |
| `orch init`, `--help`, `--version` | Need no daemon. `orch init` prints how to start one and never starts it; running it again changes nothing in an existing project. `orch --help` and `orch --version` work in any directory; the help of most other commands (seen on macOS for `orch serve --help`, `orch run --help`, `orch task add --help` and `orch agent add --help`) printed `Not initialized` outside an initialized project, and offline help for every command is unproven (waived by the owner's word of 2026-11-14). |

**Dispatch follows changes.** With dispatch on, a change to the project wakes the daemon's scheduler: a new or
edited task, a run that ends and unblocks another task, a message, from any client or from the daemon's own work.
The daemon checks for work about half a second after the first such change; changes that arrive in the meantime
join that check, so a burst of changes leads to one check, not one per change. A change that arrives while a check
is running leads to at most one more check, about half a second after it ends. The checks the scheduler makes do
not wake it again. Half a second is when the check is due, not a promise of when the agent starts: a long check or
a busy machine can delay it.

The periodic check every `scheduling.poll_interval_ms` (10 s by default) still runs, for work that becomes due
with time, such as a retry whose delay has passed. Change the interval with
`orch config set scheduling.poll_interval_ms <ms>` (values under 100 ms count as 100 ms); the daemon uses the new
value from its next check. A wake never turns dispatch on, never starts a task of a paused goal and never skips a
task's retry delay or the back-off after a failed preparation.

**Options a daemon start refuses.** A foreground `orch serve` or `orch run --watch` *is* the daemon, so the
options orch 0.13.0 used to shape that process's log or interval have nothing left to apply to. The command refuses
them with exit 2 before it opens a socket or a lock, and prints the first that applies; nothing is ignored silently:

| Option | Refused on | Use instead |
|---|---|---|
| `--tick-interval <ms>` | every `orch serve` (with `--once` too) and every `orch run` | `orch config set scheduling.poll_interval_ms <ms>`; the daemon reads it before every check |
| `--log-file <path>` | `orch serve` without `--once` | the foreground daemon's stderr (`RUST_LOG`), `.orchestry/daemon.log` when detached, `orch logs --follow` for events |
| `--log-format` other than `json` | `orch serve` without `--once` (`json`, the default, is accepted) | as for `--log-file` |
| `--verbose` | `orch serve` without `--once`, `orch run --watch` | `RUST_LOG` or `orch logs --follow` |

The messages, after `✕`:

```text
--tick-interval is not accepted: the daemon uses scheduling.poll_interval_ms from config.yml (orch config set scheduling.poll_interval_ms <ms>)
--log-file is not accepted by a daemon start; the foreground daemon logs to stderr (RUST_LOG), a detached one to .orchestry/daemon.log. Follow events with: orch logs --follow
--log-format is not accepted by a daemon start; the foreground daemon logs to stderr (RUST_LOG), a detached one to .orchestry/daemon.log. Follow events with: orch logs --follow
--verbose is not accepted by a daemon start; use RUST_LOG or orch logs --follow
```

`orch serve --once` keeps `--log-format json|text`, `--log-file` and `--verbose`: it is a client, and it renders
the batch's events itself ([Logs of `serve --once`](07-running-and-monitoring.md#logs-of-serve---once)).
`orch run <task-id>`, `--all` and `--once` accept `--verbose` and ignore it.

**`orch serve --once` is a batch.** While global dispatch is on it refuses with
`dispatch is ON; serve --once needs dispatch OFF` (exit 2). With dispatch off it runs one batch at a time (a second
exits 4): the daemon checks for work as often as dispatch would, creates no tasks for autonomous agents, and
follows every task that was waiting or becomes eligible during the batch, including tasks created meanwhile and
the ones they unblock, until none is left that is not `done`, `failed` or `cancelled`. A task in `review` or a
blocked dependency therefore keeps the batch going, as before. The batch never changes the dispatch switch:

- A later `orch daemon dispatch on` or `off`, from any client, ends the batch; `serve --once` reports
  `interrupted` and the running agents carry on. A finishing batch never turns dispatch back off.
- If `serve --once` is disconnected (`Ctrl+C`, a closed pane), the batch ends and its running agents carry on in
  the daemon. In orch 0.13.0 `serve --once` stopped them.

`orch daemon start`, `orch daemon status`, the dashboard header and `orch web` show whether dispatch is on or off,
and a daemon starts with it off unless you start it with `orch serve`, `orch run --watch` or `--dispatch on`, also
after a restart.

## How clients follow changes

The daemon tells its clients when something changes; no client watches the project's files or re-reads the
project on a timer.

- **What a client gets.** A client that wants live updates subscribes to the stores it shows (tasks, goals,
  agents, messages, context, and optionally teams, runs, configuration, daemon state and remotes) and, if it
  wants, to the orchestrator's events. Each change notice names the store, the kind of change (`created`,
  `updated`, `deleted`) and the entity's ID, never its contents. A notice without an ID means "something in this
  store changed": the daemon sends it for its own state and the configuration, and when it knows which store a
  change touched but not which entries. The client then reads what it needs again, through ordinary requests.
- **Who subscribes.** The dashboard, so every `orch web` tab too, since each tab runs a dashboard; and
  `orch mcp`, for the `orch://` resources its client subscribed to
  ([Subscribe to resources](17-mcp-server.md#subscribe-to-resources)). The CLI's one-shot commands do not
  subscribe; `orch logs --follow` streams events as before.
- **When a client falls behind.** Each subscription holds at most 256 waiting notices, and one connection at most
  4 MiB of waiting output. A client that falls further behind gets one "start over" notice instead (resync): it
  then reads every view it shows again. If even that notice does not fit, the daemon closes the connection, and
  the client treats it as a lost connection. The daemon also sends a resync to every subscriber when a change
  failed part-way and it cannot tell which stores were touched. Both are rare; a client never misses a change
  silently. A connection holds at most 8 subscriptions.
- **When the daemon goes away.** The dashboard shows the [no-daemon screen](#when-no-daemon-answers) (`r` to
  retry, `q` to quit). `orch web` answers new pages with HTTP 503, and an open tab shows its dashboard's
  no-daemon screen. `orch mcp` tells its client that every subscribed resource changed, then answers each call with
  the no-daemon error. **Nothing restarts the daemon**, and no client starts one: you do. After you start it again,
  the dashboard (`r`) and `orch mcp` (on a resource read, tool call or new resource subscription) subscribe to the new daemon and read
  everything again; no mutation is replayed. Notifications remain hints and may repeat.

**Limits and a known limitation.** Commit identity metadata holds at most 256 entries / 64 KiB; the
subscription and session limits above bound staged output, including frames handed to the writer. The engine's
per-operation domain-event buffer is not capped: it holds one operation's events until the operation releases
the permit and grows with the number of events that operation emits. An overflow design for in-process
listeners remains deferred.

An idle worker linked to a controller emits no change notices or dispatch wakes for heartbeat frames. An idle
controller with a linked peer still invalidates the Remote store about once per second for peer status; those
commits can wake dispatch.

## Upgrading from orch 0.13.0

This is a behaviour change: **no orch command that reads or changes project state works without a running
daemon**, and nothing starts one for you. The owner decided this
([ADR 0012](../decisions/0012-daemon-owned-state.md), decision 2): you should know when a long-lived process starts
on your machine. The project's file formats do not change. A v0.13.0 binary read a representative project after
this build had written it (one check on macOS; the files differ in the daemon's `.gitignore` entries, runtime files
and a populated optional task `workspace` field, and not every file or surface was compared: see
[Known limitations and unverified behaviour](../../CHANGELOG.md#known-limitations-and-unverified-behaviour)).

1. Stop every orch 0.13.0 process of the project, including `orch mcp` servers held open by IDE, Claude Code or
   Codex sessions, `orch serve`, `orch run --watch` and dashboards. `pgrep -fl orch` must show none.
2. Install the new orch.
3. Start the daemon: `orch daemon start` in a pane you keep open, or `orch daemon start --detach` (add
   `--dispatch on`, or use `orch serve`, to start work at once).
4. Check it: `orch daemon status`.
5. Restart the MCP sessions on the new build. Without a daemon their first call fails with the no-daemon message.
6. To stop it: `orch daemon stop`.

The [README](../../README.md#upgrading-from-v0130) has the short version.
