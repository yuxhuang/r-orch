# Getting started

After this chapter you know what orch does, you have it installed, and you have run your first task and opened
the dashboard. Read it first if you are new to orch. If you are only deciding whether orch fits your work, read
[What orch does](#what-orch-does) and then [Concepts](02-concepts.md).

## In this chapter

- [What orch does](#what-orch-does)
- [Requirements](#requirements)
- [Install orch](#install-orch)
- [The three binary names](#the-three-binary-names)
- [Quick start](#quick-start)
- [Use a real AI agent instead](#use-a-real-ai-agent-instead)
- [Where to go next](#where-to-go-next)

## What orch does

orch coordinates AI coding agents in one project. You describe the work as **tasks**. You describe the workers
as **agents**. Each agent is an AI command-line tool (for example Claude Code or Codex) plus a model and a role.

When you start orch, it:

1. Matches ready tasks to idle agents.
2. Prepares a workspace for each task. By default this is a separate git worktree, so agents do not overwrite
   each other's files.
3. Starts the agent's command with a prompt built from the task.
4. Writes the agent's output to a log.
5. Moves the task to `review` when the agent succeeds. You approve it, or an agent whose approval policy is
   `auto` approves it for you.
6. Retries a failed task with a growing delay, up to the task's attempt limit.

orch keeps everything in plain files in a `.orchestry/` folder in your project. There is no database and no
background service to install. You can read the files, and you can put the project under version control the
way you already do.

You can use orch from the command line, from a terminal dashboard (`orch tui`), or from a browser
(`orch web`). All three work on the same files.

## Requirements

| You need | Why |
|---|---|
| macOS or Linux | The platforms the project is built and tested on. |
| `git` | The default workspace mode, `worktree`, needs a git repository. `orch init` creates one if needed. `orch doctor` reports whether it finds one. |
| `bash` | The `shell` adapter runs your command through it. |
| An AI coding CLI for each adapter you use | orch starts these programs. It does not include them. |
| Rust, installed with [rustup](https://rustup.rs) | Only if you build orch from source. The repository's `rust-toolchain.toml` selects the version (1.99.1). |

> **Note:** orch does not bundle any agent CLI. Install and sign in to the tools you want to use before you
> create agents for them. Run `orch doctor` to see which ones orch can find.

### Adapters

An **adapter** is the kind of AI CLI an agent uses. You pick it with `--adapter` when you add an agent.

| Adapter | Program orch starts |
|---|---|
| `claude` | Claude Code (`claude`) |
| `codex` | Codex (`codex`) |
| `cursor` | Cursor's headless agent (`cursor-agent` or `agent`) |
| `opencode` | OpenCode (`opencode`) |
| `pi` | Pi (`pi`) |
| `grok` | Grok (`grok`) |
| `antigravity` | Google Antigravity (`agy`) |
| `shell` | Any command you give it, run with `bash` |

The `shell` adapter needs no AI tool. It is the quickest way to try orch, and this chapter uses it.

## Install orch

### Build from source

Before you start, install `git` and Rust (with `rustup`). `rustup` reads the version from the repository's
`rust-toolchain.toml`.

1. Clone the repository and go into it.

   ```sh
   git clone https://github.com/yuxhuang/r-orch.git r-orch && cd r-orch
   ```

2. Check out the release you want. This manual describes version 0.9.3.

   ```sh
   git checkout v0.9.3
   ```

3. Install the binaries.

   ```sh
   cargo install --locked --path crates/orch-cli
   ```

4. Check the version.

   ```sh
   orch --version
   ```

   You should see:

   ```text
   0.9.3
   ```

Keep the cloned folder. orch finds its library skills in `skills/library` of the checkout it was built from.

orch is open-source software under the Apache License 2.0; see `LICENSE` in the repository. Some bundled material,
such as the library skills, keeps its own MIT licence; `THIRD-PARTY-NOTICES.md` in the repository lists it.

To build without installing, run `cargo build --release`. The binaries are then in `target/release/`.

### Homebrew

The project README mentions a Homebrew formula named `r-orch` that follows the latest release tag. It does not
say how to install it, so this manual does not give a `brew install` command. If a formula is available to you,
`brew upgrade r-orch` upgrades it.

### Upgrade

`orch update` does not update itself. It prints your version and how to upgrade, then exits.

```sh
orch update
```

```text
  orch update · version 0.9.3

  This build does not update itself.
  To install the latest version:
    Homebrew:  brew upgrade r-orch
    Source:    git pull && cargo install --path crates/orch-cli
```

## The three binary names

`orch`, `orchestry` and `ao` are the same program. Use whichever name you like. This manual always writes
`orch`.

## Quick start

In this tutorial you create a project, add an agent that prints a greeting, give it a task, run the task, and
open the dashboard. The agent uses the `shell` adapter, so you do not need an AI CLI.

The output below was captured from orch 0.9.3. The IDs in your output differ, because orch generates them.

Before you start, install orch (see [Install orch](#install-orch)) and `git`.

### Create a project

A **project** is a directory with a `.orchestry/` folder. Make a new directory and initialize orch in it.

1. Create the directory and go into it.

   ```sh
   mkdir hello-orch && cd hello-orch
   ```

2. Initialize orch.

   ```sh
   orch init
   ```

   > **Warning:** If the directory is not a git repository, `orch init` runs `git init`. If the repository has
   > no commits, it commits every file in the repository it finds as "Initial commit" (git worktrees need one
   > commit). Inside a subdirectory of a repository without commits, that repository may be a parent
   > directory, so the commit can include files outside your current directory. Run `orch init` in a new
   > directory, or check what is there first.

   You should see:

   ```text
     ✓ initialized

     Created .orchestry/
     ├── config.yml
     ├── tasks/
     ├── agents/
     │   └── agt_creator.yml (Agent Creator)
     ├── templates/default.md
     └── .gitignore

     Next: orch task add "Create backend agent" --assignee agt_creator
   ```

orch also created an agent called Agent Creator. It uses the `claude` adapter, so it is a real AI agent.

3. Disable Agent Creator.

   ```sh
   orch agent disable agt_creator
   ```

   You should see:

   ```text
     ✓ Disabled agent agt_creator
   ```

> **Warning:** Agent Creator is a real AI agent, and its runs can cost money. If it is enabled, any of these
> can start a paid run: `orch run --watch`, `orch serve`, the dashboard (`orch tui`, which watches by default,
> and even `orch tui --observe` when you press `r` on a task), and `orch web`. Disabling it keeps this
> tutorial free of AI tools. Do not follow the `Next:` hint that `orch init` printed:
> it assigns work to Agent Creator.

### Add an agent

Add an agent named `greeter` that runs one shell command. It only prints text and writes no files, so nothing
is lost when its workspace is removed. An agent that changes files must commit them; see
[Workspaces](02-concepts.md#workspaces).

```sh
orch agent add greeter --adapter shell --command 'echo "Hello from orch"'
```

```text
  ✓ Added agent greeter (shell) → agt_cTXiWfi
```

The last word is the agent's ID. Commands that take an agent accept the ID.

### Add a task

Create a task and assign it to the agent by name.

```sh
orch task add "Say hello" --assignee greeter
```

```text
  ✓ Created tsk_Xt-LziN "Say hello"
```

List the tasks to see its state.

```sh
orch task list
```

```text
  STATUS  PRI  TASK       AGENT        TIME  WAITING  
  ○ todo  P3   Say hello  agt_cTXiWfi  —              

  1 tasks
```

The task is `todo` and has priority 3, the default.

### Run the task

Run the task by its ID. Replace `tsk_Xt-LziN` with the ID from your own output.

```sh
orch run tsk_Xt-LziN
```

```text
  orch · running tsk_Xt-LziN "Say hello"
  19:06:58  ▸ Hello from orch

  ✓ Done
```

The line after `▸` is the agent's output. The command waits until the run ends.

### See the result

1. List the tasks again.

   ```sh
   orch task list
   ```

   ```text
     STATUS  PRI  TASK       AGENT        TIME  WAITING  
     ✓ done  P3   Say hello  agt_cTXiWfi  —              

     1 tasks · 1 done
   ```

   The task is `done`. It went to `done` without waiting for you because agents created with the `shell`
   adapter use the approval policy `auto`. See [Review and approval](02-concepts.md#review-and-approval).

2. Show the task's details.

   ```sh
   orch task show tsk_Xt-LziN
   ```

   ```text
     Say hello
     ══════════════════════════════════════════

     Status    ✓ done · attempt 1/3
     Priority  P3
     Agent     agt_cTXiWfi
     Path      …/hello-orch/.orchestry/workspaces/tsk_Xt-LziN
     Created   2026-10-18T23:06:57.920Z

     Result
     ──────────────────────────────────────────
     Branch:        orchestry/tsk_Xt-LziN/say-hello
   ```

3. Read the run's log.

   ```sh
   orch logs --task tsk_Xt-LziN
   ```

   ```text
     Run run_7z41Tb7 · attempt 1 · succeeded
     19:06:58  ▸ Hello from orch
   ```

   Each run also leaves two files in `.orchestry/runs/`: a `.json` file with the run's summary and a `.jsonl`
   file with its events.

### Open the dashboard

Start the terminal dashboard.

```sh
orch tui
```

The dashboard fills the terminal with your agents, tasks and activity. You should see a header with the project
name and a row of view tabs. Use `Left` and `Right` to switch views and `Tab` to move between sections.

![The dashboard's Tasks view: nine tasks in review, to-do, done and failed states with their priority, goal and assignee, and the activity feed below](images/dashboard-overview.png)

*The Tasks view, which the dashboard opens on. The header counts tasks by status.*

The screenshot was taken with `orch tui --observe`, so its header reads `OBSERVING`. In your terminal the header
reads `WATCHING`, because `orch tui` also starts the orchestrator. It dispatches `todo` tasks to idle agents.

Press `q` to leave the dashboard. Chapter [Dashboard](08-dashboard.md) explains the layout and every key.

To watch the project in a browser instead, run `orch web`. See [Web dashboard](09-web-dashboard.md).

### Run tasks continuously

`orch run <task-id>` runs one task and exits. To keep orch working through your task list, start the
orchestrator and leave it running:

```sh
orch run --watch
```

Or run it without a terminal UI, as a background-friendly process that writes structured logs:

```sh
orch serve
```

Both pick up new `todo` tasks as you add them. See [Running and monitoring](07-running-and-monitoring.md).

## Use a real AI agent instead

To use an AI tool, change the adapter and drop `--command`. The task, run and dashboard steps stay the same.

Before you start, install the tool, sign in to it, and run `orch doctor` to confirm orch can find it. Use
this only when you want a real AI run.

```sh
orch agent add builder --adapter claude --model claude-sonnet-4-6 --approval-policy auto
orch task add "Add input validation to the signup form" --assignee builder
orch run --watch
```

Notes on the options:

- `--model` is the model name the tool accepts. It does not apply to the `shell` adapter.
- `--approval-policy` is `suggest`, `auto` or `manual`. With `auto`, a finished task goes straight to `done`.
  With `suggest` it waits in `review` until you approve it. See
  [Review and approval](02-concepts.md#review-and-approval).
- Use `--role` to describe what the agent is for, and `--skills` to give it a comma-separated list of skills.

> **Warning:** Agent Creator is disabled in the tutorial above. Enable it again (`orch agent enable agt_creator`)
> only on purpose, because any orchestrator can then start a paid run with it.

> **Warning:** Keep API keys in your environment, not in an agent's `env:` map. orch writes an agent's `env:`
> values to `.orchestry/agents/<id>.yml` as plain text. Only the file's owner can read it, and `orch init` adds
> `.orchestry` to your project's `.gitignore`, but backups and copies of the folder still carry the key. See the
> [runbook](../RUNBOOK.md#secrets-in-agent-config).

The agent shop and templates install ready-made agents. See [Templates and shop](10-templates-and-shop.md).

## Where to go next

| You want to | Read |
|---|---|
| Understand the ideas behind orch | [Concepts](02-concepts.md) |
| Add and manage agents | [Agents](03-agents.md) |
| Manage tasks, review and retry | [Tasks](04-tasks.md) |
| Run orch unattended | [Running and monitoring](07-running-and-monitoring.md) |
| Fix a problem | [Troubleshooting](13-troubleshooting.md) |
