# Concepts

After this chapter you can explain how orch works: what a project, an agent and a task are, how a task moves
from `todo` to `done`, and what the commands that start orch do. Read it before the task-by-task chapters, or on
its own if you are evaluating orch for your team.

## In this chapter

- [Project](#project)
- [Agents and adapters](#agents-and-adapters)
- [Tasks](#tasks)
- [Task lifecycle](#task-lifecycle)
- [Review and approval](#review-and-approval)
- [Workspaces](#workspaces)
- [Teams](#teams)
- [Goals](#goals)
- [Messages](#messages)
- [Context](#context)
- [Runs and logs](#runs-and-logs)
- [Autonomy](#autonomy)
- [The orchestrator](#the-orchestrator)
- [Details for operators](#details-for-operators)

## Project

A **project** is a directory that contains a `.orchestry/` folder. `orch init` creates it. orch finds your
project by looking for `.orchestry/` in the current directory and then in each parent directory, so you can run
commands from any subdirectory.

Outside a project, most commands print `Not initialized` and exit with code 3. Only `orch --help`,
`orch --version`, `orch init`, `orch doctor`, `orch update` and the `orch org template` commands work anywhere.

Everything orch knows about the project is stored as plain files in `.orchestry/`:

| Path | Holds |
|---|---|
| `config.yml` | The project's settings. See [Configuration](12-configuration-and-output.md#project-settings). |
| `agents/` | One file per agent. |
| `tasks/` | One file per task. |
| `goals/`, `teams/` | Goals and teams. |
| `messages/`, `context/` | Messages between agents and the shared context store. |
| `runs/` | A record and an event log for every run. |
| `workspaces/` | The working copies orch makes for tasks. |
| `templates/default.md` | The prompt template. |

`orch init` also adds `.orchestry` to your project's root `.gitignore`. Your agents' changes reach your project
through their git branches, not through this folder.

If the directory is not a git repository, `orch init` runs `git init`. If there are no commits, it commits your
existing files as "Initial commit". See the [warning in Getting started](01-getting-started.md#create-a-project).

Settings that follow you across projects, such as the dashboard colour palette, live in
`~/.orchestry/global.yml`. See [Global settings](12-configuration-and-output.md#global-settings).

## Agents and adapters

An **agent** is a worker that orch can start. It has a name, an ID such as `agt_cTXiWfi`, an optional role (a
short description of what it is for), and settings such as its model, skills and approval policy.

An **adapter** is the AI CLI that an agent uses. orch supports `claude`, `codex`, `cursor`, `opencode`, `pi`,
`grok`, `antigravity` and `shell`. The `shell` adapter runs a command you choose. See
[Adapters](01-getting-started.md#adapters) for the program behind each one.

An agent has one of five statuses. orch gives work only to `idle` agents.

| Status | Meaning |
|---|---|
| `idle` | Ready for work. |
| `running` | Working on a task. |
| `error` | Never set by orch 0.9.3. |
| `disabled` | You turned it off with `orch agent disable`. Turn it on again with `orch agent enable`. |
| `offline` | A remote agent whose machine cannot be reached. |

Agents on another machine appear as `<id>@<alias>`; see [Remote agents](11-remote-agents.md).

To add and manage agents, see [Agents](03-agents.md).

## Tasks

A **task** is one piece of work for an agent. It has:

- a title and an optional description, which become the agent's prompt;
- a priority from 1 (most urgent) to 4 (least urgent). The default is 3. orch starts lower numbers first;
- an optional assignee. An unassigned task goes to the best-fitting idle agent, judged by the task's labels
  against the agents' skills and roles;
- optional dependencies (`--depends-on`). A task waits until every task it depends on is `done`;
- an attempt limit (`--max-attempts`, default 3);
- optional labels, a file scope, review criteria and a goal;
- optional attached files (`--attach`), which orch copies into `.orchestry/attachments/<task id>/`.

A **file scope** is a list of glob patterns such as `src/auth/**`. orch does not run two tasks at once when their
scopes overlap.

**Review criteria** are automatic checks: `test_pass`, `typecheck` and `lint`.

To create and manage tasks, see [Tasks](04-tasks.md).

## Task lifecycle

Every task has one status. orch and you move it between statuses.

| Status | Meaning | Who or what moves it on |
|---|---|---|
| `todo` | Waiting to start. | The orchestrator starts it when an agent is idle and its dependencies are `done`. |
| `in_progress` | An agent is working on it. | The run ends: success sends it to `review`; failure sends it to `retrying` or `failed`. |
| `review` | The agent finished and the work awaits approval. | You approve it (`done`) or reject it (`todo`). An agent with approval policy `auto` approves it for you. |
| `retrying` | The last attempt failed and attempts remain. | The orchestrator starts it again after a delay. |
| `done` | Approved and finished. | Final. |
| `failed` | The task used all its attempts. | You can retry it (`todo`). |
| `cancelled` | You stopped it. | You can retry it (`todo`). |

`done`, `failed` and `cancelled` are the **terminal** statuses: orch never starts these tasks by itself.

```text
                          +------------ reject (with feedback) -----------+
                          v                                               |
 todo --- start ---> in_progress --- success ---> review --- approve ---> done
  ^                    |   |                        |
  |                    |   +- failure, attempts     +--- cancel ---> cancelled
  |                    |      left                                       ^
  |                    |        v                                        |
  |                    |    retrying --- after the delay ---> in_progress
  |                    |
  |                    +- failure, no attempts left ---> failed
  |
  +---- retry (resets attempts) ---- failed or cancelled

 cancel works from todo, in_progress, retrying and review.
```

What moves a task:

| Event | What happens |
|---|---|
| Start | The orchestrator moves a `todo` or `retrying` task to `in_progress` when it starts a run. A task with unfinished dependencies is **blocked** and does not start. |
| Success | The task always goes to `review`. If the agent's approval policy is `auto`, orch then approves it at once, so you see `done`. |
| Failure | The agent's command exits with an error, or orch stops it because it produced no output for `stall_timeout_ms`. If attempts remain, the task goes to `retrying`. Otherwise it goes to `failed`, and tasks that depend on it fail too. |
| Retry delay | The first retry waits `scheduling.retry_base_delay_ms` (10 seconds by default). Each further retry doubles the wait, up to `scheduling.retry_max_delay_ms` (300 seconds by default). |
| `orch task approve` | Moves a `review` task to `done`. |
| `orch task reject --reason "..."` | Moves a `review` task back to `todo`, sets its attempts to 0, and gives your reason to the agent in the next prompt. |
| `orch task cancel` | Moves any task that is not terminal to `cancelled`. |
| `orch task retry` | Moves a `failed` or `cancelled` task to `todo` and sets its attempts to 0. You cannot retry a task in `retrying`; it restarts by itself. |

If an agent's work cannot be merged back into your project because of a conflict, the task stays in `review`
even when the policy is `auto`, and tasks that depend on it stay blocked. See [Workspaces](#workspaces).

You can follow a task with `orch task show <id>`. See [Tasks](04-tasks.md).

## Review and approval

The **approval policy** is an agent setting. It decides what happens after the agent finishes a task
successfully:

| Policy | After a successful run |
|---|---|
| `auto` | orch approves the task at once. It becomes `done`. |
| `suggest` | The task waits in `review` until you approve or reject it. |
| `manual` | The task waits in `review` until you approve or reject it. |

The project default (`defaults.agent.approval_policy`) is `auto`. `orch agent add` applies it to a new agent,
except that `shell` agents are always `auto` unless you pass `--approval-policy`. The Agent Creator agent that
`orch init` makes uses `suggest`.

> **Note:** In orch 0.9.3, `suggest` and `manual` behave the same way: both leave a finished task in `review`.
> Only `auto` is different.

Tasks that orch creates for an [autonomous agent](#autonomy) are approved automatically.

To review work:

1. List tasks waiting for you: `orch task list --status review`.
2. Look at the task: `orch task show <id>`. The result shows the agent's summary and the files it changed.
3. Approve with `orch task approve <id>`, or send it back with `orch task reject <id> --reason "..."`.

Remote workers have their own approval step. See [Remote agents](11-remote-agents.md).

## Workspaces

A **workspace** is the folder an agent works in. The **workspace mode** decides which kind:

| Mode | What the agent gets |
|---|---|
| `worktree` | A separate git worktree on a new branch. When the run succeeds, orch merges the branch back and removes the worktree. This is the default. |
| `isolated` | A separate copy of the project, made with `git clone`. orch does not merge it back. |
| `shared` | Your project directory itself. |

A task's own mode wins over its agent's mode, and the agent's mode wins over the project default
(`defaults.agent.workspace_mode`). The `worktree` and `isolated` modes need a git repository.

> **Warning:** In `worktree` mode, orch merges back only what the agent **commits** on its branch. Files the agent
> leaves uncommitted are deleted with the worktree, and the work is lost. Tell your agents to commit their work,
> or use `shared` mode. An agent that changes no files, such as the tutorial's greeter, loses nothing.

## Teams

A **team** is a named group of agents with one **lead**. You create a team with `orch team create`, and you
add or remove members, change the lead, or disband it later. A team can also hold a **task pool**, a list of
tasks that belong to it.

In this version, a team does two things you can rely on: it groups agents for display, and it limits a
broadcast message to its members (`orch msg broadcast --team`). The orchestrator does not start tasks from a
team's pool and does not read the team's `auto_claim` setting. Assign pool tasks to agents yourself.

See [Teams and goals](05-teams-and-goals.md#teams).

## Goals

A **goal** is a lasting objective, such as "Ship the release". A goal has a title, an optional description, an
optional assignee, and one of four statuses:

| Status | Meaning |
|---|---|
| `active` | Agents work toward it. |
| `paused` | On hold. It can return to `active`. |
| `achieved` | Done. Final. |
| `abandoned` | Dropped. Final. |

Goals drive [autonomous agents](#autonomy). You can also link a task to a goal with `orch task add --goal-id`.

See [Teams and goals](05-teams-and-goals.md#goals).

## Messages

A **message** is a note sent to an agent. You send one with `orch msg send`, or to many agents with
`orch msg broadcast`. Agents can send messages too.

A message is first `pending`. The agent reads it at the start of its next run, and the message becomes
`delivered`. A message that is not read within its time to live becomes `expired`. The default time to live is
24 hours, and `--ttl` sets it in milliseconds.

A message does not interrupt a run that is already going. If nothing is running for the recipient, the message
waits.

See [Messages and context](06-messages-and-context.md#messages).

## Context

The **context** is a small key-value store that all agents and you share. Use it to pass data between agents,
for example a build status or a decision. You write an entry with `orch context set <key> <value>`, and you can
give it a time to live in milliseconds with `--ttl`.

See [Messages and context](06-messages-and-context.md#context).

## Runs and logs

When orch starts a run, it builds a prompt from the task and a template. The default prompt also tells the agent
how to use orch itself: to add tasks, send messages, and read or write the shared context. That is how agents
coordinate with each other. You can change the templates in the project settings
([Project settings](12-configuration-and-output.md#project-settings)).

A **run** is one attempt by one agent to do one task. A task that fails and retries has several runs.

Each run leaves a summary and an event log in `.orchestry/runs/`. A run's status is `preparing`, `running`, `succeeded`, `failed`, `timed_out` or `cancelled`.

Read runs with `orch logs`. You can filter by run ID, `--task`, `--agent` or `--since`, and `--follow` streams
new output. See [Running and monitoring](07-running-and-monitoring.md#view-logs).

## Autonomy

An **autonomous** agent finds its own work. Turn it on with `orch agent autonomous <id> --on` and off with
`--off`.

While the orchestrator runs, orch looks for agents that are autonomous, idle and have no open task assigned.
For each one, orch creates a task whose title starts with `[auto]`:

- If an `active` goal is assigned to the agent, the task is about that goal. Otherwise the task is about an
  `active` goal that has no assignee.
- If there is no such goal, the task is a general work cycle based on the agent's role.

orch waits at least 30 seconds before it creates another task for the same agent. These tasks are approved
automatically when they finish.

> **Warning:** An autonomous agent keeps creating work, and each run uses your AI provider's quota. Turn
> autonomy off when you do not need it, and pause or abandon goals you are finished with.

See [Agents](03-agents.md#autonomous-agents).

## The orchestrator

The **orchestrator** is orch's scheduling loop. It checks the project on a timer (every 10 seconds by default),
and again soon after a run ends. It starts ready tasks on idle agents, retries failures and stops stalled runs.
Only one orchestrator can run per project.

Different commands start the orchestrator in different ways:

| Command | What it does |
|---|---|
| `orch run <task-id>` | Starts one task, shows its output, and exits when the run ends. |
| `orch run --all` | Starts every `todo` task and exits when they finish. |
| `orch run --watch` | Keeps the orchestrator running in your terminal and prints what happens. `--verbose` adds agent output. |
| `orch serve` | Keeps the orchestrator running without a terminal UI and writes structured logs (JSON Lines by default). `--once` processes the `todo` tasks and exits. |
| `orch tui` | Opens the terminal dashboard and runs the orchestrator inside it. `--observe` opens the dashboard without starting the orchestrator. |
| `orch web` | Serves the dashboard to a browser. Each browser tab runs its own dashboard. |
| `orch` (no command) | Same as `orch tui`, with one difference: if the current directory has no `.orchestry/`, it first runs `orch init` there and then opens the dashboard (see the [warning about `init`](01-getting-started.md#create-a-project)). It checks only the current directory, not its parents, so in a subdirectory of a project it creates a nested project. |

The dashboard works as an **observer** when another process already holds the lock: it shows the project and
lets you change tasks and agents, but does not schedule work itself.

Stall detection only works while the orchestrator loop runs (`run --watch`, `serve` and the dashboard). A single
`orch run <task-id>` has no stall detection. `timeout_ms` in the agent and project settings is stored but orch
does not enforce it. See the [runbook](../RUNBOOK.md#timeout_ms-is-not-a-timeout).

> **Note:** `orch serve --once` waits until every task is `done`, `failed` or `cancelled`. A task in `review`
> keeps it waiting, so give agents you use with `--once` the policy `auto`.

See [Running and monitoring](07-running-and-monitoring.md) and [Dashboard](08-dashboard.md).

## Details for operators

You do not need this section to use orch.

- **Scheduling order.** On each pass the orchestrator checks running work and stops runs with no output for
  `stall_timeout_ms`, creates tasks for autonomous agents, then starts ready tasks. It starts tasks with the lowest
  priority number first, up to `scheduling.max_concurrent_agents` local runs (6 by default). Remote runs do not
  count toward that limit.
- **Lock file.** The running orchestrator holds `.orchestry/orchestry.lock`. A second orchestrator in the same
  project cannot start. See the [runbook](../RUNBOOK.md#corrupt-lock-file) if the lock is damaged.
- **Run files.** A run leaves `.orchestry/runs/<run-id>.json` (the summary: task, agent and outcome) and
  `<run-id>.jsonl` (the event log, to which orch only appends).
