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
| Running r-orch from inside a Claude Code session | [Nested agent sessions](#nested-agent-sessions) |
| Worried about API keys in agent config | [Secrets in agent config](#secrets-in-agent-config) |

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

## Secrets in agent config

**Agent configuration can hold credentials, and those files are not protected.**

An agent's `env:` map is written verbatim into `.orchestry/agents/<agent-id>.yml` and passed to every child
process it spawns. So this puts a live key on disk in plain text:

```yaml
config:
  env:
    ANTHROPIC_API_KEY: sk-ant-...      # ← don't
```

Two things make that worse than it looks:

1. **The files are world-readable.** Writes use the process umask, so entity files land at **`0644`** under a
   typical `umask 022`. Any local user can read them.
2. **`orch init`'s `.gitignore` does not cover them.** It ignores `state.json`, `*.lock`, `runs/`, `logs/`
   and `workspaces/` — **`agents/`, `tasks/`, `goals/`, `teams/`, `messages/` and `context/` are all
   committed.** An API key in `env:` goes into your repository history on the next `git add`.

**Do this instead.** Leave the key out of the agent and let it inherit from the environment — children
receive the parent environment, overlaid with `env:` and the three `ORCH_*` variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
orch serve
```

Use `env:` only for non-secret, per-agent settings.

**If a key has already been committed**, treat it as disclosed: rotate it, then purge it from history. And
consider adding the entity directories to your own `.gitignore` if you do not intend to version your task
and agent definitions:

```
.orchestry/agents/
.orchestry/context/
.orchestry/messages/
```

Note the trade-off — those files *are* your project's agent and task definitions, and versioning them is
often the point. Keep them versioned and keep secrets in the environment.
