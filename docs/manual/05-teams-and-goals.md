# Teams and goals

Teams group agents. Goals give agents a long-lasting objective. Read this chapter to create and manage both, and to learn what each one changes in how orch hands out work. For the short version, see [Teams](02-concepts.md#teams) and [Goals](02-concepts.md#goals).

## In this chapter

- [Teams](#teams)
  - [What a team changes](#what-a-team-changes)
  - [Create a team](#create-a-team)
  - [List and show teams](#list-and-show-teams)
  - [Add and remove members](#add-and-remove-members)
  - [Change the lead](#change-the-lead)
  - [Add a task to a team's pool](#add-a-task-to-a-teams-pool)
  - [Disband a team](#disband-a-team)
- [Goals](#goals)
  - [What a goal changes](#what-a-goal-changes)
  - [Create a goal](#create-a-goal)
  - [List and show goals](#list-and-show-goals)
  - [Link tasks to a goal](#link-tasks-to-a-goal)
  - [Pause, resume and finish a goal](#pause-resume-and-finish-a-goal)
  - [Change a goal](#change-a-goal)
  - [Delete a goal](#delete-a-goal)

The examples use agent IDs. Print them with `orch agent list --quiet`.

## Teams

A team is a named group of agents with one lead.

### What a team changes

In orch 0.9.3 a team changes less than its name suggests. It does two things:

- It groups agents, so the dashboard can show them together.
- It limits a broadcast message to the team's members: `orch msg broadcast --team <team-id>` ([Send a message to many agents](06-messages-and-context.md#send-a-message-to-many-agents)).

In 0.9.3 the orchestrator does not use a team's task pool or its auto-claim setting, so a team never changes which agent gets a task. Assign work with `orch task assign` ([Assign a task](04-tasks.md#assign-a-task)).

| To | Run |
|---|---|
| Create a team | `orch team create <name> --lead <agent-id>` |
| See teams | `orch team list`, `orch team show <team-id>` |
| Add or remove a member | `orch team join <team-id> <agent-id>`, `orch team leave <team-id> <agent-id>` |
| Change the lead | `orch team set-lead <team-id> <agent-id>` |
| Put a task in the pool | `orch team add-task <team-id> <task-id>` |
| End a team | `orch team disband <team-id>` |

### Create a team

A team needs a name and a lead. The lead must be an existing agent, given by ID.

```sh
orch team create dev --lead agt_3cU5sQA --members agt_NnBifRi -d "Delivery team"
```

```text
  ✓ Created team "dev" → team_XWLwLTn
```

| Option | What it does | Default |
|---|---|---|
| `<name>` | The team's name. Required. | |
| `--lead <agent-id>` | The lead agent's ID. Required. The lead is also a member. | |
| `--members <ids>` | Comma-separated IDs of more members. | None |
| `-d`, `--description <desc>` | A description. | None |
| `--no-auto-claim` | Sets the team's `auto_claim` to false. | `auto_claim` is true |

An unknown lead is refused. A name instead of an ID is an unknown lead:

```text
  ✕ Lead agent not found: builder
```

### List and show teams

Teams are identified by ID, not by name.

```sh
orch team list
```

```text
  ID            NAME  STATUS  LEAD         MEMBERS  POOL
  team_XWLwLTn  dev   active  agt_3cU5sQA  2        0
```

```sh
orch team show team_XWLwLTn
```

```text
  ID          team_XWLwLTn
  Name        dev
  Status      active
  Lead        agt_3cU5sQA
  Members     agt_3cU5sQA (lead), agt_NnBifRi (member)
  Pool        empty
  Auto-claim  true
  Created     2026-10-18T23:08:37.357Z
```

`POOL` counts the tasks in the team's pool. `orch team list --json` prints every field, including `config.message_ttl_ms`.

`orch team show dev` fails with `✕ Team not found: dev`; use the ID.

### Add and remove members

```sh
orch team join team_XWLwLTn agt_4EBuHFv
```

```text
  ✓ Agent agt_4EBuHFv joined team team_XWLwLTn
```

```sh
orch team leave team_XWLwLTn agt_4EBuHFv
```

```text
  ✓ Agent agt_4EBuHFv left team team_XWLwLTn
```

An agent can belong to several teams. The lead cannot leave:

```text
  ✕ Lead cannot leave team. Disband the team or transfer lead first.
```

### Change the lead

The new lead must already be a member. The old lead stays as a member.

```sh
orch team set-lead team_XWLwLTn agt_NnBifRi
```

```text
  ✓ agt_NnBifRi is now lead of team team_XWLwLTn
```

### Add a task to a team's pool

The pool is a list of task IDs for you and your tools to read. orch does not act on it.

```sh
orch team add-task team_XWLwLTn tsk_w2NQab2
```

```text
  ✓ Task tsk_w2NQab2 added to team team_XWLwLTn pool
```

Adding the same task twice changes nothing. `orch team show` then lists the task under `Pool`.

### Disband a team

```sh
orch team disband team_XWLwLTn
```

The team's status becomes `disbanded`. orch keeps the record, and its agents and tasks are not changed. A disbanded team no longer counts as the agent's team.

## Goals

A goal is a lasting objective, such as "Ship the release". It has a title, an optional description, an optional assignee and a status.

### What a goal changes

A goal changes work in three ways:

1. **It drives autonomous agents.** When the orchestrator runs, an idle autonomous agent with no open task gets a new task about an `active` goal. A goal assigned to the agent comes first, then a goal with no assignee. See [Autonomous agents](03-agents.md#autonomous-agents).
2. **It turns on autonomous mode.** Creating a goal with `--assignee` makes that agent autonomous. Pausing, finishing or deleting the goal turns it off again when the agent has no other `active` goal.
3. **It orders and informs tasks.** Among tasks of the same priority, tasks linked to a goal start first. An agent that runs a linked task sees the goal in its prompt.

A goal never starts work when no orchestrator runs. A goal without an assignee does nothing until some autonomous agent picks it up.

### Create a goal

```sh
orch goal add "Ship v1 notes" --description "Release notes written and reviewed" --assignee agt_NnBifRi
```

```text
  ✓ Created goal "Ship v1 notes" (goal_2aXRjKK)

  Tips for better results:
  • Be specific: "Implement OAuth2 with Google" > "Add auth"
  • Add --description with success criteria and constraints
  • Use --assignee to focus a specific agent on this goal
```

| Option | What it does |
|---|---|
| `<title>` | The goal's title. Required. |
| `--description <desc>` | What done looks like. Agents read it, so write the success criteria. |
| `--assignee <agentId>` | The agent that pursues this goal. This turns on its autonomous mode. |

A new goal is `active`.

An autonomous agent keeps creating tasks while an `active` goal exists. See [Autonomy](02-concepts.md#autonomy).

### List and show goals

```sh
orch goal list
```

```text
     ID            Title          Status  Assignee
  ●  goal_2aXRjKK  Ship v1 notes  active  agt_NnBifRi
```

| Option | What it does |
|---|---|
| `--status <status>` | Shows goals with this status: `active`, `paused`, `achieved` or `abandoned`. |

`orch goal ls` is a short form of `orch goal list`.

```sh
orch goal show goal_2aXRjKK
```

```text
  ID           goal_2aXRjKK
  Title        Ship v1 notes
  Status       active
  Assignee     agt_NnBifRi
  Description  Release notes written and reviewed
  Created      2026-10-18T23:08:57.440Z
  Updated      2026-10-18T23:08:57.440Z

  No tasks linked to this goal yet.
```

Once tasks are linked, `show` lists them with their status.

### Link tasks to a goal

Pass the goal's ID when you create a task:

```sh
orch task add "Goal task" --goal-id goal_2aXRjKK
```

`orch goal show` then lists it:

```text
  Tasks (1)
  ──────────────────────────────────────────
  STATUS  ID           TITLE      AGENT
  ○ todo  tsk_Ku7IQuR  Goal task  —
```

### Pause, resume and finish a goal

`orch goal status <id> <status>` changes the status.

| Status | Meaning | Side effect |
|---|---|---|
| `active` | Agents work toward it. | From `paused`, it turns the assignee's autonomous mode back on. |
| `paused` | On hold. | Turns off the assignee's autonomous mode and cancels the assignee's waiting autonomous tasks. |
| `achieved` | Done. Final. | Turns off the assignee's autonomous mode. |
| `abandoned` | Dropped. Final. | Turns off the assignee's autonomous mode. |

```sh
orch goal status goal_2aXRjKK paused
```

```text
  ✓ Goal "Ship v1 notes" → paused
```

You cannot mark a goal `achieved` while linked tasks are still open. orch tells you which:

```text
  ✕ Cannot mark goal goal_2aXRjKK as achieved: 1 task(s) still pending — tsk_Ku7IQuR (todo)
    Use --force to cancel pending tasks and mark achieved
```

| Option | What it does |
|---|---|
| `--force` | Cancels the pending linked tasks and marks the goal `achieved`. |

```sh
orch goal status goal_2aXRjKK achieved --force
```

```text
  ✓ Goal "Ship v1 notes" → achieved
```

> **Warning:** `--force` cancels every pending task linked to the goal. Check with `orch goal show` first.

`achieved` and `abandoned` are final: a goal in either status cannot return to `active`.

### Change a goal

```sh
orch goal update goal_2aXRjKK --title "Ship v1 release notes" --assignee ""
```

```text
  ✓ Updated goal "Ship v1 release notes"
```

| Option | What it does |
|---|---|
| `--title <title>` | The new title. It cannot be empty. |
| `--description <desc>` | The new description. |
| `--assignee <agentId>` | The new assignee. An empty string (`""`) removes the assignee. |

A new assignee becomes autonomous. The old assignee stops being autonomous unless another `active` goal needs it. `orch goal show` prints `any` for a goal with no assignee.

### Delete a goal

```sh
orch goal delete goal_2aXRjKK
```

```text
  ✓ Deleted goal goal_2aXRjKK
```

`orch goal rm` is a short form. Deleting does not delete the linked tasks. They keep the old goal ID. The assignee stops being autonomous unless another `active` goal needs it.

## Where to go next

- [Autonomous agents](03-agents.md#autonomous-agents) explains how agents pick up goal work.
- [Tasks](04-tasks.md) covers the tasks you link to a goal.
- Organization templates can create a team and a goal in one step ([Templates and the agent shop](10-templates-and-shop.md#deploy-an-organization)).
