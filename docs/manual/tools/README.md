# Manual tools

These tools regenerate the generated parts of the manual: the command reference and the dashboard screenshots
in `docs/manual/images/`.

## Command reference

```sh
cargo build && docs/manual/tools/gen-command-reference.sh
```

`gen-command-reference.sh` runs `orch init` in a throwaway directory under the system temp dir (with its own
`HOME`), collects `orch … --help` for every command, and has `command_reference.py` rewrite
`../14-command-reference.md`. Pass a different binary as the first argument. Needs Python 3 (standard library
only).

## Screenshots

- `demo-project.sh` builds a sandboxed demo project: 8 agents on several adapters, a team, a goal and 9 tasks in the `todo`, `review`, `done` and `failed` states, plus messages.
- `capture-screenshots.mjs` serves that project with `orch web`, drives the dashboard in headless Chrome over the DevTools protocol, and saves one PNG per shot.

### Prerequisites

- A built orch binary (default `target/debug/orch`).
- Node 22 or later. No packages are installed.
- Google Chrome or Chromium. The script checks the macOS Chrome path, then `google-chrome`, `chromium` and `chromium-browser` on `PATH`.
- `git` and `node` on `PATH` (the demo script uses both).

### Regenerate all images

```sh
cargo build && node docs/manual/tools/capture-screenshots.mjs
```

Run it from the repository root. It takes about 20 seconds.

| Variable | What it does | Default |
|---|---|---|
| `ORCH_BIN` | Path to the orch binary | `target/debug/orch` |
| `CHROME` | Path to the Chrome or Chromium binary | auto-detected |
| `ONLY` | Comma-separated shot names to capture, for example `ONLY=dashboard-help` | all shots |
| `KEEP_DEMO=1` | Keep the demo sandbox after the run | delete it |
| `DUMP_TEXT=1` | Print the terminal text before each shot, to debug key sequences | off |

To build only the demo project: `docs/manual/tools/demo-project.sh target/debug/orch [sandbox-dir]`. It prints the project path.

### Shots

| File | How the script reaches it |
|---|---|
| `dashboard-overview.png` | Default Tasks view |
| `dashboard-agents.png` | Right arrow to the Agents view |
| `dashboard-task-detail.png` | Enter on the first task |
| `dashboard-new-agent.png` | Agents view, `n` |
| `dashboard-agent-shop.png` | In the New Agent wizard, `Ctrl+S` |
| `dashboard-message-viewer.png` | Actions view, select an entry, `i`, Tab to Body, Enter |
| `web-dashboard.png` | Goals view in the browser page (the page has no chrome besides the terminal) |
| `dashboard-help.png` | `F1` |

The script switches views with the arrow keys. Letter keys act on the selected item in the Tasks view (`a` approves a task).

### Safety

- **Sandbox.** The demo lives in a temporary directory under the system temp directory with its own `HOME` and `ORCHESTRY_GLOBAL_CONFIG_PATH`. The demo script gets a built environment, and git ignores your global and system config. Both scripts refuse a sandbox under the real home, because orch searches parent directories for `.orchestry/`. After `orch init`, the demo script checks that `.orchestry/` exists in the project, and the capture script checks it again before `orch web` starts.
- **No AI CLI runs.** What keeps a real AI CLI from starting is the sandbox `HOME` and a minimal `PATH` (`/usr/bin:/bin`) for `orch web`: the AI CLIs cannot be found and would have no credentials. No runnable task is assigned to a non-shell agent, but that is not a guarantee: orch matches an unassigned task to the best idle agent, which can be one of the AI-adapter agents. `orch web --observe` only stops watch-mode dispatch: the dashboard can still run a task on `r`/`R` or a click, and the script never presses them. In the demo script, `orch run` happens only while every agent uses the `shell` adapter (checked before each run), and the default `claude` agent from `orch init` is removed first. Agents on other adapters are added after the runs finish.
- **Kept sandboxes are live projects.** A sandbox kept with `KEEP_DEMO=1`, or made by running `demo-project.sh` on its own, contains agents on the claude, codex, opencode and cursor adapters. The script writes a `WARNING.md` into the project and prints a warning next to "demo project ready:". Never serve it, open the dashboard on it, or run it with a real `PATH` or `HOME`. Delete the sandbox when you are done.
- **Loopback only.** `orch web` binds `127.0.0.1` on an ephemeral port and uses the generated access token. Chrome's DevTools port is also on `127.0.0.1`. It has no authentication, so any local process can use it while the script runs.
- **Cleanup.** Chrome and `orch web` run in their own process groups. They are killed when the script ends, on error (including unhandled errors), on SIGINT, SIGTERM or SIGHUP, and after a 3-minute watchdog. The sandbox and the Chrome profile are deleted unless `KEEP_DEMO=1` is set.
- **Clipboard.** The script does not press copy keys, so it never touches the host clipboard.
