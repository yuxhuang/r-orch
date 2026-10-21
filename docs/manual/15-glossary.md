# Glossary

Each entry says what a term means in orch and links to the section that explains it. Terms are in alphabetical
order. Use it when a word in another chapter is unfamiliar.


## A

**adapter**
The AI command-line tool an agent uses, such as `claude` or `codex`, or `shell` for any command you choose.
See [Agents and adapters](02-concepts.md#agents-and-adapters) and [Adapters](01-getting-started.md#adapters).

**agent**
A worker that orch can start on a task. It has a name, an ID, an adapter and settings such as its model and
approval policy. See [Agents and adapters](02-concepts.md#agents-and-adapters).

**agent shop**
A built-in catalogue of ready-made agent templates that you install one at a time. See
[Templates and shop](10-templates-and-shop.md).

**approval policy**
The agent setting (`suggest`, `auto` or `manual`) that decides whether a finished task waits for your approval.
See [Review and approval](02-concepts.md#review-and-approval).

**attachment**
A file you attach to a task with `orch task add --attach`. orch copies it into
`.orchestry/attachments/<task id>/`. See [Options for `orch task add`](04-tasks.md#options-for-orch-task-add).

**attempt**
One try at a task. A task has an attempt limit (3 by default). When a task uses all its attempts, it becomes
`failed`. See [Task lifecycle](02-concepts.md#task-lifecycle).

**autonomous agent**
An agent that orch gives new tasks automatically when it is idle, based on active goals or its role. See
[Autonomy](02-concepts.md#autonomy).

## B

**blocked**
The state of a task that waits because a task it depends on is not `done`. See [Tasks](02-concepts.md#tasks).

**broadcast**
A message sent to all agents, or to the members of one team. See [Messages](02-concepts.md#messages).

## C

**cancelled**
The task status after you cancel a task. You can retry it. See [Task lifecycle](02-concepts.md#task-lifecycle).

**context**
A shared key-value store that agents and you read and write with `orch context`. See
[Context](02-concepts.md#context).

**controller**
The machine that sends tasks to remote workers. See [Remote agents](11-remote-agents.md).

## D

**dashboard**
The terminal interface you open with `orch tui`. See [Dashboard](08-dashboard.md).

**dependency**
A task that must be `done` before another task can start. You set it with `--depends-on`. See
[Tasks](02-concepts.md#tasks).

**disabled**
An agent status. A disabled agent gets no work until you enable it. See
[Agents and adapters](02-concepts.md#agents-and-adapters).

**done**
The task status after approval. It is final. See [Task lifecycle](02-concepts.md#task-lifecycle).

## E

**Elapsed\***
A dashboard column on the Goals and Tasks lists: the time from the start to now, or to the last update once the
row is terminal. It includes waiting and retries. See [Sort the lists](08-dashboard.md#sort-the-lists).

## F

**failed**
The task status after a task has used all its attempts. You can retry it. See
[Task lifecycle](02-concepts.md#task-lifecycle).

**file scope**
A list of glob patterns on a task. orch does not run two tasks with overlapping scopes at the same time. See
[Tasks](02-concepts.md#tasks).

## G

**global settings**
Preferences for the dashboard that apply to every project, stored in `~/.orchestry/global.yml`. See
[Global settings](12-configuration-and-output.md#global-settings).

**goal**
A lasting objective, with the status `active`, `paused`, `achieved` or `abandoned`. Goals drive autonomous
agents. See [Goals](02-concepts.md#goals).

**grouping**
How the dashboard cuts a list into sections. Press `b` to group the Agents table by team, node, status, adapter
or capability, or not at all; press `G` to group tasks by goal. See [Group the table](08-dashboard.md#group-the-table).

## I

**idle**
An agent status. An idle agent is ready for work. See
[Agents and adapters](02-concepts.md#agents-and-adapters).

**in_progress**
The task status while an agent works on the task. See [Task lifecycle](02-concepts.md#task-lifecycle).

**isolated**
A workspace mode that gives the agent a separate copy of the project, made with `git clone`, and does not merge
it back. See [Workspaces](02-concepts.md#workspaces).

## K

**keyword filter**
A dashboard prompt, opened with `Ctrl+K`, that narrows the Goals, Tasks or Agents list to the rows that match
what you type. See [Filter a list by keyword](08-dashboard.md#filter-a-list-by-keyword).

## L

**label**
A tag on a task. orch uses labels to match an unassigned task to an agent's skills and role. See
[Tasks](02-concepts.md#tasks).

**lead**
The agent that heads a team. See [Teams](02-concepts.md#teams).

**loopback**
The address `127.0.0.1`, which only accepts connections from the same machine. orch listens on it by default.
See [Remote agents](11-remote-agents.md).

## M

**merge patch**
A JSON Merge Patch (RFC 7396): a standard way to describe a partial update. `patch` merges one into a definition. See
[Update definitions from files](12-configuration-and-output.md#update-definitions-from-files).

**message**
A note sent to an agent. The agent reads it at the start of its next run. See
[Messages](02-concepts.md#messages).

**mirror**
A copy of a controller's task on a worker. The worker runs the mirror and reports the outcome back. See
[Remote agents](11-remote-agents.md).

## O

**observer**
A dashboard that shows a project without running its orchestrator, because another process already does. See
[The orchestrator](02-concepts.md#the-orchestrator).

**orchestrator**
orch's scheduling loop. It starts ready tasks on idle agents, retries failures and stops stalled runs. Some other documents call it the "engine". See
[The orchestrator](02-concepts.md#the-orchestrator).

**OSC 52**
A terminal code that asks your terminal to copy text to the clipboard. The dashboard uses it to copy over SSH or
tmux; the terminal cannot confirm the copy. See [Dashboard](08-dashboard.md).

## P

**peer**
Another machine running orch that your installation connects to. Also called a remote. See
[Remote agents](11-remote-agents.md).

**poll interval**
How often the orchestrator checks the project (`scheduling.poll_interval_ms`, 10 seconds by default). See
[Project settings](12-configuration-and-output.md#project-settings).

**priority**
A task's urgency, from 1 to 4. orch starts lower numbers first. The default is 3. See
[Tasks](02-concepts.md#tasks).

**project**
A directory that contains a `.orchestry/` folder. See [Project](02-concepts.md#project).

**project settings**
The settings in `.orchestry/config.yml`. See
[Project settings](12-configuration-and-output.md#project-settings).

## Q

**quiet**
The `--quiet` option, which prints only IDs. See
[Choose the output format](12-configuration-and-output.md#choose-the-output-format).

## R

**remote**
Another machine running orch whose agents your project can use. Also called a peer. See
[Remote agents](11-remote-agents.md).

**retrying**
The task status after a failed attempt when attempts remain. The orchestrator starts it again after a delay. See
[Task lifecycle](02-concepts.md#task-lifecycle).

**review**
The task status when an agent has finished and the work awaits approval. See
[Review and approval](02-concepts.md#review-and-approval).

**review criteria**
Automatic checks on a task: `test_pass`, `typecheck` and `lint`. See [Tasks](02-concepts.md#tasks).

**run**
One attempt by one agent to do one task. Each run leaves a record and an event log. See
[Runs and logs](02-concepts.md#runs-and-logs).

## S

**shared**
A workspace mode in which the agent works in your project directory itself. See
[Workspaces](02-concepts.md#workspaces).

**stall timeout**
How long a run may produce no output before orch stops it (`stall_timeout_ms`, 10 minutes by default). See
[Project settings](12-configuration-and-output.md#project-settings).

## T

**task**
One piece of work for an agent, with a status, a priority and an attempt limit. See
[Tasks](02-concepts.md#tasks).

**task pool**
A list of tasks stored on a team. The orchestrator does not start tasks from it. See
[Teams](02-concepts.md#teams).

**team**
A named group of agents with one lead. See [Teams](02-concepts.md#teams).

**template**
A directory that deploys agents, a team and an optional goal in one command (`orch org deploy`). Also the
ready-made agent definitions in the agent shop. See [Templates and shop](10-templates-and-shop.md).

**terminal status**
A task status that orch does not start by itself: `done`, `failed` or `cancelled`. See
[Task lifecycle](02-concepts.md#task-lifecycle).

**todo**
The task status of a task that has not started. See [Task lifecycle](02-concepts.md#task-lifecycle).

## U

**UPDATED**
A column in the dashboard's Agents table: how long ago the agent's live run (`R`) or a task assigned to it (`T`)
last changed. It does not say when the agent last did anything. See [What UPDATED means](08-dashboard.md#what-updated-means).

## W

**web dashboard**
The dashboard in a browser, which you start with `orch web`. See [Web dashboard](09-web-dashboard.md).

**worker**
A remote machine that runs tasks sent by a controller. See [Remote agents](11-remote-agents.md).

**workspace**
The folder an agent works in for a task. See [Workspaces](02-concepts.md#workspaces).

**workspace mode**
The setting that chooses the kind of workspace: `worktree`, `isolated` or `shared`. See
[Workspaces](02-concepts.md#workspaces).

**worktree**
The default workspace mode: a separate git worktree on its own branch, merged back when the run succeeds. See
[Workspaces](02-concepts.md#workspaces).
