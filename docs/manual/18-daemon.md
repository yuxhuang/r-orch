# The orch daemon

<!-- Draft skeleton (plan task W0). It describes the daemon being built in
docs/plans/2026-11-09-daemon-owned-state.md; no release has it yet. Every example is the approved wording from the
plan, not captured output: replace each with real output from the frozen commit in task W4, and resolve every
"TBD: after A1" marker against the spec. -->

> **Note:** This chapter describes behaviour that is still being built and is in no release yet. Commands, flags
> and messages may change before it ships; until then, orch 0.13.0 works as the other chapters describe.

The **daemon** is the one orch process that owns a project. It reads and writes everything under `.orchestry/`,
starts and stops the agents, and runs the orchestrator. Every other orch command, the dashboard, `orch web` and
`orch mcp` asks the daemon to do the work over a local socket. Read this chapter to start the daemon, check and
stop it, understand the message you get when it is not running, and upgrade a project from orch 0.13.0.

## In this chapter

- [Why a daemon](#why-a-daemon)
- [Start the daemon](#start-the-daemon)
- [Check and stop the daemon](#check-and-stop-the-daemon)
- [When no daemon is running](#when-no-daemon-is-running)
- [Sockets and `daemon.json`](#sockets-and-daemonjson)
- [Version skew](#version-skew)
- [Upgrading from v0.13.0](#upgrading-from-v0130)

## Why a daemon

Up to orch 0.13.0, every orch process wrote the project's files on its own: the orchestrator, each one-off
command, the dashboard, `orch web` and every `orch mcp` server. Two of them changing the same thing at the same
time could lose an update. A field could go back to an older value, a renewed context key could disappear, or a
task could exist as a file but be missing from `orch task list` (see
[Concurrent writers](17-mcp-server.md#concurrent-writers) in the MCP chapter). With a daemon there is only one
writer, so these races cannot happen between processes. Because dispatch and storage are now in one process,
a task that a goal pause cancels can no longer start after the pause, and cancelling a task from any client stops
its running agent. Your files keep their format: tasks, agents and goals are still plain YAML in `.orchestry/`.

## Start the daemon

You start the daemon yourself; nothing starts it for you. `orch daemon start` runs it in the foreground of your
terminal until you stop it, and `orch daemon start --detach` runs it in the background. By default a new daemon
does **not** dispatch: it serves every command, but it starts no task until you turn dispatch on, so you can queue
work first. To start it dispatching, add `--dispatch on` <!-- TBD: after A1 (exact flag form) -->.
`orch serve` and `orch run --watch` also start the daemon in the foreground, with dispatch on, so they work as
they did before: they are the orchestrator, and `Ctrl+C` or `SIGTERM` stops it. Only one daemon runs per project.
If one is already running, a second start exits with code 4:

```text
  ✕ orch daemon already running (PID 4242)
    Use: orch daemon status
```

From `orch serve` or `orch run --watch`, the hint is `Use: orch daemon dispatch on` instead, which turns dispatch
on in the daemon that is already running. `orch init` creates a project and prints how to start the daemon; it
does not start one. <!-- TBD: after A1 (what `orch serve --once`, `orch run --all`, `orch tui` and `--listen` do now; where `--detach` writes its log) -->

## Check and stop the daemon

`orch daemon status` shows the daemon's process id, its socket path, its orch and protocol versions, whether
dispatch is on, the running tasks and the connected clients. `orch daemon dispatch on` and
`orch daemon dispatch off` start and stop dispatching without stopping the daemon. `orch daemon stop` asks the
daemon to shut down, waits until it has exited and reports the result. It shuts down as it does on `SIGTERM`,
`SIGINT` or `SIGHUP`: it stops its runs, saves its state and releases the lock. To run one check of the queue, or
one task now, use the one-tick command or `orch run <task-id>`; both need a running daemon and show the output
as it arrives. <!-- TBD: after A1 (status output, what stop does with running agents and how long it waits, the one-tick command's name) -->

## When no daemon is running

Every orch command except `orch init` and the commands that start a daemon needs a running daemon. When none
answers, the command does not read or change your files and does not start a daemon. It prints one message,
which names the project and what it found, and exits with code **7**:

```text
  ✕ No orch daemon is running for /path/to/project (no socket)
    Start it with: orch daemon start
```

The part in brackets says why: `no socket`, a stale socket left by a daemon that exited (such as
`stale socket from PID 1234, exited`), `connection refused`, `handshake failed`, or a version mismatch (see
[Version skew](#version-skew)). The dashboard shows the same message full-screen; press `r` to try again after
you start the daemon, or `q` to quit. `orch web` prints it and exits 7 at start; if the daemon stops while
`orch web` is serving, the browser gets HTTP 503 with the same text and a banner. `orch mcp` stays running and
returns the message as the error of `initialize` and of every tool call, so your MCP client shows it, and the
first call of a new session fails until you start the daemon. Start the daemon with `orch daemon start` (or
`orch serve`) and run the command again. <!-- TBD: after A1 (whether `--help`, `--version` and `orch doctor` need a daemon; exit codes table location in chapters 13 and 14) -->

## Sockets and `daemon.json`

Clients reach the daemon through a Unix socket, `.orchestry/orch.sock`. Only you can use it: its folder is
readable by you only (mode 0700), the socket has mode 0600, and the daemon refuses a connection from another
user. On macOS a socket path can be at most 104 bytes, so when `.orchestry/orch.sock` would be longer than 100
bytes the daemon puts the socket in your temporary folder instead, at
`$TMPDIR/orch-<16 hex characters>/orch.sock`, named after a hash of the project's path. Either way it records the
socket path, its process id, its versions and its start time in `.orchestry/daemon.json` (mode 0600), which is
how clients find it. Set `ORCH_SOCKET` to use another path. A client that finds a stale socket only reports it;
`orch daemon start` replaces it. Do not delete the socket or `daemon.json` while a daemon runs.
<!-- TBD: after A1 (`ORCH_SOCKET` precedence, `.gitignore` entries, whether Linux uses the same 100-byte rule) -->

## Version skew

The client and the daemon must be the same orch build: the same orch version and the same protocol version.
When they differ, in either direction, the client stops before it sends any request, so nothing in your project
changes, and it prints both versions and exits 7:

```text
  ✕ No orch daemon is running for /path/to/project (version skew: daemon orch 0.14.0 / protocol 1, this orch 0.14.1 / protocol 2)
    Start it with: orch daemon stop, then orch daemon start
```

This happens after you upgrade orch while a daemon from the old version is still running. Stop the old daemon
and start it again with the new binary. orch never restarts the daemon for you.

## Upgrading from v0.13.0

This is a change in how you run orch: after the upgrade, no `orch` command works until you start a daemon for the
project. Before you upgrade, stop `orch serve`, `orch run --watch` and the dashboard on each project. After you
install the new version, start the daemon in each project with `orch daemon start` (dispatch off) or `orch serve`
(dispatch on), check it with `orch daemon status`, and stop it with `orch daemon stop`. MCP clients and the agents
orch runs need the same daemon: start it before you open a client session. No file format changes, so you can go
back to orch 0.13.0 after you stop the daemon. <!-- TBD: after A1 (what v0.13.0 makes of a leftover `daemon.json` and socket; the release number) -->
