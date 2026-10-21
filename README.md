# r-orch

An AI agent orchestrator: a CLI, a terminal dashboard, and a runtime that drives external AI coding CLIs
(`claude`, `codex`, `cursor`, `opencode`, `pi`, `grok`, `agy`) as child processes, moves tasks through a state
machine, and keeps everything in plain files under `.orchestry/`.

No database or daemon to install. One engine process per project, guarded by a PID lock file; optional
peer connections let it delegate work to other installations.

r-orch was originally a port of [`oxgeneral/ORCH`](https://github.com/oxgeneral/ORCH) and is developed
independently; it makes no compatibility promise with that project or its npm package.

---

## What it does

You define **agents** (an AI CLI plus a model, a role and some skills) and **tasks** (work with a priority,
dependencies, and optionally a file scope). The orchestrator matches tasks to idle agents, prepares a
workspace, renders a prompt, spawns the agent, streams its output to an append-only log, and moves the task
through `todo → in_progress → review → done` with retries and backoff on failure.

Beyond that: **goals** that drive autonomous agents, **teams** with a lead, inter-agent **messaging**, a
shared **context** store, git **worktree** isolation per task with automatic merge-back, and automatic
review criteria (`typecheck`, `lint`, `test_pass`).

---

## Install

From source, which requires a Rust toolchain; the version is pinned in `rust-toolchain.toml`.

```bash
git clone <this repo> && cd r-orch
cargo build --release
```

The binaries land in `target/release/`. Put them on your `PATH`, or:

```bash
cargo install --path crates/orch-cli
```

### Installing a release

Releases are tags on `main` (`vX.Y.Z`). v0.5.0 is the first release with
[remote agents](#remote-agents); they were previewed in the pre-release tags `v0.5.0-alpha.1` and
`v0.5.0-alpha.2`. v0.5.1 makes remote task state truthful and reconnection reliable; v0.5.2 is a maintenance release (newer dependencies, Rust 1.99 to build); v0.6.0 improves the dashboard: full-text message viewing, focusable and scrollable sections, guarded editors with drafts, and copy to the clipboard; v0.7.0 adds Agents-list sorting and activity feeds that follow new entries; v0.8.0 adds `--yaml` output, `update`/`patch` from JSON or YAML files, and organization templates with the built-in a-team; v0.9.0 adds sortable column headers, keyword filtering and node grouping to the dashboard lists, newest-first Activity in goal and agent details, and filtering agent pickers in the wizards, and makes `orch task add --attach` save its files; v0.9.1 shows a goal detail's Tasks section as the Tasks list's table; v0.9.2 makes the Agents list a sortable table with an UPDATED column and more groupings; v0.9.3 relicenses r-orch under Apache-2.0 and fixes paths in the a-team template. The Homebrew formula pins the latest release tag (and `--HEAD` follows `main`).
`orch update` does not self-update: it only prints how to install. To build a specific release from source:

```bash
git clone <this repo> r-orch && cd r-orch
git checkout v0.9.3
cargo install --locked --path crates/orch-cli
orch --version      # 0.9.3
```

Keep the clone: the library skills are found in `skills/library` of the checkout the binary was built from.
Before upgrading a project that runs v0.4.0, read
[Upgrading from v0.4.0 and rolling back](#upgrading-from-v040-and-rolling-back).

### The three binary names

`orch`, `orchestry` and `ao` are **the same program** — three names for one binary. Use whichever you prefer; `orch` is shortest and is what the docs use.

### Agent CLIs are not bundled

r-orch spawns AI CLIs; it does not ship them. Install whichever you intend to use and check what is visible:

```bash
orch doctor
```

The `shell` adapter needs nothing but `bash` and runs any command you give it — the easiest way to try
r-orch without an AI CLI, and what the test suite uses.

---

## Quick start

```bash
cd your-project
orch init                                   # creates .orchestry/
orch agent add builder --adapter claude --model claude-sonnet-4-6 --approval-policy auto
orch task add "Add input validation to the signup form" --assignee builder
orch run --watch                            # or: orch serve      (headless)
orch                                        # the TUI dashboard
```

Bare `orch` launches the dashboard. In a directory with no `.orchestry/`, it initialises one first.

**Mouse.** Click tabs, list rows, `+ add …` rows, filter chips and the footer hint buttons;
double-click a row to open its detail; the wheel moves the selection. Everything is also a key —
the mouse adds nothing you cannot type.

Capture holds the pointer, which takes click-drag **text selection** away from the terminal. To get
it back: hold **Shift** while dragging (most terminals), or press **F2** to release the pointer for
the session. **Under tmux, Shift+drag is not the answer** — tmux holds the outer terminal's mouse
mode, so it selects across pane borders; use `F2` (or `/mouse off`), which also restores tmux's own
drag-to-copy. `prefix [` copy mode always works. Under tmux you also need `set -g mouse on`, or no
clicks reach the dashboard at all.

To start with the mouse off: `ORCH_TUI_MOUSE=0 orch`. `/mouse status` says which it is.

### Machine-readable output: `--json` and `--yaml`

Every command that can print JSON can print YAML instead. Both flags are global, so they work before or after the
subcommand, and they are mutually exclusive.

```bash
orch --yaml task list          # same as: orch task list --yaml
orch --json task list          # unchanged
```

```
$ orch --yaml task list
-
  "id": "tsk_4OfOerS"
  "title": "Add input validation"
  "description": ""
  "status": "todo"
  "priority": 3
  "assignee": "agt_s4OhRJP"
  "labels": []
  "depends_on": []
  "created_at": "2026-10-18T12:49:54.405Z"
  "updated_at": "2026-10-18T12:49:54.405Z"
  "attempts": 0
  "max_attempts": 3
```

- **Same data as `--json`.** The value is the one the JSON path prints, in the same key order, so `--yaml` parses
  to exactly what `--json` parses to. The `--json` bytes did not change.
- **One document, newline-terminated, no `---` marker.** Every key and every string is double-quoted, so a title
  such as `null`, `on`, `1_000` or `2024-01-15` stays a string in YAML 1.1 readers as well as 1.2 readers.
  Empty collections are `[]` and `{}`.
- **Structured output wins over `--quiet`**, as `--json` does: `orch --yaml --quiet task add "x"` prints the whole
  created task.
- **Errors are not structured.** A failing command prints the same text on stderr, with the same exit code, under
  `--yaml`, `--json` and neither; stdout stays empty. Passing both flags is a usage error (exit 1, nothing on
  stdout): `error: the argument '--yaml' cannot be used with '--json'`.
- **Commands that never printed JSON ignore `--yaml` too** (for example `agent shop`, which prints its table).
  `logs --follow` still streams text and `serve` still writes JSON Lines; `serve --log-format` is unrelated to
  `--yaml`, and persisted logs do not change format.
- **`orch doctor` is the exception.** Inside a project it prints its human header line on stdout before the
  structured document, under `--json` as well as `--yaml`, so the output does not parse as one document.
  Outside a project it prints text only. See the [runbook](docs/RUNBOOK.md#structured-output-for-doctor-logs---follow-and-serve).

### Reading, navigating and editing in the dashboard

`Left`/`Right` switch views; `Tab`/`Shift-Tab` move between the list, activity and available detail sections.
The Agents view aligns rows in a table. Arrows, PageUp/PageDown, Home/End and the wheel navigate the focused
section. A click or wheel focuses the section under the pointer. Entity action letters act from the main list;
return there to edit or delete. Opening a detail initially focuses its Overview section. The section titles
above a detail keep their order as you move between them; only the focused one is marked. A task detail's
Activity section shows that task's entries in the Activity panel's columns: time, agent, type icon and text,
with errors highlighted. Goal and agent details end with an Activity section in the same columns, newest
first: a goal's lists the entries of its tasks, an agent's its own. It follows new entries at the top; `Down`
pauses it and `Home` resumes.

The Goals, Tasks and Agents lists carry a column header row. `o` cycles the sort column, `O` reverses the
direction and `0` restores that list's default order. Clicking an inactive header sorts by it ascending; clicking
the active header flips its direction. The active column is marked `↑` or `↓`. Each list keeps its own sort, and
none of these keys touch grouping.

Goals and Tasks also show a Started and an Elapsed* column, both blank for an entity that never started.
Elapsed*: start to now; terminal rows end at last update (edits change it). Includes waiting/retries.
A task's Started is its earliest retained run record, which is written while the run is still Preparing — not the
moment an agent process launched; a goal's Started is its creation time. Ordering by Elapsed re-sorts on a refresh
or a list-control change, while the time the column shows stays live between those.

The Agents table's columns are STATUS, NAME, ADAPTER/MODEL, TASK, CAPS, WHERE, RUNS and UPDATED, and `o` steps
through all eight. Its default order, which `0` restores, is STATUS ascending: active, idle, error, offline, then
disabled. Agents with equal values keep their configured order and blanks (`—`) sort last, in both directions. RUNS
is the agent's lifetime runs as completed/total, for example `1/1 ✓`. UPDATED tells you which agent's run or
assignment record changed most recently, not when the agent last worked: `R <age>` is the newest start or event of
its live run, `T <age>` the newest update of a task assigned to it (possibly an administrative change such as an
edit or approval), and `R` wins over `T`. While UPDATED is the sort column the footer reads `R run · T task`.
Narrower than 93 columns, UPDATED, CAPS or both are hidden; a sort by a hidden column still applies, and the
footer names it, for example `o Caps↑`.

`b` moves the Agents table to the next grouping and `B` to the previous one: Team (the default), Node, Status,
Adapter, Caps and None. Node puts a `LOCAL` section first, then one section per remote alias in alias order, each
marked online or offline. Caps lists each agent once, under its alphabetically first capability. The footer's first
hint names the mode (`b Team`), and from 108 columns the header row ends with a `GROUP: <Mode>` chip; clicking
either moves to the next mode. The sort orders agents within each section; the sort keys never change the grouping
and `b`/`B` never change the sort, and the selected agent stays selected. Both last only for this dashboard
session. The [dashboard manual](docs/manual/08-dashboard.md#sort-and-group-agents) has the full column and
grouping reference.

Wizard steps that pick an agent (task and goal assignee, team lead and members) filter as you type: letters
narrow the numbered list to agents whose name, adapter or role contains them, `Backspace` edits the filter and
`Esc` clears it. `1`–`9` still pick a listed row, and `Ctrl+O` cycles the order through Original, Name, Adapter
and Status, as the filter row shows. Auto-assign/None stays first; only a listed agent or that choice is submitted.

`Ctrl+K` on the Goals, Tasks or Agents list opens a keyword filter (`/` stays the command bar). The list narrows
as you type, case-insensitively, on each row's ID, title or name, status (stored and shown: `in_progress` or
`RUN`), assignee or agent name and ID, and the task's goal or the agent's team, adapter, model, node and every
capability; never on descriptions or the Started/Elapsed text. It filters before the sort, grouping and the
ten-task limit, so the ten tasks listed are all matches and a match past the first ten unfiltered tasks is not
lost; with more than ten matches, `show all` lists the rest. A section with no match disappears, and counts read
`matches/total`. `Enter` keeps the keyword, shown as a `filter:` chip on the list's title rule; `Esc` in the
prompt, `Ctrl+U` on the list or a click on the chip clears it. The selected row stays selected while it still
matches. Each list keeps its own keyword, up to 256 characters, for this dashboard session; control characters
in pasted text are dropped.

The Activity panel, ACTIONS list and task-detail Activity follow new entries at the tail. Scrolling up pauses
following so you can read older entries. Resume with `End`, scroll down to the bottom with the wheel or
PageDown, click the final entry, or focus a feed whose viewport is already at the bottom. The feed shows
whether it is following or paused. Opening the Logs inspector with `i` pins the current entry so new arrivals
do not replace its content or scroll position. Expanded text viewers keep their immutable snapshots.

Press `i` on supported text, or activate a message with Enter/double-click, to open its full-text viewer.
The viewer shows an immutable Snapshot: close and reopen to see new content. Known message envelopes open as
Human text; Raw preserves the received bytes. The footer offers the representation switch. Arrows and paging
keys scroll; mouse drags select text across wrapped lines. Only complete admitted bodies are available: bodies
over 1 MiB show an unavailable notice, and older records can be evicted under the 500-record/32 MiB text budget.
These are component limits, not a process memory cap.

`Ctrl+Y` copies the whole current field/representation. Viewer `y` copies its selection; in an editor use
Copy selection on the F6 rail. In a focused read-only section Ctrl+Y copies directly. `F6` opens the copy rail; arrows or Tab choose an action and Enter invokes
it. `F6`, End, Enter chooses **Copy on this host** without needing Option/Alt. The mouse `Copy…` chip and
previous/next controls offer the same choices; `x` closes the rail. `F1` shows contextual help, full route guidance
and matching copy status. Escape/F1 closes help and restores the inspected text.

Auto copy requests the terminal under SSH, tmux, screen or `orch web`; otherwise it uses the host clipboard when
available. Explicit **Copy on this host** targets the machine running Orch, including a remote host. A terminal
status says *requested*, because OSC 52 cannot confirm clipboard delivery. Terminal requests are limited to
64 KiB; host requests support 1 MiB. Native failure never silently writes to another destination. In the browser,
click **Copy to clipboard** after requesting a copy; permission denial or an insecure context provides selectable
fallback text. See [copy troubleshooting](docs/RUNBOOK.md#copying-text-from-the-dashboard).

Selection endpoints differ between widgets: the viewer includes the pressed glyph, while the textarea uses
insertion positions. The same reverse drag can therefore include one extra glyph in the viewer. Tabs, combining
characters, trailing spaces and source bytes are preserved; review the highlight before copying.

Leaving a changed editor asks whether to continue, keep a draft or discard. Drafts live only in this process,
with at most 20 drafts, 256 KiB per draft and 2 MiB total; a full store refuses another draft rather than silently
evicting one. Quitting with unsaved edits or saved drafts asks for confirmation; exit discards RAM drafts.
Primary submit failures keep the form; a failure after creation reports the created entity and avoids another
create. Vim editing is not included; the [plan](docs/plans/2026-10-16-tui-qol.md) records feasibility and limits.

**Messaging an agent from the dashboard.** On the Agents tab, select an agent and press **`m`**: the
command bar opens prefilled `/msg "Code Reviewer" `, ready for the body. Or type `/msg ` and pick from
the agent list — Tab or a double-click fills the name, already quoted. The message is queued exactly as
`orch msg send` queues it; the recipient reads it on its **next run**, so start `/watch` if nothing is
dispatching.

```
/msg reviewer the signup test is flaky on CI      # bare name
/msg "Code Reviewer" please look at #12           # a name with spaces
/msg agt_7fK2mQ1 ping                             # by id, always unambiguous
/msg all standup in five                          # broadcast — asks for a second Enter
```

Names are quoted the way a shell quotes them, because agent names may contain anything but whitespace
alone:

| Name | Write it as |
|---|---|
| `reviewer`, `名前` | `reviewer`, `名前` |
| `Code Reviewer`, `🚀 deploy` | `"Code Reviewer"`, `"🚀 deploy"` |
| `say "hi"` | `"say \"hi\""` |
| `C:\path` | `"C:\\path"` |
| `don't` | `"don't"` |
| `all` (an agent actually named that) | `"all"`, or its id |

The body is everything after the name, verbatim — its own quotes, `#` and `/` are not interpreted, so
`/msg reviewer /quit now` sends that text and does not quit.

Autocomplete offers prefix matches, but **sending never guesses**: a prefix, an ambiguous name or an
unquoted name that could be read two ways (`Code` when `Code Reviewer` also exists, including
`/msg Code Rev hi`) is refused with the exact form to use. `all` broadcasts only as the bare lowercase
word and only after a confirming second Enter, pressed straight after the first — any edit, any other
key and leaving the bar all make it ask again. `/msg ALL …` is **not** a broadcast: `ALL`, `All` and
`@all` are read as names, so they reach an agent actually called that and are otherwise refused with
`No agent matches`. The chip in the command bar names the recipient while you type.

A queued message **expires after 24 hours** if the recipient never runs; nothing reports that it did,
so for anything time-sensitive check that something is dispatching (`/watch`, or `● WATCHING` in the
header). Long bodies: paste is bracketed, so newlines are kept and shown as `⏎` rather than submitting
the message early — but one paste is capped at **64 KiB** (with a line in the feed when it is cut), and
the engine cuts the body itself at **4000 characters**, which the TUI reports as an advisory. In a
terminal or multiplexer *without* bracketed paste (Windows legacy console, tmux with the mode off) a
pasted body still arrives as raw keystrokes: the first newline submits and the rest is typed into the
dashboard. Paste one line, or paste into `orch msg send` instead.

### Updating definitions from files

Besides `edit` (opens `$EDITOR`), agents, tasks and the project config can be replaced or patched from a JSON or
YAML file or stdin, with no editor:

```bash
orch agent update <agent-id> -f agent.yaml            # replace: omitted optional fields are cleared
echo '{"config": {"model": null}}' | orch agent patch <agent-id> -f -     # merge: null removes a member
orch task patch <task-id> -f task-patch.yaml          # RFC 7396 merge patch
orch --yaml config update -f config.json              # print the saved config as YAML
```

`update` needs the whole writable definition; `patch` merges a partial document (objects merge, arrays replace,
`null` deletes). Identity and runtime state (`id`, `status`, `stats`, an assignee, …) are never writable, a
running or remote agent or task cannot be changed, and nothing is written unless the whole result validates.
`--input-format json|yaml` overrides detection from the extension or first byte. The full contract, the
writable fields, every error and the locking limits are in [docs/file-mutations.md](docs/file-mutations.md);
runnable inputs are in [examples/file-mutations/](examples/file-mutations/).

Two things that will bite you on first use, both covered in the [runbook](docs/RUNBOOK.md):

- **`approval_policy: auto`** above is deliberate. The default `agt_creator` agent uses `suggest`, which
  leaves finished tasks in `review` — and `serve --once` waits for `review` forever.
- **Keep API keys in your environment, not in an agent's `env:` map.** Agent files are world-readable and
  are *not* gitignored by `init`.

### The agent shop

`orch agent shop` installs one agent from a built-in catalogue of 15 role templates (Backend Developer, QA
Engineer, Architect, …). It needs an initialised project (`orch init`).

```bash
orch agent shop --list      # the catalogue as a table: Key, Name, Tier, Skills (first two)
orch agent shop             # interactive picker
```

The picker draws only when both stdin and stdout are terminals. Keys: `↑`/`↓` (or `Ctrl-P`/`Ctrl-N`) move and
wrap around the ends, `Enter` installs the highlighted template, `q`, `Esc` or `Ctrl-C` cancel. Each row is
`Name — description [tier]`, cut to the terminal width with an ellipsis (description first, then the tier, then
the name), and a list taller than the terminal shows its window as `1-7 of 15`. Resizing the terminal redraws
the picker at the new size; a terminal too short for the blank lines drops them before it drops rows.
The heading uses the full help when it fits, then `AGENT SHOP  ↑↓ move · enter select · q cancel`, then
`AGENT SHOP  ↑↓ Enter q`. The compact heading keeps movement, selection and cancellation visible at
30 and 40 columns; below its display width it is cut with an ellipsis. This is the picker at 80x12:

```
  AGENT SHOP  — arrow keys to navigate, enter to select, q to cancel

  ▸ Backend Developer — APIs, databases, backend services [balanced]
    Frontend Developer — React, UI components, CSS, responsive design [balanced]
    QA Engineer — Test writing, coverage analysis, quality assurance… [balanced]
    Code Reviewer — PR review with auto-fix, adversarial review, secu… [capable]
    Architect — System design, architecture decisions, tech leadership [capable]
    DevOps Engineer — CI/CD, infrastructure, deployment, monitoring [balanced]
    Bug Hunter — Systematic debugging, root cause analysis, minimal … [balanced]

  1-7 of 15
```

Choosing a template creates one agent and prints `✓ Added agent QA Engineer (claude) → agt_…`; cancelling
prints `  Cancelled.` and exits 0 without writing anything. With stdout redirected the command prints the `--list`
table; with stdin not a terminal (for example `echo | orch agent shop` in a terminal) it cancels at once.
`--json` and `--quiet` do not change this command.

---

## Organization templates

A template is a directory — `org.yml`, one YAML file per member in `agents/`, optional shared rules in `rules/` —
that `orch org deploy` turns into agents, a team and an optional goal. The embedded **a-team** (27 specialist roles)
is built in, the ten older presets still work, and you can keep your own templates under `~/.orchestry/templates/`.

```bash
orch org template list                                      # embedded, legacy and your own templates
orch org template show a-team --resolved                    # exactly what deploy would create
orch org deploy a-team --goal "Ship the release"            # 27 agents, a team, a goal
orch org deploy --template-dir ./my-team                    # deploy a directory you wrote
orch agent shop --template-dir ./my-team                    # install one member (--list for a table)
orch org template init my-team --from builtin:a-team        # a writable copy of a builtin
orch org template agent patch my-team --agent sre -f patch.yaml        # change one member, no editor
orch org template update my-team -f org.json                # replace org.yml from JSON or YAML
```

`update` replaces a file from a complete JSON or YAML document and `patch` applies an RFC 7396 merge patch; both read
a file or stdin, validate the whole template first and write one file atomically. `--yaml` works wherever `--json`
does. Builtins are read-only (copy them with `init --from`). Deploying is **not transactional**: a failure part way
names the agents and team that were already created. The a-team assumes agent CLIs, skills and a sibling `../workspace`
directory that `deploy` does not check. The format, precedence rules, error messages, repair of a broken template,
limits and prerequisites are in [docs/org-templates.md](docs/org-templates.md); runnable templates and mutation
inputs are in [examples/org-templates/](examples/org-templates/).

---

## Remote agents

Peers use plain TCP on a trusted LAN, **without authentication or encryption**. Listening is off by
default. Only the process that owns the project engine opens peer connections; a TUI observing an
existing owner cannot enable listening.

> [!WARNING]
> **A listening worker runs whatever a connecting peer sends it.** Any host that can reach the port can
> list the worker's agents and submit tasks to them, including `shell` agents that run arbitrary
> commands. There is no password, token or allowlist. Only expose the listener on a network where you
> trust every host: never on a public interface, shared Wi-Fi or a VPN with strangers on it, and
> preferably behind a firewall that admits only your controller's IP.

A bare `--listen` and `orch remote listen on` bind **`127.0.0.1:7690`**, which only accepts
connections from the same machine. To accept another machine you must give an address explicitly,
either a specific LAN IP of the worker (preferred) or `0.0.0.0:7690` (all interfaces). For any
address other than loopback, `orch remote listen on`, `orch serve` and `orch run --watch` print a
warning on stderr, and `orch remote status` and the TUI header show the address as
`(EXPOSED, no auth)`.

On the worker, start an engine with a unique display name and an explicit listen address:

```bash
orch serve --listen 192.168.1.20:7690 --peer-name worker-linux   # the worker's own LAN IP
# Or every interface: orch serve --listen 0.0.0.0:7690 --peer-name worker-linux
# Also supported: orch run --watch --listen ... or orch tui --listen ...
```

To persist the preference for future engine starts, use `orch remote listen on 192.168.1.20:7690`
(without an address it saves `127.0.0.1:7690`); `orch remote listen off` disables it on the next
start. A `--listen` flag applies to that invocation.
Use `--peer-name` or `peer.name` in `.orchestry/config.yml` to distinguish installations.

On the controller, register the worker and run the engine:

```bash
orch remote add w1 worker-host:7690
orch serve --peer-name controller
# In another terminal:
orch remote status
orch agent list
orch task add "Run the worker's checks" --assignee 'agt_x@w1'
orch msg send 'agt_x@w1' "Please include the test results in your summary"
# On the worker, reply to the controller's CLI:
orch msg send 'cli@controller' "Checks passed"
```

Replace `agt_x` with an agent ID from the worker. Remote agents appear as `<id>@<alias>`, with
`linux` or `macos` host capabilities in the CAPS column. Add `--labels cap:linux` to a task to require
Linux; unsupported capabilities leave the task waiting. The TUI header and `orch remote status` show
the effective listen address, connected controller, and each configured remote's online state.

A controller can connect to several workers. Each worker accepts one controller at a time. Registry
entries persist across restarts; links reconnect with backoff until `orch remote remove w1`, which also
deletes the cached status; a link still running for the removed entry cannot recreate it. Offline
agents remain visible, and new tasks assigned to them wait in `todo`. During a link outage an active
remote task waits for its outcome without a retry: the worker owns execution and stall detection.
Cancelling the task or removing the remote ends that wait. A controller restart cancels its active
remote tasks; reconnecting reconciles and cancels their worker mirrors. Messages queue while offline
and deduplicate when delivered. Broadcast messages stay local.
Remote runs do not use the controller's local slot cap (`scheduling.max_concurrent_agents`): the
worker enforces its own, and each remote agent runs one task at a time.

A remote counts as online only while its link is live **and** the controller can record that state in
`.orchestry/remotes/`. If that directory is unwritable or the disk is full, or the socket drops, the
remote reports `offline` in `orch remote status`, the TUI and `orch agent list`, even if a cached
snapshot file still says online, and `orch task show` reports `remote w1 offline` for tasks waiting on
it. Nothing new is dispatched to it until the status can be saved again. The engine prints a
non-fatal `Write peer status for w1 … Check permissions and free space` (or `Peer w1 disconnected`)
error without `RUST_LOG`. Fix the permissions or free space; once the link reconnects and saves its status, the remote is online
again and waiting tasks dispatch. Local agents keep running throughout, including after an engine restart.

**Remote agents are leaf executors using worker-local resources.** There is no repository, input,
file, or artifact synchronization. Paths refer to the worker, which runs its own agent command, role,
skills, workspace integration and review criteria. Prepare required resources there before assigning
work. Hand results back in summaries or direct messages. Remote agents must not orchestrate the
controller's graph with `orch task add`, `orch goal`, or `orch context`: those commands operate on the
worker's own project. Task proofs record the remote alias, worker branch and changed files. Worker
review failures and merge conflicts put the controller task in `review` and keep dependents blocked.

### Peer names and mutual links

Every instance announces a peer name (`--peer-name`, `peer.name` in `.orchestry/config.yml`, default: the
short hostname). A worker rejects an incoming controller whose peer name equals one of **its own outbound
aliases** (`orch remote list`). This is deliberate: it keeps `<id>@<alias>` routing unambiguous.

The natural mutual setup trips it. Two hosts, `mini` and `studio`, each listen and each register the other
under its hostname:

```bash
# on studio:  orch remote add mini <mini-ip>        # outbound alias "mini"
# on mini:    orch remote add studio <studio-ip>    # outbound alias "studio"
```

mini connects to studio announcing itself as `mini`, which is studio's alias for it, so studio refuses, and
studio's link to mini is refused the same way. Both sides keep retrying and report, verbatim:

```
$ orch remote status      # on mini
  Listen: 127.0.0.1:17690
  Controller: none
  studio: offline (Inbound controller name collides with a configured outbound alias)
```

Fix: give each side a `--peer-name` that is not an alias on the other side, or use different aliases:

```bash
orch serve --listen 192.168.1.20:7690 --peer-name mini-ctl      # on mini
orch serve --listen 192.168.1.21:7690 --peer-name studio-ctl    # on studio
```

After that, `orch remote status` on both shows the peer `online` and the other side as `Controller:`.
A one-directional setup (one controller, one worker) never hits this.

### Environment of remote runs

A run started on behalf of a remote controller does **not** inherit credential-shaped variables from the
worker's `orch` process. Local runs are unaffected. A name is dropped when it matches, case-insensitively,
one of these:

- an underscore-separated segment equal to `KEY`, `KEYS`, `APIKEY`, `TOKEN`, `TOKENS`, `SECRET`, `SECRETS`,
  `PASSWORD`, `PASSWD`, `PASS`, `PAT`, `AUTH`, `OAUTH`, `CREDENTIAL`, `CREDENTIALS` or `COOKIE`;
- the substring `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `APIKEY`, `CREDENTIAL` or `PRIVATEKEY` anywhere.

So `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GITHUB_TOKEN`, `AWS_ACCESS_KEY_ID` and `SSH_AUTH_SOCK` are
stripped, and so is the harmless `TOKENIZERS_PARALLELISM` (it contains `TOKEN`). `GIT_AUTHOR_NAME` and
`PLAIN_VAR` are kept. Matching is by name only: a secret under an innocent name, such as `DATABASE_URL`, is
not caught.

**Consequence:** if a worker's `claude` or `codex` authenticates through an environment variable, or git
needs `SSH_AUTH_SOCK`, remote-submitted tasks fail on authentication while the same agent works locally.
Re-admit variables explicitly, by exact name (no wildcards):

```bash
# On the worker, for the orch process that owns the engine (serve, run --watch or tui):
ORCH_REMOTE_ENV_PASSTHROUGH=ANTHROPIC_API_KEY,SSH_AUTH_SOCK orch serve --listen 192.168.1.20:7690
```

The list is separated by commas or whitespace and is read each time a run starts. Variables set in the
agent's own `config.env` are always passed, filtered or not. Checked with a `shell` agent: with
`FAKE_API_KEY`, `SSH_AUTH_SOCK`, `TOKENIZERS_PARALLELISM`, `PLAIN_VAR` and `GIT_AUTHOR_NAME` set on the
worker, a remote run saw only `PLAIN_VAR` and `GIT_AUTHOR_NAME`; with
`ORCH_REMOTE_ENV_PASSTHROUGH=FAKE_API_KEY,TOKENIZERS_PARALLELISM` it also saw those two, and still not
`SSH_AUTH_SOCK`.

### Approval on the worker

Remote submits honour the **worker** agent's approval policy. Only an agent whose policy is `auto` runs a
remote task straight away; `suggest`, `manual` and an agent with no policy stored hold it. On a fresh
project `orch agent add` stores `auto` for `shell` agents and the project default (`defaults.agent.approval_policy`,
`auto` after `orch init`) for the others, but the `Agent Creator` agent from `orch init` is `suggest`. Check
with `orch agent show <id>` (it prints `auto` for an agent with no policy, so a hand-edited agent file
without one still holds remote tasks). A held task sits in `todo` on the worker with the `awaiting-approval` label, while the
controller shows it `in_progress` (the worker reports it as queued). Approve it **on the worker**, using the
worker's own task id (the controller's id differs):

```bash
orch task list            # on the worker: find the mirror task
orch task approve tsk_05O0U7_
# ✓ Approved tsk_05O0U7_ → released to run
```

Until then nothing runs there, so a remote task can wait indefinitely for someone at the worker.

### Upgrading from v0.4.0 and rolling back

Upgrade
- Run **one version on every peer**; mixed builds have not been tested.
- Upgrade the engine owner (`orch serve`, `orch run --watch` or the TUI) and the CLI on the same host
  together: processes of one project coordinate through lock files that only this build takes.
- Back up first: `cp -R .orchestry .orchestry.bak` in each project, then stop the old engine, install v0.5.0
  (`brew upgrade r-orch`, or from source as in [Installing a release](#installing-a-release)) and start it.
- Changes you may notice since v0.4.0:
  - the listener binds `127.0.0.1:7690` by default; give `0.0.0.0:7690` or a LAN IP explicitly to accept
    other hosts. There is still no authentication;
  - the entity indexes under `.orchestry` (tasks, agents, messages, context) take a cross-process writer lock
    (`.index.lock` in each folder), which fixes lost updates between concurrent CLI and engine writes. A writer that cannot get it within 5 seconds
    fails with `Timed out after 5000 ms waiting for index writer lock …; retry the operation`;
  - a remote counts as online only while the controller's engine holds a lock on
    `.orchestry/remotes/<alias>.health`: after `kill -9` of the engine, `orch remote status` showed the remote
    `offline` at once;
  - remote tasks wait for worker approval (above) and run without credential-shaped variables (above);
  - `.orchestry/peer-id` and `.orchestry/remotes/*.json` are written readable by the owner only;
  - `claude` agents get the system prompt through a private file (`--system-prompt-file`, in a 0700
    per-user directory: `$XDG_RUNTIME_DIR/orch`, else `<temp dir>/orch-<uid>`) and the task prompt on
    stdin, instead of as command-line arguments. This fixes `Failed to spawn process: claude` on Linux
    when the prompt exceeded the 128 KiB single-argument limit. A blank system prompt now counts as none.

Roll back
1. Stop every v0.5.0 engine on the project and make sure **no remote runs are in flight**: the old binary
   cannot resume remote execution entries and will cancel them.
2. Go back to the v0.4.0 binary: reinstall it from its tag (the steps in
   [Installing a release](#installing-a-release) with `git checkout v0.4.0`), or switch back to a v0.4.0
   install you kept. If anything looks wrong, stop the engine and restore `.orchestry` from
   `.orchestry.bak`.

v0.4.0 has no remote support, so the `remote` commands and the `Controller` and `Listen` status lines
disappear when you roll back.

### Upgrading from v0.5.0 and rolling back

Upgrade every peer to v0.5.1 together: the fixes below are mostly on the worker, so a v0.5.1 controller
talking to a v0.5.0 worker still sees the v0.5.0 failures. Changes you may notice:
- a task file containing Markdown emphasis no longer fails with `contains N YAML aliases, over the limit
  of 64` (the alias guard now counts real aliases from the parser, not `*` characters);
- a remote task shows where it really is: `dispatched → <alias>`, `queued @<alias>` (with
  `awaiting approval: orch task approve <id> on <alias>` when the worker holds it), `RUN @<alias>` only once
  the worker runs it, and `RUN? (<alias> offline)` while the link is down;
- one unreadable task file on a worker no longer drops every connection; `orch remote status` on the
  worker shows why the last session ended, and on the controller the retry countdown and attempt;
- a controller restart, graceful or not, keeps remote runs and reattaches to them instead of cancelling;
- a submit the worker never acknowledges fails after 5 minutes online; a task queued for an hour is
  reported hourly but never failed;
- a worker cancels leftover task copies of a controller it has not seen for 24 hours (todo copies only).

Rolling back to v0.5.0 cancels remote runs in flight (stop with none in flight first), and v0.5.0 never
cleans up leftover task copies on a worker. See [docs/RUNBOOK.md](docs/RUNBOOK.md) for each message.

### Upgrading from v0.9.2

v0.9.3 changes no behaviour, data or protocol. Agents you deploy from the built-in a-team template now write
specs and reports to a sibling `../workspace` directory: rename your existing sibling directory to that, or deploy
from an edited copy of the template. Their orchestrator now creates work trees under `<repo root>/.worktrees`.
Agents already deployed, and template copies you made, keep their old wording.

### Upgrading from v0.8.0

v0.9.0 changes the dashboard and fixes `orch task add --attach`. The peer protocol is unchanged, so peers on
0.5.x through 0.9.0 interoperate, and `--json` output keeps its shape. Keys you may notice: `o`, `O` and `0` now
sort the Goals and Tasks lists as well as Agents, `Ctrl+K` filters a list (`/` is still the command bar), and on
wizard steps that pick an agent, `j` and `k` type into the filter (use the arrow keys to move). `orch task add
--attach` now copies each file into `.orchestry/attachments/<task id>/` and lists it on the task. Roll back by
installing v0.8.0: it reads those attachments, and sorting, filtering and grouping last only for the dashboard
session.

### Upgrading from v0.7.0

v0.8.0 adds commands and output formats; `--json` output is byte-for-byte unchanged, and so is the peer
protocol, so peers on 0.5.x through 0.8.0 interoperate. `orch remote add`, `remove` and peer
links now order their writes through a new lock file, `.orchestry/remotes.lock`. Run one version per project
directory: an older binary does not take that lock, so it can still race a removal. Roll back by installing
v0.7.0; it ignores `remotes.lock`, which is safe to delete. Templates you create live under
`~/.orchestry/templates/` and are not read by older versions.

### Upgrading from v0.6.0

v0.7.0 changes only the dashboard, and peers on 0.5.x, 0.6.0 and 0.7.0 interoperate. The Agents list can be
sorted for the session, and the Activity, ACTIONS and task-detail feeds stay at the newest entry until you scroll
up. Roll back by installing v0.6.0: v0.7.0 writes nothing new to `.orchestry/` (the sort order lasts only for
the dashboard session).

### Upgrading from v0.5.x

v0.6.0 changes only the dashboard; 0.5.x and 0.6.0 peers interoperate, so upgrade them in any order. Keys
you may notice: Tab now moves focus between panes, Left/Right switch views, and entity action letters only act
while the main list has focus. Mouse capture now reports drags for text selection; press F2 to hand selection
back to the terminal. Roll back by installing v0.5.2: v0.6.0 writes nothing new to `.orchestry/` (editor
drafts last only for the dashboard session).

### Known gaps in v0.9.3

- Copy and mouse selection in a native terminal (Terminal.app, iTerm2) were not verified by hand before
  release; the automated tests cover the copy routing, and the web dashboard's browser copy.
- The automated multi-instance tests run every instance on one machine; no automated test covers two
  hosts or a physical network (v0.5.1 was verified by hand between a macOS controller and a Linux worker
  on a LAN).
- Messages and task results between peers are plain TCP: no authentication, no encryption.
- If a stdin write to `claude` fails after the first 32 KiB, claude can run with a truncated prompt. This
  is logged at `warn` but not reported as a run error.
- `cursor`, `opencode`, `agy` and `grok` pass the prompt as a command-line argument, and `pi` passes the
  system prompt as one, so they can still hit the Linux 128 KiB single-argument limit. `claude` and
  `codex` do not.

---

## Documentation

| Document | For |
|---|---|
| **[docs/manual/](docs/manual/README.md)** | **Users.** The user manual: installing, concepts, every command, the dashboard and web dashboard, templates, remote agents and troubleshooting, with screenshots |
| **[docs/RUNBOOK.md](docs/RUNBOOK.md)** | **Operators.** Recovery procedures and known gotchas — corrupt locks, hung `--once`, index rebuilds, state recovery, debugging, secrets |
| [docs/file-mutations.md](docs/file-mutations.md) | `update` / `patch` from JSON or YAML files: input rules, writable fields, errors, locking |
| [docs/org-templates.md](docs/org-templates.md) | Organization templates: directory format, rules, precedence, deploy, shop, `update`/`patch`, repair, the a-team and its prerequisites |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | The Rust design: crate boundaries, traits, concurrency model, error model, test strategy |
| [AGENTS.md](AGENTS.md) | Contributors and agents: rules, workflow, testing, release checklist |
| [CHANGELOG.md](CHANGELOG.md) | What changed in each release |
| [LICENSE](LICENSE), [NOTICE](NOTICE), [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) | Licence (Apache-2.0), attribution notices, and the licences of bundled third-party material |
| [docs/specs/](docs/specs/) | Specs: what goal-sized work must do and how it is accepted |
| [docs/plans/](docs/plans/) | Plans for non-trivial work, before it is built: design, task split, status |
| [docs/decisions/](docs/decisions/) | Decision records: why things are the way they are |

Start with the **runbook** if something is wrong.

---

## Development

Contributing, human or agent: read [AGENTS.md](AGENTS.md) first.

```bash
cargo test --workspace            # unit, integration and TUI tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo xtask crate-graph           # asserts the inter-crate dependency edges
```

See [ARCHITECTURE.md §8](docs/ARCHITECTURE.md) for the test strategy and
[§9](docs/ARCHITECTURE.md) for how the work is split across crates.

---

## License

Licensed under the Apache License, Version 2.0: see [LICENSE](LICENSE), and [NOTICE](NOTICE) for the
attribution notices. Contributions are accepted under the same licence.

r-orch is a derivative of [`oxgeneral/ORCH`](https://github.com/oxgeneral/ORCH) (MIT): its Rust source began as
a translation of ORCH's TypeScript, and the shop catalogue, prompt templates and library skills come from ORCH. The
library skills are ORCH's adaptations of [`garrytan/gstack`](https://github.com/garrytan/gstack) skills (MIT), and
`orch web` serves a vendored xterm.js (MIT). That material keeps its MIT licence;
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists each component with its full licence text, and says what to
ship with a built binary or package.
