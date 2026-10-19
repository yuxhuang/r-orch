# Tasks

A task is a unit of work for an agent. Read this chapter to create tasks, find and change them, review the work agents hand back, and deal with failures. For the statuses a task moves through, see [Task lifecycle](02-concepts.md#task-lifecycle).

## In this chapter

- [Add a task](#add-a-task)
- [Set priority, dependencies and labels](#set-priority-dependencies-and-labels)
- [List and show tasks](#list-and-show-tasks)
- [Assign a task](#assign-a-task)
- [Change a task](#change-a-task)
- [Review a task: approve or reject](#review-a-task-approve-or-reject)
- [Add review checks to a task](#add-review-checks-to-a-task)
- [Cancel a task](#cancel-a-task)
- [Retry a failed task](#retry-a-failed-task)
- [How orch retries by itself](#how-orch-retries-by-itself)
- [See a task in the dashboard](#see-a-task-in-the-dashboard)

The examples use `shell` agents, so they run without an AI tool. Create one first, as shown in [Add an agent](03-agents.md#add-an-agent).

## Add a task

Run `orch task add` with a title. The task starts in `todo`.

```sh
orch task add "Build the report" --assignee builder -p 2 \
  -d "Write built.txt" --labels docs --max-attempts 2
```

```text
  ✓ Created tsk_lICHVKV "Build the report"
```

`tsk_lICHVKV` is the task ID. `--assignee` accepts an agent's ID or its exact name. Without `--assignee`, orch gives the task to any idle agent that fits ([Who runs a task](#who-runs-a-task)).

Starting a task is a separate step: `orch run <task-id>`, or the orchestrator in [Running and monitoring](07-running-and-monitoring.md).

> **Note:** In orch 0.8.0, `orch run <task-id>` prints its header line and silently does nothing when another `todo` task is assigned to the same agent. Cancel or reassign the other task, or use `orch run --all`. See [Run one task](07-running-and-monitoring.md#run-one-task). The examples in this chapter give each task its own agent for that reason.

### Options for `orch task add`

| Option | What it does | Default |
|---|---|---|
| `<title>` | The task's title. Required. | |
| `-d`, `--description <desc>` | The task description. The agent reads it. | Empty |
| `-e`, `--edit` | Opens `$EDITOR` so you can write the description. | |
| `-p`, `--priority <n>` | Priority from 1 to 4. A lower number runs first. | 3 |
| `--assignee <agent-id>` | The agent that must run the task. | None |
| `--depends-on <ids>` | Comma-separated IDs of tasks that must be `done` first. | None |
| `-l`, `--labels <labels>` | Comma-separated labels. | None |
| `--max-attempts <n>` | How many times orch tries the task before it fails. | 3 |
| `--workspace-mode <mode>` | `shared`, `worktree` or `isolated`. Overrides the agent's setting. See [Workspaces](02-concepts.md#workspaces). | The agent's mode |
| `--review-criteria <list>` | Comma-separated checks: `test_pass`, `typecheck`, `lint`. See [Add review checks to a task](#add-review-checks-to-a-task). | None |
| `--scope <patterns>` | Comma-separated file patterns the task works on, such as `src/auth/**,src/session/**`. | None |
| `--goal-id <goalId>` | Links the task to a goal. See [Goals](05-teams-and-goals.md#goals). | None |
| `--attach <paths>` | Comma-separated files to attach, such as screenshots or docs. orch copies each file into `.orchestry/attachments/<task id>/` and lists its name on the task. Relative paths are read from the current directory. | None |

orch checks the values before it saves anything:

```text
  ✕ Priority must be an integer between 1 and 4
  ✕ Unknown depends_on task ID(s): tsk_nope
  ✕ Unknown review criterion: "nope". Use test_pass, typecheck or lint.
  ✕ Attachment file not found: missing.png
```

Attachments must be regular files (symlinks to regular files are accepted) with distinct basenames. If any file is missing, is not a regular file, or repeats a basename, orch copies none of them and creates no task. Appending attachments to an existing task also rejects names already listed on the task or present in its attachment directory; choose a fresh name and retry. `task update` keeps the `attachments` field read-only.

```text
$ orch task add duplicate --attach first/same.txt,second/same.txt
  ✕ Duplicate attachment name: same.txt (first/same.txt, second/same.txt)

$ orch task add directory --attach directory
  ✕ Attachment is not a regular file: directory
```

> **Note:** In orch 0.8.0 and earlier, `orch task add --attach` accepted a path but did not save the attachment. Upgrade, or put the file's path in the description instead.

Two output options help in scripts. `--quiet` prints only the new ID, and `--json` or `--yaml` prints the whole task.

```sh
T=$(orch task add "Draft the notes" --assignee noter --quiet)
```

## Set priority, dependencies and labels

### Priority

Priority runs from 1 to 4. orch starts the task with the lowest number first. Tasks linked to a goal start before other tasks of the same priority.

### Dependencies

`--depends-on` makes a task wait. The task does not start until every task it depends on is `done`. If one of them fails, the waiting task fails too.

```sh
A=$(orch task add "Compile" --assignee builder --quiet)
orch task add "Package" --assignee packager --depends-on "$A"
```

The dependencies are set when you create the task. You cannot change them afterwards with `orch task update` or `orch task patch`.

### Labels and capabilities

Labels are free text for your own use, and `orch task list` does not filter by them. Two kinds have a meaning:

- A label that starts with `cap:` is a **capability** requirement. `--labels cap:linux` makes the task wait for an online agent whose CAPS column includes `linux`. The task list shows why it waits.

  ```text
  STATUS  PRI  TASK         AGENT  TIME  WAITING
  ○ todo  P3   Needs linux  —      —     no online agent with cap linux
  ```

  Capabilities are mostly useful with [remote agents](11-remote-agents.md#use-capability-labels).
- Labels that start with `remote-ctl:` or `exec:` are reserved for remote workers. orch rejects them in `update` and `patch`.

orch also matches labels to agents. For a task without an assignee, an agent scores higher when a label equals one of its skills or appears in its role text.

### Who runs a task

- With `--assignee`, only that agent can run the task. If the agent is busy, disabled or removed, the task waits in `todo`. orch never gives it to someone else.
- Without an assignee, orch picks an idle agent. It scores each agent: labels that equal the agent's skills count most, then labels found in the agent's role, then being idle, then the agent's success rate.

### File scope

`--scope` lists the files a task is expected to touch. orch does not start a task whose scope overlaps with a task that is already in progress. That keeps two agents from working on the same files at once.

## List and show tasks

```sh
orch task list
```

```text
  STATUS    PRI  TASK              AGENT        TIME  WAITING
  ◈ review  P3   Draft the notes   agt_NnBifRi  —
  ✓ done    P2   Build the report  agt_3cU5sQA  —
  ✕ failed  P3   Break on purpose  agt_4EBuHFv  —

  3 tasks · 1 review · 1 done
```

| Column | Meaning |
|---|---|
| STATUS | The task's status. |
| PRI | The priority, `P1` to `P4`. |
| TASK | The title. |
| AGENT | The assignee's ID. |
| TIME | How long the task has been running. |
| WAITING | Why a `todo` task cannot start yet, when orch can tell. For example, an offline remote or a missing `cap:` agent. |

| Option | What it does |
|---|---|
| `--status <status>` | Shows only tasks with this status: `todo`, `in_progress`, `review`, `retrying`, `done`, `failed` or `cancelled`. |

```sh
orch task list --status review
```

An unknown status is refused: `✕ Invalid --status: "bogus"`.

Show one task:

```sh
orch task show tsk_hn6KHCh
```

```text
  Draft the notes
  ══════════════════════════════════════════

  Status    ◈ review · attempt 1/3
  Priority  P3
  Agent     agt_NnBifRi
  Created   2026-10-18T23:05:32.635Z
```

A task that has run in a `worktree` workspace also shows the workspace `Path` and a `Result` section:

```text
  Worktree job
  ══════════════════════════════════════════

  Status    ✓ done · attempt 1/3
  Priority  P3
  Agent     agt_im5zLtJ
  Path      /private/tmp/orch-manual-cv5etr/proj/.orchestry/workspaces/tsk_h_coYPX
  Created   2026-10-18T23:29:39.936Z

  Result
  ──────────────────────────────────────────
  Branch:        orchestry/tsk_h_coYPX/worktree-job
  Files changed:
    • w.txt
```

The first block is trimmed. `show` also prints the labels, scope, workspace, review checks, your reject feedback, attachments, the description and, once an agent has finished, a Result section with the agent's summary and the files it changed. It does not print dependencies; use `orch task show <id> --json` to see `depends_on`.

## Assign a task

`orch task assign <task-id> <agent>` sets or changes the assignee. The agent can be an ID or an exact name.

```sh
orch task assign tsk_oRXeHt6 builder
```

```text
  ✓ Assigned tsk_oRXeHt6 → agt_3cU5sQA
```

An unknown agent is refused:

```text
  ✕ Unknown agent: "nobody". Use an agent ID (agt_xxx) or an exact agent name.
```

Use `assign` to move a task off a disabled or removed agent ([Stop an agent from taking work](03-agents.md#stop-an-agent-from-taking-work)).

## Change a task

You have three ways to change a task's title, description, priority and labels. You cannot change a task that is `in_progress` or that belongs to a remote worker.

| Command | Use it to |
|---|---|
| `orch task edit <id>` | Change the title, priority and description in your editor. |
| `orch task patch <id> -f <file>` | Change some fields from a JSON or YAML file or standard input. |
| `orch task update <id> -f <file>` | Replace the whole definition from a file. |

### Edit in your editor

1. Run `orch task edit <id>`. orch opens the task in `$EDITOR` (then `$VISUAL`, then `vi`).
2. Change `title:` or `priority:` between the two `---` lines, or the description below them.
3. Save and close the editor.

You see `✓ Updated tsk_oRXeHt6 "For nobody"`, or `No changes.` when nothing differs.

### Change fields from a file

`patch` changes only the fields you write. `null` removes a field, so this file raises the priority and removes every label:

```yaml
# task-patch.yaml
priority: 1
labels: null
```

```sh
orch task patch tsk_oRXeHt6 -f task-patch.yaml
```

```text
  ✓ Updated task tsk_oRXeHt6
```

`update` needs the title, description and priority, and clears optional fields you leave out:

```sh
echo '{"title":"For nobody","description":"Reassigned later","priority":2}' > upd.json
orch task update tsk_oRXeHt6 -f upd.json
```

| Option | What it does |
|---|---|
| `-f`, `--file <file>` | The file to read. Use `-` for standard input. Required. |
| `--input-format json` or `yaml` | Overrides the format orch detects. |

The writable fields are `title`, `description`, `priority` (1 to 4) and `labels`. You cannot change the status, assignee or dependencies this way; use the dedicated commands. The input rules and errors are in [docs/file-mutations.md](../file-mutations.md).

## Review a task: approve or reject

A task enters `review` when its agent finishes successfully and the agent's approval policy is not `auto`. It waits there for you. "Review" means a person looks at the work and decides. See [Review and approval](02-concepts.md#review-and-approval) for the policies.

Who approves:

- **You**, with `orch task approve` and `orch task reject`, or from the dashboard.
- **orch itself**, when the agent's policy is `auto` or the task is one that orch created for an autonomous agent. You see `done` and no `review`.
- **Automatic checks**, when the task has review criteria ([Add review checks to a task](#add-review-checks-to-a-task)).

A task also stays in `review` when orch cannot merge the agent's work back into your project. Look at the Result section of `orch task show` for the reason, fix the conflict, and then approve.

### Approve a task

1. List the tasks that wait for you:

   ```sh
   orch task list --status review
   ```

2. Look at the work with `orch task show <id>`.
3. Approve it:

   ```sh
   orch task approve tsk_hn6KHCh
   ```

   ```text
     ✓ Approved tsk_hn6KHCh
   ```

The task is now `done`. Tasks that depend on it can start.

You can only approve a task in `review`. Other statuses are refused:

```text
  ✕ Invalid transition for tsk_hn6KHCh: todo → done
```

> **Note:** `orch task approve` also releases a task that a remote worker holds with the label `awaiting-approval`. See [Approve work on the worker](11-remote-agents.md#approve-work-on-the-worker).

### Reject a task

Reject sends the work back to the agent with your feedback.

```sh
orch task reject tsk_hn6KHCh --reason "Add a summary line"
```

```text
  ✓ Rejected tsk_hn6KHCh → todo (reason: Add a summary line)
```

| Option | What it does |
|---|---|
| `-r`, `--reason <reason>` | Your feedback. orch adds it to the agent's next prompt. Without it, the agent gets no feedback. |

What happens next:

1. The task goes back to `todo` and its attempt count returns to 0. `orch task show` prints your text on a `Feedback` line.
2. The next time the orchestrator starts the task, the agent's prompt says that the task "was previously completed but **rejected** during review", and quotes your feedback. A `shell` agent that prints its prompt shows it:

   ```text
   19:06:15  ▸ This task was previously completed but **rejected** during review with the follo
   19:06:15  ▸ > Add a summary line
   ```

3. When the agent finishes again, the task returns to `review`, or to `done` if the policy is `auto`.

You can only reject a task in `review`.

## Add review checks to a task

Review criteria let orch check the work itself. Set them with `--review-criteria` when you create the task.

```sh
orch task add "Fix the login bug" --assignee reviewer --review-criteria typecheck,lint,test_pass
```

When the agent finishes, orch runs the checks in the task's workspace, fastest first, and stops at the first failure.

| Criterion | Command orch runs |
|---|---|
| `typecheck` | `npx tsc --noEmit` |
| `lint` | `npm run lint` |
| `test_pass` | `npm test` |

> **Note:** The commands are fixed and use npm. They suit a Node.js project. In another kind of project they fail, so do not set criteria there. Each check times out after 120 seconds.

- If all checks pass, the task becomes `done`.
- If one fails, the task stays in `review` with the results saved, and you decide. If the agent's policy is `auto`, orch still approves the task and logs that it did so.

## Cancel a task

```sh
orch task cancel tsk_OTg9w5a
```

```text
  ✓ Cancelled tsk_OTg9w5a
```

You can cancel a task in any status except `done`, `failed` and `cancelled`. If an agent is running the task, orch stops the run. A task that is already cancelled gives an error:

```text
  ✕ Invalid transition for tsk_PjBRFEn: cancelled → cancelled
```

Check that no process the agent started keeps running; see [Troubleshooting](13-troubleshooting.md).

## Retry a failed task

`orch task retry` puts a `failed` or `cancelled` task back in `todo` with a fresh set of attempts.

```sh
orch task retry tsk_wsk8eC3
```

```text
  ✓ Reset tsk_wsk8eC3 to todo
```

Then start it again ([Run tasks](07-running-and-monitoring.md#run-one-task-or-all-todo-tasks)). Read the reason it failed first:

```sh
orch logs --task tsk_wsk8eC3
```

```text
  Run run_sCSReHu · attempt 1 · failed
  19:07:21  ✕ disk full
  19:07:21  ✕ Shell command exited with code 3
```

Retry only works on `failed` and `cancelled` tasks. A task in `retrying` restarts by itself, and `done` is final:

```text
  ✕ Invalid transition for tsk_vik9wfB: done → todo
```

If a task fails because a task it depends on failed, retry the dependency first, then retry the task.

## How orch retries by itself

When a run fails, orch does not give up at once. A task has `max_attempts` attempts (3 unless you set `--max-attempts`).

1. The first failure moves the task to `retrying`, and orch waits before the next attempt: 10 seconds, then 20, then 40, up to 300 seconds. The project settings `scheduling.retry_base_delay_ms` and `scheduling.retry_max_delay_ms` control the delays ([Project settings](12-configuration-and-output.md#project-settings)).
2. The next attempt gets the previous error and output in its prompt.
3. After the last attempt the task is `failed`, and every task that depends on it fails as well.

A task with `--max-attempts 1` fails on its first error, as `Break on purpose` did above. The retry delays apply while the orchestrator runs (`orch run --watch`, `orch serve` or the dashboard). `orch task show` prints the attempt count as `attempt 2/3`.

## See a task in the dashboard

The task detail view shows the same information as `orch task show`, and an Activity feed of what the agent did. You can approve, reject, cancel and retry from it.

![A task opened below the task list, showing its Overview tab with status, assignee, priority and attempts, next to the Description and Activity tabs](images/dashboard-task-detail.png)

*A task's detail, opened with Enter.*

See [Dashboard](08-dashboard.md) for the keys.
