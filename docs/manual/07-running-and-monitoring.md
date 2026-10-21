# Running and monitoring

Tasks do nothing until the orchestrator starts them. Read this chapter to start work, to watch what happens, to stop safely, and to find the logs. It covers `orch run`, `orch serve`, `orch status`, `orch logs`, `orch doctor` and `orch update`.

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

The **orchestrator** is the loop that matches tasks to idle agents and starts runs ([The orchestrator](02-concepts.md#the-orchestrator)). Only one orchestrator can run in a project at a time.

| You want to | Use | It ends |
|---|---|---|
| Try one task and see its output | `orch run <task-id>` | When the run ends |
| Run everything that is waiting, once | `orch run --all` | When no run is in progress |
| Watch work happen in a terminal and keep going | `orch run --watch` | When you stop it |
| Run in the background, a server or CI, with machine-readable logs | `orch serve` | When you stop it |
| Process what is waiting and exit, for a script | `orch serve --once` | When all tasks are final |
| See and manage everything on screen | `orch` or `orch tui` ([Dashboard](08-dashboard.md)) | When you quit |
| Use the dashboard in a browser | `orch web` ([Web dashboard](09-web-dashboard.md)) | When you stop it |
| Accept work from another machine | `--listen` on `run --watch`, `serve` or `tui` ([Remote agents](11-remote-agents.md)) | |

## Before you run anything

1. Check that an agent can do the work. `orch agent list` shows the agent as `idle` in the STATUS column, not `disabled`.
2. Check that the task is waiting. `orch task list --status todo` lists it, and its AGENT column names that agent or is empty.
3. Check that the agent's tool is installed. `orch doctor` shows a `✓` next to the agent's adapter ([Check your setup](#check-your-setup)).

> **Warning:** Every run of an AI agent uses your provider account and quota, and the agent changes files in your project. `orch run --all`, `orch run --watch` and `orch serve` start every waiting task. Check `orch task list --status todo` first.

> **Note:** Do not start orch from inside another AI agent's session, such as a Claude Code session, when a `claude` agent could run. The `claude` tool usually refuses to start inside itself, so the task fails at once. Run orch from a plain terminal, or use `shell` agents. See [Troubleshooting](13-troubleshooting.md).

## Run one task or all todo tasks

### Run one task

```sh
orch run tsk_lICHVKV
```

```text
  orch · running tsk_lICHVKV "Build the report"
  19:07:21  ▸ working on: tsk_lICHVKV

  ✓ Done
```

The command prints the agent's output as it arrives, each line cut to 80 characters, then `Done` or `Failed`. The exit code is 0 for success and 1 for failure, so scripts can test it.

```text
  orch · running tsk_wsk8eC3 "Break on purpose"
  19:07:21  ✕ disk full

  ✕ Failed
```

After a successful run the task is `done`, or `review` if the agent's policy asks for your approval ([Review a task](04-tasks.md#review-a-task-approve-or-reject)).

`orch run <task-id>` starts only that task. Three situations make it start nothing. The command then prints only its header line and exits 0:

- The task is not `todo` or `retrying`. Run `orch task retry` first for a `failed` task.
- The task is assigned to an agent that is busy or disabled.
- Another `todo` task is assigned to the same agent. In orch 0.9.3 the agent counts as taken, and the run does nothing. Cancel or finish the other task, reassign one of them, or use `orch run --all`.

To see why, run `orch task list --status todo` and `orch agent list`.

### Run all waiting tasks

```sh
orch run --all
```

```text
  orch · running all todo tasks
```

`--all` starts every `todo` task that can start, within the concurrency limit (default 6, set by `scheduling.max_concurrent_agents`), and waits until no run is in progress. A task that depends on another starts after it is `done`. It prints nothing for each event, and its exit code is 0 even when tasks fail. Use `orch task list` afterwards to see the results.

> **Note:** `orch run <task-id>` and `orch run --all` do not stop a stalled agent. If the agent produces no output, the command waits for it. Wrap the command in your own limit, such as `timeout 3600 orch run <task-id>` on Linux, or cancel the task from another terminal with `orch task cancel <task-id>`.

`orch run` needs a task ID, `--all` or `--watch`. With none of them it prints `✕ Specify a task ID, --all, or --watch` and exits with 2. If you combine them, `--watch` wins over `--all`, and `--all` wins over a task ID. So `orch run --all --watch` is the same as `orch run --watch`. Some commands, such as `orch org deploy`, print `Next: orch run --all --watch`. That works because the command starts the watch loop, which also dispatches everything that waits.

### Options for `orch run`

| Option | What it does |
|---|---|
| `[task-id]` | Runs this one task. |
| `--all` | Runs all `todo` tasks. |
| `--watch` | Keeps running and dispatches new tasks as they appear. See below. |
| `--verbose` | With `--watch`, also prints the agent's output. |
| `--listen [<ADDR>]` | With `--watch` only. Accepts connections from other machines. See [Remote agents](11-remote-agents.md). The default address is `127.0.0.1:7690`, which accepts only this machine. |
| `--peer-name <NAME>` | The name this machine gives itself to peers. |
| `--json`, `--yaml`, `--quiet`, `--no-color`, `--ascii` | Output options for every command ([Choose the output format](12-configuration-and-output.md#choose-the-output-format)). |

If another orchestrator already runs in the project, `orch run` refuses:

```text
  ✕ Orchestrator already running (PID: 88536)
    Use: orch status
```

The exit code is 4. See [Troubleshooting](13-troubleshooting.md) if no orchestrator is actually running.

## Keep the orchestrator running in your terminal

`orch run --watch` runs the orchestrator until you stop it. It starts tasks as you add them, retries failures, and creates tasks for autonomous agents.

```sh
orch run --watch --verbose
```

```text
orch · watching · poll interval 10s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

orch · watching · 1 running · 1 queued    19:07  ▸ working on: tsk_i-xv-Dq
19:07  ✓ DONE  run_gWLQKr3
orch · watching · 0 running · 0 queued
```

The status line shows how many runs are in progress and how many tasks are queued. Without `--verbose` it hides the agent's output and prints only the start and end of each run. The pause between checks is `scheduling.poll_interval_ms`, 10 seconds by default, but a finished run or a new task triggers a check within about half a second.

Stop it with `Ctrl+C` ([Stop orch safely](#stop-orch-safely)).

## Run orch without a terminal

`orch serve` runs the orchestrator as a background process. It writes one log line for each event to standard output.

```sh
orch serve > serve.log 2>&1 &
```

`serve.log` then contains one line per event:

```text
{"ts":"2026-10-18T23:08:21.842Z","level":"info","event":"serve:started","mode":"watch","pid":92105,"poll_interval_ms":1000}
{"ts":"2026-10-18T23:08:21.888Z","level":"info","event":"task:status_changed","taskId":"tsk_kTXhl9p","from":"todo","to":"in_progress"}
{"ts":"2026-10-18T23:08:21.951Z","level":"info","event":"agent:started","agentId":"agt_3cU5sQA","taskId":"tsk_kTXhl9p","runId":"run_hZ8Agb3"}
{"ts":"2026-10-18T23:08:21.985Z","level":"info","event":"agent:completed","runId":"run_hZ8Agb3","agentId":"agt_3cU5sQA","success":true}
…
{"ts":"2026-10-18T23:08:29.883Z","level":"info","event":"orchestrator:shutdown","reason":"Received SIGTERM"}
```

Each line is a JSON object, so tools such as `jq` can read it. For people, use `--log-format text`:

```text
23:07:26.008 INFO  serve:started  mode=once pid=86285 poll_interval_ms=500
23:07:26.087 INFO  task:status_changed  taskId=tsk_wNh-4LS from=todo to=in_progress
23:07:26.178 INFO  agent:started  agentId=agt_3cU5sQA taskId=tsk_wNh-4LS runId=run_VO8Qay2
```

| Option | What it does | Default |
|---|---|---|
| `--once` | Processes the `todo` tasks, then exits when all tasks are final. | Off |
| `--tick-interval <ms>` | Sets the pause between checks, in milliseconds. | The project setting, 10000 |
| `--log-format <format>` | `json` or `text`. | `json` |
| `--log-file <path>` | Also appends the log to this file. | None |
| `--verbose` | Adds the agent's output events, which are frequent. | Off |
| `--listen [<ADDR>]`, `--peer-name <NAME>` | As for `orch run`. | |

### Use `serve --once` in a script

`orch serve --once` starts every waiting task and exits when every task in the project is `done`, `failed` or `cancelled`. It logs the result and sets the exit code:

```text
23:07:42.209 INFO  serve:finished  result=has_failed exit_code=1
```

In this run a failed task in the project gave `result=has_failed` and exit code 1. `--once` does not create tasks for autonomous agents.

> **Note:** `serve --once` waits while a task is in `review`. If a task needs your approval and nobody gives it, the command never exits. It stays up until you run `orch task approve` from another terminal. To avoid this in scripts, give the agents the approval policy `auto`, or give the tasks review criteria ([Add review checks to a task](04-tasks.md#add-review-checks-to-a-task)).

To check that `serve` is working, wait for an `orchestrator:tick` line or a line that starts with `agent:`. If another orchestrator holds the project, `serve` exits with code 4 instead.

## Stop orch safely

Stop `run --watch` and `serve` with `Ctrl+C`, or send them `SIGTERM` (`kill <pid>`). Both stop the orchestrator in an orderly way: it stops its runs, saves its state and releases the lock. `run --watch` prints `Shutting down...`, and `serve` logs `orchestrator:shutdown`.

```text
orch · watching · 0 running · 0 queued
Shutting down...
```

After that, `orch status` says `idle`.

Avoid `kill -9`. It leaves the lock file and the saved state behind, so `orch status` may keep saying `watching`. If that happens, check that no orch process runs, and see [Troubleshooting](13-troubleshooting.md).

To stop one task and keep the orchestrator running, cancel the task: `orch task cancel <task-id>` ([Cancel a task](04-tasks.md#cancel-a-task)).

> **Note:** Stopping an agent sends a stop signal to its whole process group. A process the agent started a moment earlier can survive. After a cancel, check for leftovers with `ps`. Worktree mode limits the damage because such processes work in a throwaway checkout.

## Stop a stalled run

A **stall** is a run whose agent produces no output for a while. The agent's setting `stall_timeout_ms` sets how long; the default is 600000 ms (10 minutes). When the time passes, orch stops the run. The task then goes to `retrying` or, if no attempts remain, to `failed` ([Task lifecycle](02-concepts.md#task-lifecycle)).

- **Where it works:** wherever the orchestrator runs, which means `orch run --watch`, `orch serve` and the dashboard (`orch`, `orch tui`, `orch web`).
- **Where it does not:** `orch run <task-id>` and `orch run --all`. They wait for the agent to finish. A hung agent keeps the command waiting. Cancel the task from another terminal with `orch task cancel <task-id>`, or wrap the command in your own limit.
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
orch · proj · idle

  done        2
  failed      2
  cancelled   1
```

The first line names the project and says `idle` or `watching`. `watching` means an orchestrator holds the project. The lines below count tasks by status. While runs are in progress, orch lists them too, with their agent and elapsed time.

`orch status --json` (or `--yaml`) prints `tasks`, `agents` and `state`, which is the data a script needs.

`watching` comes from the saved state, not from a check that the process lives. If you suspect a stale value, look for a process: `pgrep -af 'orch (run|serve)'`.

## View logs

Every run leaves two files in `.orchestry/runs/`: `<run-id>.json` with the summary, and `<run-id>.jsonl` with the event log, to which orch only appends. `orch logs` reads them. With `serve`, `--log-file` writes the orchestrator's own log to a file you choose.

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

`--follow` prints `Following live events... (Ctrl+C to stop)`, then new events as runs happen. Events appear while an orchestrator runs, so start one in another terminal. `--follow` always prints text.

The JSON form is useful when an agent misbehaves, because the record holds the prompt, the process ID, the error and its kind:

```sh
orch logs --task tsk_wsk8eC3 --json
```

## Check your setup

`orch doctor` reports which adapter tools are installed, and whether the project is ready.

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

`orch doctor` works in any directory, and always exits with 0, even when it reports problems. It does not check locks, saved state or disk space. In a directory that is not a project it also says so: `✕  .orchestry/   not found — run: orch init`.

> **Note:** `orch --json doctor` prints its header line before the JSON, so the output is not one JSON document.

## Update orch

`orch update` prints your version and how to upgrade (`brew upgrade r-orch`, or build from source). It does not download or install anything, and `--check` prints the same text plus `--check: no registry query is made, so there is nothing to report.`

For the steps, see [Install orch](01-getting-started.md#install-orch). Stop the orchestrator before you upgrade.
