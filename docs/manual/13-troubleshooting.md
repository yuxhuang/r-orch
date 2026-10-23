# Troubleshooting

This chapter helps you when something does not work. Find your symptom, read the cause, and follow the fix. For recovery steps that need care, each entry links to the operator runbook ([docs/RUNBOOK.md](../RUNBOOK.md)) instead of repeating it.

## In this chapter

- [Setup and project](#setup-and-project)
- [Running and tasks](#running-and-tasks)
- [The dashboard](#the-dashboard)
- [The web dashboard](#the-web-dashboard)
- [Remote agents](#remote-agents)
- [Templates and files](#templates-and-files)
- [Known gaps in v0.9.3](#known-gaps-in-v093)
- [Get more detail](#get-more-detail)

## Setup and project

### `Not initialized` and `Run: orch init`

```text
  ✕ Not initialized
    Run: orch init
```

- **Cause.** The current directory, and every parent directory, has no `.orchestry/` folder. Almost every command needs a project. The exit code is 3.
- **Do.** Run `orch init` in your project directory. `orch --help`, `orch --version`, `orch init`, `orch doctor`, `orch update` and the `orch org template` commands work anywhere.

### orch acts on the wrong project

- **Cause.** orch uses the nearest `.orchestry/` in the current directory or any parent. If you run it in a subdirectory of a project, it acts on that project. If your home directory is itself a project, every directory under it belongs to that project.
- **Do.** Check where you are with `pwd`, and run `orch status` to see which project answers. Run `orch init` in the directory you want as its own project. Use a sandbox directory outside your home directory when you experiment.

### An adapter is not found

- **Symptom.** An agent fails at once, or `orch doctor` shows a `✕` for its adapter, for example:

  ```text
  ✕  cursor       Cursor Agent CLI not found. The headless agent CLI is required (cursor-agent or agent).
  ```

- **Cause.** orch starts the agent's own command-line program (the adapter). It does not install it, and the program must be on your `PATH`.
- **Do.** Run `orch doctor`. It lists each adapter, `git` and your project, and ends with a line such as `4 of 8 adapters ready`. Install the missing CLI and sign in to it, or use a different adapter. The `shell` adapter needs only `bash`. `orch doctor` always exits with code 0, so do not use the exit code to test it.

### Every `claude` agent fails at once inside an agent session

- **Cause.** You started orch from inside a Claude Code session. The `claude` program refuses to start a nested session.
- **Do.** Run orch from a plain shell. See [Nested agent sessions](../RUNBOOK.md#nested-agent-sessions).

### Secrets in agent config

If you worry about API keys in agent settings, read [Secrets in agent config](../RUNBOOK.md#secrets-in-agent-config).

## Running and tasks

### `Orchestrator already running (PID: undefined)` and nothing is running

- **Cause.** The lock file `.orchestry/orchestry.lock` holds something that is not a process number, for example after an interrupted write. orch refuses it and never cleans it up. `orch status` does not warn you, and the dashboard silently becomes an observer.
- **Do.**
  1. Check that no orchestrator is running: `pgrep -af 'orch (run|serve)'` should print nothing for this project. Do not delete the lock of a live orchestrator, or two orchestrators dispatch the same tasks.
  2. Look at the file: `cat .orchestry/orchestry.lock`. A number is a real process; anything else is damage.
  3. If it is damaged, delete it: `rm .orchestry/orchestry.lock`, then run your command again.

  More detail: [Corrupt lock file](../RUNBOOK.md#corrupt-lock-file).

### The dashboard says `OBSERVING` but I did not use `--observe`

- **Cause.** Another process, for example `orch serve`, holds the lock. The dashboard watches that process instead of dispatching.
- **Do.** This is normal if you started `orch serve`. If you did not, check for the corrupt lock above.

### A task sits in `todo` and nothing happens

- **Cause.** One of these:
  1. Nothing is dispatching. orch runs tasks only while an orchestrator runs (`orch serve`, `orch run --watch`, or the dashboard in `WATCHING` mode), or when you run a task by hand.
  2. The task has no enabled agent, or its agent is busy or disabled.
  3. Another `todo` task is assigned to the same agent. In 0.9.3 `orch run <task-id>` then does nothing.
- **Do.** Start an orchestrator, or run `orch run <task-id>`. In the dashboard, type `/watch`. For the third cause, cancel or reassign the other task, or use `orch run --all`. See [Run one task](07-running-and-monitoring.md#run-one-task).

### A task stays in `review` and `orch serve --once` never exits

- **Cause.** `--once` waits until every task is `done`, `failed` or `cancelled`. A task in `review` is none of those. The `Agent Creator` agent that `orch init` adds has the approval policy `suggest`, so its tasks wait in `review`.
- **Do.** Approve the task (`orch task approve <id>`), set the agent's approval policy to `auto`, or give the task review criteria. See [`serve --once` hangs on a task in review](../RUNBOOK.md#serve---once-hangs-on-a-task-in-review) and [Tasks](04-tasks.md).

### An agent has run for hours

- **Cause.** The agent setting `timeout_ms` is stored but never enforced. Only `stall_timeout_ms` (default 600000, ten minutes) stops a run that produces no output, and only under `orch serve` or `orch run --watch`. `orch run <task-id>` has no stall detection at all.
- **Do.** Cancel the task with `orch task cancel <task-id>`. For one-shot runs, wrap the command: `timeout 3600 orch run <task-id>`. Prefer `serve` for unattended work. Details: [`timeout_ms` is not a timeout](../RUNBOOK.md#timeout_ms-is-not-a-timeout).

### Agent processes still run after I cancelled a task

- **Cause.** Cancelling stops the agent, but a program the agent started can keep running.
- **Do.**
  1. Find leftovers: `ps aux | grep -i <agent program>` (for example `claude` or `codex`), or look for processes whose working directory is in `.orchestry/workspaces/`.
  2. Stop the ones that belong to the cancelled task with `kill <pid>`. Use `kill -9` only if that fails.

  More detail, including how to find all the processes a run started: [Stray agent subprocesses after a cancel](../RUNBOOK.md#stray-agent-subprocesses-after-a-cancel).

### A message never reached the agent

- **Cause.** A message is delivered on the agent's next run. If nothing is dispatching, it waits. It expires after 24 hours and nothing reports that.
- **Do.** Start `orch serve` or type `/watch` in the dashboard. See [Messages and context](06-messages-and-context.md).

### `task show` finds a task that `task list` omits, or every command fails after a crash

- **Cause.** The task index or the state file is damaged, for example after a crash or a power loss.
- **Do.**
  1. Stop every orch process for the project.
  2. Back up the folder: `cp -R .orchestry .orchestry.bak`.
  3. Follow the repair steps in [Rebuilding an index after a lost update](../RUNBOOK.md#rebuilding-an-index-after-a-lost-update) or [`state.json` parse failure](../RUNBOOK.md#statejson-parse-failure).

### `update` or `patch` fails with `mutation lock timed out` or `read mutation input`

- **Cause.** Another `update` or `patch` holds the lock, or the `-f` file cannot be read.
- **Do.** Retry, and check the file path and permissions. A failed command leaves the definition unchanged. Table of causes: [File mutations: update and patch failures](../RUNBOOK.md#file-mutations-update-and-patch-failures).

### `--json` or `--yaml` output of `orch doctor` will not parse

- **Cause.** `doctor` prints a text header before the structured document.
- **Do.** Drop the first two lines: `orch --json doctor | sed '1,2d'`. See [Structured output](../RUNBOOK.md#structured-output-for-doctor-logs---follow-and-serve) and [Configuration and output](12-configuration-and-output.md).

## The dashboard

### Copying text from the dashboard does nothing

- **Cause.** The status after a copy says "requested" when the dashboard asked your terminal to copy (the terminal cannot confirm). Check these in order:
  1. Your terminal does not allow the copy request. Under tmux, set `set-clipboard on`. Under GNU screen it is not supported.
  2. You are on SSH and expected your laptop's clipboard, but the host clipboard is the server's.
  3. The text is over the size limit.
  4. You are in the web dashboard and have not clicked **Copy to clipboard**.
- **Do.**
  1. Press `F1` on the text. The help shows the exact status and route guidance.
  2. Open the text in the viewer (`Enter`), press `F6`, then `End`, then `Enter` to choose **Copy on this host**.
  3. In a browser, click **Copy to clipboard** when the panel appears.
  4. Press `F2` to release the mouse and use your terminal's own selection.

  The routes, keys and size limits are in [Copy text](08-dashboard.md#copy-text). More detail: [Copying text from the dashboard](../RUNBOOK.md#copying-text-from-the-dashboard).

### I cannot select text with the mouse

- **Cause.** The dashboard captures the mouse so it can handle clicks.
- **Do.** Press `F2` or type `/mouse off`. Hold `Shift` while dragging works in most terminals, but not in tmux. To start with the mouse off, run `ORCH_TUI_MOUSE=0 orch`. See [Use the mouse](08-dashboard.md#use-the-mouse).

### Clicks do nothing under tmux

- **Cause.** tmux passes the mouse on only when it is on.
- **Do.** Add `set -g mouse on` to your tmux configuration.

### The terminal prints odd characters after the dashboard exits

- **Symptom.** Clicking in the terminal prints text like `[<0;44;8M`, or a paste arrives wrapped in `[200~ … [201~`.
- **Cause.** The dashboard died in a way that could not restore the terminal (`SIGKILL`, for example). Normal exits, signals and panics restore it.
- **Do.** Reset the terminal modes:

  ```sh
  printf '\033[?1006l\033[?1002l\033[?1000l\033[?2004l'
  reset
  ```

  See [A leaked mouse or paste mode](../RUNBOOK.md#a-leaked-mouse-or-paste-mode-after-an-abnormal-exit).

### I closed an editor and lost my text

- **Cause.** Drafts live in memory only, and a signal exits without a prompt.
- **Do.** Choose **Keep draft** when you leave an editor, and reopen it in the same dashboard session. See [Keep drafts](08-dashboard.md#keep-drafts-and-avoid-losing-edits) and [Editor exits and session drafts](../RUNBOOK.md#editor-exits-and-session-drafts).

### The pasted message was sent too early

- **Cause.** Your terminal or tmux has no bracketed paste, so a newline in the paste counts as `Enter`.
- **Do.** Paste a single line, or use `orch msg send`.

### The agent shop prints `Cancelled.` or a table

- **Cause.** The picker draws only when both input and output are terminals. Redirected output prints the `--list` table. Input that is not a terminal cancels at once. Both exit with code 0 and create nothing.
- **Do.** Run `orch agent shop` in an interactive terminal inside a project, or use `orch agent shop --list` and `orch agent add`. See [Templates and the agent shop](10-templates-and-shop.md) and [The agent shop picker](../RUNBOOK.md#the-agent-shop-picker).

## The web dashboard

### The browser asks for a password

- **Cause.** The server needs a credential. The default is a token that changes at every start.
- **Do.** Open the full address that `orch web` printed, including `?token=…`, or enter the token as the password (any user name). Start the server again to get a new token. See [Log in](09-web-dashboard.md#log-in).

### orch refuses my `--password` or `--no-auth`

- **Cause.** A password off loopback needs 16 or more characters. `--no-auth` is refused on wildcard addresses and cannot be combined with a password.
- **Do.** Use a longer password, or bind a specific address.

### Reaching the server by name fails

- **Cause.** The server accepts only `Host` names it knows.
- **Do.** Add `--allow-host NAME` for each name. See [Run it behind a proxy](09-web-dashboard.md#run-it-behind-a-proxy).

### The server answers `429`

- **Cause.** After repeated wrong credentials from one address, the server answers `429` and waits longer each time.
- **Do.** Wait, then use the right credential. A correct credential always works.

## Remote agents

### A remote shows `offline`

- **Cause.** Several possible causes. `orch remote status` gives the reason in brackets: `(connecting)`, `(Peer closed connection; retry in 12s, attempt 5)`, `(Worker busy: controlled by mac …)` or `(Inbound controller name collides with a configured outbound alias)`.
- **Do.**
  1. On the worker, check that the orchestrator runs with `--listen` and a LAN address, and that `orch remote status` shows `Listen:`.
  2. Check the address and port you gave to `orch remote add`, and the firewall.
  3. If the reason mentions a name collision, give each side a different `--peer-name`. See [Set up two machines that control each other](11-remote-agents.md#set-up-two-machines-that-control-each-other).
  4. If it is `Worker busy`, the worker has another controller. A worker accepts one controller at a time.
  5. A remote also shows `offline` when `.orchestry/remotes/` is unwritable or the disk is full. Free space or fix permissions.

  See [Reading a peer's link state](../RUNBOOK.md#reading-a-peers-link-state).

### A remote task never starts

- **Cause.** The worker's agent holds it for approval (`queued @alias`, `awaiting approval`), or the remote is offline (`RUN? (alias offline)`), or a `cap:` label cannot be met.
- **Do.** On the worker, run `orch task list` and `orch task approve <worker task id>`. See [Approve work on the worker](11-remote-agents.md#approve-work-on-the-worker) and [Remote task phases](../RUNBOOK.md#remote-task-phases).

### Remote tasks fail to sign in, but the agent works locally

- **Cause.** The worker drops secret-looking environment variables for remote runs.
- **Do.** Pass them by exact name with `ORCH_REMOTE_ENV_PASSTHROUGH`. See [The environment of remote runs](11-remote-agents.md#the-environment-of-remote-runs).

### A task copy sits in `todo` on the worker for ever

- **Cause.** A mirror (the task a worker makes for a controller's request) stays when its controller never returns.
- **Do.** On the worker, run `orch task cancel <task id>`. The worker also cancels `todo` mirrors itself after 24 hours without the controller. See [Mirrors left behind on a worker](../RUNBOOK.md#mirrors-left-behind-on-a-worker).

## Prompt mode and effort

### `eco prompt mode is incompatible with custom templates`

- **Cause.** You tried to switch the project, or an agent, to eco while `prompt.template`, `prompt.system_template` or `prompt.user_template` is set. Nothing was saved. If the message comes from `orch run` or the `serve` log, a file edited by hand combines them: no run was created and the task stays `todo` until the conflict is gone.
- **Do.** Give the agents the message lists `--prompt-mode normal`, or remove the custom templates (a running `orch serve` sees a removed template only after a restart). See [Eco mode and custom templates](16-eco-mode-and-effort.md#eco-mode-and-custom-templates).

### `r-orch does not accept effort …` when I add or change an agent

- **Cause.** The effort is not allowed for that adapter and model ([Effort levels by adapter](16-eco-mode-and-effort.md#effort-levels-by-adapter)). When you changed something else, the stored effort may come from orch 0.9.4: orch checks the whole result.
- **Do.** Pick a level from the `Allowed:` list, choose a model that has it, or clear the effort with `orch agent update <id> --effort default`.

### A run fails with `effort preparation failed`

- **Cause.** An OpenCode agent asks for a level that orch 0.9.4 did not have, and the installed `opencode` is not version 2 or its help does not show `provider/model#variant`. The model was not started.
- **Do.** Install OpenCode 2, or clear the effort (`orch agent update <id> --effort default`). Check with `opencode --version`.

### `orch serve` logs `configuration preflight: cannot read defaults.agent.prompt_mode` and the task stays `todo`

- **Cause.** orch reads the project prompt mode again before every dispatch and could not read or parse `.orchestry/config.yml`, which changed while `serve` was running. No run is created and the task's attempts do not change. `serve` logs the error as `orchestrator:error` and tries again on every tick; `orch serve --once` does not finish while this repeats.
- **Do.** Fix the file; `orch config get defaults.agent` shows the parse error. You do not need to retry the task: the next tick dispatches it once the file reads cleanly. See [Prompt mode and effort failures](../RUNBOOK.md#prompt-mode-and-effort-failures).

### A Codex agent uses a different model than before

- **Cause.** The agent has an effort and no model. orch now passes the effort to Codex, which can bypass a managed default model for new threads.
- **Do.** Set an explicit model: `echo '{"config":{"model":"gpt-6.1-sol"}}' | orch agent patch <id> -f -`. See [Effort warnings](16-eco-mode-and-effort.md#effort-warnings).

### After a downgrade, every agent command fails with `parse …/agents/agt_….yml`

- **Cause.** orch 0.9.4 cannot read an effort it does not know, such as `xhigh`, `max` or `ultra`.
- **Do.** Run the newer orch again and clear those efforts (`orch agent update <id> --effort default`), or restore the agent files from a backup. See [Upgrade, downgrade and roll back](16-eco-mode-and-effort.md#upgrade-downgrade-and-roll-back).

## Templates and files

### `org deploy` failed half way

- **Cause.** Deploy is not transactional. Nothing is rolled back, and the error lists what exists.
- **Do.** Fix the cause, then remove the listed agents, team and goal. See [When something fails](10-templates-and-shop.md#when-something-fails) and [Organization templates: failures and recovery](../RUNBOOK.md#organization-templates-failures-and-recovery).

### `Builtins are read-only`

- **Do.** Copy the template with `orch org template init NAME --from builtin:a-team`, then change the copy.

## Known gaps in v0.9.3

These are limits in the released version.

- Copy and mouse selection in Terminal.app and iTerm2 were not checked by hand before release. If copying fails there, use **Copy on this host** or your terminal's own selection.
- No automated test covers remote agents on two machines or a real network: the tests run every instance on one machine. Two machines were last checked by hand with v0.5.1, a macOS controller and a Linux worker on a local network.
- Messages and task results between peers use plain TCP, with no authentication or encryption.
- If writing to `claude`'s standard input fails after the first 32 KiB, `claude` can run with a cut-off prompt. orch logs a warning but does not report a run error.
- The `cursor`, `opencode`, `agy` and `grok` adapters pass the prompt as a command-line argument, and `pi` passes its system prompt that way. On Linux they can hit the 128 KiB limit for one argument. `claude` and `codex` do not.

## Get more detail

To see the full error chain of a failing command, set `ORCHESTRY_DEBUG=1`. Diagnostics go to stderr and do not disturb `--json` output.

```sh
ORCHESTRY_DEBUG=1 orch run <task-id> 2>debug.log
```

Other useful reads: `orch logs --task <task-id>`, and the raw run log in `.orchestry/runs/<run-id>.jsonl`. See [Debugging](../RUNBOOK.md#debugging) and [Running and monitoring](07-running-and-monitoring.md).
