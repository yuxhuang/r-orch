# ORCH → r-orch Parity Contract

**Reference implementation:** `oxgeneral/ORCH` @ `c066dc0` (npm `@oxgeneral/orch` v1.0.34), vendored read-only at
`parity/orch-ts/` (gitignored; re-created by `parity/setup.sh`).

**Baseline:** `CI=1 npx vitest run` → **2085 passing / 0 failing / 8 skipped**, exit 0. ~26.2k LOC under
`src/`. Running without `CI=1` produces 5–6 *flaky* failures that differ run to run — see
[§14.9](#149-the-1-failing-baseline-test-is-actually-suite-flakiness--do-not-replicate). Always use `CI=1`
when invoking the reference suite.

This document is the **authoritative specification** for the Rust rewrite. Where this document and the TypeScript
source disagree, the TypeScript source wins and this document is a bug — except for the items in
[§14 Known bugs](#14-known-bugs-in-the-original), which record deliberate divergences.

### Normative appendices

This document is the contract; the appendices under `docs/parity/` are the **exhaustive per-file detail** behind
it, derived directly from the source. They are normative — where this document summarizes, they specify. Read
the appendix for your work package before implementing.

| Appendix | Covers | Lines |
|---|---|---|
| [`docs/parity/cli.md`](parity/cli.md) | Every command: globals and routing (§1), output primitives (§2), errors and exit codes (§3), one section per command (§4–19), storage/ids/serialization (§20), interactive and non-TTY degradation (§21), ranked parity gotchas (§22) | 3384 |
| [`docs/parity/adapters.md`](parity/adapters.md) | Shared machinery, per-adapter argv/parsing/errors, the `Adapter` contract verbatim, `ProcessManager`, model discovery, model tiers, platform notes, hazards | 3493 |
| [`docs/parity/infra.md`](parity/infra.md) | Template engine (incl. byte-exact template dumps), skill loader, workspace/git, clipboard, the full agent-shop and org-shop catalogues, scope, doctor, review-runner, event bus | 2496 |
| [`docs/parity/tui.md`](parity/tui.md) | Views and columns, the complete key-dispatch chain, overlays, command bar, all 11 wizards, layout math, live-update timers, dead code, test inventory | 1001 |
| [`docs/parity/tests.md`](parity/tests.md) | Per-file test inventory with classifications, helper/mock infrastructure, process-faking mechanisms, vitest config, flakiness analysis, ranked parity oracles | 743 |

---

## 1. Parity levels

Every requirement in this document carries a level. Engineers must know which one applies before choosing how
much effort to spend.

| Level | Name | Meaning | Verified by |
|---|---|---|---|
| **L0** | **Wire contract** | Byte- or value-exact. A third party (script, CI job, the other binary) depends on it. Changing it is a breaking change. | Differential harness, exact assertions |
| **L1** | **Behavioural** | Same observable behaviour and semantics; incidental representation may differ. | Ported unit/integration tests |
| **L2** | **Best effort** | Same feature and intent; presentation may differ. | Manual checklist, smoke tests |

### 1.1 What is L0

1. **`.orchestry/` on-disk format** — every file's path, name, format (YAML/JSON/JSONL) and the *parsed value* of
   every field. Both binaries must operate on the same project directory interchangeably, in either order.
2. **CLI command and flag surface** — command names, subcommand names, aliases, positional arity, long and short
   option names, and option-argument arity.
3. **Exit codes.**
4. **`--json` output** — the parsed JSON value: key names, nesting, types, array-vs-object, and how empty results
   are represented.
5. **Prompt text produced by the default templates** — byte-exact, including blank-line runs. Agents are sensitive
   to prompt text and it is also the input to prompt caching.
6. **ID formats** — prefix and character set.
7. **Adapter argv** — the exact command line handed to each external AI CLI.

### 1.2 What is L1

Task state machine and all transitions; retry/backoff arithmetic; tick phase ordering; stall/zombie detection;
lock acquisition and staleness; concurrency limiting; dispatch candidate ordering; scope-overlap blocking;
cascade failure; auto-review; goal/team/message/context semantics; event emission (the set of events and their
payload fields); adapter stdout parsing and error classification; workspace/worktree git command sequences;
skill loading; serve-mode log record *fields*.

### 1.3 What is L2

TUI pixel layout, colours and glyph choices; spinner frames and animation timing; colour/ANSI styling of human
output; log *text* format in `serve --log-format text`; progress and status decoration.

**`--help` output is L2 — it is clap's, not Commander's.** Wording, wrapping, option ordering and section
headings all differ, and no attempt is made to reproduce Commander's layout. What *is* L0 around help:

- the **command, subcommand, alias and flag names** themselves (§2.2) — these are what `--help` advertises and
  what scripts call;
- the **stream** help goes to: stdout for an explicit `--help`, **stderr** for the bare-command-group path
  (§3);
- the **exit code**: 0 for explicit `--help`, 1 for a bare command group, 3 in an uninitialized directory.

So a harness scenario asserts the stream and exit code of a help invocation, never its body.

### 1.4 Normalization rules for comparison

The differential harness ([ARCHITECTURE.md §8.2](ARCHITECTURE.md)) compares after applying these:

| Aspect | Rule | Why |
|---|---|---|
| JSON object key order | **Not contract.** Compare parsed values. | TS key order is JS *insertion* order (e.g. `Run` emits `pid` and `finished_at` after `prompt` because they are assigned later; `Message` emits `id`/`to_agent_id` last after an object spread). Rust `serde` emits struct declaration order. RFC 8259 objects are unordered. |
| YAML emitted bytes | **Not contract.** Compare parsed values — **parsed by both parsers and cross-checked**, see below. | js-yaml folds long single-line scalars with `>-` at `lineWidth: 120`; Rust emitters pick `\|-` or a quoted style. Measured to round-trip identically in both directions across all multiline edge cases (§6.4.1). |
| Toolchain row in `doctor` output | **Masked.** The `node` row becomes the r-orch build identity (§13). | A named, deliberate divergence — masking it is honest; calling it "L2 decoration" would not be. |
| `~/.orchestry/update-check.json`, `update-installed.json` | **Excluded.** The TS binary writes them, r-orch never does (§6.1, §11.5). | Not shared state in r-orch. The harness neutralizes them rather than comparing them. |
| YAML mapping key order | **Not contract**, but SHOULD match the TS interface field order for human diff friendliness. | `writeYaml` uses `sortKeys: false`. |
| IDs | Replace `/(tsk|agt|run|goal|team|msg)_[A-Za-z0-9_-]{7}/` with `<\1_ID>`, consistently per-run. | nanoid is random. |
| Timestamps | Replace ISO-8601 instants with `<TS>`; preserve ordering relations separately where asserted. | Wall clock. |
| **Local clock times** in human output — `HH:MM:SS` from `toLocaleTimeString('en-US', { hour12: false })` in `orch run`, `orch logs` and the TUI activity feed | Replace with `<TIME>`. | The two binaries run seconds apart, so the values differ legitimately; and the TS form is **local time**, not UTC (§5.4). The *presence and position* of the field stays contract. |
| `heap_mb` in `serve` tick records | **Masked.** | V8 heap vs process RSS — no faithful counterpart (§11.1). The field name is contract, the value is not. |
| `orchestrator:tick` records in `serve` output | **Dropped** on steps marked `@ticks-vary`. | Tick count is a function of wall-clock duration vs `poll_interval_ms`. Every other record type on those steps is still compared. |
| Absolute paths | Replace the temp project root with `<ROOT>`. | Temp dirs. |
| PIDs | Replace with `<PID>`. | OS-assigned. |
| Durations / `*_ms` measured values | Replace with `<MS>`. | Timing. |
| ANSI escapes | Strip from human stdout before comparing. | L2 styling. |
| Trailing whitespace on human table rows | Significant — do **not** strip. See §5.1. | The TS table writer pads every cell including the last. |

**"Compare parsed values" means parsed by *both* implementations' parsers, cross-checked.** Verifying both
trees with the Rust parser alone would make every bug of the form *"js-yaml reads r-orch's bytes differently"*
invisible by construction — the two trees would agree under a parser that shares the writer's assumptions. So
every YAML artifact in both trees is **additionally re-parsed in the TS process with js-yaml** and
deep-compared against the Rust parse, as a harness step (ARCHITECTURE §8.2, V32). That step subsumes the
timestamp assertion (V8) as a special case and is the only check that actually tests what §6.4 calls the
cross-compat crux.

---

## 2. CLI surface

### 2.1 Binaries and global surface

Three binaries, **all the same program**: `orch`, `orchestry`, `ao` (npm `bin` map → `dist/cli.js`).

- Commander program name: `orchestry`. `--help` output begins `Usage: orchestry [options] [command]`.
- Description: `Agents Organizations — CLI orchestrator for AI agents`
- `--version` / `-V` → `1.0.34` (L0 that a version is printed; the value tracks the Rust crate version — see §13).

**Global options** (declared on the root program; L0):

| Flag | Effect |
|---|---|
| `--json` | Machine output. Per-command shapes in §4. |
| `--quiet` | Minimal output (IDs only). |
| `--no-color` | Disable colour. Also implied by the `NO_COLOR` env var being *present* (any value, including empty). |
| `--ascii` | ASCII-only glyphs. Also implied by `TERM=dumb`. |
| `-h, --help` | Help. |

`--json`, `--quiet`, `--no-color`, `--ascii` are parsed as global options and are accepted **before** the
subcommand. Commander also accepts them after; preserve that.

**Environment variables read** (L0):

| Var | Read by | Effect |
|---|---|---|
| `NO_COLOR` | `createContext` | Presence (not value) disables colour |
| `TERM` | `createContext` | `dumb` ⇒ ASCII mode |
| `ORCHESTRY_GLOBAL_CONFIG_PATH` | `GlobalConfigStore` | Overrides `~/.orchestry/global.yml` (trimmed, then `path.resolve`d). **Critical for tests.** |
| `ORCHESTRY_DEBUG` | root error boundary | Also dump the error object to stderr |
| `EDITOR` | `cli/editor.ts` | Interactive editor for `-e/--edit`, `task edit`, `agent edit`, `config edit` |
| `ORCHESTRY_TASK_PROMPT` | **written** by the shell adapter | Full prompt passed to the child |
| `ORCH_AGENT_ID`, `ORCH_AGENT_NAME`, `ORCH_TASK_ID` | **written** by the orchestrator into every adapter child env | |

**Project root resolution** (L0): walk up from `cwd` looking for a `.orchestry` entry (`accessSync`); the first
directory containing one is the project root. If none is found, the resolved start directory is used (so `init`
works). Results are cached per start-dir for the process lifetime.

**Container tiering** (L1, performance only — no observable difference except startup latency):

- *Light* (stores + services): `task`, `agent`, `status`, `logs`, `config`, `context`, `msg`, `goal`, `team`, `org`
- *Full* (+ orchestrator, adapters, process manager, template engine, skill loader, workspace manager, doctor):
  `run`, `doctor`, `tui`, `serve`, **and no subcommand at all** (bare `orch` → TUI)
- *No container*: `init`, `update`
- **`task cancel` lazily upgrades to Full** — but only when the target task is `in_progress` (it needs the
  orchestrator to kill the child process). This is observable as a latency difference only.
- **`doctor` runs in either tier**: with a container it probes **8** adapters; container-less (uninitialized
  project) it probes **5**. The adapter count in the trailing `<n> of 8 adapters ready` line changes
  accordingly.

### 2.2 Command table

Positional args: `<x>` required, `[x]` optional. All options take a value unless marked *(bool)*.

#### `init`
| | |
|---|---|
| Args | — |
| Options | `--name <name>` (project name), `--adapter <adapter>` (default agent adapter: `claude\|opencode\|codex\|cursor\|pi\|grok\|antigravity\|shell`) |
| Exits | `0` always on success |

#### `task` — *Manage tasks*
| Subcommand | Args | Options |
|---|---|---|
| `add` | `<title>` | `-d, --description <desc>`; `-p, --priority <n>` **default `'3'`**; `-l, --labels <labels>` (comma-separated); `--depends-on <ids>` (comma-separated); `--max-attempts <n>`; `--workspace-mode <mode>` (`shared\|worktree\|isolated`); `--assignee <agent-id>`; `--review-criteria <criteria>` (comma-separated of `test_pass,typecheck,lint`); `--scope <patterns>` (comma-separated globs); `--goal-id <goalId>`; `--attach <paths>` (comma-separated); `-e, --edit` *(bool)* |
| `list` | — | `--status <status>` |
| `show` | `<id>` | — |
| `edit` | `<id>` | **none** — round-trips via `$EDITOR` only (§2.4) |
| `assign` | `<task-id> <agent-id>` | — |
| `cancel` | `<id>` | — |
| `approve` | `<id>` | — |
| `reject` | `<id>` | `-r, --reason <reason>` |
| `retry` | `<id>` | — |

#### `agent` — *Manage agents*
| Subcommand | Args | Options |
|---|---|---|
| `add` | `<name>` | **`--adapter <adapter>` (REQUIRED)**; `--role <role>`; `--command <cmd>`; `--model <model>`; `--effort <level>` (`low\|medium\|high`); `--max-turns <n>`; `--timeout <ms>`; `--approval-policy <policy>` (`suggest\|auto\|manual`); `--workspace-mode <mode>`; `--skills <skills>` (comma-separated); `-e, --edit` *(bool)* |
| `shop` | — | `--list` *(bool)* — print all templates non-interactively |
| `list` | — | — |
| `status` | `<id>` | — |
| `edit` | `<id>` | **none** — round-trips via `$EDITOR` only (§2.4) |
| `remove` | `<id>` | — |
| `disable` | `<id>` | — |
| `enable` | `<id>` | — |
| `autonomous` | `<id>` | `--on` *(bool)*, `--off` *(bool)* |

> `--adapter` is a Commander `requiredOption`: omitting it is a usage error (exit 1), **not** an
> `InvalidArgumentsError`. There is no `agent show` — `agent status <id>` is the detail view. Commander's
> did-you-mean suggests `shop` for `show`.

#### `status` — *Show orchestrator status*
No args, no options.

#### `logs` — *View run logs*
| Args | Options |
|---|---|
| `[run-id]` | `--agent <agent-id>`; `--task <task-id>`; `--follow` *(bool)*; `--since <duration>` (e.g. `5m`, `1h`) |

Requires at least one of: positional `run-id`, `--task`, `--agent`, `--since`. Otherwise:
`Specify a run ID, --task, --agent, or --since <duration>` with **exit 2**.

#### `config` — *Manage configuration*
| Subcommand | Args |
|---|---|
| `get` | `<key>` (dot path) |
| `set` | `<key> <value>` |
| `edit` | — (opens `$EDITOR` on `.orchestry/config.yml`) |
| `global get` | `<key>` |
| `global set` | `<key> <value>` |
| `global show` | — |

#### `context` — *Shared context store for inter-agent data exchange*
| Subcommand | Args | Options |
|---|---|---|
| `set` | `<key> <value>` | `--ttl <ms>` |
| `get` | `<key>` | — |
| `list` | — | — |
| `delete` | `<key>` | — |

#### `msg` — *Inter-agent messaging*
| Subcommand | Args | Options |
|---|---|---|
| `send` | `<to-agent-id> <body>` | `-s, --subject <subject>`; `--from <agent-id>` (default `cli`); `--ttl <ms>`; `--reply-to <msg-id>` |
| `broadcast` | `<body>` | `-s, --subject <subject>`; `--from <agent-id>` (default `cli`); `--team <team-id>`; `--ttl <ms>` |
| `inbox` | `<agent-id>` | — |
| `list` | — | `--agent <agent-id>` |

#### `goal` — *Manage goals*
| Subcommand | Alias | Args | Options |
|---|---|---|---|
| `add` | | `<title>` | `--description <desc>`; `--assignee <agentId>` |
| `list` | `ls` | — | `--status <status>` |
| `show` | | `<id>` | — |
| `status` | | `<id> <status>` | `--force` *(bool)* |
| `update` | | `<id>` | `--title <title>`; `--description <desc>`; `--assignee <agentId>` (empty string unassigns) |
| `delete` | `rm` | `<id>` | — |

#### `team` — *Manage teams*
| Subcommand | Args | Options |
|---|---|---|
| `create` | `<name>` | **`--lead <agent-id>` (REQUIRED)**; `--members <ids>` (comma-separated); `-d, --description <desc>`; `--no-auto-claim` *(bool)* |
| `list` | — | — |
| `show` | `<id>` | — |
| `join` | `<team-id> <agent-id>` | — |
| `leave` | `<team-id> <agent-id>` | — |
| `add-task` | `<team-id> <task-id>` | — |
| `set-lead` | `<team-id> <agent-id>` | — |
| `disband` | `<id>` | — |

#### `org` — *Pre-built AI companies*
| Subcommand | Alias | Args | Options |
|---|---|---|---|
| `list` | `ls` | — | — |
| `deploy` | | `<template>` | `--goal <goal>` |

#### `run` — *Run tasks*
| Args | Options |
|---|---|
| `[task-id]` | `--all` *(bool)*; `--watch` *(bool)*; `--verbose` *(bool)* |

#### `doctor` — *Check adapters and dependencies*
No args, no options. Runs **without** an initialized project.

#### `tui` — *Launch TUI dashboard*
No args, no options.

#### `serve` — *Headless daemon mode with structured logs*
| Options |
|---|
| `--once` *(bool)*; `--tick-interval <ms>`; `--log-file <path>`; `--log-format <format>` **default `'json'`** (`json\|text`); `--verbose` *(bool)* |

#### `update` — *Check for updates*
| Options |
|---|
| `--check` *(bool)* — only check, do not install |

### 2.3 Default behaviour with no subcommand (L0)

1. `orch` with no args **in a directory that has no `.orchestry/`** → run `init` (non-interactive), then launch
   the TUI rooted at `cwd`. This is the first-run experience. The `init` must happen **before** project-root
   resolution so an ancestor project is not adopted.
2. `orch` with no args in an initialized project → launch the TUI.
3. `--help`/`-h`/`--version`/`-V` with no real subcommand → print and exit without building a container.

### 2.4 The `$EDITOR` round-trip protocol (L0)

**`task edit <id>` and `agent edit <id>` take no options whatsoever.** They sit beside flag-taking verbs in
§2.2, which invites the assumption that they accept `--title`/`--description`/`--model`. They do not. Each:

1. serializes the entity into a temp file as **`---`-delimited frontmatter**,
2. spawns an editor on it — **`$EDITOR`, then `$VISUAL`, then `vi`**,
3. parses what comes back and writes **only the fields that changed**.

This is an L0 surface that no earlier revision of this document enumerated. Three behaviours are contract:

| Situation | Behaviour |
|---|---|
| Editor exits 0 with an unchanged file | prints `No changes.` |
| Editor exits non-zero | prints `Editor exited with code N` |
| Editor exits 0 with changes | writes only the changed fields |

The exact frontmatter key set per entity is in [docs/parity/cli.md §4, §5](parity/cli.md). Scenarios reach
these commands through `fixtures/fake-editors/`; `cargo xtask lint-scenarios` rejects a scenario that passes
invented flags to them, which is how four originally-written scenarios were found to be asserting nothing.

The same `-e/--edit` flag on `task add` and `agent add` uses the same protocol to author one field.

---

## 3. Exit codes (L0)

Source of truth: `src/domain/errors.ts`.

| Code | Meaning | Error class |
|---|---|---|
| `0` | Success | — |
| `1` | General error | `OrchestryError` base, `NoAgentsError`, `TaskNotFoundError`, `AgentNotFoundError`, `TaskAlreadyRunningError`, `InvalidTransitionError`, `GoalNotFoundError`, `GoalHasPendingTasksError`, `TeamNotFoundError`, `MessageNotFoundError`, any non-`OrchestryError` throw, **and all Commander usage errors** |
| `2` | Invalid arguments | `InvalidArgumentsError` |
| `3` | Not initialized | `NotInitializedError` |
| `4` | Lock conflict | `LockConflictError` |
| `5` | Agent adapter unavailable | `AgentAdapterError` |
| `6` | Workspace error | `WorkspaceError` |

Additional exit paths (all L0):

- **Any command in an uninitialized directory exits `3`** — *including* `orch <sub> --help` and unknown
  commands. The container is built **before** Commander parses, so `NotInitializedError` wins over help and
  over usage errors. Only `init`, `doctor` and `update` are exempt.
- **A command group invoked with no subcommand** (`orch task`, `orch agent`, …) prints its help to
  **stderr** — **0 bytes on stdout** — and exits **`1`**. This is Commander's `help({ error: true })` path,
  which a subcommand without an `.action()` falls into. Measured against the oracle in a hermetic tmpdir:

  | argv | stdout | stderr | exit |
  |---|---|---|---|
  | `orch task` | 0 B | 735 B | 1 |
  | `orch agent` | 0 B | 654 B | 1 |
  | `orch config` | 0 B | 407 B | 1 |

  The first stderr line is `Usage: orchestry <group> [options] [command]`. **The stream is contract**; the
  help body's layout is clap's and is L2 (§1.3). Encoded in
  `fixtures/scenarios/cmd-exit-codes-groups.toml`.

  > An earlier revision of this document said **stdout**. That was wrong, and a parity scenario would have
  > failed on it.
- Commander errors (unknown command, unknown option, missing required option) → **exit 1**, message on stderr
  in Commander's own format (`error: unknown command 'x'`, optionally followed by `(Did you mean y?)`). The
  exact wording is **L2**; the exit code is L0.
- Explicit `process.exit(2)` argument guards: `logs` with none of
  `[run-id]`/`--task`/`--agent`/`--since`; `run` with neither a task id nor `--all`/`--watch`;
  `serve --log-format <invalid>`; `init --adapter <invalid>`.
- `goal status <id> <invalid-status>` → **1**. All `org deploy` failures → **1**.
- `run <task-id>` whose run fails sets exit **1**.
- `serve --once` → `1` **iff any task ended in status `failed`**, else `0`. A `cancelled` task does **not**
  produce a non-zero exit.
- `update` network or install failure → **1**.

**Commands that exit `0` while printing an error** — these are contract and easy to get wrong:

| Command | Behaviour |
|---|---|
| `context get <missing-key>` | prints an error / `null`, **exits 0** |
| `config global set` with an invalid value or unknown key | prints an error, **exits 0** |
| `doctor` | reports failing checks, **always exits 0** — it diagnoses, it does not gate |

### 3.1 Error message format (L0 for the message and hint text, L2 for colour)

```
  ✕ <message>
    <hint>
```

Both lines go to **stderr**. Two spaces before the glyph, one space after; the hint line is indented four
spaces and is omitted when there is no hint. In `--ascii` mode the glyph is `x`.

**Exact message and hint strings** (L0):

| Error | Message | Hint |
|---|---|---|
| `NotInitializedError` | `Not initialized` | `Run: orch init` |
| `LockConflictError` | `Orchestrator already running (PID: <pid>)` | `Use: orch status` |
| `AgentAdapterError` | `Agent adapter "<adapter>" not available` | *(detail passed in)* |
| `NoAgentsError` | `No agents configured` | `Run: orch agent add <name> --adapter <adapter>` |
| `TaskNotFoundError` | `Task not found: <id>` | — |
| `AgentNotFoundError` | `Agent not found: <id>` | — |
| `TaskAlreadyRunningError` | `Task <id> is already running (run: <runId>, agent: <agentName>)` | `Use: orch logs --task <id> --follow` |
| `InvalidTransitionError` | `Invalid transition for <id>: <from> → <to>` | — |
| `GoalNotFoundError` | `Goal not found: <id>` | — |
| `GoalHasPendingTasksError` | `Cannot mark goal <id> as achieved: <n> task(s) still pending — <summary>` | `Use --force to cancel pending tasks and mark achieved` |
| `TeamNotFoundError` | `Team not found: <id>` | — |
| `MessageNotFoundError` | `Message not found: <id>` | — |
| `WorkspaceError` | *(varies)* | *(varies)* |

Note the `→` in `InvalidTransitionError` is U+2192 and is **not** ASCII-folded (it is produced in the domain
layer, which has no knowledge of `--ascii`).

Validation messages (all `InvalidArgumentsError`, exit 2) — exact text is L0:

- `Task title is required` / `Task title cannot be empty`
- `Priority must be an integer between 1 and 4`
- `Unknown depends_on task ID(s): <id>, <id>`
- `Unknown agent ID: "<v>". No agent with this ID exists.`
- `Unknown agent: "<v>". Use an agent ID (agt_xxx) or an exact agent name.`
- `Cannot delete a running task. Cancel it first.`
- `Agent name is required` / `Agent name cannot be empty` / `Agent adapter cannot be empty`
- `Shell agents require a command`
- `Agent "<name>" already exists`
- `Cannot remove a running agent. Stop it first.`
- `Goal title is required` / `Goal title cannot be empty`
- `Cannot transition goal from '<from>' to '<to>'`
- `Team name is required` / `Team "<name>" already exists`
- `Lead agent not found: <id>` / `Member agent not found: <id>` / `Agent not found: <id>`
- `Agent <id> is already a member of team <id>`
- `Lead cannot leave team. Disband the team or transfer lead first.`
- `Agent <id> is not a member of team <id>`
- `Task not found: <id>` *(from `team add-task`)*
- `Message body is required`
- `TTL must be between 1ms and 604800000ms`
- `to_agent_id is required for direct messages` / `team_id is required for lead channel`
- `Recipient agent not found: <id>` / `Sender agent not found: <id>` / `Team not found: <id>`
- `Specify a run ID, --task, --agent, or --since <duration>`
- `TTL must be a positive number up to 2592000000ms (30 days)` *(context store; a plain `Error`, so exit 1)*
- `Agent Shop templates require an AI adapter; create shell agents with --command`
- `Paths not configured`
- `Attachment file not found: <path>`

---

## 4. `--json` output shapes (L0)

`--json` is a **global** flag; each command decides whether to honour it. Where a command does not implement
`--json`, it prints its normal human output — do not add JSON where the original has none.

JSON goes to **stdout**. Most commands pretty-print with **2-space indent and a trailing newline**
(`JSON.stringify(value, null, 2)` then `console.log`, which appends `\n`).

**Four commands emit compact JSON** (`JSON.stringify(value)`, no indent) — L0, do not normalize this away:
`config get`, `config global get`, `config global show`, `goal delete`.

**`doctor --json` is not pure JSON** — it prints **two human header lines before** the JSON document. A
consumer piping it to a parser must skip them. Replicate exactly.

**Key order is not contract** (§1.4). Field *presence* is: a field whose TS value is `undefined` is **omitted**;
a field whose value is `null` is **emitted as `null`**. The only `null`-valued field in the domain is
`Message.to_agent_id` (typed `string | null`) and `PromptContext.attempt`.

### 4.1 Commands that support `--json`

| Command | Shape |
|---|---|
| `task add` | a single `Task` object |
| `task list` | array of `Task` (possibly `[]`) |
| `task show` | a single `Task` object |
| `agent add` | a single `Agent` object |
| `agent list` | array of `Agent` |
| `agent status` | a single `Agent` object |
| `goal add` | a single `Goal` object |
| `goal list` | array of `Goal` |
| `goal show` | `Goal` plus linked tasks — see §4.4 |
| `team create` | a single `Team` object |
| `team list` | array of `Team` |
| `team show` | a single `Team` object |
| `msg send` | array of `Message` (one element for direct/lead) |
| `msg broadcast` | array of `Message` (one per recipient) |
| `msg list` | array of `Message` |
| `msg inbox` | array of `Message` |
| `context get` | a single `ContextEntry` object |
| `context list` | array of `ContextEntry` |
| `logs` | array of `Run` objects (metadata, including the full `prompt`) |
| `status` | `{ tasks, agents, state }` — see §4.3 |
| `config get` | **compact.** the raw value at the dot path, JSON-encoded. **The `value` key is omitted entirely for a missing key**. Dot paths **do** work: `config get defaults.agent.max_turns --json` → `{"key":…,"value":50}` |
| `config global get` | **compact. Cannot read a nested key** — see below |
| `config global show` | **compact.** the `GlobalConfig` object |
| `agent edit` | a single `Agent` object |
| `agent autonomous` | a single `Agent` object |
| `context set` | the written `ContextEntry` |
| `goal status` | the updated `Goal` |
| `goal update` | the updated `Goal` |
| `goal delete` | **compact** |
| `org list` | array of org templates |
| `org deploy` | the deployment result |
| `doctor` | adapter/dependency check results, **after two human header lines** |

Empty collections are `[]`. A **missing context entry** (`context get <unknown> --json`) prints the literal
line `null`.

**`config global get` cannot read a nested key, and that asymmetry is L0.** Measured in one tree:

| argv | output | exit |
|---|---|---|
| `config global show --json` | `{"tui":{"palette":"amber",…}}` | 0 |
| `config global get tui.palette --json` | `{"key":"tui.palette"}` — **no `value` at all** | 0 |
| `config global get tui --json` | likewise empty | 0 |
| `config get defaults.agent.max_turns --json` | `{"key":"defaults.agent.max_turns","value":50}` | 0 |

So the project-level `config get` walks dot paths and the global one does not. **Replicate; do not "fix" it** —
the CLI contract is byte parity, not sanity. Encoded in `fixtures/scenarios/cmd-config.toml`.

### 4.1.1 Commands that do NOT support `--json` (L0 — they print human text even with the flag)

`run`, `serve`, `tui`, `init`, `update`, `agent shop`, `agent remove`, `agent disable`, `agent enable`,
`task edit`, `task assign`, `task cancel`, `task approve`, `task reject`, `task retry`, `config set`,
`config edit`, `team join`, `team leave`, `team add-task`, `team set-lead`, `team disband`.

`context delete` suppresses output entirely.

Do **not** add JSON where the original has none — a consumer passing `--json` globally must get the same bytes.

### 4.2 Entity shapes

These are the same shapes as the on-disk records (§6), with the same omit-on-`undefined` rule. A worked
example of each, captured from the reference binary:

`Task` (minimal, as created):
```json
{
  "id": "tsk_k3GVRqg",
  "title": "Second task",
  "description": "",
  "status": "todo",
  "priority": 3,
  "labels": [],
  "depends_on": [],
  "created_at": "2026-10-13T16:37:33.426Z",
  "updated_at": "2026-10-13T16:37:33.426Z",
  "attempts": 0,
  "max_attempts": 3
}
```
Note: `assignee`, `workspace_mode`, `workspace`, `proof`, `review_criteria`, `review_results`, `scope`,
`feedback`, `goalId`, `attachments` are all absent when unset.

`Agent` (shell):
```json
{
  "id": "agt_veWG_kK",
  "name": "shelly",
  "adapter": "shell",
  "config": {
    "command": "echo hello",
    "approval_policy": "auto",
    "max_turns": 50,
    "timeout_ms": 3600000,
    "stall_timeout_ms": 600000
  },
  "status": "idle",
  "stats": {
    "tasks_completed": 0,
    "tasks_failed": 0,
    "total_runs": 0,
    "total_runtime_ms": 0
  }
}
```
Note `config.model` and `config.effort` are **forced to absent** for `adapter: "shell"`, and
`approval_policy` defaults to `auto` for shell rather than the configured default.

`Run`:
```json
{
  "id": "run_rKQA8ah",
  "task_id": "tsk_i4hkLPH",
  "agent_id": "agt_veWG_kK",
  "attempt": 1,
  "status": "succeeded",
  "started_at": "2026-10-13T16:37:59.145Z",
  "workspace_path": "/abs/path",
  "prompt": "...full rendered user prompt...",
  "pid": 360135,
  "finished_at": "2026-10-13T16:37:59.175Z"
}
```

`Message`:
```json
{
  "channel": "direct",
  "from_agent_id": "cli",
  "subject": "Hi",
  "body": "hello there",
  "created_at": "2026-10-13T16:42:10.904Z",
  "expires_at": "2026-10-14T16:42:10.904Z",
  "status": "pending",
  "id": "msg_zMOeUPn",
  "to_agent_id": "agt_creator"
}
```

`Goal`:
```json
{
  "id": "goal_717sF1B",
  "title": "Ship v2",
  "description": "Get it out",
  "status": "active",
  "created_at": "2026-10-13T16:42:02.793Z",
  "updated_at": "2026-10-13T16:42:02.793Z"
}
```

`Team`:
```json
{
  "id": "team_tmmAHja",
  "name": "squad",
  "status": "active",
  "members": [
    { "agent_id": "agt_creator", "role": "lead", "joined_at": "2026-10-13T16:42:11.108Z" }
  ],
  "task_pool": [],
  "lead_agent_id": "agt_creator",
  "created_at": "2026-10-13T16:42:11.108Z",
  "updated_at": "2026-10-13T16:42:11.108Z",
  "config": { "auto_claim": true, "message_ttl_ms": 86400000 }
}
```

`ContextEntry`:
```json
{
  "key": "mykey",
  "value": "myvalue",
  "created_at": "2026-10-13T16:42:10.630Z",
  "updated_at": "2026-10-13T16:42:10.630Z"
}
```
`ttl_ms` and `expires_at` are omitted when no TTL was given.

### 4.3 `status --json`

```json
{ "tasks": [ ...Task ], "agents": [ ...Agent ], "state": { ...OrchestratorState } }
```

**r-orch emits `state.claimed` as a JSON array of task ids** — the same shape as `state.json` on disk. The TS
original always emits `{}` here (its in-memory `claimed` is a JS `Set`, and `JSON.stringify` of a `Set` is `{}`).
That is a bug, and it is **fixed** in r-orch by user decision (§14.1). The differential harness masks
`state.claimed` in `status --json` comparisons and instead asserts that r-orch's value equals the `claimed` array
in the resulting `state.json`.

`state` otherwise matches the `state.json` schema in **§6.6** exactly (§6.5 is `config.yml`).

### 4.4 `goal show --json`

`Goal` fields plus the linked tasks and progress report. Read
`parity/orch-ts/src/cli/commands/goal.ts:86-135` for the exact assembly before implementing.

---

## 5. Human-readable output contract

Human output is split: **structural text is L0, decoration is L2.**

**L0 (must match exactly, after stripping ANSI):** every literal word, label, column header, count line,
success/error message, and the padding of table cells (see §5.1).

**L2 (may differ):** colours, ANSI codes, **the entire `--help` body** (it is clap's layout, not Commander's —
see §1.3), spinner frames, and the Unicode glyph chosen for a status — but the *set* of glyphs and their ASCII
fallbacks are L0 because `--ascii` is a documented flag.

### 5.1 Table rendering (L0)

`printTable(headers, rows, padding = 2)` in `src/cli/output.ts`:

- Column width = `max(header.length, max(ansi-stripped cell length))`.
- Every cell — **including the last in a row** — is padded to `width + padding`. Rows therefore carry
  **trailing spaces**. Do not trim them.
- Each line is prefixed with exactly two spaces.
- The header line is dimmed (L2) but its text and padding are L0.
- Output is written with `process.stdout.write(lines.join('\n') + '\n')` — one trailing newline, no blank line
  between header and rows.

`printKeyValue(pairs)`: `  ` + key padded to `maxKeyLen + 2` + value, joined by `\n`, one trailing newline.

### 5.2 Glyphs (L0 — both sets)

| Role | Unicode | `--ascii` |
|---|---|---|
| running / in_progress | `●` | `*` |
| todo / cancelled / idle | `○` | `o` |
| review | `◈` | `#` |
| done | `✓` | `+` |
| failed / error | `✕` | `x` |
| retrying | `↻` | `~` |
| disabled | `─` | `-` |
| agent action | `▸` | `>` |
| orchestrator event | `→` | `->` |
| warning | `⚠` | `!!` |

`printSuccess` → `  ✓ <msg>` on stdout. `printWarning` → `  ⚠ <msg>` on stdout.
`printError` → `  ✕ <msg>` on stderr (+ indented hint).

### 5.3 Formatters (L0)

```
formatDuration(ms):
  < 60s        → "<n>s"
  < 60min      → "<m>:<ss>"      (seconds zero-padded to 2)
  otherwise    → "<h>h<mm>m"     (minutes zero-padded to 2)
formatTokens(n):
  n >= 1000    → "<n/1000 to 1 decimal>k"
  otherwise    → "<n>"
priorityLabel(p): "P1".."P4", else "P<p>"
```

### 5.4 Captured reference output

Exact reference output for the common commands (ANSI stripped). Trailing spaces are significant.

`orch init` (stdout):
```

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

`orch task add "First task"`:
```
  ✓ Created tsk_O5WBXtX "First task"
```

`orch task list`:
```

  STATUS  PRI  TASK         AGENT  TIME  
  ○ todo  P3   Second task  —      —     
  ○ todo  P2   First task   —      —     

  2 tasks

```

`orch agent list`:
```

  STATUS  AGENT          ADAPTER  TASK  TIME  
  ○ idle  Agent Creator  claude   —     —     
  ○ idle  shelly         shell    —     —     

  2 agents · 0 running · 0 tokens total

```

`orch status`:
```

orch · proj · idle

  todo        2

```

`orch task show <id>`:
```

  Shell task
  ══════════════════════════════════════════

  Status    ✓ done · attempt 1/3
  Priority  P3
  Agent     agt_veWG_kK
  Created   2026-10-13T16:37:58.959Z

  Result
  ──────────────────────────────────────────

```

`orch run <id>` (shell agent printing `hello`):
```

  orch · running tsk_i4hkLPH "Shell task"

  12:37:59  ▸ hello
  ✓ Done
```
The timestamp is **local time**, `HH:MM:SS`.

`orch logs --task <id>`:
```

  Run run_rKQA8ah · attempt 1 · succeeded
  12:37:59  ▸ hello

```

`orch goal list`:
```
     ID            Title    Status  Assignee  
  ●  goal_717sF1B  Ship v2  active  any       
```
An unassigned goal renders its assignee as the literal `any`.

`orch goal show <id>`:
```
  ID           goal_717sF1B
  Title        Ship v2
  Status       active
  Assignee     any
  Description  Get it out
  Created      2026-10-13T16:42:02.793Z
  Updated      2026-10-13T16:42:02.793Z

  No tasks linked to this goal yet.

```

`orch context list`:
```

  KEY    VALUE    UPDATED  TTL  
  mykey  myvalue  0s       —    

  1 entries

```
Note `1 entries` — not pluralized. Replicate.

`orch context get <key>`:
```

  mykey = myvalue

```

`orch context set <key> <value>` → `  ✓ Set context "mykey"`

`orch msg send <to> <body>` → `  ✓ Message sent: msg_zMOeUPn → agt_creator`

`orch msg inbox <agent>`:
```

  msg_zMOeUPn from cli — Hi
  hello there

```

`orch config set <key> <value>` → `  ✓ defaults.agent.workspace_mode = "shared"`
(the value is JSON-encoded in the echo).

`orch doctor`:
```

  orch doctor · checking adapters and dependencies

  ✓  claude       2.1.296 (Claude Code)
  ✓  codex        codex-cli 0.162.1
  ✓  cursor       2026.03.11-6dfa30c
  ✓  shell        GNU bash, version 5.2.37(1)-release (x86_64-pc-linux-gnu)
  ✓  opencode     1.18.21
  ✕  pi           Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent
  ✕  grok         Grok CLI not found. Install and authenticate the grok CLI, then ensure `grok` is on PATH.
  ✕  antigravity  Antigravity CLI not found. Install Google Antigravity CLI and ensure `agy` is on PATH.
  ✓  git          git version 2.47.3
  ✓  git repo     git repository detected
  ✓  .gitignore   .orchestry is excluded
  ✓  node         v24.21.0

  ✓  .orchestry/   exists · 2 agents · 3 tasks

  5 of 8 adapters ready

```
Adapter probe order is L0: `claude, codex, cursor, shell, opencode, pi, grok, antigravity` — it is the
registration order in `buildFullContainer`. The `node` check is replaced in r-orch; see §13.

---

## 6. On-disk format — `.orchestry/` (L0)

Directory constant: `ORCHESTRY_DIR = ".orchestry"`, at the project root.

### 6.1 File map

| Path | Format | Written by | Notes |
|---|---|---|---|
| `.orchestry/config.yml` | YAML | `ConfigStore` | Project config, deep-merged over defaults on read |
| `.orchestry/state.json` | JSON | `StateStore` | Orchestrator runtime state. gitignored |
| `.orchestry/orchestry.lock` | plain text | `lock.ts` | Decimal PID, no trailing newline |
| `.orchestry/.gitignore` | plain text | `init` | See §6.11 |
| `.orchestry/workspace-exclude` | plain text | `init` | rsync exclude list for `isolated` mode |
| `.orchestry/templates/default.md` | Markdown | `init` | |
| `.orchestry/tasks/<task-id>.yml` | YAML | `TaskStore` | One file per task |
| `.orchestry/tasks/_index.json` | JSON | `IndexManager` | Array of full `Task` objects |
| `.orchestry/agents/<agent-id>.yml` | YAML | `AgentStore` | |
| `.orchestry/agents/_index.json` | JSON | `IndexManager` | Array of full `Agent` objects |
| `.orchestry/goals/<goal-id>.yml` | YAML | `GoalStore` | |
| `.orchestry/goals/_index.json` | JSON | `IndexManager` | Array of full `Goal` objects |
| `.orchestry/teams/<team-id>.yml` | YAML | `TeamStore` | **No index file** — `TeamStore` scans the directory |
| `.orchestry/messages/<msg-id>.json` | JSON | `MessageStore` | |
| `.orchestry/messages/_index.json` | JSON | `IndexManager` | Array of full `Message` objects |
| `.orchestry/context/<key>.json` | JSON | `ContextStore` | |
| `.orchestry/context/_index.json` | JSON | `IndexManager` | Array of full `ContextEntry` objects |
| `.orchestry/runs/<run-id>.json` | JSON | `RunStore` | Run metadata. **No index file** |
| `.orchestry/runs/<run-id>.jsonl` | JSONL | `RunStore` | Append-only event log |
| `.orchestry/attachments/<task-id>/<basename>` | copied bytes | `TaskService` | Task attachments |
| `.orchestry/workspaces/<task-id>/` | directory | `WorkspaceManager` | Worktree or clone. gitignored |
| `.orchestry/logs/` | — | reserved | Path exists in `Paths`; nothing writes it. gitignored |
| `~/.orchestry/global.yml` | YAML | `GlobalConfigStore` | Global, cross-project. Overridable via `ORCHESTRY_GLOBAL_CONFIG_PATH` |
| `~/.orchestry/update-check.json` | JSON | TS `update-check.ts` only | npm update-check SWR cache, 4 h TTL. **r-orch must never read or write it.** See below |
| `~/.orchestry/update-installed.json` | JSON | TS `update-check.ts` only | "restart needed" marker, read by the TS TUI. **r-orch must never read or write it.** |

**The two update files are cross-binary state that r-orch deliberately does not participate in.** Their path
is `path.join(os.homedir(), '.orchestry')`, hard-coded in `src/cli/update-check.ts:15-16` and `:125` with
**no environment override** — `ORCHESTRY_GLOBAL_CONFIG_PATH` redirects `global.yml` alone. Because the TS
binary writes them from almost every command (§11.5) and r-orch writes neither, they are excluded from
differential comparison, and the harness must neutralize them (ARCHITECTURE §8.2): set `HOME`/`USERPROFILE`
per tmpdir **and** pre-seed a fresh `update-check.json` so the oracle's cache is warm and it makes no network
call.

`init` creates **only**: `config.yml`, `agents/agt_creator.yml`, `templates/default.md`, `.gitignore`,
`workspace-exclude`. It does **not** create `tasks/`, `runs/`, `logs/`, or any `_index.json`. Everything else
is created lazily on first write. A first `task list` on a fresh project therefore rebuilds the index from an
empty directory.

### 6.2 ID formats (L0)

`<prefix>_<nanoid(7)>` where the alphabet is the 64-character URL-safe set `A–Z a–z 0–9 _ -`.

| Entity | Prefix | Example |
|---|---|---|
| Task | `tsk_` | `tsk_O5WBXtX` |
| Agent | `agt_` | `agt_veWG_kK` |
| Run | `run_` | `run_rKQA8ah` |
| Goal | `goal_` | `goal_717sF1B` |
| Team | `team_` | `team_tmmAHja` |
| Message | `msg_` | `msg_zMOeUPn` |

The JS `nanoid` default alphabet (`urlAlphabet`) and the Rust `nanoid::alphabet::SAFE` constant contain the
**same 64 characters** (verified), so no custom alphabet is needed — only the length (7) and the prefix.

Exception: `init` writes a **fixed** agent id `agt_creator` (not a nanoid).

**Filename safety** (L0): every id used in a path passes `sanitizeId`, which enforces `^[A-Za-z0-9._-]+$` and
throws `Invalid identifier: "<id>"` otherwise. Context keys go through the same function, so a key containing
`/`, `..`, or any other character outside that class is rejected rather than sanitized. This is the path
traversal defence and must be preserved exactly.

### 6.3 Timestamps (L0)

Every timestamp is an **ISO-8601 instant in UTC with exactly 3 fractional digits and a `Z` suffix** —
`new Date().toISOString()`, e.g. `2026-10-13T16:37:33.426Z`.

### 6.4 Serialization rules (L0 — these are the cross-compat crux)

**JSON writes** (`writeJson`): `JSON.stringify(data, null, 2) + "\n"` — 2-space indent, **trailing newline**.
Rust: `serde_json::to_string_pretty` produces the same indentation but **no trailing newline** — append one.

**JSONL appends** (`appendJsonl`): `JSON.stringify(record) + "\n"` — compact, no spaces. Opened with `O_APPEND`
so writes interleave atomically up to `PIPE_BUF`. See §6.9 for the truncation rule.

**YAML writes** (`writeYaml`): `js-yaml.dump(data, { indent: 2, lineWidth: 120, noRefs: true, sortKeys: false })`.

**Optional fields:** a TS `undefined` value is **omitted from the mapping entirely** by both `JSON.stringify`
and `js-yaml.dump` (verified). In Rust, every `Option` field that the TS type marks `?` MUST carry
`#[serde(skip_serializing_if = "Option::is_none")]`. The single exception is `Message.to_agent_id`
(`string | null`), which must serialize as `null` when absent.

> **This rule governs *persisted records and `--json` output only* — it does not extend to `PromptContext`.**
> The template context requires the **opposite** discipline: every optional variable must be **present** with
> an explicit `nil`, because the Rust `liquid` crate errors on a missing key where liquidjs renders empty
> (§9.5, ARCHITECTURE §4.3). The two rules do not conflict — they apply to different types — but the wording
> above reads as universal, so it is scoped here explicitly. **WP-3: do not "fix" `PromptContext` to use
> `skip_serializing_if`.**

**⚠ Timestamp quoting — blocking cross-compat hazard.** js-yaml's default schema includes the YAML 1.1
`timestamp` type, so an **unquoted** `created_at: 2026-10-13T16:37:58.959Z` is parsed by the TypeScript binary
into a JavaScript **`Date` object**, not a string. Downstream, `MessageStore.list()` calls
`a.created_at.localeCompare(...)` — which **throws** on a `Date`. js-yaml emits these values single-quoted;
both `serde_yaml_ng` and `serde_norway` emit them **unquoted** (verified). The Rust YAML writer MUST therefore
force quoting on any scalar that would re-parse as a YAML 1.1 timestamp or date. See
[ARCHITECTURE.md §5.3](ARCHITECTURE.md) for the implementation and its mandatory round-trip test.

Scalars that js-yaml quotes and a YAML-1.2 emitter may not: anything matching the 1.1 timestamp/date grammar.
Scalars safe either way (verified): `yes`, `no`, `on`, `off` are plain strings under js-yaml's core schema;
`null`, `true`, `false`, and numeric-looking strings are quoted by both.

#### 6.4.1 Multiline strings — measured, both directions

Agent `role` and `system_prompt` values are multi-line and are injected verbatim into prompts (§9.1, L0), so a
lossy block scalar would be a silent prompt corruption. **Measured across all ten edge cases, in both
directions — every one round-trips.**

| Case | js-yaml emits | serde_yaml_ng emits | Rust reads js-yaml's bytes |
|---|---|---|---|
| plain multiline | `\|-` | `\|-` | OK |
| line with **trailing space** | `\|-` (preserved) | **double-quoted** | OK |
| line of **only spaces** | `\|-` (preserved) | **double-quoted** | OK |
| trailing newline | `\|` | `\|` | OK |
| two trailing newlines | `\|+` | `\|+` | OK |
| leading space on a line | `\|-` | `\|-` | OK |
| tab inside / CRLF | double-quoted | double-quoted | OK |
| only a newline / empty string | `\|+` / `''` | `\|2+` / `''` | OK |

Both emitters detect when a literal block scalar would be lossy and fall back to a quoted style — they just
choose **different** styles. Different bytes, identical parsed value. This is why §1.4 excludes YAML bytes from
the contract and compares parsed values instead.

#### 6.4.2 Schema constructs — merge keys are a real bifurcation

Nothing ORCH *writes* produces these; they are reachable only in a **hand-authored `config.yml`**, which §6.5
reads as a free-form value tree. Measured:

| Construct | js-yaml (DEFAULT_SCHEMA) | serde_yaml_ng (YAML 1.2 core) | Decision |
|---|---|---|---|
| **merge keys `<<`** | **resolved by default** | **left as a literal `"<<"` key** unless `apply_merge()` is called | **r-orch MUST call `apply_merge()` on every YAML read** |
| duplicate keys | throws | rejects | agree — reject, no action |
| anchors / aliases (`&`/`*`) | resolved | resolved | agree |
| `!!binary aGk=` | byte map `{"0":104,"1":105}` | string `"aGk="` | documented delta |
| `!!timestamp 2026-01-01` | `"2026-01-01T00:00:00.000Z"` (normalized) | `"2026-01-01"` (verbatim) | documented delta |
| complex key `? [a,b]` | `{"a,b": 1}` | parses, different representation | documented delta |

**Merge keys are the load-bearing one.** Verified:

```yaml
base: &b
  max_turns: 99
  adapter: codex
scheduling:
  <<: *b
  poll_interval_ms: 5
```
js-yaml → `scheduling = {max_turns: 99, adapter: codex, poll_interval_ms: 5}`
serde_yaml_ng → `scheduling = {"<<": {max_turns: 99, adapter: codex}, poll_interval_ms: 5}`

Without `apply_merge()`, r-orch would see a literal `<<` key, fall back to `DEFAULT_CONFIG` for everything the
merge supplied, and **run the same project with a different `max_concurrent_agents` and different adapter
defaults than the TS binary** — with both trees comparing equal under the Rust parser. Calling `apply_merge()`
reproduces js-yaml's result exactly (key order differs; §1.4 excludes key order).

The three documented deltas are not individually specified further because the generalized cross-parse check
(§1.4, V32) catches them automatically — which is the argument for that check over enumerating constructs.

### 6.5 `config.yml`

Read with a recursive deep-merge over `DEFAULT_CONFIG` (objects merge; arrays and scalars replace). A file
missing entirely yields the defaults. `config get`/`set` use `.`-separated dot paths; `set` creates missing
intermediate objects.

As written by `init`:
```yaml
project:
  name: proj
defaults:
  agent:
    adapter: claude
    approval_policy: auto
    max_turns: 50
    timeout_ms: 3600000
    stall_timeout_ms: 600000
    workspace_mode: worktree
  task:
    max_attempts: 3
    priority: 3
scheduling:
  poll_interval_ms: 10000
  max_concurrent_agents: 6
  retry_base_delay_ms: 10000
  retry_max_delay_ms: 300000
```

Full schema, with defaults (L0):

| Path | Type | Default |
|---|---|---|
| `project.name` | string | `my-project` (`init` substitutes the directory name) |
| `project.description` | string? | — |
| `defaults.agent.adapter` | string | `claude` |
| `defaults.agent.approval_policy` | `suggest\|auto\|manual` | `auto` |
| `defaults.agent.max_turns` | int | `50` |
| `defaults.agent.timeout_ms` | int | `3600000` |
| `defaults.agent.stall_timeout_ms` | int | `600000` |
| `defaults.agent.workspace_mode` | `shared\|worktree\|isolated` | `worktree` |
| `defaults.task.max_attempts` | int | `3` |
| `defaults.task.priority` | int | `3` |
| `scheduling.poll_interval_ms` | int | `10000` |
| `scheduling.max_concurrent_agents` | int | `6` |
| `scheduling.retry_base_delay_ms` | int | `10000` |
| `scheduling.retry_max_delay_ms` | int | `300000` |
| `prompt.template` | string? | — (legacy single combined template) |
| `prompt.system_template` | string? | — |
| `prompt.user_template` | string? | — |

### 6.6 `state.json`

```json
{
  "version": 1,
  "onboardingCompleted": false,
  "running": {},
  "claimed": [],
  "retry_queue": [],
  "stats": {
    "total_runs": 1,
    "total_tasks_completed": 1,
    "total_tasks_failed": 0,
    "total_tokens": {
      "input": 0, "output": 0, "reasoning": 0,
      "total": 0, "cache_read": 0, "cache_write": 0
    },
    "total_runtime_ms": 15
  }
}
```

| Field | Type | Notes |
|---|---|---|
| `version` | `1` | Literal |
| `pid` | int? | PID of the watch-mode orchestrator; cleared on stop |
| `started_at` | ts? | Cleared on stop |
| `onboardingCompleted` | bool | Coerced to `false` unless strictly a boolean on read |
| `running` | map taskId → `RunningEntry` | `{}` when empty |
| `claimed` | **array of task ids** on disk | In memory a `Set`. Non-array on read ⇒ empty set |
| `retry_queue` | array of `RetryEntry` | Non-array on read ⇒ `[]` |
| `stats.total_runs` | int | |
| `stats.total_tasks_completed` | int | |
| `stats.total_tasks_failed` | int | |
| `stats.total_tokens` | `TokenUsage` | Merged field-wise over the zero default on read |
| `stats.total_runtime_ms` | int | |

`RunningEntry`: `{ run_id, agent_id, task_id, pid, started_at, last_event_at }` — all required.
`RetryEntry`: `{ task_id, attempt, due_at, error }` — all required.

Read is **tolerant**: missing or wrong-typed fields fall back to defaults field by field rather than failing.
Rust must mirror this (`#[serde(default)]` plus explicit coercion for `onboardingCompleted`, `running`,
`claimed`, `retry_queue`).

### 6.7 `orchestry.lock`

Content: the holder's PID as a decimal string, written with `O_CREAT|O_EXCL` (`fs.open(path, 'wx')`), **no
trailing newline**. Acquisition semantics in §7.13.

**Parsing (L0).** `parseInt(content.trim(), 10)`, then `isNaN ? null : pid`. A read error is also `null`. So
an **unparseable or empty lock file is indistinguishable from a missing PID** — and that is *not* treated as
"no lock":

| Lock file contents | Outcome | Lock file afterwards |
|---|---|---|
| absent | **acquired** | created with our PID |
| decimal PID, process **alive**, mtime fresh | **refused**, `pid` reported | untouched |
| decimal PID, process **alive**, mtime older than `LOCK_STALE_MS` (60 s) | **acquired** (PID-recycling defence) | replaced |
| decimal PID, process **dead** | **acquired** | replaced |
| **garbage** (e.g. `xyz`) | **REFUSED** | **untouched** |
| **empty** | **REFUSED** | **untouched** |

Measured against the oracle with `serve --once` and `run <id>` (fake agent). The garbage and empty cases
**refuse with exit 4** and the message **`Orchestrator already running (PID: undefined)`** — the literal
string `undefined`. See §14.17; this is **L0 and replicated verbatim**.

Why: `readLockPid` returns `null` for garbage, which makes the `existing !== null` guard in `doAcquire` false,
so the entire stale-handling branch — including the `unlink` — is **skipped**. The subsequent
`fs.open(path, 'wx')` then fails `EEXIST` because the file does exist, and the error path re-reads the PID,
gets `null` again, and returns `{ acquired: false, pid: undefined }`.

### 6.8 Task / Agent / Goal / Team YAML

Field order as emitted (= TS interface declaration order; L2 but recommended):

**Task** — `id, title, description, status, priority, assignee?, labels, depends_on, created_at, updated_at,
attempts, max_attempts, workspace_mode?, workspace?, proof?, review_criteria?, review_results?, scope?,
feedback?, goalId?, attachments?`

> `goalId` is **camelCase** while every other field is snake_case. This is an inconsistency in the original
> and is **preserved** — it is the on-disk key name.

`proof`: `{ branch?, pr_url?, files_changed: string[], test_results?, agent_summary? }` — `files_changed` is
required, so a task that succeeded with no file changes still writes `proof: { files_changed: [] }`.

`review_criteria`: array of `test_pass | typecheck | lint`.
`review_results`: array of `{ criterion, passed, output }`.

**Agent** — `id, name, adapter, role?, config, status, current_task?, autonomous?, stats, last_error?`

`config`: `{ command?, model?, effort?, approval_policy?, max_turns?, timeout_ms?, stall_timeout_ms?, env?,
system_prompt?, workspace_mode?, skills? }`
`stats`: `{ tasks_completed, tasks_failed, total_runs, total_runtime_ms, tokens_used? }`
`last_error`: `{ message, kind, timestamp }` — `message` truncated to 500 chars; `kind` is an
`AdapterErrorKind` string value.
`status`: `idle | running | error | disabled`.

**Goal** — `id, title, description, status, assignee?, created_at, updated_at?`
`status`: `active | paused | achieved | abandoned`.

**Team** — `id, name, description?, status, members, task_pool, lead_agent_id, created_at, updated_at, config`
`members`: array of `{ agent_id, role: "lead"|"member", joined_at }`
`config`: `{ max_concurrent_tasks?, auto_claim: bool, message_ttl_ms? }` — defaults
`{ auto_claim: true, message_ttl_ms: 86400000 }`
`status`: `active | paused | disbanded`.

### 6.9 Runs

**`<run-id>.json`** — `Run`: `id, task_id, agent_id, attempt, status, started_at, finished_at?,
workspace_path, prompt, pid?, error?, tokens?`
`status`: `preparing | running | succeeded | failed | timed_out | cancelled`.
`tokens`: `TokenUsage` = `{ input, output, reasoning, total, cache_read, cache_write }` where
**`total == input + output + reasoning`** — cache counters are informational and are *not* added to the total.

**`<run-id>.jsonl`** — one `RunEvent` per line, exactly:
```json
{"timestamp":"2026-10-13T16:37:59.169Z","type":"agent_output","data":"hello"}
```
Keys in that order (L2 for order, L0 for names). `type` is one of `agent_output | file_changed | command_run |
tool_call | error | done`. **`data` is always a string** — the orchestrator pre-serializes it (JSON-encoding
non-strings) and truncates to 8192 characters, appending `…` (U+2026) when truncated.

**Atomic-append truncation rule** (L1): if the serialized line exceeds `PIPE_BUF` (4096 bytes) and the record
is an object with a non-empty string `data`, `data` is re-truncated so the whole line fits:
`budget = 4096 - byteLen(JSON.stringify({...record, data: ""}) + "\n") - 3`, then
`data = data.slice(0, budget) + "…"`. If `budget <= 0`, the line is written over-length unchanged.

**`streamEvents(runId, signal?)` reads the file to EOF and ends — it does not tail-follow** (L1). It waits up
to **30 s** for the file to appear (polling every 100 ms), then opens a plain read stream, yields each parsed
line, skips corrupt lines with the same stderr warning, and stops at EOF. A caller wanting to follow must
**re-invoke** it.

> Two things about it are worth knowing before implementing the trait method:
> - **It has no production caller.** Only tests reference it (`run-store.test.ts`, `error-paths.test.ts`,
>   plus the fakes in `helpers.ts`). The trait method and its tests still port, because the interface declares
>   it and 5 test cases pin it — but nothing in the CLI or TUI depends on its behaviour.
> - **`logs --follow` does not use it.** It calls `followLive()`, which subscribes to the **EventBus**
>   (`eventBus.onAny`) and blocks until SIGINT/SIGTERM. Since `logs` builds a *Light* container in a one-shot
>   process, and the orchestrator that emits events runs in a **different** process, that bus is inert: the
>   command prints its `Following live events...` header and then emits nothing until interrupted. Replicate
>   this — it is the observable behaviour — and see §15.1, which lists `logs --follow` as outside the harness.

**Read behaviour** (L1): corrupt lines are skipped with a warning on stderr
(`[readJsonl] skipping corrupt line: <first 200 chars>`). Files over 50 MB read only the last 200 records and
warn (`[readJsonl] file too large (<n> MB), reading tail only: <path>`). `readJsonlTail(path, n)` reads whole
files under 32 KB, otherwise reads backwards in chunks (65536, or 131072 when over 1 MB) for up to 4 chunks,
discarding the first (possibly partial) line when it did not read from offset 0.

### 6.10 `_index.json`

A JSON **array of the complete entity objects** — not a summary. Maintained by `IndexManager`:

- `readIndex()` → parse `_index.json`; if missing, not an array, or corrupt → **rebuild** from the individual
  files and write it back.
- `rebuildIndex()` → list the directory for the store's extension, apply `fileFilter`, strip the extension to
  recover the id, read each file in parallel, drop nulls, write the index.
- `updateIndex(fn)` → read-modify-write under a mutex that serializes all index mutations within the process.
- `save(entity)` appends/replaces by id — **the entity moves to the end of the array**, so index order is
  last-write order, not creation order. All consumers sort explicitly, so index order is **not contract**.
- `.json`-backed stores (context, messages) set `fileFilter` to exclude `_index.json` from the rebuild scan.
  `.yml`-backed stores do not need it.

`IndexManager` is used by task, agent, goal, context and message stores. **`TeamStore` and `RunStore` have no
index** and scan their directories.

#### 6.10.1 Index order leaks into tied listings — position is not contract

Stated once here; the §14 entries that hit it cross-reference this section rather than repeating it.

**The mechanism, in three steps:**

1. **Concurrent saves land in nondeterministic order.** Several code paths write a batch of entities with
   `Promise.all` (`join_all` in Rust). Completion order within the batch is not specified, and `save()`
   pushes each entity to the **end** of the index, so the resulting `_index.json` order varies run to run.
2. **`updated_at` has only millisecond precision** (`new Date().toISOString()`, §6.3). A batch written in the
   same millisecond produces entities whose timestamps are **exactly equal**.
3. **The listing comparators return `0` on an exact tie**, and both JS `Array.prototype.sort` (ES2019+) and
   Rust `slice::sort_by` are **stable** — so a tie preserves input order, which is index order, which is
   nondeterministic by step 1.

```
task-store.ts:42  /  goal-store.ts:39   return bTime < aTime ? -1 : bTime > aTime ? 1 : 0;   // 0 on tie
message-store.ts:46                     a.created_at.localeCompare(b.created_at)              // 0 on tie
```

**Therefore: the position of an entity within any listing whose sort key ties is NOT contract.** This applies
to `task list`, `goal list`, `msg list`, their `--json` forms, and the `_index.json` files themselves.

**Known concurrent-save sites** (all verified in source):

| Site | Store written |
|---|---|
| `message-service.ts` broadcast fan-out | messages |
| `goal-service.ts:99` — `goal status --force` cancelling pending tasks | tasks |
| `goal-service.ts:211` — `cancelPendingAutonomousTasks` on goal pause | tasks |
| `orchestrator.ts:940` — `cascadeFailDependents` per BFS level | tasks |
| `orchestrator.ts:620` — `reconcile` resetting stale agents to `idle` | **agents** |

**How scenarios declare it.** Per step, not globally:

- **`@unordered:<reason>`** — arrays are sorted at **every depth** before comparison. The `<reason>` is
  required, so a reader can tell a real nondeterminism from a masked bug.
- **`unordered_files`** — the same, for a set of files whose relative order is not meaningful.

The operator-facing half — how to recover when a concurrent write has *dropped* an index entry rather than
reordered it — is in [docs/RUNBOOK.md](RUNBOOK.md#rebuilding-an-index-after-a-lost-update).

Everything else on the step is still compared in full. Declaring it per step keeps the blast radius visible:
a step without the marker is order-sensitive, and that is the default.

> **Interaction with §14.4.** That entry replicates TS tie-breaking in `findBestAgent`, on the stated
> precondition that index order is "deterministic for a given operation sequence". The agent row above is the
> exception: `reconcile`'s stale-agent batch writes the **agent** index concurrently, so an operation sequence
> that includes a stale-agent reset can itself produce a nondeterministic agent order — and with it a
> nondeterministic tie. Scenarios asserting agent selection must avoid both a multi-entity index rebuild
> (§14.4) **and** a concurrent stale-agent reset.

### 6.11 `init` file contents (L0)

`.orchestry/.gitignore`:
```
# Runtime state
state.json
*.lock

# Logs and runs
runs/
logs/

# Agent workspaces
workspaces/
```

`.orchestry/workspace-exclude`:
```
.orchestry
node_modules
.env
.env.*
dist
build
.next
__pycache__
*.pyc
.venv
```

`.orchestry/agents/agt_creator.yml` — the "Agent Creator" bootstrap agent. Its `role` is a ~3 KB Markdown
document that must be reproduced **byte for byte** (it is injected into prompts). Source:
`parity/orch-ts/src/domain/default-agents.ts`. Its config is
`{ model: claude-sonnet-4-6, approval_policy: suggest, max_turns: 50, timeout_ms: 3600000,
stall_timeout_ms: 300000, skills: [document-skills:skill-creator] }` — note `stall_timeout_ms` is 300000 here,
**not** the 600000 project default.

`.orchestry/templates/default.md` — see `src/cli/commands/init.ts`.

**Idempotency** (L1): re-running `init` in an initialized project must not destroy existing tasks, agents or
config. Verify against the reference binary before implementing; the behaviour is asserted in
`test/unit/cli/commands-init.test.ts`.

### 6.12 `~/.orchestry/global.yml`

```yaml
tui:
  palette: amber
  activity_filter: all
  notifications:
    toast: true
    bell: false
```

| Field | Type | Default | Validation on read |
|---|---|---|---|
| `tui.palette` | `amber\|ocean\|forest\|violet\|light` | `amber` | Invalid ⇒ fall back to `amber` |
| `tui.activity_filter` | `all\|text\|tools\|errors\|events` | `all` | **Not validated** — any value passes through |
| `tui.notifications.toast` | bool | `true` | Non-boolean ⇒ default |
| `tui.notifications.bell` | bool | `false` | Non-boolean ⇒ default |

The parent directory is created on write. Path override: `ORCHESTRY_GLOBAL_CONFIG_PATH` (trimmed, resolved).

---

## 7. Domain semantics

### 7.1 Task state machine (L0 — the transition table is contract)

Source: `src/domain/transitions.ts`.

```
            ┌──────────────────────────────┐
            ▼                              │
  ┌──────┐                                 │
  │ todo │──────────► in_progress ─────────┼──► review ───► done
  └──────┘                 │               │      │
      ▲ │                  ├──► retrying ──┘      ├──► todo        (rejected)
      │ │                  │       │              └──► cancelled
      │ │                  ├───────┴──► failed ──► todo | retrying
      │ └──────────────────┴──────────► cancelled ──► todo
      └───────────────────────────────────────────────┘
```

| From | Allowed `to` |
|---|---|
| `todo` | `in_progress`, `cancelled` |
| `in_progress` | `review`, `retrying`, `failed`, `cancelled` |
| `retrying` | `in_progress`, `failed`, `cancelled` |
| `review` | `done`, `todo`, `cancelled` |
| `done` | *(none)* |
| `failed` | `todo`, `retrying` |
| `cancelled` | `todo` |

Any other transition requested through `TaskService.updateStatus` raises
`InvalidTransitionError` (exit 1). **The orchestrator deliberately bypasses this check in recovery paths** by
writing the task through the store directly — see §7.4.

Derived predicates (L0):

- `isTerminal(s)` → `s ∈ {done, failed, cancelled}`
- `isDispatchable(s)` → `s ∈ {todo, retrying}`
- `isBlocked(task, all)` → `task.depends_on` is non-empty and **any** dependency has `status != "done"`.
  A dependency id that is **missing from the store is treated as resolved** (deleted = done), never as blocking.
- `resolveFailureStatus(task)` → `retrying` if `task.attempts < task.max_attempts`, else `failed`
- `resolveCompletionStatus(task, success, _autoApprove)` → `review` on success (always — auto-approve is applied
  afterwards by the orchestrator as a separate `review → done` transition), else `resolveFailureStatus(task)`
- `calculateRetryDelay(attempt, base, max)` → `min(base * 2^attempt, max)`

> Note the exponent is `attempt`, **not** `attempt - 1`. Callers pass `task.attempts - 1`, so the first retry
> delay is `base * 2^0 = base`. With the defaults (`base = 10000`, `max = 300000`) the sequence is
> 10 s, 20 s, 40 s, 80 s, 160 s, 300 s, 300 s, …

### 7.2 Goal state machine (L0)

| From | Allowed `to` |
|---|---|
| `active` | `paused`, `achieved`, `abandoned` |
| `paused` | `active`, `achieved`, `abandoned` |
| `achieved` | *(none)* |
| `abandoned` | *(none)* |

Invalid transition ⇒ `InvalidArgumentsError` (exit **2**), message
`Cannot transition goal from '<from>' to '<to>'`. Terminal set: `{achieved, abandoned}`.
Canonical sort order: `active(0) < paused(1) < achieved(2) < abandoned(3)`.

**Achieved guard** (L1): transitioning to `achieved` fails with `GoalHasPendingTasksError` if any linked task is
non-terminal, **excluding** tasks labelled `autonomous`. With `--force`, tasks that are not `in_progress` are
cancelled first; if any `in_progress` task remains, the error is still raised, with summary
`<id> (in_progress), …`. Without `--force` the summary is `<id> (<status>), …`.

**Autonomous side effects** (L1):
- Creating a goal with an `assignee` → `agent.autonomous = true`.
- `active → paused` → disable autonomous *if the agent has no other active goal*, **and** cancel that agent's
  dispatchable (`todo`/`retrying`) tasks labelled `autonomous`.
- `paused → active` → re-enable autonomous.
- → terminal → disable autonomous if no other active goal remains.
- `goal update` changing the assignee → enable on the new agent, maybe-disable on the old (in parallel).
- `goal delete` → maybe-disable on the former assignee.
- All of these swallow "agent not found" silently.

### 7.3 Tick loop (L1 — phase order is contract)

`startWatch()`:
1. `acquireLock(lockPath)` — on failure throw `LockConflictError(pid)` (exit 4).
2. `loadState()`.
3. `cleanupStaleRunningEntries()` (§7.6).
4. Set `state.pid = <own pid>`, `state.started_at = now`, `saveState()`.
5. Register `SIGINT`/`SIGTERM` handlers.
6. Subscribe to `task:created` for reactive dispatch.
7. Run one `tick()` immediately.
8. `setInterval(tick, config.scheduling.poll_interval_ms)`.

Each `tick()` runs **under the state mutex**, and in this order:
1. Invalidate the tick-scoped task/agent/goal caches.
2. `loadState()`.
3. `reconcile()`.
4. `seedAutonomousTasks()` — unless `skipAutonomousSeeding`.
5. `dispatchAll()`.
6. Emit `orchestrator:tick { running, queued }` where `queued` counts tasks with a dispatchable status.
7. *(outside the mutex)* `touchLock(lockPath)` — update mtime to prove liveness.

> "Collect" is **not** a synchronous phase. Event collection runs as a detached background task per run
> (`collectEvents`), and completion is handled by `handleRunSuccess`/`handleRunFailure`, each of which takes
> the state mutex. The CLAUDE.md phrase "Reconcile → Dispatch → Collect" describes the conceptual model, not
> the call order.

**Tick failure circuit breaker** (L1): consecutive tick rejections increment a counter; each emits
`orchestrator:error { context: "tick", fatal: <count >= 5> }`. On the 5th, emit
`orchestrator:shutdown { reason: "<count> consecutive tick failures" }` and call `stop()`. Any successful tick
resets the counter to 0.

**Reactive dispatch** (L1): `task:created` schedules an immediate dispatch with a **500 ms debounce**. If a tick
is in progress when the timer fires, it reschedules, up to **10 retries** (≈5 s) then gives up. The immediate
dispatch is a mini-tick: invalidate caches → `loadState` → `dispatchAll` → `saveState` — **no reconcile**. It is
skipped entirely while any `runTask` single-task run is in flight. `handleRunSuccess`/`handleRunFailure` also
schedule one, unless the finishing task was a single-task run.

### 7.4 Reconcile (L1)

For each entry in `state.running`, in insertion order:

1. **Stale terminal task** — if the task is missing or `isTerminal`, drop the running entry and set the agent
   `idle`. Continue.
2. **Active collector guard** — if `activeCollectors` contains the task id, **skip both the PID and stall
   checks**. This prevents a race where the child exited cleanly but the success handler has not yet taken the
   mutex, which would otherwise be misread as a crash.
3. **PID liveness** — `!isAlive(pid)` ⇒ `_handleRunFailure(taskId, entry, "Process crashed unexpectedly")`.
   If that itself throws, force-drop the running entry and set the agent idle.
4. **Stall** — `now - entry.last_event_at > (agent.config.stall_timeout_ms ?? config.defaults.agent.stall_timeout_ms)`
   (strictly greater) ⇒ emit `orchestrator:stall_detected { runId }`, abort the controller,
   `killWithGrace(pid, 5000)`, then `_handleRunFailure(taskId, entry, "Agent stalled (no events)")`.

   > **The stall clock is per emitted `AgentEvent`, not per stdout line.** `collectEvents` assigns
   > `state.running[taskId].last_event_at = eventTimestamp` **inside** the `for await (const event of
   > generator)` loop, so only events the adapter's parser actually yields refresh it. Consequences, which a
   > loosely-worded spec would leave open to two incompatible readings:
   >
   > - A line the parser maps to `null` (e.g. a blank line — `readLines` already skips empties, and parsers
   >   return `null` for whitespace-only input) does **not** refresh the timer.
   > - An **unparseable non-blank** line **does** refresh it for `claude`, because its parser's `catch` falls
   >   back to emitting `{ type: 'output', data: <raw line> }`. Adapters differ here; follow each adapter's
   >   parser in [docs/parity/adapters.md §2](parity/adapters.md).
   > - The write is **debounced** by `saveStateLazy()` (500 ms), so the *persisted* `last_event_at` can lag
   >   the in-memory value. Reconcile reads the in-memory value.
   >
   > Getting this wrong produces a divergence that only a chatty agent emitting unparseable output reveals —
   > one binary reaps the run, the other does not. V34 exercises it against the oracle.

Then, project-wide:

5. **Stale agents** — agents with `status == "running"` and no running entry → set `idle` (in parallel).
6. **Orphaned tasks** — tasks with `status == "in_progress"` and no running entry → `failed` (force-written
   through the store if the transition is rejected), each emitting `task:orphaned { taskId }`.
7. **Retry queue** — entries whose `due_at <= now` are removed from the queue and dispatched, but only after
   re-reading the task and confirming it is still dispatchable.
8. `saveState()`.

### 7.5 Dispatch (L1)

`dispatchAll()`:
1. `availableSlots = config.scheduling.max_concurrent_agents - |state.running|`. Return if `<= 0`.
2. Candidates = tasks that are dispatchable **and** not blocked by dependencies **and** not in `state.running`
   **and** not in `state.claimed`.
3. **Sort** (L0 — this is the scheduling contract):
   1. `priority` ascending (lower number = more urgent; P1 before P4; missing ⇒ 3)
   2. goal-linked first (`goalId` present beats absent)
   3. `updated_at` **descending** (most recently updated first)
4. Truncate to `availableSlots`.
5. **Scope overlap** — build a `ScopeIndex` from the scopes of all `in_progress` tasks. For each candidate with
   a non-empty `scope`: if it overlaps the index, emit
   `task:scope_overlap { taskId, overlappingTaskId, patterns }` and skip it; otherwise add its scope to the
   index so later candidates in the same pass are checked against it too.
6. Dispatch each surviving candidate. A `WorkspaceError` during dispatch increments `attempts` and applies
   `resolveFailureStatus`: `retrying` enqueues a retry with the backoff delay; `failed` cascade-fails
   dependents. Any dispatch error emits `orchestrator:error { context: "dispatch task <id>" }` and **does not**
   stop the remaining dispatches.

`dispatchTask(taskId)`:
1. Already in `state.running` ⇒ `TaskAlreadyRunningError`.
2. Re-read the task; return silently if no longer dispatchable.
3. Add to `state.claimed` and **persist state before spawning** (crash safety).
4. `findBestAgent(task)` (§7.7). No agent and zero agents configured ⇒ `NoAgentsError`. No *idle* agent ⇒
   unclaim, persist, return.
5. `workspaceManager.prepare(task, agent, config)` → `{ path, branch? }`.
6. Build the prompt (§9).
7. `attempt = task.attempts + 1`. If `attempt > 1`, fetch the last failed run's error and last 50
   `agent_output`/`error` events as retry context.
8. In parallel: read all shared context, **drain the agent's mailbox** (marks messages delivered), load the goal.
9. `runService.create(...)` → a `Run` in status `preparing`.
10. If the task was `failed`/`cancelled`, call `taskService.retry` first (resets to `todo`, `attempts = 0`).
11. `updateStatus(in_progress)` → `assign(agent.id)` → `incrementAttempts()`.
12. If a worktree branch was created, re-read the task and persist `proof.branch` and `workspace` **immediately**
    so the branch survives a later failure.
13. Agent → `running`, set `current_task`, **clear `last_error`**.
14. `adapterRegistry.require(agent.adapter)` — unknown adapter throws
    `Unknown adapter: "<kind>". Available: <comma-joined kinds>` (a plain `Error`, exit 1).
15. `adapter.execute({ prompt, systemPrompt, workspace, env, config, signal })` where `env` is
    `{ ...agent.config.env, ORCH_AGENT_ID, ORCH_AGENT_NAME, ORCH_TASK_ID }`.
16. `runService.start(run.id, pid)` → run status `running`, emits `agent:started`.
17. Move the task from `claimed` to `running` with `started_at = last_event_at = now`; persist.
18. Register the task in `activeCollectors` and start `collectEvents` detached.

On any throw before step 18: delete the abort controller, unclaim, persist, rethrow.

`dispatchOnlyTask(taskId)` (used by `orch run <id>`): temporarily adds **every other** dispatchable task to
`state.claimed` so the shared dispatch path cannot opportunistically start them, dispatches the one task, then
restores the original claimed set.

### 7.6 Startup recovery (L1)

`cleanupStaleRunningEntries()`:
1. Running entries whose PID is dead → drop the entry, set the agent `idle`, **force the task to `cancelled`**
   (not `failed` — so a restart does not make agents redo already-committed work), and finish the run as
   `cancelled` with error `Orchestrator restarted`.
2. **Always** clear `state.claimed` — any surviving claim is by definition stale.
3. Only if dead PIDs were found: cancel tasks still `in_progress` with no running entry, and drop their retry
   queue entries.
4. `cleanupOrphanedPreparingRuns()` — runs still in status `preparing` that are not referenced by
   `state.running` are finished as `cancelled` with error
   `Orphaned preparing run (orchestrator restarted)`.

### 7.7 Agent selection (L1)

`findBestAgent(task)`:
- Candidate pool = agents with `status == "idle"`. Empty pool ⇒ `null`.
- **Explicit assignee is a hard constraint**: match by id **or** name across *all* agents; return it only if it
  is `idle`, else return `null` (the task waits — it is never given to another agent).
- Otherwise score each idle agent:
  - **+50 per** task label that case-insensitively equals one of the agent's `config.skills`
  - **+30** if any task label is a case-insensitive **substring** of `agent.role`
  - **+20** for being idle (always applies to the pool)
  - **+0..10** success rate: `round(tasks_completed / (tasks_completed + tasks_failed) * 10)`, only when the
    denominator is non-zero
- Sort by score **descending** and take the first. Ties resolve to the earlier element of the pool, i.e. the
  `agentStore.list()` order, which is `_index.json` order — **last-write order**. This makes ties
  non-deterministic across implementations; see §14.4.

### 7.8 Completion handling (L1)

`handleRunSuccess` (under the mutex):
1. Flush any debounced state write; drop the abort controller.
2. Return immediately if the task is no longer in `state.running` (already cancelled) or no longer exists.
3. If the adapter reported no changed files **and** the task has `proof.branch`, derive them via
   `getChangedFiles(branch)`.
4. Write `proof.agent_summary` = result text truncated to **2000** chars, `proof.files_changed`; **delete
   `task.feedback`**.
5. `autoApprove = task.labels.includes("autonomous") || agent.config.approval_policy == "auto"`.
6. `newStatus = resolveCompletionStatus(task, true, autoApprove)` → always `review`.
7. `runService.finish(runId, "succeeded", tokens)` → emits `agent:completed { success: true }`.
8. Accumulate `state.stats.total_runtime_ms += now - entry.started_at`; drop the running entry **before** any
   fallible work so a later throw cannot re-enter the failure path.
9. Agent stats: `tasks_completed += 1`, `total_runs += 1`, `total_runtime_ms += runtime`,
   and `tokens_used += tokens.total` when tokens are present.
10. Global stats: `total_tasks_completed += 1`, `total_runs += 1`, and token counters accumulated field-wise
    with `total` recomputed as `input + output + reasoning`.
11. **Merge-back** if `proof.branch` is set (§10.3). On conflict or error, force the task to `review` with the
    summary prefixed `MERGE CONFLICT: <info>` / `MERGE ERROR: <msg>` and **return** — auto-approve is skipped.
12. `updateStatus(review)`; on validation failure, emit `orchestrator:error` and force-write through the store.
13. Agent → `idle`, `current_task` cleared.
14. If the task has `review_criteria`, run auto-review (§7.9). Else if `autoApprove`, transition
    `review → done` immediately.
15. `saveState()`; schedule a reactive dispatch unless this was a single-task run.

`handleRunFailure` (under the mutex):
1. Flush; drop the abort controller; return if the running entry is gone or the task is missing.
2. `runService.finish(runId, "failed", undefined, error)`; agent → `idle`.
3. Clear `current_task`; set `last_error = { message: error.slice(0, 500), kind: errorKind ??
   classifyAdapterError(error), timestamp: now }`.
4. Agent stats: `tasks_failed += 1`, `total_runs += 1`, `total_runtime_ms += runtime`.
5. `failureStatus = resolveFailureStatus(task)`; `updateStatus(failureStatus)`.
6. If `retrying`: enqueue a retry with `calculateRetryDelay(task.attempts - 1, base, max)` and emit
   `run:retry { runId, attempt: task.attempts + 1, delay_ms }`.
   Else: `state.stats.total_tasks_failed += 1` and **cascade-fail dependents** (§7.10).
7. `state.stats.total_runtime_ms += runtime`.
8. Clean up the worktree and branch if `proof.branch` is set.
9. Drop the running entry, `total_runs += 1`, `saveState()`, schedule a reactive dispatch.

**Retry queue** is deduplicated by `task_id` and bounded to **100** entries; on overflow the oldest is shifted
off.

### 7.9 Auto-review (L1)

`ReviewRunner` runs criteria as child processes in `cwd = task.workspace ?? projectRoot`:

| Criterion | Command | argv |
|---|---|---|
| `typecheck` | `npx` | `["tsc", "--noEmit"]` |
| `lint` | `npm` | `["run", "lint"]` |
| `test_pass` | `npm` | `["test"]` |

- **Execution order is fixed**: `typecheck`, `lint`, `test_pass` — fastest first, regardless of the order in
  `review_criteria`. Unknown criteria sort last.
- **Fail-fast by default**: stop at the first failure.
- Per-criterion timeout **120000 ms**, `maxBuffer` 1 MiB.
- `passed = (no error)`; `output = (stdout + "\n" + stderr).trim().slice(0, 2000)`.
- `allPassed(results)` = `results.length > 0 && every(passed)` — an empty result set is **not** "all passed".
- `formatReport(results)`: per result `"<✓|✗> <criterion>: <PASSED|FAILED>\n  <output>"`, joined by `"\n\n"`.

Results are written to `task.review_results` and `task.proof.test_results`, then
`task:auto_reviewed { taskId, passed, results }` is emitted. If all passed **or** `autoApprove` is set, the
task goes to `done` (force-written if the transition is rejected). When `autoApprove` forced a failing review
through, an `orchestrator:error` is emitted with context `auto-review-with-auto-approve`.

> These commands are hard-coded and Node-centric. They are **preserved as-is** for parity — r-orch does not
> substitute `cargo` equivalents (see §14.5).

### 7.10 Cascade failure (L1)

When a task permanently fails, every task that depends on it — directly or transitively — is set to `failed`
via a breadth-first walk over a reverse-dependency index. Already-terminal and already-visited tasks are
skipped. Each cascaded task emits **both** `task:status_changed { from, to: "failed" }` and
`task:cascade_failed { taskId, failedDependencyId, reason }`, where `reason` is
`dependency <failedTaskId> failed: <error>`. Writes are batched in parallel per BFS level, so the cascaded
tasks' order in a tied listing is not contract (§6.10.1).

### 7.11 Scope overlap (L1)

A scope is an array of glob patterns. Two scopes overlap if **any** pattern pair overlaps. A task with no scope
never overlaps anything.

`patternsOverlap(a, b)`:
1. Identical strings ⇒ overlap.
2. Reduce each pattern to its **base** = the prefix before the first `*` (whole pattern if none).
3. If either base is a string prefix of the other ⇒ overlap.
4. Otherwise, if **neither** base ends with `/` (both "file-like"), overlap iff
   `dirname(aBase) == dirname(bBase)` and that dirname is not `.` — sibling files in the same directory
   conflict.

This is deliberately **conservative**: false positives are acceptable, false negatives are not. It is
**prefix matching on raw strings, not glob semantics** — `src/a*` and `src/ab` overlap because `src/a` is a
prefix of `src/ab`.

`ScopeIndex` is the batch form: a compressed radix trie of bases plus a set of directories containing
file-like bases. `overlapsAny(scope)` returns true if any pattern's base has a prefix relationship with an
indexed base, or if the pattern is file-like and its directory (≠ `.`) already holds an indexed file-like
pattern. `add(scope)` mutates the index. The trie compares **UTF-16 code units**, matching JS `startsWith`;
in Rust, compare by `char`/byte consistently and cover the surrogate-pair cases with tests — the observable
contract is only that it agrees with the pairwise `patternsOverlap` function.

### 7.12 Autonomous task seeding (L1)

Each tick (unless suppressed), for every agent with `autonomous == true` **and** `status == "idle"`:
- Skip if the agent already has any non-terminal task assigned.
- Skip if seeded within the last **30000 ms** (per-agent cooldown, in-memory).
- Pick a goal: first an `active` goal assigned to this agent and not yet claimed this pass, else an `active`
  unassigned goal not yet claimed this pass. Claimed goal ids are tracked within the pass so two agents do not
  take the same goal.
- `role = agent.role ?? "general assistant"`.
- Create a task:
  - title: `[auto] <agent.name>: <goal.title sliced to 60>` or `[auto] <agent.name>: <role sliced to 60>`
  - description: with a goal —
    `## GOAL (highest priority)\n\n<goal.description || goal.title>\n\n---\nAgent role: <role>`;
    without — `Autonomous work cycle. Agent role: <role>`
  - `assignee = agent.id`, `labels = ["autonomous"]`, `priority = 3`, `goalId = goal?.id`
- Failures emit `orchestrator:error { context: "autonomous task for agent <id>" }` and do not abort the pass.

### 7.13 Locking (L1)

| Operation | Behaviour |
|---|---|
| `acquireLock(path)` | Serialized in-process. Reads the PID first. **If the PID parses**: a live PID with a fresh mtime fails with `{ acquired: false, pid }`; otherwise the stale lock is unlinked and a new one created with `O_CREAT\|O_EXCL`. **If the PID does not parse** (garbage, empty, unreadable) the whole stale-handling branch is **skipped — nothing is unlinked** — and the `O_CREAT\|O_EXCL` create then fails `EEXIST`, returning `{ acquired: false, pid: undefined }`. See §6.7 for the full table. |
| `releaseLock(path)` | Unlink, errors ignored. |
| `touchLock(path)` | `utimes(now, now)`, errors ignored. Called once per tick. |
| `checkLock(path)` | `{ locked, pid? }` — `locked` is false for a missing file **or a dead PID or a garbage PID**. **Does not consider mtime.** So `checkLock` reports "not locked" for a garbage lock that `acquireLock` will refuse — the two disagree. |
| `requireLock(path)` | **Dead code — no caller.** Its single occurrence is its own definition. Note it would also be *wrong* if used: `if (!result.acquired && result.pid)` is falsy when `pid` is `undefined`, so it would silently **succeed** on a garbage lock. Port it only if the ported tests require it; nothing else does. |

**The live acquisition path** is `withTemporaryLock` / `startWatch`, both of which do
`if (!lockResult.acquired) throw new LockConflictError(lockResult.pid!)` — unconditionally, with the non-null
assertion. That is why an `undefined` pid still throws (and prints `PID: undefined`) rather than being
swallowed the way `requireLock` would swallow it.

**PID liveness** = `kill(pid, 0)` succeeds, **or** fails with `EPERM` (process exists, not ours). Any other
error ⇒ dead.

The mtime check defends against PID recycling: a lock whose holder died and whose PID was reused by an
unrelated process looks "alive" but will not have been touched within 60 s.

Who takes the lock:
- `run --watch`, `serve` → hold for the process lifetime (`startWatch`).
- `run <id>`, `run --all`, `cancelTask`, `forceStopAgent` → acquire a **temporary** lock for the duration, then
  release; if the lock is already held by this instance, run inline instead.
- All read-only commands → **no lock**.

`isOwner` is true only while this instance holds the lock; state mutation paths check it.

### 7.14 Shutdown (L1)

`stop()` is idempotent. It: clears the interval; unsubscribes `task:created` and clears the debounce timer;
flushes the debounced state write; then **under the mutex**, for every running entry — abort the controller,
`killWithGrace(pid)` (default **10000 ms** grace), finish the run as `cancelled`, set the task to
`resolveFailureStatus(task)` (so it retries if budget remains), and set the agent `idle`. Then clear
`running`, clear `claimed`, clear `pid`/`started_at`, persist, release the lock, remove the signal handlers,
and resolve all `waitForStop()` waiters.

`SIGINT`/`SIGTERM` each emit `orchestrator:shutdown { reason: "Received <SIGNAL>" }` and then `stop()`.

### 7.15 Concurrency control (L1)

- **Global cap**: `scheduling.max_concurrent_agents` (default 6) bounds `|state.running|`.
- **One run per task**: enforced by `state.running` keyed on task id.
- **One run per agent**: enforced by the agent `status` field (only `idle` agents are candidates).
- **One orchestrator per project**: the PID lock.
- **State serialization**: a promise-chain mutex (`withStateLock`) wraps `tick`, `stop`, `cancelTask`,
  `forceStopAgent`, `freshDispatch`, `handleRunSuccess` and `handleRunFailure`. `IndexManager` has its own
  independent mutex per store instance.
- **Debounced state writes**: `saveStateLazy()` coalesces `last_event_at` updates on a **500 ms** timer;
  `flushStateLazy()` forces it before any critical transition.

### 7.16 Messages (L1)

- `send` validates: non-empty `body`; `0 < ttl_ms <= 604800000` (7 days), defaulting to **86400000** (24 h);
  the sender must exist **unless** it is the literal `cli`.
- `subject` is truncated to **200** chars and defaults to the literal `(no subject)`; `body` to **4000**.
  Both counts are **UTF-16 code units** — see §14.14 for the mid-surrogate case.
- **The body is validated trimmed and stored untrimmed** (L0, measured). `send` rejects on
  `!input.body.trim()` and then stores `input.body` — the original. So `"   "` is an error while `" padded "`
  is accepted and **keeps both spaces on disk**. The subject is neither trimmed nor validated; empty becomes
  the literal `(no subject)`. Trimming before storing passes every rejection test and diverges on every
  message anyone sends with a trailing newline. `fixtures/scenarios/cmd-msg-body-trimming.toml`.
- `expires_at = created_at + ttl_ms`.
- **`direct`** — `to_agent_id` required and must exist. One message.
- **`broadcast`** — one message per recipient. Recipients are all agents **excluding the sender** and
  **excluding `status == "disabled"`**; with `team_id`, further restricted to that team's members.

  **An unknown `team_id` fans out to *every* agent** (L0, measured). `message-service.ts` resolves the team and
  filters only `if (team)`, with no else branch, so an unresolvable id leaves the recipient list unfiltered —
  **and `team_id: "<unknown>"` is still persisted on every message**, leaving a dangling reference in the
  store. Measured in a four-agent tree: a real two-member team → 2 messages; `--team team_nosuch` → 4.
  **An implementation that validates the team id sends strictly fewer messages than the oracle.** Replicate.
  `fixtures/scenarios/cmd-msg-broadcast-unknown-team.toml`. The fan-out is a concurrent batch, so the
  recipients' order in `msg list` is not contract — see §6.10.1.
- **`lead`** — requires `team_id`; resolves to `team.lead_agent_id`. One message.
- `drainMailbox(agentId, taskId)` — called during dispatch: list pending, mark all delivered, emit
  `message:delivered` per message, return them for prompt injection.
- `listPending(agentId)` — `status == "pending"`, not expired, `to_agent_id == agentId`.
- `purgeExpired()` — deletes messages that are expired **or** were delivered more than **3600000 ms** ago;
  returns the count. One batched index update.
- `list()` sorts by `created_at` ascending via `localeCompare` — see the timestamp hazard in §6.4.

### 7.17 Context (L1)

- `set(key, value, ttlMs?)` — `ttlMs`, when given, must be finite, `> 0` and `<= 2592000000` (30 days), else a
  plain `Error` (exit 1): `TTL must be a positive number up to 2592000000ms (30 days)`.
- `created_at` is preserved across overwrites; `updated_at` is always refreshed.
- `expires_at = now + ttl_ms` when a TTL is given, else omitted.
- `get` lazily deletes an expired entry and returns `null`.
- `list` lazily deletes all expired entries (batched, with a single index rewrite) and returns the rest sorted
  by `key` ascending (`localeCompare`).
- `getAll()` → `{ key: value }` map built from `list()`.
- Keys go through `sanitizeId` (§6.2) — invalid keys throw.

### 7.18 Teams (L1)

- `create` — name required and unique; the lead agent must exist; members are validated and de-duplicated
  against the lead. Emits `team:created`, then `team:member_joined` per additional member.
- `join` — rejects an existing member; the agent must exist. Emits `team:member_joined`.
- `leave` — **the lead cannot leave** (`Lead cannot leave team. Disband the team or transfer lead first.`).
  Emits `team:member_left`.
- `add-task` — the task must exist; appending is idempotent; emits `team:task_added` only on an actual append.
- `set-lead` — the agent must already be a member; demotes the previous lead to `member`.
- `disband` — sets `status = "disbanded"` (does **not** delete the file); emits `team:disbanded`.
- `findTeamForAgent` — first `active` team containing the agent.

> `team:task_claimed` is declared in the event union but is **never emitted** by any code path, and
> `TeamConfig.auto_claim` / `max_concurrent_tasks` are stored but never read by the orchestrator. Team task
> pools do not currently affect dispatch. Preserve the data model and the CLI; do not invent the behaviour.

### 7.19 Event types (L1 — the names and payload fields are contract)

Emitted synchronously; handler exceptions are caught and logged, never propagated. Subscribers can register
per-type (`on`, `once`, `off`) or wildcard (`onAny`). A listener count above `maxListeners` (default 10) logs a
one-time warning per type.

| Event | Payload |
|---|---|
| `task:created` | `task: Task` |
| `task:assigned` | `taskId, agentId` |
| `task:status_changed` | `taskId, from, to` |
| `task:auto_reviewed` | `taskId, passed, results` |
| `task:scope_overlap` | `taskId, overlappingTaskId, patterns` |
| `task:cascade_failed` | `taskId, failedDependencyId, reason` |
| `task:orphaned` | `taskId` |
| `agent:started` | `agentId, taskId, runId` |
| `agent:output` | `runId, agentId, data` |
| `agent:file_changed` | `runId, agentId, path` |
| `agent:completed` | `runId, agentId, success` |
| `agent:error` | `runId, agentId, error, errorKind?` |
| `agent:autonomous_toggled` | `agentId, autonomous` |
| `run:retry` | `runId, attempt, delay_ms` |
| `orchestrator:tick` | `running, queued` |
| `orchestrator:stall_detected` | `runId` |
| `orchestrator:error` | `error, context, fatal` |
| `orchestrator:shutdown` | `reason` |
| `workspace:merge_succeeded` | `taskId, branch` |
| `workspace:merge_conflict` | `taskId, branch, conflictInfo` |
| `message:sent` | `messageId, fromAgentId, toAgentId, channel` |
| `message:delivered` | `messageId, toAgentId, taskId` |
| `team:created` | `teamId, name, leadAgentId` |
| `team:member_joined` | `teamId, agentId` |
| `team:member_left` | `teamId, agentId` |
| `team:task_claimed` | `teamId, taskId, agentId` *(never emitted)* |
| `team:disbanded` | `teamId` |
| `team:task_added` | `teamId, taskId` |
| `goal:created` | `goalId, title` |
| `goal:status_changed` | `goalId, from, to` |
| `goal:updated` | `goalId` |
| `goal:deleted` | `goalId` |

### 7.20 Event data caps (L1)

Three layers, applied in order:

1. `readLines` caps each stdout line at **16384** characters (truncated, not split). A truncated JSON line
   becomes unparseable and falls through the adapter's parse-failure path.
2. `serializeEventData(data, 8192)` before the JSONL write — stringify if not already a string, then truncate
   with a trailing `…`.
3. `serializeEventData(serialized, 4096)` before the event-bus emit, for TUI consumption.

Additionally `appendJsonl` may truncate further to fit `PIPE_BUF` (§6.9), and `proof.agent_summary` is capped
at 2000 characters.

---

## 8. Adapters

Eight adapters. Registration order (which is also the `doctor` probe order and `listKinds()` order) is L0:
**`claude, codex, cursor, shell, opencode, pi, grok, antigravity`**.

`AdapterRegistry.require(kind)` on an unknown kind throws a plain `Error`:
`Unknown adapter: "<kind>". Available: <kinds joined by ", ">`.

### 8.1 The adapter contract (L1)

```rust
pub struct ExecuteParams {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: PathBuf,
    pub env: BTreeMap<String, String>,
    pub config: AgentConfig,
    // cancellation: see ARCHITECTURE.md §6
}

pub enum AgentEventKind { Output, FileChange, Command, ToolCall, Error, Done }

pub struct AgentEvent {
    pub kind: AgentEventKind,
    pub timestamp: String,        // ISO-8601, generated at parse time
    pub data: serde_json::Value,
    pub tokens: Option<TokenUsage>,
    pub error_kind: Option<AdapterErrorKind>,
}

pub struct AdapterTestResult {
    pub ok: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub error_kind: Option<AdapterErrorKind>,
    pub details: Option<serde_json::Value>,
}
```

The TS `execute()` returns `{ pid, events: AsyncGenerator<AgentEvent> }` **synchronously** — the PID is
available before the first event is pulled. The Rust equivalent must preserve this: `execute` returns the PID
and a stream, not a future of both. The generator body must not be required to run for the process lifecycle
listeners to attach (the TS code attaches `close`/`error`/stderr listeners eagerly for exactly this reason —
a fast-failing child must not be missed).

`AgentEvent.type → RunEvent.type` mapping (L0):
`output → agent_output`, `file_change → file_changed`, `command → command_run`, `tool_call → tool_call`,
`error → error`, anything else → `done`.

### 8.2 argv per adapter (L0)

Every argv below is exact and ordered. Conditional arguments are marked. No adapter does shell quoting (the
shell adapter delegates to `bash`), none sets a per-process timeout, and **none has any resume/session/continue
flag**.

| Adapter | Binary | argv |
|---|---|---|
| `claude` | `claude` | `--print`, `--output-format stream-json`, `--max-turns <config.max_turns ?? 50>`, `--verbose`, `--dangerously-skip-permissions`, [`--model <m>`], [`--effort <e>`], [`--system-prompt <sp>`], then **`<prompt>` as the last positional** |
| `codex` | `codex` | `exec`, `--json`, `--sandbox danger-full-access`, [`--model <m>`], `-` — **prompt goes on stdin** (write + `end()`) |
| `cursor` | `cursor-agent`, falling back to `agent` | `-p`, `--output-format stream-json`, `--workspace <workspace>`, `--yolo`, `--trust`, [`--model <m>`], then `<fullPrompt>` last |
| `opencode` | `opencode` | `run`, `--format json`, [`--model <m>`], then `<fullPrompt>` last |
| `pi` | `pi` | `--mode rpc`, [`--model <m>`], [`--thinking <effort>`], [`--append-system-prompt <sp>`] — **prompt sent as a JSONL RPC command on stdin, which stays open** |
| `grok` | `grok` | `-p <prompt>` (**prompt is argv[1], raw**), `--output-format streaming-json`, `--permission-mode bypassPermissions`, `--always-approve`, `--cwd <workspace>`, [`--model <m>`], [`--effort <e>`], [`--max-turns <n>`], [`--system-prompt-override <sp>`] |
| `antigravity` | `agy` | `-p`, `<fullPrompt>` (**argv[2], not last**), `--dangerously-skip-permissions`, [`--model <m>`] |
| `shell` | `bash` | `-lc`, `<config.command>` — **prompt passed via the `ORCHESTRY_TASK_PROMPT` env var** |

**stdin protocols** (the two adapters that use stdin — L0):

- **`codex`**: a **single write** of the combined prompt with **no trailing newline**, then `end()` to close
  stdin. `buildFullPrompt` joins with `\n\n`, so the bytes are `"<system>\n\nuser"` or just `"<user>"`.
- **`pi`**: writes one JSONL RPC frame — `{"id":"orch-<epoch_ms>","type":"prompt","message":<prompt>}` followed
  by `"\n"` — and then **leaves stdin open** (it is a bidirectional session). After a terminal event the
  adapter itself calls `killWithGrace(pid, 1000)` — a **1000 ms** grace, unique to pi.

Notes that are easy to get wrong:

- `claude` always passes `--max-turns`, defaulting to **50** (`?? 50`). `grok` passes `--max-turns` only when
  `config.max_turns` is **truthy**, so `0` omits it.
- `opencode` has **no `--dir`/workspace flag** — the workspace is conveyed only through the spawn `cwd`.
- The shell adapter's `ORCHESTRY_TASK_PROMPT` is the **only** environment variable any adapter sets itself;
  everything else comes from the orchestrator's `params.env`.
- `pi` uses **`--thinking`** for effort and **`--append-system-prompt`** (never `--system-prompt`). It
  deliberately does **not** pass `--no-session`.
- `grok` passes `--cwd <workspace>` *in addition to* setting the spawn cwd.
- `cursor` resolves its binary only inside `test()`; if `test()` never ran, `execute()` spawns `cursor-agent`.
- `antigravity` puts the prompt at argv index 2, before `--dangerously-skip-permissions`.
- System prompt handling differs: `claude`/`pi`/`grok`/`antigravity` use
  `params.systemPrompt ?? config.system_prompt`; `codex`/`cursor`/`opencode`/`shell` use `params.systemPrompt`
  **only** and ignore `config.system_prompt`.
- `buildFullPrompt(system, user)` = `system ? system + "\n\n" + user : user`.

### 8.3 Spawn environment (L0)

Every adapter spawns with `{ ...process.env, ...params.env }` — the parent environment is **inherited** and
then overlaid. `params.env` is assembled by the orchestrator as
`{ ...agent.config.env, ORCH_AGENT_ID, ORCH_AGENT_NAME, ORCH_TASK_ID }` — so the three `ORCH_*` variables
**override** any same-named key in the agent's configured env.

The shell adapter additionally sets `ORCHESTRY_TASK_PROMPT` to the full combined prompt.

### 8.4 Process spawning (L0/L1)

`ProcessManager.spawn`:
- `stdio: ['ignore', 'pipe', 'pipe']` by default — **stdin is ignored**, which is why every adapter passes a
  permission-bypass flag (an interactive prompt would hang forever). `codex` and `pi` override stdin to `pipe`.
- `detached: true` — **a new process group**, so killing `-pid` reaps the whole tree (vitest, playwright, …).
- `proc.unref()` — the parent may exit without waiting; the stdout/stderr pipes keep their own refs while being
  read.
- No PID ⇒ throw `Failed to spawn process: <command>`.

`isAlive(pid)` = `kill(pid, 0)` succeeds, or fails with `EPERM`.

`kill(pid, signal = SIGTERM)` — try `kill(-pid, signal)` (the group) **first**; on failure fall back to
`kill(pid, signal)`; swallow all errors.

`killWithGrace(pid, graceMs = 10000)` — return if already dead; send `SIGTERM`; poll `isAlive` every **200 ms**
until the deadline; then send `SIGKILL`. Call sites use different grace periods:
`cancelTask`/`forceStopAgent` → **3000 ms**, stall detection and adapter abort → **5000 ms**,
`stop()` and `adapter.stop()` → the **10000 ms** default.

There is **no registry of spawned processes** — the PID in `state.running` is the only handle.

### 8.5 Line reading (L1)

`readLines(stream)` is a byte-level newline splitter, not a text-mode reader:
- Splits on `0x0A` only; a `\r` is retained as part of the line.
- **Empty lines are skipped** (`if (newlineIdx > offset)`).
- A trailing fragment with no final newline **is** yielded.
- Each line is capped at **16384** characters.
- Decoded as UTF-8 per line.

`pi` does its own framing and does not use `readLines`.

### 8.6 stdout parsing and `done` sources (L1)

| Adapter | Format | `done` from | Synthesises `done`? |
|---|---|---|---|
| `claude` | JSONL | `{"type":"result"}` | no |
| `codex` | JSONL | `{"type":"turn.completed"}` | no |
| `cursor` | JSONL | `{"type":"result"}` | no |
| `opencode` | JSONL | `step_finish` whose `reason` is anything other than `"error"`/`"tool-calls"` (including absent) | no |
| `pi` | JSONL RPC | `agent_end` (possibly promoted by `agent_settled`) with no failure | no |
| `grok` | JSONL | `{"type":"end"}` | **yes** — on a clean exit 0 with no `end` record |
| `antigravity` | plain text lines | — | **yes** — always, after exit 0 or `null`, when not aborted |
| `shell` | plain text lines | — | **never** |

`claude` line dispatch (L1):

| `parsed.type` | Emitted |
|---|---|
| `assistant` | `output`, data = `parsed.message ?? parsed` |
| `tool_use` | `tool_call`, data = whole `parsed` |
| `tool_result` | `output`, data = whole `parsed` |
| `error` | `error`, data = `parsed.error ?? parsed`, `errorKind = classifyAdapterError(stringified)` |
| `result` | `done`, data = whole `parsed`, `tokens = extractTokens(parsed, { statsFallback: true })` |
| anything else | `output`, data = whole `parsed` |
| unparseable line | `output`, data = the raw line **string** |

`data` shapes are **not uniform across adapters** — the `interface.ts` docstring declares a canonical shape
(`output: { text, raw }` etc.) but acknowledges that claude/cursor emit `parsed.message`, codex emits
`{ text, raw }`, opencode emits bare strings, and shell emits bare line strings. Consumers must be
**defensive**, exactly like the TS TUI's `formatAgentOutput`. Reproduce the per-adapter shapes as they are;
do not normalize them.

### 8.7 Token extraction (L1)

`extractTokens(parsed, { statsFallback })`:
1. `usage = parsed.usage`; if absent and `statsFallback` (claude only), `usage = parsed.stats?.usage`.
2. No `usage` ⇒ `None`.
3. `input` = `usage.input_tokens` else `usage.inputTokens`; **if neither is a number, return `None`**.
4. `output` = `output_tokens` / `outputTokens` else 0.
5. `reasoning` = `reasoning_tokens` / `reasoningTokens` else 0.
6. `cache_read` = `cache_read_input_tokens` / `cacheReadTokens` else 0.
7. `cache_write` = `cache_creation_input_tokens` / `cacheWriteTokens` else 0.
8. `total = input + output + reasoning` — **cache counters excluded**.

### 8.8 Error classification (L0)

`classifyAdapterError(error, exitCode?)` lowercases the message and tests **in this order**; the first match
wins, so **string matches always beat the exit code**:

| # | Condition (substring of the lowercased message) | Result |
|---|---|---|
| 1 | `enoent` or `spawn failed` | `spawn_failed` |
| 2 | `not found` or `command not found` or `no such file` | `adapter_not_found` |
| 3 | `auth` or `unauthorized` or `401` or `invalid api key` or `authentication` | `auth_failed` |
| 4 | `timeout` or `timed out` or `etimedout` | `timeout` |
| 5 | `rate limit` or `429` or `too many requests` | `rate_limit` |
| 6 | `exitCode` is present and non-zero | `process_crash` |
| 7 | otherwise | `unknown` |

Exactly **7** enum values, all matching `^[a-z_]+$`:
`adapter_not_found, auth_failed, timeout, rate_limit, process_crash, spawn_failed, unknown`.

Quirks to preserve: `auth` is a bare substring, so `"author"` classifies as `auth_failed`; `401`/`429` are
bare substrings, not status-code parsing. The stderr tail is appended to the message **before** classification,
so stderr content can change the class of an exit-code failure.

Conformance vectors (port these as a table test):

```
'spawn ENOENT'                  → spawn_failed        'Rate limit exceeded'           → rate_limit
'Spawn failed: /usr/bin/foo'    → spawn_failed        'HTTP 429 Too Many Requests'    → rate_limit
'claude: command not found'     → adapter_not_found   'Too many requests, slow down'  → rate_limit
'CLI not found on PATH'         → adapter_not_found   ('Segmentation fault', 139)     → process_crash
'Authentication failed'         → auth_failed         ('Unknown error occurred', 1)   → process_crash
'Request unauthorized'          → auth_failed         ('Something weird', 0)          → unknown
'HTTP 401 error'                → auth_failed         'Something else'                → unknown
'Invalid API key provided'      → auth_failed         ('ENOENT /bin/claude', 127)     → spawn_failed
'Request timeout after 60s'     → timeout             ('Unauthorized access', 1)      → auth_failed
'Connection timed out'          → timeout
'connect ETIMEDOUT 1.2.3.4:443' → timeout
```

### 8.9 `ERROR_HINTS` (L0)

| Kind | `message` | `fix` | `doctorHint` |
|---|---|---|---|
| `adapter_not_found` | `CLI is not installed.` | `Install it with: npm i -g @anthropic-ai/claude-code` | `true` |
| `auth_failed` | `API key is invalid.` | `Check it with: claude auth status` | — |
| `timeout` | `Agent timed out.` | `Increase the timeout with: orch config set agent_timeout <ms>` | — |
| `rate_limit` | `API rate limit reached.` | `Wait and retry with: orch task retry <id>` | — |
| `process_crash` | `Agent process crashed.` | `Try again with: orch task retry <id>` | — |
| `spawn_failed` | `Failed to start the process.` | `Check PATH and file permissions` | — |
| `unknown` | `Unknown error.` | `Run: orch doctor` | `true` |

> The `timeout` fix references `orch config set agent_timeout <ms>`, which is **not a real config key** (the
> real path is `defaults.agent.timeout_ms`). Replicated as-is — see §14.3.

### 8.10 Exit handling (L1)

`createStreamingEvents` (used by claude, codex, cursor, opencode):
- Attaches `close`, `error` and the stderr tail capture **eagerly**, before the generator is first polled.
- Keeps a bounded **4096-byte tail** of stderr (an exactly-sized copy, not a view).
- Reads stdout to EOF, then **destroys the stdout stream** in a `finally` to release the FD immediately.
- Then awaits exit, and throws only when **not aborted and no `done` event was seen**:
  - spawn error ⇒ `appendStderrTail(err.message, <AdapterName>, tail)`
  - non-zero, non-null exit ⇒ `appendStderrTail("<AdapterName> process exited with code <n>", …)`
  - in both cases the thrown error carries `errorKind = classifyAdapterError(message, exitCode)`

`appendStderrTail(msg, name, tail)` = `msg` when the tail is empty, else
`"<msg>\n--- <name> stderr (tail) ---\n<tail>"`.

The `gotDoneEvent` suppression matters: an adapter that emitted `done` and *then* exited non-zero is treated as
a **success**.

Shell adapter exit messages (L0): `Shell command exited with code <n>`, or
`Shell command terminated by signal <sig|unknown>` when the exit code is null. A missing `config.command`
yields a handle with `pid: 0` whose stream emits a single `error` event with data
`Shell adapter requires a command in agent config` and `errorKind = spawn_failed`.

### 8.11 Backpressure (L1)

`EventBuffer` is a ring buffer with capacity **1024**. `push()` returns a future that resolves when space is
available; the consumer dequeues in O(1) with event-driven wakeup (no polling). The shell adapter reads stdout
and stderr **concurrently** into one buffer and closes it when both drain — reading them sequentially would
deadlock on a full pipe. Reproduce the concurrency, not the data structure.

### 8.12 Model discovery (L1)

`src/infrastructure/models/model-discovery.ts` probes installed CLIs to list available models, with
per-adapter regex parsers, a process-lifetime cache, and hard-coded fallback option lists used when the probe
fails. It feeds the TUI model picker only; nothing in the engine depends on it. Port the fallback lists and the
probe table verbatim from the source — they are data, and `test/unit/infrastructure/model-discovery.test.ts`
pins the parsers.

### 8.13 Model tiers (L0 — this is data)

`resolveModel(adapter, tier)`; unknown adapter ⇒ `""`. `defaultModelForAdapter(a)` = the `balanced` entry.

| Adapter | `capable` | `balanced` | `fast` |
|---|---|---|---|
| `claude` | `claude-opus-4-6` | `claude-sonnet-4-6` | `claude-haiku-4-6` |
| `opencode` | `openrouter/anthropic/claude-opus-4.6` | `""` | `openrouter/google/gemini-2.5-flash` |
| `codex` | `gpt-5.4` | `gpt-5.3-codex` | `gpt-5-mini` |
| `cursor` | `auto` | `auto` | `auto` |
| `pi` | `openai-codex/gpt-5.5` | `openai-codex/gpt-5.5` | `openai-codex/gpt-5.5` |
| `grok` | `grok-build` | `grok-composer-2.5-fast` | `grok-composer-2.5-fast` |
| `antigravity` | `gemini-3-pro` | `""` | `gemini-3-flash` |
| `shell` | `""` | `""` | `""` |

`SUPPORTED_ADAPTERS` display order (L0):
`claude, opencode, codex, cursor, pi, grok, antigravity, shell` — note this differs from the registration
order in §8.

---

## 9. Prompts, templates and skills

### 9.1 Prompt assembly (L0 — the rendered text is contract)

1. `systemTemplate = config.prompt.system_template ?? DEFAULT_SYSTEM_TEMPLATE`
2. `userTemplate = config.prompt.user_template ?? DEFAULT_USER_TEMPLATE`
3. If `config.prompt.template` is set (**legacy single-template mode**), render **only** that one into `prompt`
   and leave `systemPrompt` unset.
4. Otherwise render both: `systemPrompt` from the system template, `prompt` from the user template.
5. If the agent has `config.skills`, load library skills and append the block:
   - split mode → `systemPrompt = systemPrompt + "\n\n" + skillBlock`
   - legacy mode → `prompt = prompt + "\n\n" + skillBlock`
6. `run.prompt` persists the **user** prompt only — the system prompt is not stored.

Render timeout: **5000 ms** by default, implemented by the caller, not the template engine. On timeout:
`Template render timed out after <ms>ms`.

The two default templates are inline constants in `src/infrastructure/template/template-engine.ts`
(`DEFAULT_SYSTEM_TEMPLATE` lines 317-360, `DEFAULT_USER_TEMPLATE` lines 366-436). **Copy them byte for byte**
into the Rust source. `DEFAULT_PROMPT_TEMPLATE = DEFAULT_SYSTEM_TEMPLATE + "\n" + DEFAULT_USER_TEMPLATE`
(deprecated, but asserted by a test).

### 9.2 Template context (L0 — variable names and nesting)

```
project.name                string
project.description         string?
task.id                     string
task.title                  string
task.description            string
task.priority               number
task.labels                 string[]
task.scope                  string[]?
task.is_autonomous          bool        (labels includes "autonomous")
task.goal_id                string?     (from Task.goalId)
agent.id, agent.name        string
agent.role                  string?
agents[]                    { id, name, role?, adapter }
attempt                     number | null   ← null when attempt <= 1, NEVER 0
workspace_path              string
retry.previous_error        string       (only when attempt > 1)
retry.previous_output       string
feedback                    string?
shared_context              { key: value }?   (absent when empty)
messages[]                  { id, from, subject, body, sent_at, reply_to? }?  (absent when empty)
goal                        { id, title, description, status, task_names[], progress? }?
```

Construction rules that matter:
- `attempt` is `null` for the first attempt, not `0` — because `0` is **truthy** in Liquid.
- `retry` is only populated when `attempt > 1`, even if a retry context is supplied.
- In `agents[]`, the **current agent's own `role` is set to `undefined`** (it is already in the system prompt).
- `shared_context` is `undefined` (not `{}`) when empty, and otherwise passes through
  `filterRelevantContext` (§9.3).
- `messages` is `undefined` (not `[]`) when empty. `messages[].from` is the sender's **name** when resolvable,
  falling back to the raw agent id.
- `goal.task_names` entries are formatted `[<status>] <title>`.
- `goal.progress` comes from the context key `<goalId>-progress`.

### 9.3 `filterRelevantContext` (L1)

Scores each shared-context entry, keeps at most **15**:

| Points | Rule |
|---|---|
| +10 | key (lowercased) starts with the task's `goalId` (lowercased) |
| +8 | key **or value** (lowercased) contains the agent name (lowercased) |
| +6 | any task scope pattern, stripped of `*` and trailing `/`, appears in the key or value (first match only) |
| +4 | key starts with `<roleKeyword>-` or `<roleKeyword>_` (first match only) |
| +1 | key matches `^(bug|perf|stability|docs|arch|spec)-` (case-insensitive) |

Sort by score descending (stable). Take all entries with score > 0 up to 15, then pad with score-0 entries up
to 15. Values are **never truncated**.

Role keywords: the first word of the lowercased agent name (if longer than 1 char); plus
`front-end, frontend, tui` if the name contains `front` or `tui`; plus `marketer, marketing, cmo` if it
contains `market` or `cmo`; plus the first word of the lowercased role (if longer than 2 chars and not already
present).

### 9.4 Liquid surface actually used (L0)

**No custom filters or tags are registered.** Everything is built-in liquidjs, configured with
`strictFilters: false, strictVariables: false`.

| Construct | Occurrences |
|---|---|
| `{{ … }}` | all interpolations |
| `{% if %}` / `{% endif %}` | 17 distinct conditions |
| `{% for x in y %}` / `{% endfor %}` | `agents`, `shared_context`, `messages`, `goal.task_names` |
| `default:` filter | 3 occurrences, all on `task.goal_id` |
| `.size` | `messages.size` ×2, `goal.task_names.size` ×2 |
| object iteration with `entry[0]`/`entry[1]` | `shared_context` |
| operators | `!=`, `>`, bare truthiness |

**Not used anywhere:** whitespace control (`{%- -%}`), `assign`, `capture`, `unless`, `case/when`, `include`,
`render`, `raw`, `comment`, `cycle`, `tablerow`, `increment`, `else`/`elsif`, `forloop`, and every date,
number and string filter.

### 9.5 liquidjs vs the Rust `liquid` crate — measured differences

Measured against `liquid` 0.26.11 and `liquidjs` 10.27.2 with the actual constructs above:

**Identical (verified):** `.size` on arrays; `!=` and `>`; Shopify truthiness (`nil`/`false` falsy; `0`, `""`
and `[]` **truthy**); `default:` on an explicit `null`; inline `{% if %}` within a line; `for` over an array of
objects; **and, critically, all whitespace handling** — including the `\n\n\n` runs that the real prompts
produce. `A\n{% if t %}\nB\n{% endif %}\nC` renders to `"A\n\nB\n\nC"` in both.

**Differs — must be mitigated:**

| # | Difference | Mitigation (mandatory) |
|---|---|---|
| 1 | **Missing key or variable** — liquidjs renders `""`; the Rust crate raises `Unknown index` / `Unknown variable`. | Always build the globals object with **every** documented variable present, using explicit `nil` for absent optionals. Never omit a key. Verified: `{"task":{"goal_id":null}}` renders identically in both. |
| 2 | **Object iteration order** — liquidjs iterates insertion order; the Rust crate's `Object` does not. | Pass `shared_context` as an **ordered array of `[key, value]` pairs**. Verified to render identically through `entry[0]`/`entry[1]` while preserving order. Keep it `nil` (not `[]`) when empty, matching the TS `undefined`. |
| 3 | **Unknown filter** — liquidjs passes the value through (`strictFilters: false`); the Rust crate fails at **parse** time. | Affects only user-supplied templates in `config.prompt.*`. Documented divergence (§14.6); surface the parse error as a clear message rather than a panic. |
| 4 | Nested access through a `nil` parent (`retry.previous_error` when `retry` is `nil`) errors in the Rust crate. | Harmless for the shipped templates, since every such access is inside an `{% if parent %}` guard which short-circuits. Keep `retry`/`goal`/`messages` as `nil` rather than empty objects — an **empty object is truthy** in Liquid and would wrongly enter the block. |
| 5 | `liquid` 0.26.11 pulls `kstring` 2.0.5, whose MSRV (1.96.0) exceeds the pinned toolchain (1.94.1). | Pin `kstring = "2.0.2"` in the workspace, or raise the toolchain. Noted in `Cargo.toml`. |

**The acceptance gate for this subsystem** is a golden-fixture suite: render both default templates through
the TS binary across the full matrix of branch combinations, store the outputs, and assert the Rust renderer
reproduces them byte for byte. See [ARCHITECTURE.md §8.3](ARCHITECTURE.md).

### 9.6 Skill loading (L1)

- **Library directory resolution**: walk up from the executable's own directory up to 5 levels looking for
  `skills/library/`; fall back to `<dir>/../../../skills/library`. In r-orch this becomes: the directory
  containing the binary, then `$CARGO_MANIFEST_DIR` in debug builds, then an installed share path. The
  **library content is shipped with the binary** and must be byte-identical to `parity/orch-ts/skills/library/`.
- **Name validity**: `^[a-z0-9-]+$`. The check runs **after** the cache lookup but **before** any filesystem
  access — this is the path-traversal defence. A rejected name returns `None` and is **not** cached (unlike a
  missing file, which is).
- **Library vs MCP**: a skill name **containing `:`** is an MCP skill, handled natively by the Claude CLI, and
  is **silently skipped** by the loader.
- **Missing skill**: logs `[orch] skill library: "<name>" not found in <dir>` to **stderr**, caches the empty
  result, and continues. Never throws.
- **Caching**: per-loader-instance, process lifetime, including negative results.
- **Output format** (L0): `## Skills\n\n` followed by, per resolved skill, `### <name>\n\n<file content>`,
  joined by `\n\n`. Returns `""` when no library skills resolve (and the caller then appends nothing).
- **The file content is injected raw, including its YAML frontmatter.** The loader never parses or strips it.
  The library has **26** `.md` files whose frontmatter keys are `name`, `version`, `description` (plus
  `benefits-from`, only in `plan-eng-review.md`) — all of it lands verbatim in the agent's system prompt.
- Skill order follows the order of `agent.config.skills`, not the directory order.
- `listAvailable()` → `.md` basenames, **sorted**, cached.

---

## 10. Workspaces

### 10.1 Mode resolution (L0)

`task.workspace_mode ?? agent.config.workspace_mode ?? config.defaults.agent.workspace_mode ?? "worktree"`.

Modes: `shared` (use the project root as-is), `worktree` (git worktree on a dedicated branch),
`isolated` (a full local clone, falling back to rsync).

Any mode other than `shared` first requires a git repository: `git rev-parse --is-inside-work-tree`. A
**positive** result is cached for the process; a negative result is **re-probed every time** (the user may run
`git init` meanwhile). Failure ⇒ `WorkspaceError` (exit 6):

```
workspace_mode "<mode>" requires a git repository
```
hint:
```
Run: git init && git add -A && git commit -m "Initial commit"
         Or set workspace_mode: shared in .orchestry/config.yml
```
(the hint contains a literal newline followed by 9 spaces).

### 10.2 Worktree preparation (L0 — paths, branch names and command sequence)

- **Directory**: `<projectRoot>/.orchestry/workspaces/<taskId>` (the task id through `sanitizeId`).
- **Branch**: `orchestry/<taskId>/<titleSlug>` where
  ```
  titleSlug = title.toLowerCase()
                   .replace(/[^a-z0-9]+/g, '-')
                   .replace(/^-|-$/g, '')
                   .slice(0, 40)
  ```
  **Dash-trimming happens before the 40-char slice**, so a truncated slug may end with `-`. A title with no
  `[a-z0-9]` characters slugs to `""` and the branch falls back to `orchestry/<taskId>/<taskId>` — tests pin
  `orchestry/tsk_cjk1/tsk_cjk1` for `修复数据库连接` and `orchestry/tsk_emo1/tsk_emo1` for `🚀🎉✨`.
- **Idempotent**: if the directory already exists, return `{ path, branch }` without touching git.
- **Command sequence**:
  1. `git worktree add <path> -b <branch>`
  2. on failure → `git worktree prune`
  3. then `git worktree add <path> <branch>` (reuse the existing branch; **no `-b`**)
  4. still failing ⇒ `WorkspaceError`: `git worktree add failed with code <n>`, hint
     `Run: git worktree prune && git branch | grep orchestry | xargs -r git branch -D`
  5. on success, recursively remove `<path>/.orchestry` so worktrees do not nest state.

### 10.3 Isolated preparation (L1)

1. `git clone --local --no-hardlinks <projectRoot> <path>`; a non-zero exit falls through to:
2. `rsync -a --exclude-from=<orchestryDir>/workspace-exclude ./ <path>/` with `cwd = projectRoot`; a non-zero
   exit throws a **plain** `Error` (`rsync failed with code <n>`), not a `WorkspaceError` — so it surfaces as
   exit 1, not 6.
3. Recursively remove `<path>/.orchestry`.

### 10.4 Merge-back (L0 for the git commands, L1 for classification)

There is exactly **one** merge strategy and no strategy option:

```
git merge --no-ff <branch> -m "Merge <branch>"      cwd = projectRoot
```

- stdout and stderr are appended into **one** buffer, guarded at **2000** chars (the guard is checked *before*
  appending, so the buffer can overshoot by one chunk).
- exit 0 ⇒ `{ success: true }`.
- otherwise `trimmed = output.slice(0, 1000)`;
  `isConflict = trimmed.includes("CONFLICT") || trimmed.includes("Merge conflict")`.
  - conflict ⇒ run `git merge --abort` (cwd = projectRoot), then `{ success: false, conflictInfo: trimmed }`
  - **not** a conflict (e.g. `fatal: branch not found`) ⇒ `{ success: false, conflictInfo: trimmed }` with
    **no abort**
- a spawn `error` event ⇒ `{ success: false, conflictInfo: err.message }`
- `mergeBack` **never rejects**.

Orchestrator integration: success ⇒ emit `workspace:merge_succeeded` then `cleanup(taskId, branch)`.
Failure ⇒ emit `workspace:merge_conflict` and force the task to `review` with summary prefix
`MERGE CONFLICT: <info>`, **returning early so auto-approve is skipped**. A thrown error ⇒ summary prefix
`MERGE ERROR: <msg>`, same early return.

### 10.5 Cleanup (L1)

```
git worktree remove --force <path>        awaited first, exit code ignored
then concurrently:
  git branch -D <branch>                  only when a branch is given
  rm -rf <path>                           errors swallowed
```
Never throws; cleaning a non-existent workspace succeeds. Called after a successful merge and on permanent
task failure, both wrapped to emit `orchestrator:error { context: "workspace cleanup for <taskId>" }`.

### 10.6 `getChangedFiles(branch)` (L1)

```
git merge-base HEAD <branch>                  → mergeBase (trimmed); empty ⇒ []
git diff --name-only <mergeBase>...<branch>   → non-zero or empty stdout ⇒ []
                                              → else split on '\n', drop empties
```
Any throw ⇒ `[]`. Called only when the adapter reported no changed files.

### 10.7 Path validation (L0)

`validateWorkspacePath(p, root)` resolves both and throws
`Workspace path "<p>" is outside project root` unless the resolved path equals the root or starts with
`root + separator`.

---

## 11. Serve mode, doctor, init, org, update

### 11.1 `serve` (L0 for the log record fields, L2 for the text format)

`serve` starts the full orchestrator in watch mode and streams structured logs. It **does not** run the update
check.

Flags: `--once`, `--tick-interval <ms>` (overrides `scheduling.poll_interval_ms`), `--log-file <path>`
(append mode, in addition to stdout), `--log-format json|text` (default `json`), `--verbose`.

**JSON record** (`--log-format json`, the default): one compact JSON object per line, `JSON.stringify(entry) + "\n"`.
Every record has:

| Field | Type |
|---|---|
| `ts` | ISO-8601 instant, generated at transform time (**not** the originating event's timestamp) |
| `level` | `debug \| info \| warn \| error` |
| `event` | the `OrchestratorEvent` type string, or a serve-local event name |

plus the per-event fields below. **Events not listed are suppressed entirely** (the transform returns null).

| `event` | `level` | Extra fields |
|---|---|---|
| `orchestrator:tick` | `info` | `running`, `queued`, `heap_mb` — see below |
| `orchestrator:shutdown` | `info` | `reason` |
| `orchestrator:error` | `error` if `fatal` else `warn` | `error`, `context`, `fatal` |
| `orchestrator:stall_detected` | `warn` | `runId` |
| `agent:started` | `info` | `agentId`, `taskId`, `runId` |
| `agent:completed` | `info` if `success` else `warn` | `runId`, `agentId`, `success` |
| `agent:error` | `error` | `runId`, `agentId`, `error`, `errorKind` |
| `agent:output` | `debug` | `runId`, `agentId`, `data` **truncated to 200 chars** — **only when `--verbose`** |
| `agent:file_changed` | `info` | `runId`, `agentId`, `path` |
| `run:retry` | `warn` | `runId`, `attempt`, `delay_ms` |
| `task:created` | `info` | `taskId`, `title` |
| `task:status_changed` | `info` | `taskId`, `from`, `to` |
| `task:auto_reviewed` | `info` | `taskId`, `passed` *(the `results` array is **not** logged)* |
| `task:orphaned` | `warn` | `taskId` |
| `task:scope_overlap` | `warn` | `taskId`, `overlappingTaskId`, `patterns` |
| `task:cascade_failed` | `warn` | `taskId`, `failedDependencyId`, `reason` |
| `workspace:merge_succeeded` | `info` | `taskId`, `branch` |
| `workspace:merge_conflict` | `warn` | `taskId`, `branch`, `conflictInfo` |

Suppressed (no record at all): `task:assigned`, all `message:*`, all `team:*`, all `goal:*`,
`agent:autonomous_toggled`, and `agent:output` without `--verbose`.

#### `heap_mb` and the tick heartbeat

**`heap_mb` has no faithful Rust counterpart.** In TS it is `process.memoryUsage().heapUsed / 1048576` to one
decimal — a **V8 heap** measurement. Rust has no V8 heap. **Decision (team lead): r-orch reports process
**RSS in MiB** under the same field name**, to one decimal. The *field* is contract (a log consumer's schema
keeps working); the *value* is not comparable, so **the harness masks it**.

**`orchestrator:tick` records are a wall-clock heartbeat and are excluded from comparison.** The number of
ticks a run emits depends on how long the run took versus `poll_interval_ms`, so two binaries started seconds
apart legitimately emit different counts. Scenario steps that run `serve` carry **`@ticks-vary`**, which drops
`event == "orchestrator:tick"` records from the comparison. **Every other record is still compared in full** —
the exclusion is per-record-type, not per-step, so dropping ticks does not weaken the §11.1 contract for
anything else.

**Idle-tick throttling** (L1): a tick is "idle" when `running == 0 && queued == 0`. In non-verbose mode an idle
tick is logged only when `tickCounter % idleLogInterval == 0`. The counter increments on **every** tick
(including non-idle ones) and is never reset. `idleLogInterval` is **6**
(`IDLE_LOG_EVERY_N_TICKS = 6` in `src/cli/commands/serve.ts:16` — verified). With `--verbose`, every tick is
logged.

**Text format** (L2): `"<HH:MM:SS.mmm> <LEVEL padded to 5> <event>  <k=v ...>\n"` — the time is
`ts.slice(11, 23)`, the level is uppercased and right-padded to 5, there are **two** spaces before the field
list, and values are raw for strings, JSON-encoded otherwise.

**`--once`** (L0 for the exit code): start watch mode with `skipAutonomousSeeding: true`, then poll
`taskStore.list()` every **2000 ms** until either the task list is **empty**, or **every** task is terminal,
or an `orchestrator:shutdown` event was observed. Then `stop()`. Exit **1** if any task ended `failed`,
else **0**. Note the check is `status === "failed"` specifically — a `cancelled` task does **not** cause a
non-zero exit.

> **Usage trap — `serve --once` never returns while any task sits in `review`.** `review` is **not** a
> terminal status (terminal is `{done, failed, cancelled}`, §7.1), so the poll loop waits for it forever.
> A task lands in `review` and stays there whenever its agent's `approval_policy` is **`suggest`** or
> **`manual`** and the task has no `review_criteria` — nothing auto-approves it, and `--once` has no
> timeout.
>
> **This is reachable out of the box**: the `agt_creator` agent that `orch init` creates ships with
> `approval_policy: suggest` (§6.11), so a `serve --once` over a task assigned to the default agent hangs.
> It applies **equally to both binaries** — it is inherited behaviour, not a divergence, and is replicated.
> Documented for users, with all three escapes, in
> [docs/RUNBOOK.md](RUNBOOK.md#serve---once-hangs-on-a-task-in-review). **Keep it out of unattended
> scenarios**: a `--once` scenario must drive tasks to a terminal status or it will hang the harness
> rather than fail it.

**Flush on exit** (L1): all non-stdout/stderr streams are `end()`ed and awaited before the process exits, so a
`--log-file` is never truncated.

### 11.2 `doctor` (L1)

Runs **without** an initialized project (the CLI registers it specially on `NotInitializedError`).

Checks, in order: each adapter's `test()` in registration order
(`claude, codex, cursor, shell, opencode, pi, grok, antigravity`), then `git` version, then whether the cwd is
a git repository, then whether `.orchestry` is git-ignored, then the Node version, then the `.orchestry/`
summary line (`exists · N agents · M tasks`), then the tally `<n> of 8 adapters ready`.

Each adapter check shells out to `<bin> --version` (`cursor` tries `cursor-agent` then `agent`; `shell` runs
`bash --version` and takes the first line). Pass shows the version string, failure shows the adapter's own
not-found message (quoted in §5.4).

**Exit code is 0 even when adapters are missing** — `doctor` reports, it does not gate.

See §13 for the `node` check's replacement in r-orch.

### 11.3 `init` (L0 for the created files, L1 for idempotency)

- `--name <name>` overrides the project name; the default is the **basename of the current directory**.
- `--adapter <adapter>` overrides `defaults.agent.adapter`.
- Creates exactly the files in §6.11 and prints the tree shown in §5.4.
- Writes `agents/agt_creator.yml` **directly**, not through `AgentStore`, so **no `_index.json` is created**.
- Exit 0.

### 11.4 `org` (L1 — the catalogue is L0 data)

`org list` prints the available pre-built organization templates; `org deploy <template> [--goal <goal>]`
instantiates one — creating the agent set, a team, and optionally a goal. The template catalogue
(`src/domain/org-shop.ts`) and the agent-shop catalogue (`src/domain/agent-shop.ts`, **15 templates**) are
**data that must be reproduced verbatim**, including every long role description, because those strings are
injected into prompts. See [docs/parity/infra.md §5–§7](parity/infra.md).

**Agent-shop keys (15, array order — L0):** `backend-dev`, `frontend-dev`, `qa-engineer`, `code-reviewer`,
`architect`, `devops-engineer`, `bug-hunter`, `tech-writer`, `marketer`, `content-creator`, `growth-hacker`,
`security-auditor`, `performance-engineer`, `data-engineer`, `fullstack-dev`.
`code-reviewer`, `architect` and `security-auditor` are `tier: capable` + `approval_policy: suggest`; the
other twelve are `balanced` + `auto`.

**Org-shop keys (10 — L0):** `startup-mvp`, `pr-review-corp`, `migration-squad`, `security-dept`,
`test-factory`, `content-agency`, `data-lab`, `sales-machine`, `bugfix-dept`, `docs-team`. All have
`lead_index: 0`.

**Default agents:** exactly one — `agt_creator` / "Agent Creator" (§6.11).

Agent-shop templates reference a semantic `tier` (`capable|balanced|fast`) rather than a model string;
`templateToAgentInput(template, adapter)` resolves it via §8.13 and **filters out MCP skills (names containing
`:`) for every adapter except `claude`**. Passing `adapter: "shell"` throws
`Agent Shop templates require an AI adapter; create shell agents with --command`.

### 11.5 `update` / update check (L1)

- `update` checks npm for a newer version and installs it; `--check` only reports.
- A **background, cache-only** check runs after every command **except** `tui`, `update` and `serve`. It is
  stale-while-revalidate: it never blocks the CLI, and prints a notification after the command completes. When
  an update is available it also kicks off a detached background install.
- Cache location and TTL: see `src/cli/update-check.ts`.

#### What the TypeScript binary actually does

Verified in source, because this decides the r-orch behaviour:

- `src/cli/update-check.ts:15-16,125` — `CACHE_DIR = path.join(os.homedir(), '.orchestry')`, holding
  `update-check.json` and `update-installed.json`. **No environment override.**
- `:61` — spawns `npm view @oxgeneral/orch version --json`, 5 s timeout.
- `:146` — spawns `npm` to install.
- `src/bin/cli.ts:210-231` — runs after every command except `tui`/`update`/`serve`; prints the notification
  to **stderr**; calls `backgroundInstall` when an update exists.

#### r-orch behaviour — **no network, no self-update**

| Aspect | Decision |
|---|---|
| `orch update` / `orch update --check` | Command, flag and **exit 0** preserved. Prints the current version and **how to install** an update. |
| Network access | **None.** No registry query, no release-channel query. |
| Self-replacement | **None.** The binary never downloads or overwrites itself. |
| Post-command background check | **Removed entirely.** |
| `update-check.json` / `update-installed.json` | **Never read or written** (§6.1). |

**This is a deliberate scope cut on security grounds, recorded in §13 as OUT OF SCOPE — not as
"reimplemented".** The reasoning: in the original, `npm install -g` replaces a package directory managed by a
package manager the user installed on purpose. The same behaviour in a Rust port means **a CLI silently
downloading and overwriting its own executable as a side effect of `orch task list`**. That is a new
supply-chain and integrity surface — there is no channel, no signing story, and no atomic-replace story — it
requires network access from a command that otherwise needs none, and it is **unverifiable by the harness by
construction**, since the TS side queries npm and r-orch would query nothing. It buys neither external
compatibility nor test determinism, which are the only two things the architecture claims to optimize for.

A real release channel, with signing and atomic replacement specified, is a **feature** to be designed on its
own merits later. It is not parity.

---

## 12. TUI feature checklist

The TUI is reimplemented with `ratatui` + `crossterm`. Grading is **not** uniform, and treating it as
uniformly L2 was a real defect in an earlier revision of this document:

| Surface | Level | Why |
|---|---|---|
| **Wizard → domain-entity field mapping** | **L0** | A wizard writes `Task`/`Agent`/`Goal`/`Team` records. One with an inverted skip predicate or a wrong default **corrupts L0 data through a UI surface**. "No wire contract" is not "no contract". |
| Keybindings, command-bar verbs, wizard step sequencing, skip predicates, validation messages, filter predicates | **L1** | Functional behaviour, not presentation. |
| Layout, colour, glyph choice, pixel arrangement | **L2** | Presentation. |

The TUI is **~10k of ~26k lines and is the default command** (§2.3: bare `orch` launches it), so it is most
users' entire experience of the product. Gating it on a human reading a checklist is therefore not acceptable:
a failure "visible immediately" is visible to a *user*, which is the reason to catch it first.
[docs/parity/tui.md](parity/tui.md) must be **enumerated into counted `TestBackend` cases** — one per wizard
field, one per validation message, one per command-bar verb, one per filter predicate, plus key dispatch per
view including the overloaded `a`/`g`/`s`/`c`/`x`/`f` — with a counted total tracked the way V1 tracks the
generated command-tree test. See **V33**.

This section is the acceptance checklist; the exhaustive inventory is in the appendix.

### 12.1 Views

Four tabs, in `Tab`-cycle order: `goals` (`G`), `tasks` (`T`, **default at startup**), `agents` (`A`),
`logs` (`L`). The logs tab's **rendered label is `ACTIONS`**, not `LOGS`.

- [ ] Goals list + goal detail panel, with linked-task display
- [ ] Tasks list + task detail panel (status, priority, agent, attempts, proof, review results, error hint)
- [ ] Agents list + agent detail panel (adapter, model, status, stats, `last_error` with hint)
- [ ] Logs/activity feed with per-agent and per-event-type filtering
- [ ] Tasks tab badge `" (N)"`, shown **only when tasks are hidden** by the viewport
- [ ] `Tab` / `→` cycle forward with wrap; `←` cycles backward; **all view switching is blocked while a detail
      panel is open**

### 12.2 Keybindings

The handler is **one ordered `if` chain and the order is load-bearing**. Reproduce this dispatch order:

1. Help overlay open → close. If the key was `?`, `Esc` or F1, swallow it; **any other key closes the overlay
   and then also performs its normal action**.
2. Agent picker open → swallow everything. (The *type* picker does **not** block — a quirk; `q` still quits
   through it. Preserve or fix deliberately — see §14.8.)
3. `Ctrl+S` in the agent wizard → open the Agent Shop wizard — **refused when the default adapter is
   `shell`**, since shop templates require an AI adapter (§11.4).

   **There are two different refusal strings, both `yellow` toasts. They are L0 and must not be unified:**

   | Call site | Reached by | Exact string |
   |---|---|---|
   | `launchShopWizard` — `App.tsx:1012` | **`Ctrl+S`** and **`/agent shop`** | `Agent Shop requires an AI provider. Press N to create a Shell agent.` |
   | `handleSuggestionSelected` — `App.tsx:1321` | selecting a template from the inline suggestion list on the agent wizard's **name** step | `Agent Shop requires an AI provider. Choose Shell and enter a command instead.` |

   They share a first sentence and differ in the advice, which fits each entry point: from `Ctrl+S` you are
   not yet in a wizard, so you are told to press `N`; from the suggestion list you already are, so you are
   told what to pick. *(An earlier revision of this document claimed both paths printed the same string. They
   do not.)*
4. Any input mode active → text-input branch.
5. Normal-mode chain, in source order.

Normal mode highlights (full table in [docs/parity/tui.md §2](parity/tui.md)):

- [ ] `q`/`Q` quit — **tested first, so `q` can never be typed in normal mode**
- [ ] `Esc` closes the detail panel, else returns the log view to tail; **`Esc` never quits**
- [ ] `+`/`=` and `-` resize the split by 3 rows; interact with the maximized state
- [ ] `j`/`k`/`↑`/`↓` navigate; `Enter` opens detail; `?` and **F1** open help; `/` opens the command bar
- [ ] `M` toggles maximize; `n`/`e`/`d`/`z`/`r`/`u` = new/edit/soft-delete/undo/run/autonomous
- [ ] `N` opens the task wizard; the unreachable `'new_task'` input mode is **not** ported (§14.8)

> **`a`, `g`, `s`, `c`, `x` and `f` are overloaded per view** — e.g. `a` approves when a review task is
> selected, otherwise jumps to the agents tab, otherwise opens the logs agent picker; `s` is "show all" on
> tasks but "force-stop" on agents; `c` cancels a task but *achieves* a goal. These collisions must be
> resolved from the dispatch chain in [docs/parity/tui.md §2.2](parity/tui.md) **explicitly**, not guessed.
> Getting one wrong means a key destroys the wrong entity.

**Layout constants** (L2, but they make the split behave): `fixedRows = 9`; the list is capped at **50 %** of
content height; `maxSplitOffset = contentH - 7`; resize steps are **3** rows. `Ctrl+Enter` vs `Enter` requires
Kitty CSI-u support; degrade cleanly when absent.

**Minimum terminal size guard — ADD (deliberate L2 improvement, decided by the team lead).** The original has
**no responsive breakpoints and no minimum-size guard**: below roughly `fixedRows = 9` plus a usable split the
layout arithmetic produces negative heights. r-orch renders a single centred "terminal too small — need at
least N×M" message instead of attempting the layout. This is an **addition**, not parity; it is recorded here
so it is not mistaken for a port and not silently removed.

### 12.3 Overlays and modes

- [ ] Command bar with ghost completion, suggestion panel and history
- [ ] Help overlay
- [ ] Logs agent-filter picker; logs event-type-filter picker
- [ ] Onboarding box (first run)
- [ ] Toast banner, optional terminal bell, tab flash
- [ ] Form wizard
- [ ] Confirmation prompts
- [ ] Undo banner (soft delete)
- [ ] `LogsSearchBar` is **not** ported — it is implemented and unit-tested in the original but never mounted
      (§13)

### 12.4 Command bar verbs (L1 — the verb and subcommand set is contract)

Registry order determines suggestion order:

| Verb | Subcommands |
|---|---|
| `task` | `add, list, show, cancel, retry, assign, approve, reject, delete` |
| `agent` | `add, list, disable, enable, delete, autonomous, shop` |
| `team` | `create, list, join, leave, disband, set-lead` |
| `goal` | `add, list, show, status, delete` |
| `run` | *(arg `[id]`)* |
| `run-all` | — |
| `watch` | — |
| `pause` | — |
| `status` | — |
| `config` | `palette, activity-filter, max-concurrent, notifications-toast, notifications-bell` |
| `help` | — |
| `quit` | — |

- [ ] `resolveCompletion` returns only the ghost **suffix**; input must start with `/`; an exact match yields
      `null`. Pinned vectors: `'task'`→`null`, `'/'`→`null`, `'/ta'`→`'sk'`, `'/task'`→`null`,
      `'/task c'`→`'ancel'`, `'/config pal'`→`'ette'`, `'/xyz'`→`null`
- [ ] **Legacy bare shortcuts** (no leading `/`) are still accepted: `cancel`, `retry`, `assign`, `disable`,
      `enable`
- [ ] Subcommand aliases: `agent delete|remove`, `agent autonomous|auto`, `team create|add`,
      `goal add|create`
- [ ] Suggestions are **category-grouped for a bare `/`** (3 separators: MANAGEMENT / MONITORING / SETTINGS)
      and **flat for any prefix**
- [ ] History **dedupes consecutive** entries and caps at **100**

### 12.5 Wizards

**Eleven** wizards, all driven by one `FormWizard` engine with `text` / `textarea` / `select` / `multiselect`
step types. Validation is **300 ms debounced** for text and textarea, **synchronous** for select.

- [ ] **NEW AGENT** — **10 steps**, 6 of them conditional: `name, adapter, command, model, effort, role,
      role_custom, skills, approval_policy, team`. Skips: `command` only for `shell`;
      `model`/`role`/`skills`/`team` skipped for `shell`; `effort` only for `{claude, pi, grok}`;
      `approval_policy` **always** skipped; `team` skipped when there are fewer than 2 options. `effort`
      **must sit immediately after `model`**. Shell collapses the wizard to `name`/`adapter`/`command` and
      forces `approval_policy = "auto"`, clearing model/effort/role/skills/team.
- [ ] **EDIT AGENT** — **8 steps**, `getEditAgentWizardSteps` at
      `src/tui/wizardConfigs.ts:463-547`: `name` (:473), `adapter` (:485), `command` (:492), `model` (:503),
      `effort` (:515), `role` (:523), `role_custom` (:531), `team` (:539).
      **It has no `skills` and no `approval_policy` step**, which is what makes it 8 rather than NEW AGENT's
      10 — so **neither field is editable through the TUI once an agent exists**, leaving `agent edit` via
      `$EDITOR` (§2.4) as the only way to change an agent's skills.
      *(An earlier revision said 9; [docs/parity/tui.md §5.2](parity/tui.md) was correct.)*
- [ ] **AGENT SHOP** (1 step, 15 templates) · **NEW AGENT — {template}** (prefilled) ·
      **NEW TASK** (4) · **EDIT TASK** (4) · **NEW TEAM** (4) · **New Goal** (3) · **Edit Goal** (3) ·
      **SETTINGS** (5 single-step variants, one per config setting)
- [ ] **Display-width-aware sliding viewport** on `text`/`textarea` fields (`TextInput.tsx`): fill `before`
      from the right while `width + 1` fits (one column reserved for the cursor block), then fill `after`
      with the remainder, measuring **per grapheme**. Required for parity — without it a cursor in a long
      value drifts out of view. r-orch measures with `unicode-width` rather than the TS range table, so the
      algorithm matches while the widths differ (§12.8.1).
- [ ] **Clipboard paste** on `text`/`textarea` steps. The TS guard is
      `(key.ctrl || key.meta) && (input === 'v' || input === 'i')`, and in Ink `key.meta` means
      **Alt/Option**, so the reachable bindings are **`Ctrl+V`, `Ctrl+I`, `Alt`/`Option`+`V`,
      `Alt`/`Option`+`I`**. Only active when a paste handler is wired and the step is `text` or `textarea`.

      **Cmd (U+2318) is not one of them, and cannot be.** Verified in Ink 6.8.0 `parse-keypress.js`: on the
      legacy path `key.meta` is set from an **ESC-prefixed** sequence (`key.meta = s.charAt(0) === '\x1b'`,
      and `metaKeyCodeRe = /^(?:\x1b)([a-zA-Z0-9])$/`), which is exactly how terminals encode
      Option-as-Meta; on the xterm path it is `!!(modifier & 10)`, i.e. the Alt/Meta bits. Under the Kitty
      protocol `meta` is a **distinct bit from `super`** — `kittyModifiers = { shift: 1, alt: 2, ctrl: 4,
      super: 8, hyper: 16, meta: 32 }` — and Cmd arrives as `super`, which Ink never maps to `key.meta`. On
      other terminals Cmd combinations are consumed by the emulator and never reach the application.

      Bind `Ctrl` and `Alt`/`Option`; do **not** add a Cmd binding expecting it to fire.

      **In crossterm terms**, since that is the vocabulary the port is written in: match
      **`KeyModifiers::ALT`**, plus **`KeyModifiers::META`** where the terminal reports it — and
      **never `KeyModifiers::SUPER`**. `SUPER` is where Cmd arrives when it arrives at all, so treating it
      as meta re-creates exactly the binding that cannot fire.
- [ ] Validation messages verbatim: `Title is required`, `Priority must be 1-4`, `Name is required`,
      `Agent with this name already exists`, `Team with this name already exists`,
      `Command is required for Shell`, `Fix the error above`

Full step/field tables in [docs/parity/tui.md §5.2](parity/tui.md).

### 12.6 Live-update mechanics (L1 — these are OOM defences, not cosmetics)

- [ ] Batched message queue with an **80 ms** flush interval
- [ ] **150 ms** refresh debounce
- [ ] **120 ms** animation tick; **10-frame braille** spinner
- [ ] Detail strings capped at **2 KB**
- [ ] RunId-keyed maps capped at **500** entries (LRU)
- [ ] Defensive `formatAgentOutput` that accepts both the canonical and every legacy adapter `data` shape

### 12.7 Theme

Five palettes (`amber`, `ocean`, `forest`, `violet`, `light`) persisted in `~/.orchestry/global.yml`. The
**semantic roles** must exist (brand/amber, success/green, error/red, info/blue, warning/yellow, dim, ghost,
solid text, selection); exact hex values are L2.

### 12.8 Deliberate divergences in r-orch's TUI

WP-6 is complete: **100 % of this section’s checklist**, with the case count and its per-area breakdown in
`crates/orch-tui/TEST-COVERAGE.md` (**authoritative** — deliberately not restated here; see V33). Nine
divergences from the Ink original
were taken deliberately. **All have comparison effect `none`** — the TUI is outside the harness (§15.1) — so
each is justified on its own merits rather than by the gate.

| # | Divergence | Level | Rationale | Recorded |
|---|---|---|---|---|
| a | Both logs pickers block global keys | L1 | The agent picker blocks, the type picker does not; `q` quits out of the latter. Unintended asymmetry. | §14.8 |
| b | Minimum-terminal-size guard added | L2 | The original has no breakpoints and no guard; below ~9 rows the layout arithmetic goes negative. **Addition, not parity.** | §12 layout note |
| c | Wizard `Esc` = back one step; `Esc` on step 0 cancels | **L1 (keymap)** | This is **FormWizard's own intended behaviour**, defeated in Ink by a second handler. | §14.18 |
| d | Cancelling a wizard clears pending attachments | L1 | The original leaks them on the `Esc`-above-step-0 path. | §14.19 |
| e | The `W` hint is dropped | L2 | Advertised by `onboarding-config.ts`, **no handler exists**. | §13 |
| f | Toast and undo rows come from the elastic bottom panel | **L2** | Pure layout. The rows exist and carry the same text; where the space is taken from differs. | here |
| g | `log_selected_index` consistently indexes the **filtered** list | L1 | The original mixes filtered and unfiltered indexing, so the highlighted row and the detail shown disagree whenever a filter is active. | §14.20 |
| h | Pre-dispatch failures reported synchronously as `Err` | L1 | The TUI renders no `orchestrator:error` case, so a stream-only dispatch error is **invisible**. | §12.10 |
| i | Grapheme display width from `unicode-width`, not the TS range table | **L2** | The TS table under-measures several emoji classes; r-orch matches what terminals actually render. | §12.8.1 |

Items (b), (f) and (i) are recorded only here, because they are additions, layout or presentation rather
than behaviours of the original. The others have §13, §14 or §12.10 entries.

#### 12.8.1 Display width (L2)

`text-cursor.ts:31` classifies a grapheme by **its first codepoint alone**, against a hand-written range
table whose emoji entry is only `0x1F300–0x1FAFF`. Grapheme *clustering* is correct (it uses
`Intl.Segmenter`); only the width classification is wrong. Measured consequences:

| Input | First codepoint | TS width | Terminal / `unicode-width` |
|---|---|---|---|
| `⌚` `⏰` `✅` — BMP pictographs | U+231A, U+23F0, U+2705 — all below the `0x2E80` CJK floor | **1** | **2** |
| `❤️` — VS16 sequence | U+2764 (the VS16 is in the same cluster but never inspected) | **1** | **2** |
| `🇺🇸` — regional-indicator pair | U+1F1FA, below the `0x1F300` emoji floor | **1** | **2** |
| `😀` | U+1F600, inside the range | 2 | 2 |
| CJK, Hangul, fullwidth forms | — | 2 | 2 |

r-orch uses **`unicode-width`**, so these measure 2 and the cursor, truncation and padding land where the
terminal actually draws. **L2 and effect `none`** — it is presentation, and the TUI is outside the harness —
but it is a visible improvement rather than a cosmetic shuffle: under the TS table an `⌚` in a task title
drifts every column to its right by one.

All on-screen measurement in r-orch goes through **ratatui**; the standalone width helpers are being
annotated or removed by WP-6 so there is one source of truth rather than two that can disagree.

**Related, same class:** the command bar truncates by **`char` count** in r-orch where the TS slices by
**UTF-16 code units** (`commandBar.ts`). Each astral character is 2 code units but 1 `char`, so a command
containing one drifts by a column between the two implementations. Also **L2**.

### 12.9 The update facade is intentionally inert

The TS header renders a **version chip** when an update is available, driven by the post-command update check.
Since self-update and all network access are **out of scope on security grounds** (§11.5, §13), r-orch's TUI
facade keeps `poll_update`, `background_install` and `installed_version` **at their defaults** — no polling, no
install, no detected version.

**Consequence: the header version chip never appears in r-orch.** That is **intended, not a missing feature.**
The fields exist so the facade matches the shape of the TS header component and so a future release channel
could populate them without reshaping the view; nothing currently does. Do not "finish" them, and do not file
the absent chip as a parity gap.

### 12.10 Pre-dispatch failures must surface synchronously (L1)

The TUI backend facade (`crates/orch-tui/BACKEND.md`) is fire-and-request: `run_task`, `run_all` and
`start_watch` ask the orchestrator to dispatch and return immediately, with outcomes arriving via
`poll_events`. **That is correct for outcomes and wrong for pre-dispatch failures**, because of a gap in the
original:

**Neither the TS TUI nor r-orch renders `orchestrator:error`.** The TS event switch in `App.tsx` handles
`agent:completed`, `agent:error`, `agent:file_changed`, `agent:output`, `agent:started`,
`orchestrator:stall_detected`, `orchestrator:tick`, `run:retry`, `task:assigned`, `task:cascade_failed`,
`task:created` and `task:status_changed` — and **no `orchestrator:error` case** (nor
`orchestrator:shutdown`).

Dispatch failures are emitted as `orchestrator:error { context: "dispatch task <id>" }` and deliberately do
**not** stop other dispatches (§7.5). So in the TS TUI, pressing `r` on a task whose dispatch fails — a
`WorkspaceError` because the project is not a git repo, `NoAgentsError`, an unknown adapter — produces
**no feedback whatsoever**. The event is emitted, nothing renders it, the task silently stays `todo`.

**Requirement:** `run_task`, `run_all` and `start_watch` must return **`Err(String)` synchronously** for any
failure detectable before the agent process is spawned — lock conflict, no agents configured, unknown adapter,
workspace preparation failure, an invalid task id or a non-dispatchable status. The TUI renders
`Failed: {err}`, so the message *is* the contract: use the same text the CLI would print for that failure
(§3.1).

Post-dispatch outcomes stay asynchronous and arrive through `poll_events` — that part of the facade is right
and must not become blocking. The dividing line is **"did the child process start?"**: before it, `Err`;
after it, events.

**One exception on the other side of that line: `stop_watch` awaits the wind-down.** It is the one facade
method that legitimately blocks, because the wind-down must complete every step and release the lock before
the call returns (§14.15), and an in-flight tick is allowed to finish rather than be aborted (§14.24).
**This matches the TypeScript and is therefore not a divergence** — it is deliberately absent from §12.8's
table, which is for divergences only. Recorded here because "the facade never blocks" would otherwise read
as absolute and invite someone to make `stop_watch` fire-and-forget, which would let the TUI exit while the
lock was still held.

This is a **fix** for a usability gap rather than a parity divergence: the original emits the error, so the
information exists; it simply never reaches the screen.

---

## 13. Out of parity scope

| Item | Reason |
|---|---|
| **npm `postinstall` script** (`scripts/postinstall.cjs`) | Node packaging artifact. r-orch ships a binary; installation is cargo/release-archive. |
| **Landing page** (`landing/index.html`) | Marketing asset, not product behaviour. |
| **`scripts/release.sh`, `.github/workflows/publish.yml`** | Node/npm release plumbing. Replaced by a Rust release pipeline. |
| **JS programmatic API** (`src/index.ts`) | Replaced by the Rust library crates — see §13.1. |
| **`tsup`/`tsc`/`vitest` configuration** | Toolchain. |
| **`components/Footer.tsx`, `components/TabBar.tsx` (the component), `components/LogsSearchBar.tsx`, the four unused `*StatsRibbon` components, `Header`'s `sparklineData` prop** | **Dead code** in the original — implemented and in some cases unit-tested, but never mounted. Not ported. Their tests are not ported either. |
| **`InputMode` value `'new_task'`** | Unreachable — nothing sets it. Not ported. |
| **The `W` key hint** | `onboarding-config.ts` advertises `W start orchestrator` but **no handler exists**. The hint is dropped (watch mode stays reachable via `/watch`). |
| **`team:task_claimed` emission** | Declared in the event union, never emitted. The variant is kept in the type for forward compatibility; nothing emits it. |
| **A TUI path that *sets* `task.goalId`** | The goal↔task linking UI is **read-only** — it displays linked tasks and goal badges, but no TUI action assigns a goal to a task. Linking happens only via `task add --goal-id`. Reproduce the display; do not invent the editor. |
| **`TeamConfig.auto_claim` / `max_concurrent_tasks` behaviour** | Stored and surfaced by the CLI but never read by the orchestrator. Data model and CLI are preserved; the (absent) behaviour is not invented. |
| **`.orchestry/logs/`** | A path exists in `Paths`; nothing writes it. Kept in the path API, unused. |
| **npm self-update, and the post-command background update check** | **OUT OF SCOPE — security.** Not "reimplemented". A Rust CLI overwriting its own executable as a side effect of an unrelated command is a supply-chain and integrity surface with no channel, no signing story and no atomic-replace story, and it is unverifiable by the harness. `orch update [--check]` keeps its name, flag and exit 0, and prints the current version plus install instructions. No network access. See §11.5. |
| **The TUI header's update/version chip** | Follows from the row above. The TUI facade's `poll_update`, `background_install` and `installed_version` stay **at their defaults**, so the chip **never renders**. Intended, not a gap — see §12.9. |
| **`doctor`'s `node` check** | Replaced by a check of the r-orch build/toolchain identity. The row keeps its **position and format**; only its label and value change. Because that is a divergence in L0 human output, the harness **masks the toolchain row** (§1.4) rather than pretending it matches. |
| **`readJsonl`'s 50 MB fallback stderr warning wording** | L1 behaviour (tail-read) is kept; the exact warning string is not contract. |

### 13.1 Rust library surface replacing the JS API

`src/index.ts` exports domain types, the state machine, errors, model tiers, the agent shop, `EventBus`,
`TaskService`/`AgentService`/`RunService`/`Orchestrator`, the adapter interface and registry, the skill
loader, the clipboard helpers, and the container builders. The equivalent Rust surface:

| TS export group | Rust equivalent |
|---|---|
| Domain types, `TaskStatus`, `canTransition`, `isTerminal`, `isDispatchable`, `isBlocked`, `resolveFailureStatus` | `orch-domain` — public structs, enums and free functions |
| `OrchestryError`, `AdapterErrorKind`, `ERROR_HINTS`, `classifyAdapterError` | `orch_domain::error` |
| `MODEL_TIER_MAP`, `resolveModel`, `defaultModelForAdapter`, `SUPPORTED_ADAPTERS` | `orch_domain::model_tiers` |
| `AGENT_SHOP_TEMPLATES`, `getShopTemplateByKey`, `templateToAgentInput`, `isMcpSkill` | `orch_domain::shop`, `orch_engine::agent_factory` |
| `EventBus`, `OrchestratorEvent` | `orch_engine::events` — a broadcast channel plus the event enum |
| `TaskService`, `AgentService`, `RunService`, `Orchestrator` | `orch-engine` — same names |
| `IAgentAdapter`, `AgentEvent`, `ExecuteParams`, `AdapterRegistry` | `orch_core::adapter` traits (in `orch-core` so `orch-engine` and `orch-infra` can both see them) |
| `ISkillLoader`, `SkillLoader` | `orch_infra::skills` |
| clipboard helpers | `orch_infra::clipboard` |
| `buildLightContainer`, `buildFullContainer` | `orch_engine::container::{build_light, build_full}` |

Public-API stability is **not** a parity requirement — no external Rust consumer exists yet. The requirement is
that the *same capabilities* are reachable from a library consumer without going through the CLI, and that
`orch-engine` has **zero dependency** on `orch-cli` or `orch-tui`.

---

## 14. Known bugs, divergences and port defects

Default policy: **fix**, and document. Exceptions are called out.

Most entries are bugs in the **reference implementation**. A few (§14.25, §14.26) are **port defects** —
places where r-orch had drifted from the original and was corrected *toward* it. They live here because the
effect-on-comparison accounting in §14.0 applies identically, and because a reader asking "why does r-orch do
X" deserves one list, not two. Each entry's heading states its kind.

### 14.0 Every entry must state its effect on the differential comparison

The differential harness compares r-orch against the **unfixed** oracle. A `FIX` therefore does not merely
change our behaviour — it changes whether the harness can still verify that behaviour at all. Asserting a
fix's benefit *in terms of the harness* is incoherent unless this is traced.

**Every entry below carries a required `Effect on differential comparison` field**, with exactly one of:

| Value | Meaning |
|---|---|
| `none` | Observable output is unchanged, or the surface is outside the harness. The row stays verified. |
| `scenario exclusion required (<named>)` | The harness cannot compare this; the named scenarios must avoid it. The row becomes **unverified** and that is now visible. |
| `oracle patched` | We patch the vendored oracle so the surface is comparable at all. Requires a patch file under `parity/patches/`, applied by `parity/setup.sh`, and an explicit note of what was patched and why. **Reserved for cases where the oracle is too broken to compare against** — not for making it agree with us. In use by §14.21. |

**An entry marked `FIX` without this field is incomplete.** Summary:

| Entry | Decision | Effect |
|---|---|---|
| §14.1 `claimed: {}` | **FIX** (user decision) | `scenario exclusion required (named)`: `state.claimed` masked in `status --json`, replaced by an equality check against `state.json` |
| §14.2 `goalId` camelCase | REPLICATE | `none` |
| §14.3 bad hint text | REPLICATE | `none` |
| §14.4 `findBestAgent` ties | **REPLICATE** *(reversed)* | `none` |
| §14.5 npm review commands | REPLICATE | `none` |
| §14.6 unknown Liquid filters | DIVERGENCE | `scenario exclusion required (custom-template-*)` |
| §14.7 `1 entries` | REPLICATE | `none` |
| §14.8 TUI type picker | FIX | `none` — TUI is outside the harness (§15) |
| §14.9 suite flakiness | n/a | `none` — not a behaviour |
| §14.10 JSONL truncation | REPLICATE | `none` |
| §14.11 merge output guard | REPLICATE | `none` |
| §14.12 grok/antigravity stderr | **FIX (drain-and-discard)** | `none` |
| §14.13 pi line reader | REPLICATE | `none` |
| §14.14 msg surrogate truncation | DIVERGENCE (lead ruling) | `scenario exclusion required (cmd-msg-truncation, mid-pair case only)` |
| §14.15 `stop()` wedges the lock | FIX | `none` |
| §14.16 DiskObserver straddling line | FIX | `none` |
| §14.17 corrupt lock wedges, `PID: undefined` | REPLICATE | `none` |
| §14.18 wizard `Esc` discards the wizard | FIX | `none` — TUI |
| §14.19 wizard cancel leaks attachments | FIX | `none` — TUI |
| §14.20 `logSelectedIndex` filtered/unfiltered mix | FIX | `none` — TUI |
| §14.21 worktree-mode `run`/`serve --once` no-op | **FIX** | **`oracle patched`** (`parity/patches/0001-…`) |
| §14.22 `kill(0)` hits our own process group | **FIX** | `none` |
| §14.23 `run <id>` awaits the collector | equivalent behaviour | `none` |
| §14.24 `stop()` lets an in-flight tick finish | REPLICATE | `none` |
| §14.25 index rebuild swallowed a parse error | PORT DEFECT → TS (convergence) | `none` |
| §14.26 signal handlers after the first tick | PORT DEFECT → TS | `none` |
| §14.27 orphaned `running` runs never swept | FIX | `none` |
| §14.28 `start_watch` leaks the lock on error | FIX | `none` |
| §14.29 lock PID `0` / `> i32::MAX` treated dead | FIX (differs from TS for `0`) | `none` expected |
| §14.30 `.`/`..` accepted as an id | FIX (additive) | `none` |
| §14.31 four YAML read guards reject what js-yaml accepts | DIVERGENCE | `none` in corpus |
| §14.32 run-events > 50 MiB tail-read (last 1 MiB) | FIX | `none` — unreachable in corpus |
| §14.33 token totals can overflow | FIX | `none` |
| §14.34 tracing to stderr under `ORCHESTRY_DEBUG` | FIX | `none` |
| §14.35 `config.yml` / `agents/*.yml` written 0600 | FIX | `none` — **modes are not compared**, hence the row |

Note that §14.4 and §14.12 were *both* originally specified as fixes that would have required scenario
exclusions. Tracing this column is what caused both to be reworked into forms that need none — which is the
point of having it.

### 14.1 `status --json` emits `"claimed": {}` — **FIX (user decision, 2026-10-13)**

`OrchestratorState.claimed` is a JS `Set`. `status --json` serializes the in-memory state object directly, and
`JSON.stringify(new Set([...]))` is `{}` for any contents. So `state.claimed` in TS `status --json` is **always**
an empty object, while `state.json` on disk correctly holds an array.

**Decision: fix.** r-orch emits `state.claimed` as the array of claimed task ids, identical to `state.json`.
**Effect on differential comparison: `scenario exclusion required (named)`** — the harness masks
`state.claimed` in every `status --json` comparison and instead asserts r-orch's array equals the `claimed` array
read from the resulting `.orchestry/state.json`.

History: the architect and team lead initially chose REPLICATE (a consumer might expect an object). The user
reviewed the work-in-progress report and decided to fix it. Consumers parsing `status --json` that relied on
`claimed` being an object will now see an array — called out in release notes.

### 14.2 `goalId` is camelCase among snake_case fields — **REPLICATE**

Every other `Task` field is snake_case. `goalId` is the on-disk key name and the `--json` key name, so it is
L0 and cannot be renamed.

### 14.3 `ERROR_HINTS[timeout].fix` names a non-existent config key — **REPLICATE**

It suggests `orch config set agent_timeout <ms>`; the real path is `defaults.agent.timeout_ms`. The hint text
is user-visible output asserted by tests. **Replicate**, and open a follow-up issue to fix it in both
implementations together.

### 14.4 `findBestAgent` ties are resolved by `_index.json` order — **REPLICATE**

**Effect on differential comparison: `none`.**

Equal-scoring agents are resolved by position in `agentStore.list()`, which is `_index.json` order, which is
last-write order.

> **This entry reversed an earlier decision.** It previously specified a fix — a final
> `(score desc, agent.id asc)` tiebreaker — justified on the grounds that it "makes the differential harness
> able to assert agent selection at all." That reasoning was **wrong**: the harness compares r-orch against
> the *unfixed* oracle, so making our side deterministic does not make it *agree*. The fix would have forced
> every scenario to avoid ties, leaving tie behaviour entirely unverified. Replicating instead makes it
> oracle-verifiable. See [architect-rebuttal.md O4](reviews/architect-rebuttal.md).

TS tie order **is** deterministic for a given operation sequence, and replicating it is therefore achievable.
It depends on three properties, all of which are now requirements:

1. **`save()` must replicate index semantics exactly**: remove the entity by id, then **push it to the end**
   of the array (§6.10). Index order is consequently last-write order, not creation order.
2. **The scoring sort must be stable.** ECMAScript ≥2019 mandates a stable `Array.prototype.sort`. In Rust
   this means **`slice::sort_by` is mandatory and `sort_unstable_by` is forbidden** on this path — a detail
   that is invisible until it silently changes agent selection.
3. **One hole remains.** `rebuildIndex` derives order from `fs.readdir`, i.e. filesystem enumeration order.
   Entity filenames embed random nanoids, so **two tmpdirs can enumerate the same logical set differently**.
   This is reachable only when `_index.json` is missing or corrupt **and** ≥2 entities exist.

   **Scenario rule:** do not construct a differential scenario that triggers a multi-entity index rebuild.
   Single-entity rebuilds are safe (trivially ordered), which covers the common `init` → first `agent add`
   path.

4. **A second hole, from §6.10.1:** `reconcile` resets stale agents with a **concurrent batch**
   (`orchestrator.ts:620`), which writes the agent index in nondeterministic order. So even a *fixed*
   operation sequence can yield a nondeterministic agent order if it includes a stale-agent reset. Scenarios
   asserting agent selection must avoid that too.

Both holes are recorded rather than hidden. Everything outside them is oracle-verifiable.

### 14.5 `ReviewRunner` commands are hard-coded to npm/npx — **REPLICATE**

`test_pass` → `npm test`, `typecheck` → `npx tsc --noEmit`, `lint` → `npm run lint`. These are wrong for a
non-Node project, but they are the documented meaning of the `review_criteria` values and are asserted by
tests. **Replicate.** Making them configurable is a feature, not parity.

### 14.6 `strictFilters: false` cannot be reproduced — **DOCUMENTED DIVERGENCE**

**Effect on differential comparison: `scenario exclusion required`.**

A user-supplied template in `config.prompt.*` that references an unknown filter renders in TS (the value
passes through) and **fails to parse** in Rust. Shipped templates are unaffected. Surface a clear error naming
the unknown filter; do not panic.

**This is the only entry in §14 that costs the harness a verified row.** Named exclusion: scenarios matching
`custom-template-*` must not set `config.prompt.template`, `prompt.system_template` or `prompt.user_template`
to a value containing a filter outside the stdlib. A scenario that exercises a *valid* custom template is
still compared normally — the exclusion is specifically the unknown-filter case, and it is therefore
**unverified against the oracle**.

### 14.7 `context list` prints `1 entries` — **REPLICATE**

Not pluralized. It is asserted human output. Replicate.

### 14.8 TUI: the type picker does not block global keys — **FIX**

**Effect on differential comparison: `none`** — the TUI is outside the harness (§15.1).

While the logs *agent* picker is open, all App keys are blocked (`if (showAgentPicker) return;`); while the
*type* picker is open they are not, so `q` quits out of it. **Decision: fix** — block global keys for
**both** pickers. The inconsistency is clearly unintended.

**Implemented as specified** — WP-6 confirms both pickers now block global keys.

Related, and **not ported** (see §13): the unreachable `'new_task'` input mode, the advertised-but-unhandled
`W` key, and the never-mounted `LogsSearchBar`.

### 14.9 The "1 failing baseline test" is actually suite flakiness — **DO NOT REPLICATE**

Measured, not assumed. Four runs of the reference suite at `c066dc0`:

| Run | Config | Result |
|---|---|---|
| 1 | default (`maxWorkers = availableParallelism()`) | **6 failed** / 2085 passed / 2 skipped — exit 1 |
| 2 | default, re-run | **5 failed** / 2086 passed / 2 skipped — exit 1, **a different failure set** |
| 3 | `--maxWorkers=1`, only the 4 failing files | **210 / 210 passed** |
| 4 | `CI=1` (⇒ `maxWorkers=2`), full suite | **0 failed** / 2085 passed / 8 skipped — **exit 0** |

There is **no deterministic failing test**. Every failure is in an Ink `.tsx` test that writes to stdin, waits
a *fixed* delay, then asserts on `lastFrame()`; under thread contention the React re-render misses the window
and the assertion reads the previous frame. `vitest.config.ts` caps `maxWorkers` at 2 when `CI` is set, which
is the only reason CI is green.

**Consequences, all mandatory:**

1. **The canonical baseline is `CI=1 npx vitest run` → 2085 passed, 8 skipped, exit 0.** Every comparison
   against the TS suite uses this invocation. The "2090 / 1 / 2" figure in the project brief is superseded.
2. **The differential harness must invoke the TS oracle with `CI=1`** (or an explicit `--maxWorkers=2`), or it
   will inherit the flakiness as spurious parity failures.
3. **Do not port the sleep-and-assert pattern.** Against `ratatui`'s `TestBackend`, drive frames
   synchronously: push the key event, call `terminal.draw()`, assert on the buffer. ~20 TUI files and ~500
   cases depend on timing that Rust can make deterministic. This is the single largest quality win available
   in the port.
4. The **2 unconditionally skipped tests** are both in `help-overlay.test.tsx` and are skipped only because
   `ink-testing-library` cannot deliver an F1 escape sequence (`\x1bOP`) through its fake stdin. In ratatui
   they are trivial (`KeyCode::F(1)`) — **port them as passing tests.** 6 further tests skip under `CI=1`
   (`process-endurance.test.ts`, which spawns real `sleep`/`bash`); in Rust these should run everywhere.

### 14.10 `appendJsonl` truncation can still exceed `PIPE_BUF`

The truncation budget is computed in characters but checked in bytes, so a multi-byte `data` payload can still
produce an over-`PIPE_BUF` line (the source acknowledges this: "acceptable on local filesystems"). **Replicate**
the arithmetic exactly — changing it changes where the `…` lands, which is observable in the JSONL.

### 14.11 `MergeStrategy` output guard overshoots 2000 chars

The length check happens before appending, so the buffer can exceed 2000 by one chunk. `conflictInfo` is
always sliced to 1000, so this is unobservable. **Replicate** the simple form.

### 14.12 `grok` and `antigravity` never read stderr — **FIX**

Every other adapter either streams stderr (shell) or attaches the bounded tail capture via
`createStreamingEvents` (claude, codex, cursor, opencode) or its own reader (pi). `grok` and `antigravity`
spawn with `stderr: 'pipe'` and **never attach a consumer**. A child that writes more than one pipe buffer
(~64 KB on Linux) to stderr **blocks forever** on the write, and ORCH sees a hang that only stall detection
eventually clears — 10 minutes by default.

**Decision: fix, by draining and discarding.** Attach a consumer that reads stderr to EOF and **throws the
bytes away**. Do **not** attach `createStderrTailCapture`, and do **not** append a tail to these two adapters'
error messages.

**Effect on differential comparison: `none`** — and that is the whole reason for this form of the fix.

> **An earlier version of this entry claimed the fix "cannot change any behaviour that currently works" and
> then, one sentence later, that "the tail then also enriches their error messages." Both cannot be true.**
> Attaching the *tail capture* is **not** observationally neutral: `utils.ts:152-158` appends the tail to the
> error message and **then** classifies that string, so it would change L0 message text (§3.1), the error kind
> from `classifyAdapterError` (§8.8, V13 — the classifier pattern-matches a string that would now contain the
> child's stderr, so a `rate limit` or `ENOENT` substring in stderr flips the classification), **and** the
> persisted `agent.last_error` in `agents/<id>.yml` (§6.1). Draining without capturing fixes the deadlock at
> zero observable cost. See [architect-rebuttal.md O9](reviews/architect-rebuttal.md).

Draining is still mandatory: a child writing past one pipe buffer (~64 KB on Linux) **blocks forever**, and
ORCH only recovers when stall detection fires — 10 minutes at the default `stall_timeout_ms`. This is a
`blocking`-severity correctness issue in the original, not a cosmetic one.

**Structural requirement** (ARCHITECTURE §9 WP-2): stderr draining belongs in the **shared spawn path**, so an
adapter cannot forget it. Every adapter gets a test that writes 256 KB to stderr and asserts the run completes.

### 14.13 `pi` diverges from the shared line reader — **REPLICATE**

`pi` does its own framing rather than using `readLines`, and consequently:
- it **strips `\r`**, which `readLines` retains;
- it does **not** apply the 16384-character line cap.

Both are **replicated** — pi's RPC frames are newline-delimited JSON where a stray `\r` would break parsing,
and the missing cap is reachable only with a pathological frame. Documented so the asymmetry is intentional
rather than an oversight in the port.

Also: pi's `waitForSettled` tests **key presence**, not value — an `agent_settled` frame with a falsy value
still settles. Replicate.

### 14.14 `msg send` can write a message file `serde_json` cannot parse — **DOCUMENTED DIVERGENCE**

**Effect on differential comparison: `scenario exclusion required (cmd-msg-truncation, mid-pair case only)`.**
The control case one character shorter is still compared normally.

`MessageService.send` truncates with JS `.slice()`, which counts **UTF-16 code units**:
`subject.slice(0, 200)`, `body.slice(0, 4000)`. A non-BMP character is two code units, so a body of 3999 ASCII
characters followed by `😀` is cut **between the surrogates** and the stored value ends in a **lone high
surrogate**. One `msg send` reaches this; it is not a hostile-input hypothetical.

Measured oracle behaviour, which differs per output path:

| Path | Bytes | Valid UTF-8 |
|---|---|---|
| `.orchestry/messages/<id>.json` | `…aaa\ud83d` (the 6-byte escape) | yes |
| `msg list --json` on stdout | `…aaa\ud83d` (same escape) | yes |
| `msg inbox` human output | `…aaa` + `ef bf bd` (U+FFFD) | yes |

A Rust `String` cannot hold a lone surrogate at all, and **`serde_json` rejects a lone-surrogate escape
outright** (`from_str(r#"{"body":"aaa\ud83d"}"#)` → `unexpected end of hex escape`), so a naive storage layer
cannot read back a file the oracle wrote — `msg list` would fail entirely rather than diverge in one field.

**Decision (team lead ruling), in three parts:**

1. **Write side — truncate at the last whole character at or below the UTF-16 limit.** r-orch never emits a
   lone surrogate. Below the limit this is byte-identical to the oracle; at a mid-pair cut r-orch stores one
   character fewer. **The field is L2 above the limit**, which is a documented concession of an L0 field and
   is recorded as such.
2. **Read side — tolerate lone-surrogate escapes in TS-written files.** Decode them to **U+FFFD** rather than
   erroring, and **never fail a whole listing** because one message is affected. This is the half that
   matters: without it, r-orch cannot read a project the TS binary has used.
3. **Do not rewrite the body on read.** A tolerated message is **not** re-serialized unless the message is
   being re-saved for an independent reason, so merely running `msg list` does not silently rewrite the file
   and destroy the oracle's bytes.

Rejected alternatives: modelling the body as UTF-16 (byte-exact, but poisons the domain type for every
consumer) and replacing lone surrogates with U+FFFD on read *and* re-saving (silently rewrites the file).

Test: `fixtures/scenarios/cmd-msg-truncation.toml`, with the `${repeat:…}` control case. The harness now
reports a file only one side can parse as a **failure** rather than aborting the run.

### 14.15 `Orchestrator.stop()` can wedge a project by propagating mid-wind-down — **FIX**

**Effect on differential comparison: `none`** — the divergence is reachable only when the state store is
already broken, which no scenario induces.

`stop()` performs the wind-down (abort, kill, finish run, transition task, release agent) and **then**
releases the lock. Several of those steps can throw — most realistically the task transition or a state write.
Because the tick circuit breaker calls `stop()` after 5 consecutive tick failures (§7.3), and those failures
are frequently *caused* by a failing state store, the original can throw partway through and **leave the lock
file held by a process that is exiting**. The project is then wedged: every subsequent `run`/`serve` fails with
`LockConflictError` until the stale lock ages past `LOCK_STALE_MS`.

**Decision: fix.** r-orch **reports** each wind-down failure (an `orchestrator:error` event per failed step)
and **completes the wind-down regardless**, including the lock release and the signal-handler teardown. Phrased
as a requirement: `stop()` is infallible from the caller's perspective — it returns after making a best effort
at every step, never short-circuiting on the first error.

### 14.16 `DiskObserver` loses a JSONL line that straddles a tick — **FIX**

**Effect on differential comparison: `none`** — the disk observer feeds the TUI only, which is outside the
harness (§15.1).

The serve-mode disk observer polls run JSONL files from a saved byte offset. On reading a chunk whose final
line is incomplete, the original **rewinds the offset and keeps the remainder**, but the bookkeeping drops
that remainder on the next tick, so **a line split across two polls is lost**. The effect is a missing
activity row in the TUI, which is why it went unnoticed.

**Decision: fix.** Carry the partial-line remainder across ticks explicitly and prepend it to the next chunk.
TUI-only, no on-disk or wire effect.

### 14.17 A corrupt lock file wedges the project, and reports `PID: undefined` — **REPLICATE**

**Effect on differential comparison: `none`.**

Three defects compound here. All three are **replicated**, including the literal `undefined`.

1. **A garbage or empty `orchestry.lock` refuses acquisition** (exit 4) instead of being treated as stale.
   `readLockPid` returns `null`, the `existing !== null` guard short-circuits the stale-handling branch, and
   the `O_CREAT|O_EXCL` create then fails `EEXIST` (§6.7).
2. **The error message interpolates `undefined`**: `Orchestrator already running (PID: undefined)`. The live
   path throws `LockConflictError(lockResult.pid!)` unconditionally, and `pid` is `undefined` for an
   unparseable lock. **This exact string is L0** — it appears on stderr and a user may well be grepping for
   it.
3. **Nothing ever clears it.** The garbage lock is never unlinked, no command repairs it, and `orch status`
   does not clear it either (`checkLock` even reports the project *unlocked*, disagreeing with
   `acquireLock`). **The project is wedged until the file is deleted by hand** — the 60 s `LOCK_STALE_MS`
   escape hatch does not apply, because that path is only reached when the PID parses.

**Decision: replicate all three.** This is the inherited behaviour at the pinned commit, the message is
user-visible output, and "fixing" it changes an exit code and a stderr line that the harness compares. The
remedy belongs in user-facing documentation — **now written up, with the confirm-first procedure and the
reasons `orch status` and the TUI both mislead here, in
[docs/RUNBOOK.md](RUNBOOK.md#corrupt-lock-file)** — not in a silent behavioural divergence.

Related, same measurement run: **`serve:started` is logged before the lock is acquired**
(`serve.ts:84` vs the `runOnce`/`startWatch` calls at `:113`/`:131`), so a `serve` invocation that is about to
fail with exit 4 still emits one `serve:started` record first. Replicate the ordering — it is part of the
§11.1 log contract.

### 14.18 Wizard `Esc` closes the whole wizard, defeating `FormWizard`'s own logic — **FIX**

**Effect on differential comparison: `none`** — TUI, outside the harness.

`FormWizard.tsx:397-404` already implements the sensible behaviour:

```ts
if (key.escape) {
  if (currentStep === 0) { onCancel(); } else { goToPrevStep(); }
  return;
}
```

But `App.tsx:1954-1960` handles `Esc` first for **any** active input mode, and `wizard` is one of them:

```ts
if (inputMode !== 'none') {
  if (key.escape) { setInputMode('none'); inputHook.reset(''); cmdHistory.reset(); return; }
```

Ink mounts both components, so **both `useInput` handlers fire**. `setInputMode('none')` unmounts the wizard,
so `goToPrevStep()` is applied to a component that is going away and the step change is never seen.
**Observable behaviour: `Esc` anywhere in a wizard discards the whole thing**, and `FormWizard`'s
back-one-step branch is dead code reachable only in an impossible state.

**Decision: fix** — implement `FormWizard`'s stated intent: `Esc` goes back one step, and `Esc` on step 0
cancels. Note what this is *not*: it is not a new design, it is the original component's documented behaviour
with the framework quirk that masks it removed. The keymap is **L1**, so this divergence is recorded
explicitly rather than waved through as presentation. The Ink double-`useInput` mechanism is described in
[docs/parity/tui.md §10](parity/tui.md).

Consequence to keep in mind: a user who wants to abandon a half-filled wizard now presses `Esc` once per step
(or `Ctrl+C`). That is the cost of the fix and it is accepted.

### 14.19 Cancelling a wizard leaks pending attachments — **FIX**

**Effect on differential comparison: `none`** — TUI, outside the harness.

`handleWizardCancel` (`App.tsx:1313-1317`) does the right thing:

```ts
setInputMode('none'); setWizardConfig(null); setPendingAttachments([]);
```

It is wired to `FormWizard`'s `onCancel`, which only fires on `Esc` **at step 0**. On any later step
`FormWizard` takes the `goToPrevStep()` branch instead, while App's handler (§14.18) tears the wizard down
with **`setInputMode('none')` alone** — clearing neither `pendingAttachments` **nor `wizardConfig`**. So
clipboard-pasted images survive a cancelled wizard and `wizardConfig` is left stale.

**Decision: fix** — clear pending attachments (and the wizard config) on **every** cancel path, not just
`onCancel`. Strictly an improvement: a cancelled wizard should not contribute files to whatever is created
next.

### 14.20 `logSelectedIndex` mixes filtered and unfiltered indexing — **FIX**

**Effect on differential comparison: `none`** — TUI, outside the harness.

In the logs/`ACTIONS` view, `logSelectedIndex` is used against **two different arrays**:

| Site | Array | Use |
|---|---|---|
| `App.tsx:2485` | `messages` — **unfiltered** | `selectedLog`, i.e. what the detail panel renders |
| `App.tsx:3406-3407` | `filteredMessages` | the rows actually drawn |
| `App.tsx:3441` | `filteredMessages.length` | the `n/N` position counter |

So with **any** filter active, the highlighted row and the detail shown are **different events**, and the
counter describes a third frame of reference. This is a correctness bug, not a cosmetic one — selecting the
3rd visible row shows the 3rd event of the *unfiltered* stream.

**Decision: fix** — `log_selected_index` indexes the **filtered** list everywhere: selection, rendering,
detail and counter. Clamp on filter change so a narrowing filter cannot leave the index out of range.

### 14.21 TS `run <id>` and `serve --once` dispatch nothing in **worktree** mode — **FIX (oracle patched)**

**Effect on differential comparison: `oracle patched`** — the first entry to use that value. A minimal
documented patch (`parity/patches/0001-…`) is applied by `parity/setup.sh`: keep the child `ref`'d while it is
being awaited. Without it, no `run`/`serve --once` scenario can be compared in the **default** workspace mode.

**The reference implementation's primary dispatch commands exit 0 without doing anything, in the mode that
ships as the default.** Lead-verified matrix:

| Mode | Repo | `run <id>` | `serve --once` |
|---|---|---|---|
| `shared` | no git | ✓ dispatches | ✓ |
| `shared` | git | ✓ dispatches | ✓ |
| **`worktree`** (default) | git | **✗ no-op, exit 0** | **✗ no-op, exit 0** |

**Mechanism**, confirmed in source:

1. `ProcessManager.spawn` (`process-manager.ts:64-80`) spawns with `stdio: ['ignore', 'pipe', 'pipe']`,
   `detached: true`, and then calls **`proc.unref()`**. Its own comment states the intent exactly:
   *"Pipes (stdout/stderr) still hold refs while being read — that's intentional."* The ref exists **only
   while something is reading**.
2. `WorkspaceManager.spawnAndWait` (`workspace-manager.ts:171-181`) uses that spawn and awaits a promise
   resolved by `'close'`/`'error'` — and **never reads `proc.stdout` or `proc.stderr`**.
3. So nothing holds a `ref` on the event loop: the child is unref'd and its pipes are unread. Node's loop
   empties, the process exits 0, and the `await` **never resolves**.

The contrast inside the same file is the proof: `spawnAndCapture` (`:184`) attaches
`proc.stdout?.on('data', …)`, which refs the pipe and keeps the loop alive — so it works, and `spawnAndWait`
does not. Every `spawnAndWait` call site is affected: `git rev-parse --is-inside-work-tree` (`:57`),
`git worktree add` (`:146`, `:152`), `git worktree prune` (`:151`), `git worktree remove` (`:79`),
`git branch -D` (`:83`), `git clone` (`:209`), `rsync` (`:218`).

**Why `shared` escapes it**: `prepare()` guards with `if (mode !== 'shared') await this.requireGitRepo(mode)`
and returns `{ path: projectRoot }` for `shared` without spawning anything. So the shared path never touches
`spawnAndWait`. The matrix above falls directly out of that guard — which is strong corroboration that the
diagnosis is right.

The first unread-pipe spawn in a worktree dispatch is `git rev-parse` (`:57`); the lead observed the exit
occurring mid-`git worktree add`, which simply means something else still ref'd the loop during `rev-parse`
and it emptied slightly later. The outcome is identical either way, and the exact exit point is
environment-dependent — do not encode it in a test.

**Wreckage left behind**, which a scenario must tolerate:

- the task stays `todo` (never transitions, never gets a run record);
- `.orchestry/orchestry.lock` persists, holding a **dead** PID — so unlike §14.17 it is re-acquirable and does
  not wedge the project;
- `state.claimed` retains the task id. `dispatchAll` filters on `!claimed.has(id)`, so the task is
  **suppressed from `run --all` and `serve`** until a `startWatch` runs `cleanupStaleRunningEntries`, which
  unconditionally clears `claimed`. A direct `run <id>` still works, because `dispatchOnlyTask` calls
  `dispatchTask` without consulting the claimed filter.

**Decision: fix** — r-orch dispatches in worktree mode. Keep the child referenced for the duration of the
await (or read its pipes, which r-orch does anyway for the stderr tail, §14.12). There is no sense in
replicating a default-mode no-op.

> **Note for anyone reading the captured reference output in §5.4:** that `orch run` transcript was taken
> after `orch config set defaults.agent.workspace_mode shared`, which is why it shows a real dispatch. The
> default-mode failure was not visible in it.

### 14.22 `kill(pid)` with a stale `pid: 0` signals the orchestrator's own process group — **FIX**

**Effect on differential comparison: `none`.**

`ProcessManager.kill` tries the process **group** first: `process.kill(-pid, signal)`. For `pid === 0` that is
`process.kill(-0, …)`, and since `-0 === 0`, the target is **process group 0 — the caller's own group.** So a
stale `pid: 0` makes the orchestrator **SIGTERM itself** (and every sibling in its group). `isAlive(0)` does
not save you: `process.kill(0, 0)` signals the caller's own group, succeeds, and reports the phantom process
as alive, so `killWithGrace(0)` proceeds straight to the `SIGTERM`.

**This is reachable, not theoretical.** The shell adapter returns `{ pid: 0, events: errorGen() }` when the
agent has no `config.command` (`shell.ts:47`, §8.10). If such a handle's `pid` reaches `state.running` — or
survives in a hand-edited or partially-written `state.json` — the next reconcile, stall kill, `cancelTask`,
`forceStopAgent` or `stop()` turns into self-termination.

**Decision: fix.** The process manager **refuses to signal pid 0** (and, for the same reason, any
non-positive pid), logging the refusal rather than silently ignoring it. There is no legitimate call: ORCH
never means "my own group". Also treat `is_alive(0)` as **false**, so a stale zero is reaped as a dead entry
instead of being believed.

### 14.23 `run <id>` must wait for the event collector to finish — **implementation of equivalent behaviour**

**Effect on differential comparison: `none`.**

`agent:completed` is emitted by `runService.finish`, which runs **inside** `collectEvents` — so the event
fires while the collector is still draining and persisting. The TS process nonetheless stays alive to the end
of the run, because the adapter's stdout pipe is **actively being read**, and that holds a `ref` on node's
event loop until EOF.

That is the same mechanism as §14.21 seen from the other side: **read pipes keep the process alive; unread
pipes let it exit.** `run <id>` works by accident of the first; worktree-mode dispatch breaks by accident of
the second.

r-orch has no equivalent implicit keep-alive, so `run <id>` must **explicitly await the collector task** —
not merely the `agent:completed` event — before returning. Returning on the event would truncate the JSONL
event log, skip the proof/stats writes, and exit before the task reaches its terminal status. This is not a
divergence: it is what the original does, made explicit.

### 14.24 `stop()` does not interrupt an in-flight tick — **REPLICATE**

**Effect on differential comparison: `none`.**

`stop()` calls `clearInterval`, which prevents **future** ticks but cannot interrupt a callback already
running. So a `stop()` issued mid-tick waits for that tick to finish (the state mutex serializes them anyway,
§7.15).

**Decision: replicate.** Aborting a tick in flight would be worse, not better: `atomicWrite` writes
`.state.json.<random>.tmp` and renames, so cancelling between write and rename **litters the `.orchestry/`
directory with temp files** that nothing ever cleans up. A tick is bounded work; letting it complete is
correct. Pair this with §14.15 — the wind-down must still complete every step and release the lock even when
steps fail.

### 14.25 Index rebuild silently discarded an unreadable entity file — **PORT DEFECT, corrected toward TS**

**Effect on differential comparison: `none`** — the correction *removes* a divergence.

Not a bug in the original. The TS `rebuildIndex` lets a parse error propagate, so a corrupt entity file fails
the read loudly. The port's `scan_items` dropped the unreadable file and then **persisted the loss** by
writing the index without it — turning a recoverable corrupt file into a silently deleted entity, which is a
divergence in the unsafe direction (gate code review B1).

**Decision: restore propagation**, matching TS — so this entry records a **convergence**, not a divergence.
`rebuild_index` now propagates parse errors: a corrupt `.yml` makes the listing fail with an error naming the
file, rather than quietly shrinking the result set. The recovery is in
[docs/RUNBOOK.md](RUNBOOK.md#rebuilding-an-index-after-a-lost-update) — fix or remove the file, then rebuild.

Worth recording *why* the defect survived review: the old code carried a comment asserting that "TS drops
nulls the same way." That was **true of nulls and false of errors** — `rebuildIndex` does filter out a
`readYaml` that returned `null` for a *missing* file, but a **parse failure throws** and propagates. A
half-true comment is harder to spot than a wrong one, which is the same failure mode as the `key.meta`
comment in §12.5.

### 14.26 Signal handlers installed after the first tick — **PORT DEFECT, corrected toward TS**

**Effect on differential comparison: `none`.**

TS registers `SIGINT`/`SIGTERM` handlers **before** the initial tick (§7.3 step 5, then step 7). The port
installed them after, leaving a window in which Ctrl-C kills the process mid-dispatch with no graceful
shutdown — no lock release, no run finalisation, so the exact wreckage of §14.21 and §14.17 by a different
route.

**Decision: install before the first tick**, matching the documented order.

### 14.27 No startup sweep of orphaned `running` runs — **FIX**

**Effect on differential comparison: `none`.**

A crash between `runService.create` and the state write leaves a `Run` in status `running` that no
`state.running` entry references, so reconcile never sees it and it stays `running` forever. TS sweeps
`preparing` runs at startup (§7.6 phase 4) but **not** `running` ones.

**Decision: fix** — extend the startup sweep to finalise orphaned `running` runs as well as `preparing` ones,
using the same `cancelled` status and a message naming the restart. Strictly additive: it only touches runs
that are unreachable from state, which TS leaves stranded.

### 14.28 `start_watch` leaks the lock file on any error after acquisition — **FIX**

**Effect on differential comparison: `none`.**

`start_watch` acquires the lock and then does several fallible things — state load, stale-entry cleanup, the
state write, the first tick. An error in any of them returns without releasing the lock, leaving a file whose
PID belongs to a process that has exited. Dead PIDs are re-acquirable, so this is a nuisance rather than the
§14.17 wedge — but it is avoidable.

**Decision: fix** — release the lock on every error path out of `start_watch`. Pairs with §14.15: the
wind-down completes and releases even when its steps fail.

### 14.29 Lock PIDs of `0` or above `i32::MAX` — **FIX (differs from TS for `0`)**

**Effect on differential comparison: `none` expected** — reaching it requires a lock file that neither
implementation writes.

A PID that parses as a number but cannot name a live process is treated as **dead**, so the lock is replaced:

- **`0`** — TS's `isProcessAlive(0)` calls `process.kill(0, 0)`, which signals the **caller's own process
  group**, succeeds, and reports it alive — so TS refuses the lock forever. r-orch treats `0` as dead. This
  is the same root cause as §14.22 and the same decision.
- **`> i32::MAX`** — cannot be a PID on any supported platform.

### 14.30 `.` and `..` accepted in extension-less id paths — **FIX (additive)**

**Effect on differential comparison: `none`.**

`sanitize_id`'s character class `^[A-Za-z0-9._-]+$` (§6.2) admits the strings `.` and `..`, because both
consist only of permitted characters. For ids used with an extension this is harmless (`./..yml` is just an
odd filename), but an id used as a **bare path segment** — the workspace directory
`.orchestry/workspaces/<taskId>` — would resolve to the parent directory.

**Decision: fix** — reject `.` and `..` outright. Additive hardening: no legitimate id is affected, and the
generated form is `<prefix>_<nanoid(7)>`, which cannot produce either. **Reachable only by hand-editing** an
entity file or `state.json` to contain such an id — no CLI path and no adapter can produce one — which is why
it is hardening rather than a live bug.

### 14.31 YAML read guards reject documents js-yaml accepts — **DOCUMENTED DIVERGENCE**

**Effect on differential comparison: `none` in the corpus** — no scenario produces a document near any bound.

js-yaml has no limits of any kind. r-orch refuses a document that trips **any of four guards**:

| Guard | Bound |
|---|---|
| Document size | **> 1 MiB** |
| Alias count | **> 64** |
| Estimated expansion — `len × (1 + aliases)` | **> 8 MiB** |
| Parse wall-clock | **> 2 s** |

The expansion estimate is the one that matters: size and alias count are each individually cheap to stay
under while still amplifying, so the product is what actually bounds a "billion laughs". The time bound is
the backstop for anything the first three miss.

**This is not a workaround for the YAML crate.** `serde_norway` was measured on the same inputs and behaves
**identically** to `serde_yaml_ng`, so the guards are r-orch's own hardening rather than compensation for a
parser deficiency — and swapping the crate would not remove the need for them.

**Decision: diverge, deliberately.** These files can be checked into a shared repository, so a hostile or
accidental one is reachable by anyone with commit access. No bound is reachable by anything r-orch *writes* —
the largest artifact is an agent file holding a few kilobytes of role prompt — so the guards only ever fire
on hand-authored or externally-supplied input.

Recorded as a divergence rather than a fix because TS genuinely accepts such files: **a project r-orch
refuses may still open in the original**, which is the asymmetry an operator would hit.

### 14.32 Run-events files over 50 MiB are tail-read, not read whole — **FIX**

**Effect on differential comparison: `none`** — unreachable in the corpus; no scenario generates an events
file of that size.

A run's `.jsonl` event log is append-only and unbounded (§6.9). **TS reads it in full** on the per-read path
the CLI and TUI use, so a pathological run can exhaust memory.

**Decision: fix.** Above **50 MiB**, r-orch reads only the **last 1 MiB** and **drops the partial first
line**, since a mid-record boundary cannot be parsed. Two consequences to be aware of rather than surprised
by:

- the result is the **tail**, not the whole log — `orch logs` on such a run shows recent events only;
- the oldest event in that window is dropped even if it happened to be complete, because the reader cannot
  distinguish "started at a record boundary" from "started mid-record" without scanning backwards.

The raw file is never truncated, so `.orchestry/runs/<run-id>.jsonl` remains the complete record for anything
that wants to read it directly.

### 14.33 Token totals can overflow — **FIX**

**Effect on differential comparison: `none`.**

Cumulative token counters in `state.json` (`stats.total_tokens`) are summed without bound. In TS they are
JS numbers, which lose precision rather than wrap. In Rust an integer accumulator would **overflow** —
panicking in debug, wrapping in release, which would corrupt the stats file.

**Decision: fix** — saturating addition. A pinned maximum is wrong but monotonic and inert; a wrapped total
is wrong and actively misleading. The totals are informational and no control flow depends on them.

### 14.34 `tracing` output goes to stderr only, and only under `ORCHESTRY_DEBUG` — **FIX**

**Effect on differential comparison: `none`** — the harness never sets the variable, so no record is emitted.

`tracing-subscriber` was a declared dependency with **no subscriber ever installed**, so all `tracing::`
call sites wrote nowhere (gate code review M4, sre C4). What was lost mattered: the only signal
distinguishing "another process holds the lock" from "I could not read the lock file" — both of which print
`PID: undefined` (§14.17) — plus failed-reap and refused-signal diagnostics.

**Decision: fix** — install a subscriber gated on `ORCHESTRY_DEBUG`, writing to **stderr only**. Stderr is
mandatory, not stylistic: stdout carries the `--json` contract (§4) and the `serve` log stream (§11.1), and a
diagnostic line on stdout would corrupt both. Documented for operators in
[docs/RUNBOOK.md](RUNBOOK.md#debugging).

### 14.35 `config.yml` and `agents/*.yml` are written owner-only (0600) — **FIX**

**Effect on differential comparison: `none` — and that is precisely why this row exists.** The harness
compares file *contents*, never file *modes*, so this divergence is invisible to every scenario. Without a
§14 row it would be an undocumented behavioural difference that nothing could catch.

TS writes every artifact at the process umask — typically **0644**, world-readable. r-orch writes the two
files that can hold credentials at **0600**:

| File | Why |
|---|---|
| `.orchestry/agents/<id>.yml` | an agent's `config.env` map is written verbatim and passed to every child it spawns, so it is the natural place an API key ends up |
| `.orchestry/config.yml` | project configuration, hand-edited and able to carry per-project settings |

**The mode is set on the temp file at creation** (`OpenOptions::mode()`), not applied after the write, so the
file is **never briefly world-readable** — there is no umask window to race. A test asserts exactly that
(`an_owner_only_write_is_never_briefly_world_readable`). Unix only: `owner_only()` yields `None` where the
platform has no modes, and the write proceeds unrestricted.

**Scope is deliberately narrow.** `tasks/`, `goals/`, `teams/`, `messages/`, `context/`, `runs/` and
`state.json` keep umask modes, matching TS. They are not credential-bearing by design, and tightening
everything would be a larger divergence for no security gain — a secret pasted into a task *description* is
a user error that 0600 would hide rather than fix.

**This does not make secrets in `env:` safe.** `.orchestry/agents/` is **not** covered by `init`'s
`.gitignore`, so an API key there still reaches your repository on the next `git add`. 0600 closes local
read access, not disclosure through version control. See
[docs/RUNBOOK.md](RUNBOOK.md#secrets-in-agent-config).

---

## 15. Verification matrix

Every row must be satisfiable by an automated check. Full harness design in
[ARCHITECTURE.md §8](ARCHITECTURE.md).

| # | Requirement | Level | How it is verified |
|---|---|---|---|
| V1 | Every command, subcommand, alias, positional and flag in §2.2 exists with the same arity | L0 | Generated test walking the clap command tree against a checked-in table extracted from `bin/cli.ts` |
| V2 | Exit codes per §3 | L0 | Differential harness: run the same argv against both binaries, compare exit codes |
| V3 | `--json` parsed values match for every command in §4.1 | L0 | Differential harness with the §1.4 normalizers |
| V4 | Human stdout matches for the commands in §5.4 | L0 (structure) | Differential harness, ANSI-stripped, trailing spaces preserved |
| V5 | Error messages and hints per §3.1 | L0 | Table test + differential on stderr |
| V6 | `.orchestry/` artifacts parse-equal after the same scenario | L0 | Differential harness compares every file, parsed, with normalizers |
| V7 | **Rust writes → TS reads** without error, and vice versa | L0 | Cross-compat scenarios: each binary performs the first half of a scenario and the other the second half |
| V8 | YAML timestamps survive a Rust-write → TS-read round trip **as strings** | L0 | Dedicated test asserting `typeof === 'string'` in the TS process (§6.4) |
| V9 | Task state machine transition table | L0 | Exhaustive 7×7 table test |
| V10 | Retry delay arithmetic | L0 | Table test over `(attempt, base, max)` |
| V11 | Dispatch candidate ordering | L1 | Property test with fixed fixtures |
| V12 | Scope overlap agrees with the pairwise function, including the trie batch form | L1 | Property test: `ScopeIndex::overlaps_any` ≡ any `patterns_overlap` |
| V13 | `classifyAdapterError` conformance vectors (§8.8) | L0 | Table test |
| V14 | Adapter argv per §8.2 | L0 | Snapshot test per adapter over the config matrix |
| V15 | Adapter stdout parsing | L1 | Fixture test: recorded stdout lines → expected `AgentEvent` sequence |
| V16 | Default prompt text, all branch combinations | L0 | Golden fixtures generated from the TS binary, byte-compared |
| V17 | Skill block format and resolution rules | L1 | Unit tests + a golden prompt with skills |
| V18 | Worktree branch naming, including CJK/emoji fallback | L0 | Table test |
| V19 | Git command sequences for prepare/cleanup/merge | L1 | Fake-git test double recording argv |
| V20 | `serve --json` log record fields per §11.1 | L0 | Differential harness on a scenario with a fake agent |
| V21 | `serve --once` exit code semantics | L0 | Scenario tests: all-pass → 0; one failed → 1; one cancelled → 0; empty → 0 |
| V22 | Lock acquisition, staleness and PID-recycling defence | L1 | Integration test with a real second process |
| V22b | **Corrupt-lock matrix** (§6.7): absent / live+fresh / live+stale / dead / garbage / empty — each yields the measured outcome, exit code and post-state of the lock file, including `PID: undefined` on stderr for the garbage and empty cases | L0 | Oracle-compared scenarios with a fake agent, one per row |
| V23 | Concurrency cap is never exceeded | L1 | Integration test with N fake agents and `max_concurrent_agents = 2` |
| V24 | Stall detection fires at the configured timeout | L1 | Integration test with a fake agent that sleeps |
| V25 | Crash recovery: dead PID → task `cancelled`, claims cleared, `preparing` runs finalized | L1 | Integration test that kills the orchestrator mid-run |
| V26 | Cascade failure graph walk | L1 | Unit test with a diamond dependency graph |
| V27 | TUI keymap and command-bar verbs | L1 | `ratatui::TestBackend` tests driving key sequences |
| V28 | Agent-shop / org-shop / default-agent catalogue data | L0 | Byte comparison against strings extracted from the TS source |
| V29 | ID format: prefix + 7 chars from the 64-char URL-safe alphabet | L0 | Property test |
| V30 | `sanitizeId` rejects path traversal | L0 | Table test with `../`, `/`, `a/b`, empty, unicode |
| V31 | **Harness hermeticity**: `HOME`/`USERPROFILE` redirected per tmpdir; a fresh `update-check.json` pre-seeded; the oracle spawns **no `npm`** and makes **no network call** | L0 | Scenario asserting the process tree contains no `npm` and no socket; run with the network namespace disabled in CI |
| V32 | **Generalized YAML cross-parse**: every YAML artifact in **both** trees re-parsed **in the TS process with js-yaml** and deep-compared against the Rust parse | L0 | Harness step (~50 lines of Node glue). **Subsumes V8.** The single highest-value check in the plan |
| V33 | **TUI counted-case enumeration**: every wizard field, validation message, command-bar verb and filter predicate has a named `TestBackend` case; the total is tracked and must not regress. **Baseline: `crates/orch-tui/TEST-COVERAGE.md`, which carries the count and its per-area breakdown and is the authoritative figure. No count is restated in this document** — it was hand-bumped four times (740 → 742 → 747 → 754) before being removed, which is the drift this rule exists to stop. | L0/L1 | Counted test inventory, asserted like V1's generated command-tree test |
| V34 | **Stall clock semantics**: a run emitting blank lines stalls; a run emitting unparseable non-blank lines does not | L1 | Cross-compat `serve --once` scenario with a fake agent, compared against the oracle |
| V35 | **`serve --once` killed mid-run** → task recovery, run finalization and `--json` records match the oracle | L1 | Cross-compat scenario with a fake agent and an external kill |
| V36 | **`max_concurrent_agents` saturation**: N ready tasks, cap 2 — dispatch order, claim/release and final tree match the oracle | L1 | Cross-compat scenario with fake agents |
| V37 | **Concurrent cross-binary `_index.json` mutation** — documents the lost-update behaviour rather than asserting it away | L1 | Two processes (TS + Rust) interleaving `task add`; asserts the **documented** outcome of ARCHITECTURE A2, including that an entity file can exist while absent from the index |
| V38 | **`config.yml` merge keys** (`<<`) resolve identically in both binaries | L0 | Hand-authored `config.yml` fixture; `config get` compared across binaries (§6.4.2) |
| V39 | **Walking skeleton gate** (WP-0b): ≥1 green differential scenario and ≥1 green `rust_then_ts` cross-compat scenario over `init`/`agent add`/`task add`/`task list` | L0 | `cargo xtask parity` — the first ratchet, required before WP-4 and WP-5 begin |
| V40 | **Adapter stderr draining**: every adapter completes a run in which the child writes 256 KB to stderr | L1 | Per-adapter integration test (§14.12) |

### 15.1 Deliberately outside the harness

A scenario step is an argv line run to completion, so four surfaces **cannot** be expressed as differential
scenarios. Stating them makes the gap visible instead of implied:

| Surface | Why it cannot be compared | Covered instead by |
|---|---|---|
| `tui` | Interactive, never terminates, renders to a TTY | V33 (`TestBackend` counted cases) |
| `serve` **without** `--once` | Never terminates | V20, V34–V36 via `--once` |
| `run --watch` | Never terminates | V22–V25 (Rust-side integration tests) |
| `logs --follow` | Streams from the event bus until interrupted (`src/cli/commands/logs.ts:22,34-35,204-207`) | Rust-side integration test only |

Everything in this table is verified **against the specification, not against the oracle.** That is a real
limitation: §7's timing constants and the orchestrator's live behaviour rest on this document's reading of
`orchestrator.ts`, and V34–V36 exist specifically to pull the highest-density parts of that behaviour back
into oracle-verified territory via `--once`.
