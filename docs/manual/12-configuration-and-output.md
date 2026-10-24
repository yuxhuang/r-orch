# Configuration and output

After this chapter you can read and change orch's settings, choose how commands print their results, and replace
or patch agent, task and project definitions from JSON or YAML files. It is for people who tune a project or
script orch.

## In this chapter

- [Where settings live](#where-settings-live)
- [Read and change project settings](#read-and-change-project-settings)
- [Project settings](#project-settings)
- [Global settings](#global-settings)
- [Choose the output format](#choose-the-output-format)
- [Update definitions from files](#update-definitions-from-files)

## Where settings live

orch has two sets of settings:

| Settings | File | Applies to | Command |
|---|---|---|---|
| Project | `.orchestry/config.yml` | One project | `orch config` |
| Global | `~/.orchestry/global.yml` | Every project for your user | `orch config global` |

Set the environment variable `ORCHESTRY_GLOBAL_CONFIG_PATH` to use another file in place of
`~/.orchestry/global.yml`. That variable moves `global.yml` only. orch keeps other per-user files, such as its
update-check files, under `HOME`. To try orch without touching your real settings, set both variables.

## Read and change project settings

Project settings use dot notation. `scheduling.max_concurrent_agents` means the key `max_concurrent_agents`
inside `scheduling`.

| Command | What it does |
|---|---|
| `orch config get <key>` | Prints one value. A key that holds a group, such as `project`, prints the whole group. |
| `orch config set <key> <value>` | Sets one value and saves it. |
| `orch config edit` | Opens `config.yml` in your `$EDITOR`. |
| `orch config update -f <file>` | Replaces the whole configuration from a JSON or YAML file. |
| `orch config patch -f <file>` | Merges a JSON or YAML document into the configuration. |

To see the whole project configuration, print the file:

```sh
cat .orchestry/config.yml
```

To change it by hand, run `orch config edit`. `orch config get` needs a key.

### Get a value

```sh
orch config get scheduling.max_concurrent_agents
```

```text
  scheduling.max_concurrent_agents = 6
```

A key that does not exist prints `undefined`:

```sh
orch config get nope.x
```

```text
  nope.x = undefined
```

With `--json` it prints only the key:

```sh
orch --json config get nope.x
```

```text
{"key":"nope.x"}
```

### Set a value

```sh
orch config set scheduling.max_concurrent_agents 2
```

```text
  ✓ scheduling.max_concurrent_agents = 2
```

`config set` reads the value as JSON when it can: `2` is a number, `true` is a boolean, and `"text"` is a string.
Anything that is not valid JSON, such as `nope`, is saved as a string. It creates missing groups and keeps keys
orch does not know.

> **Warning:** `config set` does not check that a value makes sense. It accepted `0` for
> `scheduling.max_concurrent_agents`, which is not a valid limit. To have orch validate your change, use
> `orch config patch`, which rejects it:
>
> ```sh
> echo '{"scheduling": {"max_concurrent_agents": 0}}' | orch config patch -f -
> ```
>
> ```text
>   ✕ <stdin>: config: runtime limits and concurrency must be positive
> ```

orch reads `config.yml` when it starts. To be sure a running orchestrator uses a changed value, stop and start it
again.

### Replace or patch from a file

See [Update definitions from files](#update-definitions-from-files).

## Project settings

`orch init` writes `config.yml` with these values. Omit a key and orch uses the default shown.

| Setting | What it does | Default |
|---|---|---|
| `project.name` | The project's name. | The directory name, or `--name` on `orch init` |
| `project.description` | A short description. Optional. | None |
| `defaults.agent.adapter` | The adapter for agents added without one, and for agent-shop templates that do not name one. | `claude`, or `--adapter` on `orch init` |
| `defaults.agent.approval_policy` | The approval policy a new agent gets: `suggest`, `auto` or `manual`. | `auto` |
| `defaults.agent.max_turns` | The turn limit a new agent gets. | `50` |
| `defaults.agent.timeout_ms` | The timeout a new agent gets, in milliseconds. orch stores it but does not enforce it. | `3600000` |
| `defaults.agent.stall_timeout_ms` | How long a run may produce no output before orch stops it, in milliseconds. Applies while the orchestrator loop runs. | `600000` |
| `defaults.agent.workspace_mode` | The workspace mode when neither the task nor the agent sets one: `shared`, `worktree` or `isolated`. | `worktree` |
| `defaults.agent.prompt_mode` | The prompt mode for agents that have no `config.prompt_mode` of their own: `normal` or `eco`. Read again at every run. See [Eco mode](16-eco-mode-and-effort.md). | `normal`, or `--prompt-mode` on `orch init` |
| `defaults.task.max_attempts` | The attempt limit a new task gets. | `3` |
| `defaults.task.priority` | Stored, but not used in orch 0.9.3: `orch task add` gives a new task priority 3 unless you pass `-p`. | `3` |
| `scheduling.poll_interval_ms` | How often the orchestrator checks the project, in milliseconds. | `10000` |
| `scheduling.max_concurrent_agents` | How many local runs may go at the same time. Remote runs do not count. | `6` |
| `scheduling.retry_base_delay_ms` | The wait before the first retry of a failed task. Each later retry doubles it. | `10000` |
| `scheduling.retry_max_delay_ms` | The longest wait between retries. | `300000` |
| `prompt.template` | Optional. The text of one prompt template for the whole prompt. When set, `system_template` and `user_template` are ignored. None of the three can be set while any agent uses eco mode ([why](16-eco-mode-and-effort.md#eco-mode-and-custom-templates)). | Built-in prompt |
| `prompt.system_template` | Optional. The text of the system prompt template. | Built-in prompt |
| `prompt.user_template` | Optional. The text of the task prompt template. | Built-in prompt |
| `peer.name` | The name this installation announces to other machines. See [Remote agents](11-remote-agents.md). | The short host name |
| `peer.listen` | The address to accept connections on, saved by `orch remote listen on`. See [Remote agents](11-remote-agents.md). | Not set (not listening) |

Things to know:

- **Defaults apply when you create something.** `orch agent add` copies `approval_policy`, `max_turns`,
  `timeout_ms` and `stall_timeout_ms` into the new agent's file. `orch task add` copies `max_attempts`
  into the new task. Changing a default later does not change existing agents and tasks. Change
  those with [`orch agent edit`](03-agents.md) or [`orch task edit`](04-tasks.md).
- **`workspace_mode` is read when a run starts.** A task's own mode wins, then the agent's, then this default.
- **`prompt_mode` is read when a run starts, too.** An agent's own `config.prompt_mode` wins over this default,
  and `orch agent add` does not copy the default into the agent. A change reaches the next run in every orch
  process on the project.
- **Retry delays double.** With the defaults, retries wait 10, 20, 40, 80, 160 seconds, then 300 seconds each.
- **Prompt templates are advanced.** Leave `prompt` unset unless you need to change what agents are told.
- **`orch init` can change three defaults.** With `--adapter`, it sets `defaults.agent.adapter`; with
  `--prompt-mode`, `defaults.agent.prompt_mode`. If git is not available, it sets
  `defaults.agent.workspace_mode` to `shared`. A project made by an older orch gets `prompt_mode: normal`
  written into `config.yml` by the first `orch config set`.
- **Unknown keys are kept.** You can add your own keys to `config.yml`. orch leaves them alone.

The dashboard can also change `scheduling.max_concurrent_agents`. See [Dashboard](08-dashboard.md).

> **Warning:** `config.yml` can hold adapter commands and environment names. orch writes it so that only your
> user can read it. Do not put API keys in it. See the [runbook](../RUNBOOK.md#secrets-in-agent-config).

## Global settings

Global settings are your dashboard preferences. They are stored in `~/.orchestry/global.yml`.

| Command | What it does |
|---|---|
| `orch config global show` | Prints all global settings. |
| `orch config global get <key>` | Prints one setting. |
| `orch config global set <key> <value>` | Sets one setting. |

```sh
orch config global show
```

```text
  tui.activity_filter = all
  tui.palette = amber
```

`get` and `set` accept two keys. Write them without the `tui.` prefix.

| Key | What it does | Values | Default |
|---|---|---|---|
| `palette` | The dashboard's colours. | `amber`, `ocean`, `forest`, `violet`, `light`, `catppuccin_latte`, `catppuccin_mocha`, `dracula`, `gruvbox_dark`, `nord`, `one_dark`, `solarized_dark`, `solarized_light`, `tokyo_night` | `amber` |
| `activity_filter` | Which entries the dashboard's activity feed shows. | `all`, `text`, `tools`, `errors`, `events` | `all` |

```sh
orch config global set palette ocean
```

```text
  ✓ palette = ocean
```

```sh
orch config global get palette
```

```text
  palette = "ocean"
```

`set` ignores capital letters in a palette name. Other keys, such as `tui.palette`, are not accepted.

`light`, `catppuccin_latte` and `solarized_light` suit a light terminal background; the others suit a dark one.
The palettes from `catppuccin_latte` on are adapted from the Catppuccin, Dracula, Gruvbox, Nord, One Dark,
Solarized and Tokyo Night themes. An orch older than the release that added them reads an unknown palette name as
`amber`, and writes `amber` back the next time it saves a global setting (such as the activity filter).

> **Note:** `orch config global set` exits with code 0 even when it rejects your input. It prints an error
> instead, for example `✕ Invalid value "pink". Valid: amber, ocean, forest, violet, light, catppuccin_latte, …` or
> `✕ Unknown global config key: foo`. In a script, run `orch config global get <key>` afterwards to check.

`global.yml` also holds `tui.notifications.toast` (default `true`) and `tui.notifications.bell` (default
`false`). The `orch config global` commands do not change them. Change them in the dashboard or edit the file.
The dashboard can also change the palette and the activity filter. See [Dashboard](08-dashboard.md).

## Choose the output format

These flags work on every command, before or after the subcommand. `orch --json task list` and
`orch task list --json` are the same.

| Option | What it does |
|---|---|
| `--json` | Prints the result as JSON. |
| `--yaml` | Prints the result as YAML. |
| `--quiet` | Prints only IDs. |
| `--no-color` | Turns colours off. |
| `--ascii` | Uses plain ASCII characters instead of symbols such as `✓` and `○`. |

### JSON and YAML

`--json` and `--yaml` print the same data. You cannot use both:

```sh
orch --json --yaml task list
```

```text
error: the argument '--json' cannot be used with '--yaml'
…
```

The command exits with code 1 and prints nothing on standard output.

```sh
orch --yaml task add "T1"
```

```text
"id": "tsk_6Y95y5s"
"title": "T1"
"description": ""
"status": "todo"
"priority": 3
"labels": []
"depends_on": []
"created_at": "2026-10-18T23:05:36.692Z"
"updated_at": "2026-10-18T23:05:36.692Z"
"attempts": 0
"max_attempts": 3
```

Details:

- orch writes one document, and every key and string is in double quotes. A title such as `null` or `2024-01-15`
  therefore stays a string.
- Structured output wins over `--quiet`. `orch --yaml --quiet task add "x"` prints the whole task.
- Errors are not structured. A failing command prints the same error text on standard error, with the same exit
  code, whatever you choose. Standard output stays empty.
- Commands that never print JSON ignore `--yaml` too. `orch agent shop` is one.
- `orch doctor` prints a header line before its structured document, so its output is not one document.
- `orch logs --follow` still streams text, and `orch serve` still writes JSON Lines. `serve --log-format` is a
  separate option. See [Running and monitoring](07-running-and-monitoring.md).

### Quiet

`--quiet` prints only IDs. Use it to capture an ID in a script:

```sh
TASK=$(orch task add "Say hello" --quiet)
orch run "$TASK"
```

### Colour and ASCII

orch prints colour only when its output goes to a terminal. These also turn it off:

- the `--no-color` flag;
- the `NO_COLOR` environment variable, set to any value.

`--ascii` replaces the symbols. The same task list prints like this:

```text
  STATUS  PRI  TASK  AGENT  TIME  WAITING  
  o todo  P3   T2    —      —              
  o todo  P3   T1    —      —              

  2 tasks
```

orch also uses ASCII when the `TERM` environment variable is `dumb`.

## Update definitions from files

Agents, tasks and the project configuration each have an `edit` command that opens your `$EDITOR`. For scripts,
reviews and version-controlled definitions, they also have two commands that read a JSON or YAML file:

- **Replace** (`update`) swaps in the whole writable definition. Optional fields you leave out are cleared.
  Commands: `orch agent update <id> -f <file>`, `orch task update <id> -f <file>`, `orch config update -f <file>`.
- **Merge** (`patch`) changes only the members you name. The document is a JSON Merge Patch (RFC 7396), a
  standard way to describe a partial update. Commands: `orch agent patch <id> -f <file>`,
  `orch task patch <id> -f <file>`, `orch config patch -f <file>`.

Goals, teams, context and messages have no file commands.

`update` and `patch` need the entity ID (`agt_…` or `tsk_…`). They do not accept an agent name.

| Option | What it does | Default |
|---|---|---|
| `-f`, `--file <file>` | The input file. `-` reads standard input. Required. | None |
| `--input-format json\|yaml` | Forces the input language. | Detected |

orch detects the language from the file extension: `.json` is JSON, and `.yml` and `.yaml` are YAML. For any other
name, and for standard input, it reads JSON if the first non-blank character is `{` or `[`, and YAML otherwise.

### Replace an agent

Before you start, find the agent's ID with `orch agent list --quiet`.

1. Write the whole definition to a file, `agent.yaml`:

   ```yaml
   name: a1
   adapter: shell
   role: Greeter
   config:
     command: echo hello
   ```

2. Replace the agent.

   ```sh
   orch agent update agt_jiKEIg9 -f agent.yaml
   ```

   You should see:

   ```text
     ✓ Updated agent a1 (agt_jiKEIg9)
   ```

### Patch an agent

A patch changes only the members you name. An object merges into the existing object, an array replaces the
whole array, and `null` removes a member.

```sh
echo '{"config":{"command":"echo patched"}}' | orch agent patch agt_jiKEIg9 -f -
```

```text
  ✓ Updated agent a1 (agt_jiKEIg9)
```

### Patch the project configuration

```sh
orch --yaml config patch -f - <<'EOF'
project:
  description: null
EOF
```

This removes `project.description` and prints the saved configuration as YAML.

### What you can and cannot change

| Definition | You can write | You cannot write |
|---|---|---|
| Agent | `name`, `adapter`, `role`, and in `config`: `command`, `model`, `effort`, `approval_policy`, `max_turns`, `timeout_ms`, `stall_timeout_ms`, `env`, `system_prompt`, `workspace_mode`, `skills`, `prompt_mode`, `role_summary` | `id`, `status`, `stats`, `capabilities`, `current_task`, `last_error`, `autonomous`, `remote` |
| Task | `title`, `description`, `priority` (1 to 4), `labels` | `id`, `status`, `assignee`, `depends_on`, timestamps, `attempts`, `max_attempts`, `scope`, `attachments` |
| Project configuration | The whole `config.yml` object | Global settings |

Rules to remember:

- **Nothing is saved unless the whole result is valid.** A failed command changes nothing.
- **`update` needs every required field.** A missing required field fails. A missing optional field is cleared.
- **You cannot change a running or remote agent or task.**
- **You can feed `show` output back to `update`.** Read-only fields whose value matches the current one are
  ignored. A `patch` rejects any read-only field.
- **Unknown fields are rejected** in agents and tasks. Unknown keys in the project configuration are kept.
- **To reassign a task, use `orch task assign`.**
- **Output.** Human output is `✓ Updated agent <name> (<id>)`, `✓ Updated task <id>` or
  `✓ Updated project configuration`. `--quiet` prints only the ID for an agent or task. `--json` and `--yaml`
  print the saved definition.
- **Limits.** An input file may be at most 1 MiB, one document, nested at most 64 levels deep.
- **Errors.** Validation errors name the source and the field, and exit with code 2. A missing ID, a lock timeout
  or an unreadable file exits with code 1.

To roll back, run another `update` with the previous definition. Save it first with `orch --yaml agent status <id>`.

The full rules, every error message and the locking limits are in [file-mutations.md](../file-mutations.md).
Runnable inputs are in [examples/file-mutations/](../../examples/file-mutations/).
