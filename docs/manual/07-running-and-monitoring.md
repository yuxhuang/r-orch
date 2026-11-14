# Running and monitoring

Tasks do nothing until the orchestrator starts them, and the orchestrator runs inside the project's [daemon](18-daemon.md). Read this chapter to start work, to watch what happens, to stop safely, and to find the logs. It covers `orch run`, `orch serve`, `orch status`, `orch logs`, `orch doctor` and `orch update`.

Every command in this chapter needs a running daemon, except `orch update`, the daemon starts `orch run --watch` and
`orch serve` (without `--once`), and `orch doctor`, which without a daemon prints only a discovery note
([Check your setup](#check-your-setup)). Without one it prints `No orch daemon is running for <project root>` and exits 7 ([When no daemon answers](18-daemon.md#when-no-daemon-answers)).

## In this chapter

- [Choose how to run](#choose-how-to-run)
- [Before you run anything](#before-you-run-anything)
- [Run one task or all todo tasks](#run-one-task-or-all-todo-tasks)
- [Keep the orchestrator running in your terminal](#keep-the-orchestrator-running-in-your-terminal)
- [Run orch without a terminal](#run-orch-without-a-terminal)
- [Stop orch safely](#stop-orch-safely)
- [Check the status](#check-the-status)
- [View logs](#view-logs)
- [Check your setup](#check-your-setup)
- [Update orch](#update-orch)

## Choose how to run

The **orchestrator** is the loop that matches tasks to idle agents and starts runs ([The orchestrator](02-concepts.md#the-orchestrator)). It runs inside the daemon, the one process that owns the project, and it dispatches by itself only while the daemon's **dispatch** is on. Some commands start the daemon; the others ask the running one.

| You want to | Use | It ends |
|---|---|---|
| Start the daemon and keep dispatching, in this terminal | `orch run --watch`, or `orch serve`, which is the same | When you stop it; it stops the daemon |
| Start the daemon in the background | `orch daemon start --detach` (add `--dispatch on` to dispatch at once) | When you run `orch daemon stop` |
| Turn dispatching on or off in a running daemon | `orch daemon dispatch on` / `off` | |
| Try one task and see its output | `orch run <task-id>` | When the run ends |
| Run everything that is waiting, once | `orch run --all` | When no run of the batch is in progress |
| Run one scheduling check | `orch run --once` | After that check |
| Process what is waiting and exit, for a script, with JSON Lines logs | `orch serve --once` (dispatch must be off) | When all tracked tasks are final |
| See and manage everything on screen | `orch` or `orch tui` ([Dashboard](08-dashboard.md)) | When you quit; the daemon keeps running |
| Use the dashboard in a browser | `orch web` ([Web dashboard](09-web-dashboard.md)) | When you stop it |
| Accept work from another machine | `--listen` when you start the daemon: `orch daemon start`, `orch serve` or `orch run --watch` ([Remote agents](11-remote-agents.md)) | |

## Before you run anything

1. Check that the daemon runs: `orch daemon status` shows its PID and whether dispatch is on.
2. Check that an agent can do the work. `orch agent list` shows the agent as `idle` in the STATUS column, not `disabled`.
3. Check that the task is waiting. `orch task list --status todo` lists it, and its AGENT column names that agent or is empty.
4. Check that the agent's tool is installed. `orch doctor` shows a `✓` next to the agent's adapter ([Check your setup](#check-your-setup)).

> **Warning:** Every run of an AI agent uses your provider account and quota, and the agent changes files in your project. `orch run --all`, `orch run --watch`, `orch serve` and `orch daemon dispatch on` start every waiting task. Check `orch task list --status todo` first.

> **Note:** Do not start the daemon from inside another AI agent's session, such as a Claude Code session, when a `claude` agent could run. The `claude` tool usually refuses to start inside itself, so the task fails at once. Run orch from a plain terminal, or use `shell` agents. See [Troubleshooting](13-troubleshooting.md).

## Run one task or all todo tasks

### Run one task

```sh
orch run tsk_lICHVKV
```

```text
  orch · running tsk_Fi_TstG "Say hello"

  15:08:32  ▸ hello from greeter
  ✓ Done
```

The daemon starts the run, and the command follows it: it prints the agent's output as it arrives, each line cut to 80 characters, then `Done` or `Failed`. The exit code is 0 for success and 1 for failure, so scripts can test it. Dispatch does not need to be on.

```text
  orch · running tsk_glJT-SE "Fail"

  15:08:33  ▸ going to fail
  ✕ Failed
```

After a successful run the task is `done`, or `review` if the agent's policy asks for your approval ([Review a task](04-tasks.md#review-a-task-approve-or-reject)).

`orch run <task-id>` starts only that task, with the usual checks. A `retrying` task starts at once, without waiting for its retry delay; that attempt replaces the queued retry ([How orch retries by itself](04-tasks.md#how-orch-retries-by-itself)).

When the task cannot start, the command prints its header line and the refusal, and exits 1:

```text
  orch · running tsk_6joYwaY "C2"

  ✕ task is not eligible for launch
```

The task cannot start when:

- It is not `todo` or `retrying`, for example `done` or `cancelled`. Run `orch task retry` first for a `failed` or `cancelled` task.
- Its agent is disabled, or busy with another run, or the concurrency limit is reached.
- It waits for approval, has a dependency that is not `done`, belongs to a paused goal, or its scope overlaps a running task. `orch serve --once` warns about a scope hold when it first sees it and then reminds you; what that warning does and does not tell you is in [The scope-hold warning](04-tasks.md#the-scope-hold-warning).

Other `todo` tasks of the same agent do not stop it. To see why a task cannot start, run `orch task show <task-id>` and `orch agent list`.

### Run all waiting tasks

```sh
orch run --all
```

```text
  orch · running all todo tasks
```

`--all` asks the daemon to start every `todo` task that can start, as one batch, within the concurrency limit (default 6, set by `scheduling.max_concurrent_agents`), and waits until no run is in progress. A task that depends on another starts after it is `done`. It prints nothing for each event, and its exit code is 1 if any of its runs failed or was cancelled, otherwise 0. Use `orch task list` afterwards to see the results.

> **Note:** If the agent produces no output, `orch run <task-id>` waits for it. Stopping the command (`Ctrl+C`, or `timeout 3600 orch run <task-id>` on Linux) stops following the run, not the run itself: the agent keeps running in the daemon. To stop it, cancel the task: `orch task cancel <task-id>`. `orch run` then prints `✕ observation interrupted; runs continue in the daemon` and exits 1. `orch run --all` behaves the same.

### Run one scheduling check

```sh
orch run --once
```

`--once` asks the running daemon for one scheduling check, as dispatch would make it, prints that check's events and exits. It does not stop the daemon, and it does not turn dispatch on or off.

`orch run` needs a task ID, `--all`, `--once` or `--watch`. With none of them it prints `✕ Specify a task ID, --all, --once, or --watch` and exits with 2. If you combine them, `--watch` wins over `--all`, and `--all` wins over a task ID. So `orch run --all --watch` is the same as `orch run --watch`. Some commands, such as `orch org deploy`, print `Next: orch run --all --watch`. That starts the daemon with dispatch on, which also dispatches everything that waits; if a daemon already runs, use `orch daemon dispatch on` instead.

### Options for `orch run`

| Option | What it does |
|---|---|
| `[task-id]` | Runs this one task in the running daemon. |
| `--all` | Runs all `todo` tasks in the running daemon. |
| `--once` | Runs one scheduling check in the running daemon. |
| `--watch` | Starts the daemon in this terminal with dispatch on. See below. |
| `--from-agent` | With `--watch` only (exit 1 otherwise). Allows the start from an agent's environment (`ORCH_TASK_ID` or `ORCH_AGENT_ID` set), which otherwise refuses with exit 2 ([Refusals](18-daemon.md#start-the-daemon)). Use it only when you deliberately start a daemon from such a shell. |
| `--verbose` | Refused with `--watch` (exit 2, below). Has no effect with a task ID, `--all` or `--once`, which print the agent's output anyway. |
| `--listen [<ADDR>]` | With `--watch` only. Accepts connections from other machines. See [Remote agents](11-remote-agents.md). The default address is `127.0.0.1:7690`, which accepts only this machine. |
| `--peer-name <NAME>` | With `--watch` only. The name this machine gives itself to peers. |
| `--json`, `--yaml`, `--quiet`, `--no-color`, `--ascii` | Output options for every command ([Choose the output format](12-configuration-and-output.md#choose-the-output-format)). |

If a daemon already runs for the project, `orch run --watch` refuses with exit 4: it does not resume a daemon whose dispatch is off. `orch run <task-id>`, `--all` and `--once` need that running daemon instead.

```text
  ✕ orch daemon already running (PID 41235)
    Use: orch daemon dispatch on
```

`orch run` refuses two options of orch 0.13.0 with exit 2, before it contacts any daemon:

| You pass | `orch run` prints |
|---|---|
| `--tick-interval <ms>`, in any form | `--tick-interval is not accepted: the daemon uses scheduling.poll_interval_ms from config.yml (orch config set scheduling.poll_interval_ms <ms>)` |
| `--verbose` with `--watch` | `--verbose is not accepted by a daemon start; use RUST_LOG or orch logs --follow` |

The message follows `✕`, as every error does. `orch run --help` no longer lists `--tick-interval`. See
[Daemon logs and events](#daemon-logs-and-events) for what replaces both.

## Keep the orchestrator running in your terminal

`orch run --watch` starts the daemon in this terminal with dispatch on, and runs until you stop it. The daemon starts tasks as you add them, retries failures, and creates tasks for autonomous agents. Other terminals, the dashboard and MCP clients use this daemon while it runs. `orch run --watch` is `orch daemon start --dispatch on`: the same start, the same refusals and the same output.

```sh
orch run --watch
```

Once the daemon is ready it prints two lines, with your own socket path, and then nothing more until it stops:

```text
listening /private/tmp/hello-orch/.orchestry/run/orch.sock
dispatch on
```

The command shows no status line and no agent output: it is the daemon, not a client. To watch what happens, run
`orch logs --follow` in another terminal ([Daemon logs and events](#daemon-logs-and-events)).

With dispatch on and the scheduler idle, the first change that may have made work ready, such as a new task
or a finished run, schedules a check for 500 ms later; further changes coalesce without moving that deadline.
Changes during a check schedule at most one follow-up, due 500 ms after it ends. After each completed check,
the daemon schedules the next periodic check using `scheduling.poll_interval_ms`, 10 seconds by default.
These are scheduling intervals, not maximum elapsed times between checks or promises of when a task starts:
a long check, waiting for the coordinator or a busy machine can delay execution
([Dispatch follows changes](18-daemon.md#how-serve-run-and-the-dashboard-map-to-the-daemon)). The daemon reads
the interval again for the next scheduling round, so `orch config set scheduling.poll_interval_ms <ms>` takes
effect without a restart; values under 100 ms count as 100 ms.

Stop it with `Ctrl+C` ([Stop orch safely](#stop-orch-safely)). That stops the daemon, so every other client of the project then reports that no daemon runs.

## Run orch without a terminal

`orch serve` starts the daemon in the foreground with dispatch on, exactly like `orch run --watch`: it prints the
same two lines (`listening <socket>`, `dispatch on`) and then nothing more. **A foreground `orch serve` is the
daemon; it no longer writes a log line for each event.** Only `orch serve --once`, a client of a running daemon,
writes JSON Lines ([below](#use-serve---once-in-a-script)). To keep the daemon running after you close the
terminal, use `orch daemon start --detach --dispatch on` instead: a daemon started by `orch serve` stops when its
terminal sends SIGHUP ([Start the daemon](18-daemon.md#start-the-daemon)).

```sh
orch serve
```

The logging options of orch 0.13.0 apply to `serve --once` only. Without `--once`, `orch serve` refuses them with
exit 2, before it contacts any daemon, and prints the first that applies:

| You pass, without `--once` | `orch serve` prints |
|---|---|
| `--tick-interval <ms>` (also refused with `--once`) | `--tick-interval is not accepted: the daemon uses scheduling.poll_interval_ms from config.yml (orch config set scheduling.poll_interval_ms <ms>)` |
| `--log-file <path>` | `--log-file is not accepted by a daemon start; the foreground daemon logs to stderr (RUST_LOG), a detached one to .orchestry/daemon.log. Follow events with: orch logs --follow` |
| `--log-format` with any value but `json` | `--log-format is not accepted by a daemon start; the foreground daemon logs to stderr (RUST_LOG), a detached one to .orchestry/daemon.log. Follow events with: orch logs --follow` |
| `--verbose` | `--verbose is not accepted by a daemon start; use RUST_LOG or orch logs --follow` |

`--log-format json`, the default, is accepted, so an existing `orch serve --log-format json` still starts. Nothing
is ignored silently.

### Daemon logs and events

- **Events** (task and run changes): `orch logs --follow`, from any terminal, streams them from the daemon
  ([View logs](#view-logs)). `orch serve --once` writes them as JSON Lines or text for one batch.
- **The daemon's own diagnostics** go to standard error, and only when you ask for them: set `RUST_LOG` (for
  example `RUST_LOG=info`) or `ORCHESTRY_DEBUG=1` before you start it. Without either, a foreground daemon prints
  only its two readiness lines.
- **A detached daemon** (`orch daemon start --detach`) writes the same output to `.orchestry/daemon.log`
  ([Start the daemon](18-daemon.md#start-the-daemon)).

### Logs of `serve --once`

`orch serve --once` writes one line for each event to standard output:

```sh
orch serve --once > serve.log 2>&1
```

Each line is a JSON object, so tools such as `jq` can read it. For people, use `--log-format text`. The first
record is `serve:started` with `mode` `once`; the last is `serve:finished` ([below](#use-serve---once-in-a-script)).
In orch 0.13.0 `serve:started` also had `pid` and `poll_interval_ms`; it no longer has them.

**Start-up warnings.** Right after `serve:started`, and before the batch starts any work, `serve --once` logs
one `serve:diagnostic` warning for each effort warning
([Effort warnings](16-eco-mode-and-effort.md#effort-warnings)), and one for each agent whose eco mode conflicts with
a custom template ([Eco mode and custom templates](16-eco-mode-and-effort.md#eco-mode-and-custom-templates)): the
same warnings `orch run` prints on standard error. The records are per warning, not per agent: an agent with two
warnings gets two records with the same `agentId`, like `pi1` here:

```text
17:57:31.513 INFO  serve:started  mode=once
17:57:31.514 WARN  serve:diagnostic  agentId=agt_3YgL3Wx agentName=cx message=capability-unknown: codex effort with no model: forwarding effort can bypass a managed new-thread model default, so codex may select a different model; set an explicit model to avoid this
17:57:31.514 WARN  serve:diagnostic  agentId=agt_uuqDve1 agentName=pi1 message=clamp-possible: pi clamps the requested thinking level to the model's support; a non-reasoning model runs with thinking off
17:57:31.514 WARN  serve:diagnostic  agentId=agt_uuqDve1 agentName=pi1 message=request-only: the explicit --thinking level takes precedence over the model's :level suffix
17:57:31.521 INFO  serve:finished  result=all_done exit_code=0
```

In JSON the fields are `agentId`, `agentName` and `message`:

```json
{"ts":"2026-11-09T17:57:31.498Z","level":"warn","event":"serve:diagnostic","agentId":"agt_3YgL3Wx","agentName":"cx","message":"capability-unknown: codex effort with no model: forwarding effort can bypass a managed new-thread model default, so codex may select a different model; set an explicit model to avoid this"}
```

If `serve --once` cannot read the warnings from the daemon, it logs one `serve:diagnostics_unavailable` warning
with an `error` field and carries on. A foreground `orch serve` is the daemon and logs neither record.

When a run starts
for an agent with an effort, each effort warning for that run is logged once as an `orchestrator:error` warning
with `fatal=false` and the context `effort preparation for run <run-id> (agent <agent-id>)`. An agent with no
warning logs none ([Warnings when a run starts](16-eco-mode-and-effort.md#warnings-when-a-run-starts)).

| Option | What it does | Default |
|---|---|---|
| `--once` | Runs a batch in the running daemon and exits when all its tasks are final (below). | Off |
| `--log-format <format>` | With `--once`: `json` or `text`. Without `--once`: only `json` is accepted. | `json` |
| `--log-file <path>` | With `--once`: also appends the log to this file. Without `--once`: refused (exit 2). | None |
| `--verbose` | With `--once`: adds the agent's output events, which are frequent. Without `--once`: refused (exit 2). | Off |
| `--listen [<ADDR>]`, `--peer-name <NAME>` | Without `--once`: start options of the daemon, as for `orch run --watch`. | |
| `--from-agent` | Without `--once`: allows the start from an agent's environment, as for `orch run --watch`. With `--once`: rejected (exit 1). | Off |

No option sets the interval. `scheduling.poll_interval_ms` sets the periodic delay after a completed check;
changes can schedule an earlier check, subject to the [scheduling-only qualification above](#keep-the-orchestrator-running-in-your-terminal).
Change the interval with `orch config set scheduling.poll_interval_ms <ms>`, which a running daemon reads for
its next scheduling round.

### Use `serve --once` in a script

`orch serve --once` asks the running daemon for a **batch**: it starts every waiting task and exits when every task it tracks is `done`, `failed` or `cancelled`. It needs a running daemon with dispatch **off**; with dispatch on it refuses with `dispatch is ON; serve --once needs dispatch OFF` (exit 2), and while another batch runs it exits 4. The batch tracks the tasks that were waiting and every task that becomes eligible while it runs, including new ones. It logs the result and sets the exit code:

```text
23:07:42.209 INFO  serve:finished  result=has_failed exit_code=1
```

In this run a failed task in the project gave `result=has_failed` and exit code 1. `--once` does not create tasks for autonomous agents.

A later `orch daemon dispatch on` or `off`, from any terminal or the dashboard, ends the batch: `serve --once` reports `interrupted`, and the agents keep running. Stopping `serve --once` (`Ctrl+C`) also ends the batch and leaves its running agents running in the daemon; in orch 0.13.0 it stopped them. The daemon also ends the stream of a command that stops reading its output for about five seconds while the daemon has more to send (read from the source: an event that cannot be queued within the five-second write limit cancels the observation; not tested or timed here); the agents keep running. To run a batch from cron, start the daemon yourself first; see [Recovery](18-daemon.md#recovery) for a cron line that fails visibly.

> **Note:** `serve --once` waits while a task is in `review`. If a task needs your approval and nobody gives it, the command never exits. It stays up until you run `orch task approve` from another terminal. To avoid this in scripts, give the agents the approval policy `auto`, or give the tasks review criteria ([Add review checks to a task](04-tasks.md#add-review-checks-to-a-task)).

To check that `serve` is working, wait for the `listening <socket>` line, or run `orch daemon status` in another terminal. If a daemon already runs for the project, `serve` exits with code 4 and `Use: orch daemon dispatch on` instead.

## Stop orch safely

Stop the daemon with `orch daemon stop`, from any terminal. A daemon in the foreground (`orch daemon start`, `orch serve`, `orch run --watch`) also stops on `Ctrl+C`, SIGTERM (`kill <pid>`) or SIGHUP (closing its terminal or tmux pane). Each way stops it in an orderly way: it stops its running agents (each gets a 10 s grace before it is killed), records their runs as `cancelled`, settles their tasks' retries and releases the project. `orch daemon stop` returns once the daemon has confirmed its cleanup and released the project, by exiting or by releasing its ownership lock ([Stop](18-daemon.md#stop)).

To stop dispatching new work but keep the daemon, and the agents that already run, use `orch daemon dispatch off`.

`orch run <task-id>`, `orch run --all`, `orch run --once` and `orch serve --once` are clients of the daemon: stopping them with `Ctrl+C` stops following, not the agents they started. Cancel a task to stop its agent.

Avoid `kill -9` on the daemon. It cannot stop its agents or save its state; every client then reports that no daemon runs, and the next `orch daemon start` reconciles what it left ([After a daemon crash](../RUNBOOK.md#after-a-daemon-crash-or-kill--9)).

To stop one task and keep the daemon running, cancel the task: `orch task cancel <task-id>` ([Cancel a task](04-tasks.md#cancel-a-task)). The daemon stops the task's agent before the command returns.

> **Note:** Stopping an agent sends a stop signal to its whole process group. A process the agent started a moment earlier can survive. After a cancel, check for leftovers with `ps`. Worktree mode limits the damage because such processes work in a throwaway checkout.

## Stop a stalled run

A **stall** is a run whose agent produces no output for a while. The agent's setting `stall_timeout_ms` sets how long; the default is 600000 ms (10 minutes). When the time passes, orch stops the run. The task then goes to `retrying` or, if no attempts remain, to `failed` ([Task lifecycle](02-concepts.md#task-lifecycle)).

- **Where it works:** in the daemon, at each of its scheduling checks: every poll interval while dispatch is on, at each `orch run --once`, and throughout an `orch serve --once` batch. Each check covers every running run, including those started by `orch run <task-id>` and `orch run --all`.
- **Where it does not:** while dispatch is off and none of those checks runs, nothing stops a silent run; cancel it with `orch task cancel <task-id>`.
- **What it does not catch:** an agent that keeps printing output never stalls, however long it runs.
- **Not a time limit:** the agent setting `timeout_ms` (`--timeout`) is stored but not enforced.

Change the limit for one agent with `orch agent patch <id> -f -`:

```sh
echo '{"config": {"stall_timeout_ms": 120000}}' | orch agent patch agt_3cU5sQA -f -
```

## Check the status

```sh
orch status
```

```text
orch · proj · watching

  done        2
  failed      2
  cancelled   1
```

The first line names the project and says `watching`, because a daemon is answering; it does not show whether dispatch is on (use `orch daemon status`). The lines below count tasks by status. While runs are in progress, orch lists them too, with their agent and elapsed time. `orch status` asks the daemon, so it needs one; without a daemon it exits 7.

`orch status --json` (or `--yaml`) prints `tasks`, `agents` and `state`, which is the data a script needs.

`orch daemon status` shows whether dispatch is on, from the running daemon itself.

## View logs

Every run leaves two files in `.orchestry/runs/`: `<run-id>.json` with the summary, and `<run-id>.jsonl` with the event log, to which orch only appends. `orch logs` reads them. With `serve --once`, `--log-file` also writes that batch's log to a file you choose.

`orch logs` needs a run ID or a filter. Without one it tells you what is missing:

```text
  ✕ Specify a run ID, --task, --agent, or --since <duration>
```

Show the logs of one task:

```sh
orch logs --task tsk_wsk8eC3
```

```text
  Run run_sCSReHu · attempt 1 · failed
  19:07:21  ✕ disk full
  19:07:21  ✕ Shell command exited with code 3
```

Show one run, or a time window:

```sh
orch logs run_sCSReHu
orch logs --agent agt_4EBuHFv --since 5m
```

| Option | What it does |
|---|---|
| `[run-id]` | Shows this run. |
| `--task <task-id>` | Shows the runs of a task. |
| `--agent <agent-id>` | Shows the runs of an agent. |
| `--since <duration>` | Shows runs from the last period, such as `5m` or `1h`. |
| `--follow` | Streams events live until you press `Ctrl+C`. |
| `--json`, `--yaml` | Prints the run records, including the full prompt the agent got. |

`--follow` prints `Following live events... (Ctrl+C to stop)`, then new events as runs happen. It streams from the daemon; events appear while runs happen. `--follow` always prints text.

The JSON form is useful when an agent misbehaves, because the record holds the prompt, the process ID, the error and its kind:

```sh
orch logs --task tsk_wsk8eC3 --json
```

## Check your setup

`orch doctor` reports which adapter tools are installed, and whether the project is ready. The daemon runs the
checks, so the report below needs a running daemon.

```sh
orch doctor
```

```text
  orch doctor · checking adapters and dependencies

  ✓  claude       2.1.303 (Claude Code)
  ✓  codex        codex-cli 0.163.0
  ✕  cursor       Cursor Agent CLI not found. The headless agent CLI is required (cursor-agent or agent).
  ✓  shell        GNU bash, version 3.2.57(1)-release (arm64-apple-darwin26)
  ✓  opencode     opencode v2.0.32
  ✕  pi           Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent
  ✕  grok         Grok CLI not found. Install and authenticate the grok CLI, then ensure `grok` is on PATH.
  ✕  antigravity  Antigravity CLI not found. Install Google Antigravity CLI and ensure `agy` is on PATH.
  ✓  git          git version 2.56.0
  ✓  git repo     git repository detected
  ✓  .gitignore   .orchestry is excluded
  ✓  toolchain    r-orch 0.9.3 (rust 1.99)

  ✓  .orchestry/   exists · 5 agents · 20 tasks

  4 of 8 adapters ready
```

A `✕` next to an adapter matters only if you use that adapter. A `✕` next to `git repo` means the `worktree` and `isolated` modes fail; run `git init` or use `--workspace-mode shared`.

`orch doctor` is a client of the daemon. With a daemon it exits with 0, even when it reports problems.

Without a daemon it runs no check: it starts no process (no adapter, `git` or shell probe) and opens no project store. In a project it prints a header, the project root and whether the daemon left its discovery file, then the [no-daemon message](18-daemon.md#when-no-daemon-answers), and exits 7:

```text
  orch doctor · discovery only (daemon unavailable)

  project root: /path/to/project
  daemon discovery: unavailable
```

`daemon discovery` is `available` when the client could read the project's `daemon.json`. It does not say that a daemon answers: nothing is probed to find out. In a directory with no project it prints `project discovery: unavailable`, then `Not initialized` and `Run: orch init`, and exits 3. To see which agent CLIs are installed, start the daemon and run `orch doctor` again.

The `(daemon unavailable)` header is printed for every failure to connect, including one caused by an invalid `--agent` (exit 2), an unusable `ORCH_SOCKET` (exit 2) or a project that cannot be found (exit 3), so it can stand above an exit-2 error. With a daemon, `orch doctor` does not check locks, saved state or disk space.

> **Note:** `orch --json doctor` prints its header line before the JSON, so the output is not one JSON document.

## Update orch

`orch update` prints your version and how to upgrade (`brew upgrade r-orch`, or build from source). It does not download or install anything, and `--check` prints the same text plus `--check: no registry query is made, so there is nothing to report.`

For the steps, see [Install orch](01-getting-started.md#install-orch). Stop the daemon (`orch daemon stop`) before you upgrade, then start the new one; see [Version skew](18-daemon.md#version-skew).
