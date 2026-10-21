# Command reference

Every orch command, with what it does and the chapter that explains how to use it. Use this page to find the
right command; use the chapter for steps and examples. For every option of a command, run it with `--help`:

```sh
orch task add --help
```

> **Note:** Only `orch --help`, `orch --version`, `orch init`, `orch doctor`, `orch update` and the
> `orch org template` commands work outside a project. Every other command, even with `--help`, needs a project:
> run it in a directory where `orch init` has run, or in a subdirectory of one. Otherwise it prints
> `Not initialized` and exits with code 3.

How to read the usage column:

| Notation | Meaning |
|---|---|
| `<id>` | A value you must give, such as a task ID |
| `[OPTIONS]` | Optional flags, listed by `--help` |
| `[COMMAND]` | A subcommand, listed in the rows below |

## Options that work with every command

`-V, --version` works only on its own (`orch --version`); the others work with any command.

| Option | What it does |
|---|---|
| `--json` | Output as JSON |
| `--yaml` | Output as YAML |
| `--quiet` | Minimal output (IDs only) |
| `--no-color` | Disable colors |
| `--ascii` | ASCII-only output (no Unicode) |
| `-h, --help` | Print help |
| `-V, --version` | Print version |

See [Configuration and output](12-configuration-and-output.md) for how the output options behave.

## Commands

### orch init

Initialize .orchestry/ in the current directory. Explained in [Getting started](01-getting-started.md).

| Command | What it does | Usage |
|---|---|---|
| `orch init` | Initialize .orchestry/ in the current directory | `orch init [OPTIONS]` |

### orch agent

Manage agents. Explained in [Agents](03-agents.md).

| Command | What it does | Usage |
|---|---|---|
| `orch agent` | Manage agents | `orch agent [OPTIONS] [COMMAND]` |
| `orch agent add` | Add a new agent | `orch agent add [OPTIONS] --adapter <adapter> <name>` |
| `orch agent shop` | Browse and install pre-built agent templates. See [Templates and the agent shop](10-templates-and-shop.md). | `orch agent shop [OPTIONS]` |
| `orch agent list` | List all agents | `orch agent list [OPTIONS]` |
| `orch agent status` | Show agent details | `orch agent status [OPTIONS] <id>` |
| `orch agent edit` | Edit an agent in $EDITOR | `orch agent edit [OPTIONS] <id>` |
| `orch agent update` | Replace an authored definition from JSON or YAML | `orch agent update [OPTIONS] --file <FILE_OR_DASH> <id>` |
| `orch agent patch` | Apply an RFC 7396 merge patch from JSON or YAML | `orch agent patch [OPTIONS] --file <FILE_OR_DASH> <id>` |
| `orch agent remove` | Remove an agent | `orch agent remove [OPTIONS] <id>` |
| `orch agent disable` | Disable an agent | `orch agent disable [OPTIONS] <id>` |
| `orch agent enable` | Enable an agent | `orch agent enable [OPTIONS] <id>` |
| `orch agent autonomous` | Toggle autonomous mode for an agent | `orch agent autonomous [OPTIONS] <id>` |

### orch task

Manage tasks. Explained in [Tasks](04-tasks.md).

| Command | What it does | Usage |
|---|---|---|
| `orch task` | Manage tasks | `orch task [OPTIONS] [COMMAND]` |
| `orch task add` | Create a new task | `orch task add [OPTIONS] <title>` |
| `orch task list` | List all tasks | `orch task list [OPTIONS]` |
| `orch task show` | Show task details | `orch task show [OPTIONS] <id>` |
| `orch task edit` | Open task in $EDITOR to modify title, priority and description | `orch task edit [OPTIONS] <id>` |
| `orch task update` | Replace an authored definition from JSON or YAML | `orch task update [OPTIONS] --file <FILE_OR_DASH> <id>` |
| `orch task patch` | Apply an RFC 7396 merge patch from JSON or YAML | `orch task patch [OPTIONS] --file <FILE_OR_DASH> <id>` |
| `orch task assign` | Assign task to agent | `orch task assign [OPTIONS] <task-id> <agent-id>` |
| `orch task cancel` | Cancel a task | `orch task cancel [OPTIONS] <id>` |
| `orch task approve` | Approve a task in review | `orch task approve [OPTIONS] <id>` |
| `orch task reject` | Reject a task and send it back for rework | `orch task reject [OPTIONS] <id>` |
| `orch task retry` | Retry a failed task | `orch task retry [OPTIONS] <id>` |

### orch team

Manage agent teams. Explained in [Teams and goals](05-teams-and-goals.md).

| Command | What it does | Usage |
|---|---|---|
| `orch team` | Manage agent teams | `orch team [OPTIONS] [COMMAND]` |
| `orch team create` | Create a new team | `orch team create [OPTIONS] --lead <agent-id> <name>` |
| `orch team list` | List all teams | `orch team list [OPTIONS]` |
| `orch team show` | Show team details | `orch team show [OPTIONS] <id>` |
| `orch team join` | Add an agent to a team | `orch team join [OPTIONS] <team-id> <agent-id>` |
| `orch team leave` | Remove an agent from a team | `orch team leave [OPTIONS] <team-id> <agent-id>` |
| `orch team add-task` | Add a task to the team pool | `orch team add-task [OPTIONS] <team-id> <task-id>` |
| `orch team set-lead` | Transfer team lead to another member | `orch team set-lead [OPTIONS] <team-id> <agent-id>` |
| `orch team disband` | Disband a team | `orch team disband [OPTIONS] <id>` |

### orch goal

Manage goals. Explained in [Teams and goals](05-teams-and-goals.md).

| Command | What it does | Usage |
|---|---|---|
| `orch goal` | Manage goals | `orch goal [OPTIONS] [COMMAND]` |
| `orch goal add` | Create a new goal | `orch goal add [OPTIONS] <title>` |
| `orch goal list` | List all goals | `orch goal list [OPTIONS]` |
| `orch goal show` | Show goal details | `orch goal show [OPTIONS] <id>` |
| `orch goal status` | Change goal status (active, paused, achieved, abandoned) | `orch goal status [OPTIONS] <id> <status>` |
| `orch goal update` | Update goal fields | `orch goal update [OPTIONS] <id>` |
| `orch goal delete` | Delete a goal | `orch goal delete [OPTIONS] <id>` |

### orch msg

Inter-agent messaging. Explained in [Messages and context](06-messages-and-context.md).

| Command | What it does | Usage |
|---|---|---|
| `orch msg` | Inter-agent messaging | `orch msg [OPTIONS] [COMMAND]` |
| `orch msg send` | Send a direct message to an agent | `orch msg send [OPTIONS] <to-agent-id> <body>` |
| `orch msg broadcast` | Broadcast a message to all agents (or team members) | `orch msg broadcast [OPTIONS] <body>` |
| `orch msg inbox` | Show pending messages for an agent | `orch msg inbox [OPTIONS] <agent-id>` |
| `orch msg list` | List all messages | `orch msg list [OPTIONS]` |

### orch context

Shared context store for inter-agent data exchange. Explained in [Messages and context](06-messages-and-context.md).

| Command | What it does | Usage |
|---|---|---|
| `orch context` | Shared context store for inter-agent data exchange | `orch context [OPTIONS] [COMMAND]` |
| `orch context set` | Set a shared context entry | `orch context set [OPTIONS] <key> <value>` |
| `orch context get` | Get a shared context entry | `orch context get [OPTIONS] <key>` |
| `orch context list` | List all shared context entries | `orch context list [OPTIONS]` |
| `orch context delete` | Delete a shared context entry | `orch context delete [OPTIONS] <key>` |

### orch run

Run tasks. Explained in [Running and monitoring](07-running-and-monitoring.md).

| Command | What it does | Usage |
|---|---|---|
| `orch run` | Run tasks | `orch run [OPTIONS] [task-id]` |

### orch serve

Headless daemon mode — structured logs to stdout. Explained in [Running and monitoring](07-running-and-monitoring.md).

| Command | What it does | Usage |
|---|---|---|
| `orch serve` | Headless daemon mode — structured logs to stdout | `orch serve [OPTIONS]` |

### orch status

Show orchestrator status. Explained in [Running and monitoring](07-running-and-monitoring.md).

| Command | What it does | Usage |
|---|---|---|
| `orch status` | Show orchestrator status | `orch status [OPTIONS]` |

### orch logs

View run logs. Explained in [Running and monitoring](07-running-and-monitoring.md).

| Command | What it does | Usage |
|---|---|---|
| `orch logs` | View run logs | `orch logs [OPTIONS] [run-id]` |

### orch doctor

Check adapters and dependencies. Explained in [Running and monitoring](07-running-and-monitoring.md).

| Command | What it does | Usage |
|---|---|---|
| `orch doctor` | Check adapters and dependencies | `orch doctor [OPTIONS]` |

### orch update

Check for updates and install the latest version. Explained in [Running and monitoring](07-running-and-monitoring.md). In 0.9.3 it does not install anything; it prints how to upgrade. See [Getting started](01-getting-started.md).

| Command | What it does | Usage |
|---|---|---|
| `orch update` | Check for updates and install the latest version. In 0.9.3 it does not install anything; it prints how to upgrade. See [Getting started](01-getting-started.md). | `orch update [OPTIONS]` |

### orch tui

Launch interactive TUI dashboard. Explained in [The dashboard](08-dashboard.md).

| Command | What it does | Usage |
|---|---|---|
| `orch tui` | Launch interactive TUI dashboard | `orch tui [OPTIONS]` |

### orch web

Serve the TUI dashboard in a browser (xterm.js over a websocket). Explained in [The web dashboard](09-web-dashboard.md).

| Command | What it does | Usage |
|---|---|---|
| `orch web` | Serve the TUI dashboard in a browser (xterm.js over a websocket) | `orch web [OPTIONS]` |

### orch org

Pre-built AI companies — deploy a full department with one command. Explained in [Templates and the agent shop](10-templates-and-shop.md).

| Command | What it does | Usage |
|---|---|---|
| `orch org` | Pre-built AI companies — deploy a full department with one command | `orch org [OPTIONS] [COMMAND]` |
| `orch org list` | List available company templates | `orch org list [OPTIONS]` |
| `orch org template` | Manage organization templates: list, show, create and change them | `orch org template [OPTIONS] [COMMAND]` |
| `orch org template list` | Discover user, embedded and legacy templates, including invalid entries | `orch org template list [OPTIONS]` |
| `orch org template show` | Inspect an authoring bundle or its effective deployment inputs | `orch org template show [OPTIONS] [NAME]` |
| `orch org template init` | Create a new template (never overwrite a destination) | `orch org template init [OPTIONS] <NAME>` |
| `orch org template edit` | Edit org.yml on a temporary copy, then validate and publish | `orch org template edit [OPTIONS] [NAME]` |
| `orch org template update` | Replace org.yml from a complete JSON or YAML definition | `orch org template update [OPTIONS] --file <FILE_OR_DASH> [NAME]` |
| `orch org template patch` | Merge a partial JSON or YAML definition into org.yml | `orch org template patch [OPTIONS] --file <FILE_OR_DASH> [NAME]` |
| `orch org template agent` | Manage individual member definitions | `orch org template agent [OPTIONS] <COMMAND>` |
| `orch org template agent list` | List the members of a template as written | `orch org template agent list [OPTIONS] [NAME]` |
| `orch org template agent add` | Add a member to a template from a JSON or YAML definition | `orch org template agent add [OPTIONS] --file <FILE_OR_DASH> [NAME]` |
| `orch org template agent edit` | Edit one member's definition in $EDITOR | `orch org template agent edit [OPTIONS] [NAME] [AGENT]` |
| `orch org template agent update` | Replace one member's definition from JSON or YAML | `orch org template agent update [OPTIONS] --file <FILE_OR_DASH> [NAME] [AGENT]` |
| `orch org template agent patch` | Merge a partial JSON or YAML definition into one member | `orch org template agent patch [OPTIONS] --file <FILE_OR_DASH> [NAME] [AGENT]` |
| `orch org template agent delete` | Remove a member from a template (not the lead) | `orch org template agent delete [OPTIONS] [NAME] [AGENT]` |
| `orch org deploy` | Deploy a template: create its agents, a team and an optional goal | `orch org deploy [OPTIONS] [template]` |

### orch remote

Manage remote orch peers. Explained in [Remote agents](11-remote-agents.md).

| Command | What it does | Usage |
|---|---|---|
| `orch remote` | Manage remote orch peers | `orch remote [OPTIONS] [COMMAND]` |
| `orch remote add` | Register a remote orch peer; persists and reconnects until removed | `orch remote add [OPTIONS] <ALIAS> <ADDR>` |
| `orch remote remove` | Unregister a remote; its in-flight remote tasks fail with an unknown outcome | `orch remote remove [OPTIONS] <ALIAS>` |
| `orch remote list` | List registered remotes with their addresses and online state | `orch remote list [OPTIONS]` |
| `orch remote status` | Show the listen address, connected controller and each remote's state | `orch remote status [OPTIONS]` |
| `orch remote listen` | Save the incoming-connection preference for the next engine start. | `orch remote listen [OPTIONS] <STATE> [ADDR]` |

### orch config

Manage configuration. Explained in [Configuration and output](12-configuration-and-output.md).

| Command | What it does | Usage |
|---|---|---|
| `orch config` | Manage configuration | `orch config [OPTIONS] [COMMAND]` |
| `orch config get` | Get a config value (dot notation) | `orch config get [OPTIONS] <key>` |
| `orch config set` | Set a config value (dot notation) | `orch config set [OPTIONS] <key> <value>` |
| `orch config edit` | Open config.yml in $EDITOR | `orch config edit [OPTIONS]` |
| `orch config update` | Replace raw project configuration from JSON or YAML | `orch config update [OPTIONS] --file <FILE_OR_DASH>` |
| `orch config patch` | Apply an RFC 7396 merge patch to raw project configuration | `orch config patch [OPTIONS] --file <FILE_OR_DASH>` |
| `orch config global` | Manage global settings (~/.orchestry/global.yml) | `orch config global [OPTIONS] [COMMAND]` |
| `orch config global get` | Get a global config value | `orch config global get [OPTIONS] <key>` |
| `orch config global set` | Set a global config value | `orch config global set [OPTIONS] <key> <value>` |
| `orch config global show` | Show all global settings | `orch config global show [OPTIONS]` |

---

This page is generated from the `--help` output of orch 0.9.3. If it and `--help` ever disagree, `--help` is
right; see [About this manual](about-this-manual.md#keeping-the-manual-accurate).
