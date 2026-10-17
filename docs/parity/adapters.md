# ORCH-TS Adapter Layer — Behavioural Specification for Rust Reimplementation

> **Status: historical.** Matching the TypeScript original is no longer a goal; this appendix is not updated.
> See [ADR 0002](../decisions/0002-parity-is-no-longer-a-goal.md).

Source repo: `parity/orch-ts` (read-only; nothing modified).

> **Finding up front: there is no `AgentResult` type anywhere in the codebase.**
> `grep -rn "AgentResult" src/ test/` returns zero hits. The adapter contract's
> return types are `ExecuteHandle`, `AdapterTestResult`, and `AgentEvent`. See §6.

---

## 0. Files read (all absolute)

Adapter layer — `parity/orch-ts/src/infrastructure/adapters/`:

| file | lines |
|---|---|
| `interface.ts` | 75 |
| `registry.ts` | 40 |
| `utils.ts` | 163 |
| `event-buffer.ts` | 138 |
| `claude.ts` | 106 |
| `codex.ts` | 221 |
| `cursor.ts` | 119 |
| `opencode.ts` | 125 |
| `pi.ts` | 625 |
| `grok.ts` | 204 |
| `antigravity.ts` | 116 |
| `shell.ts` | 158 |

Also:
- `parity/orch-ts/src/infrastructure/models/model-discovery.ts` (303 L)
- `parity/orch-ts/src/domain/model-tiers.ts` (115 L)
- `parity/orch-ts/src/infrastructure/process/process-manager.ts` (141 L)

Supporting files read because adapter behaviour is actually defined there:
- `parity/orch-ts/src/domain/errors.ts` — `classifyAdapterError`, `AdapterErrorKind`, `ERROR_HINTS`, `extractErrorMessage`, `isModelMetadataWarning`
- `parity/orch-ts/src/domain/run.ts` — `TokenUsage`, `createTokenUsage`, `RunEvent`, `RunEventType`
- `parity/orch-ts/src/domain/agent.ts` — `AgentConfig`, `ReasoningEffort`, `ApprovalPolicy`
- `parity/orch-ts/src/domain/config.ts` — `DEFAULT_CONFIG` (timeouts)
- `parity/orch-ts/src/container.ts` — adapter registration order
- `parity/orch-ts/src/application/orchestrator.ts` — `execute()` call site, stall detection

Tests read for fixture shapes:
`test/unit/infrastructure/{claude,codex,cursor,opencode,pi,grok,antigravity,shell}-adapter.test.ts`,
`test/unit/infrastructure/adapter-utils.test.ts`, `test/unit/infrastructure/event-buffer.test.ts`,
`test/unit/infrastructure/process-manager.test.ts`, `test/unit/infrastructure/model-discovery.test.ts`,
`test/unit/domain/adapter-error-kind.test.ts`.

---

## 1. Shared machinery

### 1.1 `ProcessManager.spawn` defaults (`process-manager.ts:64-80`)

```ts
const proc = spawn(command, args, {
  stdio: ['ignore', 'pipe', 'pipe'],
  detached: true,
  ...options,          // caller options OVERRIDE the two defaults above
});

if (!proc.pid) {
  throw new Error(`Failed to spawn process: ${command}`);
}

// Allow parent to exit without waiting for this child.
// Pipes (stdout/stderr) still hold refs while being read — that's intentional.
proc.unref();

return { process: proc, pid: proc.pid };
```

- No `shell: true` anywhere in the layer.
- **No adapter performs any shell quoting or escaping.** Every adapter passes an argv
  array straight to `spawn` (execve semantics). The single exception is the shell
  adapter, which hands one unquoted command string to `bash -lc` and lets bash parse
  it (§2.8).
- Caller-supplied `stdio` fully replaces the default; caller `env` fully replaces the
  inherited environment (every adapter constructs `{ ...process.env, ...params.env }`
  itself, so nothing is lost in practice).

### 1.2 `readLines` (`process-manager.ts:88-141`) — line splitter used by every adapter except Pi

```ts
/**
 * Max stdout line length before truncation (16 KB).
 * First layer of a three-layer cap: readLines (16 KB) → serializeEventData (8 KB) → bus emit (4 KB).
 * Truncated lines produce invalid JSON → adapters' catch block handles gracefully.
 */
const MAX_LINE_LEN = 16384;

function capLine(s: string): string {
  return s.length > MAX_LINE_LEN ? s.slice(0, MAX_LINE_LEN) : s;
}

export async function* readLines(stream: Readable): AsyncGenerator<string> {
  const chunks: Buffer[] = [];
  let totalLen = 0;

  for await (const chunk of stream) {
    const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk as string, 'utf-8');
    if (buf.length === 0) continue;
    chunks.push(buf);
    totalLen += buf.length;

    const buffer = chunks.length === 1 ? chunks[0]! : Buffer.concat(chunks, totalLen);
    chunks.length = 0;
    totalLen = 0;

    let offset = 0;
    let newlineIdx: number;
    while ((newlineIdx = buffer.indexOf(0x0a, offset)) !== -1) {
      if (newlineIdx > offset) {
        yield capLine(buffer.toString('utf-8', offset, newlineIdx));
      }
      offset = newlineIdx + 1;
    }

    if (offset < buffer.length) {
      const remainder = buffer.subarray(offset);
      chunks.push(remainder);
      totalLen = remainder.length;
    }
  }

  if (totalLen > 0) {
    const final = chunks.length === 1 ? chunks[0]! : Buffer.concat(chunks, totalLen);
    yield capLine(final.toString('utf-8'));
  }
}
```

Parity-critical semantics:

1. `if (newlineIdx > offset)` → **zero-length lines are silently dropped**. A `"\n\n"`
   write produces *no* yielded lines (asserted by "skips empty lines" tests in the
   claude and opencode suites).
2. Delimiter is `\n` (0x0A) only. **`readLines` does NOT strip a trailing `\r`.**
   Only Pi's private reader does (§2.5).
3. Final partial line (no trailing newline) is flushed at stream end.
4. `capLine` truncates to 16384 **UTF-16 code units** (JS `String.length`), not bytes.
   Truncated JSON becomes invalid → each adapter's `catch` turns it into a raw
   `output` event.
5. Uses `for await` on the raw `Readable` for proper backpressure. Source comment:
   `readline.createInterface` was rejected because it "buffers all 'line' events in an
   unbounded queue even when the consumer is paused — causing OOM under high throughput".

### 1.3 `buildFullPrompt` (`utils.ts:30-32`)

```ts
/** Combine system and user prompts. Adapters without native system prompt support use this. */
export function buildFullPrompt(systemPrompt: string | undefined, userPrompt: string): string {
  return systemPrompt ? systemPrompt + '\n\n' + userPrompt : userPrompt;
}
```

- Empty-string `systemPrompt` is falsy → returns `userPrompt` unchanged.
- Separator is **exactly two `\n`** (asserted by a dedicated test that slices the
  separator out and compares to `'\n\n'`).
- Used by: codex (stdin), cursor (positional), opencode (positional), antigravity
  (positional), shell (env var).
- *Not* used by claude, pi, grok — they have native system-prompt flags.

### 1.4 `extractTokens` (`utils.ts:40-61`) — generic JSON token extraction

```ts
function readUsageNumber(usage, primaryKey, fallbackKey): number | undefined {
  const primary = usage[primaryKey];
  if (typeof primary === 'number') return primary;
  const fallback = usage[fallbackKey];
  return typeof fallback === 'number' ? fallback : undefined;
}

export function extractTokens(
  parsed: Record<string, unknown>,
  opts?: { statsFallback?: boolean },
): TokenUsage | undefined {
  let usage = parsed.usage as Record<string, unknown> | undefined;

  if (!usage && opts?.statsFallback) {
    const stats = parsed.stats as Record<string, unknown> | undefined;
    usage = stats?.usage as Record<string, unknown> | undefined;
  }

  if (!usage) return undefined;

  const input = readUsageNumber(usage, 'input_tokens', 'inputTokens');
  if (input === undefined) return undefined;

  const output      = readUsageNumber(usage, 'output_tokens', 'outputTokens') ?? 0;
  const reasoning   = readUsageNumber(usage, 'reasoning_tokens', 'reasoningTokens') ?? 0;
  const cache_read  = readUsageNumber(usage, 'cache_read_input_tokens', 'cacheReadTokens') ?? 0;
  const cache_write = readUsageNumber(usage, 'cache_creation_input_tokens', 'cacheWriteTokens') ?? 0;
  return createTokenUsage(input, output, { reasoning, cache_read, cache_write });
}
```

Literal key names and precedence (first numeric match wins):

| field | primary key | camelCase fallback | default |
|---|---|---|---|
| `input` | `input_tokens` | `inputTokens` | **none — missing ⇒ discard whole usage, return `undefined`** |
| `output` | `output_tokens` | `outputTokens` | `0` |
| `reasoning` | `reasoning_tokens` | `reasoningTokens` | `0` |
| `cache_read` | `cache_read_input_tokens` | `cacheReadTokens` | `0` |
| `cache_write` | `cache_creation_input_tokens` | `cacheWriteTokens` | `0` |

`createTokenUsage` (`src/domain/run.ts:42-56`):

```ts
/** Create TokenUsage with total always computed as input + output + reasoning. */
export function createTokenUsage(input, output, opts?): TokenUsage {
  const reasoning = opts?.reasoning ?? 0;
  return {
    input,
    output,
    reasoning,
    total: input + output + reasoning,
    cache_read: opts?.cache_read ?? 0,
    cache_write: opts?.cache_write ?? 0,
  };
}
```

**`total` excludes cache tokens** — source comment on the type: "Cache tokens —
informational only, NOT added to total (subset of input)."

`statsFallback: true` is used by **claude only**. Codex and cursor call `extractTokens`
with no opts. Opencode and pi have their own extractors (§2.4, §2.5).

### 1.5 `createStderrTailCapture` / `appendStderrTail` (`utils.ts:70-93`)

```ts
const STDERR_TAIL_BYTES = 4096;

export function createStderrTailCapture(stderr: Readable | null | undefined): () => string {
  if (!stderr) return () => '';

  let buf: Buffer = Buffer.alloc(0);
  stderr.on('data', (chunk: Buffer | string) => {
    const next = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, 'utf-8');
    buf = buf.length === 0 ? next : Buffer.concat([buf, next], buf.length + next.length);
    if (buf.length > STDERR_TAIL_BYTES) {
      // Materialize an exactly-sized copy rather than retaining a view into a
      // potentially much larger chunk's backing ArrayBuffer.
      buf = Buffer.from(buf.subarray(buf.length - STDERR_TAIL_BYTES));
    }
  });
  stderr.on('error', () => {});

  return () => buf.toString('utf-8').trimEnd();
}

/** Append a captured stderr tail to an adapter process error. */
export function appendStderrTail(message: string, adapterName: string, tail: string): string {
  return tail ? `${message}\n--- ${adapterName} stderr (tail) ---\n${tail}` : message;
}
```

- The `'data'` listener is attached **immediately** — docstring: "Attaching the data
  listener immediately prevents a chatty child process from blocking on a full stderr
  pipe."
- A no-op `'error'` handler is attached too.
- Getter result is `trimEnd()`-ed.
- Null/undefined stderr → getter returns `''`.
- Tested: writing `'x'.repeat(5000) + 'ACTIONABLE_ERROR'` yields a tail of exactly
  4096 chars that `endsWith('ACTIONABLE_ERROR')`.
- Exact separator line format: `--- <adapterName> stderr (tail) ---`.

### 1.6 `createStreamingEvents` (`utils.ts:105-163`) — generator used by claude, codex, cursor, opencode

```ts
export function createStreamingEvents(
  proc: ChildProcess,
  parseEvent: (line: string) => AgentEvent | null,
  adapterName: string,
  signal?: AbortSignal,
): AsyncGenerator<AgentEvent> {
  // Set up lifecycle and stderr listeners eagerly. Async generator bodies do
  // not run until their first next(), and fast process failures can otherwise
  // happen before ORCH starts consuming the event stream.
  let exitCode: number | null = null;
  let exitError: Error | null = null;
  const stderrTail = createStderrTailCapture(proc.stderr);
  const exitPromise = new Promise<void>((resolve) => {
    proc.once('close', (code) => { exitCode = code; resolve(); });
    proc.once('error', (err) => { exitError = err; resolve(); });
  });

  async function* generate(): AsyncGenerator<AgentEvent> {
    let gotDoneEvent = false;

    if (proc.stdout) {
      try {
        for await (const line of readLines(proc.stdout)) {
          if (signal?.aborted) break;
          const event = parseEvent(line);
          if (event) {
            if (event.type === 'done') gotDoneEvent = true;
            yield event;
          }
        }
      } finally {
        // Destroy the stream to release the FD immediately rather than waiting
        // for the process to die — critical for rapid abort/restart cycles.
        // destroy() is idempotent: safe on an already-ended stream.
        proc.stdout.destroy();
      }
    }

    await exitPromise;

    if (exitError && !signal?.aborted && !gotDoneEvent) {
      const spawnErr = exitError as Error;
      const message = appendStderrTail(spawnErr.message, adapterName, stderrTail());
      const classified = classifyAdapterError(message, exitCode ?? undefined);
      throw Object.assign(new Error(message), { errorKind: classified });
    }
    if (exitCode !== 0 && exitCode !== null && !signal?.aborted && !gotDoneEvent) {
      const msg = appendStderrTail(
        `${adapterName} process exited with code ${exitCode}`,
        adapterName,
        stderrTail(),
      );
      const classified = classifyAdapterError(msg, exitCode);
      throw Object.assign(new Error(msg), { errorKind: classified });
    }
  }

  return generate();
}
```

Invariants to reproduce exactly:

- Listeners are installed at **call time**, before the generator's first `next()`.
  A cursor test relies on this: it writes stderr and emits `close 1` synchronously
  before iteration begins, and the thrown message must still contain the stderr text.
- `proc.once(...)` (not `on`).
- A `done` event **suppresses all exit-code errors**.
- An aborted signal suppresses them too.
- `exitCode === null` (killed by signal) does **not** throw.
- `adapterName` literals in use: `'Claude'`, `'Codex'`, `'Cursor agent'`, `'OpenCode'`.
  (Pi uses lowercase `'pi'` in its own path; grok/antigravity/shell don't use this helper.)

### 1.7 Error classification — `classifyAdapterError` (`src/domain/errors.ts`)

```ts
export enum AdapterErrorKind {
  ADAPTER_NOT_FOUND = 'adapter_not_found',
  AUTH_FAILED = 'auth_failed',
  TIMEOUT = 'timeout',
  RATE_LIMIT = 'rate_limit',
  PROCESS_CRASH = 'process_crash',
  SPAWN_FAILED = 'spawn_failed',
  UNKNOWN = 'unknown',
}

export function classifyAdapterError(error: string, exitCode?: number): AdapterErrorKind {
  const lower = error.toLowerCase();

  if (lower.includes('enoent') || lower.includes('spawn failed')) {
    return AdapterErrorKind.SPAWN_FAILED;
  }
  if (lower.includes('not found') || lower.includes('command not found') || lower.includes('no such file')) {
    return AdapterErrorKind.ADAPTER_NOT_FOUND;
  }
  if (lower.includes('auth') || lower.includes('unauthorized') || lower.includes('401') || lower.includes('invalid api key') || lower.includes('authentication')) {
    return AdapterErrorKind.AUTH_FAILED;
  }
  if (lower.includes('timeout') || lower.includes('timed out') || lower.includes('etimedout')) {
    return AdapterErrorKind.TIMEOUT;
  }
  if (lower.includes('rate limit') || lower.includes('429') || lower.includes('too many requests')) {
    return AdapterErrorKind.RATE_LIMIT;
  }
  if (exitCode !== undefined && exitCode !== 0) {
    return AdapterErrorKind.PROCESS_CRASH;
  }

  return AdapterErrorKind.UNKNOWN;
}
```

**Exactly 7 enum values** (asserted: `Object.values(AdapterErrorKind)).toHaveLength(7)`;
all values match `/^[a-z_]+$/`).

Rules in evaluation order, on `lower = error.toLowerCase()`:

| # | Condition (substring match on the lowercased message) | Result |
|---|---|---|
| 1 | `'enoent'` OR `'spawn failed'` | `SPAWN_FAILED` |
| 2 | `'not found'` OR `'command not found'` OR `'no such file'` | `ADAPTER_NOT_FOUND` |
| 3 | `'auth'` OR `'unauthorized'` OR `'401'` OR `'invalid api key'` OR `'authentication'` | `AUTH_FAILED` |
| 4 | `'timeout'` OR `'timed out'` OR `'etimedout'` | `TIMEOUT` |
| 5 | `'rate limit'` OR `'429'` OR `'too many requests'` | `RATE_LIMIT` |
| 6 | `exitCode !== undefined && exitCode !== 0` | `PROCESS_CRASH` |
| 7 | otherwise | `UNKNOWN` |

Behaviour notes:
- String matches **always beat** the exit code. Asserted:
  `classifyAdapterError('ENOENT /bin/claude', 127) === SPAWN_FAILED`;
  `classifyAdapterError('Unauthorized access', 1) === AUTH_FAILED`.
- `'auth'` is a bare substring, so e.g. `"author"` classifies as `AUTH_FAILED`.
- `'401'` / `'429'` are bare substrings of the whole message (not status-code parsing).
- `exitCode === 0` + no keyword → `UNKNOWN` (asserted).
- No `exitCode` + no keyword → `UNKNOWN` (asserted).
- Case-insensitive (asserted with `'RATE LIMIT exceeded'`, `'AUTHENTICATION FAILED'`, `'TIMEOUT'`).
- Because adapter error messages have the stderr tail appended **before**
  classification, stderr content can change the classification of an exit-code error.

Verbatim test vectors from `test/unit/domain/adapter-error-kind.test.ts`:

```
'spawn ENOENT'                      → SPAWN_FAILED
'Spawn failed: /usr/bin/foo'        → SPAWN_FAILED
'claude: command not found'         → ADAPTER_NOT_FOUND
'CLI not found on PATH'             → ADAPTER_NOT_FOUND
'Authentication failed'             → AUTH_FAILED
'Request unauthorized'              → AUTH_FAILED
'HTTP 401 error'                    → AUTH_FAILED
'Invalid API key provided'          → AUTH_FAILED
'Request timeout after 60s'         → TIMEOUT
'Connection timed out'              → TIMEOUT
'connect ETIMEDOUT 1.2.3.4:443'     → TIMEOUT
'Rate limit exceeded'               → RATE_LIMIT
'HTTP 429 Too Many Requests'        → RATE_LIMIT
'Too many requests, slow down'      → RATE_LIMIT
('Segmentation fault', 139)         → PROCESS_CRASH
('Unknown error occurred', 1)       → PROCESS_CRASH
('Something weird', 0)              → UNKNOWN
'Something else'                    → UNKNOWN
('ENOENT /bin/claude', 127)         → SPAWN_FAILED
('Unauthorized access', 1)          → AUTH_FAILED
```

### 1.8 `ERROR_HINTS` (`src/domain/errors.ts`)

```ts
export interface AdapterErrorHint {
  message: string;
  fix: string;
  doctorHint?: boolean;
}

export const ERROR_HINTS: Record<AdapterErrorKind, AdapterErrorHint> = {
  [AdapterErrorKind.ADAPTER_NOT_FOUND]: {
    message: 'CLI is not installed.',
    fix: 'Install it with: npm i -g @anthropic-ai/claude-code',
    doctorHint: true,
  },
  [AdapterErrorKind.AUTH_FAILED]: {
    message: 'API key is invalid.',
    fix: 'Check it with: claude auth status',
  },
  [AdapterErrorKind.TIMEOUT]: {
    message: 'Agent timed out.',
    fix: 'Increase the timeout with: orch config set agent_timeout <ms>',
  },
  [AdapterErrorKind.RATE_LIMIT]: {
    message: 'API rate limit reached.',
    fix: 'Wait and retry with: orch task retry <id>',
  },
  [AdapterErrorKind.PROCESS_CRASH]: {
    message: 'Agent process crashed.',
    fix: 'Try again with: orch task retry <id>',
  },
  [AdapterErrorKind.SPAWN_FAILED]: {
    message: 'Failed to start the process.',
    fix: 'Check PATH and file permissions',
  },
  [AdapterErrorKind.UNKNOWN]: {
    message: 'Unknown error.',
    fix: 'Run: orch doctor',
    doctorHint: true,
  },
};
```

Asserted: every kind has a hint with truthy `message` and `fix`;
`adapter_not_found.doctorHint === true`; `unknown.doctorHint === true`;
`auth_failed.doctorHint === undefined`.

### 1.9 `extractErrorMessage` (`src/domain/errors.ts`) — used by codex

```ts
export function extractErrorMessage(value: unknown, depth = 0): string {
  if (depth >= 6) return stringifyErrorValue(value);
  if (value instanceof Error) return value.message;

  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (!trimmed) return '';
    try {
      return extractErrorMessage(JSON.parse(trimmed), depth + 1);
    } catch {
      return value;            // NOTE: the ORIGINAL, un-trimmed value
    }
  }

  if (value && typeof value === 'object') {
    const record = value as Record<string, unknown>;
    for (const key of ['error', 'message', 'detail', 'reason']) {
      if (record[key] === undefined || record[key] === null) continue;
      const message = extractErrorMessage(record[key], depth + 1);
      if (message) return message;
    }
  }

  return stringifyErrorValue(value);
}

function stringifyErrorValue(value: unknown): string {
  if (value === undefined) return '';
  if (value === null) return 'null';
  if (typeof value === 'string') return value;
  try { return JSON.stringify(value); } catch { return String(value); }
}
```

- Depth guard at **6**.
- Object key probe order is exactly `['error', 'message', 'detail', 'reason']`.
- String values are JSON-parsed and recursed into (this is how nested provider
  envelopes get unwrapped).

Asserted:
```ts
extractErrorMessage({
  type: 'turn.failed',
  error: { message: JSON.stringify({
    type: 'error', status: 400,
    error: { type: 'invalid_request_error', message: "The 'gpt-5.6' model is not supported for this account." },
  }) },
}) === "The 'gpt-5.6' model is not supported for this account."

extractErrorMessage('Connection refused') === 'Connection refused'
```

### 1.10 `isModelMetadataWarning` (`src/domain/errors.ts`) — used by codex

```ts
/** Codex reports this recoverable cache miss as an error item. */
export function isModelMetadataWarning(message: string): boolean {
  return /^Model metadata for .+ not found\. Defaulting to fallback metadata\b/i.test(message.trim());
}
```

Asserted true for
`'Model metadata for \`gpt-5.6\` not found. Defaulting to fallback metadata; this can degrade performance.'`,
false for `'Model request failed'`.

### 1.11 Adapter registry (`registry.ts`, full file semantics)

```ts
export class AdapterRegistry {
  private readonly adapters = new Map<string, IAgentAdapter>();

  register(adapter: IAgentAdapter): void { this.adapters.set(adapter.kind, adapter); }
  get(kind: string): IAgentAdapter | undefined { return this.adapters.get(kind); }

  require(kind: string): IAgentAdapter {
    const adapter = this.adapters.get(kind);
    if (!adapter) {
      throw new Error(`Unknown adapter: "${kind}". Available: ${this.listKinds().join(', ')}`);
    }
    return adapter;
  }

  list(): IAgentAdapter[] { return Array.from(this.adapters.values()); }
  listKinds(): string[] { return Array.from(this.adapters.keys()); }
  has(kind: string): boolean { return this.adapters.has(kind); }
}
```

- Keyed by `adapter.kind`; `register` overwrites on duplicate kind.
- `require` error string is exactly: `` `Unknown adapter: "${kind}". Available: ${kinds.join(', ')}` ``
- JS `Map` preserves insertion order, so `listKinds()` returns the registration order
  from `container.ts:201-208`:
  `claude, codex, cursor, shell, opencode, pi, grok, antigravity`.

### 1.12 Environment contributed by the caller (`orchestrator.ts:1131-1146`)

```ts
const abortController = new AbortController();
this.abortControllers.set(taskId, abortController);

const handle = adapter.execute({
  prompt,
  systemPrompt,
  workspace: workspacePath,
  env: {
    ...agent.config.env,
    ORCH_AGENT_ID: agent.id,
    ORCH_AGENT_NAME: agent.name,
    ORCH_TASK_ID: task.id,
  },
  config: agentData.config,
  signal: abortController.signal,
});
```

Every adapter then merges `{ ...process.env, ...params.env }`, so `ORCH_AGENT_ID`,
`ORCH_AGENT_NAME` and `ORCH_TASK_ID` reach **every** child process, and they shadow
same-named keys from `agent.config.env`.

---

## 2. Per-adapter specification

### 2.1 claude — `ClaudeAdapter` (`adapters/claude.ts`), `kind = 'claude'`

Header docstring: "Spawns `claude --print --output-format stream-json` in headless mode.
Parses JSON-lines from stdout into AgentEvent stream."

#### 2.1.1 argv (`claude.ts:37-59`) — exact construction order

```ts
const args = [
  '--print',
  '--output-format', 'stream-json',
  '--max-turns', String(params.config.max_turns ?? 50),
  '--verbose',
  '--dangerously-skip-permissions', // Agents run autonomously; stdin is 'ignore' so prompts would hang
];

if (params.config.model)  args.push('--model', params.config.model);
if (params.config.effort) args.push('--effort', params.config.effort);

// System prompt: orchestrator-generated (cacheable) takes priority, then per-agent config
const effectiveSystemPrompt = params.systemPrompt ?? params.config.system_prompt;
if (effectiveSystemPrompt) args.push('--system-prompt', effectiveSystemPrompt);

args.push(params.prompt);
```

Flat form:

```
claude
  --print
  --output-format stream-json
  --max-turns <String(config.max_turns ?? 50)>      ALWAYS present; default literal 50
  --verbose
  --dangerously-skip-permissions
  [--model <config.model>]                          iff config.model truthy
  [--effort <config.effort>]                        iff config.effort truthy ('low'|'medium'|'high')
  [--system-prompt <effectiveSystemPrompt>]         iff (params.systemPrompt ?? config.system_prompt) truthy
  <params.prompt>                                   positional, ALWAYS LAST
```

- `effectiveSystemPrompt = params.systemPrompt ?? params.config.system_prompt` —
  the orchestrator prompt wins (`??`, so an empty-string `params.systemPrompt` still
  wins but is then falsy and emits no flag).
- Prompt goes in **argv**, never stdin.
- No sandbox / permission-mode / resume / session / cwd flag. Working directory comes
  from spawn `cwd`.
- No shell quoting (argv array).

#### 2.1.2 Spawn options

```ts
this.processManager.spawn('claude', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
});
```

No `stdio` override ⇒ ProcessManager default `['ignore','pipe','pipe']` (stdin closed),
`detached: true`.

#### 2.1.3 Environment

Sets nothing of its own; reads nothing of its own. Passes through
`process.env` + `params.env`.

#### 2.1.4 stdout parsing — `parseClaudeEvent` (`claude.ts:77-106`)

```ts
function parseClaudeEvent(line: string): AgentEvent | null {
  if (!line.trim()) return null;

  try {
    const parsed: Record<string, unknown> = JSON.parse(line);
    const timestamp = new Date().toISOString();

    switch (parsed.type) {
      case 'assistant':
        return { type: 'output', timestamp, data: (parsed.message as unknown) ?? parsed };
      case 'tool_use':
        return { type: 'tool_call', timestamp, data: parsed };
      case 'tool_result':
        return { type: 'output', timestamp, data: parsed };
      case 'error': {
        const errData = (parsed.error as unknown) ?? parsed;
        const errMsg = typeof errData === 'string' ? errData : JSON.stringify(errData);
        return { type: 'error', timestamp, data: errData, errorKind: classifyAdapterError(errMsg) };
      }
      case 'result': {
        const tokens = extractTokens(parsed, { statsFallback: true });
        return { type: 'done', timestamp, data: parsed, tokens };
      }
      default:
        return { type: 'output', timestamp, data: parsed };
    }
  } catch {
    return { type: 'output', timestamp: new Date().toISOString(), data: line };
  }
}
```

| input JSON `type` | emitted `AgentEvent.type` | `data` |
|---|---|---|
| (blank / whitespace-only line) | `null` (dropped) | — |
| `"assistant"` | `output` | `parsed.message ?? parsed` (raw, un-normalised) |
| `"tool_use"` | `tool_call` | the whole `parsed` object |
| `"tool_result"` | `output` | the whole `parsed` object |
| `"error"` | `error` | `parsed.error ?? parsed`; `errorKind` from `classifyAdapterError` on the string (or `JSON.stringify`) |
| `"result"` | `done` | the whole `parsed`; `tokens = extractTokens(parsed, { statsFallback: true })` |
| anything else | `output` | the whole `parsed` object |
| JSON.parse throws | `output` | the **raw line string** |

`timestamp` is always `new Date().toISOString()` computed at parse time.

#### 2.1.5 Fixture lines (from `test/unit/infrastructure/claude-adapter.test.ts`)

```json
{"type":"assistant","message":"hello"}
{"type":"tool_use","name":"read"}
{"type":"error","error":"something broke"}
{"type":"result","usage":{"input_tokens":100,"output_tokens":50}}
{"type":"result","data":"ok"}
{"type":"unknown_type","info":"data"}
{"type":"error","error":"Request unauthorized"}
```
plus the non-JSON line `not json at all` and a bare `"\n\n"` write.

Assertions:
- `{"type":"result","usage":{"input_tokens":100,"output_tokens":50}}` →
  `events[0].type === 'done'` and
  `events[0].tokens === {input:100, output:50, reasoning:0, total:150, cache_read:0, cache_write:0}`
- `{"type":"error","error":"something broke"}` → `events[0].data === 'something broke'`
  (the **unwrapped bare string**, not an object) and `errorKind === UNKNOWN`
- `{"type":"error","error":"Request unauthorized"}` → `errorKind === AUTH_FAILED`
- `not json at all` → `{ type:'output', data:'not json at all' }`
- `"\n\n"` followed by one assistant line → `events.length === 1`
- `{"type":"result","data":"ok"}` then `close 1` → **no throw**, `events[0].type === 'done'`
- `close 1` with no events → rejects with message containing `'exited with code 1'`,
  and the thrown object has a defined `errorKind` property
- Spawn assertion: `pm.spawn` called with `'claude'`,
  `expect.arrayContaining(['--print','--output-format','stream-json','--max-turns','10','--verbose','--dangerously-skip-permissions','test prompt'])`,
  and `expect.objectContaining({ cwd: '/tmp/workspace' })`
- `params.systemPrompt` beats `config.system_prompt` for the `--system-prompt` value
- `args[args.length - 1] === params.prompt` (prompt last)

#### 2.1.6 Exit handling

Per §1.6 with `adapterName = 'Claude'`:
message `` `Claude process exited with code ${exitCode}` `` plus the stderr tail block
`\n--- Claude stderr (tail) ---\n<tail>`.

#### 2.1.7 `test()` (`claude.ts:22-34`)

```ts
try {
  const { stdout } = await execFileAsync('claude', ['--version']);
  return { ok: true, version: stdout.trim() };
} catch (err) {
  const msg = err instanceof Error ? err.message : String(err);
  return {
    ok: false,
    error: 'Claude Code CLI not found. Install: npm i -g @anthropic-ai/claude-code',
    errorKind: classifyAdapterError(msg),     // NOTE: no exitCode argument
  };
}
```
Test: `'spawn claude ENOENT'` → `errorKind === SPAWN_FAILED`.

#### 2.1.8 `stop(pid)`

```ts
async stop(pid: number): Promise<void> { await this.processManager.killWithGrace(pid); }
```
Default 10 s grace. Tested: `killWithGrace` called with `5555` (no second arg).

Minor note: `AdapterErrorKind` is imported at `claude.ts:11` but never used in that file.

---

### 2.2 codex — `CodexAdapter` (`adapters/codex.ts`), `kind = 'codex'`

Header docstring: "Spawns `codex exec --json -` in headless mode. Prompt is piped via
stdin (avoids CLI arg length limits). Parses JSONL events from stdout into AgentEvent
stream."

#### 2.2.1 argv (`codex.ts:42-53`)

```ts
const args = [
  'exec',
  '--json',
  '--sandbox', 'danger-full-access', // autonomous agents can't respond to approval prompts
];

if (params.config.model) args.push('--model', params.config.model);

// Read prompt from stdin (avoids ARG_MAX limits on long prompts)
args.push('-');
```

Flat form:

```
codex
  exec
  --json
  --sandbox danger-full-access      unconditional
  [--model <config.model>]          iff config.model truthy
  -                                 ALWAYS LAST → prompt read from stdin
```

No `--effort`, no system-prompt flag, no cwd / resume / session / permission-mode flag.

#### 2.2.2 Spawn options

```ts
this.processManager.spawn('codex', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
  stdio: ['pipe', 'pipe', 'pipe'],   // stdin must be 'pipe' to send prompt
});
```

#### 2.2.3 stdin write (`codex.ts:63-66`)

```ts
// Pipe prompt via stdin — prepend system prompt if present (Codex has no native --system-prompt)
if (proc.stdin) {
  proc.stdin.write(buildFullPrompt(params.systemPrompt, params.prompt));
  proc.stdin.end();
}
```

- Exactly one `write`, **no trailing newline**, then `end()`.
- Only `params.systemPrompt` is consulted — `params.config.system_prompt` is **ignored**
  (unlike claude / pi / grok / antigravity).
- Tested: `write('codex prompt')` and `end()` both called; with a system prompt →
  `write('system instructions\n\nuser task')`; without → `write('just the task')`.

#### 2.2.4 Environment

Sets nothing of its own.

#### 2.2.5 stdout parsing

`createStreamingEvents(proc, createCodexEventParser(), 'Codex', params.signal)`.

**Stateful dedup wrapper** (`codex.ts:78-95`):

```ts
function createCodexEventParser(): (line: string) => AgentEvent | null {
  let lastErrorMessage: string | null = null;

  return (line) => {
    const event = parseCodexEvent(line);
    if (!event) return null;

    if (event.type !== 'error') {
      lastErrorMessage = null;
      return event;
    }

    const message = extractErrorMessage(event.data);
    if (message && message === lastErrorMessage) return null;
    lastErrorMessage = message;
    return event;
  };
}
```

- Any non-error event resets `lastErrorMessage` to `null`.
- Consecutive duplicate error *messages* collapse to one.
- The comparison key is `extractErrorMessage(event.data)` where `event.data` is
  `{ message, raw }` — so the probe order `['error','message','detail','reason']`
  returns `message`.

**`parseCodexEvent`** (`codex.ts:97-195`), on `type = (parsed.type as string) ?? ''`:

| `type` | emitted |
|---|---|
| blank/whitespace line | `null` |
| `"thread.started"` | `null` — "Provider lifecycle noise carries no user-facing information." |
| `"turn.started"` | `null` (same) |
| `"item.started"` | `null` (same) |
| `"turn.completed"` | `done`; `data = { ...(typeof parsed.result === 'string' ? { result: parsed.result } : {}), raw: parsed }`; `tokens = extractTokens(parsed)` |
| `"turn.failed"` | `codexErrorEvent(timestamp, parsed, parsed.error ?? parsed, extractTokens(parsed))` |
| `"error"` | `codexErrorEvent(timestamp, parsed, parsed.error ?? parsed)` — no tokens |
| `"item.completed"` | dispatch on `item.type`, table below |
| default | `text = extractItemText(parsed)`; `text ? { output, data: { text, raw: parsed } } : null` |
| JSON.parse throws | `{ type:'output', timestamp, data: <raw line string> }` |

`item.completed` sub-dispatch on `itemType = ((parsed.item as Record<string,unknown>) ?? {}).type ?? ''`:

| `item.type` | emitted |
|---|---|
| `"agent_message"` | `text = typeof item.text === 'string' ? item.text : ''`; `text ? { output, data: { text, raw: item } } : null` |
| `"reasoning"` | `null` |
| `"command_execution"` | `command = typeof item.command === 'string' ? item.command : ''`; `result = typeof item.aggregated_output === 'string' ? item.aggregated_output : undefined`; `command ? { type:'command', data: { command, ...(result !== undefined ? { result } : {}), raw: item } } : null` |
| `"file_change"` | `changes = Array.isArray(item.changes) ? item.changes : []`; `paths = changes.map(c => typeof c.path === 'string' ? c.path : '').filter(Boolean)`; **always** emits `{ type:'file_change', data: { paths, raw: item } }` (even when `paths` is empty) |
| `"tool_use"` or `"mcp_tool_call"` | `name = typeof item.name === 'string' ? item.name : (typeof item.tool === 'string' ? item.tool : itemType)`; `{ type:'tool_call', data: { name, ...('input' in item ? { input: item.input } : {}), raw: item } }` — note `'input' in item` tests **presence**, not type |
| `"web_search"` | `input = { ...(typeof item.query === 'string' ? { query: item.query } : {}), ...(item.action !== undefined ? { action: item.action } : {}) }`; `{ type:'tool_call', data: { name: 'web_search', input, raw: item } }` |
| `"tool_result"` | `text = extractItemText(item)`; `text ? { output, data: { text, raw: item } } : null` |
| `"error"` | `codexErrorEvent(timestamp, item)` — `value` defaults to `item` |
| anything else | `text = extractItemText(item)`; `text ? { output, data: { text, raw: item } } : null` |

Helpers:

```ts
function extractItemText(item: Record<string, unknown>): string {
  if (typeof item.text === 'string') return item.text;
  if (typeof item.content === 'string') return item.content;
  if (typeof item.message === 'string') return item.message;
  return '';
}

function codexErrorEvent(
  timestamp: string,
  raw: Record<string, unknown>,
  value: unknown = raw,
  tokens?: AgentEvent['tokens'],
): AgentEvent {
  const message = extractErrorMessage(value);
  if (isModelMetadataWarning(message)) {
    return { type: 'output', timestamp, data: { text: `⚠ ${message}`, raw } };
  }
  return {
    type: 'error',
    timestamp,
    data: { message, raw },
    ...(tokens ? { tokens } : {}),
    errorKind: classifyAdapterError(message),
  };
}
```

The metadata-warning downgrade is the one place Codex turns an error into `output`.
Prefix is the literal `"⚠ "` (U+26A0 WARNING SIGN followed by one space).

#### 2.2.6 Fixture lines (from `test/unit/infrastructure/codex-adapter.test.ts`)

```json
{"type":"turn.completed","usage":{"input_tokens":200,"output_tokens":80}}
{"type":"turn.failed","error":"bad"}
{"type":"thread.started"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hello"}}
{"type":"item.started","item":{"type":"command_execution","command":"ls","status":"in_progress"}}
{"type":"item.completed","item":{"type":"command_execution","command":"ls","aggregated_output":"file.txt\n","exit_code":0}}
{"type":"item.completed","item":{"type":"file_change","changes":[{"path":"a.ts"},{"path":"b.ts"}]}}
{"type":"item.completed","item":{"type":"tool_use","name":"search","input":{"query":"orch"}}}
{"type":"item.completed","item":{"id":"call_1","type":"web_search","query":"privacy preserving AI","action":{"type":"search","queries":["privacy preserving AI"]}}}
{"type":"item.completed","item":{"type":"error","message":"oops"}}
{"type":"error","error":"fatal"}
{"type":"error","error":"HTTP 401 unauthorized"}
{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Model metadata for `gpt-5.6` not found. Defaulting to fallback metadata; this can degrade performance and cause issues."}}
```
plus the non-JSON line `garbage`, and the dedup pair where
`providerError = JSON.stringify({type:'error',status:400,error:{type:'invalid_request_error',message:"The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account."}})`
is emitted twice:
```json
{"type":"error","message":"<providerError>"}
{"type":"turn.failed","error":{"message":"<providerError>"}}
```

Assertions:
- `turn.completed` → `done` with
  `tokens === {input:200, output:80, reasoning:0, total:280, cache_read:0, cache_write:0}`
- `thread.started` + `turn.started` → `events === []`
- `agent_message` → `output` with `data` matching `{ text:'hello', raw:{ id:'item_0', type:'agent_message', text:'hello' } }`
- `item.started` command_execution dropped; `item.completed` →
  exactly 1 event, `type === 'command'`, `data` matching `{ command:'ls', result:'file.txt\n' }`
- `file_change` → `data.paths === ['a.ts','b.ts']`
- `tool_use` → `tool_call` with `data` matching `{ name:'search', input:{query:'orch'} }`
- `web_search` → `tool_call` with `data` matching `{ name:'web_search', input:{query:'privacy preserving AI'} }`
- `item.completed` error → `error` with `data` matching `{ message:'oops' }`, `errorKind === UNKNOWN`
- top-level `{"type":"error","error":"fatal"}` → `error` with `data` matching `{ message:'fatal' }`
- `{"type":"error","error":"HTTP 401 unauthorized"}` → `errorKind === AUTH_FAILED`
- the dedup pair → **exactly 1** event, `error`, with `data.message ===`
  `"The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account."`
- the metadata-warning item → **exactly 1** event of type `output`, with
  `data.text` containing `` 'Model metadata for `gpt-5.6` not found' ``
- `garbage` → `{ type:'output', data:'garbage' }`
- `close 1` with no events → rejects with `'exited with code 1'`
- Spawn assertion: `'codex'`,
  `expect.arrayContaining(['exec','--json','--sandbox','danger-full-access','-'])`,
  `expect.objectContaining({ cwd:'/tmp/codex-ws', stdio:['pipe','pipe','pipe'] })`

#### 2.2.7 Exit handling
Per §1.6, `adapterName = 'Codex'` → `"Codex process exited with code N"` + tail.

#### 2.2.8 `test()`
```ts
const { stdout } = await execFileAsync('codex', ['--version']);  // ok → { ok:true, version: stdout.trim() }
// fail → { ok:false, error:'Codex CLI not found. Install: npm i -g @openai/codex',
//          errorKind: classifyAdapterError(msg) }
```

#### 2.2.9 `stop(pid)` → `killWithGrace(pid)`.

---

### 2.3 cursor — `CursorAdapter` (`adapters/cursor.ts`), `kind = 'cursor'`

Header docstring: "Spawns `cursor-agent` (Cursor's headless agent CLI) with
`--output-format stream-json`. Falls back to `agent` command if `cursor-agent` is not
found. … Note: This requires Cursor Agent CLI, not the regular `cursor` IDE command.
Install via: npm i -g @anthropic-ai/cursor-agent (when available)"

#### 2.3.1 Binary resolution — per-instance mutable state

```ts
/** Try multiple command names and return the first that works */
async function findCommand(): Promise<{ command: string; version: string } | null> {
  for (const cmd of ['cursor-agent', 'agent']) {
    try {
      const { stdout } = await execFileAsync(cmd, ['--version']);
      return { command: cmd, version: stdout.trim() };
    } catch {
      // try next
    }
  }
  return null;
}

export class CursorAdapter implements IAgentAdapter {
  readonly kind = 'cursor';
  private resolvedCommand: string = 'cursor-agent';
  ...
}
```

- Candidate order is exactly `['cursor-agent', 'agent']`, each probed with `['--version']`.
- `resolvedCommand` is **only** assigned inside `test()`. If `test()` was never called
  (or failed), `execute()` spawns `'cursor-agent'`.

#### 2.3.2 argv (`cursor.ts:57-70`)

```ts
// Cursor print mode requires the prompt as a positional argument. Unlike
// Codex, it does not support reading the prompt from stdin.
const fullPrompt = buildFullPrompt(params.systemPrompt, params.prompt);
const args = [
  '-p',
  '--output-format', 'stream-json',
  '--workspace', params.workspace,
  '--yolo',   // bypass interactive prompts for autonomous agents
  '--trust',  // ORCH creates fresh worktrees that have not been trusted interactively
];

if (params.config.model) args.push('--model', params.config.model);

args.push(fullPrompt);
```

Flat form:

```
<resolvedCommand>                   # 'cursor-agent' or 'agent'
  -p
  --output-format stream-json
  --workspace <params.workspace>
  --yolo
  --trust
  [--model <config.model>]          iff config.model truthy
  <fullPrompt>                      positional, ALWAYS LAST
```

- `fullPrompt = buildFullPrompt(params.systemPrompt, params.prompt)`;
  `params.config.system_prompt` is **ignored**.
- `--workspace` carries the workspace **in addition to** spawn `cwd`.
- The test asserts the argv array **exactly** (not `arrayContaining`):
  `['-p','--output-format','stream-json','--workspace','/tmp/cursor-ws','--yolo','--trust','cursor prompt']`

#### 2.3.3 Spawn options

```ts
this.processManager.spawn(this.resolvedCommand, args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
  stdio: ['ignore', 'pipe', 'pipe'],
});
```
Test asserts nothing is written to stdin.

#### 2.3.4 Environment
Sets nothing of its own.

#### 2.3.5 stdout parsing — `parseCursorEvent` (`cursor.ts:89-119`)

Comment in source: "Cursor stream-json uses the same format as Claude stream-json".
The switch is identical to claude's **except the `result` case omits `statsFallback`**:

| `type` | emitted | `data` |
|---|---|---|
| blank line | `null` | — |
| `"assistant"` | `output` | `parsed.message ?? parsed` |
| `"tool_use"` | `tool_call` | whole `parsed` |
| `"tool_result"` | `output` | whole `parsed` |
| `"error"` | `error` | `parsed.error ?? parsed`; `errorKind = classifyAdapterError(string-or-JSON.stringify)` |
| `"result"` | `done` | whole `parsed`; `tokens = extractTokens(parsed)` — **no `stats.usage` fallback** |
| default | `output` | whole `parsed` |
| JSON.parse throws | `output` | raw line string |

Generator: `createStreamingEvents(proc, parseCursorEvent, 'Cursor agent', params.signal)`.

#### 2.3.6 Fixture lines (from `test/unit/infrastructure/cursor-adapter.test.ts`)

```json
{"type":"assistant","message":"hi"}
{"type":"tool_use","name":"edit"}
{"type":"error","error":"cursor error"}
{"type":"result","usage":{"inputTokens":50,"outputTokens":25,"cacheReadTokens":10,"cacheWriteTokens":5}}
{"type":"tool_result","content":"ok"}
{"type":"result"}
```
plus the non-JSON line `bad line`.

Assertions:
- `assistant` → `output` with `data === 'hi'`
- camelCase `usage` → `done` with
  `tokens === {input:50, output:25, reasoning:0, total:75, cache_read:10, cache_write:5}`
  (this is the camelCase-alias path of `extractTokens`)
- `error` → `data === 'cursor error'`
- `tool_result` → `output`
- `{"type":"result"}` (no usage) → still `done`, `tokens` undefined, and it
  **suppresses** the non-zero-exit error
- `bad line` → `output` with `data === 'bad line'`
- stderr `Error: No prompt provided for print mode\n` written and `close 1` emitted
  **synchronously before iteration** → rejects with a message matching
  `/No prompt provided for print mode/`

#### 2.3.7 Exit handling
Per §1.6, `adapterName = 'Cursor agent'` →
`"Cursor agent process exited with code N"` + `--- Cursor agent stderr (tail) ---`.

#### 2.3.8 `test()` (`cursor.ts:41-52`)

```ts
const found = await findCommand();
if (found) {
  this.resolvedCommand = found.command;
  return { ok: true, version: found.version };
}
return {
  ok: false,
  error: 'Cursor Agent CLI not found. The headless agent CLI is required (cursor-agent or agent).',
  errorKind: AdapterErrorKind.ADAPTER_NOT_FOUND,     // HARD-CODED, not classified
};
```
Cursor is the **only** adapter whose `test()` hard-codes the error kind instead of
calling `classifyAdapterError`.

#### 2.3.9 `stop(pid)` → `killWithGrace(pid)`.

---

### 2.4 opencode — `OpenCodeAdapter` (`adapters/opencode.ts`), `kind = 'opencode'`

Header docstring: "Spawns `opencode run --format json` in headless mode. Parses JSONL
events from stdout into AgentEvent stream."

#### 2.4.1 argv (`opencode.ts:38-49`)

```ts
const args = ['run', '--format', 'json'];

if (params.config.model) args.push('--model', params.config.model);

// OpenCode has no native --system-prompt; prepend to user prompt
const fullPrompt = buildFullPrompt(params.systemPrompt, params.prompt);
args.push(fullPrompt);
```

Flat form:

```
opencode
  run
  --format json
  [--model <config.model>]   iff config.model truthy
  <fullPrompt>               positional, ALWAYS LAST
```

- `params.config.system_prompt` ignored.
- **No `--dir` flag** — workspace is conveyed only via spawn `cwd`. Test explicitly
  asserts `args` does **not** contain `'--dir'`, with comment
  "No --dir flag — workspace is set via cwd".

#### 2.4.2 Spawn options

```ts
this.processManager.spawn('opencode', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
});
```
Default stdio (stdin ignored).

#### 2.4.3 Environment
Sets nothing of its own.

#### 2.4.4 stdout parsing — `parseOpenCodeEvent` (`opencode.ts:67-114`)

```ts
const parsed: Record<string, unknown> = JSON.parse(line);
const timestamp = new Date().toISOString();
const type = (parsed.type as string) ?? '';
const part = (parsed.part as Record<string, unknown>) ?? {};
```

| `type` | emitted | detail |
|---|---|---|
| blank line | `null` | |
| `"step_start"` | `null` | "lifecycle event — no user-visible content" |
| `"text"` | `output` | `data = part.text ?? part` — **the bare string**, not `{text}` |
| `"tool_use"` | `error` or `tool_call` | `state = (part.state as Record<string,unknown>) ?? {}`. If `state.status === 'error'`: `errMsg = typeof state.error === 'string' ? state.error : JSON.stringify(state)`; `{ type:'error', data: state, errorKind: classifyAdapterError(errMsg) }`. Else: `{ type:'tool_call', data: { name: part.tool, input: state.input } }` — comment "Map to { name, input } shape expected by TUI formatToolInput"; **no `raw`** |
| `"step_finish"` | `error`/`null`/`done` | `reason = part.reason as string \| undefined`; `tokens = extractOpenCodeTokens(part)`. `reason === 'error'` → `errMsg = typeof part.error === 'string' ? part.error : JSON.stringify(part)`; `{ type:'error', data: part, tokens, errorKind: classifyAdapterError(errMsg) }`. `reason === 'tool-calls'` → `null` ("intermediate lifecycle — tool_use events carry the actual content"). Otherwise (`'stop'`, `'max_tokens'`, **or any other terminal reason including `undefined`**) → `{ type:'done', data: part, tokens }` |
| default | `output` | `data = parsed` (whole object) |
| JSON.parse throws | `output` | raw line string |

**Opencode-specific token extractor** (`opencode.ts:117-125`) — flat keys under `part.tokens`:

```ts
/** Extract token usage from opencode step_finish part. */
function extractOpenCodeTokens(part: Record<string, unknown>): TokenUsage | undefined {
  const tokens = part.tokens as Record<string, unknown> | undefined;
  if (!tokens || typeof tokens.input !== 'number') return undefined;

  const input = tokens.input;
  const output = typeof tokens.output === 'number' ? tokens.output : 0;
  const reasoning = typeof tokens.reasoning === 'number' ? tokens.reasoning : 0;
  return createTokenUsage(input, output, { reasoning });
}
```

Literal keys: `part.tokens.input`, `part.tokens.output`, `part.tokens.reasoning`.
No cache fields ⇒ `cache_read` and `cache_write` are always `0`.
The provider's own `part.tokens.total` is **ignored** — `total` is recomputed.

Generator: `createStreamingEvents(proc, parseOpenCodeEvent, 'OpenCode', params.signal)`.

#### 2.4.5 Fixture lines (from `test/unit/infrastructure/opencode-adapter.test.ts`)

Note the capitalisation of `sessionID`.

```json
{"type":"text","timestamp":1234,"sessionID":"ses_1","part":{"type":"text","text":"hello world"}}
{"type":"step_start","timestamp":1234,"sessionID":"ses_1","part":{"type":"step-start","snapshot":"abc123"}}
{"type":"text","part":{"text":"hello"}}
{"type":"tool_use","timestamp":1234,"sessionID":"ses_1","part":{"type":"tool","tool":"write","state":{"status":"completed","input":{"filePath":"/tmp/test.txt","content":"test"}}}}
{"type":"tool_use","timestamp":1234,"sessionID":"ses_1","part":{"type":"tool","tool":"write","state":{"status":"error","error":"The user rejected permission"}}}
{"type":"step_finish","timestamp":1234,"sessionID":"ses_1","part":{"type":"step-finish","reason":"stop","tokens":{"total":14502,"input":12416,"output":2,"reasoning":0},"cost":0}}
{"type":"step_finish","timestamp":1234,"sessionID":"ses_1","part":{"type":"step-finish","reason":"tool-calls","tokens":{"total":500,"input":400,"output":100}}}
{"type":"step_finish","timestamp":1234,"sessionID":"ses_1","part":{"type":"step-finish","reason":"error","error":"model overloaded"}}
{"type":"step_finish","timestamp":1234,"sessionID":"ses_1","part":{"type":"step-finish","reason":"max_tokens","tokens":{"total":8000,"input":7000,"output":1000}}}
{"type":"step_finish","part":{"reason":"stop","tokens":{"total":100,"input":80,"output":20}}}
{"type":"text","part":{"text":"hi"}}
{"type":"unknown_type","data":"info"}
```
plus `not json at all` and a `"\n\n"` write.

Assertions:
- `text` → `output` with `data === 'hello world'` (bare string)
- `step_start` then a `text` → exactly 1 event
- completed `tool_use` → `tool_call`, `data.name === 'write'`,
  `data.input === { filePath:'/tmp/test.txt', content:'test' }`
- errored `tool_use` → `error`
- `reason:'stop'` with provider `total:14502` → `done` with
  `tokens === {input:12416, output:2, reasoning:0, total:12418, cache_read:0, cache_write:0}`
  — **note the recomputed total (12418), not the provider's 14502**
- `reason:'tool-calls'` → dropped (exactly 1 event remains)
- `reason:'error'` → `error`
- `reason:'max_tokens'` → `done` with
  `tokens === {input:7000, output:1000, reasoning:0, total:8000, cache_read:0, cache_write:0}`
- `{"type":"unknown_type","data":"info"}` → `output`
- `not json at all` → `output` with `data === 'not json at all'`
- `close 1` with no events → rejects `'exited with code 1'`;
  a `step_finish reason:'stop'` before `close 1` → no throw, `events[0].type === 'done'`
- Spawn assertion: `'opencode'`,
  `expect.arrayContaining(['run','--format','json','test prompt'])`,
  `expect.objectContaining({ cwd:'/tmp/workspace' })`

#### 2.4.6 Exit handling
Per §1.6, `adapterName = 'OpenCode'` → `"OpenCode process exited with code N"`.

#### 2.4.7 `test()`
```ts
const { stdout } = await execFileAsync('opencode', ['--version']);  // ok → { ok:true, version: stdout.trim() }
// fail → { ok:false, error:'OpenCode CLI not found. Install: npm i -g opencode',
//          errorKind: classifyAdapterError(msg) }
```

#### 2.4.8 `stop(pid)` → `killWithGrace(pid)`.

---

### 2.5 pi — `PiAdapter` (`adapters/pi.ts`), `kind = 'pi'`

Header docstring: "Spawns `pi --mode rpc` in headless RPC mode. Sends the ORCH prompt
as a JSONL `prompt` command over stdin and maps Pi RPC events from stdout into ORCH
AgentEvents. Pi extensions/skills/context remain enabled by default; UI-only extension
events are ignored."

This is the most complex adapter: a **long-lived bidirectional JSONL RPC session** that
the adapter itself terminates. It does **not** use `createStreamingEvents` or `readLines`.

#### 2.5.1 argv (`pi.ts:43-60`)

```ts
const args = ['--mode', 'rpc'];

if (params.config.model)  args.push('--model', params.config.model);
if (params.config.effort) args.push('--thinking', params.config.effort);

// Preserve Pi's own coding-agent harness prompt. ORCH's system prompt is
// appended as additional project/task governance rather than replacing it.
const effectiveSystemPrompt = params.systemPrompt ?? params.config.system_prompt;
if (effectiveSystemPrompt) args.push('--append-system-prompt', effectiveSystemPrompt);
```

Flat form:

```
pi
  --mode rpc
  [--model <config.model>]                            iff config.model truthy
  [--thinking <config.effort>]                        iff config.effort truthy   ← --thinking, NOT --effort
  [--append-system-prompt <effectiveSystemPrompt>]    iff (params.systemPrompt ?? config.system_prompt) truthy
```

- **`--append-system-prompt`, never `--system-prompt`.** Test asserts
  `args` does **not** contain `'--system-prompt'`.
- No `--no-session` flag — test asserts its absence
  ("session persistence enabled by default"; extensions/skills/context stay on).
- No positional prompt, no cwd flag, no sandbox flag, no max-turns flag.

#### 2.5.2 Spawn options

```ts
this.processManager.spawn('pi', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
  stdio: ['pipe', 'pipe', 'pipe'],
});
```

#### 2.5.3 stdin — one JSONL RPC command, stdin left OPEN (`pi.ts:73-87`)

```ts
if (proc.stdin) {
  proc.stdin.write(JSON.stringify({
    id: `orch-${Date.now()}`,
    type: 'prompt',
    message: params.prompt,
  }) + '\n');
  // DO NOT call proc.stdin.end() here. Pi --mode rpc is a long-lived
  // persistent session: it sends a prompt preflight response, then drives
  // the LLM call asynchronously, streaming message_update / turn_end /
  // agent_end as the model responds. Closing stdin after the write breaks
  // that pipeline — verified on pi-coding-agent 0.73.1: pi stalls right
  // after the user-message_end event and never produces an assistant turn.
  // We terminate the long-lived process via processManager.killWithGrace
  // immediately after the terminal `done` event (see createPiRpcEvents).
}
```

- JSON keys are exactly `id`, `type`, `message`.
- `id` is literally `` `orch-${Date.now()}` `` (ms epoch).
- `type` is the literal `'prompt'`.
- `message` is `params.prompt` **alone** — the system prompt went into argv.
- Trailing `'\n'` is required.
- Tests assert: exactly 1 write; the written line `endsWith('\n')`;
  `JSON.parse(line)` matches `{ type:'prompt', message:'do work' }`;
  `stdin.end` is **never** called.

#### 2.5.4 Environment
Sets nothing of its own.

#### 2.5.5 stderr
`const stderrTail = createStderrTailCapture(proc.stderr);` installed **in `execute()`**
(before the generator is created). Comment: "Capture stderr tail so auth/extension-load
errors surface in non-zero exits rather than being silently drained. Drains backpressure
at the same time."

#### 2.5.6 Private line reader `readPiRpcLines` (`pi.ts:589-625`)

Docstring: "Pi RPC can emit very large JSONL records (notably agent_end with the full
message transcript). Do not use the generic process `readLines` helper: it caps lines at
16 KB, which corrupts large JSON records before the adapter can parse the terminal event.
Same algorithm otherwise … Concats once per chunk arrival and scans with an offset to
avoid the O(n²) 'concat([pending, buf]) per chunk' anti-pattern."

Two deliberate differences from `readLines`:
1. **No 16 KB cap.** (Test: a 20 000-char `agent_end` must round-trip intact.)
2. **Strips a trailing `\r`**:
   `yield line.endsWith('\r') ? line.slice(0, -1) : line;` — both in the scan loop and
   on the final flush.

Zero-length lines are still dropped (`if (newlineIdx > offset)`).

#### 2.5.7 Intermediate types

```ts
interface ParseState {
  finalText: string;
  lastTokens?: TokenUsage;
}

interface ParsedPiEvent {
  agentEvent?: AgentEvent;
  finalText?: string;
  tokens?: TokenUsage;
  completion?: PiCompletion;
  settled?: boolean;
}

interface PiCompletion {
  result: string;
  raw: Record<string, unknown>;
  tokens?: TokenUsage;
  waitForSettled: boolean;
  failure?: {
    message: string;
    errorKind: ReturnType<typeof classifyAdapterError>;
  };
}

class PiTerminalError extends Error {
  constructor(message: string, readonly errorKind: ReturnType<typeof classifyAdapterError>) {
    super(message);
    this.name = 'PiTerminalError';
  }
}
```

#### 2.5.8 `parsePiRpcEvent(line, state)` (`pi.ts:256-347`)

Non-JSON line →
`{ agentEvent: { type:'output', timestamp, data: { text: line } } }`
(note the `{text}` wrapper, unlike claude/codex/cursor/opencode which pass a bare string).

Dispatch on `type = typeof parsed.type === 'string' ? parsed.type : ''`:

| `parsed.type` | handling |
|---|---|
| `"extension_ui_request"`, `"agent_start"`, `"turn_start"`, `"message_start"`, `"message_end"`, `"turn_end"`, `"queue_update"`, `"compaction_start"`, `"compaction_end"`, `"auto_retry_start"`, `"auto_retry_end"` | `extractPassiveUpdate(parsed)` → `tokens ? { tokens } : null`. **No AgentEvent emitted**; only harvests `parsed.usage` into `lastTokens`. |
| `"response"` | if `parsed.success === false`: `message = typeof parsed.error === 'string' ? parsed.error : JSON.stringify(parsed)` → `{ completion: { result:'', raw: parsed, waitForSettled: false, failure: { message, errorKind: classifyAdapterError(message) } } }`. Otherwise `null`. |
| `"message_update"` | `parseMessageUpdate(parsed, timestamp, state)` |
| `"tool_execution_start"` | `{ agentEvent: { type:'tool_call', timestamp, data: { name: parsed.toolName, input: parsed.args, raw: parsed } } }` |
| `"tool_execution_update"` | `null` — "Intermediate progress: pi emits one of these per chunk of tool output (e.g. streaming bash stdout). Other adapters don't surface this in their event streams — drop it so the canonical contract holds." |
| `"tool_execution_end"` | `parseToolExecutionEnd(parsed, timestamp)` |
| `"agent_end"` | `completion = parsePiCompletion(parsed, state)` → `{ finalText: completion.result, tokens: completion.tokens, completion }` |
| `"agent_settled"` | `{ settled: true }` |
| `"extension_error"` | `message = typeof parsed.message === 'string' ? parsed.message : JSON.stringify(parsed)` → `{ agentEvent: { type:'error', timestamp, data: { message, raw: parsed }, errorKind: classifyAdapterError(message) } }` |
| **anything else** | `null` — "Unknown pi event types are silently dropped: they would otherwise pollute logs with raw RPC envelopes the renderer can't summarize. Adding a known type above is the right way to surface a new event." **Contrast every other adapter, which defaults to `output`.** |

```ts
function extractPassiveUpdate(parsed: Record<string, unknown>): ParsedPiEvent | null {
  const tokens = extractPiTokensFromMessage(parsed);
  return tokens ? { tokens } : null;
}
```

#### 2.5.9 `parseMessageUpdate` (`pi.ts:349-390`)

`updateType = typeof parsed.assistantMessageEvent?.type === 'string' ? … : ''`

- `"text_delta"` → `delta = typeof ev.delta === 'string' ? ev.delta : ''`;
  returns `{ finalText: state.finalText + delta }`. **Emits nothing.**
  Comment: "Aggregate deltas into state.finalText; emit nothing here. A long LLM
  response is hundreds of deltas — flushing one event per delta floods the activity
  feed with character-level fragments. text_end emits the full message in one canonical
  `output` event below."
- `"text_end"` → `content = typeof ev.content === 'string' ? ev.content : state.finalText`;
  if `!content` return `{ finalText: '' }` (reset only);
  else `{ finalText: '', agentEvent: { type:'output', timestamp, data: { text: content } } }`.
  Comment: "Reset the delta buffer so a follow-up assistant message in the same pi
  session doesn't get concatenated onto this one (agent_end uses its own messages[]
  extraction, not state.finalText, so this is safe to drop)."
- `"error"` → message resolved in this exact fallback chain:
  ```ts
  extractPiMessageError(assistantMessageEvent?.error)
    ?? extractPiMessageError(parsed.message)
    ?? (typeof assistantMessageEvent?.reason === 'string' ? assistantMessageEvent.reason : undefined)
    ?? JSON.stringify(parsed)
  ```
  → `{ agentEvent: { type:'error', timestamp, data: { message: errorMessage, raw: parsed }, errorKind: classifyAdapterError(errorMessage) } }`
- anything else → `null`

```ts
function extractPiMessageError(value: unknown): string | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const record = value as Record<string, unknown>;
  if (typeof record.errorMessage === 'string' && record.errorMessage.trim()) {
    return record.errorMessage.trim();
  }
  return undefined;
}
```

#### 2.5.10 `parseToolExecutionEnd` (`pi.ts:392-432`)

```ts
const toolName = typeof parsed.toolName === 'string' ? parsed.toolName : '';
const args = parsed.args as Record<string, unknown> | undefined;
const resultText = extractToolResultText(parsed.result);
```

1. `parsed.isError === true` (strict equality) →
   `message = resultText || JSON.stringify(parsed.result ?? parsed)` →
   `{ type:'error', data: { message, raw: parsed }, errorKind: classifyAdapterError(message) }`
2. `toolName === 'bash'` (exact, case-sensitive) →
   `command = typeof args?.command === 'string' ? args.command : JSON.stringify(args ?? {})` →
   `{ type:'command', data: { command, result: resultText, raw: parsed } }`
3. `/^(write|edit)$/i.test(toolName)` **and** a path is found →
   `{ type:'file_change', data: { paths: [path], raw: parsed } }`.
   If no path is found, falls through to 4.
4. fallback → `summary = resultText || \`${toolName || 'tool'} completed\`` →
   `{ type:'output', data: { text: summary, raw: parsed } }`.
   Comment: "Generic tool result (read, grep, ls, …) — surface the result text so logs
   show what came back, not the raw RPC envelope."

```ts
function extractPath(args: Record<string, unknown> | undefined): string | undefined {
  if (!args) return undefined;
  if (typeof args.path === 'string') return args.path;
  if (typeof args.file_path === 'string') return args.file_path;
  return undefined;
}

/** Pi tool results wrap content arrays in `{ content: [...] }`; messages don't. */
function extractToolResultText(result: unknown): string {
  if (typeof result === 'string') return result;
  if (!result || typeof result !== 'object') return '';
  return extractTextFromContent((result as { content?: unknown }).content) ?? '';
}

function extractTextFromContent(content: unknown): string | undefined {
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return undefined;
  const parts = content
    .map((part) => {
      const p = part as Record<string, unknown>;
      return typeof p.text === 'string' ? p.text : '';
    })
    .filter(Boolean);
  return parts.length ? parts.join('') : undefined;   // joined with '' — NO separator
}
```

#### 2.5.11 `parsePiCompletion` (`pi.ts:446-476`)

```ts
const assistantMessage = extractLastAssistantMessage(parsed);
const result = extractTextFromContent(assistantMessage?.content) ?? state.finalText;
const tokens = extractPiTokensFromMessage(assistantMessage ?? {}) ?? state.lastTokens;
const stopReason = typeof assistantMessage?.stopReason === 'string' ? assistantMessage.stopReason : undefined;
const errorMessage = typeof assistantMessage?.errorMessage === 'string'
  ? assistantMessage.errorMessage.trim()
  : '';

const completion: PiCompletion = {
  result,
  raw: parsed,
  tokens,
  // Current Pi RPC emits agent_settled after agent_end and exposes
  // willRetry on agent_end. Older Pi versions have neither field, where
  // agent_end remains the terminal event.
  waitForSettled: Object.prototype.hasOwnProperty.call(parsed, 'willRetry'),
};

if (stopReason === 'error' || stopReason === 'aborted') {
  const message = errorMessage || result || `Pi agent stopped with reason: ${stopReason}`;
  completion.failure = { message, errorKind: classifyAdapterError(message) };
}

return completion;
```

```ts
function extractLastAssistantMessage(parsed: Record<string, unknown>): Record<string, unknown> | undefined {
  const messages = parsed.messages;
  if (!Array.isArray(messages)) return undefined;

  for (let i = messages.length - 1; i >= 0; i--) {     // BACKWARD scan
    const message = messages[i] as Record<string, unknown>;
    if (message.role === 'assistant') return message;
  }
  return undefined;
}
```

Literal JSON keys consumed here: `parsed.messages[]`, `.role === 'assistant'`,
`.content`, `.stopReason`, `.errorMessage`, `.usage`, and the top-level `willRetry`
(presence only).

**`waitForSettled` is key-presence, not value** — `willRetry: false` still sets it `true`.

#### 2.5.12 Pi token aliases (`pi.ts:549-577`)

```ts
// Pi exposes usage under multiple key shapes across versions and across the
// `message_*` vs `agent_end` paths — Pi-native names, snake_case shims, and
// Anthropic-style names when Pi forwards Claude usage records unchanged.
// First match per field wins, in this order.
const PI_TOKEN_ALIASES: Record<'input' | 'output' | 'reasoning' | 'cache_read' | 'cache_write', readonly string[]> = {
  input:       ['input', 'input_tokens'],
  output:      ['output', 'output_tokens'],
  reasoning:   ['reasoning', 'reasoning_tokens'],
  cache_read:  ['cacheRead', 'cache_read', 'cache_read_input_tokens'],
  cache_write: ['cacheWrite', 'cache_write', 'cache_creation_input_tokens'],
};

function extractPiTokensFromMessage(parsed: Record<string, unknown>): TokenUsage | undefined {
  const usage = parsed.usage as Record<string, unknown> | undefined;
  if (!usage) return undefined;

  const pick = (keys: readonly string[]): number => {
    for (const k of keys) {
      const v = usage[k];
      if (typeof v === 'number') return v;
    }
    return 0;
  };

  const input = pick(PI_TOKEN_ALIASES.input);
  const output = pick(PI_TOKEN_ALIASES.output);
  const reasoning = pick(PI_TOKEN_ALIASES.reasoning);
  const cache_read = pick(PI_TOKEN_ALIASES.cache_read);
  const cache_write = pick(PI_TOKEN_ALIASES.cache_write);

  if (input === 0 && output === 0 && reasoning === 0 && cache_read === 0 && cache_write === 0) return undefined;
  return createTokenUsage(input, output, { reasoning, cache_read, cache_write });
}
```

Fields default to `0`; **if all five are 0 the function returns `undefined`**, not a
zero `TokenUsage`. `usage.totalTokens` is ignored.

#### 2.5.13 `toTerminalPiEvent` (`pi.ts:478-504`)

```ts
if (completion.failure) {
  const { message, errorKind } = completion.failure;
  return {
    event: { type: 'error', timestamp, data: { message, raw: completion.raw }, errorKind },
    error: new PiTerminalError(message, errorKind),
  };
}
return {
  event: { type: 'done', timestamp, data: { result: completion.result, raw: completion.raw }, tokens: completion.tokens },
};
```

#### 2.5.14 Generator `createPiRpcEvents(proc, pid, processManager, stderrTail, signal)` (`pi.ts:98-220`)

Local state: `gotDoneEvent`, `streamErrorYielded`, `finalText = ''`, `lastTokens`,
`pendingCompletion`, `exitCode = null`, `exitError = null`.

Lifecycle listeners use `proc.on` (not `once`):
```ts
const exitPromise = new Promise<void>((resolve) => {
  proc.on('close', (code) => { exitCode = code; resolve(); });
  proc.on('error', (err) => { exitError = err; resolve(); });
});
```

Main loop per parsed record:
1. `if (signal?.aborted) break;`
2. `if (event.finalText !== undefined) finalText = event.finalText;`
3. `if (event.tokens) lastTokens = event.tokens;`
4. `if (event.completion) pendingCompletion = event.completion;`
5. If `event.agentEvent`: `done` sets `gotDoneEvent`; `yield` it; if it was `done` →
   `await processManager.killWithGrace(pid, 1_000).catch(() => {})` then `return`.
   Comment: "Pi RPC is a long-lived process. ORCH tasks are one-shot runs, so stop Pi
   after the terminal event instead of waiting forever."
6. Terminal-completion selection:
   ```ts
   const terminalCompletion = event.settled
     ? pendingCompletion
     : event.completion && !event.completion.waitForSettled
       ? event.completion
       : undefined;
   ```
   i.e. `agent_settled` promotes whatever completion is pending; an `agent_end`
   **without** a `willRetry` key is itself terminal; an `agent_end` **with** `willRetry`
   (either value) waits for `agent_settled`.
7. If terminal: `terminal = toTerminalPiEvent(terminalCompletion)`;
   `done` sets `gotDoneEvent`; `yield terminal.event`;
   `await processManager.killWithGrace(pid, 1_000).catch(() => {})`;
   `if (terminal.error) throw terminal.error;` else `return`.

Catch around the stdout loop:
- `PiTerminalError` → **rethrown**. Comment: "Terminal provider failures are
  deliberately thrown after their canonical error event is yielded. Let the
  orchestrator's failure path handle retry/failed state instead of weakening them into
  a stdout transport error or a successful generator completion."
- Any other error (stdout `'error'` such as ECONNRESET/EPIPE) →
  `streamError = err instanceof Error ? err : new Error(String(err))`;
  if `!signal?.aborted && !gotDoneEvent` → `streamErrorYielded = true` and
  ```ts
  yield {
    type: 'error',
    timestamp: new Date().toISOString(),
    data: { message: streamError.message },
    errorKind: classifyAdapterError(streamError.message),
  };
  ```

`finally`:
```ts
proc.stdout?.destroy();
// Pi RPC is long-lived. If we leave via abort/break without a 'done' event,
// the process is still alive — kill it so exitPromise resolves and the
// generator doesn't pin the ChildProcess via dangling 'close' / 'error' listeners.
if (!gotDoneEvent && (signal?.aborted || streamError)) {
  processManager.killWithGrace(pid, 1_000).catch(() => {});   // not awaited
}
```

After the loop: `await exitPromise;` then in order:
1. ```ts
   // streamError was already surfaced as an error event — don't double-report.
   if (streamErrorYielded && streamError) {
     const classified = classifyAdapterError(streamError.message, exitCode ?? undefined);
     throw Object.assign(streamError, { errorKind: classified });   // reuses the SAME Error object
   }
   ```
2. ```ts
   const spawnError = exitError as Error | null;
   if (spawnError && !signal?.aborted && !gotDoneEvent) {
     const message = appendStderrTail(spawnError.message, 'pi', stderrTail());
     const classified = classifyAdapterError(message, exitCode ?? undefined);
     throw Object.assign(new Error(message), { errorKind: classified });
   }
   ```
3. ```ts
   if (exitCode !== 0 && exitCode !== null && !signal?.aborted && !gotDoneEvent) {
     const baseMsg = `Pi process exited with code ${exitCode}`;
     const message = appendStderrTail(baseMsg, 'pi', stderrTail());
     const classified = classifyAdapterError(message, exitCode);
     throw Object.assign(new Error(message), { errorKind: classified });
   }
   ```

The stderr-tail label is lowercase `'pi'`, so the separator line is
`--- pi stderr (tail) ---`.

#### 2.5.15 Fixture lines (from `test/unit/infrastructure/pi-adapter.test.ts`)

```json
{"type":"extension_ui_request","method":"setTitle","title":"noise"}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hello"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":" world"}}
{"type":"message_update","assistantMessageEvent":{"type":"text_end","content":"hello world"}}
{"type":"tool_execution_update","toolName":"bash","result":{"content":[{"type":"text","text":"partial output…"}]}}
{"type":"tool_execution_start","toolName":"read","args":{"path":"a.ts"}}
{"type":"tool_execution_end","toolName":"read","result":{"content":[{"type":"text","text":"ok"}]},"isError":false}
{"type":"tool_execution_end","toolName":"bash","args":{"command":"npm test"},"result":{"content":[{"type":"text","text":"passed"}],"details":{"exitCode":0}},"isError":false}
{"type":"tool_execution_end","toolName":"write","args":{"path":"src/new.ts"},"result":{"content":[{"type":"text","text":"written"}]},"isError":false}
{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"done text"}],"usage":{"input":10,"output":4,"totalTokens":14}}]}
{"type":"response","command":"prompt","success":false,"error":"bad request"}
{"type":"agent_end","messages":[{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Permission denied: Reached overall message rate limit. Please try again later."}]}
{"type":"agent_settled"}
{"type":"auto_retry_start","attempt":1}
{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"partial"}}
```

Assertions:
- `extension_ui_request` → `events === []`
- the two `text_delta` + one `text_end` → **exactly 1** event,
  `type === 'output'`, `data === { text: 'hello world' }`
- `tool_execution_update` → `events === []`
- `tool_execution_start` read → `tool_call`, `data.name === 'read'`;
  then `tool_execution_end` read → `output`
- `tool_execution_end` bash → `command`, `data.command === 'npm test'`
- `tool_execution_end` write → `file_change`, `data.paths === ['src/new.ts']`
- `agent_end` (no `willRetry`) → last event `done`, `data.result === 'done text'`,
  `tokens === {input:10, output:4, reasoning:0, total:14, cache_read:0, cache_write:0}`
- a 20 000-char (`'x'.repeat(20_000)`) `agent_end` text round-trips exactly into
  `data.result` — proves the no-cap reader
- `response success:false` → yields **1** `error` event (`errorKind === UNKNOWN`) **and**
  the generator rejects with `{ message:'bad request', errorKind: UNKNOWN }`;
  no `done` event; `killWithGrace` called with `(proc.pid, 1_000)`
- `agent_end` with `stopReason:'error'` + rate-limit `errorMessage` → 1 `error` event
  (`errorKind === RATE_LIMIT`, `data.message === message`) **and** rejects with the same
  message/kind; no `done`; `killWithGrace(pid, 1_000)`
- Auto-retry sequence (asserted to produce **exactly one** `done`):
  ```json
  {"type":"agent_end","willRetry":true,"messages":[{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Rate limit exceeded; retrying"}]}
  {"type":"auto_retry_start","attempt":1}
  {"type":"agent_end","willRetry":false,"messages":[{"role":"assistant","content":[{"type":"text","text":"Recovered after retry."}],"stopReason":"stop","usage":{"input":12,"output":5}}]}
  {"type":"agent_settled"}
  ```
  → 1 event `{ type:'done', data:{ result:'Recovered after retry.' } }`,
  `tokens === {input:12, output:5, reasoning:0, total:17, cache_read:0, cache_write:0}`,
  `killWithGrace(pid, 1_000)`.
  **Note**: the second `agent_end` has `willRetry:false` yet `waitForSettled` is `true`
  (key present), so `agent_settled` is what promotes it.
- Exhausted-retries sequence:
  ```json
  {"type":"agent_end","willRetry":false,"messages":[{"role":"assistant","content":[],"stopReason":"error","errorMessage":"Request timed out after retries"}]}
  {"type":"agent_settled"}
  ```
  → 1 `error` event with `errorKind === TIMEOUT`, then rejects with the same;
  no `done`
- stdout `'error'` (`new Error('ECONNRESET')`) before any line, then `close 1` →
  1 `error` event whose `data.message` contains `'ECONNRESET'`, then rejects with
  `{ message:'ECONNRESET', errorKind: PROCESS_CRASH }`, plus `killWithGrace(pid, 1_000)`
- Abort before `done` (one `text_delta` then `controller.abort()`, `close null`) →
  generator completes without throwing, `killWithGrace(pid, 1_000)` called
- stderr `'error: failed to load extension foo\n'` + `close 2` →
  rejects with a message matching `/failed to load extension foo/`
- Spawn assertion: `'pi'`, `expect.arrayContaining(['--mode','rpc'])`,
  `expect.objectContaining({ cwd:'/tmp/pi-ws', stdio:['pipe','pipe','pipe'] })`;
  `args` does **not** contain `'--no-session'`

#### 2.5.16 `test()` (`pi.ts:23-40`) — hand-rolled, not `promisify`

```ts
try {
  const stdout = await new Promise<string>((resolve, reject) => {
    execFile('pi', ['--version'], (err, out) => {
      if (err) reject(err);
      else resolve(out);
    });
  });
  return { ok: true, version: stdout.trim() };
} catch (err) {
  const msg = err instanceof Error ? err.message : String(err);
  return {
    ok: false,
    error: 'Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent',
    errorKind: classifyAdapterError(msg),
  };
}
```
Tested: success → `{ ok:true, version:'pi 1.0.0' }`; `'spawn pi ENOENT'` → `SPAWN_FAILED`.

#### 2.5.17 `stop(pid)`
`await this.processManager.killWithGrace(pid);` — default 10 s, **not** the internal 1 s.
Tested: called with `8888` and no second arg.

---

### 2.6 grok — `GrokAdapter` (`adapters/grok.ts`), `kind = 'grok'`

Header docstring: "Spawns `grok -p ... --output-format streaming-json` in headless mode.
Grok streams text/thought deltas; this adapter aggregates text deltas into bounded output
chunks and emits a terminal `done` event at session end."

#### 2.6.1 argv (`grok.ts:40-61`) — prompt is FIRST, via `-p`

```ts
const args = [
  '-p', params.prompt,
  '--output-format', 'streaming-json',
  '--permission-mode', 'bypassPermissions',
  '--always-approve',
  '--cwd', params.workspace,
];

if (params.config.model)     args.push('--model', params.config.model);
if (params.config.effort)    args.push('--effort', params.config.effort);
if (params.config.max_turns) args.push('--max-turns', String(params.config.max_turns));

const effectiveSystemPrompt = params.systemPrompt ?? params.config.system_prompt;
if (effectiveSystemPrompt) args.push('--system-prompt-override', effectiveSystemPrompt);
```

Flat form:

```
grok
  -p <params.prompt>                                  prompt is argv[1], passed RAW
  --output-format streaming-json
  --permission-mode bypassPermissions
  --always-approve
  --cwd <params.workspace>
  [--model <config.model>]                            iff config.model truthy
  [--effort <config.effort>]                          iff config.effort truthy
  [--max-turns <String(config.max_turns)>]            iff config.max_turns TRUTHY (0 omits it — unlike claude's `?? 50`)
  [--system-prompt-override <effectiveSystemPrompt>]  iff (params.systemPrompt ?? config.system_prompt) truthy
```

- `params.prompt` is passed **raw**, not through `buildFullPrompt` — the system prompt
  goes to `--system-prompt-override`.
- `--permission-mode bypassPermissions` and `--always-approve` are unconditional.
- `--cwd <workspace>` is passed **in addition to** spawn `cwd`.

#### 2.6.2 Spawn options

```ts
this.processManager.spawn('grok', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
});
```
Default stdio (stdin ignored). Nothing written to stdin.

#### 2.6.3 Environment
Sets nothing of its own.

#### 2.6.4 stdout parsing — own generator with 240-char text aggregation

```ts
const OUTPUT_CHUNK_LEN = 240;
```

`createGrokEvents(proc, signal)` (`grok.ts:78-158`). State: `gotDoneEvent`,
`textBuffer = ''`, `finalText = ''`, `exitCode`, `exitError`.
Lifecycle via `proc.on('close'|'error')`.

```ts
const flushOutput = function* (): Generator<AgentEvent> {
  if (!textBuffer) return;
  const chunk = textBuffer;
  textBuffer = '';
  yield { type: 'output', timestamp: new Date().toISOString(), data: { text: chunk } };
};

if (proc.stdout) {
  try {
    for await (const line of readLines(proc.stdout)) {
      if (signal?.aborted) break;
      const event = parseGrokEvent(line, {
        appendText: (text) => { textBuffer += text; finalText += text; },
        finalText: () => finalText,
      });
      if (!event) {
        if (textBuffer.length >= OUTPUT_CHUNK_LEN) yield* flushOutput();
        continue;
      }
      if (event.type === 'done') { yield* flushOutput(); gotDoneEvent = true; }
      yield event;
    }
  } finally {
    proc.stdout.destroy();
  }
}
```

Ordering quirks to reproduce:
- A pending text buffer is flushed **before** any `done` event.
- Partial buffers flush only when `textBuffer.length >= 240`, and that check runs
  **only on a dropped/null event**.
- A non-null, non-`done` event (`tool_call` / `error` / `output`) is yielded
  **without** flushing the pending text buffer first.

`parseGrokEvent(line, state)` (`grok.ts:160-204`),
`type = typeof parsed.type === 'string' ? parsed.type : ''`:

| `type` | emitted |
|---|---|
| blank line | `null` |
| `"thought"` | `null` (dropped) |
| `"text"` | `null`, with the side effect `if (typeof parsed.data === 'string') state.appendText(parsed.data)` — the text payload lives in **`data`**, not `text` |
| `"tool_call"` or `"tool_use"` | `{ type:'tool_call', timestamp, data: parsed }` (whole object) |
| `"tool_result"` | `{ type:'output', timestamp, data: parsed }` |
| `"error"` | `message = typeof parsed.data === 'string' ? parsed.data : JSON.stringify(parsed)`; `{ type:'error', timestamp, data: parsed, errorKind: classifyAdapterError(message) }` — `data` is the whole `parsed`, **not** the message |
| `"end"` | `{ type:'done', timestamp, data: { result: state.finalText(), raw: parsed } }` |
| default | `{ type:'output', timestamp, data: parsed }` |
| JSON.parse throws | `{ type:'output', timestamp, data: { text: line } }` — note the `{text}` wrapper |

#### 2.6.5 Exit handling (`grok.ts:130-154`) — exact order after `await exitPromise`

```ts
if (!gotDoneEvent && !signal?.aborted) {
  yield* flushOutput();                       // trailing text flushed even on failure paths
}

if (exitError && !signal?.aborted && !gotDoneEvent) {
  const spawnErr = exitError as Error;
  throw Object.assign(new Error(spawnErr.message), {
    errorKind: classifyAdapterError(spawnErr.message, exitCode ?? undefined),
  });
}
if (exitCode !== 0 && exitCode !== null && !signal?.aborted && !gotDoneEvent) {
  const msg = `Grok process exited with code ${exitCode}`;
  throw Object.assign(new Error(msg), { errorKind: classifyAdapterError(msg, exitCode) });
}
if (!gotDoneEvent && !signal?.aborted && exitCode === 0) {
  yield { type: 'done', timestamp: new Date().toISOString(), data: { result: finalText } };
}
```

- **No stderr tail.** Grok never calls `createStderrTailCapture`, so its stderr is
  never read or drained.
- A clean exit with no `end` record still produces a **synthesised `done`**
  (with `data.result` only — no `raw` key).

#### 2.6.6 Fixture lines (from `test/unit/infrastructure/grok-adapter.test.ts`)

```json
{"type":"thought","data":"skip me"}
{"type":"text","data":"ORCH"}
{"type":"text","data":"_OK"}
{"type":"end","stopReason":"EndTurn"}
{"type":"tool_call","name":"edit"}
{"type":"error","data":"bad"}
```

Assertions:
- The first four lines produce **exactly 2** events:
  ```js
  events[0] → { type:'output', data: { text: 'ORCH_OK' } }
  events[1] → { type:'done',   data: { result: 'ORCH_OK', raw: { type:'end', stopReason:'EndTurn' } } }
  ```
- `tool_call` → `tool_call`; `error` → `error`
- `close 1` with no done → rejects with `'Grok process exited with code 1'`
- Spawn assertion: `'grok'`,
  `expect.arrayContaining(['-p','grok prompt','--output-format','streaming-json','--permission-mode','bypassPermissions','--always-approve','--cwd','/tmp/grok-ws'])`,
  `expect.objectContaining({ cwd:'/tmp/grok-ws' })`
- With `{ model:'grok-build', effort:'high', max_turns:7 }` and
  `systemPrompt:'system instructions'`, the args contain
  `--model grok-build`, `--effort high`, `--max-turns 7`,
  `--system-prompt-override system instructions`

#### 2.6.7 `test()`
```ts
const { stdout } = await execFileAsync('grok', ['--version']);  // ok → { ok:true, version: stdout.trim() }
// fail → { ok:false,
//          error:'Grok CLI not found. Install and authenticate the grok CLI, then ensure `grok` is on PATH.',
//          errorKind: classifyAdapterError(msg) }
```

#### 2.6.8 `stop(pid)` → `killWithGrace(pid)`.

---

### 2.7 antigravity — `AntigravityAdapter` (`adapters/antigravity.ts`), `kind = 'antigravity'`

Header docstring: "Spawns `agy -p ...` in headless mode. Current Antigravity CLI
headless mode is plain-text oriented, so stdout is streamed as output lines and a
terminal `done` event is emitted after a successful process exit."

#### 2.7.1 argv (`antigravity.ts:40-48`)

```ts
const args = [
  '-p',
  buildFullPrompt(params.systemPrompt ?? params.config.system_prompt, params.prompt),
  '--dangerously-skip-permissions',
];

if (params.config.model) args.push('--model', params.config.model);
```

Flat form:

```
agy
  -p
  <buildFullPrompt(params.systemPrompt ?? config.system_prompt, params.prompt)>   argv[2]
  --dangerously-skip-permissions                                                  unconditional
  [--model <config.model>]                                                        iff config.model truthy
```

- The combined prompt sits **between** `-p` and `--dangerously-skip-permissions`
  (argv index 2) — it is **not** the last argument.
- No cwd / sandbox / effort / max-turns / resume flags.

#### 2.7.2 Spawn options

```ts
this.processManager.spawn('agy', args, {
  cwd: params.workspace,
  env: { ...process.env, ...params.env },
  signal: params.signal,
});
```
Default stdio (stdin ignored).

#### 2.7.3 Environment
Sets nothing of its own.

#### 2.7.4 stdout parsing — no JSON at all (`antigravity.ts:65-116`)

```ts
if (proc.stdout) {
  try {
    for await (const line of readLines(proc.stdout)) {
      if (signal?.aborted) break;
      finalText += finalText ? `\n${line}` : line;        // join with '\n', no leading newline
      yield {
        type: 'output',
        timestamp: new Date().toISOString(),
        data: { text: line },
      };
    }
  } finally {
    proc.stdout.destroy();
  }
}
```

Every non-empty stdout line becomes exactly one `output` event with
`data = { text: line }`. stderr is **not** read or drained at all (same hazard as grok).

#### 2.7.5 Exit handling — note the missing done-event guard

```ts
await exitPromise;

if (exitError && !signal?.aborted) {
  const spawnErr = exitError as Error;
  throw Object.assign(new Error(spawnErr.message), {
    errorKind: classifyAdapterError(spawnErr.message, exitCode ?? undefined),
  });
}
if (exitCode !== 0 && exitCode !== null && !signal?.aborted) {
  const msg = `Antigravity process exited with code ${exitCode}`;
  throw Object.assign(new Error(msg), { errorKind: classifyAdapterError(msg, exitCode) });
}
if (!signal?.aborted) {
  yield { type: 'done', timestamp: new Date().toISOString(), data: { result: finalText } };
}
```

- There is **no `gotDoneEvent` flag** in this adapter, so a non-zero exit always throws
  even when output was produced.
- The synthetic `done` is reached for `exitCode === 0` **and** `exitCode === null`
  (signal-killed).

#### 2.7.6 Fixture (from `test/unit/infrastructure/antigravity-adapter.test.ts`)

Raw stdout lines `line one\n` and `line two\n`, then `close 0` → **3 events**:

```js
events[0] → { type:'output', data: { text: 'line one' } }
events[1] → { type:'output', data: { text: 'line two' } }
events[2] → { type:'done',   data: { result: 'line one\nline two' } }
```

- `close 1` → rejects with `'Antigravity process exited with code 1'`
- Spawn assertion: `'agy'`,
  `expect.arrayContaining(['-p','antigravity prompt','--dangerously-skip-permissions'])`,
  `expect.objectContaining({ cwd:'/tmp/agy-ws' })`
- With `systemPrompt:'system instructions'`, `prompt:'user task'`,
  `config:{ model:'gemini-3-pro' }` → args contain
  `'system instructions\n\nuser task'`, `'--model'`, `'gemini-3-pro'`

#### 2.7.7 `test()`
```ts
const { stdout } = await execFileAsync('agy', ['--version']);  // ok → { ok:true, version: stdout.trim() }
// fail → { ok:false,
//          error:'Antigravity CLI not found. Install Google Antigravity CLI and ensure `agy` is on PATH.',
//          errorKind: classifyAdapterError(msg) }
```

#### 2.7.8 `stop(pid)` → `killWithGrace(pid)`.

---

### 2.8 shell — `ShellAdapter` (`adapters/shell.ts`), `kind = 'shell'`

Header docstring: "Spawns an arbitrary command via `bash -lc`. Task prompt is passed via
ORCHESTRY_TASK_PROMPT env variable. Consumes stdout and stderr concurrently to avoid
deadlocks."

#### 2.8.1 Missing-command short circuit (`shell.ts:36-48`)

```ts
const command = params.config.command;
if (!command) {
  // Return a handle that immediately yields an error
  async function* errorGen(): AsyncGenerator<AgentEvent> {
    yield {
      type: 'error',
      timestamp: new Date().toISOString(),
      data: 'Shell adapter requires a command in agent config',
      errorKind: AdapterErrorKind.SPAWN_FAILED,
    };
  }
  return { pid: 0, events: errorGen() };
}
```

- `pid: 0`, no process spawned.
- `data` is the **bare string** `'Shell adapter requires a command in agent config'`.
- `errorKind` is hard-coded `SPAWN_FAILED`.
- Tested: exactly 1 event, `type === 'error'`, `data` contains `'requires a command'`,
  `errorKind === SPAWN_FAILED`.

#### 2.8.2 argv (`shell.ts:50`)

```ts
this.processManager.spawn('bash', ['-lc', command], { ... });
```

**Exactly three argv entries: `bash`, `-lc`, `<config.command>`.**

- `-l` makes it a *login* shell (sources the user's profile).
- The command string is one argv element that **bash** parses — the adapter performs
  **no quoting or escaping** of `config.command`.
- This is the only place a shell is involved anywhere in the adapter layer.

#### 2.8.3 Spawn options + env (`shell.ts:51-57`)

```ts
{
  cwd: params.workspace,
  env: {
    ...process.env,
    ...params.env,
    ORCHESTRY_TASK_PROMPT: buildFullPrompt(params.systemPrompt, params.prompt),
  },
  signal: params.signal,
}
```

Default stdio `['ignore','pipe','pipe']`.

**`ORCHESTRY_TASK_PROMPT` is the only env var any adapter sets.** Value is
`systemPrompt + '\n\n' + prompt`, or just `prompt`. It is placed **after**
`...params.env`, so it wins over any same-named key from `agent.config.env`.
Tested both with and without `systemPrompt`:
`'user task'` and `'be concise\n\nuser task'`.

#### 2.8.4 Exit promise, set up eagerly in `execute()` (`shell.ts:66-77`)

```ts
// Subscribe immediately after spawn. The process may finish while the
// orchestrator is still persisting/consuming its final output event.
// Resolving (rather than rejecting) keeps an early spawn error from
// becoming an unhandled promise rejection before the generator is read.
const exitPromise = new Promise<{ code: number | null; signal: NodeJS.Signals | null; error?: Error }>((resolve) => {
  proc.once('close', (code, exitSignal) => { resolve({ code, signal: exitSignal }); });
  proc.once('error', (error) => { resolve({ code: proc.exitCode, signal: proc.signalCode, error }); });
});
```

#### 2.8.5 Abort handling inside the generator (`shell.ts:85-93, 147-149`)

```ts
// Ensure process is reaped on abort — SIGTERM + grace period + SIGKILL
const onAbort = () => { processManager.killWithGrace(pid, 5_000).catch(() => {}); };
if (signal) {
  if (signal.aborted) onAbort();
  else signal.addEventListener('abort', onAbort, { once: true });
}
...
finally { signal?.removeEventListener('abort', onAbort); }
```

**5 000 ms** grace (the only adapter-local grace besides Pi's 1 000 ms).

#### 2.8.6 stdout/stderr — concurrent drain through `EventBuffer` (`shell.ts:96-128`)

The only adapter that uses `event-buffer.ts`.

```ts
// Ring buffer with backpressure replaces Array.shift() polling
const buffer = new EventBuffer();            // DEFAULT_CAPACITY = 1024

const stdoutPromise = (async () => {
  if (!proc.stdout) return;
  for await (const line of readLines(proc.stdout)) {
    if (signal?.aborted) break;
    await buffer.push({ type: 'output', timestamp: new Date().toISOString(), data: line });
  }
})();

const stderrPromise = (async () => {
  if (!proc.stderr) return;
  for await (const line of readLines(proc.stderr)) {
    if (signal?.aborted) break;
    await buffer.push({
      type: 'error',
      timestamp: new Date().toISOString(),
      data: line,
      errorKind: classifyAdapterError(line),
    });
  }
})();

// Close the buffer once both streams are drained (or on error)
void Promise.all([stdoutPromise, stderrPromise]).then(
  () => buffer.close(),
  () => buffer.close(),
);

// Yield events as they arrive — no polling, no busy-wait
yield* buffer;
```

- `data` is the **bare line string** (not `{text}`).
- Every stderr line is classified individually via `classifyAdapterError(line)`
  (no exit code).
- The buffer is closed on success **or** rejection — tested by
  "does not hang when stream emits an error (buffer.close is called)".

#### 2.8.7 Exit-code handling (`shell.ts:130-146`)

```ts
const exit = await exitPromise;
if (signal?.aborted) return;                 // silent on abort

if (exit.error) {
  throw Object.assign(exit.error, {
    errorKind: classifyAdapterError(exit.error.message, exit.code ?? undefined),
  });
}

if (exit.code !== 0) {
  const message = exit.code === null
    ? `Shell command terminated by signal ${exit.signal ?? 'unknown'}`
    : `Shell command exited with code ${exit.code}`;
  throw Object.assign(new Error(message), {
    errorKind: classifyAdapterError(message, exit.code ?? undefined),
  });
}
```

Differences from `createStreamingEvents`:
- **No `gotDoneEvent` suppression** — shell never emits `done`.
- `exit.code === null` **does** throw, with the signal-name message
  `Shell command terminated by signal <sig>` (or `... signal unknown`).

Tested: `exit 7` scenario rejects with
`{ message:'Shell command exited with code 7', errorKind: PROCESS_CRASH }`.
A generic stderr line yields an `error` event with `errorKind === UNKNOWN`.

#### 2.8.8 `test()` (`shell.ts:25-33`)

```ts
try {
  const { stdout } = await execFileAsync('bash', ['--version']);
  const version = stdout.split('\n')[0]?.trim() ?? 'unknown';     // FIRST LINE ONLY
  return { ok: true, version };
} catch {
  return { ok: false, error: 'bash not found', errorKind: classifyAdapterError('bash not found') };
}
```
`classifyAdapterError('bash not found')` evaluates to `ADAPTER_NOT_FOUND` (rule 2).

#### 2.8.9 `stop(pid)` → `killWithGrace(pid)`.

#### 2.8.10 No `done` event, ever
Consumers see only `output` / `error` events, then either clean generator completion
(exit 0) or a throw.

---

## 3. Cross-adapter comparison tables

### 3.1 argv one-line summary

| adapter | binary | prompt location | system prompt | model | effort | max-turns | permissions / sandbox | cwd flag | stdin |
|---|---|---|---|---|---|---|---|---|---|
| claude | `claude` | last positional | `--system-prompt` (params ?? config) | `--model` | `--effort` | `--max-turns` always, default `50` | `--dangerously-skip-permissions` | — | ignored |
| codex | `codex` | **stdin**; argv ends with `-` | prepended to stdin (params only) | `--model` | — | — | `--sandbox danger-full-access` | — | pipe, write + `end()` |
| cursor | `cursor-agent` ∥ `agent` | last positional | prepended (params only) | `--model` | — | — | `--yolo` + `--trust` | `--workspace` | ignored |
| opencode | `opencode` | last positional | prepended (params only) | `--model` | — | — | — | — | ignored |
| pi | `pi` | **stdin JSONL `prompt` cmd**; stdin kept open | `--append-system-prompt` (params ?? config) | `--model` | `--thinking` | — | — | — | pipe, write, **no `end()`** |
| grok | `grok` | `-p <prompt>` (argv[1]) | `--system-prompt-override` (params ?? config) | `--model` | `--effort` | `--max-turns` only if truthy | `--permission-mode bypassPermissions` + `--always-approve` | `--cwd` | ignored |
| antigravity | `agy` | `-p <combined>` (argv[2]) | prepended (params ?? config) | `--model` | — | — | `--dangerously-skip-permissions` | — | ignored |
| shell | `bash` | `ORCHESTRY_TASK_PROMPT` env var | prepended into that env var (params only) | — | — | — | — | — | ignored |

**No adapter has any resume / session / continue flag. No adapter sets a per-process
timeout. No adapter does shell quoting (shell adapter delegates parsing to bash).**

### 3.2 `done`-event sources

| adapter | `done` from | synthesised `done`? |
|---|---|---|
| claude | `{"type":"result"}` | no |
| codex | `{"type":"turn.completed"}` | no |
| cursor | `{"type":"result"}` | no |
| opencode | `step_finish` with any `reason` other than `'error'` / `'tool-calls'` (incl. `undefined`) | no |
| pi | `agent_end` (possibly promoted by `agent_settled`) with no failure | no |
| grok | `{"type":"end"}` | **yes** — on clean exit 0 with no `end` record |
| antigravity | — | **yes** — always, on `!signal.aborted` after exit 0 or `null` |
| shell | never | no |

### 3.3 `data` shape per adapter (canonical contract only partially honoured)

| adapter | `output.data` | `tool_call.data` | `error.data` |
|---|---|---|---|
| claude | `parsed.message ?? parsed`, or bare line string | whole `parsed` | `parsed.error ?? parsed` (often a bare string) |
| codex | `{ text, raw }` | `{ name, input?, raw }` | `{ message, raw }` |
| cursor | `parsed.message ?? parsed`, or bare line string | whole `parsed` | `parsed.error ?? parsed` |
| opencode | `part.text ?? part` (bare string), or bare line string | `{ name, input }` (no `raw`) | `state` object or `part` object |
| pi | `{ text }` or `{ text, raw }` | `{ name, input, raw }` | `{ message, raw }` |
| grok | `{ text }` (aggregated) or whole `parsed` | whole `parsed` | whole `parsed` |
| antigravity | `{ text }` | — | — |
| shell | bare line string | — | bare line string |

Source acknowledges the drift (`interface.ts:52-55`): "Existing adapters predate this
contract and emit a variety of shapes (claude/cursor: full `parsed.message`; codex:
`item`). The TUI renderer (`formatAgentOutput` in src/tui/App.tsx) is defensive and
accepts both canonical and legacy shapes during the migration window."

### 3.4 Which `AgentConfig` fields each adapter reads

| field | claude | codex | cursor | opencode | pi | grok | antigravity | shell |
|---|---|---|---|---|---|---|---|---|
| `command` | — | — | — | — | — | — | — | **yes** (`bash -lc`) |
| `model` | yes | yes | yes | yes | yes | yes | yes | — |
| `effort` | yes (`--effort`) | — | — | — | yes (`--thinking`) | yes (`--effort`) | — | — |
| `max_turns` | yes (default 50) | — | — | — | — | yes (if truthy) | — | — |
| `system_prompt` | yes (fallback) | — | — | — | yes (fallback) | yes (fallback) | yes (fallback) | — |
| `approval_policy` | — | — | — | — | — | — | — | — |
| `timeout_ms` | — | — | — | — | — | — | — | — |
| `stall_timeout_ms` | — | — | — | — | — | — | — | — |
| `workspace_mode` | — | — | — | — | — | — | — | — |
| `skills` | — | — | — | — | — | — | — | — |
| `env` | — (pre-merged by caller) | — | — | — | — | — | — | — |

---

## 4. Timeouts, idle detection, buffering / backpressure

### 4.1 Timeouts

**No adapter implements a wall-clock timeout on the child process.** Every timeout in
the layer:

| location | value |
|---|---|
| `model-discovery.ts:15` | `const DISCOVERY_TIMEOUT_MS = 15_000;` per probe command |
| `ProcessManager.killWithGrace` default | `graceMs = 10_000`, polling every `200` ms |
| pi internal post-terminal / abort / stream-error kill | `killWithGrace(pid, 1_000)` |
| shell abort handler | `killWithGrace(pid, 5_000)` |
| orchestrator stall handler | `killWithGrace(entry.pid, 5_000)` |

Timeout **policy** lives above the adapter layer:

```ts
// src/domain/agent.ts — declared but NEVER read by any adapter
timeout_ms?: number;
stall_timeout_ms?: number;

// src/domain/config.ts
export const DEFAULT_CONFIG: OrchestratorConfig = {
  project: { name: 'my-project' },
  defaults: {
    agent: {
      adapter: 'claude',
      approval_policy: 'auto',
      max_turns: 50,
      timeout_ms: 3_600_000,
      stall_timeout_ms: 600_000,
      workspace_mode: 'worktree',
    },
    task: { max_attempts: 3, priority: 3 },
  },
  scheduling: {
    poll_interval_ms: 10_000,
    max_concurrent_agents: 6,
    retry_base_delay_ms: 10_000,
    retry_max_delay_ms: 300_000,
  },
};

// src/domain/default-agents.ts
timeout_ms: 3_600_000,
stall_timeout_ms: 300_000,

// src/application/review-runner.ts:38 (unrelated to adapters)
this.timeoutMs = options.timeout_ms ?? 120_000;
```

### 4.2 Idle / stall detection (`src/application/orchestrator.ts:583-606`)

```ts
// Stall detection — use per-agent timeout if configured, fallback to global
const lastEventAt = new Date(entry.last_event_at).getTime();
const agentForStall = runningAgentData[i];
const stallTimeout = agentForStall?.config.stall_timeout_ms ?? this.deps.config.defaults.agent.stall_timeout_ms;

if (now - lastEventAt > stallTimeout) {
  this.deps.eventBus.emit({ type: 'orchestrator:stall_detected', runId: entry.run_id });
  this.abortControllers.get(taskId)?.abort();
  await this.deps.processManager.killWithGrace(entry.pid, 5_000);
  try {
    await this._handleRunFailure(taskId, entry, 'Agent stalled (no events)');
  } catch { /* fallback cleanup */ }
}
```

- Strictly `>` the timeout.
- `entry.last_event_at` is refreshed by each `AgentEvent`.
- PID liveness is checked **first**:
  `if (!this.deps.processManager.isAlive(entry.pid))` →
  `_handleRunFailure(taskId, entry, 'Process crashed unexpectedly')`.
- Both checks are skipped while `this.activeCollectors.has(taskId)` — comment:
  "the process may have exited cleanly but handleRunSuccess/Failure hasn't acquired the
  mutex yet. Without this guard, reconcile would false-positive mark successfully
  completed runs as 'crashed'."
- Failure message literals: `'Process crashed unexpectedly'`, `'Agent stalled (no events)'`.

### 4.3 `EventBuffer` (`adapters/event-buffer.ts`) — full semantics

Header docstring: "Lock-free ring buffer for AgentEvent streaming. Replaces
Array.shift() O(n) polling with O(1) dequeue and event-driven notification instead of
50ms busy-wait. Includes backpressure: when buffer is full, push() returns a promise
that resolves when space is available."

```ts
const DEFAULT_CAPACITY = 1024;

interface Deferred<T> { promise: Promise<T>; resolve: (value: T) => void; }

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
}

export class EventBuffer {
  private buf: (AgentEvent | undefined)[];
  private head = 0;  // read index
  private tail = 0;  // write index
  private count = 0;
  private readonly capacity: number;
  private dataReady: Deferred<void> | null = null;   // consumer notification
  private spaceReady: Deferred<void> | null = null;  // producer notification
  private closed = false;

  constructor(capacity = DEFAULT_CAPACITY) {
    this.capacity = capacity;
    this.buf = new Array(capacity);
  }

  get size(): number { return this.count; }
  get isFull(): boolean { return this.count >= this.capacity; }
  get isClosed(): boolean { return this.closed; }

  async push(event: AgentEvent): Promise<void> {
    while (this.isFull && !this.closed) {
      if (!this.spaceReady) this.spaceReady = deferred<void>();
      await this.spaceReady.promise;
    }
    if (this.closed) return;                     // event SILENTLY DROPPED after close

    this.buf[this.tail] = event;
    this.tail = (this.tail + 1) % this.capacity;
    this.count++;

    if (this.dataReady) {
      const dr = this.dataReady;
      this.dataReady = null;
      dr.resolve();
    }
  }

  async take(): Promise<AgentEvent | undefined> {
    while (this.count === 0) {
      if (this.closed) return undefined;
      if (!this.dataReady) this.dataReady = deferred<void>();
      await this.dataReady.promise;
    }

    const event = this.buf[this.head];
    this.buf[this.head] = undefined;             // allow GC
    this.head = (this.head + 1) % this.capacity;
    this.count--;

    if (this.spaceReady) {
      const sr = this.spaceReady;
      this.spaceReady = null;
      sr.resolve();
    }

    return event;
  }

  close(): void {
    this.closed = true;
    if (this.dataReady)  { const dr = this.dataReady;  this.dataReady = null;  dr.resolve(); }
    if (this.spaceReady) { const sr = this.spaceReady; this.spaceReady = null; sr.resolve(); }
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<AgentEvent> {
    while (true) {
      const event = await this.take();
      if (event === undefined) return;
      yield event;
    }
  }
}
```

Parity notes:
- `DEFAULT_CAPACITY = 1024`.
- `push` loops with `while` (not `if`) so multiple blocked producers each re-check.
- `push` after `close()` **silently drops** the event and resolves (does not throw).
- `take` returns `undefined` **only** when empty **and** closed — buffered events still
  drain after `close()`.
- `Deferred.resolve` only; nothing ever rejects.
- Slot is nulled on dequeue to allow GC.

Tested invariants (`test/unit/infrastructure/event-buffer.test.ts`): FIFO order;
`take` waits for `push`; `undefined` after close-on-empty; drains remaining after close;
`size` tracking; ring wrap-around with capacity 3; backpressure with capacity 2 (third
`push` stays pending for ≥10 ms until a `take`); async iterator drains all and stops on
close; `close()` unblocks a waiting `push`.

### 4.4 Other backpressure mechanisms

- `readLines` / `readPiRpcLines` use `for await` on the raw `Readable` (native
  backpressure). `readline.createInterface` was explicitly rejected (unbounded internal
  line queue ⇒ OOM).
- `createStderrTailCapture` drains stderr eagerly with a bounded 4 KB tail, preventing a
  full-stderr-pipe deadlock. **grok and antigravity do not call it and never read
  stderr at all.**
- Three-layer size cap chain (comment in `process-manager.ts:85`):
  `readLines` 16 KB → `serializeEventData` 8 KB → bus emit 4 KB.

---

## 5. The `IAgentAdapter` / `AgentEvent` contract (verbatim)

From `parity/orch-ts/src/infrastructure/adapters/interface.ts`,
reproduced verbatim:

```ts
/**
 * Agent adapter interface.
 *
 * Every AI tool (Claude, Codex, Shell, etc.) implements this contract.
 * execute() returns an AsyncGenerator for pull-based streaming of events.
 */

import type { AgentConfig } from '../../domain/agent.js';
import type { AdapterErrorKind } from '../../domain/errors.js';

export interface AdapterTestResult {
  ok: boolean;
  version?: string;
  error?: string;
  errorKind?: AdapterErrorKind;
  details?: Record<string, unknown>;
}

export interface ExecuteParams {
  prompt: string;
  systemPrompt?: string;
  workspace: string;
  env?: Record<string, string>;
  config: AgentConfig;
  signal?: AbortSignal;
}

/**
 * Canonical `data` shape per AgentEvent type. Each adapter should emit `data`
 * that matches its event's row below so that downstream consumers (TUI logs,
 * `orch logs` CLI, serve daemon) can render events without knowing adapter
 * internals.
 *
 *   | type        | data shape                                        |
 *   |-------------|---------------------------------------------------|
 *   | output      | { text: string, raw?: unknown }                   |
 *   | tool_call   | { name: string, input?: unknown, raw?: unknown }  |
 *   | command     | { command: string, result?: unknown, raw?: unknown } |
 *   | file_change | { paths: string[], raw?: unknown }                |
 *   | error       | { message: string, raw?: unknown }                |
 *   | done        | { result?: string, raw?: unknown }                |
 *
 * - `raw` is an optional escape hatch for the full provider payload; logs
 *   renderers must not include it in the default summary.
 * - Adapters should emit ONE `output` per logical assistant message (per
 *   text-block or per turn), not per-character delta. Use adapter-local state
 *   to aggregate streaming deltas before emitting.
 * - Intermediate progress events (tool-in-flight, "thinking" pings) should be
 *   dropped at the adapter boundary — they belong in adapter-specific UIs,
 *   not in the orchestrator event stream.
 *
 * Existing adapters predate this contract and emit a variety of shapes
 * (claude/cursor: full `parsed.message`; codex: `item`). The TUI renderer
 * (`formatAgentOutput` in src/tui/App.tsx) is defensive and accepts both
 * canonical and legacy shapes during the migration window.
 */
export interface AgentEvent {
  type: 'output' | 'file_change' | 'command' | 'tool_call' | 'error' | 'done';
  timestamp: string;
  data: unknown;
  tokens?: { input: number; output: number; reasoning?: number; total: number; cache_read?: number; cache_write?: number };
  errorKind?: AdapterErrorKind;
}

export interface ExecuteHandle {
  pid: number;
  events: AsyncGenerator<AgentEvent>;
}

export interface IAgentAdapter {
  readonly kind: string;
  test(): Promise<AdapterTestResult>;
  execute(params: ExecuteParams): ExecuteHandle;
  stop(pid: number): Promise<void>;
}
```

Notes:
- `execute` is **not** `async` — it returns an `ExecuteHandle` synchronously, so the
  `pid` is available immediately while events stream lazily.
- The six `AgentEvent` variants are exactly:
  `'output' | 'file_change' | 'command' | 'tool_call' | 'error' | 'done'`.
- `AgentEvent.tokens` is structurally **looser** than the `TokenUsage` the adapters
  actually construct (`reasoning`, `cache_read`, `cache_write` are optional on the event,
  required on `TokenUsage`).
- `timestamp` is always an ISO-8601 string from `new Date().toISOString()`.
- **There is no `AgentResult` type.** If a parity spec needs a "result" notion, the
  closest things are `AdapterTestResult` (for `test()`) and the `done` event's
  `data = { result?: string, raw?: unknown }`.

### 5.1 Supporting types (verbatim)

```ts
// src/domain/run.ts
export interface TokenUsage {
  input: number;
  output: number;
  reasoning: number;
  total: number;
  /** Cache tokens — informational only, NOT added to total (subset of input). */
  cache_read: number;
  cache_write: number;
}

/** Create TokenUsage with total always computed as input + output + reasoning. */
export function createTokenUsage(
  input: number,
  output: number,
  opts?: { reasoning?: number; cache_read?: number; cache_write?: number },
): TokenUsage {
  const reasoning = opts?.reasoning ?? 0;
  return {
    input,
    output,
    reasoning,
    total: input + output + reasoning,
    cache_read: opts?.cache_read ?? 0,
    cache_write: opts?.cache_write ?? 0,
  };
}

export interface RunEvent {
  timestamp: string;
  type: RunEventType;
  data: unknown;
}

export type RunEventType =
  | 'agent_output'
  | 'file_changed'
  | 'command_run'
  | 'tool_call'
  | 'error'
  | 'done';
```

```ts
// src/domain/agent.ts
export type ApprovalPolicy = 'suggest' | 'auto' | 'manual';
export type ReasoningEffort = 'low' | 'medium' | 'high';

export interface AgentConfig {
  command?: string;
  model?: string;
  effort?: ReasoningEffort;
  approval_policy?: ApprovalPolicy;
  max_turns?: number;
  timeout_ms?: number;
  stall_timeout_ms?: number;
  env?: Record<string, string>;
  system_prompt?: string;
  workspace_mode?: WorkspaceMode;
  skills?: string[];
}
```

Note `RunEventType` is a **different** vocabulary from `AgentEvent.type`
(`agent_output` / `file_changed` / `command_run` vs `output` / `file_change` / `command`)
— the mapping happens above the adapter layer.

---

## 6. ProcessManager (`src/infrastructure/process/process-manager.ts`)

```ts
export interface SpawnResult {
  process: ChildProcess;
  pid: number;
}

export interface IProcessManager {
  isAlive(pid: number): boolean;
  kill(pid: number, signal?: NodeJS.Signals): void;
  killWithGrace(pid: number, graceMs?: number): Promise<void>;
  spawn(command: string, args: string[], options?: SpawnOptions): SpawnResult;
}
```

### 6.1 Spawning

```ts
spawn(command: string, args: string[], options?: SpawnOptions): SpawnResult {
  const proc = spawn(command, args, {
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true, // Create new process group so killWithGrace(-pid) kills all children
    ...options,
  });

  if (!proc.pid) {
    throw new Error(`Failed to spawn process: ${command}`);
  }

  // Allow parent to exit without waiting for this child.
  // Pipes (stdout/stderr) still hold refs while being read — that's intentional.
  proc.unref();

  return { process: proc, pid: proc.pid };
}
```

Tested (`test/unit/infrastructure/process-manager.test.ts`):
`proc.unref()` is called exactly once; `spawnOpts.detached === true`;
a `pid`-less child throws `'Failed to spawn process'`.

### 6.2 Tracking

ProcessManager is **stateless** — it keeps no registry of spawned processes.
All tracking lives in the orchestrator:

- `state.running[taskId] = { run_id, agent_id, task_id, pid, started_at, last_event_at }`,
  persisted to `.orchestry/state.json`.
- In-memory `private abortControllers = new Map<string, AbortController>()`.
- In-memory `activeCollectors` set.

### 6.3 PID liveness check

```ts
isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    // EPERM means process exists but we lack permission to signal it
    if ((err as NodeJS.ErrnoException).code === 'EPERM') return true;
    return false;
  }
}
```

Signal `0` probe; `EPERM` counts as **alive**.

### 6.4 Kill — process group first, direct PID fallback

```ts
kill(pid: number, signal: NodeJS.Signals = 'SIGTERM'): void {
  // Kill entire process group (-pid) to clean up child processes (vitest, playwright, etc.)
  try {
    process.kill(-pid, signal);
  } catch {
    // Group kill failed — fall back to direct PID kill
    try {
      process.kill(pid, signal);
    } catch {
      // Process already dead
    }
  }
}
```

Default signal is `'SIGTERM'`. Both failures are swallowed silently.

### 6.5 Signal order and grace period

```ts
async killWithGrace(pid: number, graceMs: number = 10_000): Promise<void> {
  if (!this.isAlive(pid)) return;          // early return, NO signal sent

  this.kill(pid, 'SIGTERM');

  const deadline = Date.now() + graceMs;

  while (Date.now() < deadline) {
    if (!this.isAlive(pid)) return;
    await new Promise((r) => setTimeout(r, 200));
  }

  // Force kill if still alive
  this.kill(pid, 'SIGKILL');
}
```

Exact sequence: early-return if already dead → `SIGTERM` (group-first) →
poll `isAlive` every **200 ms** until `Date.now() >= deadline` → `SIGKILL`
(group-first, same fallback). It does **not** verify death after `SIGKILL`,
and does not `waitpid`/reap.

Grace values observed in the codebase: default `10_000`; pi `1_000`;
shell abort `5_000`; orchestrator stall `5_000`. Every adapter's `stop(pid)`
uses the default.

---

## 7. Model discovery (`src/infrastructure/models/model-discovery.ts`)

Header docstring: "Runtime model discovery for adapter CLIs. Prefer the catalog exposed
by the installed CLI. When a CLI has no model listing command (Codex and Claude), use its
own local cache or stable aliases. Fallbacks deliberately avoid pinned model versions so
they cannot go stale."

### 7.1 Types and probe table

```ts
const DISCOVERY_TIMEOUT_MS = 15_000;

export interface ModelOption { value: string; label: string; hint?: string; }
export type ModelCatalog = Partial<Record<AdapterKind, ModelOption[]>>;
export interface CommandOutput { stdout: string; stderr: string; }

export interface ModelDiscoveryRuntime {
  runCommand?: (command: string, args: string[]) => Promise<CommandOutput>;
  readTextFile?: (path: string) => Promise<string>;
  codexHome?: string;
}
```

```ts
export async function discoverModelOptions(
  adapter: AdapterKind,
  runtime: ModelDiscoveryRuntime = {},
): Promise<ModelOption[]> {
  const runCommand = runtime.runCommand ?? run;

  try {
    switch (adapter) {
      case 'claude':
        return await discoverCommand(runCommand, 'claude', ['--help'], parseClaudeModelAliases, 'use Claude configured default');
      case 'cursor':
        return await discoverCommand(runCommand, 'cursor-agent', ['--list-models'], parseCursorModels, 'use Cursor configured default');
      case 'grok':
        return await discoverCommand(runCommand, 'grok', ['models'], parseGrokModels, 'use Grok configured default');
      case 'antigravity':
        return await discoverCommand(runCommand, 'agy', ['models'], parseRuntimeLineModels, 'use Antigravity configured default');
      case 'opencode':
        return await discoverCommand(runCommand, 'opencode', ['models'], parseRuntimeLineModels, 'use model configured in OpenCode');
      case 'pi':
        return await discoverCommand(runCommand, 'pi', ['--list-models'], parsePiModels, 'use Pi configured default', true);
      case 'codex': {
        const codexHome = runtime.codexHome ?? process.env.CODEX_HOME ?? join(homedir(), '.codex');
        const readTextFile = runtime.readTextFile ?? ((path: string) => readFile(path, 'utf8'));
        return withDefault(
          parseCodexModelsCache(await readTextFile(join(codexHome, 'models_cache.json'))),
          'use Codex configured default',
        );
      }
      case 'shell':
        return getFallbackModelOptions('shell');
    }
  } catch {
    return [];
  }
}
```

| adapter | probe | parser | `defaultHint` | `includeStderr` |
|---|---|---|---|---|
| `claude` | `claude --help` | `parseClaudeModelAliases` | `'use Claude configured default'` | false |
| `cursor` | `cursor-agent --list-models` | `parseCursorModels` | `'use Cursor configured default'` | false |
| `grok` | `grok models` | `parseGrokModels` | `'use Grok configured default'` | false |
| `antigravity` | `agy models` | `parseRuntimeLineModels` | `'use Antigravity configured default'` | false |
| `opencode` | `opencode models` | `parseRuntimeLineModels` | `'use model configured in OpenCode'` | false |
| `pi` | `pi --list-models` | `parsePiModels` | `'use Pi configured default'` | **true** |
| `codex` | **no command** — reads `join(codexHome, 'models_cache.json')` where `codexHome = runtime.codexHome ?? process.env.CODEX_HOME ?? join(homedir(), '.codex')` | `parseCodexModelsCache` | `'use Codex configured default'` | n/a |
| `shell` | none | — | returns `getFallbackModelOptions('shell')` | n/a |

**The whole switch is wrapped in `try { … } catch { return []; }`** — any failure yields
`[]`, not a fallback list. `CODEX_HOME` is the only env var read by the discovery module.

```ts
async function discoverCommand(runCommand, command, args, parse, defaultHint, includeStderr = false) {
  const { stdout, stderr } = await runCommand(command, args);
  const output = includeStderr ? `${stdout}\n${stderr}` : stdout || stderr;
  return withDefault(parse(output), defaultHint);
}
```

### 7.2 Default command runner

```ts
async function run(command: string, args: string[]): Promise<CommandOutput> {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let settled = false;
    let timer: ReturnType<typeof setTimeout>;

    const finish = (err?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (err) reject(err);
      else resolve({ stdout, stderr });
    };

    timer = setTimeout(() => {
      child.kill('SIGTERM');
      finish(new Error(`${command} ${args.join(' ')} timed out`));
    }, DISCOVERY_TIMEOUT_MS);

    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('error', finish);
    child.on('close', (code, signal) => {
      if (code === 0) {
        finish();
      } else {
        finish(new Error(`${command} ${args.join(' ')} failed: ${signal ?? code}${stderr ? ` ${stderr}` : ''}`));
      }
    });
  });
}
```

- **Not detached**, no `cwd`, inherits the full environment.
- Output accumulates as unbounded utf8 strings.
- Timeout message: `` `${command} ${args.join(' ')} timed out` ``.
- Failure message: `` `${command} ${args.join(' ')} failed: ${signal ?? code}${stderr ? ` ${stderr}` : ''}` ``.

### 7.3 Caching

**There is no in-process cache.** Every `discoverModelOptions` call re-probes.
The only "cache" involved is Codex's own on-disk `~/.codex/models_cache.json`, which this
module **reads** and never writes.

### 7.4 `loadModelCatalog`

```ts
export async function loadModelCatalog(
  adapters: readonly AdapterKind[],
  onUpdate?: (update: ModelCatalog) => void,
): Promise<ModelCatalog> {
  const entries = await Promise.all(adapters.map(async (adapter) => {
    const discovered = await discoverModelOptions(adapter);
    const options = discovered.length > 0 ? discovered : getFallbackModelOptions(adapter);
    onUpdate?.({ [adapter]: options });
    return [adapter, options] as const;
  }));
  return Object.fromEntries(entries) as ModelCatalog;
}
```
Fully parallel; `onUpdate` fires per adapter as each resolves (progressive streaming —
tested).

### 7.5 Parsers — exact regexes

```ts
export function parseClaudeModelAliases(output: string): ModelOption[] {
  const plain = stripAnsi(output);
  const modelFlag = plain.match(/--model <model>\s+([\s\S]*?)(?=\n\s{2,}--|\nCommands:|$)/)?.[1] ?? '';
  const aliases = [...modelFlag.matchAll(/(?:^|[\s,(])['"]([a-z0-9][a-z0-9._/-]*)['"]/gi)]
    .map((match) => match[1]!.trim())
    .filter(Boolean);
  return dedupeOptions(aliases.map((value) => ({
    value,
    label: labelFromModelId(value),
    hint: value.includes('-') ? 'installed CLI example' : 'latest alias',
  })));
}
```
Test input (real `claude --help` wording) and expected values:
```
  --model <model>  Model for the current session. Provide an alias for the latest
                   model (e.g. 'fable', 'opus', or 'sonnet') or a model's full
                   name (e.g. 'claude-fable-5').
  --name <name>    Session name
```
→ `['fable','opus','sonnet','claude-fable-5']` (the `--name` block is excluded by the
`(?=\n\s{2,}--)` lookahead).

```ts
export function parseCodexModelsCache(input: string): ModelOption[] {
  let parsed: unknown;
  try { parsed = JSON.parse(input); } catch { return []; }
  if (!isRecord(parsed) || !Array.isArray(parsed.models)) return [];

  const options: ModelOption[] = [];
  for (const item of parsed.models) {
    if (!isRecord(item) || typeof item.slug !== 'string') continue;
    if (item.visibility !== undefined && item.visibility !== 'list') continue;
    options.push({
      value: item.slug,
      label: typeof item.display_name === 'string' ? item.display_name : labelFromModelId(item.slug),
      hint: typeof item.description === 'string' ? item.description : 'Codex cache',
    });
  }
  return dedupeOptions(options);
}
```
Literal JSON keys: `models[]`, `.slug`, `.visibility` (must be absent or `'list'`),
`.display_name`, `.description`.
Tested: `{slug:'gpt-current',display_name:'GPT Current',description:'Latest',visibility:'list'}`
→ `{value:'gpt-current',label:'GPT Current',hint:'Latest'}`; `visibility:'hide'` dropped;
`{slug:'gpt-compatible'}` → `{value:'gpt-compatible',label:'GPT Compatible',hint:'Codex cache'}`;
`{display_name:'Missing slug'}` dropped; `'{broken'` → `[]`.

```ts
export function parseCursorModels(output: string): ModelOption[] {
  const options: ModelOption[] = [];
  for (const rawLine of stripAnsi(output).split('\n')) {
    const match = rawLine.trim().match(/^(\S+)\s+-\s+(.+)$/);
    if (!match) continue;
    const value = match[1]!;
    const labelAndHint = match[2]!;
    const status = labelAndHint.match(/\s+\(([^)]+)\)\s*$/);
    const label = status ? labelAndHint.slice(0, status.index).trim() : labelAndHint.trim();
    options.push({ value, label, hint: status?.[1] ?? 'runtime' });
  }
  return dedupeOptions(options);
}
```
Tested: `auto - Auto (current, default)` → `{value:'auto',label:'Auto',hint:'current, default'}`;
`gpt-5.6-sol-high - GPT-5.6 Sol 1M High` → `{value:'gpt-5.6-sol-high',label:'GPT-5.6 Sol 1M High',hint:'runtime'}`.

```ts
export function parsePiModels(output: string): ModelOption[] {
  const lines = stripAnsi(output).split('\n').map((line) => line.trim()).filter(Boolean);
  const headerIndex = lines.findIndex((line) => /^provider\s{2,}model\s{2,}/i.test(line));
  if (headerIndex < 0) return [];

  const options: ModelOption[] = [];
  for (const line of lines.slice(headerIndex + 1)) {
    const columns = line.split(/\s{2,}/);
    const provider = columns[0];
    const model = columns[1];
    if (!provider || !model) continue;
    options.push({
      value: `${provider}/${model}`,
      label: `${model} (${provider})`,
      hint: columns.slice(2).filter(Boolean).join(' · ') || 'runtime',
    });
  }
  return dedupeOptions(options);
}
```
Column separator is 2+ whitespace. Hint join separator is `' · '`
(space, U+00B7 MIDDLE DOT, space).
Tested: `anthropic  claude-sonnet-4-6  200K     64K      yes       yes` →
`{value:'anthropic/claude-sonnet-4-6', label:'claude-sonnet-4-6 (anthropic)', hint:'200K · 64K · yes · yes'}`.

```ts
export function parseGrokModels(output: string): ModelOption[] {
  const result: ModelOption[] = [];
  for (const line of stripAnsi(output).split('\n')) {
    const match = line.match(/^\s*([*-])\s+([^\s].*?)(?:\s+\(default\))?\s*$/);
    if (!match) continue;
    const isDefault = line.includes('(default)') || match[1] === '*';
    const value = match[2]!.replace(/\s+\(default\)\s*$/, '').trim();
    result.push({ value, label: labelFromModelId(value), hint: isDefault ? 'current default' : 'runtime' });
  }
  return dedupeOptions(result);
}
```
Tested: `  * grok-4.5 (default)` → `{value:'grok-4.5',label:'Grok 4.5',hint:'current default'}`;
`  - grok-build` → `{value:'grok-build',label:'Grok Build',hint:'runtime'}`;
`Default model: grok-4.5` and `Available models:` don't match.

```ts
export function parseLineModels(output: string, hint: string): ModelOption[] {
  const options = stripAnsi(output)
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith('No models available'))
    .filter((line) => !line.startsWith('Use ') && !line.startsWith('/'))
    .map((value) => ({ value, label: labelFromModelId(value), hint }));
  return dedupeOptions(options);
}

function parseRuntimeLineModels(output: string): ModelOption[] {
  return parseLineModels(output, 'runtime');
}
```
Tested: `Gemini 3.6 Flash (Medium)` and `Claude Sonnet 5 (Thinking)` kept (label === value,
because `labelFromModelId` returns the input verbatim when it contains whitespace);
`No models available for a disabled provider`, `Use /help for usage`, `/tmp/cache/file`
dropped.

### 7.6 Shared helpers

```ts
function withDefault(options: ModelOption[], hint: string): ModelOption[] {
  if (options.length === 0) return [];                               // NO default row injected
  if (options.some((option) => option.value === '')) return options;
  return [{ value: '', label: 'Default', hint }, ...options];
}

function dedupeOptions(options: ModelOption[]): ModelOption[] {
  const seen = new Set<string>();
  const result: ModelOption[] = [];
  for (const option of options) {
    if (seen.has(option.value)) continue;
    seen.add(option.value);
    result.push(option);
  }
  return result;                                                     // first occurrence wins
}

function stripAnsi(value: string): string {
  return value.replace(/\u001B\[[0-?]*[ -/]*[@-~]/g, '');
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;                // arrays pass this
}

function labelFromModelId(value: string): string {
  if (!value) return 'Default';
  if (/\s/.test(value)) return value;                                // whitespace → verbatim
  const last = value.split('/').pop() ?? value;
  return last
    .replace(/^~+/, '')
    .split(/[-_]/g)
    .filter(Boolean)
    .map((part) => {
      if (/^(gpt|api|ai)$/i.test(part)) return part.toUpperCase();
      return /^[a-z]+$/i.test(part) ? part.charAt(0).toUpperCase() + part.slice(1) : part.toUpperCase();
    })
    .join(' ');
}
```
Verified label examples: `'gpt-compatible'` → `'GPT Compatible'`;
`'grok-4.5'` → `'Grok 4.5'`; `'grok-build'` → `'Grok Build'`.

### 7.7 Fallback options (verbatim)

```ts
export const FALLBACK_MODEL_OPTIONS: Record<AdapterKind, ModelOption[]> = {
  claude: [
    { value: '', label: 'Default', hint: 'use Claude configured default' },
    { value: 'opus', label: 'Opus', hint: 'latest Opus alias' },
    { value: 'sonnet', label: 'Sonnet', hint: 'latest Sonnet alias' },
  ],
  codex: [
    { value: '', label: 'Default', hint: 'use Codex configured default' },
  ],
  cursor: [
    { value: '', label: 'Default', hint: 'use Cursor configured default' },
    { value: 'auto', label: 'Auto', hint: 'let Cursor choose' },
  ],
  opencode: [
    { value: '', label: 'Default', hint: 'use model configured in OpenCode' },
  ],
  pi: [
    { value: '', label: 'Default', hint: 'use Pi configured default; run /login in Pi if unavailable' },
  ],
  grok: [
    { value: '', label: 'Default', hint: 'use Grok configured default' },
  ],
  antigravity: [
    { value: '', label: 'Default', hint: 'use Antigravity configured default' },
  ],
  shell: [
    { value: '', label: 'Default', hint: 'model is not used by the shell adapter' },
  ],
};

export function getFallbackModelOptions(adapter: string): ModelOption[] {
  return isAdapterKind(adapter)
    ? FALLBACK_MODEL_OPTIONS[adapter]
    : [{ value: '', label: 'Default', hint: 'use adapter default' }];
}
```

---

## 8. Model tiers (`src/domain/model-tiers.ts`) — verbatim

```ts
/** The supported adapter kinds. */
export type AdapterKind =
  | 'claude'
  | 'opencode'
  | 'codex'
  | 'cursor'
  | 'pi'
  | 'grok'
  | 'antigravity'
  | 'shell';

/**
 * Semantic capability tiers — adapter-agnostic.
 *   capable  — most powerful / highest quality (opus, gpt-5.4)
 *   balanced — good quality + speed (sonnet, gpt-5.3-codex)
 *   fast     — cheapest / fastest (haiku, gpt-5-mini)
 */
export type ModelTier = 'capable' | 'balanced' | 'fast';

/**
 * Tier → model mapping per adapter.
 *
 * Conventions:
 *   - shell:    '' for all tiers (model irrelevant)
 *   - opencode: '' for balanced (delegate to opencode's own config)
 *   - cursor:   'auto' for all tiers (Cursor handles selection)
 *   - antigravity: '' for balanced (delegate to Antigravity's configured default)
 */
export const MODEL_TIER_MAP: Record<AdapterKind, Record<ModelTier, string>> = {
  claude: {
    capable: 'claude-opus-4-6',
    balanced: 'claude-sonnet-4-6',
    fast: 'claude-haiku-4-6',
  },
  opencode: {
    capable: 'openrouter/anthropic/claude-opus-4.6',
    balanced: '',
    fast: 'openrouter/google/gemini-2.5-flash',
  },
  codex: {
    capable: 'gpt-5.4',
    balanced: 'gpt-5.3-codex',
    fast: 'gpt-5-mini',
  },
  cursor: {
    capable: 'auto',
    balanced: 'auto',
    fast: 'auto',
  },
  pi: {
    capable: 'openai-codex/gpt-5.5',
    balanced: 'openai-codex/gpt-5.5',
    fast: 'openai-codex/gpt-5.5',
  },
  grok: {
    capable: 'grok-build',
    balanced: 'grok-composer-2.5-fast',
    fast: 'grok-composer-2.5-fast',
  },
  antigravity: {
    capable: 'gemini-3-pro',
    balanced: '',
    fast: 'gemini-3-flash',
  },
  shell: {
    capable: '',
    balanced: '',
    fast: '',
  },
};

/**
 * Resolve a concrete model string from adapter + tier.
 * Returns '' for unknown adapters (let the adapter decide).
 */
export function resolveModel(adapter: string, tier: ModelTier): string {
  const adapterMap = MODEL_TIER_MAP[adapter as AdapterKind];
  if (!adapterMap) return '';
  return adapterMap[tier];
}

/** Returns the default (balanced) model for the adapter. */
export function defaultModelForAdapter(adapter: string): string {
  return resolveModel(adapter, 'balanced');
}

/** Type guard: is a string a valid AdapterKind? */
export function isAdapterKind(value: string): value is AdapterKind {
  return value in MODEL_TIER_MAP;
}

/** Type guard: is a string a valid ModelTier? */
export function isModelTier(value: string): value is ModelTier {
  return value === 'capable' || value === 'balanced' || value === 'fast';
}

/** All supported adapter names in display order. */
export const SUPPORTED_ADAPTERS: readonly AdapterKind[] = [
  'claude',
  'opencode',
  'codex',
  'cursor',
  'pi',
  'grok',
  'antigravity',
  'shell',
];
```

Note: `SUPPORTED_ADAPTERS` display order (`claude, opencode, codex, cursor, pi, grok,
antigravity, shell`) **differs** from the registry insertion order
(`claude, codex, cursor, shell, opencode, pi, grok, antigravity`).

---

## 9. Platform-specific behaviour (POSIX vs Windows)

`grep -rn 'win32|process.platform' src` finds **zero** hits in any adapter,
process-manager, or model-discovery file. (Only `src/infrastructure/clipboard-service.ts`
and `src/tui/components/FormWizard.tsx` branch on platform, both out of scope.)
Everything in this layer assumes POSIX:

1. **`ProcessManager.kill` uses a negative PID** (`process.kill(-pid, signal)`).
   Process-group signalling is POSIX-only. On Windows, Node throws for a negative PID,
   so the code silently falls back to `process.kill(pid, signal)` and
   **grandchild processes are never reaped**. A port must mirror both the attempt and
   the fallback.
2. **`detached: true`** — on POSIX this calls `setsid()`, creating a new process group,
   which is exactly what makes `-pid` work. On Windows it means "new console window,
   child survives parent", with no group semantics.
3. **`SIGTERM` then `SIGKILL`** — Windows Node maps both to an unconditional
   `TerminateProcess`, so the 10 s grace period is meaningless there.
4. **`process.kill(pid, 0)` liveness probe plus `EPERM`-means-alive** — POSIX signal-0
   semantics. Windows has no signal 0 and different error codes.
5. **`bash -lc`** (shell adapter) — requires a POSIX `bash` with login-shell profile
   semantics. There is no `cmd.exe`/PowerShell path and no `.cmd` shim handling.
6. **No `.cmd`/`.bat` resolution anywhere.** Every binary is spawned by bare name
   (`claude`, `codex`, `cursor-agent`, `agent`, `opencode`, `pi`, `grok`, `agy`, `bash`)
   with `shell: false`. On Windows, npm-installed CLIs are `.cmd` shims that `spawn`
   cannot execute without a shell — all these adapters would fail with `ENOENT`
   (→ `SPAWN_FAILED`).
7. **Line ending `\n` only.** `readLines` does **not** strip a trailing `\r`, so CRLF
   output from a Windows child leaves a `\r` on every line. Harmless for `JSON.parse`
   (which tolerates trailing whitespace), but corrupting for the plain-text adapters:
   antigravity's `data.text` and shell's `data` would retain the `\r`.
   **Only `readPiRpcLines` strips `\r`.** This asymmetry is deliberate in the source and
   must be preserved for byte-level parity.
8. **`homedir()` + `join(homedir(), '.codex')`** for the Codex model cache, with a
   `CODEX_HOME` env override. The separator comes from `node:path` (portable), but the
   `~/.codex` convention is POSIX-flavoured.
9. **`process.env` case-sensitivity** — `{ ...process.env, ...params.env }` plus
   `ORCHESTRY_TASK_PROMPT` assumes case-sensitive env keys; Windows env is
   case-insensitive.
10. **`ARG_MAX`** — codex's `-`/stdin design exists specifically because of POSIX
    `ARG_MAX` ("avoids ARG_MAX limits on long prompts"). claude, cursor, opencode, grok
    and antigravity all pass the full prompt (and for claude/grok the full system prompt
    too) in argv and are therefore exposed to it.

---

## 10. Divergences and hazards for the Rust implementation

1. **`AgentResult` does not exist.** The real contract is
   `ExecuteHandle { pid, events }` + `AdapterTestResult`, plus the `done` event's
   `{ result?, raw? }` payload.
2. **grok and antigravity never read stderr.** No `createStderrTailCapture`, no drain,
   and no stderr tail in their error messages. A chatty child can fill the pipe and
   deadlock. Exact parity means reproducing this; a deliberate improvement should be
   called out in the spec.
3. **`cursor.resolvedCommand` is mutable per-instance state assigned only by `test()`.**
   `execute()` before a successful `test()` always spawns `cursor-agent`.
4. **antigravity's exit checks omit a done-event guard**, so a non-zero exit always
   throws even when output was produced; and `exitCode === null` (signal kill) still
   yields a synthetic `done`.
5. **`shell` throws on `exit.code === null`** (`"Shell command terminated by signal <sig>"`)
   whereas `createStreamingEvents` deliberately does not.
6. **`readLines` drops empty lines and caps at 16 384 UTF-16 units**, truncating
   mid-JSON; each adapter's `catch` then emits the fragment as an `output` event.
   Pi opts out of both behaviours.
7. **`extractTokens` discards an entire `usage` object when `input_tokens`/`inputTokens`
   is missing**, while opencode's and pi's extractors default fields to 0 (pi
   additionally returns `undefined` when all five fields are 0).
8. **`total` is always recomputed** as `input + output + reasoning`; provider-supplied
   `total`/`totalTokens` is ignored. The opencode test proves it: provider `total: 14502`
   → emitted `total: 12418`.
9. **`classifyAdapterError`'s `'auth'` / `'401'` / `'429'` / `'not found'` checks are
   bare case-insensitive substring matches over the entire message**, which includes any
   appended stderr tail — so stderr content can change the classification of an
   exit-code error.
10. **Codex's error dedup is stateful across lines**, resets on any non-error event, and
    compares `extractErrorMessage(event.data)` (the unwrapped message), not the raw line.
11. **Pi silently drops unknown event types** (`default: return null`); every other
    adapter defaults to an `output` event.
12. **Pi's `waitForSettled` depends on key *presence*** (`hasOwnProperty('willRetry')`),
    not value — `willRetry: false` still waits for `agent_settled`.
13. **`EventBuffer.push` silently drops events pushed after `close()`** and resolves
    rather than throwing.
14. **`killWithGrace` never confirms death after `SIGKILL`** and swallows all kill errors.
15. **`AgentEvent.type` and `RunEventType` are different vocabularies**
    (`output`/`file_change`/`command` vs `agent_output`/`file_changed`/`command_run`);
    the mapping lives above the adapter layer.
