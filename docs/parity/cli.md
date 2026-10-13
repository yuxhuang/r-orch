# ORCH TypeScript CLI — Complete Surface Specification (input to Rust parity spec)

Source of truth: `parity/orch-ts` (read-only).
Package `@oxgeneral/orch`, version **1.0.34**, `"type": "module"`, Node `>=20.0.0`.
Binaries (all three map to `./dist/cli.js`): `orchestry`, `orch`, `ao`.

Every claim below was read from source **and**, where observable, verified by executing the built
`dist/cli.js` against four throwaway projects (`/tmp/op2` shell-adapter, `/tmp/op3` claude-adapter,
`/tmp/op4` shell+run, `/tmp/op5` empty). Verified output is marked “verified”.

---

## 1. Global surface, routing, container tiers

### 1.1 Program declaration (`src/bin/cli.ts`)

```ts
program
  .name('orchestry')
  .description('Agents Organizations — CLI orchestrator for AI agents')
  .version('1.0.34')
  .option('--json', 'Output as JSON')
  .option('--quiet', 'Minimal output (IDs only)')
  .option('--no-color', 'Disable colors')
  .option('--ascii', 'ASCII-only output (no Unicode)')
  .hook('preAction', async (thisCommand) => {
    const opts = thisCommand.opts();
    if (opts.ascii) setAsciiMode(true);
    if (opts.color === false) setNoColor(true);
  });
```

### 1.2 Global options (contract)

| Long | Short | Type | Default | Effect |
|---|---|---|---|---|
| `--json` | — | boolean | `false` | `CliContext.json = true` |
| `--quiet` | — | boolean | `false` | `CliContext.quiet = true` |
| `--no-color` | — | negated boolean | color on | sets `chalk.level = 0` |
| `--ascii` | — | boolean | `false` | swaps Unicode icons for ASCII |
| `--version` | `-V` | — | — | prints `1.0.34\n`, exit 0 (verified) |
| `--help` | `-h` | — | — | Commander help, exit 0 |

`--no-color` is a Commander negated option, so the parsed value is `opts.color` (`true` by default,
`false` when the flag is present). Both `setAsciiMode` / `setNoColor` are applied in the `preAction`
hook, i.e. only when an action command actually runs.

### 1.3 Version string

Literal `'1.0.34'`, hard-coded in `.version(...)` in `src/bin/cli.ts` (kept in sync with
`package.json` by `scripts/release.sh`). `program.version()` is read back at runtime by `serve`
(for `update:available` logging), `tui` and `update`.

### 1.4 CLI context (`src/cli/context.ts`)

```ts
export interface CliContext {
  projectRoot: string;
  json: boolean;
  quiet: boolean;
  noColor: boolean;
  ascii: boolean;
}

export function createContext(opts: {...}): CliContext {
  const noColor = opts.noColor || 'NO_COLOR' in process.env || false;
  const ascii   = opts.ascii || process.env['TERM'] === 'dumb' || false;
  return { projectRoot: findProjectRoot(), json: opts.json ?? false,
           quiet: opts.quiet ?? false, noColor, ascii };
}
```

* `NO_COLOR` is a **presence** check, not a truthiness check — `NO_COLOR=0` and `NO_COLOR=`
  both disable color (asserted in `test/unit/cli/create-context.test.ts`).
* `TERM=dumb` forces ASCII mode.
* `findProjectRoot(startDir = process.cwd())` walks up using `accessSync(path.join(dir, '.orchestry'))`
  until the filesystem root; returns the resolved start dir if nothing is found. Results are memoized
  in a module-level `Map<string,string>`; `clearProjectRootCache()` exists for tests.

### 1.5 Subcommand routing and container tiers

```ts
const LIGHT_COMMANDS: Record<string, (p, c: LightContainer) => Promise<void>> = {
  task, agent, status, logs, config, context, msg, goal, team, org,
};
const FULL_COMMANDS: Record<string, (p, c: Container) => Promise<void>> = {
  run, doctor, tui, serve,
};
```

```ts
program.parseOptions(process.argv);
const globalOpts = program.opts();
const sub = process.argv.slice(2).find((arg) => !arg.startsWith('-'));   // first non-flag token
const needsFull = !sub || sub in FULL_COMMANDS;                           // NO sub => FULL
```

* `sub` is found by skipping leading flags, so `orch --json run tsk_1` still routes to `run`
  (asserted in `test/unit/cli/cli-lazy-routing.test.ts`).
* `init` and `update` are registered with **no container at all** (tier N).
* `doctor` is registered with an optional container: `registerDoctorCommand(program, container?)`.
* `task cancel` is the single light command that lazily builds a **full** container, and only when
  the target task is `in_progress`:

```ts
if (task.status === 'in_progress') {
  const { buildFullContainer } = await import('../../container.js');
  const full = await buildFullContainer(container.context);
  await full.orchestrator.cancelTask(id);
} else {
  await container.taskService.cancel(id);
}
```

**LightContainer** (`buildLightContainer`) contains: `context`, `paths`, `config`,
`taskStore`, `agentStore`, `runStore`, `stateStore`, `configStore`, `globalConfigStore`,
`globalConfig` (left at `DEFAULT_GLOBAL_CONFIG`, **not** read from disk), `contextStore`,
`messageStore`, `goalStore`, `teamStore`, `eventBus`, `taskService`, `agentService`,
`runService`, `messageService`, `goalService`, `teamService`.
It calls `paths.requireInit()` in parallel with `configStore.read()`, so a missing `.orchestry/`
throws `NotInitializedError` during container construction.

**Container** (`buildFullContainer`) = LightContainer + global config read from disk +
`processManager`, `adapterRegistry`, `workspaceManager`, `templateEngine` (LiquidJS),
`skillLoader`, `doctorService`, `orchestrator`.
Adapter registration order (this is the `doctor` check order): **claude, codex, cursor, shell,
opencode, pi, grok, antigravity**.

### 1.6 Help / version fast path and command stubs

```ts
const COMMAND_STUBS: Array<[name: string, description: string]> = [
  ['task',    'Manage tasks'],
  ['agent',   'Manage agents'],
  ['status',  'Show orchestrator status'],
  ['logs',    'View run logs'],
  ['config',  'Manage configuration'],
  ['context', 'Shared context store for inter-agent data exchange'],
  ['msg',     'Inter-agent messaging'],
  ['goal',    'Manage goals'],
  ['team',    'Manage teams'],
  ['org',     'Pre-built AI companies'],
  ['run',     'Run tasks'],
  ['doctor',  'Check adapters and dependencies'],
  ['tui',     'Launch TUI dashboard'],
  ['serve',   'Headless daemon mode with structured logs'],
  ['init',    'Initialize project'],
  ['update',  'Check for updates'],
];
const ALL_KNOWN_COMMANDS = new Set(COMMAND_STUBS.map(([name]) => name));
```

```ts
const hasRealSub = sub !== undefined && ALL_KNOWN_COMMANDS.has(sub);
const isHelpOrVersion = process.argv.includes('--help') || process.argv.includes('-h')
  || process.argv.includes('--version') || process.argv.includes('-V');
if (isHelpOrVersion && !hasRealSub) { /* register stubs only, no container */ }
```

Verified `orch --help` (exact bytes of the body):

```
Usage: orchestry [options] [command]

Agents Organizations — CLI orchestrator for AI agents

Options:
  -V, --version   output the version number
  --json          Output as JSON
  --quiet         Minimal output (IDs only)
  --no-color      Disable colors
  --ascii         ASCII-only output (no Unicode)
  -h, --help      display help for command

Commands:
  task            Manage tasks
  agent           Manage agents
  status          Show orchestrator status
  logs            View run logs
  config          Manage configuration
  context         Shared context store for inter-agent data exchange
  msg             Inter-agent messaging
  goal            Manage goals
  team            Manage teams
  org             Pre-built AI companies
  run             Run tasks
  doctor          Check adapters and dependencies
  tui             Launch TUI dashboard
  serve           Headless daemon mode with structured logs
  init            Initialize project
  update          Check for updates
  help [command]  display help for command
```

The stub descriptions differ from the **registered** descriptions used when a container is built:

| command | stub description | registered description |
|---|---|---|
| `tui` | `Launch TUI dashboard` | `Launch interactive TUI dashboard` |
| `serve` | `Headless daemon mode with structured logs` | `Headless daemon mode — structured logs to stdout` |
| `init` | `Initialize project` | `Initialize .orchestry/ in the current directory` |
| `update` | `Check for updates` | `Check for updates and install the latest version` |
| `org` | `Pre-built AI companies` | `Pre-built AI companies — deploy a full department with one command` |
| `team` | `Manage teams` | `Manage agent teams` |

All other registered descriptions equal their stub text.

### 1.7 FTUE (bare `orch`)

```ts
const isBareOrch = process.argv.length <= 2;
if (isBareOrch && !(await pathExists(path.join(process.cwd(), ORCHESTRY_DIR)))) {
  const { runInit } = await import('../cli/commands/init.js');
  await runInit();
  const freshContext = createContext({...});
  const freshContainer = await buildFullContainer(freshContext);
  await FULL_COMMANDS['tui']!(program, freshContainer);
  await program.parseAsync([...process.argv, 'tui']);
  return;
}
...
if (isBareOrch) { process.argv.push('tui'); }
```

Important: the FTUE check runs **before** `createContext()`, because `findProjectRoot()` walks up and
would otherwise bind to a parent project. The FTUE path calls `runInit()` directly, so the
`Next: …` hint lines printed by `registerInitCommand` are **not** emitted.

### 1.8 Background update check hook

```ts
const skipUpdateCheck = sub === 'tui' || sub === 'update' || sub === 'serve';
const updateCheck = skipUpdateCheck ? Promise.resolve(null)
  : import('../cli/update-check.js').then((m) => { updateMod = m;
      return m.checkForUpdateSWR(program.version() ?? '0.0.0'); });

await program.parseAsync(process.argv);

if (!skipUpdateCheck) {
  const info = await updateCheck;
  if (info && updateMod) {
    updateMod.printUpdateNotification(info);
    if (info.updateAvailable) { updateMod.backgroundInstall(info.latest).catch(() => {}); }
  }
}
```

So every command except `tui`, `update`, `serve` may print an update notice to **stderr** after its
own output and may trigger a silent `npm install -g` in the background.

---

## 2. Output primitives (`src/cli/output.ts`) — the shared rendering contract

### 2.1 Icons

```ts
export const icons = {
  running: '●', todo: '○', review: '◈', done: '✓', failed: '✕', retrying: '↻',
  cancelled: '○', idle: '○', error: '✕', disabled: '─',
  agentAction: '▸', orchestratorEvent: '→', warning: '⚠',
} as const;

const asciiIcons = {
  running: '*', todo: 'o', review: '#', done: '+', failed: 'x', retrying: '~',
  cancelled: 'o', idle: 'o', error: 'x', disabled: '-',
  agentAction: '>', orchestratorEvent: '->', warning: '!!',
};
```

`getIcon(name)` returns the ASCII variant when `--ascii`/`TERM=dumb` is active.

### 2.2 Colors (decoration only — never structural)

```ts
const COLOR_CODES = { amber: 214, green: 72, red: 167, blue: 74, yellow: 178,
                      dim: 240, ghost: 236, white: 255, purple: 141 } as const;
```

Applied via `chalk.ansi256(code)`, lazily instantiated behind a `Proxy` (so importing the module
makes no chalk calls — asserted in `test/unit/cli/output-lazy-chalk.test.ts`).

`statusIcon(status)` mapping:

| status | render |
|---|---|
| `running`, `in_progress` | `green(●)` |
| `todo` | `dim(○)` |
| `review` | `blue(◈)` |
| `done` | `green(✓)` |
| `failed` | `red(✕)` |
| `retrying` | `yellow(↻)` |
| `cancelled` | `dim(○)` |
| `idle` | `dim(○)` |
| `error` | `red(✕)` |
| `disabled` | `ghost(─)` |
| default | `getIcon(status)` uncolored |

`priorityLabel(n)`: `1 → red('P1')`, `2 → yellow('P2')`, `3 → 'P3'` (uncolored),
`4 → dim('P4')`, otherwise `` `P${priority}` `` uncolored.

Helpers: `filePath(p) = purple(p)`, `agentName(n) = green(n)`, `amber(t)`, `dim(t)`.

### 2.3 Formatters

```ts
export function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const secs = seconds % 60;
  if (minutes < 60) return `${minutes}:${String(secs).padStart(2, '0')}`;
  const hours = Math.floor(minutes / 60);
  const mins = minutes % 60;
  return `${hours}h${String(mins).padStart(2, '0')}m`;
}
export function formatDurationSince(isoDate: string): string {
  return formatDuration(Date.now() - new Date(isoDate).getTime());
}
export function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
```

**Not clamped**: a future timestamp yields a negative duration, e.g. `formatDuration(-60000)`
→ `"-60s"`. This is observable in `context list`’s TTL column (verified: `-60s`).

### 2.4 Line emitters

```ts
export function printError(message: string, hint?: string): void {
  console.error(`  ${colors.red(getIcon('failed'))} ${message}`);
  if (hint) { console.error(`    ${hint}`); }
}
export function printSuccess(message: string): void {
  console.log(`  ${colors.green(getIcon('done'))} ${message}`);
}
export function printWarning(message: string): void {
  console.log(`  ${colors.yellow(getIcon('warning'))} ${message}`);
}
```

* `printError` writes to **stderr**; hint line is 4 spaces, no icon, no color.
* `printSuccess` / `printWarning` write to **stdout**.

```ts
export function printTable(headers: string[], rows: string[][], padding: number = 2): void {
  const colWidths = headers.map((h, i) =>
    Math.max(h.length, ...rows.map((r) => stripAnsi(r[i] ?? '').length)));
  const headerLine = headers.map((h, i) => h.padEnd(colWidths[i]! + padding)).join('');
  lines.push(`  ${colors.dim(headerLine)}`);
  for (const row of rows) {
    const line = row.map((cell, i) => {
      const stripped = stripAnsi(cell);
      const pad = (colWidths[i] ?? 0) + padding - stripped.length;
      return cell + ' '.repeat(Math.max(0, pad));
    }).join('');
    lines.push(`  ${line}`);
  }
  process.stdout.write(lines.join('\n') + '\n');
}
```

* Column width = max(header length, widest ANSI-stripped cell). Padding default 2.
* Two-space line prefix. The whole header line is dimmed as one unit.
* **Trailing spaces are emitted on every line** (header and rows), because the last cell is also padded.
* Written with a single `process.stdout.write`, joined by `\n`, terminated by one `\n`.

```ts
export function printKeyValue(pairs: Array<[string, string]>): void {
  const maxKey = Math.max(...pairs.map(([k]) => k.length));
  const lines = pairs.map(([key, value]) => `  ${colors.dim(key.padEnd(maxKey + 2))}${value}`);
  process.stdout.write(lines.join('\n') + '\n');
}
```

`stripAnsi` = `str.replace(/\x1b\[[0-9;]*m/g, '')`.

---

## 3. Errors, exit codes, and failure rendering

### 3.1 Code scheme (`src/domain/errors.ts` header comment)

```
0 - Success
1 - General error
2 - Invalid arguments
3 - Not initialized (.orchestry/ not found)
4 - Lock conflict (orchestrator already running)
5 - Agent error (adapter test failed)
```
plus `6` used by `WorkspaceError`.

### 3.2 Error classes

| Class | `exitCode` | `message` | `hint` |
|---|---|---|---|
| `OrchestryError` | ctor arg | ctor arg | ctor arg (optional) |
| `NotInitializedError` | 3 | `Not initialized` | `Run: orch init` |
| `InvalidArgumentsError` | 2 | ctor arg | — |
| `LockConflictError` | 4 | `Orchestrator already running (PID: ${pid})` | `Use: orch status` |
| `AgentAdapterError` | 5 | `Agent adapter "${adapter}" not available` | `detail` |
| `NoAgentsError` | 1 | `No agents configured` | `Run: orch agent add <name> --adapter <adapter>` |
| `TaskNotFoundError` | 1 | `Task not found: ${taskId}` | — |
| `AgentNotFoundError` | 1 | `Agent not found: ${agentId}` | — |
| `TaskAlreadyRunningError` | 1 | `Task ${taskId} is already running (run: ${runId}, agent: ${agentName})` | `Use: orch logs --task ${taskId} --follow` |
| `InvalidTransitionError` | 1 | `Invalid transition for ${taskId}: ${from} → ${to}` | — |
| `GoalNotFoundError` | 1 | `Goal not found: ${goalId}` | — |
| `GoalHasPendingTasksError` | 1 | `Cannot mark goal ${goalId} as achieved: ${count} task(s) still pending — ${summary}` | `Use --force to cancel pending tasks and mark achieved` |
| `TeamNotFoundError` | 1 | `Team not found: ${teamId}` | — |
| `MessageNotFoundError` | 1 | `Message not found: ${messageId}` | — |
| `WorkspaceError` | 6 | ctor arg | optional |

### 3.3 Rendering

Every error is rendered by `printError(message, hint?)` to **stderr**:

```
  ✕ Not initialized
    Run: orch init
```

Line 1 = `"  "` + red failed icon + `" "` + message. Line 2 (only when a hint exists) = 4 spaces + hint.

### 3.4 Global boundary

```ts
main().catch((err) => {
  if (err instanceof OrchestryError) { printError(err.message, err.hint); process.exit(err.exitCode); }
  printError(err instanceof Error ? err.message : String(err));
  if (process.env['ORCHESTRY_DEBUG']) { console.error(err); }
  process.exit(1);
});
```

`ORCHESTRY_DEBUG` (any truthy value) additionally dumps the raw error object via `console.error`.

### 3.5 `NotInitializedError` special case

```ts
catch (err) {
  if (err instanceof NotInitializedError) {
    if (sub === 'doctor') { registerDoctorCommand(program); }           // container-less
    if (sub === 'init' || sub === 'doctor' || sub === 'update') {
      await program.parseAsync(process.argv); return;                   // allowed to run
    }
    printError(err.message, err.hint);
    process.exit(err.exitCode);                                        // 3
  }
  throw err;
}
```

Because this happens during container construction — i.e. **before** Commander parses — every other
command in an uninitialized directory exits 3, including `--help` on a subcommand and unknown
commands. Verified in an empty dir:

```
$ orch task list      ->  "  ✕ Not initialized" / "    Run: orch init"   EXIT=3
$ orch logs --help    ->  same                                           EXIT=3
$ orch frobnicate     ->  same                                           EXIT=3
$ orch --nope         ->  same                                           EXIT=3
```

### 3.6 Commander-level failures (initialized directory, verified)

| Situation | Output (stderr unless noted) | Exit |
|---|---|---|
| unknown command | `error: unknown command 'frobnicate'` | 1 |
| unknown global option | `error: unknown option '--nope'` | 1 |
| unknown subcommand option | `error: unknown option '--bogus'` | 1 |
| missing required option | `error: required option '--adapter <adapter>' not specified` | 1 |
| group invoked with no subcommand (`orch task`) | that group’s help, on **stdout** | 1 |

### 3.7 Per-command exit codes (consolidated, verified)

| Command / situation | Exit | Mechanism |
|---|---|---|
| success (all commands) | 0 | — |
| `logs` with no run-id/`--task`/`--agent`/`--since` | 2 | `printError(...)` + `process.exit(2)` |
| `logs --since <bad>` | 2 | `InvalidArgumentsError` → boundary |
| `run` with no task-id/`--all`/`--watch` | 2 | `printError(...)` + `process.exit(2)` |
| `run <task-id>` whose run fails | 1 | `process.exitCode = 1` (set in the `agent:completed` handler) |
| `run <task-id>` succeeding | 0 | exitCode untouched |
| `serve --log-format <other>` | 2 | `process.exitCode = 2` + early return |
| `serve --once` with any `failed` task | 1 | `process.exitCode = 1` |
| `serve --once` otherwise | 0 | `process.exitCode = 0` |
| `init --adapter <invalid>` | 2 | `process.exitCode = 2`, nothing written |
| `init` when already initialized | 0 | `printWarning('Already initialized')` |
| `goal status <id> <invalid>` | 1 | `process.exitCode = 1` |
| `org deploy <unknown>` | 1 | `process.exitCode = 1` |
| `org deploy` shop-template missing / agent create throws | 1 | `process.exitCode = 1` (no rollback) |
| `update` registry unreachable | 1 | `process.exit(1)` |
| `update` install failed | 1 | `process.exit(1)` |
| `context get <missing>` | **0** | `printError` only |
| `config global set` invalid value / unknown key | **0** | `printError` + `return` |
| `task show`/`agent status`/`goal show`/`team show` with a missing id | 1 | service throws `*NotFoundError` |
| `task assign` to a missing agent, duplicate agent name, etc. | 2 | `InvalidArgumentsError` |
| `$EDITOR` exits non-zero | 1 | `Error('Editor exited with code N')` → boundary |
| `doctor` with failing checks | **0** | doctor never sets a non-zero code |
| id failing `sanitizeId` | 1 | `Error('Invalid identifier: "…"')` |

---

## 4. `task` — tier L (LightContainer)

Group: `program.command('task').description('Manage tasks')`.

### 4.1 Subcommands and flags

| Subcommand | Positionals | Options (long, short) | Type | Default |
|---|---|---|---|---|
| `task add <title>` | `title` required | `-d, --description <desc>` “Task description” | string | — |
| | | `-p, --priority <n>` “Priority (1-4)” | string→`parseInt(,10)` | **`'3'`** |
| | | `-l, --labels <labels>` “Comma-separated labels” | CSV | — |
| | | `--depends-on <ids>` “Comma-separated dependency task IDs” | CSV | — |
| | | `--max-attempts <n>` “Max retry attempts” | string→`parseInt` | — |
| | | `--workspace-mode <mode>` “Workspace mode: shared\|worktree\|isolated” | string | — |
| | | `--assignee <agent-id>` “Assign to agent” | string | — |
| | | `--review-criteria <criteria>` “Comma-separated auto-review criteria: test_pass,typecheck,lint” | CSV | — |
| | | `--scope <patterns>` “Comma-separated glob patterns for file scope (e.g. src/auth/\*\*,src/session/\*\*)” | CSV | — |
| | | `--goal-id <goalId>` “Associate task with a goal” | string | — |
| | | `--attach <paths>` “Comma-separated file paths to attach (screenshots, docs)” | CSV | — |
| | | `-e, --edit` “Open $EDITOR to write the description” | boolean | `false` |
| `task list` | — | `--status <status>` “Filter by status” | string | — |
| `task show <id>` | `id` | — | | |
| `task edit <id>` | `id` | — | | |
| `task assign <task-id> <agent-id>` | both required | — | | |
| `task cancel <id>` | `id` | — | | |
| `task approve <id>` | `id` | — | | |
| `task reject <id>` | `id` | `-r, --reason <reason>` “Feedback for the agent explaining what to fix” | string | — |
| `task retry <id>` | `id` | — | | |

CSV options are parsed as `opts.x?.split(',').map((s) => s.trim())`.

Subcommand descriptions: `Create a new task`, `List all tasks`, `Show task details`,
`Open task in $EDITOR to modify title, priority and description`, `Assign task to agent`,
`Cancel a task`, `Approve a task in review`, `Reject a task and send it back for rework`,
`Retry a failed task`.

Verified `orch task` (no subcommand) help, exit 1:

```
Usage: orchestry task [options] [command]

Manage tasks

Options:
  -h, --help                   display help for command

Commands:
  add [options] <title>        Create a new task
  list [options]               List all tasks
  show <id>                    Show task details
  edit <id>                    Open task in $EDITOR to modify title, priority
                               and description
  assign <task-id> <agent-id>  Assign task to agent
  cancel <id>                  Cancel a task
  approve <id>                 Approve a task in review
  reject [options] <id>        Reject a task and send it back for rework
  retry <id>                   Retry a failed task
  help [command]               display help for command
```

### 4.2 `--json` shapes

`task add`, `task show` → one `Task`. `task list` → `Task[]`, empty `[]` (verified).
All use `JSON.stringify(x, null, 2)`.

```ts
// task add
if (container.context.json)      { console.log(JSON.stringify(task, null, 2)); }
else if (container.context.quiet){ console.log(task.id); }
else                             { printSuccess(`Created ${task.id} "${task.title}"`); }
```

Verified `orch --json task add "My task" -p 2 -l a,b`:

```json
{
  "id": "tsk_5TbyPRF",
  "title": "My task",
  "description": "",
  "status": "todo",
  "priority": 2,
  "labels": [
    "a",
    "b"
  ],
  "depends_on": [],
  "created_at": "2026-10-13T16:02:43.250Z",
  "updated_at": "2026-10-13T16:02:43.250Z",
  "attempts": 0,
  "max_attempts": 3
}
```

Full `Task` interface (keys appear in this declaration order; `undefined` optionals are omitted
by `JSON.stringify`):

```ts
interface Task {
  id: string; title: string; description: string;
  status: 'todo'|'in_progress'|'retrying'|'review'|'done'|'failed'|'cancelled';
  priority: number; assignee?: string;
  labels: string[]; depends_on: string[];
  created_at: string; updated_at: string;
  attempts: number; max_attempts: number;
  workspace_mode?: 'shared'|'worktree'|'isolated';
  workspace?: string;
  proof?: { branch?: string; pr_url?: string; files_changed: string[];
            test_results?: string; agent_summary?: string };
  review_criteria?: Array<'test_pass'|'typecheck'|'lint'>;
  review_results?: Array<{ criterion: 'test_pass'|'typecheck'|'lint'; passed: boolean; output: string }>;
  scope?: string[]; feedback?: string; goalId?: string; attachments?: string[];
}
```

Creation defaults (`TaskService.create`): `id = 'tsk_' + nanoid(7)`, `description = input.description?.trim() ?? ''`,
`status = 'todo'`, `priority = input.priority ?? config.defaults.task.priority`,
`labels = input.labels ?? []`, `depends_on = input.depends_on ?? []`,
`attempts = 0`, `max_attempts = input.max_attempts ?? config.defaults.task.max_attempts`,
`created_at = updated_at = new Date().toISOString()`.
Validation: empty title → `InvalidArgumentsError('Task title is required')`;
priority must be an integer 1–4 → `InvalidArgumentsError('Priority must be an integer between 1 and 4')`;
unknown dependency ids → `InvalidArgumentsError('Unknown depends_on task ID(s): …')`.

`task list`, `task show` have no `--quiet` handling other than:
```ts
if (container.context.quiet) { tasks.forEach((t) => console.log(t.id)); return; }  // task list only
```

### 4.3 Human output

**`task list`** — empty:
```
\n  No tasks. Create one: ${dim('orch task add "Title"')}\n
```
Table (verified):
```

  STATUS  PRI  TASK        AGENT  TIME
  ○ todo  P3   Quiet task  —      —

  2 tasks

```
Headers (contract): `STATUS`, `PRI`, `TASK`, `AGENT`, `TIME`.
Cells:
```ts
`${statusIcon(t.status)} ${t.status}`,
priorityLabel(t.priority),
t.title.slice(0, 35),
t.assignee ? agentName(t.assignee) : dim('—'),
time    // see below
```
```ts
const time = (t.status === 'in_progress' || t.status === 'done') && t.updated_at
  ? formatDurationSince(t.updated_at) : dim('—');
```
(asserted in `test/unit/cli/commands-task-time.test.ts`: done/in_progress use `updated_at`, never
`created_at`; todo/review/failed and done-without-`updated_at` show `dim('—')`.)
Footer:
```ts
`\n  ${tasks.length} tasks${running ? ` · ${running} running` : ''}${review ? ` · ${review} review` : ''}${done ? ` · ${done} done` : ''}\n`
```
where `running` counts `in_progress`, `review` counts `review`, `done` counts `done`.

**`task show`** (verified):
```

  Show me
  ══════════════════════════════════════════

  Status    ○ todo · attempt 0/3
  Priority  P3
  Created   2026-10-13T16:02:49.005Z

  Description
  ──────────────────────────────────────────
  A desc

```
Structure:
* blank line, `  ${t.title}`, `  ${'═'.repeat(42)}` (U+2550 × 42), blank line.
* `printKeyValue` pairs, pushed in this order, conditionals skipped when absent:
  `['Status', `${statusIcon(t.status)} ${t.status} · attempt ${t.attempts}/${t.max_attempts}`]`,
  `['Priority', priorityLabel(t.priority)]`,
  `['Agent', agentName(t.assignee)]`,
  `['Labels', t.labels.join(', ')]`,
  `['Scope', t.scope.join(', ')]`,
  `['Workspace', t.workspace_mode]`,
  `['Path', filePath(t.workspace)]`,
  `['Review', t.review_criteria.join(', ')]`,
  `['Feedback', t.feedback]`,
  `['Created', t.created_at]` (raw ISO).
* `Attachments (${n})` block: `\n  Attachments (${n})\n  ${'─'.repeat(42)}` then `    ${filePath(a)}` per entry.
* `Description` block: `\n  Description\n  ${'─'.repeat(42)}` then each description line prefixed `  `.
* `Result` block (when `t.proof`): `\n  Result\n  ${'─'.repeat(42)}` then, conditionally,
  `  Branch:        ${t.proof.branch}`,
  `  PR:            ${t.proof.pr_url}`,
  `  Files changed:` followed by `    • ${f}` per file,
  `  Test results:` followed by `    ${line}` per line,
  `  Agent summary:` followed by `    ${line}` per line.
  (The label padding above is literal in the source.)
* `Review Results` block: `\n  Review Results\n  ${'─'.repeat(42)}` then
  `  ${r.passed ? '✓' : '✗'} ${r.criterion}: ${r.passed ? 'passed' : 'failed'}` and, when `r.output`,
  each output line prefixed `    `. **These `✓`/`✗` are hard-coded, not `getIcon`, so `--ascii`
  does not change them.**
* trailing blank line.

**Mutations** (all via `printSuccess`, i.e. `  ✓ <text>` on stdout):

| Subcommand | Text |
|---|---|
| `task add` | `Created ${task.id} "${task.title}"` |
| `task edit` (no diff) | `  No changes.` (plain `console.log`, not printSuccess) |
| `task edit` (changed) | `Updated ${updated.id} "${updated.title}"` |
| `task assign` | `Assigned ${t.id} → ${t.assignee ?? agentId}` |
| `task cancel` | `Cancelled ${id}` |
| `task approve` | `Approved ${id}` |
| `task reject` | `Rejected ${id} → todo${opts.reason ? ` (reason: ${opts.reason})` : ''}` |
| `task retry` | `Reset ${id} to todo` |

`task edit` only sends changed fields:
```ts
if (parsed.title && parsed.title !== existing.title) fields.title = parsed.title;
if (parsed.priority && parsed.priority !== existing.priority) fields.priority = parsed.priority;
if (parsed.description !== undefined && parsed.description !== existing.description) {
  fields.description = parsed.description ?? '';
}
if (Object.keys(fields).length === 0) { console.log('  No changes.'); return; }
```
and seeds the buffer with an attachment comment:
```ts
const attachmentNote = existing.attachments?.length
  ? `\n# Attachments: ${existing.attachments.join(', ')}` : '';
```
Note this comment is **not** stripped by `fromEditorContent`, so if the user leaves it in place it
becomes part of the description.

`task approve` calls `taskService.updateStatus(id, 'done')`; `task reject` calls
`taskService.reject(id, opts.reason)`; `task retry` calls `taskService.retry(id)`.

---

## 5. `agent` — tier L

Group: `program.command('agent').description('Manage agents')`.

### 5.1 Subcommands and flags

| Subcommand | Positionals | Options |
|---|---|---|
| `agent add <name>` | `name` | **`.requiredOption('--adapter <adapter>', 'Adapter type: claude, opencode, codex, cursor, pi, grok, antigravity, shell')`**; `--role <role>` “Agent role description”; `--command <cmd>` “Shell command (for shell adapter)”; `--model <model>` “Model name (for AI adapters)”; `--effort <level>` “Reasoning effort: low, medium, high”; `--max-turns <n>` “Max turns per run”; `--timeout <ms>` “Timeout in ms”; `--approval-policy <policy>` “suggest\|auto\|manual”; `--workspace-mode <mode>` “shared\|worktree\|isolated”; `--skills <skills>` “Comma-separated list of agent skills”; `-e, --edit` “Open $EDITOR to write the role description” |
| `agent shop` | — | `--list` “Print all templates (non-interactive)” |
| `agent list` | — | — |
| `agent status <id>` | `id` | — |
| `agent edit <id>` | `id` | — |
| `agent remove <id>` | `id` | — |
| `agent disable <id>` | `id` | — |
| `agent enable <id>` | `id` | — |
| `agent autonomous <id>` | `id` | `--on` “Enable autonomous mode”; `--off` “Disable autonomous mode” |

Descriptions: `Add a new agent`, `Browse and install pre-built agent templates`, `List all agents`,
`Show agent details`, `Edit an agent in $EDITOR`, `Remove an agent`, `Disable an agent`,
`Enable an agent`, `Toggle autonomous mode for an agent`.

`agent autonomous` tri-state:
```ts
const enable = opts.on ? true : opts.off ? false : !current.autonomous;
```

`--skills` parsing: `opts.skills ? opts.skills.split(',').map((s) => s.trim()) : undefined`.
`--max-turns`/`--timeout`: `parseInt(x, 10)` when present.

### 5.2 `--json` shapes

`agent add`, `agent status`, `agent edit`, `agent autonomous` → one `Agent`.
`agent list` → `Agent[]`, empty `[]`. `agent shop` has **no** JSON mode. All `, null, 2`.

Verified `orch --json agent add "T2" --adapter shell --command "echo hi"`:

```json
{
  "id": "agt_irdYX8h",
  "name": "T2",
  "adapter": "shell",
  "config": {
    "command": "echo hi",
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

```ts
interface Agent {
  id: string; name: string; adapter: string; role?: string;
  config: { command?: string; model?: string;
            effort?: 'low'|'medium'|'high';
            approval_policy?: 'suggest'|'auto'|'manual';
            max_turns?: number; timeout_ms?: number; stall_timeout_ms?: number;
            env?: Record<string,string>; system_prompt?: string;
            workspace_mode?: 'shared'|'worktree'|'isolated'; skills?: string[] };
  status: 'idle'|'running'|'error'|'disabled';
  current_task?: string; autonomous?: boolean;
  stats: { tasks_completed: number; tasks_failed: number; total_runs: number;
           total_runtime_ms: number; tokens_used?: number };
  last_error?: { message: string; kind: string; timestamp: string };
}
```

`AgentService.create` behaviour: `id = 'agt_' + nanoid(7)`; `name` trimmed and required
(`InvalidArgumentsError('Agent name is required')`); `adapter = input.adapter?.trim() || config.defaults.agent.adapter`;
shell agents require a command (`InvalidArgumentsError('Shell agents require a command')`);
duplicate names rejected (`InvalidArgumentsError('Agent "X" already exists')`);
for shell adapters `model` and `effort` are forced to `undefined`;
`approval_policy = input.approval_policy ?? (isShell ? 'auto' : config.defaults.agent.approval_policy)`;
`max_turns`/`timeout_ms`/`stall_timeout_ms` fall back to the config defaults; `status = 'idle'`;
stats all zero.

`agent remove` of a genuinely running agent → `InvalidArgumentsError('Cannot remove a running agent. Stop it first.')`
(if `status === 'running'` but no matching entry in `state.running`, the status is reset to `idle`
and the delete proceeds).

### 5.3 Human output

**`agent list`** — empty:
```
\n  No agents. Add one: ${dim('orch agent add <name> --adapter <adapter>')}\n
```
Verified table:
```

  STATUS  AGENT        ADAPTER  TASK  TIME
  ○ idle  Test Runner  shell    —     —
  ○ idle  T2           shell    —     —

  2 agents · 0 running · 0 tokens total

```
Headers: `STATUS`, `AGENT`, `ADAPTER`, `TASK`, `TIME`.
Cells: `${statusIcon(a.status)} ${a.status}`, `agentName(a.name)`, `a.adapter`,
`a.current_task ?? dim('—')`, **`dim('—')` (TIME is hard-coded, never computed)**.
Footer:
```ts
`\n  ${agents.length} agents · ${running} running · ${formatTokens(agents.reduce((sum, a) => sum + (a.stats.tokens_used ?? 0), 0))} tokens total\n`
```

**`agent status`** (verified):
```

  T2
  ══════════════════════════════════════════

  Adapter  shell
  Command  echo hi
  Status   ○ idle
  Policy   auto

  Stats
  ──────────────────────────────────────────
  Tasks completed  0
  Tasks failed     0
  Total runs       0
  Tokens used      0

```
Construction:
```ts
const pairs = [
  ['Adapter', `${a.adapter}${a.adapter !== 'shell' && a.config.model ? ` (${a.config.model})` : ''}`],
  ['Status',  `${statusIcon(a.status)} ${a.status}`],
  ['Policy',  a.config.approval_policy ?? 'auto'],
];
if (a.adapter === 'shell') pairs.splice(1, 0, ['Command', a.config.command ?? '—']);
else                       pairs.splice(2, 0, ['Effort',  a.config.effort ?? 'default']);
if (a.current_task)        pairs.push(['Task', a.current_task]);
if (a.role)                pairs.push(['Role', a.role]);
if (a.config.skills?.length) pairs.push(['Skills', a.config.skills.join(', ')]);
```
Then `\n  Stats\n  ${'─'.repeat(42)}` and a second `printKeyValue` with exactly
`Tasks completed`, `Tasks failed`, `Total runs`, `Tokens used` (`formatTokens(stats.tokens_used ?? 0)`).
Trailing blank line.

**Mutations** (`printSuccess`):

| Subcommand | Text |
|---|---|
| `agent add` / shop install | `Added agent ${agentName(a.name)} (${a.adapter}) → ${a.id}` |
| `agent edit` | `Updated agent ${agentName(updated.name)} (${updated.id})` |
| `agent remove` | `Removed agent ${id}` |
| `agent disable` | `Disabled agent ${id}` |
| `agent enable` | `Enabled agent ${id}` |
| `agent autonomous` | `Autonomous mode ${enable ? 'enabled' : 'disabled'} for agent ${agentName(a.name)} (${a.id})` |

`--quiet` for `agent add`, `agent edit`, `agent autonomous` prints the bare id; `agent list --quiet`
prints one id per line.

### 5.4 `agent shop`

```ts
if (opts.list || !process.stdout.isTTY) {
  printTable(['Key', 'Name', 'Tier', 'Skills'],
    AGENT_SHOP_TEMPLATES.map((t) => [t.key, t.name, t.tier, t.skills.slice(0, 2).join(', ')]));
  return;
}
const template = await pickFromShop(AGENT_SHOP_TEMPLATES);
if (!template) { console.log('  Cancelled.'); return; }
const input = templateToAgentInput(template, container.config.defaults.agent.adapter);
const a = await container.agentService.create(input);
printSuccess(`Added agent ${agentName(a.name)} (${a.adapter}) → ${a.id}`);
```

Verified `orch agent shop --list` (headers are `Key Name Tier Skills`, mixed case, unlike the
upper-case headers elsewhere):

```
  Key                   Name                  Tier      Skills
  backend-dev           Backend Developer     balanced  review, careful
  frontend-dev          Frontend Developer    balanced  design-review, review
  qa-engineer           QA Engineer           balanced  qa, testing-suite:generate-tests
  code-reviewer         Code Reviewer         capable   review, careful
  architect             Architect             capable   plan-eng-review, office-hours
  devops-engineer       DevOps Engineer       balanced  ship, canary
  bug-hunter            Bug Hunter            balanced  investigate, careful
  tech-writer           Technical Writer      balanced  document-release, review
  marketer              Marketer              balanced  office-hours
  content-creator       Content Creator       balanced  office-hours
  growth-hacker         Growth Hacker         balanced  office-hours, feature-dev:feature-dev
  security-auditor      Security Auditor      capable   review, careful
  performance-engineer  Performance Engineer  balanced  benchmark, investigate
  data-engineer         Data Engineer         balanced  careful, feature-dev:feature-dev
  fullstack-dev         Full-Stack Developer  balanced  review, design-review
```

---

## 6. `status` — tier L

`program.command('status').description('Show orchestrator status')`. No positionals, no options.

### 6.1 `--json`

```ts
if (container.context.json) { console.log(JSON.stringify({ tasks, agents, state }, null, 2)); return; }
```

Three top-level keys: `tasks` (`Task[]`), `agents` (`Agent[]`), `state` (`OrchestratorState`).

Verified `state` block:

```json
  "state": {
    "version": 1,
    "onboardingCompleted": false,
    "running": {},
    "claimed": {},
    "retry_queue": [],
    "stats": {
      "total_runs": 0,
      "total_tasks_completed": 0,
      "total_tasks_failed": 0,
      "total_tokens": {
        "input": 0,
        "output": 0,
        "reasoning": 0,
        "total": 0,
        "cache_read": 0,
        "cache_write": 0
      },
      "total_runtime_ms": 0
    }
  }
```

```ts
interface OrchestratorState {
  version: 1; pid?: number; started_at?: string; onboardingCompleted?: boolean;
  running: Record<string, { run_id: string; agent_id: string; task_id: string;
                            pid: number; started_at: string; last_event_at: string }>;
  claimed: Set<string>;                 // <-- serializes to {}
  retry_queue: Array<{ task_id: string; attempt: number; due_at: string; error: string }>;
  stats: { total_runs: number; total_tasks_completed: number; total_tasks_failed: number;
           total_tokens: { input: number; output: number; reasoning: number; total: number;
                           cache_read: number; cache_write: number };
           total_runtime_ms: number };
}
```

**Critical parity note:** `claimed` is an in-memory `Set<string>`, so `JSON.stringify` always emits
`"claimed": {}` — never an array, regardless of contents. (`StateStore.write` serializes it as
`Array.from(state.claimed)` on disk, but the CLI prints the in-memory object.)
`pid` and `started_at` appear only when set.

### 6.2 Human output

Verified:
```

orch · op2 · idle

  todo        2

```

Construction:
```ts
const runningCount = Object.keys(state.running).length;
const mode = state.pid ? 'watching' : 'idle';
const uptime = state.started_at ? formatDurationSince(state.started_at) : '';

console.log();
console.log(`${amber('orch')} · ${container.config.project.name} · ${mode}`);   // NO leading indent
console.log();

if (runningCount > 0) {
  console.log(`  ${'RUNNING'.padEnd(12)}${runningCount}${''.padEnd(20)}AGENTS  ${agents.length}`);
}
for (const [status, count] of Object.entries(counts)) {
  if (status !== 'in_progress') { console.log(`  ${dim(status.padEnd(12))}${count}`); }
}
```
(`''.padEnd(20)` produces 20 spaces, so the RUNNING line is
`"  RUNNING     " + count + 20 spaces + "AGENTS  " + n`.)
Status counts are iterated in `Object.entries` insertion order, i.e. first-seen order over the task list.

Running-task block (after a blank line), one line per `in_progress` task:
```ts
`  ${statusIcon('in_progress')} ${t.assignee ? agentName(t.assignee) : ''}  ${t.title.slice(0, 35).padEnd(37)}${time}  ${priorityLabel(t.priority)}`
```
with `time = formatDurationSince(t.updated_at)`.

Footer (after a blank line, only when non-empty):
```ts
const tt = state.stats.total_tokens;
const tokenParts = [];
if (tt.total > 0) {
  tokenParts.push(`↑${formatTokens(tt.input)}`);     // ↑
  tokenParts.push(`↓${formatTokens(tt.output)}`);    // ↓
  if (tt.reasoning > 0) tokenParts.push(`\u{1F9E0}${formatTokens(tt.reasoning)}`);  // 🧠
  tokenParts.push(`Σ${formatTokens(tt.total)}`);     // Σ
}
const footer = [uptime ? `up ${uptime}` : null,
                tokenParts.length > 0 ? tokenParts.join(' ') : null].filter(Boolean).join(' · ');
if (footer) { console.log(); console.log(`  ${dim(footer)}`); }
console.log();
```

`--quiet` is **not** honoured by `status`.

---

## 7. `logs` — tier L

`program.command('logs [run-id]').description('View run logs')`.

### 7.1 Flags

| Option | Description | Type |
|---|---|---|
| `--agent <agent-id>` | `Filter by agent` | string |
| `--task <task-id>` | `Filter by task` | string |
| `--follow` | `Live stream` | boolean |
| `--since <duration>` | `Filter by time (e.g. 5m, 1h)` | string |

Positional `[run-id]` optional. No defaults.

### 7.2 Routing

```ts
const sinceMs = opts.since ? parseDuration(opts.since) : undefined;

if (opts.follow)                { await followLive(container, { runId, taskId: opts.task, agentId: opts.agent }); }
else if (runId)                 { await showRunLogs(container, runId, sinceMs); }
else if (opts.task)             { await showTaskLogs(container, opts.task, sinceMs); }
else if (opts.agent)            { await showAgentLogs(container, opts.agent, sinceMs); }
else if (sinceMs !== undefined) { await showAllRecentLogs(container, sinceMs); }
else { printError('Specify a run ID, --task, --agent, or --since <duration>'); process.exit(2); }
```

`--follow` wins over everything; a positional run-id wins over `--task`/`--agent`.
`--since` is parsed (and may throw) even in follow mode, then ignored.

```ts
function parseDuration(input: string): number {
  const match = input.match(/^(\d+)(s|m|h|d)$/);
  if (!match) throw new InvalidArgumentsError(`Invalid duration: "${input}". Use format: 5m, 1h, 30s, 1d`);
  switch (unit) { case 's': return v*1_000; case 'm': return v*60_000;
                  case 'h': return v*3_600_000; case 'd': return v*86_400_000;
                  default: return v*60_000; }
}
```

### 7.3 Fetch sizes (asserted by tests)

| path | without `--since` | with `--since` |
|---|---|---|
| positional run-id | `readEventsTail(runId, 50)` | `readEventsTail(runId, 500)` then `filterBySince` |
| `--task` | all runs from `listForTask`, each `readEventsTail(id, 10)` | last **20** runs (`runs.slice(-20)`), each `readEventsTail(id, 500)` + filter |
| `--agent` | last **5** runs (`runs.slice(-5)`), each `readEventsTail(id, 5)` | last 5 runs, each `readEventsTail(id, 500)` + filter |
| `--since` only | n/a | `runService.listAll()` filtered by window, first **20** (`slice(0,20)`), each `readEventsTail(id, 500)` + filter |

All per-run reads use `Promise.all`. `readEvents` (full read) is never used by the CLI.

```ts
function filterBySince(events: RunEvent[], sinceMs: number | undefined): RunEvent[] {
  if (!sinceMs) return events;                                  // 0 also returns unfiltered
  const cutoff = Date.now() - sinceMs;
  return events.filter((e) => new Date(e.timestamp).getTime() >= cutoff);
}
```

`showAllRecentLogs` run-window predicate:
```ts
const startedAt  = new Date(run.started_at).getTime();
const finishedAt = run.finished_at ? new Date(run.finished_at).getTime() : Date.now();
return finishedAt >= cutoff || startedAt >= cutoff;             // unfinished runs always match
```

Display slices applied after the fetch: `--task` prints `events.slice(-10)`, `--agent`
`events.slice(-5)`, `--since`-only `events.slice(-10)`.

### 7.4 `--json` shapes — four different payloads

```ts
// showRunLogs
if (container.context.json) { console.log(JSON.stringify(events, null, 2)); return; }   // RunEvent[]
// showTaskLogs
if (container.context.json) { console.log(JSON.stringify(runs, null, 2)); return; }     // Run[]
// showAgentLogs
if (container.context.json) { console.log(JSON.stringify(runs, null, 2)); return; }     // Run[]
// showAllRecentLogs
if (container.context.json) { console.log(JSON.stringify(recentRuns, null, 2)); return; } // Run[]
```

`--task`, `--agent` and `--since`-only therefore emit **run metadata, not events**.

```ts
interface RunEvent { timestamp: string; type: RunEventType; data: unknown }
type RunEventType = 'agent_output'|'file_changed'|'command_run'|'tool_call'|'error'|'done';

interface Run {
  id: string; task_id: string; agent_id: string; attempt: number;
  status: 'preparing'|'running'|'succeeded'|'failed'|'timed_out'|'cancelled';
  started_at: string; finished_at?: string; workspace_path: string; prompt: string;
  pid?: number; error?: string;
  tokens?: { input: number; output: number; reasoning: number; total: number;
             cache_read: number; cache_write: number };
}
```

Verified `orch --json logs run_5F5P9K6`:
```json
[
  {
    "timestamp": "2026-10-13T16:03:45.086Z",
    "type": "agent_output",
    "data": "done"
  }
]
```
Empty → `[]`. `--follow` ignores `--json` entirely.

### 7.5 Human output

Event line:
```ts
function formatEvent(event: RunEvent): string {
  const time = new Date(event.timestamp).toLocaleTimeString('en-US',
    { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const icon = event.type === 'error' ? getIcon('failed') : getIcon('agentAction');
  const data = typeof event.data === 'string' ? event.data : JSON.stringify(event.data);
  return `  ${dim(time)}  ${icon} ${data.slice(0, 80)}`;
}
```
Time is **local** (e.g. `12:03:45` for a `16:03:45Z` timestamp in UTC−4).

| path | empty message | per-run header |
|---|---|---|
| run-id | `\n  No events for run ${runId}\n` | — (blank line, events, blank line) |
| `--task` | `\n  No runs for task ${taskId}\n` | `\n  Run ${run.id} · attempt ${run.attempt} · ${run.status}` |
| `--agent` | `\n  No runs for agent ${agentId}\n` | `\n  Run ${run.id} · task ${run.task_id} · ${run.status}` |
| `--since` only | `\n  No runs in the specified time window\n` | `\n  Run ${run.id} · task ${run.task_id} · agent ${run.agent_id} · ${run.status}` |

`--since`-only also skips runs with zero matching events and, when more than 20 runs matched,
prints `\n  ${dim(`(showing 20 of ${recentRuns.length} matching runs)`)}`. All paths end with a blank line.

Verified:
```
$ orch logs --task tsk_yXmm5f4

  Run run_5F5P9K6 · attempt 1 · succeeded
  12:03:45  ▸ done

$ orch logs --agent agt_dfdK5ZL

  Run run_5F5P9K6 · task tsk_yXmm5f4 · succeeded
  12:03:45  ▸ done

$ orch logs --since 1h

  Run run_5F5P9K6 · task tsk_yXmm5f4 · agent agt_dfdK5ZL · succeeded
  12:03:45  ▸ done
```

### 7.6 `--follow`

```ts
if (filter.runId)  runIds.add(filter.runId);
if (filter.taskId) { for (const r of await runService.listForTask(filter.taskId)) runIds.add(r.id); }
if (filter.agentId) agentIds.add(filter.agentId);
const hasFilter = runIds.size > 0 || agentIds.size > 0;

console.log(`\n  ${dim('Following live events...')} ${dim('(Ctrl+C to stop)')}\n`);

const unsub = container.eventBus.onAny((event) => {
  const time = new Date().toLocaleTimeString('en-US', { hour12:false, hour:'2-digit', minute:'2-digit', second:'2-digit' });
  if (hasFilter) {
    if ('runId' in event && runIds.size > 0 && !runIds.has(event.runId)) return;
    if ('agentId' in event && agentIds.size > 0 && !agentIds.has(event.agentId)) return;
  }
  switch (event.type) { ... }
});

await new Promise<void>((resolve) => {
  const cleanup = () => { unsub(); resolve(); };
  process.once('SIGINT', cleanup);
  process.once('SIGTERM', cleanup);
});
```

Rendered lines (only these 7 event types; everything else is silently dropped):

```
  {time}  ▸ {data.slice(0,80)}                    agent:output      (non-string data -> '')
  {time}  ▸ Modified {event.path}                 agent:file_changed
  {time}  ✕ {event.error}                         agent:error
  {time}  → Started {runId} (agent: {agentId})    agent:started
  {time}  ✓ DONE  {runId}                         agent:completed, success
  {time}  ✕ FAIL  {runId}                         agent:completed, failure
  {time}  ↻ RETRY  attempt {n} · next in {s}s     run:retry, s = Math.round(delay_ms/1000)
  {time}  ⚠ STALL  {runId}                        orchestrator:stall_detected
```

`process.once` (not `on`) is used for both signals — asserted by
`test/unit/cli/commands-logs.test.ts`.

---

## 8. `config` — tier L

Group: `program.command('config').description('Manage configuration')`.
Nested group: `config.command('global').description('Manage global settings (~/.orchestry/global.yml)')`.

### 8.1 Subcommands

| Subcommand | Positionals | Description |
|---|---|---|
| `config get <key>` | key | `Get a config value (dot notation)` |
| `config set <key> <value>` | key, value | `Set a config value (dot notation)` |
| `config edit` | — | `Open config.yml in $EDITOR` |
| `config global get <key>` | key | `Get a global config value` |
| `config global set <key> <value>` | key, value | `Set a global config value` |
| `config global show` | — | `Show all global settings` |

No options on any of them.

### 8.2 Project config file — `.orchestry/config.yml`

`ConfigStore.read()` = `deepMerge(DEFAULT_CONFIG, yamlOrEmpty)` — recursive merge for plain objects,
arrays replaced wholesale.

Complete key space with defaults (`src/domain/config.ts`):

```
project.name                        'my-project'     (init overwrites with --name or basename(cwd))
project.description                 (optional, no default)
defaults.agent.adapter              'claude'         (init overwrites with the chosen adapter)
defaults.agent.approval_policy      'auto'           'suggest' | 'auto' | 'manual'
defaults.agent.max_turns            50
defaults.agent.timeout_ms           3600000
defaults.agent.stall_timeout_ms     600000
defaults.agent.workspace_mode       'worktree'       'shared' | 'worktree' | 'isolated'
defaults.task.max_attempts          3
defaults.task.priority              3
scheduling.poll_interval_ms         10000
scheduling.max_concurrent_agents    6
scheduling.retry_base_delay_ms      10000
scheduling.retry_max_delay_ms       300000
prompt.template                     (optional)
prompt.system_template              (optional)
prompt.user_template                (optional)
```

`get` / `set` semantics:

```ts
function getByPath(obj, keyPath) {           // returns undefined on any non-object hop
  let current = obj;
  for (const key of keyPath.split('.')) {
    if (current === null || current === undefined || typeof current !== 'object') return undefined;
    current = current[key];
  }
  return current;
}
function setByPath(obj, keyPath, value) {    // AUTO-VIVIFIES intermediate objects
  const keys = keyPath.split('.');
  let current = obj;
  for (let i = 0; i < keys.length - 1; i++) {
    const key = keys[i]!;
    if (typeof current[key] !== 'object' || current[key] === null) current[key] = {};
    current = current[key];
  }
  current[keys[keys.length - 1]!] = value;
}
```

So `config get` never errors on an unknown key (returns `undefined`), and `config set` accepts and
persists **arbitrary** keys, rewriting the whole merged config as YAML.

CLI value parsing:
```ts
let parsed: unknown;
try { parsed = JSON.parse(value); } catch { parsed = value; }
await container.configStore.set(key, parsed);
printSuccess(`${key} = ${JSON.stringify(parsed)}`);
```
Verified: `10` → number `10`, `true` → boolean `true`, `hello-world` → string (asserted in
`test/unit/cli/commands-config.test.ts`).

### 8.3 Global config file — `~/.orchestry/global.yml`

```ts
function globalConfigPath(): string {
  const override = process.env['ORCHESTRY_GLOBAL_CONFIG_PATH']?.trim();
  return override ? path.resolve(override) : path.join(homedir(), '.orchestry', 'global.yml');
}
```
Directory is created with `mkdir(..., { recursive: true })` on write.

```ts
export const DEFAULT_GLOBAL_CONFIG: GlobalConfig = {
  tui: { palette: 'amber', activity_filter: 'all', notifications: { toast: true, bell: false } },
};
export const TUI_PALETTE_NAMES = ['amber','ocean','forest','violet','light'] as const;
const VALID_FILTER_PRESETS: ActivityFilterPreset[] = ['all','text','tools','errors','events'];
```

Verified on-disk file after `config global set activity_filter errors`:
```yaml
tui:
  palette: amber
  activity_filter: errors
  notifications:
    toast: true
    bell: false
```

`read()` is defensive: an invalid `palette` falls back to `amber`; `activity_filter` is passed
through **unvalidated**; `notifications.toast`/`bell` must be booleans or fall back to the defaults.

CLI surface is narrower than the file:
```ts
// global get
const value = key === 'activity_filter' ? gc.tui.activity_filter
            : key === 'palette'         ? gc.tui.palette
            : undefined;
// global set
if (key === 'activity_filter') { if (!VALID_FILTER_PRESETS.includes(value)) { printError(`Invalid value "${value}". Valid: ${VALID_FILTER_PRESETS.join(', ')}`); return; } ... }
else if (key === 'palette')    { const palette = value.toLowerCase();
                                 if (!isTuiPaletteName(palette)) { printError(`Invalid value "${value}". Valid: ${TUI_PALETTE_NAMES.join(', ')}`); return; } ... }
else                           { printError(`Unknown global config key: ${key}`); }
```
`notifications` can only be changed from the TUI.

### 8.4 `--json` shapes (all **compact**, no indent)

```ts
// config get            ->  {"key":"...","value":...}
console.log(JSON.stringify({ key, value }));
// config global get     ->  {"key":"palette","value":"amber"}
console.log(JSON.stringify({ key, value }));
// config global show    ->  full GlobalConfig
console.log(JSON.stringify(gc));
```

Verified:
```
$ orch --json config get scheduling.poll_interval_ms
{"key":"scheduling.poll_interval_ms","value":10000}

$ orch --json config get nope.nope
{"key":"nope.nope"}                      <-- value is undefined, so the key is DROPPED

$ orch --json config global get palette
{"key":"palette","value":"amber"}

$ orch --json config global show
{"tui":{"palette":"amber","activity_filter":"all","notifications":{"toast":true,"bell":false}}}
```

`config set` and `config edit` have **no** JSON mode.

### 8.5 Human output (verified)

```
$ orch config get scheduling.poll_interval_ms
  scheduling.poll_interval_ms = 10000

$ orch config get nope.nope
  nope.nope = undefined                 <-- JSON.stringify(undefined) -> undefined -> "undefined"

$ orch config set scheduling.poll_interval_ms 5000
  ✓ scheduling.poll_interval_ms = 5000

$ orch config global show
  tui.activity_filter = all
  tui.palette = amber

$ orch config global get palette
  palette = "amber"

$ orch config global set palette solarized
  ✕ Invalid value "solarized". Valid: amber, ocean, forest, violet, light     (exit 0)

$ orch config global set zzz 1
  ✕ Unknown global config key: zzz                                           (exit 0)

$ orch config global set activity_filter errors
  ✓ activity_filter = errors
```

Formats: `get` → `  ${dim(key)} = ${JSON.stringify(value)}`;
`global show` → `  ${dim('tui.activity_filter')} = ${gc.tui.activity_filter}` and
`  ${dim('tui.palette')} = ${gc.tui.palette}` (raw, not JSON-quoted).

`config edit` spawns the editor directly and prints nothing:
```ts
const editor = process.env['EDITOR'] || process.env['VISUAL'] || 'vi';
const parts = editor.split(/\s+/);
const child = spawn(parts[0]!, [...parts.slice(1), container.paths.configPath], { stdio: 'inherit' });
// non-zero close code -> reject(new Error(`Editor exited with code ${code}`))
```

---

## 9. `context` — tier L

Group description: `Shared context store for inter-agent data exchange`.

### 9.1 Subcommands

| Subcommand | Positionals | Options | Description |
|---|---|---|---|
| `context set <key> <value>` | key, value | `--ttl <ms>` “Time-to-live in milliseconds” (`parseInt(,10)`) | `Set a shared context entry` |
| `context get <key>` | key | — | `Get a shared context entry` |
| `context list` | — | — | `List all shared context entries` |
| `context delete <key>` | key | — | `Delete a shared context entry` |

Keys are used as filenames (`.orchestry/context/<key>.json`) and so must satisf`sanitizeId`
(`/^[A-Za-z0-9._-]+$/`).

### 9.2 Entry shape

```ts
interface ContextEntry {
  key: string; value: string; created_at: string; updated_at: string;
  ttl_ms?: number; expires_at?: string;
}
```

### 9.3 `--json`

```ts
// set: re-reads and prints the stored entry
if (container.context.json) { const entry = await container.contextStore.get(key);
                              console.log(JSON.stringify(entry, null, 2)); }
else if (container.context.quiet) { console.log(key); }
else { printSuccess(`Set context "${key}"`); }

// get, missing
if (!entry) { if (container.context.json) { console.log('null'); } else { printError(`Context key "${key}" not found`); } return; }

// list
if (container.context.json) { console.log(JSON.stringify(entries, null, 2)); return; }

// delete
if (!container.context.quiet && !container.context.json) { printSuccess(`Deleted context "${key}"`); }
```

Verified:
```
$ orch --json context set k2 v2 --ttl 60000
{
  "key": "k2",
  "value": "v2",
  "created_at": "2026-10-13T16:03:00.479Z",
  "updated_at": "2026-10-13T16:03:00.479Z",
  "ttl_ms": 60000,
  "expires_at": "2026-10-13T16:04:00.480Z"
}

$ orch --json context get nope
null

$ orch --json context list
[
  { "key": "k1", "value": "v1", "created_at": "...", "updated_at": "..." },
  { "key": "k2", "value": "v2", "created_at": "...", "updated_at": "...", "ttl_ms": 60000, "expires_at": "..." }
]
```
Empty list → `[]`. `context delete --json` prints **nothing at all**.

### 9.4 Human output

```
$ orch context set k1 v1
  ✓ Set context "k1"

$ orch context get k1

  k1 = v1

$ orch context get nope
  ✕ Context key "nope" not found            (exit 0)

$ orch context list

  KEY  VALUE  UPDATED  TTL
  k1   v1     0s       —
  k2   v2     0s       -60s

  2 entries

$ orch context delete k1
  ✓ Deleted context "k1"
```

`get` with a TTL also prints `  ${dim(`expires: ${entry.expires_at}`)}` between the value line and
the trailing blank line.

Empty list message: `\n  No shared context entries. Set one: ${dim('orch context set key value')}\n`.
`--quiet`: `set` → bare key; `get` → bare value; `list` → one `key=value` line per entry;
`delete` → nothing.

Table construction:
```ts
const headers = ['KEY', 'VALUE', 'UPDATED', 'TTL'];
const rows = entries.map((e) => [
  e.key,
  e.value.length > 50 ? e.value.slice(0, 47) + '...' : e.value,
  formatDurationSince(e.updated_at),
  e.expires_at ? formatDurationSince(e.expires_at) : dim('—'),
]);
console.log(); printTable(headers, rows); console.log(`\n  ${entries.length} entries\n`);
```
The TTL column is **negative** for a future `expires_at` because `formatDuration` is unclamped
(verified `-60s`).

---

## 10. `msg` — tier L

Group description: `Inter-agent messaging`.

### 10.1 Subcommands

| Subcommand | Positionals | Options |
|---|---|---|
| `msg send <to-agent-id> <body>` | both required | `-s, --subject <subject>` “Message subject”; `--from <agent-id>` “Sender agent ID (default: cli)”; `--ttl <ms>` “TTL in milliseconds”; `--reply-to <msg-id>` “Reply to a message” |
| `msg broadcast <body>` | body | `-s, --subject <subject>`; `--from <agent-id>` “Sender agent ID (default: cli)”; `--team <team-id>` “Limit broadcast to team members”; `--ttl <ms>` “TTL in milliseconds” |
| `msg inbox <agent-id>` | agent-id | — |
| `msg list` | — | `--agent <agent-id>` “Filter by agent (sent or received)” |

Descriptions: `Send a direct message to an agent`,
`Broadcast a message to all agents (or team members)`, `Show pending messages for an agent`,
`List all messages`.

`--from` default is applied in code (`opts.from ?? 'cli'`), not as a Commander default.
`--ttl` is `parseInt(opts.ttl, 10)` when present.

### 10.2 Message shape and service behaviour

```ts
interface Message {
  id: string; channel: 'direct'|'broadcast'|'lead';
  from_agent_id: string; to_agent_id: string | null;
  subject: string; body: string;
  created_at: string; expires_at?: string;
  status: 'pending'|'delivered'|'expired';
  delivered_at?: string; team_id?: string; reply_to?: string;
}
const MAX_MESSAGE_TTL_MS = 7 * 24 * 60 * 60 * 1000;      // 7 days
const DEFAULT_MESSAGE_TTL_MS = 24 * 60 * 60 * 1000;      // 24 hours
```

`MessageService.send` rules:
* empty body → `InvalidArgumentsError('Message body is required')`
* `ttlMs <= 0 || ttlMs > MAX_MESSAGE_TTL_MS` → `InvalidArgumentsError(`TTL must be between 1ms and ${MAX_MESSAGE_TTL_MS}ms`)`
* unknown sender (other than the literal `'cli'`) → `InvalidArgumentsError(`Sender agent not found: ${id}`)`
* `subject: (input.subject || '(no subject)').slice(0, 200)` — **the literal fallback string is `(no subject)`**
* `body: input.body.slice(0, 4000)`
* `expires_at = new Date(now + ttlMs).toISOString()`, `status: 'pending'`
* direct: unknown recipient → `InvalidArgumentsError(`Recipient agent not found: ${id}`)`
* broadcast: one message per agent, excluding the sender and any agent whose `status === 'disabled'`;
  with `--team`, restricted to that team’s members
* `id = 'msg_' + nanoid(7)`

### 10.3 `--json`

All four print `JSON.stringify(x, null, 2)`. `send` and `broadcast` return **arrays**
(`Message[]` — one element for direct, N for broadcast); `inbox` and `list` return `Message[]`
(empty `[]`).

Verified `orch --json msg send agt_irdYX8h "b"` — note the key order produced by the object spread
(base fields first, then `id`, then `to_agent_id`):

```json
[
  {
    "channel": "direct",
    "from_agent_id": "cli",
    "subject": "(no subject)",
    "body": "b",
    "created_at": "2026-10-13T16:03:17.192Z",
    "expires_at": "2026-10-14T16:03:17.192Z",
    "status": "pending",
    "id": "msg_GX0tPQv",
    "to_agent_id": "agt_irdYX8h"
  }
]
```

### 10.4 Human output (verified)

```
$ orch msg send agt_irdYX8h "hello there" -s "Hi"
  ✓ Message sent: msg_ANKYStb → agt_irdYX8h

$ orch msg broadcast "all hands"
  ✓ Broadcast sent to 2 agent(s)

$ orch msg inbox agt_irdYX8h

  msg_ANKYStb from cli — Hi
  hello there


$ orch msg list

  ID           FROM  TO           CHANNEL  STATUS   SENT
  msg_ANKYStb  cli   agt_irdYX8h  direct   pending  0s

  1 message(s)

```

Formats:
* `send` → `printSuccess(`Message sent: ${messages[0]?.id} → ${toAgentId}`)`; `--quiet` → `messages[0]?.id`.
* `broadcast` → `printSuccess(`Broadcast sent to ${messages.length} agent(s)`)`; `--quiet` → `messages.map((m) => m.id).join('\n')`.
* `inbox` empty → `console.log(dim('\n  No pending messages.\n'))`. Otherwise a blank line then per message:
  `  ${dim(m.id)} from ${m.from_agent_id}${m.subject ? ` — ${m.subject}` : ''}`, `  ${m.body}`, blank.
* `list` empty → `console.log(dim('\n  No messages.\n'))`. Otherwise headers
  `['ID','FROM','TO','CHANNEL','STATUS','SENT']`, rows `m.id`, `m.from_agent_id`,
  `m.to_agent_id ?? '*'`, `m.channel`, `m.status`, `formatDurationSince(m.created_at)`;
  footer `\n  ${messages.length} message(s)\n`.

---

## 11. `goal` — tier L

Group description: `Manage goals`.

### 11.1 Subcommands

| Subcommand | Alias | Positionals | Options | Description |
|---|---|---|---|---|
| `goal add <title>` | — | title | `--description <desc>` “Goal description”; `--assignee <agentId>` “Assign to a specific agent” | `Create a new goal` |
| `goal list` | **`ls`** | — | `--status <status>` “Filter by status” | `List all goals` |
| `goal show <id>` | — | id | — | `Show goal details` |
| `goal status <id> <status>` | — | id, status | `--force` “Force transition: cancel pending tasks when marking achieved” | `Change goal status (active, paused, achieved, abandoned)` |
| `goal update <id>` | — | id | `--title <title>` “New title”; `--description <desc>` “New description”; `--assignee <agentId>` “New assignee (empty string to unassign)” | `Update goal fields` |
| `goal delete <id>` | **`rm`** | id | — | `Delete a goal` |

```ts
const GOAL_STATUSES = ['active', 'paused', 'achieved', 'abandoned'] as const;
if (!GOAL_STATUSES.includes(status)) {
  printError(`Invalid status "${status}". Valid: ${GOAL_STATUSES.join(', ')}`);
  process.exitCode = 1; return;
}
```
Terminal goal statuses: `achieved`, `abandoned`.

### 11.2 Goal shape

```ts
interface Goal {
  id: string; title: string; description: string;
  status: 'active'|'paused'|'achieved'|'abandoned';
  assignee?: string; created_at: string; updated_at?: string;
}
```

### 11.3 `--json`

```ts
// add / status / update  ->  single Goal (indent 2)
// list                   ->  Goal[]      (indent 2; empty [])
// show                   ->  spread + extras (indent 2)
console.log(JSON.stringify({ ...g, tasks, progress }, null, 2));
// delete                 ->  COMPACT
console.log(JSON.stringify({ deleted: id }));
```

Verified:
```
$ orch --json goal show goal_6Hcvju3
{
  "id": "goal_6Hcvju3",
  "title": "G2",
  "description": "",
  "status": "active",
  "created_at": "2026-10-13T16:03:15.863Z",
  "updated_at": "2026-10-13T16:03:15.863Z",
  "tasks": []
}

$ orch --json goal delete goal_6Hcvju3
{"deleted":"goal_6Hcvju3"}
```
`progress` is `undefined` when no `<goalId>-progress` context entry exists, so the key is dropped.
`progress` comes from `goalService.getProgressReport(goalId)` → `contextStore.get(`${goalId}-progress`)?.value`.
`tasks` is `Task[]` from `goalService.listTasksForGoal(id)`.

### 11.4 Human output

**`goal add`** (verified):
```
  ✓ Created goal "Ship v2" (goal_SCQoWOH)

  Tips for better results:
  • Be specific: "Implement OAuth2 with Google" > "Add auth"
  • Add --description with success criteria and constraints
  • Use --assignee to focus a specific agent on this goal
```
Source:
```ts
printSuccess(`Created goal "${g.title}" (${g.id})`);
console.log();
console.log(`  ${dim('Tips for better results:')}`);
console.log(`  ${dim('•')} Be specific: ${dim('"Implement OAuth2 with Google" > "Add auth"')}`);
console.log(`  ${dim('•')} Add ${dim('--description')} with success criteria and constraints`);
console.log(`  ${dim('•')} Use ${dim('--assignee')} to focus a specific agent on this goal`);
```

**`goal list`** — empty → `console.log(dim('No goals found.'))` (no indentation, no blank lines).
Otherwise (verified):
```
     ID            Title    Status  Assignee
  ●  goal_SCQoWOH  Ship v2  active  any
```
```ts
const STATUS_ICON: Record<GoalStatus, string> = {
  active: '●',     // ●
  paused: '‖',     // ‖
  achieved: '✓',   // ✓
  abandoned: '✕',  // ✕
};
printTable(['', 'ID', 'Title', 'Status', 'Assignee'], rows);
// rows: [STATUS_ICON[g.status] ?? '?', g.id, g.title, g.status, g.assignee ?? dim('any')]
```
The first header is the **empty string**, so the icon column width is the icon width.
These icons are a separate hard-coded map, **not** `statusIcon` — `--ascii` does not affect them.

**`goal show`** (verified):
```
  ID           goal_6Hcvju3
  Title        G2
  Status       active
  Assignee     any
  Description  none
  Created      2026-10-13T16:03:15.863Z
  Updated      2026-10-13T16:03:15.863Z

  No tasks linked to this goal yet.

```
```ts
printKeyValue([
  ['ID', g.id], ['Title', g.title], ['Status', g.status],
  ['Assignee', g.assignee ?? dim('any')],
  ['Description', g.description || dim('none')],
  ['Created', g.created_at],
  ['Updated', g.updated_at ?? dim('never')],
]);
```
With tasks: `\n  Tasks (${tasks.length})\n  ${'─'.repeat(42)}` then
`printTable(['STATUS','ID','TITLE','AGENT'], rows)` where rows are
`${statusIcon(t.status)} ${t.status}`, `t.id`, `t.title.slice(0, 40)`,
`t.assignee ? agentName(t.assignee) : dim('—')`.
Without tasks: `\n  ${dim('No tasks linked to this goal yet.')}`.
With progress: `\n  Progress Report\n  ${'─'.repeat(42)}` then each progress line prefixed `  `.
Trailing blank line.

**Mutations**: `goal status` → `printSuccess(`Goal "${g.title}" → ${g.status}`)`;
`goal update` → `printSuccess(`Updated goal "${g.title}"`)`;
`goal delete` → `printSuccess(`Deleted goal ${id}`)`. `--quiet` prints `g.id` for
add/status/update and nothing for delete.

---

## 12. `team` — tier L

Group description: `Manage agent teams`.

### 12.1 Subcommands

| Subcommand | Positionals | Options | Description |
|---|---|---|---|
| `team create <name>` | name | **`.requiredOption('--lead <agent-id>', 'Lead agent ID')`**; `--members <ids>` “Comma-separated member agent IDs”; `-d, --description <desc>` “Team description”; `--no-auto-claim` “Disable auto-claiming” | `Create a new team` |
| `team list` | — | — | `List all teams` |
| `team show <id>` | id | — | `Show team details` |
| `team join <team-id> <agent-id>` | both | — | `Add an agent to a team` |
| `team leave <team-id> <agent-id>` | both | — | `Remove an agent from a team` |
| `team add-task <team-id> <task-id>` | both | — | `Add a task to the team pool` |
| `team set-lead <team-id> <agent-id>` | both | — | `Transfer team lead to another member` |
| `team disband <id>` | id | — | `Disband a team` |

Verified `orch team create --help`:
```
Usage: orchestry team create [options] <name>

Create a new team

Options:
  --lead <agent-id>         Lead agent ID
  --members <ids>           Comma-separated member agent IDs
  -d, --description <desc>  Team description
  --no-auto-claim           Disable auto-claiming
  -h, --help                display help for command
```

```ts
const t = await container.teamService.create({
  name, description: opts.description, lead_agent_id: opts.lead,
  member_agent_ids: opts.members?.split(',').map((s) => s.trim()),
  config: { auto_claim: opts.autoClaim !== false },
});
```

### 12.2 Team shape

```ts
interface Team {
  id: string; name: string; description?: string;
  status: 'active'|'paused'|'disbanded';
  members: Array<{ agent_id: string; role: 'lead'|'member'; joined_at: string }>;
  task_pool: string[]; lead_agent_id: string;
  created_at: string; updated_at: string;
  config: { max_concurrent_tasks?: number; auto_claim: boolean; message_ttl_ms?: number };
}
export const DEFAULT_TEAM_CONFIG: TeamConfig = { auto_claim: true, message_ttl_ms: 24*60*60*1000 };
```

### 12.3 `--json`

`team create`, `team show` → one `Team`; `team list` → `Team[]` (empty `[]`); all with indent 2.
`join`, `leave`, `add-task`, `set-lead`, `disband` have **no** JSON mode.

### 12.4 Human output (verified)

```
$ orch team list

  No teams. Create one: orch team create <name> --lead <agent-id>

$ orch team create "Alpha" --lead agt_irdYX8h -d "desc"
  ✓ Created team "Alpha" → team_tvnjYxp

$ orch team list

  ID            NAME   STATUS  LEAD         MEMBERS  POOL
  team_tvnjYxp  Alpha  active  agt_irdYX8h  1        0

$ orch team show team_tvnjYxp

  ID          team_tvnjYxp
  Name        Alpha
  Status      active
  Lead        agt_irdYX8h
  Members     agt_irdYX8h (lead)
  Pool        empty
  Auto-claim  true
  Created     2026-10-13T16:03:16.572Z

```
Empty message: `console.log(dim(`\n  No teams. Create one: orch team create <name> --lead <agent-id>\n`))`.
Headers: `['ID','NAME','STATUS','LEAD','MEMBERS','POOL']` with `String(t.members.length)` and
`String(t.task_pool.length)`.
`show` pairs: `['ID',…]`, `['Name',…]`, `['Status',…]`, `['Lead',…]`,
`['Members', t.members.map((m) => `${m.agent_id} (${m.role})`).join(', ')]`,
`['Pool', t.task_pool.length > 0 ? t.task_pool.join(', ') : dim('empty')]`,
`['Auto-claim', String(t.config.auto_claim)]`, `['Created', t.created_at]`.

Mutations: `Created team "${t.name}" → ${t.id}` | `Agent ${agentId} joined team ${teamId}` |
`Agent ${agentId} left team ${teamId}` | `Task ${taskId} added to team ${teamId} pool` |
`${agentId} is now lead of team ${teamId}` | `Team ${id} disbanded`. `--quiet` affects only
`team create` (bare `t.id`).

---

## 13. `run` — tier **F** (full Container)

`program.command('run [task-id]').description('Run tasks')`.

### 13.1 Flags

| Option | Description |
|---|---|
| `--all` | `Run all todo tasks` |
| `--watch` | `Watch mode: continuous orchestration` |
| `--verbose` | `Include agent output in watch mode` |

```ts
if (opts.watch)      { await runWatch(container, opts.verbose ?? false); }
else if (opts.all)   { await runAll(container); }
else if (taskId)     { await runSingle(container, taskId); }
else { printError('Specify a task ID, --all, or --watch'); process.exit(2); }
```

No `--json`, no `--quiet` handling.

### 13.2 `run <task-id>`

```
(blank)
  {amber('orch')} · running {taskId} "{task.title}"
  {time}  ▸ {data.slice(0,80)}
  {time}  ▸ Modified {event.path}
  {time}  ✕ {event.error}
  ✓ Done                                    (printSuccess)
  ✕ Failed                                  (printError, + process.exitCode = 1)
(blank)
```

Time format: `new Date().toLocaleTimeString('en-US', { hour12:false, hour:'2-digit', minute:'2-digit', second:'2-digit' })`.
`agent:started` is used only to capture `targetRunId` (when `event.taskId === taskId`);
`agent:completed` is acted on only when `event.runId === targetRunId`. The subscription is kept
alive past `orchestrator.runTask()` returning unless no run was dispatched:
```ts
if (!targetRunId) { unsub(); }
```
and is always released on a throw.

### 13.3 `run --all`

```
(blank)
  {amber('orch')} · running all todo tasks
(blank)
```
then `await container.orchestrator.runAll();` (no further CLI-level output).

### 13.4 `run --watch`

Header (note: **no two-space indent** on these lines, unlike every other command):
```
{amber('orch')} · watching · poll interval {config.scheduling.poll_interval_ms / 1000}s
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━        ('━'.repeat(43), U+2501)
(blank)
```
Event lines (time here is `{ hour12:false, hour:'2-digit', minute:'2-digit' }` — **no seconds**):
```
{time}  ▸ {data.slice(0,60)}                   agent:output, only when --verbose
{time}  ✓ DONE  {runId}                        agent:completed success
{time}  ✕ FAIL  {runId}                        agent:completed failure
{time}  ↻ RETRY  attempt {n} · next in {s}s    run:retry
{time}  ⚠ STALL  {runId}                       orchestrator:stall_detected
\n{dim('Shutting down...')}                    orchestrator:shutdown
```
Tick status line (carriage-return rewrite, four trailing spaces):
```ts
process.stdout.write(`\r${amber('orch')} · watching · ${event.running} running · ${event.queued} queued    `);
```
Then:
```ts
await container.orchestrator.startWatch();
await container.orchestrator.waitForStop();
```

---

## 14. `doctor` — tier F (also works container-less)

`program.command('doctor').description('Check adapters and dependencies')`. No options.

### 14.1 Container-less fallback

```ts
if (container) { doctorService = container.doctorService; paths = container.paths; hasContainer = true; }
else {
  const pm = new ProcessManager();
  const registry = new AdapterRegistry();
  registry.register(new ClaudeAdapter(pm));
  registry.register(new ShellAdapter(pm));
  registry.register(new PiAdapter(pm));
  registry.register(new GrokAdapter(pm));
  registry.register(new AntigravityAdapter(pm));
  doctorService = new DoctorService(registry, pm, process.cwd());
  paths = new Paths(process.cwd());
}
```
So pre-init `doctor` checks only **5** adapters (claude, shell, pi, grok, antigravity); with a full
container it checks **8** in registration order (claude, codex, cursor, shell, opencode, pi, grok,
antigravity).

### 14.2 Checks (`DoctorService.runAll`)

1. **One check per registered adapter, sequentially** (`await adapter.test()`):
   `{ name: adapter.kind, status: result.ok ? 'ok' : 'fail', detail: result.ok ? result.version : result.error }`.
   `adaptersReady` counts the ok ones; `adaptersTotal = adapters.length`.
2. `git` — `checkCommand('git', ['--version'], 'git')`:
   ok → `detail = stdout.trim()`; fail → `detail = 'git: command not found'`.
3. `git repo` — `git rev-parse --is-inside-work-tree` with `cwd`:
   ok → `'git repository detected'`;
   fail → `'not a git repository — worktree/isolated modes will fail. Run: git init'`.
4. `.gitignore` — reads `<cwd>/.gitignore`:
   a line whose `.trim() === '.orchestry'` → ok, `'.orchestry is excluded'`;
   file present without the entry → fail,
   `'.orchestry not in .gitignore — worktrees will copy state recursively. Run: orch init'`;
   file missing → fail, `'no .gitignore found — .orchestry may be committed to git. Run: orch init'`.
5. `node` — `checkCommand('node', ['--version'], 'node')`; fail → `'node: command not found'`.

```ts
interface DoctorCheck { name: string; status: 'ok'|'fail'|'skip'; detail?: string }
interface DoctorReport { checks: DoctorCheck[]; adaptersReady: number; adaptersTotal: number }
```
`'skip'` exists in the type but is **never produced**; the renderer maps it to `dim('—')`.
There is **no warn state**.

### 14.3 `--json`

```ts
console.log();
console.log(`  ${amber('orch doctor')} · checking adapters and dependencies`);
console.log();
const report = await doctorService.runAll();
if (container?.context.json) { console.log(JSON.stringify(report, null, 2)); return; }
```

**The two header lines are printed before the JSON** (verified) — `doctor --json` stdout is not pure
JSON. Also, JSON mode is gated on `container?.context.json`, so container-less `doctor` always prints
text. The project-state line and the `N of M` footer are **not** part of the JSON (early return).

Shape:
```json
{
  "checks": [ { "name": "claude", "status": "ok",   "detail": "2.1.296 (Claude Code)" },
              { "name": "pi",     "status": "fail", "detail": "Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent" } ],
  "adaptersReady": 5,
  "adaptersTotal": 8
}
```

### 14.4 Human output

```ts
for (const check of report.checks) {
  const icon = check.status === 'ok'   ? chalk.ansi256(72)(getIcon('done'))
             : check.status === 'fail' ? chalk.ansi256(167)(getIcon('failed'))
             : dim('—');
  const detail = check.detail ? dim(` ${check.detail}`) : '';
  console.log(`  ${icon}  ${check.name.padEnd(12)}${detail}`);
}
if (paths) {
  const initialized = await paths.isInitialized();
  if (initialized && hasContainer) {
    const agents = await container!.agentService.list();
    const tasks  = await container!.taskService.list();
    console.log();
    console.log(`  ${chalk.ansi256(72)(getIcon('done'))}  .orchestry/   ${dim(`exists · ${agents.length} agents · ${tasks.length} tasks`)}`);
  } else if (!initialized) {
    console.log();
    console.log(`  ${chalk.ansi256(167)(getIcon('failed'))}  .orchestry/   ${dim('not found — run: orch init')}`);
  }
}
console.log();
console.log(`  ${report.adaptersReady} of ${report.adaptersTotal} adapters ready`);
console.log();
```
Note the **double space** after the icon and `padEnd(12)` on the name. The `.orchestry/` line is
omitted entirely when the project is initialized but no container was passed.

Verified full run:
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

  ✓  .orchestry/   exists · 8 agents · 0 tasks

  5 of 8 adapters ready

```

### 14.5 Exit code

**Always 0**, regardless of how many checks fail. `doctor` never sets `process.exitCode`.

---

## 15. `init` — tier N (no container)

`program.command('init').description('Initialize .orchestry/ in the current directory')`.

### 15.1 Flags

| Option | Description |
|---|---|
| `--name <name>` | `Project name` |
| `--adapter <adapter>` | `Default agent adapter (claude, opencode, codex, cursor, pi, grok, antigravity, shell)` |

**There is no `--force` and there are no presets.**

Verified help:
```
Usage: orchestry init [options]

Initialize .orchestry/ in the current directory

Options:
  --name <name>        Project name
  --adapter <adapter>  Default agent adapter (claude, opencode, codex, cursor,
                       pi, grok, antigravity, shell)
  -h, --help           display help for command
```

### 15.2 Validation

```ts
if (opts.adapter && !isAdapterKind(opts.adapter)) {
  printError(`Unknown adapter "${opts.adapter}"`, `Supported: ${SUPPORTED_ADAPTERS.join(', ')}`);
  process.exitCode = 2;
  return;
}
```
Verified (nothing is written):
```
$ orch init --adapter bogus
  ✕ Unknown adapter "bogus"
    Supported: claude, opencode, codex, cursor, pi, grok, antigravity, shell
EXIT=2
```

### 15.3 Idempotency

```ts
if (await pathExists(paths.root)) { printWarning('Already initialized'); return; }
```
Verified (exit 0, nothing written; the wrapper still prints a `Next:` line chosen by
`opts.adapter === 'shell'`, so a bare re-`init` in a shell project prints the `agt_creator` hint):
```
$ orch init
  ⚠ Already initialized
  Next: orch task add "Create backend agent" --assignee agt_creator

EXIT=0
```
Also asserted by `test/unit/cli/commands-init.test.ts`: when already initialized, no `ensureDir` and
no agent `writeYaml` calls occur.

### 15.4 Adapter detection (only when `--adapter` is omitted)

```ts
const checks = await Promise.all(
  SUPPORTED_ADAPTERS.filter((a) => a !== 'shell').map(async (name) => {
    const cmdsToTry = name === 'cursor' ? ['cursor-agent']
                    : name === 'antigravity' ? ['agy'] : [name];
    for (const cmd of cmdsToTry) {
      try { const { stdout } = await execFileAsync(cmd, ['--version'], { timeout: 5_000 });
            return { name, ok: true, version: stdout.trim().split('\n')[0] }; } catch {}
    }
    return { name, ok: false };
  }),
);
const available = checks.filter((c) => c.ok);
```
* 0 available → `  ${dim('No AI adapters detected — defaulting to claude')}`, returns `'claude'`.
* 1 available → `  ${dim(`Detected: ${available[0].name}`)} ${dim(version ? `(${version})` : '')}`, returns it.
* ≥2 and **not** (`process.stdout.isTTY && process.stdin.isTTY`) → returns `available[0].name` silently.
* ≥2 and TTY:
```
(blank)
  Available adapters:
    1) claude 2.1.296 (Claude Code)
    2) codex codex-cli 0.162.1
(blank)
  Choose default adapter [1-2]:
```
  (`readline.createInterface({ input: process.stdin, output: process.stdout })` + one `rl.question`).
  Out-of-range or non-numeric answers fall back to `available[0]`.

### 15.5 Directories created

`ensureDir` in parallel: `.orchestry/tasks`, `.orchestry/agents`, `.orchestry/goals`,
`.orchestry/runs`, `.orchestry/templates`, `.orchestry/logs`.
**Not** created at init time (created lazily on first write): `context/`, `messages/`, `teams/`,
`attachments/`. Verified tree:
```
.orchestry
.orchestry/agents
.orchestry/config.yml
.orchestry/.gitignore
.orchestry/goals
.orchestry/logs
.orchestry/runs
.orchestry/tasks
.orchestry/templates
.orchestry/templates/default.md
.orchestry/workspace-exclude
```

### 15.6 Git bootstrapping

```ts
async function ensureGitRepo(projectRoot) {
  try { await execFileAsync('git', ['rev-parse', '--is-inside-work-tree'], { cwd: projectRoot }); return true; }
  catch {
    try { await execFileAsync('git', ['init'], { cwd: projectRoot }); return true; }
    catch { return false; }   // git binary unavailable
  }
}
if (!gitAvailable) { config.defaults.agent.workspace_mode = 'shared'; }
...
async function ensureGitCommit(projectRoot) {
  try { await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: projectRoot }); }
  catch {
    try { await execFileAsync('git', ['add', '-A'], { cwd: projectRoot });
          await execFileAsync('git', ['commit', '-m', 'Initial commit', '--allow-empty'], { cwd: projectRoot }); }
    catch { /* no user.name/email — non-fatal */ }
  }
}
```
Asserted by the init tests: git unavailable → `workspace_mode: 'shared'` and no `git add`;
git available with no commits → `git add -A` then `git commit -m "Initial commit" --allow-empty`.

### 15.7 Files written

All via `atomicWrite` (temp `.<basename>.<8 hex>.tmp` + rename) or `writeYaml`
(`js-yaml.dump(data, { indent: 2, lineWidth: 120, noRefs: true, sortKeys: false })`).

**1. `.orchestry/config.yml`** — `structuredClone(DEFAULT_CONFIG)` with
`config.project.name = opts.name ?? path.basename(projectRoot)` and
`config.defaults.agent.adapter = chosenAdapter`. Verified for `init --adapter shell` in a dir named
`op2`:
```yaml
project:
  name: op2
defaults:
  agent:
    adapter: shell
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

**2. `.orchestry/.gitignore`** — exact bytes (trailing newline):
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

**3. `.orchestry/workspace-exclude`** — exact bytes (trailing newline):
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

**4. `.orchestry/templates/default.md`** — `DEFAULT_PROMPT_TEMPLATE` from
`src/infrastructure/template/template-engine.ts`, **4822 bytes**, a LiquidJS template beginning:
```
You are {{ agent.name }}{% if agent.role %} ({{ agent.role }}){% endif %}.

## Orchestrator CLI
Manage tasks and coordinate with other agents using `orch`:
...
```
with sections `## Orchestrator CLI`, `## Autonomous Goal Mode` (guarded by
`{% if task.is_autonomous %}`), `## Rules`, and a goal/progress block at the end.

**5. `.orchestry/agents/agt_creator.yml`** — `getDefaultAgents(chosenAdapter)`:
```ts
export function getDefaultAgents(adapter: string = 'claude'): Agent[] {
  if (adapter === 'shell') return [];                       // shell needs an explicit command
  const model = resolveModel(adapter, 'balanced');
  const skills = adapter === 'claude' ? ['document-skills:skill-creator'] : [];
  return [{
    id: 'agt_creator', name: 'Agent Creator', adapter, role: AGENT_CREATOR_ROLE,
    config: { model: model || undefined, approval_policy: 'suggest', max_turns: 50,
              timeout_ms: 3_600_000, stall_timeout_ms: 300_000, skills },
    status: 'idle',
    stats: { tasks_completed: 0, tasks_failed: 0, total_runs: 0, total_runtime_ms: 0 },
  }];
}
```
Asserted by tests: exactly one agent; `id === 'agt_creator'`; `name === 'Agent Creator'`;
default adapter `claude`; `shell` → `[]`; models `claude-sonnet-4-6` / `gpt-5.3-codex` /
`openai-codex/gpt-5.5`; MCP skills only for claude; `approval_policy === 'suggest'`;
`status === 'idle'`; zeroed stats; a fresh array on each call.

**6. Root `<project>/.gitignore`**:
```ts
try {
  const content = await fs.readFile(gitignorePath, 'utf-8');
  if (content.split('\n').some((line) => line.trim() === '.orchestry')) return;
  const separator = content.endsWith('\n') ? '' : '\n';
  await fs.appendFile(gitignorePath, `${separator}\n# Orchestry state\n.orchestry\n`);
} catch {
  await atomicWrite(gitignorePath, '# Orchestry state\n.orchestry\n');
}
```
Verified created file:
```
# Orchestry state
.orchestry
```

### 15.8 Output

Verified `orch init --adapter shell` in `/tmp/op2`:
```

  ✓ initialized

  Created .orchestry/
  ├── config.yml
  ├── tasks/
  ├── agents/
  ├── templates/default.md
  └── .gitignore

  Next: orch agent add "Test Runner" --adapter shell --command "npm test"

```
With a default agent (non-shell adapter) the tree gains one line per agent:
```ts
console.log(`  ${dim('├──')} agents/`);
for (const agent of defaultAgents) {
  console.log(`  ${dim('│   └──')} ${agent.id}.yml ${dim(`(${agent.name})`)}`);
}
```
i.e. `  │   └── agt_creator.yml (Agent Creator)`.

Exact source of the block:
```ts
console.log();
printSuccess('initialized');
console.log();
console.log(`  Created ${dim('.orchestry/')}`);
console.log(`  ${dim('├──')} config.yml`);
console.log(`  ${dim('├──')} tasks/`);
console.log(`  ${dim('├──')} agents/`);
// ...agent lines...
console.log(`  ${dim('├──')} templates/default.md`);
console.log(`  ${dim('└──')} .gitignore`);
console.log();
```
and then, from `registerInitCommand` (not `runInit`):
```ts
const next = opts.adapter === 'shell'
  ? 'orch agent add "Test Runner" --adapter shell --command "npm test"'
  : 'orch task add "Create backend agent" --assignee agt_creator';
console.log(`  Next: ${dim(next)}`);
console.log();
```
The FTUE auto-init path calls `runInit()` directly and therefore omits both `Next:` lines.

`init` has **no** `--json` and **no** `--quiet` handling.

---

## 16. `serve` — tier F

`program.command('serve').description('Headless daemon mode — structured logs to stdout')`.

### 16.1 Flags

| Option | Description | Type | Default |
|---|---|---|---|
| `--once` | `Process todo tasks and exit when all are terminal` | boolean | `false` |
| `--tick-interval <ms>` | `Override polling interval (ms)` | string→`parseInt` | — |
| `--log-file <path>` | `Also write logs to file (append mode)` | string | — |
| `--log-format <format>` | `Log format: json or text (default: json)` | string | **`'json'`** (Commander default) |
| `--verbose` | `Include high-frequency agent:output events` | boolean | `false` |

Verified help (note Commander renders the explicit default):
```
Usage: orchestry serve [options]

Headless daemon mode — structured logs to stdout

Options:
  --once                 Process todo tasks and exit when all are terminal
  --tick-interval <ms>   Override polling interval (ms)
  --log-file <path>      Also write logs to file (append mode)
  --log-format <format>  Log format: json or text (default: json) (default:
                         "json")
  --verbose              Include high-frequency agent:output events
  -h, --help             display help for command
```

### 16.2 Setup

```ts
const VALID_LOG_FORMATS: ReadonlySet<string> = new Set(['json', 'text']);
if (opts.logFormat && !VALID_LOG_FORMATS.has(opts.logFormat)) {
  printError(`Unknown --log-format "${opts.logFormat}". Valid: json, text`);
  process.exitCode = 2; return;
}
const format: LogFormat = opts.logFormat === 'text' ? 'text' : 'json';

const streams: Writable[] = [process.stdout];
if (opts.logFile) {
  fileStream = fs.createWriteStream(opts.logFile, { flags: 'a' });
  fileStream.on('error', (err) => { process.stderr.write(`Log file error: ${err.message}\n`); });
  streams.push(fileStream);
}

if (opts.tickInterval) {
  const ms = parseInt(opts.tickInterval, 10);
  if (!isNaN(ms) && ms > 0) { container.config.scheduling.poll_interval_ms = ms; }
}

const logger = new StructuredLogger({ format, verbose: opts.verbose ?? false, streams,
                                      idleLogInterval: IDLE_LOG_EVERY_N_TICKS /* 6 */ });
const unsub = logger.subscribe(container.eventBus);      // BEFORE startWatch
logger.log('info', 'serve:started', { mode: opts.once ? 'once' : 'watch', pid: process.pid,
                                      poll_interval_ms: container.config.scheduling.poll_interval_ms });
...
finally { unsub(); await logger.flush(); }
```
`--tick-interval abc` is silently ignored (asserted: config stays unchanged).
`flush()` ends every stream that is not `process.stdout`/`process.stderr`.
Subscription-before-startWatch is asserted by `test/unit/cli/commands-serve.test.ts`.

Background update check inside `serve` (the only place `update:available` / `update:installed` are logged):
```ts
if (info?.updateAvailable) {
  logger.log('warn', 'update:available',
    { current: info.current, latest: info.latest, hint: 'Run: npm install -g @oxgeneral/orch' });
  ...then(ok => { if (ok) logger.log('info', 'update:installed', { version: info.latest, hint: 'Restart to apply' }); });
}
```

### 16.3 Record shape

```ts
export type LogFormat = 'json' | 'text';
export type LogLevel  = 'debug' | 'info' | 'warn' | 'error';
export interface ServeEvent { ts: string; level: LogLevel; event: string; [key: string]: unknown }
```
`ts = new Date().toISOString()`, generated at transform time (not taken from the event).

**Synthetic records (`logger.log`)**

| `event` | `level` | fields (name: type) |
|---|---|---|
| `serve:started` | info | `mode: 'once'\|'watch'`, `pid: number`, `poll_interval_ms: number` |
| `serve:finished` | info | `result: 'all_done'\|'has_failed'`, `exit_code: 0\|1` |
| `update:available` | warn | `current: string`, `latest: string`, `hint: 'Run: npm install -g @oxgeneral/orch'` |
| `update:installed` | info | `version: string`, `hint: 'Restart to apply'` |

**Transformed records** (`event` always equals `event.type`)

| `event` | `level` | fields |
|---|---|---|
| `orchestrator:tick` | info | `running: number`, `queued: number`, `heap_mb: number` = `+(process.memoryUsage().heapUsed / 1_048_576).toFixed(1)` |
| `orchestrator:shutdown` | info | `reason: string` |
| `orchestrator:error` | `fatal ? 'error' : 'warn'` | `error: string`, `context: string`, `fatal: boolean` |
| `orchestrator:stall_detected` | warn | `runId: string` |
| `agent:started` | info | `agentId: string`, `taskId: string`, `runId: string` |
| `agent:completed` | `success ? 'info' : 'warn'` | `runId: string`, `agentId: string`, `success: boolean` |
| `agent:error` | error | `runId: string`, `agentId: string`, `error: string`, `errorKind: string \| undefined` |
| `agent:output` | debug | `runId: string`, `agentId: string`, `data: string` (`event.data.slice(0, 200)`); **returns `null` unless `verbose`** |
| `agent:file_changed` | info | `runId: string`, `agentId: string`, `path: string` |
| `run:retry` | warn | `runId: string`, `attempt: number`, `delay_ms: number` |
| `task:created` | info | `taskId: string` (= `event.task.id`), `title: string` |
| `task:status_changed` | info | `taskId: string`, `from: TaskStatus`, `to: TaskStatus` |
| `task:auto_reviewed` | info | `taskId: string`, `passed: boolean` (the `results[]` payload is **dropped**) |
| `workspace:merge_succeeded` | info | `taskId: string`, `branch: string` |
| `workspace:merge_conflict` | warn | `taskId: string`, `branch: string`, `conflictInfo: string` |
| `task:orphaned` | warn | `taskId: string` |
| `task:scope_overlap` | warn | `taskId: string`, `overlappingTaskId: string`, `patterns: string[]` |
| `task:cascade_failed` | warn | `taskId: string`, `failedDependencyId: string`, `reason: string` |

Every other `OrchestratorEvent` type hits `default: return null` and is **never logged**:
`task:assigned`, `message:sent`, `message:delivered`, `team:created`, `team:member_joined`,
`team:member_left`, `team:task_claimed`, `team:disbanded`, `team:task_added`,
`agent:autonomous_toggled`, `goal:created`, `goal:status_changed`, `goal:updated`, `goal:deleted`
(asserted by `structured-logger.test.ts` → “does not log team/goal/message events”).

**Idle tick throttling**
```ts
case 'orchestrator:tick': {
  this.tickCounter++;
  const isIdle = event.running === 0 && event.queued === 0;
  if (!this.opts.verbose && isIdle && this.tickCounter % this.opts.idleLogInterval !== 0) return null;
  ...
}
```
`idleLogInterval` is `IDLE_LOG_EVERY_N_TICKS = 6`. Non-idle ticks are always logged; with
`--verbose` every tick is logged. Asserted: with `idleLogInterval: 3`, six idle ticks produce two
records (counters 3 and 6).

### 16.4 Serialization

```ts
private write(entry: ServeEvent): void {
  const line = this.opts.format === 'json' ? JSON.stringify(entry) + '\n' : this.formatText(entry);
  for (const stream of this.opts.streams) { stream.write(line); }
}
private formatText(entry: ServeEvent): string {
  const time = entry.ts.slice(11, 23);                       // HH:MM:SS.mmm  (UTC!)
  const level = entry.level.toUpperCase().padEnd(5);
  const { ts: _ts, level: _level, event, ...rest } = entry;
  const fields = Object.entries(rest)
    .map(([k, v]) => `${k}=${typeof v === 'string' ? v : JSON.stringify(v)}`)
    .join(' ');
  return `${time} ${level} ${event}  ${fields}\n`;           // TWO spaces before fields
}
```
JSON field order follows the literal object-literal order in `transform()` / `log()`:
`ts, level, event, …`.

Verified json (`serve --once --tick-interval 1000`):
```json
{"ts":"2026-10-13T16:03:45.027Z","level":"info","event":"serve:started","mode":"once","pid":297270,"poll_interval_ms":1000}
{"ts":"2026-10-13T16:03:45.069Z","level":"info","event":"task:status_changed","taskId":"tsk_yXmm5f4","from":"todo","to":"in_progress"}
{"ts":"2026-10-13T16:03:45.079Z","level":"info","event":"agent:started","agentId":"agt_dfdK5ZL","taskId":"tsk_yXmm5f4","runId":"run_5F5P9K6"}
{"ts":"2026-10-13T16:03:45.080Z","level":"info","event":"orchestrator:tick","running":1,"queued":1,"heap_mb":10.9}
{"ts":"2026-10-13T16:03:45.094Z","level":"info","event":"agent:completed","runId":"run_5F5P9K6","agentId":"agt_dfdK5ZL","success":true}
{"ts":"2026-10-13T16:03:45.096Z","level":"info","event":"task:status_changed","taskId":"tsk_yXmm5f4","from":"in_progress","to":"review"}
{"ts":"2026-10-13T16:03:45.098Z","level":"info","event":"task:status_changed","taskId":"tsk_yXmm5f4","from":"review","to":"done"}
{"ts":"2026-10-13T16:03:47.089Z","level":"info","event":"serve:finished","result":"all_done","exit_code":0}
```

Verified text (`serve --once --log-format text --tick-interval 1000`):
```
16:03:53.143 INFO  serve:started  mode=once pid=297564 poll_interval_ms=1000
16:03:53.151 INFO  serve:finished  result=all_done exit_code=0
```
(UTC timestamps, `INFO ` padded to 5, two spaces before the field list.)

### 16.5 `--once` termination rule and exit code

```ts
export async function runOnce(orchestrator, taskStore, eventBus, pollIntervalMs = 2000): Promise<OnceResult> {
  await orchestrator.startWatch({ skipAutonomousSeeding: true });
  let orchestratorStopped = false;
  const unsubShutdown = eventBus.on('orchestrator:shutdown', () => { orchestratorStopped = true; });
  try {
    const finalTasks = await waitForAllTerminal(taskStore, pollIntervalMs, () => orchestratorStopped);
    await orchestrator.stop();
    return finalTasks.some((t) => t.status === 'failed') ? 'has_failed' : 'all_done';
  } finally { unsubShutdown(); }
}

async function waitForAllTerminal(taskStore, pollIntervalMs, isStopped): Promise<Task[]> {
  for (;;) {
    const tasks = await taskStore.list();
    if (tasks.length === 0) return tasks;                       // empty project exits immediately
    if (tasks.every((t) => isTerminal(t.status))) return tasks;  // {done, failed, cancelled}
    if (isStopped()) return tasks;                               // orchestrator gave up
    await new Promise((r) => { setTimeout(r, pollIntervalMs); });
  }
}
```
`TERMINAL_STATUSES = new Set(['done', 'failed', 'cancelled'])`.
**The 2000 ms poll interval is a hard-coded default argument and is NOT affected by `--tick-interval`**
(which only changes `config.scheduling.poll_interval_ms`, i.e. the orchestrator tick).

```ts
logger.log('info', 'serve:finished', { result, exit_code: result === 'has_failed' ? 1 : 0 });
process.exitCode = result === 'has_failed' ? 1 : 0;
```
Asserted: `[{done}]` → exitCode 0; `[{done},{failed}]` → exitCode 1; `[{done},{cancelled}]` →
`all_done`; `startWatch` receives `{ skipAutonomousSeeding: true }` and `stop()` is called.

Watch mode (no `--once`):
```ts
await container.orchestrator.startWatch();
await container.orchestrator.waitForStop();
```

`serve` has **no** `--json` and **no** `--quiet` handling, and is exempt from the background update
notification (it logs `update:available` instead).

### 16.6 Disk observer (`src/cli/serve/disk-observer.ts`)

Not reachable from `serve`; used by `tui` when `orchestrator.startWatch()` throws (another process
holds the lock). Constants:
```ts
const EVICTION_TTL_MS   = 5 * 60_000;    // keep offsets 5 min after a run leaves state
const MAX_READ_PER_TICK = 512 * 1024;    // bytes per JSONL file per tick
const MAX_REMAINDER_LEN = 64 * 1024;     // discard a partial line beyond this
this.pollIntervalMs = opts.pollIntervalMs ?? 1000;   // DEFAULT 1000 ms
```

`subscribe(handler)` runs `poll()` immediately, then `setInterval(poll, pollIntervalMs)`; the
returned function nulls the handler and clears the interval. `poll()` is re-entrancy-guarded by
`isPolling` and swallows every error; `emit()` wraps the handler in try/catch.

Per tick (`doPoll`):
1. `stateStore.read()`; on throw, return.
2. For each `state.running` entry: track `{runId, agentId, taskId, offset: 0, remainder: '', lastSeenAt}`
   (existing entries just refresh `lastSeenAt`). New runs emit
   `{ type: 'agent:started', agentId: entry.agent_id, taskId: entry.task_id, runId }`.
3. For each run in `prevRunning` not in the current set: tail its events one last time, read
   `.orchestry/runs/<id>.json`, then emit
   `{ type: 'agent:completed', runId, agentId: prevEntry.agent_id, success: run?.status === 'succeeded' }`
   and, if `run?.task_id`,
   `{ type: 'task:status_changed', taskId: run.task_id, from: 'in_progress', to: success ? 'review' : 'failed' }`.
4. Tail events for every still-running tracked run.
5. If `currentRunIds.size > 0`, emit `{ type: 'orchestrator:tick', running: currentRunIds.size, queued: 0 }`
   (**`queued` is always 0**).
6. Rebuild `prevRunning`; delete tracked runs absent from state for more than `EVICTION_TTL_MS`.

`tailRunEvents(tr)`:
```ts
fd = await open(this.opts.paths.runEventsPath(tr.runId), 'r');
const stat = await fd.stat();
if (stat.size <= tr.offset) return;
const bytesToRead = Math.min(stat.size - tr.offset, MAX_READ_PER_TICK);
const { bytesRead } = await fd.read(buf, 0, bytesToRead, tr.offset);
if (bytesRead === 0) return;
const chunk = tr.remainder + buf.subarray(0, bytesRead).toString('utf8');
const lines = chunk.split('\n');
const lastLine = lines.pop() ?? '';
if (chunk.endsWith('\n')) { tr.remainder = ''; tr.offset += bytesRead; }
else { tr.remainder = lastLine.length > MAX_REMAINDER_LEN ? '' : lastLine;
       const remainderBytes = tr.remainder ? Buffer.byteLength(lastLine, 'utf8') : 0;
       tr.offset += bytesRead - remainderBytes; }
for (const line of lines) { /* trim, skip blanks, JSON.parse in try/catch, translate, emit */ }
// catch: file not yet created -> ignore; finally: await fd?.close().catch(() => {})
```

`translateEvent(runEvent, tr)`:

| RunEvent type | emitted OrchestratorEvent |
|---|---|
| `agent_output` | `{ type: 'agent:output', runId, agentId, data }` (`JSON.stringify` when not a string) |
| `file_changed` | `{ type: 'agent:file_changed', runId, agentId, path }` where `path` is the string `data`, else `data.path`, else `''` |
| `error` | `{ type: 'agent:error', runId, agentId, error }` |
| `tool_call`, `command_run` | `{ type: 'agent:output', runId, agentId, data }` |
| `done` | `null` |
| default | `null` |

---

## 17. `tui` — tier F

`program.command('tui').description('Launch interactive TUI dashboard')`. No positionals, no options.

```ts
const { waitUntilExit } = render(createElement(App, { ...~45 props... }),
  { incrementalRendering: true, kittyKeyboard: { mode: 'auto', flags: ['disambiguateEscapeCodes'] } });
await waitUntilExit();
if (watchStarted) { await container.orchestrator.stop().catch(() => {}); }
if (diskObserver) { diskObserver.stop(); }
container.eventBus.clear();
```

Pre-render it loads `tasks`, `agents`, `state`, dynamically imports `ink` / `react` / `../../tui/App.js`,
fires a background `checkForUpdateSWR`, and then:

```ts
try { await container.orchestrator.startWatch(); watchStarted = true; }
catch (err) {
  watchError = err instanceof Error ? err.message : String(err);
  const { DiskObserver } = await import('../serve/disk-observer.js');
  diskObserver = new DiskObserver({ paths: container.paths, stateStore: container.stateStore });
  observerMode = true;
  onSubscribeEvents = (handler) => diskObserver!.subscribe(handler);
}
```
and passes `initialWatchActive: watchStarted`, `observerMode`,
`watchError: observerMode ? undefined : watchError`.

Settings wired to the stores: `initialPalette: container.globalConfig.tui.palette` /
`onSavePalette`; `initialActivityFilter` / `onSaveActivityFilter`;
`initialNotifications` / `onSaveNotifications`;
`initialMaxConcurrent: container.config.scheduling.max_concurrent_agents` /
`onSaveMaxConcurrent` (writes `scheduling.max_concurrent_agents` via `configStore.set`);
`onCompleteOnboarding` (sets `state.onboardingCompleted = true`);
`defaultAdapter: container.config.defaults.agent.adapter`; `version: program.version() ?? '0.0.0'`.

History loading is progressive: all runs are listed once, sorted by `started_at` descending,
filtered to `succeeded`/`failed`, first batch = 3 runs, second = next 7 (`TOTAL_RUNS = 10`),
`readEventsTail(run.id, 30)` each, each batch sorted ascending and capped at `slice(-200)`.

There is **no TTY guard** in the command. `tui` is exempt from the background update notification.
`tui` has no `--json`/`--quiet` handling.

---

## 18. `org` — tier L

Group description: `Pre-built AI companies — deploy a full department with one command`.

### 18.1 Subcommands

| Subcommand | Alias | Positionals | Options | Description |
|---|---|---|---|---|
| `org list` | **`ls`** | — | — | `List available company templates` |
| `org deploy <template>` | — | template | `--goal <goal>` “Set a goal for the team” | `Deploy a pre-built AI company` |

Verified `orch org --help`:
```
Usage: orchestry org [options] [command]

Pre-built AI companies — deploy a full department with one command

Options:
  -h, --help                   display help for command

Commands:
  list|ls                      List available company templates
  deploy [options] <template>  Deploy a pre-built AI company
  help [command]               display help for command
```

The source comment claims `org list` works before `orch init`, but `org` is a light command, so
`requireInit()` still runs — verified: in an uninitialized dir `org list` prints
`Not initialized` and exits 3.

### 18.2 `org deploy` algorithm

```ts
const template = getOrgTemplateByKey(templateKey);
if (!template) {
  printError(`Unknown template "${templateKey}"`, `Run: orch org list — to see available templates`);
  process.exitCode = 1; return;
}
for (const entry of template.agents) {
  const shopTemplate = getShopTemplateByKey(entry.shop_key);
  if (!shopTemplate) { printError(`Agent shop template not found: ${entry.shop_key}`); process.exitCode = 1; return; }
  try {
    const defaultAdapter = container.config.defaults.agent.adapter;
    const input = templateToAgentInput(shopTemplate, defaultAdapter);
    const agent = await container.agentService.create({ ...input, name: entry.name });
    agentIds.push(agent.id);
  } catch (err) {
    printError(`Failed to create agent "${entry.name}": ${err instanceof Error ? err.message : String(err)}`,
      agentIds.length > 0 ? `${agentIds.length} agent(s) were already created. Clean up with: orch agent list` : undefined);
    process.exitCode = 1; return;                        // NO ROLLBACK
  }
}
const leadId = agentIds[template.lead_index]!;
const memberIds = agentIds.filter((id) => id !== leadId);
const team = await container.teamService.create({ name: template.name, description: template.description,
                                                  lead_agent_id: leadId, member_agent_ids: memberIds });
if (opts.goal) { const goal = await container.goalService.create({ title: opts.goal, assignee: leadId }); goalId = goal.id; }
```

`templateToAgentInput`:
```ts
if (adapter === 'shell') {
  throw new InvalidArgumentsError('Agent Shop templates require an AI adapter; create shell agents with --command');
}
const model = resolveModel(adapter, template.tier);
const skills = adapter === 'claude' ? template.skills : template.skills.filter((s) => !s.includes(':'));
return { name: template.name, adapter, model: model || undefined, role: template.role, skills,
         approval_policy: template.approval_policy };
```
Verified consequence — in a shell-initialized project:
```
$ orch org deploy data-lab
  ✕ Failed to create agent "Lead Analyst": Agent Shop templates require an AI adapter; create shell agents with --command
EXIT=1
```

### 18.3 `--json`

```ts
// org list
if (container.context.json) { console.log(JSON.stringify(ORG_TEMPLATES, null, 2)); return; }
// org deploy
if (container.context.json) { console.log(JSON.stringify({ team, agentIds, goalId }, null, 2)); return; }
if (container.context.quiet) { console.log(team.id); return; }
```

Verified `org list --json` key order (declaration order):
```json
[
  {
    "key": "startup-mvp",
    "name": "Startup MVP",
    "description": "Ship an MVP in 48 hours",
    "lead_index": 0,
    "agents": [
      { "shop_key": "architect",   "name": "CTO" },
      { "shop_key": "backend-dev", "name": "Backend" },
      ...
    ]
  },
  ...
]
```
`org deploy --json` → `{ "team": Team, "agentIds": string[], "goalId": string }`; `goalId` is
omitted without `--goal` (undefined).

### 18.4 Human output

Verified `org list`:
```

  KEY              NAME                 AGENTS  DESCRIPTION
  startup-mvp      Startup MVP          6       Ship an MVP in 48 hours
  pr-review-corp   PR Review Corp       5       Automated review for every PR
  migration-squad  Migration Squad      6       JS-to-TS migration over a weekend
  security-dept    Security Department  5       Multi-layer security audit
  test-factory     Test Factory         6       Coverage from 40% to 80% overnight
  content-agency   Content Agency       5       Content factory: plan, write, edit, optimize
  data-lab         Data Lab             2       3 CSVs to executive report by morning
  sales-machine    Sales Machine        5       Outbound pipeline: research, outreach, follow-up, close
  bugfix-dept      Bugfix Department    6       100 issues to 0 in a week
  docs-team        Docs Team            5       Technical docs from codebase analysis

  Deploy: orch org deploy <key> --goal "Your objective"

```
Headers `['KEY','NAME','AGENTS','DESCRIPTION']`, `AGENTS = String(t.agents.length)`.
Footer `  ${dim('Deploy:')} orch org deploy <key> --goal "Your objective"`.

Verified `org deploy data-lab --goal "Analyze"`:
```

  ✓ Deployed team "Data Lab" — 2 agents

  ★ Lead Analyst (agt_tLBaA4v, lead)
  • Data Engineer (agt_JSDJ5S5, member)

  Team: team_WQI1EcW
  Goal: goal_qlZO7xJ — "Analyze"

  Next: orch run --all --watch

```
Source:
```ts
printSuccess(`Deployed team "${template.name}" — ${template.agents.length} agents`);
console.log();
for (let i = 0; i < template.agents.length; i++) {
  const isLead = i === template.lead_index;
  console.log(`  ${isLead ? '★' : '•'} ${entry.name} ${dim(`(${id}, ${isLead ? 'lead' : 'member'})`)}`);
}
console.log(`\n  Team: ${dim(team.id)}`);
if (goalId) { console.log(`  Goal: ${dim(goalId)} — "${opts.goal}"`); }
console.log();
console.log(`  ${dim('Next:')} orch run --all --watch`);
console.log();
```
`★` = U+2605, `•` = U+2022; both hard-coded (unaffected by `--ascii`).

### 18.5 ORG_TEMPLATES — 10 presets

All have `lead_index: 0`.

| key | name | agents | description |
|---|---|---|---|
| `startup-mvp` | Startup MVP | 6 | Ship an MVP in 48 hours |
| `pr-review-corp` | PR Review Corp | 5 | Automated review for every PR |
| `migration-squad` | Migration Squad | 6 | JS-to-TS migration over a weekend |
| `security-dept` | Security Department | 5 | Multi-layer security audit |
| `test-factory` | Test Factory | 6 | Coverage from 40% to 80% overnight |
| `content-agency` | Content Agency | 5 | Content factory: plan, write, edit, optimize |
| `data-lab` | Data Lab | 2 | 3 CSVs to executive report by morning |
| `sales-machine` | Sales Machine | 5 | Outbound pipeline: research, outreach, follow-up, close |
| `bugfix-dept` | Bugfix Department | 6 | 100 issues to 0 in a week |
| `docs-team` | Docs Team | 5 | Technical docs from codebase analysis |

Rosters (`shop_key` → display `name`, in creation order):

* **startup-mvp**: architect→`CTO`, backend-dev→`Backend`, backend-dev→`Backend 2`, frontend-dev→`Frontend`, qa-engineer→`QA`, code-reviewer→`Reviewer`
* **pr-review-corp**: architect→`CTO`, security-auditor→`Security`, performance-engineer→`Performance`, code-reviewer→`Style`, qa-engineer→`QA`
* **migration-squad**: architect→`CTO`, fullstack-dev→`Migrator`, fullstack-dev→`Migrator 2`, fullstack-dev→`Migrator 3`, qa-engineer→`QA`, code-reviewer→`Reviewer`
* **security-dept**: security-auditor→`Lead Auditor`, security-auditor→`Scanner`, security-auditor→`Secrets Auditor`, bug-hunter→`Hunter`, code-reviewer→`Reviewer`
* **test-factory**: qa-engineer→`Coverage Lead`, backend-dev→`Backend`, backend-dev→`Backend 2`, qa-engineer→`QA`, qa-engineer→`QA 2`, code-reviewer→`Reviewer`
* **content-agency**: marketer→`Strategist`, content-creator→`Writer`, content-creator→`Writer 2`, tech-writer→`Editor`, growth-hacker→`SEO`
* **data-lab**: data-engineer→`Lead Analyst`, data-engineer→`Data Engineer`
* **sales-machine**: marketer→`Sales Director`, content-creator→`SDR`, content-creator→`SDR 2`, content-creator→`Copywriter`, growth-hacker→`Growth Analyst`
* **bugfix-dept**: architect→`Triager`, bug-hunter→`Fixer`, bug-hunter→`Fixer 2`, bug-hunter→`Fixer 3`, qa-engineer→`QA`, code-reviewer→`Reviewer`
* **docs-team**: architect→`Docs Lead`, tech-writer→`Writer`, tech-writer→`Writer 2`, tech-writer→`Editor`, code-reviewer→`Reviewer`

### 18.6 AGENT_SHOP_TEMPLATES — 15 presets

```ts
interface AgentShopTemplate { key: string; name: string; description: string;
  tier: 'capable'|'balanced'|'fast'; approval_policy: ApprovalPolicy; skills: string[]; role: string }
```

| key | name | tier | approval_policy | description | skills |
|---|---|---|---|---|---|
| `backend-dev` | Backend Developer | balanced | auto | APIs, databases, backend services | review, careful, feature-dev:feature-dev, feature-dev:code-explorer |
| `frontend-dev` | Frontend Developer | balanced | auto | React, UI components, CSS, responsive design | design-review, review, feature-dev:feature-dev, feature-dev:code-explorer |
| `qa-engineer` | QA Engineer | balanced | auto | Test writing, coverage analysis, quality assurance, browser testing | qa, testing-suite:generate-tests, testing-suite:test-coverage |
| `code-reviewer` | Code Reviewer | capable | suggest | PR review with auto-fix, adversarial review, security checks | review, careful, feature-dev:code-reviewer, feature-dev:code-explorer |
| `architect` | Architect | capable | suggest | System design, architecture decisions, tech leadership | plan-eng-review, office-hours, feature-dev:code-architect, feature-dev:code-explorer |
| `devops-engineer` | DevOps Engineer | balanced | auto | CI/CD, infrastructure, deployment, monitoring | ship, canary, devops-automation:cloud-architect |
| `bug-hunter` | Bug Hunter | balanced | auto | Systematic debugging, root cause analysis, minimal fixes | investigate, careful, feature-dev:feature-dev, feature-dev:code-explorer |
| `tech-writer` | Technical Writer | balanced | auto | Documentation, READMEs, API docs, release notes | document-release, review, feature-dev:code-explorer |
| `marketer` | Marketer | balanced | auto | Marketing strategy, positioning, copy, campaigns | office-hours |
| `content-creator` | Content Creator | balanced | auto | Blog posts, articles, social media content | office-hours |
| `growth-hacker` | Growth Hacker | balanced | auto | Growth experiments, analytics, user acquisition | office-hours, feature-dev:feature-dev |
| `security-auditor` | Security Auditor | capable | suggest | Security scanning, vulnerability analysis, OWASP, guardrails | review, careful, guard, feature-dev:code-reviewer |
| `performance-engineer` | Performance Engineer | balanced | auto | Optimization, profiling, benchmarks, load testing | benchmark, investigate, feature-dev:feature-dev, feature-dev:code-explorer |
| `data-engineer` | Data Engineer | balanced | auto | Data pipelines, ETL, analytics, SQL | careful, feature-dev:feature-dev, feature-dev:code-explorer |
| `fullstack-dev` | Full-Stack Developer | balanced | auto | End-to-end development, frontend and backend | review, design-review, feature-dev:feature-dev, feature-dev:code-explorer |

Each also carries a long multi-paragraph `role` prompt (e.g. `BACKEND_DEV_ROLE` begins
“Backend engineer — builds APIs, services, database layers, and server-side business logic.”
with a numbered `## WORKFLOW`).

### 18.7 Tier → model resolution

```ts
export const MODEL_TIER_MAP: Record<AdapterKind, Record<ModelTier, string>> = {
  claude:      { capable: 'claude-opus-4-6', balanced: 'claude-sonnet-4-6', fast: 'claude-haiku-4-6' },
  opencode:    { capable: 'openrouter/anthropic/claude-opus-4.6', balanced: '', fast: 'openrouter/google/gemini-2.5-flash' },
  codex:       { capable: 'gpt-5.4', balanced: 'gpt-5.3-codex', fast: 'gpt-5-mini' },
  cursor:      { capable: 'auto', balanced: 'auto', fast: 'auto' },
  pi:          { capable: 'openai-codex/gpt-5.5', balanced: 'openai-codex/gpt-5.5', fast: 'openai-codex/gpt-5.5' },
  grok:        { capable: 'grok-build', balanced: 'grok-composer-2.5-fast', fast: 'grok-composer-2.5-fast' },
  antigravity: { capable: 'gemini-3-pro', balanced: '', fast: 'gemini-3-flash' },
  shell:       { capable: '', balanced: '', fast: '' },
};
export const SUPPORTED_ADAPTERS: readonly AdapterKind[] =
  ['claude','opencode','codex','cursor','pi','grok','antigravity','shell'];
```
`resolveModel(adapter, tier)` returns `''` for an unknown adapter. `isAdapterKind(v)` = `v in MODEL_TIER_MAP`.

---

## 19. `update` — tier N (no container)

`program.command('update').description('Check for updates and install the latest version')`.

### 19.1 Flags

`--check` — `Only check, do not install`. Verified help:
```
Usage: orchestry update [options]

Check for updates and install the latest version

Options:
  --check     Only check, do not install
  -h, --help  display help for command
```

### 19.2 update-check module (`src/cli/update-check.ts`)

```ts
const PACKAGE_NAME = '@oxgeneral/orch';
const CACHE_DIR = path.join(os.homedir(), '.orchestry');
const CACHE_FILE = path.join(CACHE_DIR, 'update-check.json');
const CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000;          // 4 hours
const REVALIDATE_AFTER_MS = CHECK_INTERVAL_MS * 0.75;  // 3 hours
const INSTALL_MARKER = path.join(CACHE_DIR, 'update-installed.json');
interface UpdateCache { latest: string; checked_at: number }
export interface UpdateInfo { current: string; latest: string; updateAvailable: boolean }
```
These paths use `os.homedir()` directly — **not** `ORCHESTRY_GLOBAL_CONFIG_PATH`.

Verified cache file: `{"latest":"1.0.34","checked_at":1791907441253}`, written atomically
(`${CACHE_FILE}.tmp.${process.pid}` → rename).

Network:
```ts
execFile('npm', ['view', PACKAGE_NAME, 'version', '--json'], { timeout: 5000 }, (err, stdout) => {
  if (err) return resolve(null);
  try { resolve(JSON.parse(stdout.trim()) as string); } catch { resolve(null); }
});
child.unref();     // never keeps the event loop alive
```

```ts
function compareSemver(a, b) {   // only the first 3 numeric components; missing -> 0
  const pa = a.split('.').map(Number), pb = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) { const diff = (pa[i] ?? 0) - (pb[i] ?? 0); if (diff !== 0) return diff > 0 ? 1 : -1; }
  return 0;
}
```
Pre-release tags are ignored / NaN-prone.

```ts
export async function checkForUpdateSWR(currentVersion: string): Promise<UpdateInfo | null> {
  try {
    const cached = await readCache();                             // null when older than 4h / corrupt
    if (!cached) return checkForUpdateNow(currentVersion).catch(() => null);   // cold start: AWAITS (<=5s)
    const age = Date.now() - cached.checked_at;
    if (age >= REVALIDATE_AFTER_MS) { checkForUpdateNow(currentVersion).catch(() => {}); }  // background
    return buildUpdateInfo(currentVersion, cached.latest);
  } catch { return null; }
}
export function printUpdateNotification(info: UpdateInfo): void {
  if (!info.updateAvailable) return;
  const msg = `\n  Update available: ${info.current} → ${info.latest}\n  Run: npm install -g ${PACKAGE_NAME}\n`;
  process.stderr.write(msg);
}
export function backgroundInstall(version: string): Promise<boolean> {
  // skip when INSTALL_MARKER has { version: same, installed_at } within CHECK_INTERVAL_MS
  execFile('npm', ['install', '-g', `${PACKAGE_NAME}@${version}`], { timeout: 60_000 }, (err) => {
    if (!err) { mkdirSync(CACHE_DIR, { recursive: true });
                writeFileSync(INSTALL_MARKER, JSON.stringify({ version, installed_at: Date.now() })); }
    resolve(!err);
  });
}
export function isRestartNeeded(currentVersion: string): boolean {
  // marker.version !== currentVersion && Date.now() - marker.installed_at < CHECK_INTERVAL_MS
}
```

So after most commands the CLI prints the notification to **stderr** and silently runs
`npm install -g @oxgeneral/orch@<latest>` in the background.

### 19.3 `orch update` output

```
(blank)
  {amber('orch update')} · checking for updates…        <-- U+2026 ellipsis
(blank)
```
then one of:

* registry unreachable:
```
  ✕  Could not reach npm registry
     Check your network connection and try again.

```
  → `process.exit(1)`
* already current (verified):
```
  ✓  Already up to date (1.0.34)

```
  → return (exit 0)
* update available:
```
  ●  Update available: 1.0.33 → 1.0.34

```
  then with `--check`:
```
  Run orch update to install.

```
  → exit 0; without `--check`:
```
  Installing @oxgeneral/orch@1.0.34…

```
  runs `npm install -g @oxgeneral/orch@latest` with a 60 s timeout, piping child stdout/stderr
  straight to the terminal; on failure:
```

  ✕  Update failed. Try manually:
     npm install -g @oxgeneral/orch@latest

```
  → `process.exit(1)`; on success:
```

  ✓  Updated to 1.0.34

```
  → exit 0.

`update` uses raw `chalk.ansi256(167|72|214)` and hard-coded `✕ ✓ ●` glyphs, so `--ascii` has no
effect. It always calls `checkForUpdateNow` (a forced network hit that also refreshes the cache),
never the SWR path. No `--json`, no `--quiet`.

---

## 20. Storage layout, ids and serialization

```ts
export const ORCHESTRY_DIR = '.orchestry';
```

| Path | Contents |
|---|---|
| `.orchestry/config.yml` | project config (YAML) |
| `.orchestry/state.json` | `OrchestratorState` (JSON, `claimed` as an array on disk) |
| `.orchestry/orchestry.lock` | orchestrator lock |
| `.orchestry/tasks/<id>.yml` | one `Task` per file |
| `.orchestry/agents/<id>.yml` | one `Agent` per file |
| `.orchestry/runs/<id>.json` | `Run` metadata |
| `.orchestry/runs/<id>.jsonl` | append-only `RunEvent` lines |
| `.orchestry/templates/default.md` | prompt template |
| `.orchestry/logs/` | reserved |
| `.orchestry/context/<key>.json` | `ContextEntry` |
| `.orchestry/messages/<id>.json` | `Message` |
| `.orchestry/goals/<id>.yml` | `Goal` |
| `.orchestry/teams/<id>.yml` | `Team` |
| `.orchestry/attachments/<taskId>/` | task attachments |
| `.orchestry/.gitignore` | runtime-state ignores |
| `.orchestry/workspace-exclude` | worktree copy excludes |

```ts
const ID_PATTERN = /^[A-Za-z0-9._-]+$/;
export function sanitizeId(id: string): string {
  if (!ID_PATTERN.test(id)) throw new Error(`Invalid identifier: "${id}"`);
  return id;
}
```
Id prefixes (all + `nanoid(7)`): `tsk_`, `agt_`, `run_`, `goal_`, `team_`, `msg_`.

Serialization:
```ts
writeYaml: yaml.dump(data, { indent: 2, lineWidth: 120, noRefs: false /*true*/, sortKeys: false })
writeJson: JSON.stringify(data, null, 2) + '\n'
atomicWrite: temp `.${basename}.${randomBytes(4).toString('hex')}.tmp` -> fs.rename
appendJsonl: O_APPEND handle; a line exceeding PIPE_BUF (4096 bytes) has its `data` field truncated to fit
```
Verified JSONL line: `{"timestamp":"2026-10-13T16:03:45.086Z","type":"agent_output","data":"done"}`.

---

## 21. Interactive surfaces and non-TTY degradation

### 21.1 `$EDITOR` (`src/cli/editor.ts`)

Used by `task add -e`, `task edit`, `agent add -e`, `agent edit`. `config edit` spawns directly.

```ts
export async function openInEditor(initialContent: string, opts: OpenInEditorOptions = {}): Promise<string> {
  const { extension = '.yml', prefix = 'orch-' } = opts;
  const editor = process.env['EDITOR'] || process.env['VISUAL'] || 'vi';
  const dir = await mkdtemp(join(tmpdir(), prefix));
  const filePath = join(dir, `edit${extension}`);
  await writeFile(filePath, initialContent, 'utf8');
  try {
    const parts = editor.split(/\s+/);
    const child = spawn(parts[0]!, [...parts.slice(1), filePath], { stdio: 'inherit' });
    await new Promise<void>((resolve, reject) => {
      child.on('close', (code) => { if (code === 0) resolve(); else reject(new Error(`Editor exited with code ${code}`)); });
      child.on('error', reject);
    });
    return await readFile(filePath, 'utf8');
  } finally {
    await unlink(filePath).catch(() => {});
    await rm(dir, { recursive: true }).catch(() => {});
  }
}
```
**There is no TTY check.** In a non-TTY the editor is still spawned with inherited stdio and will
typically fail (→ exit 1) or hang. No `--yes`/non-interactive fallback exists.

Task buffer format:
```
---
title: My Task
priority: 2
---

Free-form description here...
```
```ts
export function fromEditorContent(content: string) {
  const match = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
  if (!match) return { description: content.trim() || undefined };
  for (const line of frontmatter.split('\n')) {
    const kv = line.match(/^([\w]+):\s*(.*)$/);
    if (!kv) continue;
    if (key === 'title' && value.trim()) result.title = value.trim();
    else if (key === 'priority') { const n = parseInt(value.trim(), 10); if (n >= 1 && n <= 4) result.priority = n; }
  }
  const desc = body.trim(); if (desc) result.description = desc;
  return result;
}
```

Agent buffer format:
```
# Edit agent configuration.
# Lines starting with # are ignored.
# Role description goes below the second --- separator.
---
name: My Agent
command:
model: claude-sonnet-4-6
---

Role description here...
```
```ts
// strips '#' lines ONLY before the first '---' line, preserving '#' inside the role body
const firstFence = lines.findIndex((l) => l.trimEnd() === '---');
const stripped = (firstFence >= 0
  ? [...lines.slice(0, firstFence).filter((l) => !l.startsWith('#')), ...lines.slice(firstFence)]
  : lines.filter((l) => !l.startsWith('#'))).join('\n');
...
result.role = body.trim();      // ALWAYS set (empty string = cleared) so callers can detect clearing
```
Reads `name`, `command`, `model` (trimmed; empty allowed).

### 21.2 Shop picker (`src/cli/shop-picker.ts`)

Reached only when **not** `--list` **and** `process.stdout.isTTY`; `pickFromShop` itself returns
`null` immediately when `!process.stdin.isTTY`.

```ts
function getPageSize() { const rows = process.stdout.rows ?? 24; return Math.max(1, Math.min(templates.length, rows - 4)); }
process.stdout.write('\x1B[2J\x1B[H');
console.log(chalk.bold.yellow('\n  AGENT SHOP') + chalk.gray('  — arrow keys to navigate, enter to select, q to cancel\n'));
const start = Math.max(0, Math.min(selected - Math.floor(pageSize / 2), templates.length - pageSize));
const cursor = isSelected ? chalk.yellow('  ▸ ') : '    ';          // ▸
const name   = isSelected ? chalk.bold.white(t.name) : chalk.gray(t.name);
const desc   = chalk.gray(` — ${t.description}`);                   // —
const model  = chalk.gray.dim(` [${t.tier}]`);
console.log(`${cursor}${name}${desc}${model}`);
if (templates.length > pageSize) { console.log(chalk.gray(`\n  ${start + 1}-${end} of ${templates.length}`)); }
```
Keys: `up` / `Ctrl+P` and `down` / `Ctrl+N` wrap modulo the list length; `return` selects;
`q`, `escape`, `Ctrl+C` cancel (resolve `null`). `cleanup()` removes the keypress listener, restores
`setRawMode(false)`, closes readline and clears the screen again. `rl.on('error')` and
`rl.on('close')` also cancel. Uses plain `chalk.yellow/gray/white` (not the ansi256 palette) and
ignores `--ascii`.

### 21.3 `init` adapter prompt
See §15.4 — prompts only with ≥2 detected adapters **and** both `stdout.isTTY` and `stdin.isTTY`;
otherwise silently picks `available[0]`.

### 21.4 Clipboard (`src/infrastructure/clipboard-service.ts`)

**Not wired into any CLI command** — TUI-only (attachment paste).
API: `isClipboardToolAvailable(): boolean`, `detectClipboardType(): 'image'|'text'|'empty'`,
`getClipboardImage(): { data: Buffer; ext: 'png' } | null`.
Platform commands: macOS `osascript -e 'clipboard info'` + an AppleScript PNGf write;
Linux `xclip -selection clipboard -t TARGETS -o` then `xclip -selection clipboard -t image/png -o`
(requires `xclip`, probed via `which xclip`); Windows PowerShell `Get-Clipboard -Format Image` /
`System.Windows.Forms.Clipboard::GetImage()`. `EXEC_TIMEOUT_MS = 3_000`; Linux `maxBuffer` 50 MB.
Unsupported platform → `OrchestryError(`Unsupported platform for clipboard: ${platform}`, 1,
'Supported: macOS, Linux, Windows')`. Every failure path degrades to `'empty'` / `null`.

### 21.5 `postinstall` (`scripts/postinstall.cjs`)

Patches Ink's unbounded caches (`wrap-text.js`, `measure-text.js`, `output.js`, LRU cap 2000),
installs `skills/orch/SKILL.md` to `~/.claude/skills/orch/SKILL.md`, then prints a banner to
**stderr** — skipped entirely when `process.env.CI` is set or `!process.stderr.isTTY`:
```
  ✓ orchestry installed
  ✓ Claude Code skill /orch registered

  Get started:
    $ orch
    or use /orch in Claude Code

  ⭐ Like it? Star us on GitHub: https://github.com/oxgeneral/ORCH
```

### 21.6 `--quiet` support matrix

**Honoured by**: `task add`, `task list`, `agent add`, `agent list`, `agent edit`,
`agent autonomous`, `context set`, `context get`, `context list`, `context delete` (suppresses),
`msg send`, `msg broadcast`, `goal add`, `goal status`, `goal update`, `goal delete` (suppresses),
`team create`, `org deploy`.

**Ignored by**: `status`, `logs` (all paths), `doctor`, `run`, `serve`, `init`, `update`, `tui`,
`task show`, `task edit`, `task assign`, `task cancel`, `task approve`, `task reject`, `task retry`,
`agent status`, `agent remove`, `agent disable`, `agent enable`, `agent shop`, all `config`
subcommands, `msg inbox`, `msg list`, `goal list`, `goal show`, `team list`, `team show`,
`team join`, `team leave`, `team add-task`, `team set-lead`, `team disband`, `org list`.

---

## 22. Parity gotchas (ranked by risk)

1. `status --json` → `state.claimed` is always `{}` (JS `Set` serialization), never an array.
2. `doctor --json` emits two human header lines on stdout **before** the JSON object.
3. `config get --json` drops the `value` key entirely when the key is missing (`{"key":"a.b"}`).
4. Four `--json` sites are **compact** (no indent): `config get`, `config global get`,
   `config global show`, `goal delete`. Everything else uses 2-space indent.
5. `logs --task` / `--agent` / `--since`-only with `--json` emit `Run[]`, **not** events; only a
   positional run-id emits `RunEvent[]`.
6. `formatDuration` is unclamped — a future `expires_at` renders as `-60s` in `context list`.
7. `agent list`'s TIME column is hard-coded `dim('—')`; it is never computed.
8. `init` when already initialized still prints a `Next:` line, selected by
   `opts.adapter === 'shell'`, so a bare re-`init` in a shell project shows the `agt_creator` hint.
9. Hard-coded glyphs that `--ascii` does **not** affect: `task show` review results (`✓`/`✗`),
   `goal list` status icons (`● ‖ ✓ ✕`), `org deploy` (`★ •`), `update` (`✕ ✓ ●`), and the whole
   shop picker.
10. `serve --once` polls task terminality every **2000 ms**, hard-coded, independent of
    `--tick-interval`.
11. Any command in an uninitialized directory — including a subcommand's `--help` and unknown
    commands — exits **3** with `Not initialized` / `Run: orch init`, because the container is built
    before Commander parses.
12. Commander prints a group's help to **stdout** with exit code **1** when the group is invoked
    with no subcommand.
13. The CLI performs a silent background `npm install -g` self-update after most commands.
14. `serve` text-log timestamps are **UTC** (`ts.slice(11, 23)`); `logs` / `run` human timestamps are
    **local** (`toLocaleTimeString('en-US', …)`), and `run --watch` omits seconds.
15. `run --watch` is the only command whose lines have **no** two-space indent; its tick line is a
    `\r`-rewritten string with four trailing spaces.
16. `printTable` and `printKeyValue` emit **trailing spaces** on every line.
17. `msg` subject falls back to the literal string `(no subject)`.
18. `org deploy` leaves partially-created agents behind on failure (no rollback).
19. `doctor` always exits 0, even with failing checks.
20. `config set` auto-vivifies and accepts arbitrary dotted keys with no validation.

---

## 23. Source file index

| Concern | Path (all under `parity/orch-ts`) |
|---|---|
| entry point, routing, global flags | `src/bin/cli.ts` |
| output primitives | `src/cli/output.ts` |
| CLI context | `src/cli/context.ts` |
| editor helpers | `src/cli/editor.ts` |
| shop picker | `src/cli/shop-picker.ts` |
| update check | `src/cli/update-check.ts` |
| commands | `src/cli/commands/{task,agent,run,logs,config,context,msg,goal,team,status,doctor,init,org,serve,update,tui}.ts` |
| serve subsystem | `src/cli/serve/{types,once-runner,structured-logger,disk-observer}.ts` |
| DI container | `src/container.ts` |
| domain models / errors / presets | `src/domain/{errors,config,global-config,events,task,run,agent,state,goal,team,message,agent-shop,org-shop,default-agents,model-tiers,transitions}.ts` |
| services | `src/application/{doctor-service,task-service,agent-service,run-service,message-service,goal-service,team-service,agent-factory,event-bus,orchestrator}.ts` |
| storage | `src/infrastructure/storage/{paths,config-store,global-config-store,state-store,fs-utils,interfaces,task-store,agent-store,run-store,context-store,message-store,goal-store,team-store}.ts` |
| clipboard | `src/infrastructure/clipboard-service.ts` |
| postinstall | `scripts/postinstall.cjs` |
| contract-string tests | `test/unit/cli/{commands-logs,commands-logs-since-all,commands-config,commands-doctor,commands-init,commands-serve,commands-run,commands-task-cancel,commands-task-time,commands-context,structured-logger,serve-once-runner,disk-observer,editor,editor-agent,create-context,output-lazy-chalk,cli-lazy-routing,postinstall,helpers}.ts` |
