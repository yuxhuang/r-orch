# r-orch operator runbook

Recovery and gotcha reference for running r-orch. Everything here is **inherited behaviour replicated
deliberately** from the TypeScript original unless marked otherwise — the parity contract
([PARITY.md](PARITY.md)) explains why each was kept rather than fixed, and each entry below names its section.

If you are looking for *what r-orch is* and how to build it, start at the [README](../README.md).

---

## Quick index

| Symptom | Go to |
|---|---|
| `Orchestrator already running (PID: undefined)` and nothing is running | [Corrupt lock file](#corrupt-lock-file) |
| `serve --once` never exits | [`serve --once` hangs on review](#serve---once-hangs-on-a-task-in-review) |
| `task show <id>` works but `task list` omits the task | [Rebuilding an index](#rebuilding-an-index-after-a-lost-update) |
| Every command fails after a crash or power loss | [state.json recovery](#statejson-parse-failure) |
| `.state.json.*.tmp` files piling up | [Temp-file litter](#state-file-temp-litter) |
| `serve:started` logged but the daemon is not up | [serve:started ordering](#servestarted-precedes-the-lock-check) |
| Need to see what r-orch is doing internally | [Debugging](#debugging) |
| Agent processes still running after a cancel | [Stray agent subprocesses](#stray-agent-subprocesses-after-a-cancel) |
| An agent has run for hours and nothing stopped it | [`timeout_ms` is not a timeout](#timeout_ms-is-not-a-timeout) |
| Running r-orch from inside a Claude Code session | [Nested agent sessions](#nested-agent-sessions) |
| Worried about API keys in agent config | [Secrets in agent config](#secrets-in-agent-config) |
| Cannot select text in the dashboard any more | [Mouse capture holds the pointer](#mouse-capture-holds-the-pointer) |
| The terminal prints `[<0;44;8M` when you click | [A leaked mouse mode](#a-leaked-mouse-mode-after-an-abnormal-exit) |
| A task handed to a peer shows `dispatched`, `queued` or `RUN?` | [Remote task phases](#remote-task-phases) |
| A peer is `offline` and you cannot tell why | [Reading a peer's link state](#reading-a-peers-link-state) |
| A mirror on a worker sits `todo` for ever | [Mirrors left behind on a worker](#mirrors-left-behind-on-a-worker) |
| Changing the peer protocol | [Peer protocol compatibility](#peer-protocol-compatibility) |
| `orch agent shop` prints `Cancelled.` or a table instead of a picker | [The agent shop picker](#the-agent-shop-picker) |
| `orch --json doctor` or `--yaml doctor` output will not parse | [Structured output for `doctor`, `logs --follow` and `serve`](#structured-output-for-doctor-logs---follow-and-serve) |
| `update`/`patch` fails with `mutation lock timed out`, `read mutation input` or a `<stdin>:` error | [File mutations: update and patch failures](#file-mutations-update-and-patch-failures) |
| `org deploy` failed half way, or a template will not load or change | [Organization templates: failures and recovery](#organization-templates-failures-and-recovery) |

---

## Corrupt lock file

**Symptom.** Any dispatching command (`run`, `run --all`, `serve`) fails with exit code 4:

```
  ✕ Orchestrator already running (PID: undefined)
    Use: orch status
```

…and no orchestrator is actually running. The literal word `undefined` is the tell.

**Cause.** `.orchestry/orchestry.lock` holds something that is not a decimal PID — empty, truncated, or
garbage. A lock whose PID parses is handled correctly (a dead PID is replaced, a live one is reported with its
number); a lock whose PID does **not** parse is refused and **never cleaned up**. The 60-second staleness
escape hatch does not apply, because reaching it requires a parseable PID. See
[PARITY.md §6.7 and §14.17](PARITY.md).

**The suggested command will not help you, and neither will the dashboard.** This is the part worth knowing:

- the hint says `Use: orch status`, but `orch status` is deliberately mtime-blind and **reports the project
  unlocked** on a corrupt lock — so it tells you nothing is wrong;
- `orch status` also prints `watching` whenever `state.json` has a `pid` field, with **no liveness check**.
  After a `SIGKILL` the field is still set, because the graceful shutdown never ran, so it says `watching`
  indefinitely;
- in the **TUI**, a refused lock becomes **silent observer mode** — a badge, no message. You get a dashboard
  that appears to be watching someone else's orchestrator, forever.

All three are faithful to the original, which is why the remedy lives here rather than in the code.

**Recovery.**

1. **Confirm no orchestrator is running.** Do not skip this; deleting a live lock lets two orchestrators
   dispatch the same tasks.
   ```bash
   pgrep -af 'orch (run|serve)' ; pgrep -af orchestry
   cat .orchestry/orchestry.lock        # inspect what is actually in there
   ```
2. Delete the lock:
   ```bash
   rm .orchestry/orchestry.lock
   ```
3. Re-run your command. If it fails again immediately with a *numeric* PID, that PID is real — find it before
   deleting anything.

**Prevention.** Nothing in r-orch writes a malformed lock; this comes from an interrupted write, a full disk,
or external editing.

---

## `serve --once` hangs on a task in `review`

**Symptom.** `orch serve --once` runs, logs activity, and then never returns. No timeout. Ctrl-C works.

**Cause.** `--once` polls until **every** task is terminal, and the terminal statuses are
`done`, `failed`, `cancelled` — **`review` is not among them**. A task that lands in `review` and has nothing
to approve it blocks the loop forever. See [PARITY.md §11.1](PARITY.md).

**This is reachable out of the box.** The `agt_creator` agent that `orch init` creates ships with
`approval_policy: suggest`, so a task assigned to the default agent will sit in `review` and hang `--once`.

**Three escapes**, in order of how often they are the right answer:

1. **Set the agent's approval policy to `auto`** so completed work self-approves:
   ```bash
   orch agent edit <agent-id>          # opens $EDITOR; set approval_policy: auto
   ```
2. **Give the task review criteria**, so auto-review decides:
   ```bash
   orch task add "..." --review-criteria test_pass,typecheck
   ```
   All criteria passing transitions `review → done`.
3. **Approve out of band**, from another shell while `--once` is still polling:
   ```bash
   orch task list --status review
   orch task approve <task-id>
   ```

**In CI**, prefer (1) or (2). An unattended `--once` that can reach `review` will hang the job rather than
fail it, which is the worse failure mode.

---

## Rebuilding an index after a lost update

**Symptom.** `orch task show <id>` returns the task, but `orch task list` and `orch status` do not show it,
and the orchestrator never dispatches it. The entity's `.yml` file exists on disk.

**Cause.** Each store keeps an `_index.json` cache alongside the individual entity files, maintained by a
read-modify-write that is serialised **only within one process**. Two processes writing concurrently — the
normal case, since one-shot commands never take the orchestrator lock — can drop an entry. See
[ARCHITECTURE.md §2.1](ARCHITECTURE.md) and [PARITY.md §6.10](PARITY.md).

This cannot be fixed by locking: any lock r-orch takes is invisible to the TypeScript binary, and
interchangeable operation is a goal. So it is a documented hazard with a one-command recovery.

**Recovery.** Delete the index; the next read rebuilds it from the entity files, which are the source of
truth:

```bash
rm .orchestry/tasks/_index.json      # or agents/, goals/, context/, messages/
orch task list                       # rebuilds
```

Safe to run at any time. There is **no command that does this for you**, and nothing reports the divergence —
the only symptom is the disagreement above.

**Prevention.** Avoid running mutating commands (`task add`, `agent add`, `goal add`) concurrently with a
`serve` or `run --watch`, and avoid two of them at once. `teams/` and `runs/` have no index and are immune.

**The inverse case, worth knowing while you are here.** A listing reads only the index — it never opens the
entity files — so the failure can run the other way too: a **corrupt or truncated** `tasks/<id>.yml` is
**invisible** to `task list`, which keeps serving the stale-but-valid index entry. It surfaces on
`task show <id>`, or once the index is rebuilt. So a healthy-looking `task list` is **not** evidence that
your entity files are intact. If you suspect corruption, force the read:

```bash
rm .orchestry/tasks/_index.json && orch task list
```

A rebuild reads every file and **fails loudly, naming the offending one** — which is what you want when
diagnosing, and is why the rebuild is the diagnostic as well as the repair.

---

## `state.json` parse failure

**Symptom.** Every command that reads orchestrator state fails, immediately and identically, after a crash or
power loss.

**Cause.** `.orchestry/state.json` is truncated or corrupt. Writes are atomic against *process* crashes
(temp file + rename), so a half-written file means the machine lost power or the filesystem did not order the
data before the rename.

**Recovery.** The file is **runtime state, not your data** — tasks, agents and goals live in their own files
and are untouched. Delete it:

```bash
rm .orchestry/state.json
```

The next run recreates it from defaults. What you lose: cumulative statistics (`total_runs`, token totals,
runtime), the retry queue, and the record of what was running. Tasks left mid-flight will be reconciled on the
next `start_watch` — `in_progress` tasks with no live process are cancelled rather than retried, so agents do
not redo committed work.

Check also for a stale lock afterwards ([above](#corrupt-lock-file)).

---

## State-file temp litter

**Symptom.** Files named `.state.json.<8 hex>.tmp` (or `.<entity>.yml.<hex>.tmp`) accumulate in
`.orchestry/`.

**Cause.** `atomic_write` writes a temp file and renames it. A process killed **between** the write and the
rename leaves the temp file behind; cleanup only runs on the write's own error paths.

**These files are inert.** Directory scans filter by `.yml`/`.json`, so index rebuilds ignore them, and
`.orchestry/.gitignore` keeps them out of git. They cost disk and nothing else.

**Recovery.** Delete them whenever you like, including while the orchestrator runs:

```bash
find .orchestry -name '*.tmp' -delete
```

Nothing sweeps them automatically and nothing reports them, so add this to periodic maintenance on a
long-lived deployment.

---

## `serve:started` precedes the lock check

**Do not treat `serve:started` as "the daemon is up."** `orch serve` emits that record **before** acquiring
the lock, so a `serve` that is about to exit 4 on a lock conflict still logs exactly one `serve:started` and
no error record — the failure leaves by a path that bypasses the log pump.

For monitoring, treat the daemon as up only once you see a record that implies the loop is running — an
`orchestrator:tick`, or any `agent:*` record. Faithful to the original
([PARITY.md §11.1](PARITY.md)); the shape of the log contract is the reason it was not changed.

---

## Debugging

Internal diagnostics go to **stderr** and never to stdout, so they cannot corrupt `--json` output or the log
stream.

| Variable | Effect |
|---|---|
| `ORCHESTRY_DEBUG=1` | Installs the tracing subscriber and dumps the full error chain on failure |
| `RUST_LOG=…` | Standard `tracing-subscriber` filter, e.g. `RUST_LOG=orch_engine=debug,orch_infra=trace` |
| `ORCHESTRY_GLOBAL_CONFIG_PATH=<path>` | Redirects `~/.orchestry/global.yml`. Useful for isolating a test run from your real settings |

```bash
ORCHESTRY_DEBUG=1 orch run <task-id> 2>debug.log
```

Without `ORCHESTRY_DEBUG`, tracing output is suppressed entirely — including the one signal that
distinguishes "another process holds the lock" from "I could not read the lock file", since both print
`PID: undefined`. **If you are diagnosing a lock problem, set it.**

Other useful reads:

```bash
orch logs --task <task-id>            # run metadata + rendered events
orch logs --task <task-id> --json     # the Run records, including the full prompt
cat .orchestry/runs/<run-id>.jsonl    # raw append-only event log
orch doctor                           # adapter availability — note it checks nothing operational
```

`orch doctor` reports whether the AI CLIs are installed and whether you are in a git repository. It does
**not** check the lock, the state file, index consistency, or disk space, and it **always exits 0**.

---

## Stray agent subprocesses after a cancel

**Symptom.** You cancelled a task, force-stopped an agent, or shut the orchestrator down, and a process the
agent started is still running — often reparented to init (`ppid 1`).

**Cause.** Stopping an agent sends `SIGTERM` to the whole **process group**, then waits for the **direct
child** to exit and returns. The final `SIGKILL` sweep of the group never happens. A process forked in the
window between the group signal and the child's death **inherits the group without having been signalled**,
survives, and is reparented. Load-dependent, so it shows up under contention rather than reliably.
Faithful to the original — its `killWithGrace` returns the same way ([PARITY.md §8.4](PARITY.md)).

**Spotting it.** Agents are spawned into their own process group whose **group id is the child's pid**, and
that pid is in the run record:

```bash
# the pid for a task's most recent run
orch logs --task <task-id> --json | grep -o '"pid":[0-9]*'
# or straight from disk
grep -h '"pid"' .orchestry/runs/*.json

# anything still alive in that group
ps -eo pid,ppid,pgid,stat,etime,args | awk -v g=<pid> 'NR==1 || $3==g'
```

A row with `ppid 1` and an `etime` older than your cancel is a survivor.

**Cleanup.** Signal the group explicitly, which is the step that was skipped:

```bash
kill -TERM -<pid>      # note the leading minus: the GROUP, not the process
sleep 2
kill -KILL -<pid>      # if anything remains
```

Check before you do: confirm the group id is the one from the run record, and that no *current* run is using
it. A recycled pid would make you kill an unrelated group.

**Prevention.** None available at the operator level — it is inherent to the cancel path. If stray processes
are a recurring problem in your deployment (long-running builds, test runners that fork), prefer
`workspace_mode: worktree` so survivors at least operate on a throwaway checkout rather than your working
tree.

---

## `timeout_ms` is not a timeout

**It is accepted, stored, displayed — and never enforced.** `orch agent add --timeout <ms>` and
`defaults.agent.timeout_ms` (default `3600000`) write a field that **no code reads**. The same is true in the
TypeScript original. If you set it expecting a run to be killed after an hour, nothing will kill it.

**What actually bounds a run** is `stall_timeout_ms` (default `600000`), and only partly:

| Situation | Bounded? |
|---|---|
| Agent emits no events for `stall_timeout_ms`, under `run --watch` or `serve` | **Yes** — the run is killed and the task fails or retries |
| Agent keeps emitting events but never finishes | **No** — every event refreshes the stall clock, so it runs forever |
| `orch run <task-id>` or `orch run --all` | **No stall detection at all** — these wait for completion, so a hung agent wedges the command indefinitely |

The last row is the one that catches people: stall detection lives in the watch loop's reconcile phase, so
the one-shot commands have none of it.

**What to do.**

- **Don't rely on `timeout_ms`.** Leave it at the default; changing it has no effect.
- **Bound it outside r-orch.** For one-shot runs, wrap the command:
  ```bash
  timeout 3600 orch run <task-id>
  ```
  In CI, set a job-level timeout — that is your real backstop.
- **Prefer `serve` or `run --watch` for unattended work**, so `stall_timeout_ms` is at least active, and tune
  it per agent (`stall_timeout_ms` in the agent's config) rather than reaching for `timeout_ms`.
- **To stop a wedged run by hand:** `orch task cancel <task-id>`, then check for survivors as
  [above](#stray-agent-subprocesses-after-a-cancel).

Both the inert field and the one-shot gap are replicated from the original rather than fixed, because the
field name and shape are part of the `--json` contract ([PARITY.md §6.5](PARITY.md)).

---

## Nested agent sessions

**Hazard, with no guard in the product.** If you run `orch` from inside an agent session — a Claude Code
session, say — and dispatch a task to an agent whose adapter is `claude`, r-orch spawns a **nested** `claude`
process inside the session you are already in. Nested sessions compete for the same credentials and rate
limits, produce interleaved output, and are hard to attribute.

**r-orch itself does not prevent it.** It sets exactly three environment variables on every child —
`ORCH_AGENT_ID`, `ORCH_AGENT_NAME`, `ORCH_TASK_ID` — and inspects **no** session marker. There is no
`CLAUDECODE` check in r-orch or in the TypeScript original, and no guard is planned in either. Do not expect
one.

**In practice the `claude` CLI usually stops it for you — by failing.** Recent Claude Code versions set
`CLAUDECODE=1` in the session environment, and the `claude` CLI refuses to start a *nested* session when it
sees it. Adapters inherit the parent environment and only overlay their own keys — verified: nothing in
r-orch calls `env_clear`, so every variable in your shell reaches the child — so `CLAUDECODE` arrives with
it. The visible result is that **every `claude`-adapter agent exits immediately**, the task fails, and
`orch logs` shows a near-instant non-zero exit rather than any useful agent output. If you see that pattern
inside an agent session, this is why; it is the outer CLI declining, not an r-orch fault.

**What to do:**

- **Preferred — run from a plain shell.** `orch serve` and `orch run` belong outside any agent session.
- **If you must drive r-orch from inside one**, use `shell`-adapter agents: they spawn `bash -lc <command>`
  and start no AI CLI, so the guard is irrelevant.
- **To force it anyway**, strip the variable for r-orch and everything it spawns:
  ```bash
  env -u CLAUDECODE orch serve
  ```
  Understand what this does: it removes the guard rather than satisfying it, so you get genuinely nested
  sessions competing for the same credentials and rate limits, with interleaved output that is hard to
  attribute. Use it knowingly, not as the default fix.

---

## Mouse capture holds the pointer

**Symptom.** `orch tui` is running and you cannot select text with the mouse: dragging moves the
dashboard's selection instead of highlighting, and your terminal's own copy-on-drag does nothing.

**Why.** The dashboard asks the terminal for mouse reports (DEC 1000 + 1002 + 1006), so button events are
routed to the application instead of to the terminal's selection handling. That is the trade mouse
support makes; PARITY.md §12.11 records it.

**Fix**, in increasing order of permanence:

```bash
# 1. In the dashboard: release the pointer for this session.
#    F2, or type the command:
/mouse off          # /mouse on to re-enable, /mouse status to check

# 2. Keep it: hold Shift while dragging (most terminals bypass reporting).
#    NOT under tmux — see below.

# 3. Start with it off, every time:
ORCH_TUI_MOUSE=0 orch
```

While capture is off the footer shows `⌨ F2 mouse`, so there is never a doubt about which mode you
are in.

**Under tmux.** Shift+drag is **not** the bypass here: tmux holds the *outer* terminal's mouse mode,
so Shift+drag selects raw outer-terminal cells, spanning pane borders and neighbouring panes rather
than the pane's text. Use `F2` or `/mouse off`, which also gives tmux's own drag-to-copy back.
`prefix [` copy mode works either way. And note that tmux must have `set -g mouse on` for clicks to
reach the dashboard in the first place — with it off, mouse support silently does nothing, which is
not a fault.

---

## A leaked mouse or paste mode after an abnormal exit

**Symptom.** `orch tui` is gone, but clicking in that terminal prints gibberish like `[<0;44;8M`, or
the shell receives it as input. Or a paste into that terminal arrives wrapped in `^[[200~` … `^[[201~`
(often shown as `[200~`), with the markers landing in your shell command instead of the text.

**Why.** Mouse reporting and bracketed paste are terminal modes the application turns on and is
responsible for turning off. Every normal exit does: `q`, a signal, a panic, and a terminal that hangs
up all run the same restore (PARITY.md §14.39), and the disables are written *first*, before leaving
the alternate screen. `SIGKILL` and an abort cannot be covered — nothing runs on the way out.

**Fix.**

```bash
printf '\033[?1006l\033[?1002l\033[?1000l\033[?2004l'   # mouse SGR, mouse, bracketed paste
reset                                         # or just reset the terminal wholesale
```

---

## Secrets in agent config

**Agent configuration can hold credentials. r-orch protects those files better than you might expect — and not well enough to put a key in one.**

An agent's `env:` map is written verbatim into `.orchestry/agents/<agent-id>.yml` and passed to every child
process it spawns. So this puts a live key on disk in plain text:

```yaml
config:
  env:
    ANTHROPIC_API_KEY: sk-ant-...      # ← don't
```

**r-orch protects the file locally.** `.orchestry/agents/<id>.yml` and `.orchestry/config.yml` are written
**`0600`**, owner-only — and the mode is set when the temp file is created, so the file is never even briefly
world-readable. The TypeScript original writes them at the process umask (typically `0644`), so this is one
of the few places r-orch is deliberately stricter ([PARITY.md §14.35](PARITY.md)).

**And git ignores the whole directory by default.** `orch init` appends `.orchestry` to your project's
**root** `.gitignore` (creating it if absent), so no entity file is committed unless you go out of your way.
Verify on your own project:

```bash
git check-ignore -v .orchestry/agents/*.yml     # should name the root .gitignore rule
git status --short | grep orchestry             # should be empty
```

**So the exposure is narrower than it looks — but it is not zero.** What remains:

1. **Someone force-adds or un-ignores the directory.** `git add -f .orchestry` works, and teams sometimes
   un-ignore it deliberately to share agent definitions. Once a key is in a repository, **`0600` protects
   nothing** — git preserves no mode beyond the executable bit, and anyone who can read the repo can read
   the key.
2. **Only those two files are owner-only.** `tasks/`, `goals/`, `teams/`, `messages/` and `context/` keep
   umask modes, so a credential pasted into a task description or a context value **is** world-readable to
   other local users.
3. **The child process still receives it.** `env:` is passed to every agent the entry spawns, so anything
   that agent logs, echoes or sends onward can leak it.
4. **Backups and sync tools do not read `.gitignore`.** Dropbox, Time Machine, `rsync` without
   `--exclude` and most backup agents copy `.orchestry/` verbatim.

**Do this instead.** Leave the key out of the agent and let it inherit from the environment — children
receive the parent environment, overlaid with `env:` and the three `ORCH_*` variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
orch serve
```

Use `env:` only for non-secret, per-agent settings.

**If a key has already reached a repository**, treat it as disclosed: rotate it first, then purge it from
history. Rotation is the step that matters — a purge alone leaves the key live in every existing clone.

**If you deliberately un-ignore `.orchestry/` to share agent definitions across a team** — a reasonable thing
to want, since those files *are* your agent and task definitions — then re-ignore the parts that can hold
credentials, and keep versioning the rest:

```gitignore
!.orchestry/
.orchestry/agents/
.orchestry/config.yml
```

The guidance is the same either way: **version the definitions if you want to, keep secrets in the
environment.**

---

## Remote task phases

**A task dispatched to a peer is `in_progress` here from the moment the controller reserves it** — before a
byte reaches the worker. So task views do not show that status for remote work; they show the phase the
worker last reported, in `orch task list`, `orch task show` and the dashboard's status chip:

| Shown | Means | What to do |
|---|---|---|
| `dispatched → 2com` | The request is queued locally. The worker has not acknowledged it. | Nothing, unless it stays. After **5 minutes of link time** (offline time does not count) the task fails with `remote 2com never acknowledged submit` and a cancel goes to the worker. |
| `queued @2com` | The worker holds a mirror that has not started. | If the WAITING column says `awaiting approval: orch task approve <id> on 2com`, that command has to be run **on the worker** — its own owner decides whether a peer may use a `suggest`/`manual` agent. |
| `RUN @2com` | The worker says its agent is executing. | Nothing. This is never shown without a running signal from the worker. |
| `RUN? (2com offline)` | The link dropped after the submit was written, so nobody here knows what the worker has. | Nothing. The next session's sync resolves it to queued, running or finished — or to a failure if the worker has lost the record. |

**A restart or a shutdown does not change any of this.** A graceful exit
(`SIGTERM`, the dashboard's `/pause`, a service-manager restart) **detaches** a
remote run: no cancel is sent, the worker keeps executing, and the entry stays
in `state.running` as `RUN?` for the next start to pick back up. Only a peer
that has been **removed** from the registry turns its runs into
`remote <alias> removed; outcome unknown`.

Two messages come out of the deadlines:

- **`tsk_x has been queued on 2com for 1h`** (`orchestrator:error`, once an hour, with the approve command as
  its context). A queued mirror is usually waiting for the worker owner's approval, which this side cannot
  grant, so it is **never** failed automatically however long it waits.
- **`remote lost execution record (2com: worker storage error: corrupt record)`**. The worker answered a sync
  with `unknown`: it has no execution record for the key. The parenthesis is the worker's own reason, and it
  is the part that says where to look — on the worker, in that example, at the mirror's task file. The task
  fails without a retry, and a cancel goes out in case the mirror becomes readable again.

### What the deadlines do not cover

- **`RUN?` has no deadline.** An execution nobody can account for — a dropped
  link, a detached shutdown, a rehydrated restart — waits for a sync, for as
  long as that takes. Nothing times it out, because the only thing that can
  resolve it is the worker. If the peer never returns, `orch remote remove
  <alias>` is the deliberate way to end it (that fails its runs as
  `removed; outcome unknown`).
- **The clocks live in memory.** A restart starts every phase clock again, so a
  dispatched run gets a fresh five minutes and a queued one a fresh hour.
- **A clock counts reconciles, not wall time.** Each pass credits at most two
  poll intervals, so a laptop that slept for eight hours does not fail a
  dispatched run on the first tick after it wakes.
- **A deadline is final for the controller, not for the worker.** When a
  dispatched run is failed, a Cancel goes out, but if the worker's acceptance
  was merely slow, a success landing after that is dropped: the task is
  terminal and the key is no longer tracked, and the worker's mirror is
  cancelled on the next sync. Check the worker before re-running such a task,
  or the work may happen twice.

---

## Reading a peer's link state

`orch remote status` and `orch remote list` no longer print a bare `offline`, which covered three very
different situations:

```
  2com: offline (Peer closed connection; retry in 12s, attempt 5)
  2com: offline (Worker busy: controlled by mac; retry in 8s)
  2com: offline (connecting)
  2com: online v0.5.0
```

- **`attempt N`** is the number of consecutive dials since the last session that proved the peer healthy. A
  climbing count with a short `retry in` is the signature of a worker that accepts a connection and then
  closes it — look in the **worker's** `serve.log` and at `orch remote status` there, which reports
  `Listen: … (last session ended: <error>)`.
- **`retry in Ns`** counts down to the next dial. The backoff is 1, 2, 4, 8, capped at 15 seconds, each with
  ±25 % jitter — so the longest real wait is about 18.75 s — and it only resets after a session has cleared
  its sync fence and stayed up for 30 seconds. The disconnect event in the activity feed is debounced to one
  per distinct error per 30 s, so a peer that drops every session reports once, not once per redial; the
  `attempt` counter is what shows the rate.
- **`(version unknown)`** beside an online peer means that peer predates the version field in the handshake.
  Nothing is wrong with the link; it is an older build.

---

## Mirrors left behind on a worker

A **mirror** is the task a worker creates for a controller's request (labelled `remote-ctl:<controller id>`).
A mirror runs only while the controller that submitted it is connected, so one left behind by a controller
that never comes back would sit `todo` in a local agent's queue indefinitely.

The worker sweeps for those hourly, and once when `orch serve` starts listening. A `todo` mirror whose
controller has not been admitted for **24 hours** is cancelled with:

```
orphaned remote mirror: controller <instance id> absent since <timestamp>
```

A mirror can also be cancelled for two other reasons, and both say so on the
task:

- **`orphaned remote mirror: controller <id> no longer tracks execution <key>`**
  — the controller synced and did not list that key, so it has already
  accounted for the execution (finished, failed, or given up as unknown). This
  is the common one after a controller failed a run whose mirror it could not
  read.
- **`cancelled by controller <name>`** — an `orch task cancel` on the
  controller, or its reconcile, reached this worker.

Three things are deliberately *not* swept:

1. **`in_progress` mirrors.** They own a live local process, and the quarantine path owns that pid.
2. **The connected controller's mirrors.** A controller that is here now is not absent.
3. **A controller this worker has never recorded.** Its 24 hours start at the first sweep that sees it, which
   is also what spares mirrors that predate the feature.

Last-seen timestamps live in `.orchestry/remotes/_controllers.json` (owner-only). Deleting it restarts every
controller's clock; it is safe to delete.

To cancel one by hand, on the worker: `orch task cancel <mirror task id>`.

---

## Peer protocol compatibility

**Within one `PROTOCOL` number, every peer must tolerate every frame variant and every omitted field.** Both
sides of a link can be different builds — that is the normal case for a LAN of machines upgraded one at a
time — so a change that keeps the current `PROTOCOL` may only **add fields**, each with
`#[serde(default)]` (to read an old peer's frame) and `skip_serializing_if` (so an old peer reads ours
byte-identically to what it used to see).

A **new frame variant, a removed field, or a changed meaning for an existing one requires a `PROTOCOL`
bump**, because an old peer treats a frame it cannot parse as fatal and closes the session. A mismatched
`PROTOCOL` is refused at the handshake with `Peer protocol mismatch`, which is the honest failure: both
owners then know to upgrade.

**Downgrading is not symmetric.** A 0.5.0 build understands every frame this release sends, but it does
not have the behaviour behind them: it **cancels** remote runs on a restart or a `SIGTERM` instead of
detaching them, it never sweeps orphaned mirrors on a worker, and it shows a dispatched or queued remote
task as `RUN`. So a downgrade with remote runs in flight loses them, and the work may already have happened
on the worker. Drain remote work first, or check each worker's task list afterwards.

The rule is enforced by tests in `crates/orch-peer/src/protocol.rs`, in both directions — current frames
parsed by a copy of the previous release's shapes, and previous-release JSON parsed by the current frames.
If you add a field, extend those tests; if you cannot keep them passing, you need the bump.

## Copying text from the dashboard

1. Focus a text section or open its viewer. Ctrl+Y copies the whole field/representation; viewer `y` copies its selection. Open F6 and choose
   Copy selection or an explicit destination with arrows/Tab and Enter. F6, End, Enter explicitly chooses **Copy on this host**. The mouse
   Copy chip/carousel offers the same actions. F1 shows the full matching request status and route guidance.
2. **Copy requested from terminal** acknowledges emission only. The terminal must support and permit OSC 52.
   Under tmux, `set-clipboard on` permits application requests subject to terminal policy; `external` ignores
   application requests. Orch never changes this setting. Choose Copy on this host when the host clipboard is
   the intended destination. With SSH, that host may be the server rather than your client.
3. Under GNU screen the unwrapped OSC path is unsupported. Choose the host route or retry outside screen in a
   supporting terminal. Auto under screen still requests the terminal; it does not write both clipboards.
4. In `orch web`, request copy, then click **Copy to clipboard**. A secure context and browser permission are
   needed. Denial or an insecure page exposes a selectable text box. Select that text and use the browser's
   ordinary copy command. The client never reads the clipboard.
5. **Busy** means the bounded host worker/tickets are occupied. Let results drain; a timed-out OS call cannot
   be killed by the UI and no replacement worker is spawned. Native failure does not trigger a terminal write.
   Requests over 64 KiB on the terminal route or 1 MiB on the host route are refused whole; select less text.

The viewer includes the pressed glyph in a drag. Textarea selection uses insertion-position endpoints, so the
same reverse drag can copy one fewer glyph there. Both preserve original source bytes. F2 releases capture for
terminal-native selection. Hardware Option/Alt delivery depends on the terminal profile; use F6/arrows/Enter
when modified copy keys are unavailable. Native Terminal.app/iTerm2 profiles and a real clipboard smoke have
not been verified for this change; fake-sink tests do not prove OS delivery.

## Editor exits and session drafts

A changed editor offers Continue, Keep draft and Discard before navigation drops its edits. Keeping a draft
uses RAM only, keyed to the edited form/entity. Reopening offers recovery; full/oversized draft storage refuses
the save visibly. Quitting with drafts asks for confirmation and discards them on exit. A signal exits without
an interactive prompt. Drafts do not survive crashes or restart, and wall-clock backsteps are not a durability
mechanism. On a primary submit error, correct the retained form and retry. A partial post-create failure names
the created entity; repair that entity rather than creating a duplicate.

## The agent shop picker

**Symptom.** `orch agent shop` prints `  Cancelled.` at once, or prints a table instead of the interactive picker.

**Cause.** The picker draws only when stdin **and** stdout are terminals. With stdout redirected (a pipe, a file,
a CI log) the command prints the same table as `--list`; with a terminal on stdout but stdin not a terminal
(`echo | orch agent shop`) it cancels immediately. Both are exit 0 and neither creates an agent. The command
needs an initialised project; elsewhere it prints `Not initialized` / `Run: orch init`. `--json` and `--quiet`
are ignored here.

**Fix.** Run it from an interactive terminal inside a project, or use `orch agent shop --list` and add the
agent you want with `orch agent add`.

**Display.** The picker fits every row to the terminal width and counts its heading, blank lines and footer
against the terminal height, so it should never wrap or scroll the heading away. On a terminal narrower than
the full help it chooses the widest heading that fits: `AGENT SHOP  ↑↓ move · enter select · q cancel`,
then `AGENT SHOP  ↑↓ Enter q`. At 30 and 40 columns the compact heading keeps all three keys visible;
below its display width it is cut with an ellipsis. The footer `n-m of total` appears only when the list is
taller than the terminal. The template slice reads the current size before each frame, retaining the event-loop
correction for a resize received together with a keypress; heading state-machine tests alone do not prove that case.

## Structured output for `doctor`, `logs --follow` and `serve`

**Symptom.** `orch --json doctor` (or `--yaml doctor`) piped into a parser fails on the first line;
`orch --yaml logs --follow` or `orch --yaml serve` prints text or JSON Lines, not YAML.

**Cause.**
- `doctor` prints its human header (`  orch doctor · checking adapters and dependencies`) to **stdout** before the
  structured document, whichever of `--json` or `--yaml` is given. The header is older than `--yaml` (master
  prints it under `--json` too), and `--yaml` does not change it. Outside a project, `doctor` ignores both flags
  and prints the text report.
- `logs --follow` prints a text banner and then live events; `serve` writes JSON Lines (`--log-format json`, the
  default) or text. `--yaml` changes neither, and it never changes the on-disk `.jsonl` logs.
- A failing command prints its error as text on stderr under every output flag; stdout is empty.

**Fix.** For `doctor`, drop the leading header lines before parsing: `orch --json doctor | sed '1,2d'`. For
streams, parse the JSON Lines `serve` writes, or poll `orch --yaml logs --task <id>` for run metadata. To tell an
error from output, check the exit code; never parse stderr as YAML or JSON.

## File mutations: `update` and `patch` failures

Applies to `orch agent|task|config update|patch -f FILE` ([guide](file-mutations.md)). Every failure leaves the
definition unchanged, because parsing, merging and validation finish before anything is written.

| Symptom | Cause | Fix |
|---|---|---|
| `<source>: mutation lock timed out after 5 s; retry` (exit 1) | Another `update`/`patch` (or a hung one) holds `.orchestry/.definition.lock`. The lock is released when its process exits, however it exits. | Retry. If it persists, find the process (`lsof .orchestry/.definition.lock`) and stop it. Do not delete the file. |
| `read mutation input` (exit 1) | The `-f` file cannot be read: missing, unreadable or a permission problem. The message does not name the path or the OS reason. | Check the path and permissions yourself (`ls -l FILE`). |
| `<source>: <field>: read-only field` / `unknown field` / `required string` (exit 2) | The document names a field that cannot be written, does not exist or is missing. | Fix the document. `orch --yaml agent show <id>` shows the shape; read-only fields can be echoed back in `update` only when unchanged. |
| `Agent not found: <name>` (exit 1) | `update`/`patch` take the id (`agt_…`), not the name. | `orch agent list` for the id. |
| `cannot mutate a running or remote agent definition` (or task) | The target is running, `in_progress`, remote (`@` in the id) or mirrors a remote task. | Stop or cancel the run first, then retry; remote entities are changed on their own host. |
| `labels: remote-ctl: and exec: prefixes are reserved for worker mirrors` | A label uses a reserved prefix. | Use another label. |
| You are unsure whether a failed or interrupted command saved | The write is a temporary file renamed over the target; a crash after the rename but before the output has still saved. | `orch agent show <id>`, `orch task show <id>` or `orch config get <key>` before retrying. |

Mutations cannot interlock with `edit`, `agent add`, a running orchestrator or the dashboard: those keep
last-writer-wins behaviour. For bulk changes, stop the runner and use one writer. To roll back, run `update` with
the definition you saved earlier (`orch --yaml agent show <id> > before.yaml`).

## Organization templates: failures and recovery

Applies to `orch org deploy`, `orch agent shop --template…` and `orch org template …`
([guide](org-templates.md)). Template commands change one file at a time and validate the whole template first, so a
refused command leaves the template unchanged; `deploy` is the only command that can leave work behind.

| Symptom | Cause | Fix |
|---|---|---|
| `Template deployment failed at <stage>: … Surviving entities: {"agentIds":[…],"teamId":…,"goalId":…,"unverifiedIds":[…]}` (exit 1) | A step failed after earlier ones succeeded. Nothing is rolled back. | Fix the cause (a full disk, a bad `.orchestry/` file), then remove the survivors: `orch agent remove <id>` for each agent, `orch team disband <team>`, `orch goal delete <goal>`. `unverifiedIds` could not be checked: look for their files. |
| After cleaning up, the same deploy fails at `team` with `Team "<name>" already exists` and leaves new agents | A disbanded team keeps its name, and deploy checks agent names but not team names before creating anything. | Remove the new agents again, then deploy a copy under another name (`orch org template init <new> --from user:<name>`), or delete the leftover `.orchestry/teams/<team-id>.yml` by hand (not a supported interface). |
| `Agent "<name>" already exists; no agents created` (exit 2) | A member name is taken. Nothing was created. | Remove or rename the existing agent, or edit the member's name in a copy of the template. |
| `Builtins are read-only; run orch org template init … --from builtin:…` | You tried to change `a-team` or a legacy preset. | Copy it with `init --from`, then change the copy. |
| `<file>: <reason>` from `list`, `show`, `deploy` or the shop (exit 2) | A file in the template fails validation. `list` shows the reason for each user template. | Fix the named file. `org template update` with a complete definition repairs one broken `org.yml` or member; `patch` and `edit` need a file that parses. |
| `name is immutable` / `schema_version is immutable` | `org.yml` has a present `name` or `schema_version` (even `null`, or an unsupported version) and the update changes it. | Edit the file by hand. An *absent* key can be repaired with `update`. |
| `…/agents/<file>: exceeds 1 MiB` on every command | A file larger than 1 MiB blocks the whole template, `update` included. | Shrink or delete it by hand. |
| `Timed out after 5 s locking …/.template.lock` (exit 2) | Another template command holds the lock. The lock is released when its process exits. | Retry. Do not delete the file. |
| `expected directory without symlinks` | The selected template root, with or without a trailing `/` or `/.`, is a symlink. | Point `--template-dir` at the real directory. |
| The shop refused a member and the screen shows nothing | The refusal (`Agent "<name>" already exists; no agents created`) goes to stderr and exit status 2; the picker may clear the screen. | Check `echo $?` and `orch agent list`. |
| A deployed agent does not follow the template after you edited it | Deployed agents are snapshots; nothing synchronises them. | Remove the agent and deploy or shop again. |

A change made while another tool writes the same template, or a hand edit during a command, is not covered by the
lock: the last writer wins. If a command was interrupted, run `orch org template show` before repeating it, because
a crash after the file was renamed has still saved the change.
