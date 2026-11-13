# r-orch operator runbook

Recovery and gotcha reference for running r-orch. Entries describe current behaviour that is known, not yet
changed, and has a documented remedy.

If you are looking for *what r-orch is* and how to build it, start at the [README](../README.md).

---

## Quick index

| Symptom | Go to |
|---|---|
| `No orch daemon is running for <project root>` (exit 7), from any command, the dashboard, `orch web` or an MCP client | [No orch daemon is running](#no-orch-daemon-is-running-for-project-root) |
| How do I start, see or stop the daemon? | [Start, check and stop the daemon](#start-check-and-stop-the-daemon) |
| `orch daemon start` exits 2 naming the socket, or `connection refused` in the no-daemon detail | [A stale or stuck socket](#a-stale-or-stuck-socket) |
| The daemon crashed or was killed | [After a daemon crash](#after-a-daemon-crash-or-kill--9) |
| `daemon is orch … protocol …, this client is orch …`, or upgrading orch | [Version skew and upgrading](#version-skew-and-upgrading-orch) |
| Commands time out, or `orch daemon stop` says the PID is still running | [A hung daemon](#a-hung-daemon) |
| `orch` commands, the dashboard or MCP tools are slow while many tasks fail to start | [Commands are slow while many tasks fail to start](#orch-commands-are-slow-while-many-tasks-fail-to-start) |
| The dashboard or an MCP client re-reads everything at once, `resources/subscribe` is refused, or a new task does not start within a second | [Live updates and dispatch wake](#live-updates-and-dispatch-wake) |
| `Orchestrator already running (PID: undefined)` and nothing is running | [Corrupt lock file](#corrupt-lock-file) |
| `serve --once` never exits | [`serve --once` hangs on review](#serve---once-hangs-on-a-task-in-review) |
| `task show <id>` works but `task list` omits the task | [Rebuilding an index](#rebuilding-an-index-after-a-lost-update) (legacy procedure) |
| Every command fails after a crash or power loss | [state.json recovery](#statejson-parse-failure) |
| `.state.json.*.tmp` files piling up | [Temp-file litter](#state-file-temp-litter) |
| `serve:started` logged but the daemon is not up | [serve:started ordering](#servestarted-precedes-the-batch-check) |
| Need to see what r-orch is doing internally | [Debugging](#debugging) |
| Agent processes still running after a cancel | [Stray agent subprocesses](#stray-agent-subprocesses-after-a-cancel) |
| An agent has run for hours and nothing stopped it | [`timeout_ms` is not a timeout](#timeout_ms-is-not-a-timeout) |
| Running r-orch from inside a Claude Code session | [Nested agent sessions](#nested-agent-sessions) |
| Worried about API keys in agent config | [Secrets in agent config](#secrets-in-agent-config) |
| Cannot select text in the dashboard any more | [Mouse capture holds the pointer](#mouse-capture-holds-the-pointer) |
| The terminal prints `[<0;44;8M` when you click | [A leaked mouse mode](#a-leaked-mouse-or-paste-mode-after-an-abnormal-exit) |
| A task handed to a peer shows `dispatched`, `queued` or `RUN?` | [Remote task phases](#remote-task-phases) |
| A peer is `offline` and you cannot tell why | [Reading a peer's link state](#reading-a-peers-link-state) |
| A mirror on a worker sits `todo` for ever | [Mirrors left behind on a worker](#mirrors-left-behind-on-a-worker) |
| Changing the peer protocol | [Peer protocol compatibility](#peer-protocol-compatibility) |
| `orch agent shop` prints `Cancelled.` or a table instead of a picker | [The agent shop picker](#the-agent-shop-picker) |
| `orch --json doctor` or `--yaml doctor` output will not parse | [Structured output for `doctor`, `logs --follow` and `serve`](#structured-output-for-doctor-logs---follow-and-serve) |
| `update`/`patch` fails with `mutation lock timed out`, `read mutation input` or a `<stdin>:` error | [File mutations: update and patch failures](#file-mutations-update-and-patch-failures) |
| `org deploy` failed half way, or a template will not load or change | [Organization templates: failures and recovery](#organization-templates-failures-and-recovery) |
| A task stays `todo` with `configuration preflight` or the eco/custom-template message, or a run fails before start with `effort preparation failed` | [Prompt mode and effort failures](#prompt-mode-and-effort-failures) |
| A goal was resumed but its checkpoint or continuation stays `cancelled` and the tasks after it never start | [A resumed goal leaves cancelled tasks cancelled](#a-resumed-goal-leaves-cancelled-tasks-cancelled) |
| Data lost or overwritten while orch 0.13.0 `orch mcp` clients and other writers shared a project | [Recovering after concurrent MCP writers](#recovering-after-concurrent-mcp-writers) (legacy) |
| `orch status`, `orch task list` or the MCP `status` tool fails with `parse <project>/.orchestry/config.yml`, or a new `orch mcp` session fails `initialize` with it | [Restoring a broken configuration](#restoring-a-broken-configuration) |
| No trace of what an `orch mcp` server did, or its client discards stderr | [Capturing `orch mcp` diagnostics](#capturing-orch-mcp-diagnostics) |
| An `orch mcp` server does not answer, every call is `Not executed: timed out …`, or a server outlives its client | [A stuck or leftover `orch mcp` process](#a-stuck-or-leftover-orch-mcp-process) |

---

## The orch daemon

Every orch command that reads or changes a project, the dashboard, `orch web` and `orch mcp` talk to the project's
daemon. **Nothing starts the daemon but you.** The operator's reference is [manual chapter 18](manual/18-daemon.md);
the normative contract is the spec's
[recovery table](specs/2026-11-09-daemon-owned-state.md#normative-recovery-table). These entries are the
procedures.

### Start, check and stop the daemon

| To | Run | Expect |
|---|---|---|
| Start it in this terminal, dispatch off | `orch daemon start` | `listening <socket>`, then the dispatch state; it runs until stopped |
| Start it in the background | `orch daemon start --detach` | Returns 0 only once the daemon is ready; output goes to `.orchestry/daemon.log` |
| Start it with dispatch on | `orch serve`, `orch run --watch` or `orch daemon start --dispatch on` | As above, dispatch on |
| See it | `orch daemon status` | `orch daemon PID <pid> · dispatch ON\|OFF`, then its socket, instance, running tasks and ownership; exit 7 if none answers |
| Turn dispatch on or off | `orch daemon dispatch on` / `off` | Off does not stop running agents |
| Stop it | `orch daemon stop` | Returns once the daemon has confirmed its cleanup and released the project, by exiting or by releasing its ownership lock (at most 30 s); agents get the usual 10 s grace and their runs end `cancelled` |

`Ctrl+C`, SIGTERM and SIGHUP (closing the terminal or tmux pane of a foreground daemon) stop it cleanly, like
`orch daemon stop`. A detached daemon survives closing its terminal or pane; it does not survive a reboot, and orch
supplies no launchd job, watchdog or restart.

To see which orch processes run, `pgrep -fl orch` lists the daemon (`orch daemon start`, `orch serve` or
`orch run --watch`) and its clients (`orch mcp`, `orch tui`, `orch web`) separately.

### `No orch daemon is running for <project root>`

**Symptom.** A command, the dashboard, `orch web` or an MCP client reports, with exit 7 (`exit_code` 7 in MCP):

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon start
```

**Cause.** No daemon answers for this project. The detail says why: `no daemon.json`, `no socket`,
`connection refused` (a socket left by a daemon that exited or was killed), `timed out`, `permission denied`,
`handshake failed`, `connection lost` or `daemon is stopping`. Clients never start, restart or repair a daemon, and
never delete its socket or `daemon.json`.

**Fix.**

1. Check that the project is the one you expect: the message names its root. If not, check `ORCH_PROJECT_ROOT`,
   `ORCH_SOCKET` and your working directory ([Which project](manual/18-daemon.md#which-project)).
2. Start the daemon: `orch daemon start` (or `--detach`).
3. Retry the command. In an MCP client, reconnect or restart the MCP session: not every client re-initialises by
   itself.

**In an agent's environment** (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set) the message reads
`The orch daemon for <root> is not running. Do not start it; report this to the owner and stop.` The agent is
meant to stop and report; you start the daemon, then inspect the task, its run and what it changed before you
retry it. A restart does not resume the agent's conversation. `orch daemon start`, `orch serve` and
`orch run --watch` refuse to start from such an environment (exit 2); each accepts `--from-agent` to override
that on purpose.

**Not this entry:** `daemon_busy` (the daemon runs but found no room for the client within about 13 s, that is 10 s
of waiting plus one last connection attempt, for a session
or for the connection itself, so the count can read `1/64 sessions`: close clients and retry), `wrong_project` (the socket
belongs to another project: fix `ORCH_SOCKET`/`ORCH_PROJECT_ROOT`), `Cannot confirm the orch request outcome`
(the reply was lost: read the state before you retry; see
[the manual](manual/18-daemon.md#when-no-daemon-answers)).

### A stale or stuck socket

**Symptom.** `connection refused` in the no-daemon detail, or `orch daemon start` exits 2 naming the socket path.

**Cause and fix.** Only an explicit start replaces a socket, and only one it can prove dead:

| `orch daemon start` says | Meaning | Fix |
|---|---|---|
| nothing; it starts | Connecting to the old socket was refused, so nobody listens: the start replaced it | None |
| `<path> already has a live listener; another daemon or program uses it. Set ORCH_SOCKET to a different path` | Something answers on that path | `orch daemon status`. If a daemon of this project runs, use it. If another program owns the path, give this project its own `ORCH_SOCKET` |
| `<path> could not be probed (<reason>), so it may still have a live listener; it was left alone. Check its owner and mode, or set ORCH_SOCKET to a different path` | The connect failed with something other than connection refused, for example permission denied | Check the socket's owner and mode (`ls -l <path>`). Remove it yourself only after `orch daemon status` and `pgrep -fl orch` show no daemon for this project |
| `<path> is a symbolic link` (or `a regular file`, `not a socket`, `a socket owned by another user`)`; it is never removed automatically. Remove it yourself or set ORCH_SOCKET to a different path` | The path is not this user's socket | Inspect it and remove it yourself if it is yours and unused |

Never delete `.orchestry/run/orch.sock` or `.orchestry/daemon.json` while a daemon runs: its clients find it
through them, and it cannot republish them.

**Give each project its own socket.** A project is identified by the device and inode of its `.orchestry/`
directory. On a file system that reuses inodes, two projects pointed at one `ORCH_SOCKET` path can, in a narrow
window, have one daemon remove the other's socket. Set `ORCH_SOCKET` per project, or leave it unset.

### After a daemon crash or `kill -9`

**Symptom.** Every client reports no daemon (`connection refused` or `no socket`); nothing restarts it.

**Fix.**

1. Confirm no daemon of this project runs: `orch daemon status` exits 7, and `pgrep -fl orch` shows no
   `orch daemon start`, `orch serve` or `orch run --watch` for it.
2. Start it: `orch daemon start`. It starts with dispatch **off**, whatever it ran with before.
3. At start it reconciles what the dead daemon left: stale claims are cleared; a run whose local agent died is
   recorded `cancelled` and its agent `idle`; a run that was still preparing is cancelled; remote runs stay
   unknown until their peer syncs. It also rebuilds any missing or corrupt `_index.json`.
4. Inspect before you go on: `orch task list`, `orch logs --task <id>`, `orch daemon status`. Retry what needs it
   (`orch task retry <id>`), then `orch daemon dispatch on`.

If the start fails with exit 1 naming a write, recovery could not save a change (permissions, disk full). Fix the
cause and start again; the state it needs is kept.

### Version skew and upgrading orch

**Symptom.** Every command fails with exit 7 and:

```text
  ✕ No orch daemon is running for <project root> (daemon is orch <a> protocol <n>, this client is orch <b> protocol <m>)
    Start it with: orch daemon stop, then orch daemon start
```

**Cause.** The running daemon is a different orch or protocol version from the `orch` you ran. They refuse to talk,
before any request is read, so nothing changed.

**Fix (also the upgrade procedure).**

1. `orch daemon stop`. It works with either binary, even after the old one was deleted. It stops the running agents
   (10 s grace) and records their runs as `cancelled`: an upgrade is not transparent to running work, so do it when
   nothing important runs.
2. Install the new orch.
3. `orch daemon start` (or `--detach`), then `orch daemon status`.
4. Restart MCP sessions so that they run the new `orch mcp`.

Developers: two builds with the same version and protocol cannot tell each other apart. Stop the daemon before
you switch builds; `orch daemon status` shows the executable and start time of the one that runs.

**Upgrading from orch 0.13.0** adds one step before the first start: stop **every** orch 0.13.0 process of the
project, including `orch mcp` servers held by IDE, Claude Code or Codex sessions. They write `.orchestry/` without
the daemon's locks. `pgrep -fl orch` must show none. See the [README](../README.md#upgrading-from-v0130).

### A hung daemon

orch never kills a daemon for you, and the PID in `daemon.json` is only a hint. Which fix applies depends on
whether you already asked it to stop.

**Symptom A.** Commands time out (`timed out`), and you have not run `orch daemon stop`.

**Fix.** Signal it yourself, only after you verify it:

1. Read the candidate PID: the `pid` field of `.orchestry/daemon.json`.
2. `ps -p <pid> -o command=` must show an orch daemon (`orch daemon start`, `orch serve` or `orch run --watch`) for
   this project. If it does not, or you cannot tell, **do not signal it**.
3. `kill -TERM <pid>`: the same clean stop as `orch daemon stop`.
4. When it has exited, `orch daemon start`.

**Symptom B.** `orch daemon stop` reports `daemon accepted stop but PID <pid> is still running after 30 s`
(exit 7), and `orch daemon start` then exits 4 with `orch daemon already running (PID <pid>)`.

**Cause.** The daemon is finishing an operation it already accepted, such as a hung `git worktree add`, and owns
the project until that call returns. Its shutdown is already under way, so **`kill -TERM` does nothing**.

**Fix.** Wait: the daemon exits by itself when the call returns. To end it now, follow
[manual 18, Recovery, row "`orch daemon stop` timed out"](manual/18-daemon.md#recovery):

1. `ps -p <pid> -o command=` must show an orch daemon for this project. If it does not, **do not signal it**.
2. List its child processes: `pgrep -P <pid>`.
3. `kill -KILL <pid>`. Its children are not killed with it.
4. Check each child with `ps -p <child-pid> -o command=` and stop it too, or wait until it has exited.
5. `orch daemon start`. The new daemon reconciles the killed one's work as after a crash.

### A process is waiting to start

On macOS the daemon admits complete process spawns one at a time. A diagnostic
waiting or stuck in spawn does not make a queued agent launch hold the project
operation boundary. Reads, queued task cancellation and goal pause can still
complete. A task cancelled or paused while queued is checked again before launch.

The private daemon log reports `process spawn pending` with `stage=Waiting` or
`stage=Entered`, a generated `spawn_id`, fixed `site`, and monotonic `elapsed_ms`.
The first warning is after 1 s; persistent warnings are at least 5 s apart per
record. These are warning thresholds, not execution timeouts. Records are bounded
(128 individual entries plus fixed overflow counts); the log includes no command,
arguments, environment, prompt or process output. Records clear when that stage
ends, including cancellation and spawn errors.

With neither `RUST_LOG` nor `ORCHESTRY_DEBUG` set, explicit daemon starts
(`daemon start`, detached starts, `serve`, and `run --watch`) enable these
warnings alone. Foreground warnings go to stderr; detached warnings go to the
existing private capped `.orchestry/daemon.log`. Other commands keep their quiet
default. An explicit logging filter takes precedence, including `RUST_LOG=off`;
SDK debug/trace payloads remain excluded even with a specific SDK directive.

`Entered` means the actual synchronous spawn has begun. The OS call cannot be
interrupted safely by dropping its caller. Stop first terminates registered agent
groups; an unresolved entered diagnostic makes stop report incomplete and keeps
project ownership until the late child has been killed/reaped and accepted work
has drained. Do not start a second daemon while ownership is retained. An actual
agent already inside spawn can still hold the final project boundary; this is
separate from waiting behind an unrelated diagnostic. Use [A hung daemon](#a-hung-daemon)
for owner-directed recovery.

### `orch` commands are slow while many tasks fail to start

**Symptom.** Dispatch is on and many tasks fail before their agent starts, for example because their workspace
cannot be prepared. Meanwhile `orch task list`, `orch status`, the dashboard and MCP tools take noticeably longer
than usual. In the [measured burst](manual/18-daemon.md#why-a-daemon), every sampled read succeeded; other requests
can be refused at admission.

**Cause.** Reads such as `orch task list` and `orch status` share a FIFO operation boundary with failure-record
writes: each read waits behind operations already queued to acquire that boundary. This orders boundary
acquisition, not every request's network arrival; some external operations do work outside it. These store
writes await file sync and attempt a best-effort parent-directory sync before releasing the boundary. Read times
depending on writes are a stated non-guarantee: see
[What the daemon does not promise](manual/18-daemon.md#why-a-daemon).

**What to check.**

1. `orch daemon status` answers and shows `dispatch ON`. If it does not answer, this is not the entry: see
   [No orch daemon is running](#no-orch-daemon-is-running-for-project-root).
2. `orch task list --status retrying` lists tasks waiting for another attempt and exhausted tasks awaiting
   final-failure recording; `orch task show <id>` shows the attempts already used, for example
   `retrying · attempt 1/3`.
3. Why they fail: `orch --json status` shows each waiting retry's error in `state.retry_queue[].error`, for
   example `"error": "workspace_mode \"worktree\" requires a git repository"`. Once terminal `failed` is recorded,
   the failed task and status retry queue no longer retain its workspace failure reason (pending fix).
   An exhausted `retrying · attempt 3/3` can still carry the reason
   while final-failure recording is pending, so read it while the task is still `retrying`.

**Fix.** For a finite burst with working storage, preparations stop at each task's attempt limit (`max_attempts`,
3 unless set; [How orch retries by itself](manual/04-tasks.md#how-orch-retries-by-itself)). After final-failure
recording succeeds, those tasks are `failed` and the burst's writes cease. Queueing alone needs no state
recovery. This does not promise convergence or normal read times with failed or hung storage. If a cascade-list
read fails during final recording, a task can remain exhausted `retrying` with its error and back-off; subsequent
back-offs retry the recording without another workspace preparation. Resolve storage errors first. Two
consecutive preparation-release errors can retain an agent reservation; cancelling the task does not free it.
After storage recovery, explicitly stop and start the daemon to clear the reservation; this recovery is
code-audited, not an end-to-end fault-recovery test result.

To stop future starts and retries sooner, use `orch daemon dispatch off`; it does not cancel a preparation
already executing or free a retained reservation. Fix the cause the errors name, then `orch task retry <id>`
for each failed task and `orch daemon dispatch on`.

If the error is `daemon operation queue is full or admission timed out` (`Overloaded`, `NotExecuted`), the
request was not run and can be retried. An ordinary request still waiting to acquire the boundary after 10 s
is refused this way; this is not a completion deadline for work already started. If the error instead says
`request outcome unknown`, inspect state before retrying: the request may have applied. Exit 7 or elapsed time
alone does not establish that nothing ran.

---

### Live updates and dispatch wake

The daemon pushes change notices to the dashboard (every `orch web` tab runs one) and to `orch mcp` for subscribed
resources, and wakes its own scheduler about half a second after a change
([How clients follow changes](manual/18-daemon.md#how-clients-follow-changes)). No client polls.

| Symptom | Cause | Fix |
|---|---|---|
| The dashboard re-reads every view at once, or an MCP client gets `notifications/resources/updated` for every subscribed resource | The client fell behind (more than 256 waiting notices on a subscription, or 4 MiB on its connection), a change failed part-way with the affected stores unknown, or the daemon stopped or restarted | Nothing, if reads work: the client is catching up. If they fail with the no-daemon message, start the daemon yourself; nothing restarts it. Frequent resyncs mean a client reads too slowly: close clients you do not need. |
| `resources/subscribe` is refused | That `orch mcp` process follows 256 distinct URIs, or the URI exceeds 527 bytes or an identifier/alias exceeds 256 bytes | Unsubscribe to free a slot; for `-32602`, correct the URI. Oversized strings are refused before retention. |
| The prompt-mode header temporarily shows the old value during an F3 change | Its own asynchronous write is still pending | Wait for its answer. A pushed invalidation received meanwhile is refreshed after the usual debounce on success or refusal, without another key press or event. |
| With dispatch on, a new or unblocked task does not start within about a second | The task is not eligible: dispatch is off, its goal is paused, its agent is busy, disabled or at capacity, it waits out a retry delay, or a failed preparation is backing off. A long scheduling check can also delay the next one. | `orch daemon status` (dispatch ON?), `orch task show <id>`, `orch logs --task <id>`. The periodic check still runs every `scheduling.poll_interval_ms`; a wake never overrides these checks. |

The engine's per-operation domain-event buffer is not capped: it holds the events of one operation until
the operation releases the permit and grows with the number of events the operation emits. Commit metadata
(256 entries / 64 KiB), subscription counts and staged output have separate limits; see
[How clients follow changes](manual/18-daemon.md#how-clients-follow-changes). An overflow design for in-process
listeners remains deferred.

## Corrupt lock file

**Symptom.** Any dispatching command (`run`, `run --all`, `serve`) fails with exit code 4:

```
  ✕ Orchestrator already running (PID: undefined)
    Use: orch status
```

…and no orchestrator is actually running. The literal word `undefined` is the tell.

**Cause.** `.orchestry/orchestry.lock` holds something that is not a decimal PID — empty, truncated, or
garbage. A lock whose PID parses is handled correctly (a dead PID is replaced, a live one is reported with its
number); a lock whose PID does **not** parse is refused and **never cleaned up**. The 60-second staleness
escape hatch does not apply, because reaching it requires a parseable PID.

**The suggested command will not help you, and neither will the dashboard.** This is the part worth knowing:

- the hint says `Use: orch status`, but `orch status` is deliberately mtime-blind and **reports the project
  unlocked** on a corrupt lock — so it tells you nothing is wrong;
- `orch status` also prints `watching` whenever `state.json` has a `pid` field, with **no liveness check**.
  After a `SIGKILL` the field is still set, because the graceful shutdown never ran, so it says `watching`
  indefinitely;
- in the **TUI**, a refused lock becomes **silent observer mode** — a badge, no message. You get a dashboard
  that appears to be watching someone else's orchestrator, forever.

The remedy lives here rather than in the code.

**Recovery.**

1. **Confirm no orchestrator is running.** Do not skip this; deleting a live lock lets two orchestrators
   dispatch the same tasks.
   ```bash
   pgrep -af 'orch (run|serve)' ; pgrep -af orchestry
   cat .orchestry/orchestry.lock        # inspect what is actually in there
   ```
2. Delete the lock:
   ```bash
   rm .orchestry/orchestry.lock
   ```
3. Re-run your command. If it fails again immediately with a *numeric* PID, that PID is real — find it before
   deleting anything.

**Prevention.** Nothing in r-orch writes a malformed lock; this comes from an interrupted write, a full disk,
or external editing.

---

## `serve --once` hangs on a task in `review`

**Symptom.** `orch serve --once` runs, logs activity, and then never returns. No timeout. Ctrl-C works.

**Cause.** `--once` polls until **every** task is terminal, and the terminal statuses are
`done`, `failed`, `cancelled` — **`review` is not among them**. A task that lands in `review` and has nothing
to approve it blocks the loop forever.

**This is reachable out of the box.** The `agt_creator` agent that `orch init` creates ships with
`approval_policy: suggest`, so a task assigned to the default agent will sit in `review` and hang `--once`.

**Three escapes**, in order of how often they are the right answer:

1. **Set the agent's approval policy to `auto`** so completed work self-approves:
   ```bash
   orch agent edit <agent-id>          # opens $EDITOR; set approval_policy: auto
   ```
2. **Give the task review criteria**, so auto-review decides:
   ```bash
   orch task add "..." --review-criteria test_pass,typecheck
   ```
   All criteria passing transitions `review → done`.
3. **Approve out of band**, from another shell while `--once` is still polling:
   ```bash
   orch task list --status review
   orch task approve <task-id>
   ```

**In CI**, prefer (1) or (2). An unattended `--once` that can reach `review` will hang the job rather than
fail it, which is the worse failure mode.

---

## Rebuilding an index after a lost update

> **Legacy procedure.** This entry describes orch 0.13.0, where several processes wrote the stores. With the
> daemon as the only writer, two writers can no longer leave an index behind its files; a crash of the daemon
> between the two writes still can. The daemon rebuilds a **missing or corrupt** `_index.json` at start, but not
> one that is stale and still valid. The procedure with a daemon: `orch daemon stop`, remove the affected
> `_index.json`, `orch daemon start`; never remove an index while the daemon runs. A listing that an earlier
> version left inconsistent on a disk that ignores case (a deleted context key or agent that is still listed, or
> one key listed under two spellings) is not repaired by upgrading and is not detected by `orch doctor`; remove the
> affected `_index.json` as above to rebuild it from the files (`orch agent remove` cannot clear a listed agent
> whose file is gone: it reports `Agent not found`). The 0.13.0 text below is removed in a later release.

**Symptom.** `orch task show <id>` returns the task, but `orch task list` and `orch status` do not show it,
and the orchestrator never dispatches it. The entity's `.yml` file exists on disk.

**Cause.** Each store keeps an `_index.json` cache alongside the individual entity files. Current orch
serialises every index read-modify-write across processes with `<store>/.index.lock` (waiting at most 5 s), so
two current writers creating different entities keep both entries. The entity file and its index entry are still
written as two separate steps, not one transaction, and a writer that does not take the lock (an older orch
binary, a hand edit) can still overwrite the index. A crash between the two writes, or such a writer, leaves the
index behind the files. See [ARCHITECTURE.md §2.1](ARCHITECTURE.md#21-a2-in-detail--the-concurrency-story-is-weaker-than-single-writer).

This is a documented hazard with a one-command recovery.

**Recovery.** Delete the index; the next read rebuilds it from the entity files, which are the source of
truth:

```bash
rm .orchestry/tasks/_index.json      # or agents/, goals/, context/, messages/
orch task list                       # rebuilds
```

Safe to run at any time. There is **no command that does this for you**, and nothing reports the divergence —
the only symptom is the disagreement above.

**Prevention.** Avoid running mutating commands (`task add`, `agent add`, `goal add`), or `orch mcp` clients that
change the project, concurrently with a `serve` or `run --watch` during bulk changes, and do not mix orch
versions on one project. `teams/` and `runs/` have no index and are immune.

**The inverse case, worth knowing while you are here.** A listing reads only the index — it never opens the
entity files — so the failure can run the other way too: a **corrupt or truncated** `tasks/<id>.yml` is
**invisible** to `task list`, which keeps serving the stale-but-valid index entry. It surfaces on
`task show <id>`, or once the index is rebuilt. So a healthy-looking `task list` is **not** evidence that
your entity files are intact. If you suspect corruption, force the read:

```bash
rm .orchestry/tasks/_index.json && orch task list
```

A rebuild reads every file and **fails loudly, naming the offending one** — which is what you want when
diagnosing, and is why the rebuild is the diagnostic as well as the repair.

---

## `state.json` parse failure

**Symptom.** Every command that reads orchestrator state fails, immediately and identically, after a crash or
power loss.

**Cause.** `.orchestry/state.json` is truncated or corrupt. Writes are atomic against *process* crashes
(temp file + rename), so a half-written file means the machine lost power or the filesystem did not order the
data before the rename.

**Recovery.** The file is **runtime state, not your data** — tasks, agents and goals live in their own files
and are untouched. Delete it:

```bash
rm .orchestry/state.json
```

The next run recreates it from defaults. What you lose: cumulative statistics (`total_runs`, token totals,
runtime), the retry queue, and the record of what was running. Tasks left mid-flight will be reconciled on the
next `start_watch` — `in_progress` tasks with no live process are cancelled rather than retried, so agents do
not redo committed work.

Check also for a stale lock afterwards ([above](#corrupt-lock-file)).

---

## State-file temp litter

**Symptom.** Files named `.state.json.<8 hex>.tmp` (or `.<entity>.yml.<hex>.tmp`) accumulate in
`.orchestry/`.

**Cause.** `atomic_write` writes a temp file and renames it. A process killed **between** the write and the
rename leaves the temp file behind; cleanup only runs on the write's own error paths.

**These files are inert.** Directory scans filter by `.yml`/`.json`, so index rebuilds ignore them, and
`.orchestry/.gitignore` keeps them out of git. They cost disk and nothing else.

**Recovery.** Delete them whenever you like, including while the orchestrator runs:

```bash
find .orchestry -name '*.tmp' -delete
```

Nothing sweeps them automatically and nothing reports them, so add this to periodic maintenance on a
long-lived deployment.

---

## `serve:started` precedes the batch check

A foreground `orch serve` is the daemon and writes no `serve:started` record; treat it as up once it prints
`listening <socket>`, or once `orch daemon status` exits 0.

`orch serve --once` logs `serve:started` after it has reached the daemon but before the daemon accepts the batch.
When dispatch is on (exit 2) or another batch runs (exit 4) you get one `serve:started`, possibly `serve:diagnostic`
records, no `serve:finished`, and the refusal on stderr. Treat a batch as running only after a later record, and
check the exit code.

---

## Debugging

Internal diagnostics go to **stderr** and never to stdout, so they cannot corrupt `--json` output or the log
stream.

| Variable | Effect |
|---|---|
| `ORCHESTRY_DEBUG=1` | Installs the tracing subscriber and dumps the full error chain on failure |
| `RUST_LOG=…` | Standard `tracing-subscriber` filter, e.g. `RUST_LOG=orch_engine=debug,orch_infra=trace` |
| `ORCHESTRY_GLOBAL_CONFIG_PATH=<path>` | Redirects `~/.orchestry/global.yml`. Useful for isolating a test run from your real settings |

```bash
ORCHESTRY_DEBUG=1 orch run <task-id> 2>debug.log
```

For `orch mcp`, stderr belongs to the MCP client, which may discard it; see
[Capturing `orch mcp` diagnostics](#capturing-orch-mcp-diagnostics). On that path the MCP SDK's `debug` and
`trace` events, which carry tool arguments and results, are never recorded, whatever `RUST_LOG` says.

Without `ORCHESTRY_DEBUG`, tracing output is suppressed entirely — including the one signal that
distinguishes "another process holds the lock" from "I could not read the lock file", since both print
`PID: undefined`. **If you are diagnosing a lock problem, set it.**

Other useful reads:

```bash
orch logs --task <task-id>            # run metadata + rendered events
orch logs --task <task-id> --json     # the Run records, including the full prompt
cat .orchestry/runs/<run-id>.jsonl    # raw append-only event log
orch doctor                           # adapter availability — note it checks nothing operational
```

`orch doctor` reports whether the AI CLIs are installed and whether you are in a git repository. It does
**not** check the lock, the state file, index consistency, or disk space. With a daemon it exits 0 even when
checks fail. Without one it runs no check and exits 7 (3 outside a project).

---

## Stray agent subprocesses after a cancel

**Symptom.** You cancelled a task, force-stopped an agent, or shut the orchestrator down, and a process the
agent started is still running — often reparented to init (`ppid 1`).

**Cause.** Stopping an agent sends `SIGTERM` to the whole **process group**, then waits for the **direct
child** to exit and returns. The final `SIGKILL` sweep of the group never happens. A process forked in the
window between the group signal and the child's death **inherits the group without having been signalled**,
survives, and is reparented. Load-dependent, so it shows up under contention rather than reliably.

**Spotting it.** Agents are spawned into their own process group whose **group id is the child's pid**, and
that pid is in the run record:

```bash
# the pid for a task's most recent run
orch logs --task <task-id> --json | grep -o '"pid":[0-9]*'
# or straight from disk
grep -h '"pid"' .orchestry/runs/*.json

# anything still alive in that group
ps -eo pid,ppid,pgid,stat,etime,args | awk -v g=<pid> 'NR==1 || $3==g'
```

A row with `ppid 1` and an `etime` older than your cancel is a survivor.

**Cleanup.** Signal the group explicitly, which is the step that was skipped:

```bash
kill -TERM -<pid>      # note the leading minus: the GROUP, not the process
sleep 2
kill -KILL -<pid>      # if anything remains
```

Check before you do: confirm the group id is the one from the run record, and that no *current* run is using
it. A recycled pid would make you kill an unrelated group.

**Prevention.** None available at the operator level — it is inherent to the cancel path. If stray processes
are a recurring problem in your deployment (long-running builds, test runners that fork), prefer
`workspace_mode: worktree` so survivors at least operate on a throwaway checkout rather than your working
tree.

---

## `timeout_ms` is not a timeout

**It is accepted, stored, displayed — and never enforced.** `orch agent add --timeout <ms>` and
`defaults.agent.timeout_ms` (default `3600000`) write a field that **no code reads**. If you set it expecting a run to be killed after an hour, nothing will kill it.

**What actually bounds a run** is `stall_timeout_ms` (default `600000`), and only partly:

| Situation | Bounded? |
|---|---|
| Agent emits no events for `stall_timeout_ms`, under `run --watch` or `serve` | **Yes** — the run is killed and the task fails or retries |
| Agent keeps emitting events but never finishes | **No** — every event refreshes the stall clock, so it runs forever |
| `orch run <task-id>` or `orch run --all` | **No stall detection at all** — these wait for completion, so a hung agent wedges the command indefinitely |

The last row is the one that catches people: stall detection lives in the watch loop's reconcile phase, so
the one-shot commands have none of it.

**What to do.**

- **Don't rely on `timeout_ms`.** Leave it at the default; changing it has no effect.
- **Bound it outside r-orch.** For one-shot runs, wrap the command:
  ```bash
  timeout 3600 orch run <task-id>
  ```
  In CI, set a job-level timeout — that is your real backstop.
- **Prefer `serve` or `run --watch` for unattended work**, so `stall_timeout_ms` is at least active, and tune
  it per agent (`stall_timeout_ms` in the agent's config) rather than reaching for `timeout_ms`.
- **To stop a wedged run by hand:** `orch task cancel <task-id>`, then check for survivors as
  [above](#stray-agent-subprocesses-after-a-cancel).

The inert field and the one-shot gap are not fixed: the field name and shape are part of the `--json`
contract.

---

## Nested agent sessions

**Hazard, with no guard in the product.** If you run `orch` from inside an agent session — a Claude Code
session, say — and dispatch a task to an agent whose adapter is `claude`, r-orch spawns a **nested** `claude`
process inside the session you are already in. Nested sessions compete for the same credentials and rate
limits, produce interleaved output, and are hard to attribute.

**r-orch itself does not prevent it.** It sets exactly three environment variables on every child —
`ORCH_AGENT_ID`, `ORCH_AGENT_NAME`, `ORCH_TASK_ID` — and inspects **no** session marker. There is no
`CLAUDECODE` check in r-orch, and no guard is planned. Do not expect one.

**In practice the `claude` CLI usually stops it for you — by failing.** Recent Claude Code versions set
`CLAUDECODE=1` in the session environment, and the `claude` CLI refuses to start a *nested* session when it
sees it. Adapters inherit the parent environment and only overlay their own keys — verified: nothing in
r-orch calls `env_clear`, so every variable in your shell reaches the child — so `CLAUDECODE` arrives with
it. The visible result is that **every `claude`-adapter agent exits immediately**, the task fails, and
`orch logs` shows a near-instant non-zero exit rather than any useful agent output. If you see that pattern
inside an agent session, this is why; it is the outer CLI declining, not an r-orch fault.

**What to do:**

- **Preferred — run from a plain shell.** `orch serve` and `orch run` belong outside any agent session.
- **If you must drive r-orch from inside one**, use `shell`-adapter agents: they spawn `bash -lc <command>`
  and start no AI CLI, so the guard is irrelevant.
- **To force it anyway**, strip the variable for r-orch and everything it spawns:
  ```bash
  env -u CLAUDECODE orch serve
  ```
  Understand what this does: it removes the guard rather than satisfying it, so you get genuinely nested
  sessions competing for the same credentials and rate limits, with interleaved output that is hard to
  attribute. Use it knowingly, not as the default fix.

---

## Mouse capture holds the pointer

**Symptom.** `orch tui` is running and you cannot select text with the mouse: dragging moves the
dashboard's selection instead of highlighting, and your terminal's own copy-on-drag does nothing.

**Why.** The dashboard asks the terminal for mouse reports (DEC 1000 + 1002 + 1006), so button events are
routed to the application instead of to the terminal's selection handling. That is the trade mouse
support makes.

**Fix**, in increasing order of permanence:

```bash
# 1. In the dashboard: release the pointer for this session.
#    F2, or type the command:
/mouse off          # /mouse on to re-enable, /mouse status to check

# 2. Keep it: hold Shift while dragging (most terminals bypass reporting).
#    NOT under tmux — see below.

# 3. Start with it off, every time:
ORCH_TUI_MOUSE=0 orch
```

While capture is off the footer shows `⌨ F2 mouse`, so there is never a doubt about which mode you
are in.

**Under tmux.** Shift+drag is **not** the bypass here: tmux holds the *outer* terminal's mouse mode,
so Shift+drag selects raw outer-terminal cells, spanning pane borders and neighbouring panes rather
than the pane's text. Use `F2` or `/mouse off`, which also gives tmux's own drag-to-copy back.
`prefix [` copy mode works either way. And note that tmux must have `set -g mouse on` for clicks to
reach the dashboard in the first place — with it off, mouse support silently does nothing, which is
not a fault.

---

## A leaked mouse or paste mode after an abnormal exit

**Symptom.** `orch tui` is gone, but clicking in that terminal prints gibberish like `[<0;44;8M`, or
the shell receives it as input. Or a paste into that terminal arrives wrapped in `^[[200~` … `^[[201~`
(often shown as `[200~`), with the markers landing in your shell command instead of the text.

**Why.** Mouse reporting and bracketed paste are terminal modes the application turns on and is
responsible for turning off. Every normal exit does: `q`, a signal, a panic, and a terminal that hangs
up all run the same restore, and the disables are written *first*, before leaving
the alternate screen. `SIGKILL` and an abort cannot be covered — nothing runs on the way out.

**Fix.**

```bash
printf '\033[?1006l\033[?1002l\033[?1000l\033[?2004l'   # mouse SGR, mouse, bracketed paste
reset                                         # or just reset the terminal wholesale
```

---

## Secrets in agent config

**Agent configuration can hold credentials. r-orch protects those files better than you might expect — and not well enough to put a key in one.**

An agent's `env:` map is written verbatim into `.orchestry/agents/<agent-id>.yml` and passed to every child
process it spawns. So this puts a live key on disk in plain text:

```yaml
config:
  env:
    ANTHROPIC_API_KEY: sk-ant-...      # ← don't
```

**r-orch protects the file locally.** `.orchestry/agents/<id>.yml` and `.orchestry/config.yml` are written
**`0600`**, owner-only — and the mode is set when the temp file is created, so the file is never even briefly
world-readable. A file written at the process umask would typically be `0644`; r-orch is deliberately stricter.

**And git ignores the whole directory by default.** `orch init` appends `.orchestry` to your project's
**root** `.gitignore` (creating it if absent), so no entity file is committed unless you go out of your way.
Verify on your own project:

```bash
git check-ignore -v .orchestry/agents/*.yml     # should name the root .gitignore rule
git status --short | grep orchestry             # should be empty
```

**So the exposure is narrower than it looks — but it is not zero.** What remains:

1. **Someone force-adds or un-ignores the directory.** `git add -f .orchestry` works, and teams sometimes
   un-ignore it deliberately to share agent definitions. Once a key is in a repository, **`0600` protects
   nothing** — git preserves no mode beyond the executable bit, and anyone who can read the repo can read
   the key.
2. **Only those two files are owner-only.** `tasks/`, `goals/`, `teams/`, `messages/` and `context/` keep
   umask modes, so in a project whose `.orchestry` is not itself owner-only (every project created before
   `orch init` made it 0700, and any whose directory was opened up) a credential pasted into a task description
   or a context value **is** world-readable to other local users. For what a 0700 `.orchestry` does and does not
   protect, see [Permissions of `.orchestry/`](manual/18-daemon.md#permissions-of-orchestry).
3. **The child process still receives it.** `env:` is passed to every agent the entry spawns, so anything
   that agent logs, echoes or sends onward can leak it.
4. **Backups and sync tools do not read `.gitignore`.** Dropbox, Time Machine, `rsync` without
   `--exclude` and most backup agents copy `.orchestry/` verbatim.

**Do this instead.** Leave the key out of the agent and let it inherit from the environment — children
receive the parent environment, overlaid with `env:` and the three `ORCH_*` variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
orch serve
```

Use `env:` only for non-secret, per-agent settings.

**If a key has already reached a repository**, treat it as disclosed: rotate it first, then purge it from
history. Rotation is the step that matters — a purge alone leaves the key live in every existing clone.

**If you deliberately un-ignore `.orchestry/` to share agent definitions across a team** — a reasonable thing
to want, since those files *are* your agent and task definitions — then re-ignore the parts that can hold
credentials, and keep versioning the rest:

```gitignore
!.orchestry/
.orchestry/agents/
.orchestry/config.yml
```

The guidance is the same either way: **version the definitions if you want to, keep secrets in the
environment.**

---

## Remote task phases

**A task dispatched to a peer is `in_progress` here from the moment the controller reserves it** — before a
byte reaches the worker. So task views do not show that status for remote work; they show the phase the
worker last reported, in `orch task list`, `orch task show` and the dashboard's status chip:

| Shown | Means | What to do |
|---|---|---|
| `dispatched → 2com` | The request is queued locally. The worker has not acknowledged it. | Nothing, unless it stays. After **5 minutes of link time** (offline time does not count) the task fails with `remote 2com never acknowledged submit` and a cancel goes to the worker. |
| `queued @2com` | The worker holds a mirror that has not started. | If the WAITING column says `awaiting approval: orch task approve <id> on 2com`, that command has to be run **on the worker** — its own owner decides whether a peer may use a `suggest`/`manual` agent. |
| `RUN @2com` | The worker says its agent is executing. | Nothing. This is never shown without a running signal from the worker. |
| `RUN? (2com offline)` | The link dropped after the submit was written, so nobody here knows what the worker has. | Nothing. The next session's sync resolves it to queued, running or finished — or to a failure if the worker has lost the record. |

**A restart or a shutdown does not change any of this.** A graceful exit
(`SIGTERM`, the dashboard's `/pause`, a service-manager restart) **detaches** a
remote run: no cancel is sent, the worker keeps executing, and the entry stays
in `state.running` as `RUN?` for the next start to pick back up. Only a peer
that has been **removed** from the registry turns its runs into
`remote <alias> removed; outcome unknown`.

Two messages come out of the deadlines:

- **`tsk_x has been queued on 2com for 1h`** (`orchestrator:error`, once an hour, with the approve command as
  its context). A queued mirror is usually waiting for the worker owner's approval, which this side cannot
  grant, so it is **never** failed automatically however long it waits.
- **`remote lost execution record (2com: worker storage error: corrupt record)`**. The worker answered a sync
  with `unknown`: it has no execution record for the key. The parenthesis is the worker's own reason, and it
  is the part that says where to look — on the worker, in that example, at the mirror's task file. The task
  fails without a retry, and a cancel goes out in case the mirror becomes readable again.

### What the deadlines do not cover

- **`RUN?` has no deadline.** An execution nobody can account for — a dropped
  link, a detached shutdown, a rehydrated restart — waits for a sync, for as
  long as that takes. Nothing times it out, because the only thing that can
  resolve it is the worker. If the peer never returns, `orch remote remove
  <alias>` is the deliberate way to end it (that fails its runs as
  `removed; outcome unknown`).
- **The clocks live in memory.** A restart starts every phase clock again, so a
  dispatched run gets a fresh five minutes and a queued one a fresh hour.
- **A clock counts reconciles, not wall time.** Each pass credits at most two
  poll intervals, so a laptop that slept for eight hours does not fail a
  dispatched run on the first tick after it wakes.
- **A deadline is final for the controller, not for the worker.** When a
  dispatched run is failed, a Cancel goes out, but if the worker's acceptance
  was merely slow, a success landing after that is dropped: the task is
  terminal and the key is no longer tracked, and the worker's mirror is
  cancelled on the next sync. Check the worker before re-running such a task,
  or the work may happen twice.

---

## Reading a peer's link state

`orch remote status` and `orch remote list` no longer print a bare `offline`, which covered three very
different situations:

```
  2com: offline (Peer closed connection; retry in 12s, attempt 5)
  2com: offline (Worker busy: controlled by mac; retry in 8s)
  2com: offline (connecting)
  2com: online v0.5.0
```

- **`attempt N`** is the number of consecutive dials since the last session that proved the peer healthy. A
  climbing count with a short `retry in` is the signature of a worker that accepts a connection and then
  closes it — look in the **worker's** `serve.log` and at `orch remote status` there, which reports
  `Listen: … (last session ended: <error>)`.
- **`retry in Ns`** counts down to the next dial. The backoff is 1, 2, 4, 8, capped at 15 seconds, each with
  ±25 % jitter — so the longest real wait is about 18.75 s — and it only resets after a session has cleared
  its sync fence and stayed up for 30 seconds. The disconnect event in the activity feed is debounced to one
  per distinct error per 30 s, so a peer that drops every session reports once, not once per redial; the
  `attempt` counter is what shows the rate.
- **`(version unknown)`** beside an online peer means that peer predates the version field in the handshake.
  Nothing is wrong with the link; it is an older build.
- **`Write peer status for 2com: Lock remote registry …: timed out after 5s`** means something held
  `.orchestry/remotes.lock` for over five seconds, usually an `orch remote add` or `remove` that is stopped
  or hung. A link records its status only under that lock, and only while its exact registry entry still
  exists, so that a removal cannot be undone by a late write. Until it can, the remote reports `offline`.
  Let the command finish or stop it; the next status write recovers. A registry that cannot be read
  (`Duplicate remote alias`, `Invalid remote alias`) is reported the same way: fix `.orchestry/remotes.yml`.

---

## Mirrors left behind on a worker

A **mirror** is the task a worker creates for a controller's request (labelled `remote-ctl:<controller id>`).
A mirror runs only while the controller that submitted it is connected, so one left behind by a controller
that never comes back would sit `todo` in a local agent's queue indefinitely.

The worker sweeps for those hourly, and once when `orch serve` starts listening. A `todo` mirror whose
controller has not been admitted for **24 hours** is cancelled with:

```
orphaned remote mirror: controller <instance id> absent since <timestamp>
```

A mirror can also be cancelled for two other reasons, and both say so on the
task:

- **`orphaned remote mirror: controller <id> no longer tracks execution <key>`**
  — the controller synced and did not list that key, so it has already
  accounted for the execution (finished, failed, or given up as unknown). This
  is the common one after a controller failed a run whose mirror it could not
  read.
- **`cancelled by controller <name>`** — an `orch task cancel` on the
  controller, or its reconcile, reached this worker.

Three things are deliberately *not* swept:

1. **`in_progress` mirrors.** They own a live local process, and the quarantine path owns that pid.
2. **The connected controller's mirrors.** A controller that is here now is not absent.
3. **A controller this worker has never recorded.** Its 24 hours start at the first sweep that sees it, which
   is also what spares mirrors that predate the feature.

Last-seen timestamps live in `.orchestry/remotes/_controllers.json` (owner-only). Deleting it restarts every
controller's clock; it is safe to delete.

To cancel one by hand, on the worker: `orch task cancel <mirror task id>`.

---

## Peer protocol compatibility

**Within one `PROTOCOL` number, every peer must tolerate every frame variant and every omitted field.** Both
sides of a link can be different builds — that is the normal case for a LAN of machines upgraded one at a
time — so a change that keeps the current `PROTOCOL` may only **add fields**, each with
`#[serde(default)]` (to read an old peer's frame) and `skip_serializing_if` (so an old peer reads ours
byte-identically to what it used to see).

A **new frame variant, a removed field, or a changed meaning for an existing one requires a `PROTOCOL`
bump**, because an old peer treats a frame it cannot parse as fatal and closes the session. A mismatched
`PROTOCOL` is refused at the handshake with `Peer protocol mismatch`, which is the honest failure: both
owners then know to upgrade.

**Downgrading is not symmetric.** A 0.5.0 build understands every frame this release sends, but it does
not have the behaviour behind them: it **cancels** remote runs on a restart or a `SIGTERM` instead of
detaching them, it never sweeps orphaned mirrors on a worker, and it shows a dispatched or queued remote
task as `RUN`. So a downgrade with remote runs in flight loses them, and the work may already have happened
on the worker. Drain remote work first, or check each worker's task list afterwards.

The rule is enforced by tests in `crates/orch-peer/src/protocol.rs`, in both directions — current frames
parsed by a copy of the previous release's shapes, and previous-release JSON parsed by the current frames.
If you add a field, extend those tests; if you cannot keep them passing, you need the bump.

## Copying text from the dashboard

1. Focus a text section or open its viewer. Ctrl+Y copies the whole field/representation; viewer `y` copies its selection. Open F6 and choose
   Copy selection or an explicit destination with arrows/Tab and Enter. F6, End, Enter explicitly chooses **Copy on this host**. The mouse
   Copy chip/carousel offers the same actions. F1 shows the full matching request status and route guidance.
2. **Copy requested from terminal** acknowledges emission only. The terminal must support and permit OSC 52.
   Under tmux, `set-clipboard on` permits application requests subject to terminal policy; `external` ignores
   application requests. Orch never changes this setting. Choose Copy on this host when the host clipboard is
   the intended destination. With SSH, that host may be the server rather than your client.
3. Under GNU screen the unwrapped OSC path is unsupported. Choose the host route or retry outside screen in a
   supporting terminal. Auto under screen still requests the terminal; it does not write both clipboards.
4. In `orch web`, request copy, then click **Copy to clipboard**. A secure context and browser permission are
   needed. Denial or an insecure page exposes a selectable text box. Select that text and use the browser's
   ordinary copy command. The client never reads the clipboard.
5. **Busy** means the bounded host worker/tickets are occupied. Let results drain; a timed-out OS call cannot
   be killed by the UI and no replacement worker is spawned. Native failure does not trigger a terminal write.
   Requests over 64 KiB on the terminal route or 1 MiB on the host route are refused whole; select less text.

The viewer includes the pressed glyph in a drag. Textarea selection uses insertion-position endpoints, so the
same reverse drag can copy one fewer glyph there. Both preserve original source bytes. F2 releases capture for
terminal-native selection. Hardware Option/Alt delivery depends on the terminal profile; use F6/arrows/Enter
when modified copy keys are unavailable. Native Terminal.app/iTerm2 profiles and a real clipboard smoke have
not been verified for this change; fake-sink tests do not prove OS delivery.

## Editor exits and session drafts

A changed editor offers Continue, Keep draft and Discard before navigation drops its edits. Keeping a draft
uses RAM only, keyed to the edited form/entity. Reopening offers recovery; full/oversized draft storage refuses
the save visibly. Quitting with drafts asks for confirmation and discards them on exit. A signal exits without
an interactive prompt. Drafts do not survive crashes or restart, and wall-clock backsteps are not a durability
mechanism. On a primary submit error, correct the retained form and retry. A partial post-create failure names
the created entity; repair that entity rather than creating a duplicate.

## The agent shop picker

**Symptom.** `orch agent shop` prints `  Cancelled.` at once, or prints a table instead of the interactive picker.

**Cause.** The picker draws only when stdin **and** stdout are terminals. With stdout redirected (a pipe, a file,
a CI log) the command prints the same table as `--list`; with a terminal on stdout but stdin not a terminal
(`echo | orch agent shop`) it cancels immediately. Both are exit 0 and neither creates an agent. The command
needs an initialised project; elsewhere it prints `Not initialized` / `Run: orch init`. `--json` and `--quiet`
are ignored here.

**Fix.** Run it from an interactive terminal inside a project, or use `orch agent shop --list` and add the
agent you want with `orch agent add`.

**Display.** The picker fits every row to the terminal width and counts its heading, blank lines and footer
against the terminal height, so it should never wrap or scroll the heading away. On a terminal narrower than
the full help it chooses the widest heading that fits: `AGENT SHOP  ↑↓ move · enter select · q cancel`,
then `AGENT SHOP  ↑↓ Enter q`. At 30 and 40 columns the compact heading keeps all three keys visible;
below its display width it is cut with an ellipsis. The footer `n-m of total` appears only when the list is
taller than the terminal. The template slice reads the current size before each frame, retaining the event-loop
correction for a resize received together with a keypress; heading state-machine tests alone do not prove that case.

## Structured output for `doctor`, `logs --follow` and `serve`

**Symptom.** `orch --json doctor` (or `--yaml doctor`) piped into a parser fails on the first line;
`orch --yaml logs --follow` or `orch --yaml serve --once` prints text or JSON Lines, not YAML.

**Cause.**
- `doctor` prints its human header (`  orch doctor · checking adapters and dependencies`) to **stdout** before the
  structured document, whichever of `--json` or `--yaml` is given. The header is older than `--yaml` (master
  prints it under `--json` too), and `--yaml` does not change it. Without a daemon, `doctor` ignores both flags
  and prints a plain-text discovery note; the exit code (7, or 3 outside a project) is the signal.
- `logs --follow` prints a text banner and then live events; `serve --once` writes JSON Lines
  (`--log-format json`, the default) or text. `--yaml` changes neither, and it never changes the on-disk `.jsonl`
  logs. A foreground `serve` is the daemon: it prints `listening <socket>` and `dispatch on`, no event lines, and
  its operator diagnostics go to stderr regardless of `RUST_LOG` (to `.orchestry/daemon.log` when detached).
  Arbitrary error details are omitted there; use `orch logs --follow` for the live error text and context.
- A failing command prints its error as text on stderr under every output flag; stdout is empty.

**Fix.** For `doctor`, drop the leading header lines before parsing: `orch --json doctor | sed '1,2d'`. For
streams, parse the JSON Lines `serve --once` writes, or poll `orch --yaml logs --task <id>` for run metadata. To tell an
error from output, check the exit code; never parse stderr as YAML or JSON.

## File mutations: `update` and `patch` failures

Applies to `orch agent|task|config update|patch -f FILE` ([guide](file-mutations.md)). Every failure leaves the
definition unchanged, because parsing, merging and validation finish before anything is written.

| Symptom | Cause | Fix |
|---|---|---|
| `<source>: mutation lock timed out after 5 s; retry` (exit 1) | Another `update`/`patch` (or a hung one) holds `.orchestry/.definition.lock`. The lock is released when its process exits, however it exits. | Retry. If it persists, find the process (`lsof .orchestry/.definition.lock`) and stop it. Do not delete the file. |
| `read mutation input` (exit 1) | The `-f` file cannot be read: missing, unreadable or a permission problem. The message does not name the path or the OS reason. | Check the path and permissions yourself (`ls -l FILE`). |
| `<source>: <field>: read-only field` / `unknown field` / `required string` (exit 2) | The document names a field that cannot be written, does not exist or is missing. | Fix the document. `orch --yaml agent show <id>` shows the shape; read-only fields can be echoed back in `update` only when unchanged. |
| `Agent not found: <name>` (exit 1) | `update`/`patch` take the id (`agt_…`), not the name. | `orch agent list` for the id. |
| `cannot mutate a running or remote agent definition` (or task) | The target is running, `in_progress`, remote (`@` in the id) or mirrors a remote task. | Stop or cancel the run first, then retry; remote entities are changed on their own host. |
| `labels: remote-ctl: and exec: prefixes are reserved for worker mirrors` | A label uses a reserved prefix. | Use another label. |
| You are unsure whether a failed or interrupted command saved | The write is a temporary file renamed over the target; a crash after the rename but before the output has still saved. | `orch agent show <id>`, `orch task show <id>` or `orch config get <key>` before retrying. |

Mutations cannot interlock with `edit`, `agent add`, a running orchestrator or the dashboard: those keep
last-writer-wins behaviour. For bulk changes, stop the runner and use one writer. To roll back, run `update` with
the definition you saved earlier (`orch --yaml agent show <id> > before.yaml`).

## Prompt mode and effort failures

Applies to the eco prompt mode and effort validation ([manual](manual/16-eco-mode-and-effort.md)). These
failures take one of three paths:

- **Configuration errors** (`configuration preflight` and the eco/custom-template message) stop the dispatch
  before any run exists. No run is recorded, the task stays `todo` with its attempts unchanged, and the agent
  stays idle. `orch run <id>` prints the error and exits without running the task. Foreground `orch serve`
  writes only `⚠ orchestrator:error (details omitted from daemon stderr; use orch logs --follow)` to stderr.
  In another terminal, start `orch logs --follow` to see the task context and full error text from subsequent
  scheduling checks. The underlying live `orchestrator:error` event carries `context=dispatch task <id>` and
  `fatal=false`; the daemon's stderr omits those fields. Dispatch tries again at each periodic or change-triggered scheduling
  check (the periodic interval is `scheduling.poll_interval_ms`, 10 s by default) until the file is fixed.
  These configuration failures do not consume an attempt or enter the workspace-preparation retry back-off. `orch serve --once` does not finish
  while this repeats. There is nothing to retry by hand: once the file is fixed, the next tick dispatches the
  task.
- **`effort preparation failed`** fails one run before the tool starts, through the normal failed-run path (the
  agent goes back to idle, the task retries with its usual backoff).
- **A refused write** leaves the files unchanged.

| Symptom | Cause | Fix |
|---|---|---|
| `orch logs --follow` shows `configuration preflight: cannot read defaults.agent.prompt_mode: …` on scheduling checks; foreground `orch serve` prints the redacted diagnostic above; the task stays `todo` | `.orchestry/config.yml` became unreadable or invalid while `serve` was running. orch reads only the prompt mode again before each dispatch; other settings come from start-up. Commands that load the config, such as `orch run` and `orch task show`, refuse to start with the parse error itself. | Repair `.orchestry/config.yml`. The next tick reads it again and dispatches the task. |
| `eco prompt mode is incompatible with custom templates (…)` from `orch run` (exit code 2), or in `orch logs --follow` on scheduling checks; foreground `orch serve` prints the redacted diagnostic above; the task stays `todo` | A hand-edited file combines eco with a custom prompt template. | `orch agent update <id> --prompt-mode normal` for each listed agent, or `orch config set defaults.agent.prompt_mode normal`. A running `serve` picks either up on the next tick. Removing the template instead needs a `serve` restart, because `serve` reads the `prompt` settings only at start-up. |
| Run fails: `effort preparation failed: … the installed opencode did not prove provider/model#variant support …` | OpenCode agent with an effort newer than 0.9.4's, and the `opencode` on `PATH` is not version 2 (or the check timed out after 2 s or produced more than 64 KiB). | Install OpenCode 2, or `orch agent update <id> --effort default`. A stored `low`/`medium`/`high` never fails this way: it is dropped with a `legacy-ignored` warning. |
| `legacy-ignored`, `capability-unknown` and other effort warnings at `orch run` start, or `serve:diagnostic` records in `orch serve --once` output | Stored efforts that this adapter or model cannot take, or that orch cannot check. Not an error. | Review with `orch agent list`; clear with `--effort default` or set a model. |
| `⚠ effort preparation: …` on foreground `orch serve` or `orch run --watch` stderr; `orch logs --follow` shows the full warning and `effort preparation for run … (agent …)` context | Each effort warning for that run, once: the same statuses as the start-up warnings, after the OpenCode version check where there is one. An agent with no warning logs none. Not an error; the run goes ahead. | Treat it as in the row above. |
| v0.9.4 fails every agent command with `parse …/agents/agt_….yml` | A newer effort value in that file. | Clear it with the newer binary, or restore the file from a backup. Do not hand-map `max`/`ultra` to `high`. |

The OpenCode check runs `opencode --version` and `opencode run --help` in the agent's workspace with the agent's
environment; on a timeout or too much output orch kills the process group. Its diagnostics name a fixed reason,
never the tool's output. A binary replaced between the check and the run is not detected; pin the installed
version if that matters.

To turn eco off everywhere: `orch config set defaults.agent.prompt_mode normal`, then
`orch agent update <id> --prompt-mode inherit` for agents with their own eco setting. It applies from the next
run; nothing needs restarting.

## Organization templates: failures and recovery

Applies to `orch org deploy`, `orch agent shop --template…` and `orch org template …`
([guide](org-templates.md)). Template commands change one file at a time and validate the whole template first, so a
refused command leaves the template unchanged; `deploy` is the only command that can leave work behind.

| Symptom | Cause | Fix |
|---|---|---|
| `Template deployment failed at <stage>: … Surviving entities: {"agentIds":[…],"teamId":…,"goalId":…,"unverifiedIds":[…]}` (exit 1) | A step failed after earlier ones succeeded. Nothing is rolled back. | Fix the cause (a full disk, a bad `.orchestry/` file), then remove the survivors: `orch agent remove <id>` for each agent, `orch team disband <team>`, `orch goal delete <goal>`. `unverifiedIds` could not be checked: look for their files. |
| After cleaning up, the same deploy fails at `team` with `Team "<name>" already exists` and leaves new agents | A disbanded team keeps its name, and deploy checks agent names but not team names before creating anything. | Remove the new agents again, then deploy a copy under another name (`orch org template init <new> --from user:<name>`), or delete the leftover `.orchestry/teams/<team-id>.yml` by hand (not a supported interface). |
| `Agent "<name>" already exists; no agents created` (exit 2) | A member name is taken. Nothing was created. | Remove or rename the existing agent, or edit the member's name in a copy of the template. |
| `Builtins are read-only; run orch org template init … --from builtin:…` | You tried to change `a-team` or a legacy preset. | Copy it with `init --from`, then change the copy. |
| `<file>: <reason>` from `list`, `show`, `deploy` or the shop (exit 2) | A file in the template fails validation. `list` shows the reason for each user template. | Fix the named file. `org template update` with a complete definition repairs one broken `org.yml` or member; `patch` and `edit` need a file that parses. |
| `name is immutable` / `schema_version is immutable` | `org.yml` has a present `name` or `schema_version` (even `null`, or an unsupported version) and the update changes it. | Edit the file by hand. An *absent* key can be repaired with `update`. |
| `…/agents/<file>: exceeds 1 MiB` on every command | A file larger than 1 MiB blocks the whole template, `update` included. | Shrink or delete it by hand. |
| `Timed out after 5 s locking …/.template.lock` (exit 2) | Another template command holds the lock. The lock is released when its process exits. | Retry. Do not delete the file. |
| `expected directory without symlinks` | The selected template root, with or without a trailing `/` or `/.`, is a symlink. | Point `--template-dir` at the real directory. |
| The shop refused a member and the screen shows nothing | The refusal (`Agent "<name>" already exists; no agents created`) goes to stderr and exit status 2; the picker may clear the screen. | Check `echo $?` and `orch agent list`. |
| A deployed agent does not follow the template after you edited it | Deployed agents are snapshots; nothing synchronises them. | Remove the agent and deploy or shop again. |

A change made while another tool writes the same template, or a hand edit during a command, is not covered by the
lock: the last writer wins. If a command was interrupted, run `orch org template show` before repeating it, because
a crash after the file was renamed has still saved the change.

## A resumed goal leaves cancelled tasks cancelled

**Symptom.** You paused a goal (`orch goal status <id> paused`, dashboard `P`, or the MCP `goal_status` tool) and
resumed it, and the work does not pick up again: a checkpoint or continuation is `cancelled`, and the tasks that
depend on it sit in `todo` and never start. A task can also end `cancelled` although it was already running when
the pause happened.

**Cause.** Pausing cancels the assignee's `todo`/`retrying` tasks that are labelled `autonomous` and linked to that
goal. Resuming does not restore them, and a dependent stays blocked until each task it depends on is `done`. The
pause is not an atomic barrier either: it lists the tasks and then cancels them, so a listed task that an
orchestrator starts in between is cancelled anyway, and a live orchestrator then stops its run. See
[what a pause cancels](manual/17-mcp-server.md#choose-a-mode) and
[Pause, resume and finish a goal](manual/05-teams-and-goals.md#pause-resume-and-finish-a-goal).

**Recovery.**

1. Find the cancelled task. `orch goal show <goal-id>` lists the goal's tasks with their status:

   ```text
     Tasks (2)
     ──────────────────────────────────────────
     STATUS       ID           TITLE               AGENT
     ○ todo       tsk_G5ggNHE  next step           agt_8EplcH4
     ○ cancelled  tsk_IA7oEPK  [ateam] checkpoint  agt_8EplcH4
   ```

2. Confirm what waits on it: `orch --json task show <dependent-id>` shows the dependency in `depends_on`
   (`orch task show` without `--json` does not list dependencies).
3. Decide whether the cancelled task is still wanted. Retrying runs it again, so do not retry work that is no
   longer needed or was already done another way.
4. Retry that same task, which keeps its id and dependents and resets it to `todo` with a fresh attempt budget:

   ```console
   $ orch task retry tsk_IA7oEPK
     ✓ Reset tsk_IA7oEPK to todo
   ```

   From an MCP client, call `task_retry` with `{"id": "tsk_IA7oEPK"}`; it works in the default mode.
5. Wait for it to reach `done` (a running orchestrator such as `orch serve`, `orch run --watch` or the dashboard
   dispatches it). Only then can its dependents start.

Do not recreate the task graph, and do not wait for the resumed goal to seed a replacement. Resuming re-enables
the assignee's `autonomous` flag, but a fresh `[auto]` task is seeded only for an agent with no unfinished task
assigned, so a blocked dependent assigned to the same agent prevents it; and seeding never brings back a
cancelled task.

**Prevention.** None makes a pause atomic. After every resume, check the goal's tasks (step 1) before you expect
work to continue.

## Recovering after concurrent MCP writers

> **Legacy procedure, for orch 0.13.0 only.** With the daemon every client, `orch mcp` included, goes through one
> writer, so this hazard no longer arises between orch processes of this release
> ([One writer: the daemon](manual/17-mcp-server.md#one-writer-the-daemon)). Use this entry only to repair a
> project that orch 0.13.0 processes shared. The entry is removed in a later release.

Applies to projects that `orch mcp` clients share with other writers: `orch run --watch`, `orch serve`, the
dashboard, other `orch mcp` processes and one-off `orch` commands of orch 0.13.0 (the 0.13.0 manual's "Concurrent
writers" section; this release replaces it with [One writer: the daemon](manual/17-mcp-server.md#one-writer-the-daemon)).

**Symptom.** A field you set through a tool, the CLI or the orchestrator is back to an older value (a task's
status or assignee, an agent setting); a context key that was just set or renewed is gone; a task exists as a file
but is missing from `task list`; or, after a directory under `.orchestry/` was removed or replaced, calls keep
failing with an I/O error.

**Cause.** The server is not transactional, and its one-call-at-a-time queue covers only its own calls. It shares
the hazards of any two orch processes on one project:

| Boundary | Existing protection at source base | Remaining failure |
|---|---|---|
| IndexManager | Cross-process `.index.lock`, bounded 5-second wait, locked read-modify-publish and stable ordering | Entity+index are not one atomic transaction; old writers may ignore the protocol. Modern distinct-id creates must retain both index entries. |
| Task/other whole-record updates | Single-file atomic writes; some authored-definition paths have `.definition.lock` | Concurrent owner status advancement and task assignment can overwrite one another's whole records. No compare-and-swap promise. |
| Context expiry | list/get can delete expired entity files and list publishes a computed keep-list | Another writer's newly indexed key may disappear from that list; a renewal of an expired key may be deleted. Read-only sessions also participate. |
| Messages | Store transaction holds its mutation lock across entity/index operations | Do not generalize weaker task/context guarantees to messages or imply a multi-service transaction. |
| Process directory cache | Fresh Engines refresh config/service defaults, but `ensure_dir` retains a process-global set | In orch 0.13.0, removing a cached directory can make later operations fail until process restart. The daemon of this release recreates the directory at its next write into it and needs no restart. |

These interleavings are traced from the code; none is a measured failure rate. `--read-only` sessions are
included: their context reads delete expired entries.

**Recovery.**

1. Stop issuing work in every connected MCP client.
2. Stop every writer: disconnect or quit each client (which ends its `orch mcp` child; check with
   `pgrep -fl 'orch mcp'`, and stop a leftover one as in
   [A stuck or leftover `orch mcp` process](#a-stuck-or-leftover-orch-mcp-process)), stop the orchestrator owner (`orch serve`, `orch run --watch` or the dashboard), and let
   one-off `orch` commands finish.
3. Back up the state before you touch it:

   ```bash
   cp -R .orchestry ../orchestry-backup-$(date +%Y%m%d-%H%M%S)
   ```

4. Inspect what is wrong: `orch --json task show <id>`, `orch --json agent status <id>`,
   `orch --json context list`, `orch logs --task <id>`.
5. If a list disagrees with the entity files, rebuild the index as in
   [Rebuilding an index after a lost update](#rebuilding-an-index-after-a-lost-update). A rebuild restores
   entries for files that exist; it cannot bring back an overwritten field or a deleted context entry.
6. Restore lost fields and entries from what you know is right: your backups, the run logs, the messages and the
   agents' outputs. Use the normal commands (`orch task assign`, `orch agent update`, `orch context set`), one
   writer at a time.
7. Restart the orchestrator owner, then **every** MCP session (restart each client, or its MCP server). A restart
   is required after a directory under `.orchestry/` was removed or replaced: each orch 0.13.0 `orch mcp` process
   remembers the directories it has created, and only a new process forgets them. (The daemon of this release
   recreates such a directory at its next write into it.)

**Prevention.** Do not remove or replace directories under `.orchestry/` while anything runs. Make bulk or
consistency-sensitive changes (configuration, many tasks, team changes) while the other writers and any client
that reads context are stopped. This is advice, not a lock: nothing enforces it.

## Restoring a broken configuration

**Symptom.** `orch status` and `orch task list` fail with `✕ parse <project>/.orchestry/config.yml` (exit 1), while
other commands such as `orch agent list`, `orch context list` and `orch config get` still work. In an open
`orch mcp` session, `status` and the `orch://status` resource fail with the same error and other tools still
answer. A new `orch mcp` starts but answers every request, `initialize` included, with that error (JSON-RPC
`-32001`, `exit_code` 1). If the file is not valid YAML at all, the error reads
`<project>/.orchestry/config.yml is not valid YAML: …` instead.

**Cause.** `config.yml` holds a value of the wrong type for a known key, or invalid YAML, written by a hand edit
or another program. orch's own writers do not store a wrongly typed value: `orch config set` and `config_set`
reject it with `config: missing required field or invalid known field type/enum` (the CLI exits 2), `config_patch`
with `<file>: config.<key>: invalid field type`, and the file is left unchanged.

**Fix.**

1. Find the bad key. The error does not name it: compare with a backup
   (`diff <backup>/config.yml .orchestry/config.yml`), or check the values you edited last. `orch config get <key>`
   still prints the stored value, for example `scheduling.poll_interval_ms = "hello"` where a number belongs.
2. Restore a valid file: copy back your backup, or edit `.orchestry/config.yml` by hand. For a wrongly typed value
   you can also set that key to a valid value, `orch config set scheduling.poll_interval_ms 10000`; setting any other
   key fails while the file is broken, and on invalid YAML `orch config set` fails too.
3. Check with the CLI: `orch status` succeeds again. The daemon needs no restart.
4. An open MCP session sees the repair at its next call. A session whose `initialize` failed must initialise
   again: reconnect the server, or restart the client's session.

**Prevention.** Change configuration with `orch config set` (or `config_set`) rather than by hand, and back up
`.orchestry/config.yml` before a hand edit.

## Capturing `orch mcp` diagnostics

**Why.** `orch mcp` keeps no log file and no record of requests or errors. Its diagnostics go to stderr only, and
they are off unless `RUST_LOG` or `ORCHESTRY_DEBUG` is set. Only startup errors and the fatal
`cannot write to stdout` error are printed without them. The MCP client owns the server's stderr and may discard
it, so by default there may be no trace at all. stdout cannot carry diagnostics: it is the protocol.

**Recipe.** Register a wrapper instead of `orch mcp`. It appends the server's stderr to one file per server
process and leaves stdout alone:

```sh
#!/bin/sh
# orch-mcp-logged: run orch mcp with its stderr kept in one file per server.
# stdout is left alone: it carries the protocol.
umask 077                                   # logs can hold project content
dir="$HOME/.local/state/orch-mcp"
mkdir -p "$dir"
log="$dir/orch-mcp-$$.log"                  # exec keeps the PID: $$ is the server's
echo "start $(date -u +%Y-%m-%dT%H:%M:%SZ) pid $$ cwd $PWD args $*" >>"$log"
export NO_COLOR=1                           # no colour codes in the file
exec /path/to/orch mcp "$@" 2>>"$log"
```

1. Save it, for example as `~/bin/orch-mcp-logged`, replace `/path/to/orch` with `command -v orch`, and
   `chmod +x` it.
2. In the client's entry, replace `orch` and its `mcp` argument with the wrapper, and keep the other arguments:
   `claude mcp add orch -- /path/to/orch-mcp-logged`, or `"command": "/path/to/orch-mcp-logged", "args":
   ["--agent", "agt_example"]`.
3. Choose how much to record in the entry's environment (`"env": {"RUST_LOG": "info"}`, or `[mcp_servers.orch.env]`
   in Codex). The wrapper passes the environment through.

The file name carries the server's PID, so it matches `ps` and the procedure below. Measured with the wrapper
above, for a session with a `task_add`, a `task_show` of a missing task, an invalid JSON line and an unknown
tool:

| Setting | What the file records |
|---|---|
| none | The wrapper's `start` line, startup errors and the `cannot write to stdout` error |
| `RUST_LOG=warn` | Also each JSON-RPC error sent to the client: `WARN rmcp::service: response error id=4 error=ErrorData { code: ErrorCode(-32602), message: "tool not found", data: None }` |
| `RUST_LOG=info` | Also the connection's life: `Service initialized as server`, `client initialized`, `input stream terminated`, `serve finished quit_reason=Closed` |
| `RUST_LOG=debug` or `ORCHESTRY_DEBUG=1` | The same as `info` for this session. The SDK's `debug` and `trace` events (requests and responses with their arguments and results) are never recorded, even with `RUST_LOG=trace` or `RUST_LOG=rmcp=trace`; orch's own `debug` lines, such as an error's cause chain, are still recorded |

- Tool errors (`isError: true`, such as `Task not found: tsk_nope`) and dropped invalid JSON lines leave no SDK
  line at any level.
- `RUST_LOG` wins over `ORCHESTRY_DEBUG` when both are set.
- Lines have no timestamps; the `start` line has one.
- The file can still hold project content. `info` lines include the client's `initialize` parameters and each
  notification it sends; a `warn` line repeats the error sent to the client. An invalid-arguments error names the
  argument and the type or form it expects, never the value sent (``field `priority`: expected i64``, or
  `Invalid since duration: expected ASCII digits followed by s, m, h, or d within the supported time range` for
  a bad `logs` `since`); for an unknown argument it names that argument. `umask 077` makes it readable by you only
  (mode `600`). Nothing rotates or deletes these files.

When the client stops reading the server's output, a file recorded at `RUST_LOG=info` ends like this (from a
disposable project):

```
ERROR serve_inner: rmcp::service: fail to response message error=Broken pipe (os error 32)
 INFO serve_inner: rmcp::service: task cancelled
 INFO serve_inner: rmcp::service: serve finished quit_reason=Cancelled
  ✕ cannot write to stdout: Broken pipe (os error 32); the MCP client can no longer receive responses. Calls already started may have completed: inspect state before retrying
```

The server then exits with code 1. In this example the `task_add` whose answer was lost had run: the task
exists.

## A stuck or leftover `orch mcp` process

**Current daemon bridge.** Check `orch daemon status` first. The bridge admits up to 16 project calls and
two diagnostic calls; excess calls are refused immediately, while daemon admission and storage can delay
admitted work. A lost observation is recovered on a later explicit call, with at most one recovery pending.
There is no background daemon start or mutation replay.

On stdin EOF the bridge stops its observation/recovery tasks and drains healthy responses. A stdout frame's
writes and flush share a 5 s budget, even with partial progress; pending output shutdown is also bounded.
Output failure returns `cannot write to stdout` and exit 1, including when the client has already closed stdin
but keeps output unread. This is not a total-stop deadline or a limit on an already-started daemon operation.
Inspect state before repeating a call whose answer was lost. Restarting the MCP client does not stop the daemon
or undo its writes. See [MCP lifecycle](manual/17-mcp-server.md#exit-codes-and-startup-errors).

### Legacy direct-writer MCP recovery (0.12–0.13)

The observations and procedure below describe the old direct-writer server, which ran its own probes and FIFO.
They are retained for older installations; the exit times are historical observations, not deadlines for the
daemon bridge. Its probes now run in the daemon: do not apply the legacy probe-cleanup procedure to them.

**Legacy symptom.** A client shows the orch server as not responding; every call is refused with
`Not executed: timed out after 10 s waiting for the store queue; retry later`; or an `orch mcp` process is still
running after its client quit.

**What normally happens.** Measured on macOS with disposable projects:

| Event | Exit | Time to exit |
|---|---|---|
| SIGTERM, SIGINT or SIGHUP, also during a stalled `doctor` probe (SIGTERM also during a write waiting for a held `.index.lock`) | 0 | about 2.3 s |
| The client closes stdin | 0 | up to about 5 s, while running calls finish |
| The client stops reading stdout | 1, `cannot write to stdout` | 0.3 s |
| SIGKILL | killed | at once; a running `doctor` probe is left behind |

A call still running when the server stops gets no answer, and its change may be saved: in the `.index.lock` case
the new task's file was written and the client got no reply. **A server still running 10 s after SIGTERM is
stuck.** A server whose calls all time out has one call that never finishes, usually on a slow or hung file
system: a running call has no time limit of its own.

**Do not** use `pkill -f 'orch mcp'` or `killall orch`. Every client has its own server, and a pattern also stops
the servers of other clients and projects, and `orch run` or `orch serve`.

**Procedure.**

1. Find the exact process:

   ```bash
   ps -o pid,ppid,pgid,stat,etime,command -p "$(pgrep -d, -f 'orch mcp')"
   ```

   ```
     PID  PPID  PGID STAT ELAPSED COMMAND
   62392 62297 62295 T      00:02 /path/to/orch mcp --project-root /path/to/project
   ```

   Pick the row by its parent (`PPID` is the client; `ps -o pid,command -p <ppid>` shows it, and `1`, or on
   Linux a subreaper such as `systemd --user`, means the client is gone), its age and its arguments. Then
   `PID=<pid>`.
2. Note the server's `doctor` probes before you signal it. They are its only child processes; usually there are
   none. Keep each probe's `PID`, `PGID`, `STARTED` and `COMMAND`: after a SIGKILL its parent changes, so these
   are what identify it later:

   ```bash
   for p in $(pgrep -P "$PID"); do ps -o pid,pgid,lstart,command -p "$p"; done
   ```

   ```
     PID  PGID STARTED                      COMMAND
    5987  5987 Sat Nov  7 18:45:41 2026     /bin/sh /path/to/bin/claude --version
   ```

3. Ask it to stop, and wait at most 10 s:

   ```bash
   kill -TERM "$PID"
   for i in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$PID" 2>/dev/null || break; sleep 1; done
   ps -o pid,stat,etime,command -p "$PID"
   ```

   If only the header line prints, it has exited, and it stopped its own probes: go to step 7.
4. Still listed? Read `STAT` before you escalate:

   | `STAT` | Meaning | Do |
   |---|---|---|
   | `T` | Stopped, by job control or a debugger; it has not acted on the SIGTERM | `kill -CONT "$PID"`: it then finishes stopping (measured: within 1 s). Or go on to step 5 |
   | `Z` | Already exited; its client has not collected it | Nothing to kill. Quit or restart the client |
   | `D` (Linux) or `U` (macOS) | Waiting in the kernel, usually on storage | Fix the storage first. If the wait is truly uninterruptible, a SIGKILL takes effect only when it ends; the state letter alone does not tell you that (Linux also shows `D` for waits a SIGKILL can end). Not measured here |
   | anything else | It ignored the SIGTERM | Step 5 |

5. Kill it: `kill -KILL "$PID"`. A SIGKILL gives the server no chance to stop its probes.
6. Stop leftover probes. For each pid from step 2 that is still running, check that it is still the probe: the
   same `PGID` (equal to its pid), `STARTED` and `COMMAND` as you noted. Do not require a particular parent. The
   killed server's probes are adopted by a new parent: PID 1 on macOS (measured); on Linux, PID 1 or the nearest
   ancestor that set `PR_SET_CHILD_SUBREAPER`, such as `systemd --user` or a container's init (checked in a
   Linux container with a stand-in for the server, not with orch itself). Then signal its process group:

   ```bash
   ps -o pid,ppid,pgid,lstart,command -p <probe-pid>
   kill -TERM -<probe-pid>      # leading minus: the probe's process group
   ```

   If you did not note them (the server was killed or crashed before you looked), search for probes left behind.
   A probe is an adapter's version check, such as `claude --version`, or `opencode run --help`, leading its own
   group. The search does not filter on the parent, so it can also list a version check that something else is
   running now:

   ```bash
   ps -eo pid,ppid,pgid,etime,args | awk 'NR == 1 || ($1 == $3 && / (--version|run --help)$/)'
   ```

   Check each row before you signal it. Its command must be one of this project's adapters, and it must have
   started before the server died; a probe never runs longer than 10 s while its server is alive. Look at its
   parent (`ps -o pid,command -p <ppid>`): a leftover probe's parent is PID 1 or, on Linux, a subreaper, such as
   a service manager, a container's init or a supervisor. A version check whose parent is a shell is someone's
   current command, not a leftover. A recycled pid would make you kill an unrelated group.
7. Inspect before anything is retried. Each call that was in flight, and each call whose answer was lost, may have
   saved its change. Read the entities it touched (`orch --json task show <id>`, `orch --json task list`,
   `orch --json context get <key>`, `orch --json agent status <id>`) and the captured log, if any. Retry only what
   is missing. For a create, look in the list first: repeating it makes a second entity.
8. If a list disagrees with the entity files (a task `task show` finds but `task list` omits), stop every writer,
   back up and rebuild the index, as in steps 2 to 5 of
   [Recovering after concurrent MCP writers](#recovering-after-concurrent-mcp-writers).
9. Restart the client. It starts a new server.

**Never delete `.index.lock` files** while you do this. A store's `.index.lock` stays after every write: that is
normal, not a sign of a stuck process, and the file is locked only while a writer updates the index. Deleting it
lets two writers update the same index at once. `orch mcp` never takes the orchestrator's PID lock,
`.orchestry/orchestry.lock` ([Corrupt lock file](#corrupt-lock-file)), so there is no MCP lock to clear.

**Prevention.** Keep the server's stderr ([Capturing `orch mcp` diagnostics](#capturing-orch-mcp-diagnostics)).
Treat a server still running 10 s after its client closed stdin or sent SIGTERM as stuck. Send a call that depends
on an earlier one only after the earlier call's answer arrives
([manual](manual/17-mcp-server.md#results-and-errors)).
