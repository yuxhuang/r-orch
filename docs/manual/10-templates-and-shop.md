# Templates and the agent shop

Templates save you from describing every agent by hand. Read this chapter when you want to add a ready-made agent, deploy a whole team of agents in one command, or keep your own team definitions. The directory format and the precedence rules are in [docs/org-templates.md](../org-templates.md).

## In this chapter

- [Add one agent from the agent shop](#add-one-agent-from-the-agent-shop)
- [See the available organizations](#see-the-available-organizations)
- [Deploy an organization](#deploy-an-organization)
- [The a-team and what it needs](#the-a-team-and-what-it-needs)
- [Manage your own templates](#manage-your-own-templates)
- [Change members of a template](#change-members-of-a-template)
- [When something fails](#when-something-fails)

## Add one agent from the agent shop

The agent shop is a catalogue of 15 ready-made agent roles, such as Backend Developer, QA Engineer and Code Reviewer. Each one sets a name, model tier, role text and skills for you.

Before you start: run `orch init` in your project.

### List the catalogue

```sh
orch agent shop --list
```

```text
  Key                   Name                  Tier      Skills                                 
  backend-dev           Backend Developer     balanced  review, careful                        
  frontend-dev          Frontend Developer    balanced  design-review, review                  
  qa-engineer           QA Engineer           balanced  qa, testing-suite:generate-tests       
  code-reviewer         Code Reviewer         capable   review, careful                        
…
```

### Pick an agent in the terminal

1. Run `orch agent shop`.
2. Move with `Up` and `Down` (or `Ctrl+P` and `Ctrl+N`). The list wraps around.
3. Press `Enter` to install the highlighted template.
4. Check for a line such as `✓ Added agent QA Engineer (claude) → agt_…`.

To leave without installing, press `q`, `Esc` or `Ctrl+C`. orch prints `Cancelled.` and writes nothing.

The picker draws only when both input and output are a terminal. With output redirected, `orch agent shop` prints the same table as `--list`. With input that is not a terminal, it cancels at once. `--json` and `--quiet` do not change this command. If the picker looks wrong, see [Troubleshooting](13-troubleshooting.md#the-agent-shop-prints-cancelled-or-a-table).

### Pick an agent in the dashboard

In the dashboard, open the New Agent wizard and press `Ctrl+S`, or type `/agent shop`. Choose a template and review the filled-in steps before you create the agent. See [Install from the agent shop](08-dashboard.md#install-from-the-agent-shop).

![The Agent Shop picker opened from the New Agent wizard, listing fifteen numbered role templates with the first one selected](images/dashboard-agent-shop.png)

*The Agent Shop, opened with Ctrl+S in the New Agent wizard.*

### Shop from a template

Add `--template NAME` or `--template-dir PATH` to list and install the members of an [organization template](#manage-your-own-templates) instead of the built-in catalogue:

```sh
orch agent shop --template review-pair --list
```

```text
  NAME  DESCRIPTION  ADAPTER/MODEL  SKILLS  
  lead               claude                 
```

| Option | What it does |
|---|---|
| `--list` | Prints the table and exits. |
| `--template NAME` | Uses a template by name (`review-pair`, `builtin:a-team`, `user:review-pair`). |
| `--template-dir PATH` | Uses exactly this directory. |

An agent installed from a template has the same adapter, model, skills and rules that `deploy` would give it. If an agent with the same name already exists, the shop refuses (exit code 2) and writes nothing. The message goes to stderr and the picker may clear it, so check `orch agent list`.

A shop-installed agent is a copy. Changing the template later does not change it. See [Agents](03-agents.md) to edit it.

## See the available organizations

An organization is a set of agents, a team with a lead, and optionally a goal. orch includes ten presets and the a-team.

```sh
orch org list
```

```text
  KEY              NAME                 AGENTS  DESCRIPTION                                              
  startup-mvp      Startup MVP          6       Ship an MVP in 48 hours                                  
  pr-review-corp   PR Review Corp       5       Automated review for every PR                            
  …
  a-team           a-team               27      A specialist engineering team                            

  Deploy: orch org deploy <key> --goal "Your objective"
```

To see your own templates too, and any that are broken, use `orch org template list`:

```sh
orch org template list
```

```text
  NAME             SOURCE   STATUS  
  a-team           builtin  ok      
  bugfix-dept      legacy   ok      
  …
  review-pair      user     ok      
```

| Column | Meaning |
|---|---|
| `SOURCE` | `builtin` (the a-team), `legacy` (the ten presets) or `user` (yours). |
| `STATUS` | `ok`, `shadowed` (your template hides a built-in of the same name), or the reason an invalid template fails. |

## Deploy an organization

Before you start: run `orch init`, and install the agent CLIs the organization uses. `orch doctor` shows which are ready. See [Running and monitoring](07-running-and-monitoring.md).

> **Warning:** Deploying creates real agents that can spend money. With `--goal`, the goal is assigned to the team lead, who is then set to work on it. Once an orchestrator runs (the dashboard, `orch serve` or `orch run --watch`), the lead creates tasks and agents start working. In a test, the lead created and ran a task within a second of `orch serve` starting. Deploy without `--goal` if you want to look first.

1. Deploy a preset or template:

   ```sh
   orch org deploy startup-mvp
   ```

   ```text
     ✓ Deployed team "Startup MVP" — 6 agents

     ★ CTO (agt_IYYwqZc, lead)
     • Backend (agt_bvIYhBK, member)
     …
     Team: team_PPYdjLT

     Next: orch run --all --watch
   ```

   > **Warning:** The last line is a suggestion, not something that has happened. As in the warning above, running `orch run --all --watch` starts the deployed agents (for example `claude` and `codex`) on their tasks, and that is a paid run. Run it only when `orch doctor` shows the adapters ready and you intend to spend.

2. Check the result with `orch team list` and `orch agent list`. See [Teams and goals](05-teams-and-goals.md).

| Option | What it does |
|---|---|
| `KEY` or `--template NAME` | The organization to deploy. Do not use both. |
| `--template-dir PATH` | Deploys a directory you wrote. |
| `--adapter ADAPTER` | Uses this adapter for every member. |
| `--model MODEL` | Uses this model for every member. A model written for another adapter is dropped. |
| `--goal "TITLE"` | Creates a goal for the team, assigned to the lead. |

Deploying a template other than a legacy preset prints the `team`, `agentIds` and `goalId` (YAML by default; `--json` and `--yaml` choose). With `--quiet` it prints only the team ID.

`orch org template show NAME --resolved` prints what a deploy would create, member by member, without creating anything. Run it first.

Deploying does not roll back. If a step fails, the error lists the agents and team that already exist. See [When something fails](#when-something-fails).

## The a-team and what it needs

The a-team is a built-in template of 27 specialist roles (lead, architects, engineers, reviewers, QA, security and more) with the lead `orchestrator`. Fourteen members use the `codex` adapter and thirteen use `claude`. None pins a model.

```sh
orch org template show a-team --resolved    # all 27 roles
orch org deploy a-team --goal "Ship the release"
```

`deploy` does not check any of the following. Prepare them first:

- The `claude` and `codex` CLIs are installed and signed in. Run `orch doctor`.
- Your agent CLI provides the skills the roles name, such as `review`, `qa`, `ship` and `feature-dev:feature-dev`. orch does not install skills. A missing skill only shows when an agent tries to use it.
- A directory called `workspace` exists next to your project root (`../workspace`). Several roles write specs and reports there.
- Roles that refer to `<workspace root>` mean the directory that contains `.orchestry/`.
- The lead (`orchestrator`) creates git work trees under `<repo root>/.worktrees`, as its role tells it to.

> **Note:** Before orch 0.9.3, the a-team named a different sibling directory, and its lead said `.worktree`. A copy you made with `orch org template init --from a-team`, and agents you already deployed, keep that old wording. Rename your sibling directory to `workspace`, or edit the copy and the deployed agents.

To use the a-team in another layout, copy it and edit the copy (see below). The full list of skills is in [docs/org-templates.md](../org-templates.md#prerequisites).

The a-team cannot use `--adapter shell`, because shell members need a command.

## Manage your own templates

A template is a directory with an `org.yml`, one YAML file per member and optional shared rules. Your templates live in `~/.orchestry/templates/`. If you set `ORCHESTRY_GLOBAL_CONFIG_PATH`, they live in a `templates` folder next to that file.

The `orch org template` commands need no project, except that `show --resolved` uses the project defaults when there is one. Deploy and the shop need a project.

You choose a template by name, or by directory with `--template-dir PATH`. Use one of them, not both. A name is `KEY`, `builtin:KEY` or `user:KEY`. A plain `KEY` finds your template first, then the a-team, then a legacy preset.

| Command | What it does |
|---|---|
| `orch org template list` | Lists templates and their status. |
| `orch org template show [NAME] [--resolved]` | Prints a template. `--resolved` adds the exact agents a deploy creates. |
| `orch org template init NAME [--from SOURCE]` | Creates a template: minimal, or a copy of `SOURCE`. Never overwrites. |
| `orch org template edit [NAME]` | Opens `org.yml` in `$EDITOR` (then `$VISUAL`), checks it, then saves it. |
| `orch org template update [NAME] -f FILE` | Replaces `org.yml` from a complete JSON or YAML document. |
| `orch org template patch [NAME] -f FILE` | Merges a partial document into `org.yml`. |

All of them accept `--template-dir PATH`, `--json`, `--yaml` and `--quiet`. `-f` reads a file, or `-` reads standard input. Use `--input-format json|yaml` when the format cannot be guessed from the name.

### Make a copy to change

Built-in templates are read-only. Copy one, then change the copy.

1. Create the copy:

   ```sh
   orch org template init my-team --from builtin:a-team
   ```

   ```text
   name: my-team
   path: …/templates/my-team
   ```

2. Check that it appears as a `user` template with `orch org template list`.
3. Change it with the commands in this chapter.
4. Deploy it with `orch org deploy my-team`.

A new template made without `--from` has one member, a lead:

```sh
orch org template init review-pair
```

To change `org.yml` without an editor, write a small file and patch it:

```sh
echo '{"description":"A lead who plans and a reviewer who checks"}' > patch.json
orch org template patch review-pair -f patch.json
```

`update` needs the complete document; `patch` merges only the keys you give (RFC 7396). `null` in a patch removes a key. Both check the whole template first, then write one file in one step, so a refused change leaves the template untouched.

For the file format, rules, the order in which values win, and limits, see [docs/org-templates.md](../org-templates.md).

## Change members of a template

Members live in `agents/<name>.yml`. Name the template first, then the member, either as the second positional argument or with `--agent`.

| Command | What it does |
|---|---|
| `orch org template agent list [NAME]` | Lists the members as written. |
| `orch org template agent add [NAME] -f FILE` | Adds a member from a document. Refuses if it exists. |
| `orch org template agent edit [NAME] [AGENT]` | Edits a member in your editor. |
| `orch org template agent update [NAME] [AGENT] -f FILE` | Replaces a member from a complete document. |
| `orch org template agent patch [NAME] [AGENT] -f FILE` | Merges a partial document into a member. |
| `orch org template agent delete [NAME] [AGENT]` | Removes a member. You cannot delete the lead. |

These two commands are the same:

```sh
orch org template agent patch review-pair --agent lead -f patch.json
orch org template agent patch review-pair lead -f patch.json
```

For example, give the lead a description:

```sh
echo '{"description":"Plans the work"}' | orch org template agent patch review-pair --agent lead -f -
```

```text
name: lead
role: Coordinate the team and report outcomes.
description: Plans the work
rules: []
```

Agents you deployed earlier are snapshots. Changing a template never changes them. To apply a change, remove the agent and deploy or shop again.

## When something fails

| What you see | What to do |
|---|---|
| `Builtins are read-only; run orch org template init …` | Copy the template with `init --from`, then change the copy. |
| `Agent "<name>" already exists; no agents created` (exit 2) | Remove or rename the existing agent, or change the member name in a copy of the template. |
| `Template deployment failed at <stage> … Surviving entities` | Fix the cause, then remove what was created: `orch agent remove <id>` for each agent, `orch team disband <team>`, `orch goal delete <goal>`. |
| `<file>: <reason>` from `list`, `show` or `deploy` | Fix the named file. |
| `Timed out after 5 s locking …/.template.lock` | Another template command is running. Retry. Do not delete the file. |

Edge cases, such as redeploying after a failure, a team name that is already used, and repairing a broken template, are in the runbook: [Organization templates: failures and recovery](../RUNBOOK.md#organization-templates-failures-and-recovery).
