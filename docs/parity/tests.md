# orch-ts Test Suite Inventory — Rust Port Spec

Source repo: `parity/orch-ts` (read-only; nothing was modified).
Generated 2026-10-13.

**Totals:** 135 files under `test/`. 127 are vitest test files (`test/**/*.test.{ts,tsx}` — matches vitest's own count). 2 shared helper modules. 6 non-vitest harness files (3 smoke scripts, 1 battle script, 1 pty driver + 1 expect script).

**Vitest-reported case totals:** 2093 (2085 pass, 8 skipped under `CI=1`).

> Note on the "Cases" column: counted by regex over `it(`/`test(` occurrences. Close to vitest's count but slightly low for files that build cases in loops, slightly high where `it` appears inside a string. Treat as ±2%.

## Classification legend

| Class | Meaning | Rust target |
|---|---|---|
| PORTABLE | Pure logic, no FS/process/render | Rust unit tests (`#[test]`) |
| RENDER-SPECIFIC | Asserts Ink/React rendered output | Re-express against ratatui `TestBackend` |
| PROCESS/IO | Spawns real processes or touches the filesystem | Rust integration tests with `tempfile::tempdir()` |
| NEEDS-REAL-CLI | Requires an actual AI CLI binary installed | Must be skipped or faked |
| HARNESS | Not a vitest test (smoke scripts, battle tests, pty e2e) | Port last or drop |

---

## 1. Per-file inventory

### test/unit/domain — 10 files, ~183 cases

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/domain/adapter-error-kind.test.ts` | 161 | 30 | `AdapterErrorKind` enum, `ERROR_HINTS` table, `classifyAdapterError()`, adapter error display helpers | PORTABLE |
| `test/unit/domain/agent-shop-skills.test.ts` | 159 | 8 | Agent-shop template `skills` references resolve; integrity of the real `skills/library/*.md` files on disk | PROCESS/IO |
| `test/unit/domain/agent-shop.test.ts` | 169 | 21 | Agent Shop catalog, shop wizard step definitions, `applyShopTemplate`, `agentWizardToInput` with skills | PORTABLE |
| `test/unit/domain/errors.test.ts` | 124 | 12 | `OrchestryError` hierarchy — `message`, `hint`, `exitCode`, `name`; `NotInitializedError`, `InvalidArgumentsError`, `LockConflictError`, `AgentAdapterError` | PORTABLE |
| `test/unit/domain/global-config.test.ts` | 37 | 5 | `DEFAULT_GLOBAL_CONFIG` shape and defaults | PORTABLE |
| `test/unit/domain/model-tiers.test.ts` | 134 | 21 | `resolveModel`, `defaultModelForAdapter`, `MODEL_TIER_MAP` completeness, `isAdapterKind`/`isModelTier` guards | PORTABLE |
| `test/unit/domain/org-shop.test.ts` | 76 | 10 | `ORG_TEMPLATES` catalog + `getOrgTemplateByKey()` | PORTABLE |
| `test/unit/domain/run-token-usage.test.ts` | 133 | 15 | `createTokenUsage`, `extractTokens` accumulation semantics | PORTABLE |
| `test/unit/domain/scope.test.ts` | 178 | 29 | `scopesOverlap` predicate + `ScopeIndex` path-scope collision detection | PORTABLE |
| `test/unit/domain/transitions.test.ts` | 262 | 32 | State machine: `canTransition`, `isTerminal`, `isDispatchable`, `isBlocked`, `Map<string, Task>` overload | PORTABLE |

### test/unit/application — 25 test files + 1 helper, ~429 cases

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/application/helpers.ts` | 283 | — | Shared mock factories — see section 2 | HELPER |
| `test/unit/application/agent-factory.test.ts` | 86 | 12 | `isMcpSkill` predicate, `templateToAgentInput` mapping | PORTABLE |
| `test/unit/application/agent-service.test.ts` | 562 | 39 | AgentService CRUD, `get`, `remove`, disable/enable, `findBestAgent`, claim/release | PORTABLE |
| `test/unit/application/defensive-validation.test.ts` | 404 | 42 | TaskService priority validation [1-4] on create/update; Orchestrator retry_queue bounds + dedup; ContextStore TTL bounds | PROCESS/IO |
| `test/unit/application/error-kind-propagation.test.ts` | 306 | 9 | `errorKind` propagation adapter → run event → task (commit 593cee9); uses fake timers | PORTABLE |
| `test/unit/application/event-bus.test.ts` | 257 | 20 | `EventBus`: `on`/`emit`, `off`, `once`, `onAny`, listener leak safety | PORTABLE |
| `test/unit/application/goal-service-autonomous.test.ts` | 560 | 36 | GoalService autonomous-mode side effects on create, delete, `updateStatus`, assignee change | PORTABLE |
| `test/unit/application/message-service.test.ts` | 348 | 15 | Direct message, broadcast, lead channel, `drainMailbox` | PORTABLE |
| `test/unit/application/orchestrator-auto-review-approve.test.ts` | 205 | 6 | `autoApprove` × `review_criteria` interaction | PORTABLE |
| `test/unit/application/orchestrator-bugfixes-2026-03.test.ts` | 456 | 8 | P2 proof detection via tool_call file-path extraction; P3 orphaned `preparing` run cleanup at startup; per-agent `stall_timeout_ms` in reconcile | PORTABLE |
| `test/unit/application/orchestrator-dispatch-worktree.test.ts` | 246 | 7 | `dispatchTask` worktree stale-overwrite fix; graceful null on `freshTask` re-read | PORTABLE |
| `test/unit/application/orchestrator-endurance.test.ts` | 602 | 17 | Memory bounds, retry-queue saturation, stats accumulation, event-bus listener leaks | PROCESS/IO |
| `test/unit/application/orchestrator-force-review.test.ts` | 113 | 3 | `forceTaskToReview` clears `agent.current_task` | PORTABLE |
| `test/unit/application/orchestrator-mutex.test.ts` | 102 | 4 | `stateMutex` promise-chain serialization of state mutations | PORTABLE |
| `test/unit/application/orchestrator-parallel-race.test.ts` | 228 | 4 | Issue #8: parallel run race — `activeCollectors` duplicate-collect guard | PORTABLE |
| `test/unit/application/orchestrator-perf-opts.test.ts` | 191 | 12 | `claimed` Set operations + JSON serialization round-trip; reconcile parallel task reads via `Promise.all` | PORTABLE |
| `test/unit/application/orchestrator-resilience.test.ts` | 452 | 15 | `consecutiveTickFailures` counter; agent-stats update emits `orchestrator:error`; SIGINT/SIGTERM handlers; structured error events instead of `console.error` | PORTABLE |
| `test/unit/application/orchestrator-retry-race.test.ts` | 162 | 5 | Retry dispatch race-condition guards | PORTABLE |
| `test/unit/application/orchestrator-skills.test.ts` | 608 | 13 | Library-skill injection into `systemPrompt`; MCP skills skipped; mixed library+MCP; no skills leaves `systemPrompt` unchanged | PORTABLE |
| `test/unit/application/orchestrator.test.ts` | 1401 | 48 | **Core orchestrator**: constructor, `requireOwnership`, `startWatch`+`stop`, full tick (reconcile + dispatch + collect) | PORTABLE |
| `test/unit/application/review-runner.test.ts` | 180 | 13 | `ReviewRunner.runAll`, `allPassed`, `formatReport`; asserts `execFile` called with `cwd: '/my/project'` and `timeout: 60_000` | PROCESS/IO |
| `test/unit/application/run-service.test.ts` | 204 | 7 | `RunService.getLastFailedRunContext` | PORTABLE |
| `test/unit/application/task-service-assignee.test.ts` | 166 | 11 | Issue #7: assignee name→ID resolution in `TaskService.create`, `TaskService.assign`, `AgentService.findBestAgent` name fallback | PORTABLE |
| `test/unit/application/task-service-attachments.test.ts` | 280 | 19 | Task `attachments` field: domain shape, Paths support, undefined / empty array / real file copying | PROCESS/IO |
| `test/unit/application/task-service.test.ts` | 313 | 26 | TaskService `create`, `depends_on` validation, `get`, `updateStatus` | PORTABLE |
| `test/unit/application/team-service.test.ts` | 327 | 23 | TeamService `create`, `join`, `leave`, `addTask`, `removeTask`, `setLead`, `disband` | PORTABLE |

### test/unit/infrastructure — 38 files, ~562 cases

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/infrastructure/adapter-utils.test.ts` | 56 | 8 | `buildFullPrompt`, `createStderrTailCapture` | PORTABLE |
| `test/unit/infrastructure/agent-store.test.ts` | 164 | 13 | AgentStore YAML CRUD + `_index.json` cache | PROCESS/IO |
| `test/unit/infrastructure/antigravity-adapter.test.ts` | 142 | 6 | AntigravityAdapter argv, event parsing, version probe | PORTABLE (DI seam) |
| `test/unit/infrastructure/cached-stores.test.ts` | 176 | 17 | `CachedTaskStore` / `CachedAgentStore` read-through + invalidation | PORTABLE |
| `test/unit/infrastructure/claude-adapter.test.ts` | 449 | 25 | ClaudeAdapter argv, stream-json event parse, `test()`, `stop()`, `kind` | PORTABLE (DI seam) |
| `test/unit/infrastructure/clipboard-service.test.ts` | 306 | 25 | `isClipboardToolAvailable`, `detectClipboardType`, macOS / linux / WSL code paths | PROCESS/IO |
| `test/unit/infrastructure/codex-adapter.test.ts` | 537 | 26 | CodexAdapter `exec --json --sandbox danger-full-access -` argv, stdin prompt composition, JSONL event parse | PORTABLE (DI seam) |
| `test/unit/infrastructure/concurrent-access.test.ts` | 220 | 8 | Parallel `appendJsonl`, concurrent state mutations, concurrent lock scenarios, JSONL file integrity | PROCESS/IO |
| `test/unit/infrastructure/context-store.test.ts` | 223 | 19 | ContextStore CRUD, TTL expiry, `_index.json` cache | PROCESS/IO |
| `test/unit/infrastructure/cursor-adapter.test.ts` | 338 | 20 | CursorAdapter argv + event parse, `test()`, `stop()`, `kind` | PORTABLE (DI seam) |
| `test/unit/infrastructure/error-paths.test.ts` | 355 | 32 | FS errors on save (read-only dirs via `chmod 0o444`), `JSON.parse` failures in stores, `process.kill` failures, lock error paths | PROCESS/IO |
| `test/unit/infrastructure/event-buffer.test.ts` | 123 | 10 | `EventBuffer` ring-buffer semantics | PORTABLE |
| `test/unit/infrastructure/fs-endurance.test.ts` | 313 | 6 | Large-file / high-volume FS behavior; `describe(..., { timeout: 30_000 })` | PROCESS/IO |
| `test/unit/infrastructure/fs-utils.test.ts` | 220 | 21 | `atomicWrite`, `readYaml`/`writeYaml`, `readJson`/`writeJson`, `appendJsonl`/`readJsonl`, `ensureDir` | PROCESS/IO |
| `test/unit/infrastructure/global-config-store.test.ts` | 57 | 3 | GlobalConfigStore palette migration on read | PORTABLE (mocks `fs-utils`) |
| `test/unit/infrastructure/goal-store.test.ts` | 167 | 13 | GoalStore CRUD + `_index.json` cache | PROCESS/IO |
| `test/unit/infrastructure/grok-adapter.test.ts` | 169 | 7 | GrokAdapter argv + event parse | PORTABLE (DI seam) |
| `test/unit/infrastructure/indexed-store.test.ts` | 317 | 20 | `IndexManager` with YAML items (`.yml`), JSON items (`.json`) + `fileFilter`, custom `readItem`, edge cases | PROCESS/IO |
| `test/unit/infrastructure/lock-concurrency.test.ts` | 77 | 5 | `acquireLock` TOCTOU race conditions | PROCESS/IO |
| `test/unit/infrastructure/lock.test.ts` | 73 | 8 | `acquireLock`, `releaseLock`, `checkLock` PID lock | PROCESS/IO |
| `test/unit/infrastructure/merge-strategy.test.ts` | 155 | 7 | `MergeStrategy.mergeBack`: exact `git` argv; `git merge --abort` fires only on conflict, not on other failures | PORTABLE (injected pm) |
| `test/unit/infrastructure/message-store-index.test.ts` | 168 | 9 | MessageStore `_index.json` cache | PROCESS/IO |
| `test/unit/infrastructure/message-store.test.ts` | 157 | 7 | MessageStore CRUD, pending, `purgeExpired` | PROCESS/IO |
| `test/unit/infrastructure/model-discovery.test.ts` | 193 | 8 | Model-discovery output parsers + per-adapter model discovery | PORTABLE |
| `test/unit/infrastructure/opencode-adapter.test.ts` | 468 | 21 | OpenCodeAdapter `kind`, argv, event parse, `test()`, `stop()` | PORTABLE (DI seam) |
| `test/unit/infrastructure/paths.test.ts` | 144 | 25 | `Paths` getters, `sanitizeId`, `findProjectRoot` cache, `validateWorkspacePath` | PORTABLE |
| `test/unit/infrastructure/pi-adapter.test.ts` | 556 | 24 | PiAdapter RPC JSONL over stdin/stdout, long-lived process, usage extraction, `test()`, `stop()`, `kind` | PORTABLE (DI seam) |
| `test/unit/infrastructure/process-endurance.test.ts` | 258 | 6 | **Real** `sleep` / `bash` processes: PID liveness, process-group kill, cleanup. `describe(..., { timeout: 30_000, skip: isCI })` | PROCESS/IO (skipped in CI) |
| `test/unit/infrastructure/process-manager.test.ts` | 51 | 3 | `ProcessManager.spawn` calls `unref()`, uses `detached: true`, throws on missing pid | PROCESS/IO |
| `test/unit/infrastructure/run-store.test.ts` | 90 | 4 | `RunStore.streamEvents` tailing; spies `Date.now` | PROCESS/IO |
| `test/unit/infrastructure/shell-adapter.test.ts` | 253 | 10 | ShellAdapter arbitrary command, stdout/stderr → events, exit-code mapping | PORTABLE (injected pm) |
| `test/unit/infrastructure/skill-loader.test.ts` | 181 | 14 | `SkillLoader.loadSkills`, `listAvailable`, in-memory cache, path-traversal guard, integration against the **real** `skills/library` | PROCESS/IO |
| `test/unit/infrastructure/state-store.test.ts` | 167 | 12 | StateStore `state.json` read/write, `claimed` Set serialization | PROCESS/IO |
| `test/unit/infrastructure/storage/index-manager-mutex.test.ts` | 92 | 3 | IndexManager mutex serialization | PROCESS/IO |
| `test/unit/infrastructure/task-store.test.ts` | 181 | 14 | TaskStore CRUD + `_index.json` cache | PROCESS/IO |
| `test/unit/infrastructure/team-store.test.ts` | 103 | 7 | TeamStore CRUD | PROCESS/IO |
| `test/unit/infrastructure/template-engine.test.ts` | 565 | 37 | `buildPromptContext` (incl. team listing), `LiquidTemplateEngine` timeout, retry context, `filterRelevantContext` | PORTABLE |
| `test/unit/infrastructure/workspace-manager.test.ts` | 362 | 20 | `resolveMode` (via `prepare`), `prepareWorktree`, `prepareIsolated`, `cleanup`; git argv via mock pm + real tempdirs | PROCESS/IO |

### test/unit/cli — 18 test files + 1 helper, ~224 cases

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/cli/helpers.ts` | 103 | — | `makeContainer()` factory — see section 2 | HELPER |
| `test/unit/cli/cli-lazy-routing.test.ts` | 283 | 25 | Light commands register with `LightContainer`; full commands with `Container`; `Container extends LightContainer`; LiquidTemplateEngine lazy loading; CLI routing logic | PORTABLE |
| `test/unit/cli/commands-config.test.ts` | 84 | 8 | `config get`, `config set`, `config global`; JSON-mode output parses | PORTABLE |
| `test/unit/cli/commands-context.test.ts` | 189 | 14 | `context set`/`get`/`list`/`delete`; table rendering + JSON mode | PORTABLE |
| `test/unit/cli/commands-doctor.test.ts` | 126 | 6 | `doctor` check rendering; JSON mode (filters for first parseable `console.log` call, then asserts on the parsed object) | PORTABLE |
| `test/unit/cli/commands-init.test.ts` | 367 | 31 | `getDefaultAgents()` and `init` scaffolding; all fs + `execFile` mocked via `vi.hoisted` + a `MockPaths` class | PORTABLE |
| `test/unit/cli/commands-logs-since-all.test.ts` | 186 | 6 | `logs --since` across all runs with no filter; JSON mode | PORTABLE |
| `test/unit/cli/commands-logs.test.ts` | 299 | 18 | `logs <run-id>`, `--task`, `--agent`, `--since`; tail counts (`readEventsTail(id, 50)` vs `(id, 10)`); "No events" path | PORTABLE |
| `test/unit/cli/commands-run.test.ts` | 112 | 8 | `run <task-id>`, `run --all`, no-args behavior | PORTABLE |
| `test/unit/cli/commands-serve.test.ts` | 109 | 8 | watch mode (default), `--tick-interval`, `--once` mode, `--log-format` validation | PORTABLE |
| `test/unit/cli/commands-task-cancel.test.ts` | 85 | 3 | `orchestrator.cancelTask` for in_progress vs `taskService.cancel` for non-running and review tasks | PORTABLE |
| `test/unit/cli/commands-task-time.test.ts` | 183 | 7 | `task list` TIME column: `AGE(<ts>)` header, `formatDurationSince(UPDATED)` not `(CREATED)`, `dim('—')` fallback; asserts `printTable` rows | PORTABLE |
| `test/unit/cli/create-context.test.ts` | 90 | 11 | `createContext` NO_COLOR env handling + other fields | PORTABLE |
| `test/unit/cli/disk-observer-integration.test.ts` | 325 | 4 | DiskObserver against real `.orchestry` directory trees | PROCESS/IO |
| `test/unit/cli/disk-observer.test.ts` | 303 | 11 | DiskObserver state polling + JSONL tailing with real timers | PROCESS/IO |
| `test/unit/cli/editor-agent.test.ts` | 320 | 21 | `agentToEditorContent` / `agentFromEditorContent` round-trip | PORTABLE |
| `test/unit/cli/editor.test.ts` | 68 | 8 | `toEditorContent` / `fromEditorContent`, `openInEditor` temp-directory cleanup | PORTABLE |
| `test/unit/cli/output-lazy-chalk.test.ts` | 53 | 6 | Lazy chalk initialization in `output.ts` | PORTABLE |
| `test/unit/cli/postinstall.test.ts` | 106 | 10 | `scripts/postinstall.cjs` source content; CI suppression; non-TTY suppression — **runs real `node`** via `execFileSync`/`spawnSync`, `{ timeout: 5000 }` ×3 | PROCESS/IO |
| `test/unit/cli/serve-once-runner.test.ts` | 133 | 8 | `runOnce` terminal-status detection and exit code | PORTABLE |
| `test/unit/cli/structured-logger.test.ts` | 327 | 21 | `StructuredLogger` JSON format, `agent:output` verbose filtering, idle-tick throttling (every 6th), text format | PORTABLE |

### test/unit/tui — 30 files, ~681 cases

RENDER-SPECIFIC subset (all use `ink-testing-library` `render()` + `lastFrame()`, driven by `stdin.write(...)` + `await delay(ms)`):

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/tui/app.test.tsx` | 2895 | 140 | Header, TaskList, Footer, whole App, command bar + tab completion, wizard flows, hotkeys. **Largest file in the suite** | RENDER-SPECIFIC |
| `test/unit/tui/form-wizard.test.tsx` | 1535 | 54 | FormWizard textarea steps, inline validation, clipboard paste (`onPasteImage` / `footerExtra`) | RENDER-SPECIFIC |
| `test/unit/tui/help-overlay.test.tsx` | 691 | 89 | HelpOverlay + `resolveSuggestions`, `resolveCompletion`, `CommandHistory`, CommandBar `?` hint. **MIXED** — resolver/history describes are pure logic; only overlay rendering is render-bound. Holds both skipped tests | RENDER-SPECIFIC (mixed) |
| `test/unit/tui/form-wizard-validation.test.tsx` | 557 | 22 | FormWizard inline validation with fake timers | RENDER-SPECIFIC |
| `test/unit/tui/text-input.test.tsx` | 502 | 33 | Unified text input: FormWizard text-step, Ctrl+Z undo, visual rendering | RENDER-SPECIFIC |
| `test/unit/tui/goal-task-linking.test.tsx` | 389 | 21 | TaskRow goal badge, GoalRow progress bar, GoalSectionRow / UngroupedSectionRow headers, `G` group-by-goals toggle | RENDER-SPECIFIC |
| `test/unit/tui/detail-panel-resize.test.tsx` | 339 | 13 | Divider visual hint, `M` fullscreen toggle, `+`/`-` exit fullscreen, logs-view exemption, splitOffset preserved between tabs | RENDER-SPECIFIC |
| `test/unit/tui/onboarding.test.tsx` | 337 | 21 | `OnboardingStep` type, WelcomeScreen, OnboardingNudge, OnboardingToast, App onboarding state machine | RENDER-SPECIFIC |
| `test/unit/tui/tui-endurance.test.tsx` | 301 | 12 | Message-queue cap, runId map LRU, detail truncation, batch-flush efficiency, toast-queue saturation | RENDER-SPECIFIC |
| `test/unit/tui/soft-delete.test.tsx` | 291 | 9 | Soft-delete with undo (`D` key), fake timers | RENDER-SPECIFIC |
| `test/unit/tui/error-hint-panel.test.tsx` | 290 | 14 | AgentRow inline error summary; AgentDetailPanel ErrorHintPanel incl. relative `ago` timestamps | RENDER-SPECIFIC |
| `test/unit/tui/logs-type-filter-picker.test.tsx` | 208 | 19 | `LogsTypeFilterPicker` | RENDER-SPECIFIC |
| `test/unit/tui/app-paste-image.test.tsx` | 184 | 4 | App image paste during task wizard | RENDER-SPECIFIC |
| `test/unit/tui/logs-filter-picker.test.tsx` | 170 | 15 | `LogsFilterPicker` | RENDER-SPECIFIC |
| `test/unit/tui/header-flash.test.tsx` | 138 | 9 | Header tab flash animation (fake timers) | RENDER-SPECIFIC |
| `test/unit/tui/toast-banner.test.tsx` | 123 | 10 | `ToastBanner` (fake timers) | RENDER-SPECIFIC |
| `test/unit/tui/logs-filter-status.test.tsx` | 119 | 4 | Logs filter status bar | RENDER-SPECIFIC |
| `test/unit/tui/model-suggestions.test.tsx` | 91 | 2 | Dynamic model-catalog filter + refresh in an already open agent wizard | RENDER-SPECIFIC |
| `test/unit/tui/logs-search-bar.test.tsx` | 90 | 11 | `LogsSearchBar` | RENDER-SPECIFIC |

PORTABLE subset (pure `.ts`, no rendering):

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/unit/tui/text-cursor.test.ts` | 390 | 61 | `graphemeSegments`, `graphemeLength`, `displayWidth`, `textDisplayWidth`, Cursor construction/movement. **Highest case density in the suite** | PORTABLE |
| `test/unit/tui/wizard-validate-functions.test.ts` | 342 | 34 | Task/agent wizard `validate()` functions: title, priority, name, edit-name with self-exclusion | PORTABLE |
| `test/unit/tui/wizard-configs-team.test.ts` | 196 | 16 | `getEditAgentWizardSteps` team step, `editAgentWizardToFields` `team_id`, team change logic | PORTABLE |
| `test/unit/tui/wizard-effort.test.ts` | 177 | 21 | Agent-wizard effort step `skip()` per adapter: claude/pi/grok shown; codex/shell/opencode/cursor skipped; `{}` skipped | PORTABLE |
| `test/unit/tui/wizard-agent-hint.test.ts` | 121 | 10 | Agent hint `[adapter] role` format, role truncation to 60 chars, hint in goal wizard, disabled agents excluded | PORTABLE |
| `test/unit/tui/wizard-shell.test.ts` | 122 | 7 | Shell agent wizard step set and skip predicates | PORTABLE |
| `test/unit/tui/wizard-model-catalog.test.ts` | 109 | 8 | Agent wizard model catalog | PORTABLE |
| `test/unit/tui/colors.test.ts` | 83 | 6 | TUI color palettes | PORTABLE |
| `test/unit/tui/wizard-palette.test.ts` | 50 | 2 | Config wizard palette setting | PORTABLE |
| `test/unit/tui/onboarding-agents.test.ts` | 22 | 2 | `ONBOARDING_AGENTS` constant | PORTABLE |
| `test/unit/tui/entity-list-changed.test.ts` | 18 | 2 | `entityListChanged` diff helper | PORTABLE |

### test/integration — 4 files, 6 cases

| File | Lines | Cases | Description | Class |
|---|---|---|---|---|
| `test/integration/pi-adapter.e2e.test.ts` | 364 | 2 | Real `PiAdapter` + real `Orchestrator` + real state machine + real JSONL run storage, with a **mocked `ProcessManager.spawn`** returning a controllable child. Asserts (1) dispatch pulls a todo task, finds the pi agent, spawns, writes prompt JSONL to stdin, closes stdin; (2) RPC events on stdout produce matching `AgentEvent`s on the bus; (3) each event appended to run JSONL and task walks `todo → in_progress → review → done` (auto-approve via `makeAgent()`); (4) `processManager.killWithGrace(pid, 1000)` after terminal event; (5) usage from `agent_end` propagates onto the done event. Header comment calls itself "the template for e2e-through-Orchestrator adapter tests" | PORTABLE |
| `test/integration/new-adapters.e2e.test.ts` | 202 | 2 | Same shape for Grok + Antigravity: real adapter classes, `AdapterRegistry`, `Orchestrator`, services, state machine, JSONL path; spawned process mocked so CI needs no live credentials | PORTABLE |
| `test/integration/scope-index.e2e.test.ts` | 173 | 1 | Scope scheduling through the **real** runtime stack: `fs.mkdtemp` project root, `buildFullContainer`, real FS stores, real `ShellAdapter` + `ProcessManager` + PID lock, real child processes (`process.execPath -e "setTimeout(...)"`), `poll_interval_ms: 25`, `max_concurrent_agents: 4`, `waitUntil` with 10s deadline | PROCESS/IO |
| `test/integration/pi-tui.e2e.test.tsx` | 144 | 1 | Renders real `App.tsx` with a pi-adapter agent and todo task; walks agent chip on Agents tab → task in_progress + agent running → activity feed shows `agent:output` / `agent:file_changed` → agent idle + task done | RENDER-SPECIFIC |

### Harness files (not vitest) — 6 files

| File | Lines | Description | Class |
|---|---|---|---|
| `test/smoke-fullcycle.ts` | 411 | Full-cycle TUI E2E: launch → see tasks/agents → `/task add` → `/agent add` → run a task (`R`) → live events in activity feed → approve a review task → reject one → view switching/navigation → watch/pause → data refresh from events. Prints PASS/FAIL; run via `tsx` | HARNESS |
| `test/battle/observer-battle-test.ts` | 386 | DiskObserver cross-process simulation: process A ("external orchestrator") acquires lock and writes state + JSONL; process B ("TUI observer") polls and tails via DiskObserver. Verifies real-time event delivery across process boundaries. `#!/usr/bin/env npx tsx`, ANSI-colored PASS/FAIL | HARNESS |
| `test/smoke-commandbar.ts` | 258 | Command Bar smoke: tab completion, history, `/commands`, view switching | HARNESS |
| `test/smoke-tui.ts` | 220 | Launches the real App with `ink-testing-library`, exercises every US-9.x hotkey, prints PASS/FAIL | HARNESS |
| `test/e2e/tui-codex-logs.pty.ts` | 171 | Node driver for the real-PTY test: seeds a tempdir project (config, global config, state, agent, run metadata + JSONL via `yaml` and JSON), resolves `dist/cli.js`, then `spawnSync`s the expect script | HARNESS |
| `test/e2e/tui-codex-logs.exp` | 86 | `#!/usr/bin/expect -f`, `set timeout 25`, `log_user 0`. Spawns `env ORCHESTRY_GLOBAL_CONFIG_PATH=... /bin/sh -c "stty rows 50 columns 180; export TERM=xterm-256color; exec node \"$cli_path\" tui"`, matches `WATCHING|OBSERVING` for boot then `F:`. Requires the `expect` binary + a built `dist/cli.js`. Invoked only by `npm run test:e2e:tui` | HARNESS |

**NEEDS-REAL-CLI: zero files.** No test in the suite requires claude, codex, cursor, opencode, pi, grok, or antigravity to be installed. Greps for `which` / `command -v` / binary-presence skip guards across `test/` found nothing. Every adapter test fakes the child process.

---

## 2. Shared helper / mock infrastructure

Only **two** shared helper modules exist. There is no `test/fixtures/` directory, no `setupFiles`, and no global setup.

### `test/unit/application/helpers.ts` (283 lines)

Imported by **31 files**: 18 under `test/unit/application`, 9 under `test/unit/cli`, 2 under `test/unit/tui`, 3 under `test/integration`.

#### Entity factories

All are plain object literals with a trailing `...overrides` spread, every value hard-coded and deterministic.

| Factory | Fabricates |
|---|---|
| `makeTask(overrides)` | `{ id: 'tsk_test1', title: 'Test task', description: 'desc', status: 'todo', priority: 3, labels: [], depends_on: [], created_at: '2025-01-01T00:00:00Z', updated_at: '2025-01-01T00:00:00Z', attempts: 0, max_attempts: 3 }` |
| `makeAgent(overrides)` | `{ id: 'agt_test1', name: 'TestAgent', adapter: 'shell', status: 'idle', config: { approval_policy: 'auto', max_turns: 50, timeout_ms: 3_600_000, stall_timeout_ms: 300_000 }, stats: { tasks_completed: 0, tasks_failed: 0, total_runs: 0, total_runtime_ms: 0 } }` |
| `makeRun(overrides)` | `{ id: 'run_test1', task_id: 'tsk_test1', agent_id: 'agt_test1', attempt: 1, status: 'preparing', started_at: '2025-01-01T00:00:00Z', workspace_path: '/tmp/ws', prompt: 'do stuff' }` |

**Important:** `makeAgent()` sets `approval_policy: 'auto'`. This is why the e2e tests observe `review → done` without an explicit approve step.

#### Store fakes

In-memory `Map`-backed. Every method is a `vi.fn()` so call assertions work, and every read and write `structuredClone`s to prevent aliasing bugs leaking between the SUT and the test.

| Factory | Behavior |
|---|---|
| `createMockTaskStore(tasks[])` | Real filter logic for `filter.status` and `filter.goalId`; `get`/`save`/`delete` over a Map |
| `createMockAgentStore(agents[])` | Adds `getByName` (linear scan over values) |
| `createMockRunStore()` | Two Maps — runs, and events-per-run. `appendEvent`/`readEvents` are real; `listForTask`/`listForAgent`/`readEventsTail` return `[]`; `streamEvents` is an empty async generator; `closeRunEvents` a bare `vi.fn()` |
| `createMockStateStore(initial?)` | `{ ...DEFAULT_STATE, ...initial }` held in a closure var; `read`/`write` clone |
| `createMockContextStore()` | No-op stubs: `get` → null, `set`/`delete` → void, `list` → `[]`, `getAll` → `{}` |
| `createMockMessageStore()` | No-op stubs; `purgeExpired` → `0` |
| `createMockTeamStore()` | No-op stubs; `get`/`getByName` → null, `list` → `[]` |

#### Injected seams

| Seam | Fake | Notes |
|---|---|---|
| **Process spawn** | `createMockProcessManager()` → `{ isAlive: vi.fn(() => true), kill: vi.fn(), killWithGrace: vi.fn(async () => {}), spawn: vi.fn(() => ({ process: {} as any, pid: 12345 })) }` | Fixed pid 12345; `process` is an empty object, so adapter tests that need streams build their own richer fake |
| **Filesystem** | **Not faked at this layer.** Faked by swapping whole *stores* instead | There is no fs abstraction in `OrchestratorDeps` |
| **Workspace / git** | `createMockWorkspaceManager()` → `prepare: async () => ({ path: '/tmp/ws' })`, `mergeBack: async () => ({ success: true as const })`, `cleanup`, `validate` | |
| **Template rendering** | `createMockTemplateEngine()` → `render: vi.fn(async () => 'rendered prompt')` | Constant string regardless of input |
| **Agent adapter** | `createMockAdapter(events)` → `{ kind: 'shell', test: async () => ({ ok: true }), execute: () => ({ pid: 99999, events: makeEventGenerator(events) }), stop }`, paired with `makeEventGenerator(events)` — an `async function*` that yields each event | The canonical adapter fake |
| **Clock** | **NOT injected. No clock seam exists anywhere in the helper.** | Timestamps come from real `new Date()`. Only 7 files use `vi.useFakeTimers()`; only `run-store.test.ts` spies `Date.now` |
| **ID generator** | **NOT injected. No nanoid mock anywhere in `test/`.** | Tests assert ID *prefixes* (`tsk_`, `agt_`, `run_`) rather than exact generated values |

#### `buildDeps(overrides)`

Assembles a complete `OrchestratorDeps`. Two details matter for the port:

1. It resolves each store **once** (`const taskStore = overrides.taskStore ?? createMockTaskStore()`) and then threads the *same* instance into the real services. So passing one mock store means both the orchestrator and the services observe it.
2. It uses **real** `EventBus`, **real** `AdapterRegistry`, **real** `TaskService`, **real** `AgentService`, **real** `RunService`. Only the stores and the I/O edges are faked.

```ts
const config = overrides.config ?? {
  ...DEFAULT_CONFIG,
  scheduling: { ...DEFAULT_CONFIG.scheduling, poll_interval_ms: 100_000 },
};
```

`poll_interval_ms: 100_000` means the tick loop never fires on its own — every test drives ticks manually. Fixed `projectRoot: '/tmp/project'` and `lockPath: '/tmp/project/.orchestry/lock'`. The trailing `...overrides` spread means an override wins over everything, including the pre-resolved stores.

#### `cleanupOrch(orch)`

Reaches into orchestrator privates via `as any` to clear `intervalId`, `immediateDispatchTimer`, `saveStateTimer`, set `shuttingDown = true`, call `removeSignalHandlers?.()`, and clear `lockAcquired`. Prevents timer leaks between tests. In Rust this is a `Drop` / explicit-shutdown concern, not a test helper.

### `test/unit/cli/helpers.ts` (103 lines)

Imported by 9 CLI test files. One function, `makeContainer(overrides)`, returning a fully-populated `Container` where every service method is a `vi.fn()` with a canned resolved value — e.g. `taskService.get` → `{ id: 'tsk_1', title: 'Test task', status: 'todo' }`, `agentService.list` → `[{ id: 'agt_1', name: 'A1' }]`, `doctorService.runAll` → `{ checks: [], passed: 0, failed: 0, warnings: 0 }`.

The `context` field carries the output-mode flags that drive CLI formatting:

```ts
context: { json: false, quiet: false, noColor: false, ascii: false, projectRoot: '/tmp' },
```

Tests flip `json: true` to exercise JSON output. The whole object is cast `as any` at the end, so it is structurally incomplete on purpose.

### Ad-hoc per-file helpers (duplicated across ~10 adapter test files)

The same two functions are copy-pasted into every adapter test file, with only `pid` and the `stdin` initializer varying:

```ts
function createMockProcess() {
  const proc = new EventEmitter() as EventEmitter & {
    stdout: PassThrough;
    stderr: PassThrough;
    stdin: PassThrough | null;
    pid: number;
    kill: () => void;
  };
  proc.stdout = new PassThrough();
  proc.stderr = new PassThrough();
  proc.stdin = null;            // or new PassThrough() for stdin-writing adapters
  proc.pid = 5555;
  proc.kill = vi.fn();
  return proc;
}

function createMockProcessManager(proc: ReturnType<typeof createMockProcess>): IProcessManager {
  return {
    isAlive: vi.fn(() => true),
    kill: vi.fn(),
    killWithGrace: vi.fn(async () => {}),
    spawn: vi.fn(() => ({ process: proc as any, pid: proc.pid })),
  };
}
```

Plus a `makeParams(overrides)` producing an `ExecuteParams`. For the Rust port this should be one shared `FakeProcess` + `FakeProcessManager` in a test-support module, not ten copies.

---

## 3. How child processes and adapters are faked

Four distinct mechanisms, ordered by how much of the suite relies on each.

### (a) Injected `IProcessManager` returning a hand-built `EventEmitter` + `PassThrough` pair

The dominant pattern — all 8 adapter tests plus `merge-strategy` and `workspace-manager`. The adapter under test is constructed with the fake manager, `execute()` is called, and then the test **drives the fake process from outside**: it writes bytes into `proc.stdout` / `proc.stderr` and emits `proc.emit('exit', code, signal)` or `proc.emit('error', err)`, asserting the resulting `AgentEvent` stream.

Spawn arguments are asserted directly on the spy:

```ts
expect(pm.spawn).toHaveBeenCalledWith(
  'codex',
  expect.arrayContaining(['exec', '--json', '--sandbox', 'danger-full-access', '-']),
  expect.objectContaining({
    cwd: '/tmp/codex-ws',
    stdio: ['pipe', 'pipe', 'pipe'],
  }),
);
```

And stdin payloads on a spy over the fake's stdin:

```ts
const writeSpy = vi.spyOn(proc.stdin, 'write');
adapter.execute(makeParams({ systemPrompt: 'system instructions', prompt: 'user task' }));
expect(writeSpy).toHaveBeenCalledWith('system instructions\n\nuser task');
```

```ts
adapter.execute(makeParams({ prompt: 'just the task', systemPrompt: undefined }));
expect(writeSpy).toHaveBeenCalledWith('just the task');
```

### (b) Module-level `vi.mock('node:child_process')` with `importOriginal` passthrough

11 files. Used for the *version-probe* path (`adapter.test()` calls `execFile`) and for `ProcessManager` itself. Hoisted to the top of the file with a default success, then overridden per-test via `mockImplementationOnce`:

```ts
// Top-level mock so vi.mock hoisting applies to the whole module.
// execFile is intercepted; by default it succeeds with a version string.
// Individual tests override this via mockImplementationOnce.
vi.mock('node:child_process', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:child_process')>();
  return {
    ...actual,
    execFile: vi.fn(
      (
        _cmd: string,
        _args: string[],
        cb: (err: Error | null, stdout: string, stderr: string) => void,
      ) => {
        cb(null, 'claude/1.0.0', '');
      },
    ),
  };
});
```

Files using this: `review-runner`, `commands-init`, `antigravity-adapter`, `claude-adapter`, `clipboard-service`, `codex-adapter`, `cursor-adapter`, `grok-adapter`, `opencode-adapter`, `pi-adapter`, `process-manager`.

`process-manager.test.ts` mocks `spawn` the same way and asserts on the options object and side effects:

```ts
vi.mock('node:child_process', async (importOriginal) => {
  const actual = await importOriginal<typeof childProcess>();
  return { ...actual, spawn: vi.fn() };
});
// ...
const spawnOpts = vi.mocked(childProcess.spawn).mock.calls[0][2];
expect(spawnOpts?.detached).toBe(true);
expect(mockUnref).toHaveBeenCalledOnce();
```

### (c) `vi.hoisted()` + whole-module replacement

`commands-init.test.ts` is the cleanest example: a `vi.hoisted` block builds the fs spies **and a `MockPaths` class** with every path hard-coded, then `vi.mock` swaps in both `fs-utils.js` and `paths.js`. No filesystem is touched at all.

```ts
const mocks = vi.hoisted(() => {
  const ensureDir = vi.fn(async () => {});
  const pathExists = vi.fn(async () => false as boolean);
  const writeYaml = vi.fn(async () => {});
  const atomicWrite = vi.fn(async () => {});
  const agentPathFn = vi.fn((id: string) => `/mock/.orchestry/agents/${id}.yml`);

  class MockPaths {
    readonly root = '/mock/.orchestry';
    readonly tasksDir = '/mock/.orchestry/tasks';
    readonly agentsDir = '/mock/.orchestry/agents';
    // ... runsDir, templatesDir, logsDir, configPath, gitignorePath,
    //     workspaceExcludePath, defaultTemplatePath, agentPath
  }

  const execFile = vi.fn((_cmd, _args, _opts, cb) => { cb(null); });
  return { ensureDir, pathExists, writeYaml, atomicWrite, agentPathFn, MockPaths, execFile };
});

vi.mock('../../../src/infrastructure/storage/fs-utils.js', () => ({
  ensureDir: mocks.ensureDir,
  pathExists: mocks.pathExists,
  writeYaml: mocks.writeYaml,
  atomicWrite: mocks.atomicWrite,
  // ...
}));
```

`clipboard-service.test.ts` uses the same shape against `node:fs/promises`:

```ts
vi.mock('node:fs/promises', () => ({
  mkdtemp: mkdtempMock,
  readFile: readFileMock,
  unlink: unlinkMock,
  rm: rmMock,
  writeFile: writeFileMock,
}));
```

### (d) Real processes and real filesystem

Three places only:

- `test/unit/infrastructure/process-endurance.test.ts` spawns actual `sleep` and `bash -c` processes through the real `ProcessManager` to verify PID liveness and process-group kill. Header comment: *"Integration tests using real shell processes — NOT mocks. Skipped in CI where process spawning may be restricted."*
- `test/integration/scope-index.e2e.test.ts` uses the Node binary itself as a deterministic workload:
  ```ts
  const command = `${JSON.stringify(process.execPath)} -e "setTimeout(() => console.log('scope-e2e-done'), 350)"`;
  ```
- `test/unit/cli/postinstall.test.ts` runs the actual `scripts/postinstall.cjs` via `execFileSync` / `spawnSync` to test CI and non-TTY suppression.

**No fake shell scripts are written to disk anywhere in the suite.** The only `chmod` calls are in `error-paths.test.ts`, making tempdirs read-only (`0o444`) to provoke FS errors, then restoring `0o755` in a `finally`:

```ts
await fs.chmod(readonlyDir, 0o444);
try { /* assert the store surfaces an error */ }
finally { await fs.chmod(readonlyDir, 0o755); }
```

### Mock-target census

| `vi.mock` target | Files |
|---|---|
| `node:child_process` | 11 |
| `src/infrastructure/storage/lock.js` | 8 |
| `src/infrastructure/storage/paths.js` | 2 |
| `src/infrastructure/storage/fs-utils.js` | 2 |
| `src/infrastructure/template/template-engine.js` | 1 |
| `src/infrastructure/clipboard-service.js` | 1 |
| `src/domain/config.js` | 1 |
| `src/container.js` | 1 |
| `src/cli/output.js` | 1 |
| `src/cli/editor.js` | 1 |
| `src/application/review-runner.js` | 1 |
| `node:fs/promises` | 1 |

The lock mock (8 files, all orchestrator tests) is uniformly:

```ts
vi.mock('../../../src/infrastructure/storage/lock.js', () => ({
  acquireLock: vi.fn(async () => ({ acquired: true, pid: process.pid })),
  releaseLock: vi.fn(async () => {}),
  touchLock: vi.fn(async () => {}),
}));
```

---

## 4. vitest.config.ts

```ts
import { availableParallelism } from 'node:os';
import { defineConfig } from 'vitest/config';

const isCI = !!process.env['CI'];
const maxThreads = isCI ? 2 : Math.max(2, availableParallelism());

export default defineConfig({
  test: {
    globals: true,
    environment: 'node',
    include: ['test/**/*.test.{ts,tsx}'],
    pool: 'threads',
    maxWorkers: maxThreads,
    typecheck: {
      tsconfig: 'tsconfig.test.json',
    },
    coverage: {
      provider: 'v8',
      include: ['src/**/*.ts'],
      exclude: ['src/bin/**', 'src/cli/tui/**'],
    },
  },
  optimizeDeps: {
    include: ['vitest', 'js-yaml', 'nanoid', 'commander'],
  },
});
```

| Setting | Value | Why it matters |
|---|---|---|
| `pool` | `'threads'` | Worker threads, not forks — shared-process state leaks are possible |
| `maxWorkers` | `availableParallelism()` locally, **2 under `CI=1`** | This single line is the difference between a green and a red suite — see section 5 |
| `include` | `test/**/*.test.{ts,tsx}` | Smoke / battle / pty harnesses are deliberately **outside** the vitest run |
| `setupFiles` | **absent** | No global setup, no shared beforeEach |
| `globalSetup` | **absent** | |
| `test.env` | **absent** | Env vars are read by tests, never injected by config |
| `testTimeout` / `hookTimeout` | **absent** | Default 5s per test applies everywhere except three opt-ins below |
| `globals` | `true` | Bare `describe`/`it`/`expect` work, though most files import them explicitly |
| `environment` | `'node'` | No jsdom even for the Ink `.tsx` tests |
| `coverage.exclude` | `['src/bin/**', 'src/cli/tui/**']` | The TUI actually lives at `src/tui/**`, so TUI code **is** counted. Minor pre-existing config bug; harmless |

### Explicit timeouts (options-object form)

| Location | Timeout |
|---|---|
| `test/unit/infrastructure/fs-endurance.test.ts:40` | `describe('FS endurance', { timeout: 30_000 }, ...)` |
| `test/unit/infrastructure/process-endurance.test.ts:38` | `describe('ProcessManager endurance', { timeout: 30_000, skip: isCI }, ...)` |
| `test/unit/cli/postinstall.test.ts:70,80,100` | `it(..., { timeout: 5000 })` ×3 |

### Env vars read by tests

| Var | Uses |
|---|---|
| `NO_COLOR` | 5 |
| `ORCHESTRY_GLOBAL_CONFIG_PATH` | 3 (incl. the pty harness) |
| `CI` | 3 (skip guard + worker cap) |

### Observed wall-clock

| Mode | Duration |
|---|---|
| `CI=1` (2 workers) | ~55s (transform 2.22s, import 8.72s, tests 86.15s cumulative) |
| Default (full parallelism) | 2-4 min (transform 175.89s, import 231.85s cumulative — import/transform dominates) |

---

## 5. Failing and skipped tests

### There is no deterministic failing test

The brief anticipated "ONE currently failing test". That premise does not hold. Four runs:

| Run | Config | Result |
|---|---|---|
| 1 | default (`maxWorkers = availableParallelism()`) | **6 failed** / 2085 passed / 2 skipped — exit 1 |
| 2 | default, re-run | **5 failed** / 2086 passed / 2 skipped — exit 1, **different failure set** |
| 3 | `--maxWorkers=1`, the 4 failing files only | **210 / 210 passed** — exit 0 |
| 4 | `CI=1` (`maxWorkers=2`), full suite | **0 failed** / 2085 passed / 8 skipped — **exit 0** |

Additionally, `model-suggestions.test.tsx` + `app.test.tsx` were run three times in isolation: 142/142 passed each time.

**Root cause.** Every failure is in a `.tsx` Ink test that writes to stdin, waits a fixed delay, then asserts on `lastFrame()`:

```ts
stdin.write('\r');
await delay(50);
expect(lastFrame()).toContain('Model');
```

Under heavy thread contention the React/Ink re-render does not land inside the fixed delay, so the assertion observes the previous frame. A classic fixed-sleep race, not a logic bug.

### Observed failures across both default-parallelism runs

| Test | Run 1 | Run 2 | Symptom |
|---|---|---|---|
| `app.test.tsx > App > creates a shell agent through the minimal A → N wizard path` | FAIL | FAIL | frame still shows the Agents-tab hint card, not `Agent name` (`app.test.tsx:632`) |
| `model-suggestions.test.tsx > model suggestions > refreshes model suggestions in an already open agent wizard` | FAIL | FAIL | frame still on `NEW AGENT ── step 1/5`, `Model` not yet rendered (`model-suggestions.test.tsx:81`) |
| `error-hint-panel.test.tsx > AgentDetailPanel — ErrorHintPanel > shows relative timestamp with "ago" suffix` | FAIL | — | detail panel not open yet (`error-hint-panel.test.tsx:261`) |
| `form-wizard.test.tsx > FormWizard textarea > textarea works as non-first step after text input` | FAIL | — | got `body: "Line A"`, expected `"Line A\nLine B"` (`form-wizard.test.tsx:750`) |
| `form-wizard.test.tsx > FormWizard textarea > paste multiline text in the middle of existing text` | FAIL | — | got `"HelloWA\nB\nCorld"`, expected `"HelloA\nB\nCWorld"` — a keystroke landed mid-paste (`form-wizard.test.tsx:815`) |
| `model-suggestions.test.tsx > model suggestions > filters a large dynamic catalog and submits the selected model id` | FAIL | — | got `model: "terra"`, expected `"gpt-5.6-terra"` — filter text only partially applied (`model-suggestions.test.tsx:49`) |
| `app.test.tsx > App > executes cancel command on selected task` | — | FAIL | |
| `form-wizard.test.tsx > FormWizard textarea > Backspace at start of line merges with previous line` | — | FAIL | |
| `soft-delete.test.tsx > Soft-delete with undo > D key on agents view schedules agent deletion` | — | FAIL | |

**Do not treat any of these as known-broken behavior to replicate.** The lesson is structural: when re-expressing these against ratatui's `TestBackend`, drive the frame synchronously (push key event → `terminal.draw()` → assert buffer) instead of porting the sleep-and-hope pattern. Roughly 20 TUI files and ~500 cases currently depend on timing that Rust can make deterministic.

### Skipped tests — 2 unconditional

Both in `test/unit/tui/help-overlay.test.tsx`, with the reason stated in the title:

| Line | Test |
|---|---|
| 610 | `it.skip('shows HelpOverlay after pressing F1 (\x1bOP) [terminal escape sequence — not testable via stdin]', ...)` |
| 638 | `it.skip('dismisses HelpOverlay on F1 press when already open [terminal escape sequence — not testable via stdin]', ...)` |

These are **not** skipped for logic reasons — `ink-testing-library` cannot deliver an F1 escape sequence through its fake stdin. In ratatui they become trivially testable: construct a `KeyEvent::from(KeyCode::F(1))` and feed it to the handler. **Two tests the Rust port can un-skip.**

### Skipped under `CI=1` — 6 more (8 total)

The whole `ProcessManager endurance` describe in `test/unit/infrastructure/process-endurance.test.ts`:

```ts
const isCI = !!process.env['CI'];
// ...
describe('ProcessManager endurance', { timeout: 30_000, skip: isCI }, () => {
```

A `.skip` grep alone does **not** find this one — it uses the options-object form. These 6 tests spawn real `sleep` / `bash` processes.

### No other skips

No `.only`, no `.todo`, no `skipIf` / `runIf` anywhere. The ~15 other `.skip` grep hits across `test/` are all `step.skip!(...)` — the wizard-step predicate function in `wizard-configs-team.test.ts`, `wizard-shell.test.ts`, `wizard-effort.test.ts`, and `agent-shop.test.ts` — unrelated to test skipping.

---

## 6. Parity oracles — tests asserting exact CLI stdout or exact JSON

Ranked by how directly each pins down observable behavior a Rust rewrite must match.

### Tier 1 — exact external command lines and stdin payloads

These define the contract with every AI CLI. If the Rust port matches them, real CLIs behave identically; if not, nothing works. Each asserts `spawn` with exact binary name + argv + `cwd`/`stdio`, and exact stdin bytes.

| File | What it pins |
|---|---|
| `test/unit/infrastructure/codex-adapter.test.ts` | `'codex'`, argv `['exec','--json','--sandbox','danger-full-access','-']`, stdio `['pipe','pipe','pipe']`, `cwd: '/tmp/codex-ws'`; stdin exactly `'codex prompt'`, exactly `'system instructions\n\nuser task'` (systemPrompt joined with `\n\n`), exactly `'just the task'` when systemPrompt absent |
| `test/unit/infrastructure/claude-adapter.test.ts` | Claude argv + stream-json event parse; `execFile` version probe returns `'claude/1.0.0'` |
| `test/unit/infrastructure/pi-adapter.test.ts` | Pi RPC JSONL written to stdin, long-lived process, `killWithGrace(pid, 1000)` on terminal event |
| `test/unit/infrastructure/opencode-adapter.test.ts` | OpenCode argv + event parse |
| `test/unit/infrastructure/cursor-adapter.test.ts` | cursor-agent argv |
| `test/unit/infrastructure/grok-adapter.test.ts` | Grok argv |
| `test/unit/infrastructure/antigravity-adapter.test.ts` | Antigravity argv |
| `test/unit/infrastructure/shell-adapter.test.ts` | Arbitrary-command passthrough; exit-code → event mapping |
| `test/unit/infrastructure/merge-strategy.test.ts` | Exact `git` argv; `git merge --abort` fires **only** on conflict, not on other failures |

### Tier 2 — exact JSON output (strongest oracle in the suite)

`test/unit/cli/structured-logger.test.ts` — 327 lines, 21 cases. Pipes the logger into a `PassThrough`, splits on `\n`, `JSON.parse`s each line, and asserts field-by-field:

```ts
function parseLines(chunks: string[]): Record<string, unknown>[] {
  return chunks.join('').split('\n').filter(Boolean).map((line) => JSON.parse(line));
}

eventBus.emit({ type: 'agent:started', agentId: 'agt_1', taskId: 'tsk_1', runId: 'run_1' });

const lines = parseLines(chunks);
expect(lines).toHaveLength(1);
expect(lines[0]).toMatchObject({
  level: 'info',
  event: 'agent:started',
  agentId: 'agt_1',
  taskId: 'tsk_1',
  runId: 'run_1',
});
expect(lines[0]!['ts']).toBeDefined();
```

It pins the JSONL record shape, the `info`/`warn` level mapping (`agent:completed` with `success: true` → info, `false` → warn), `agent:output` verbose filtering, and idle-tick throttling (every 6th). This is the `orch serve --log-format json` surface that CI consumers depend on. **Port it first and port it literally.**

### Tier 3 — CLI JSON mode via `console.log` capture

Five files spy on `console.log` and assert the captured output parses.

| File | Assertion site |
|---|---|
| `test/unit/cli/commands-config.test.ts` | `:40` — `expect(() => JSON.parse(output)).not.toThrow()` |
| `test/unit/cli/commands-context.test.ts` | `:120` — same |
| `test/unit/cli/commands-logs.test.ts` | `:82` — same |
| `test/unit/cli/commands-logs-since-all.test.ts` | `:168` — same |
| `test/unit/cli/commands-doctor.test.ts` | `:86-90` — filters calls for the first parseable one, then asserts on the parsed object |

The capture mechanism in every case:

```ts
vi.spyOn(console, 'log').mockImplementation(() => {});
// ...
const output = (console.log as ReturnType<typeof vi.fn>).mock.calls[0][0];
expect(() => JSON.parse(output)).not.toThrow();
```

**Caveat:** most only verify *that* the output is JSON, not *what* JSON. The Rust port should strengthen these into exact-shape assertions rather than copying the weak form.

Non-JSON stdout is asserted by substring over joined calls:

```ts
const calls = (console.log as ReturnType<typeof vi.fn>).mock.calls;
const allOutput = calls.map((c: any[]) => String(c[0])).join('\n');
expect(allOutput).toContain('No events');
```

### Tier 4 — exact user-facing strings

| File | Exact asserts | What it pins |
|---|---|---|
| `test/unit/tui/wizard-validate-functions.test.ts` | 21 | Validation error messages for task/agent wizards, incl. self-exclusion on edit |
| `test/unit/cli/editor-agent.test.ts` | 12 | `agentToEditorContent` / `agentFromEditorContent` round-trip text — effectively a serialization format |
| `test/unit/tui/text-cursor.test.ts` | 8 (+61 cases) | Grapheme segmentation and display width. Rust needs `unicode-segmentation` + `unicode-width` to match |
| `test/unit/domain/errors.test.ts` | 7 | Error `message`, `hint` (`'Run: orch init'`, `'Use: orch status'`, `'do something'`), `name`, `exitCode` |
| `test/unit/tui/wizard-agent-hint.test.ts` | 6 | `[adapter] role` hint format + 60-char truncation |
| `test/unit/infrastructure/claude-adapter.test.ts` | 6 | Exact parsed event fields |
| `test/unit/infrastructure/cursor-adapter.test.ts` | 5 | Exact parsed event fields |
| `test/unit/infrastructure/template-engine.test.ts` | 4 (+37 cases) | Rendered prompt strings (`'Hello test-agent'`) and exact shared-context key names (`'goal_abc-progress'`, `'backend-a-status'`, `'some-result'`, `'api-refactor'`, `'backend-dedup-done'`, `'goal_test-progress'`) |
| `test/unit/tui/wizard-shell.test.ts` | 4 | Shell wizard step ids |
| `test/unit/infrastructure/opencode-adapter.test.ts` | 4 | Exact parsed event fields |
| `test/unit/domain/adapter-error-kind.test.ts` | 30 cases | `ERROR_HINTS` table + `classifyAdapterError` mapping + `extractErrorMessage` |
| `test/unit/cli/commands-task-time.test.ts` | — | Mocks `src/cli/output.js` and asserts `printTable` row contents: `rows[0][4] === 'AGE(<ts>)'`, `dim('—')` for missing values. Structural rather than raw-stdout, but pins the TIME column exactly |

### Explicitly NOT an oracle

The Ink `lastFrame()` assertions in ~20 `.tsx` files. Almost all are `toContain('substring')`, and the exact frame layout — box-drawing characters, column positions, the `━━━` rule, the `↑↓  Tab/←→  / cmd  Enter detail  Q quit  ? help` footer — is an artifact of Ink's flexbox layout engine. Ratatui will not reproduce it and should not try.

Port the *intent* ("after pressing `N` on the Agents tab, the agent-name prompt is visible") against `TestBackend`, and treat the TS frames as documentation of intent rather than expected output.

---

## 7. Port-planning summary

| Class | Files | ~Cases | Notes |
|---|---|---|---|
| PORTABLE | 71 | ~1100 | Pure logic → Rust `#[test]`. Includes all 8 adapter tests (clean DI seam via `IProcessManager`) and most of application + cli |
| RENDER-SPECIFIC | 20 | ~500 | 19 `test/unit/tui/*.tsx` + `pi-tui.e2e.test.tsx`. Re-express against ratatui `TestBackend`, driving frames synchronously. `help-overlay.test.tsx` is mixed — roughly 40 of its 89 cases are pure resolver/history logic |
| PROCESS/IO | 36 | ~490 | Real tempdirs (`tempfile::tempdir()`), real locks, mocked-or-real child processes → Rust integration tests |
| NEEDS-REAL-CLI | 0 | 0 | Nothing requires an AI CLI binary |
| HARNESS | 6 | — | Smoke / battle / pty scripts outside vitest's `include`. Port last or drop; the pty test needs the `expect` binary and a built CLI |

### Three decisions worth making early

1. **Inject a clock and an ID generator.** Neither is injected in TS, which is why tests assert ID *prefixes* instead of values and why 7 files reach for `vi.useFakeTimers()` as a workaround. Adding `trait Clock` and `trait IdGen` to the Rust deps struct from day one makes a large fraction of the suite deterministic for free, and removes the need to port the fake-timer scaffolding at all.

2. **`buildDeps` is the shape to copy.** Real services + real event bus + real registry, with only stores and I/O edges faked, and `poll_interval_ms` set high so ticks are driven manually. That design is why the TS orchestrator tests are both fast and meaningful.

3. **The TUI timing flakiness is a TS-only problem.** ~500 render cases currently depend on `sleep(25ms)` landing after a React re-render. Driving `terminal.draw()` synchronously eliminates the entire failure mode — and lets you un-skip the two F1 escape-sequence tests that `ink-testing-library` simply cannot express.
