# ORCH TUI — Exhaustive Feature Inventory (input to the Rust/ratatui parity spec)

> **Status: historical.** Matching the TypeScript original is no longer a goal; this appendix is not updated.
> See [ADR 0002](../decisions/0002-parity-is-no-longer-a-goal.md).

## Current Rust additions

The historical inventory below describes the TypeScript source, not current Rust behavior. The Rust dashboard
adds aligned Agents rows, explicit pane focus, display-row paging/wheel navigation, full-text immutable viewers,
Human/Raw representations, source-byte selection, bounded host/terminal/browser copy and guarded RAM drafts.
Tab moves pane focus; Left/Right switch views. The historical 2,048-character detail cap and absence of clipboard
writing below do not apply to the Rust implementation. See [dashboard usage](../../README.md#reading-navigating-and-editing-in-the-dashboard),
[copy troubleshooting](../RUNBOOK.md#copying-text-from-the-dashboard) and [ADR 0003](../decisions/0003-tui-focus-viewer-and-clipboard-boundaries.md).

Derived from a full read of the Ink/React implementation at `parity/orch-ts`.

**Primary sources**
- `src/tui/App.tsx` (4428 lines) — authoritative UI state machine: key dispatcher, command dispatcher, layout math, logs/activity/detail panels, event formatter
- `src/tui/commandBar.ts` — command registry, tab completion, suggestion grouping, history
- `src/tui/wizardConfigs.ts` — all wizard step definitions and value mappers
- `src/tui/colors.ts` — palettes, semantic color resolvers, truncation helpers, glyph constants
- `src/tui/text-cursor.ts` — immutable grapheme-aware cursor
- `src/tui/hooks/useTextInput.ts` — keyboard editing logic (returns a handler, not a hook)
- `src/tui/onboarding-config.ts`, `src/tui/paletteContext.ts`
- `src/tui/components/`: `Header.tsx`, `TabBar.tsx`, `TaskList.tsx`, `AgentList.tsx`, `GoalList.tsx`, `DetailPanel.tsx`, `CommandBar.tsx`, `FormWizard.tsx`, `HelpOverlay.tsx`, `OnboardingBox.tsx`, `ToastBanner.tsx`, `LogsFilterPicker.tsx`, `LogsTypeFilterPicker.tsx`, `LogsSearchBar.tsx`, `TextInput.tsx`, `Spinner.tsx`, `useAnimTick.ts`, `Footer.tsx`
- `src/cli/commands/tui.ts` — how the TUI is launched and wired
- Supporting domain: `src/domain/agent-shop.ts`, `errors.ts`, `goal.ts`, `global-config.ts`, `model-tiers.ts`, `src/infrastructure/models/model-discovery.ts`, `src/infrastructure/clipboard-service.ts`, `src/cli/output.ts`
- Tests: all 24 files under `test/unit/tui/`

---

## 0. Dead / unwired code — decide deliberately before porting

| Item | Status |
|---|---|
| `components/Footer.tsx` | Never imported by `App.tsx`. Only exercised by a standalone `describe('Footer')` in `app.test.tsx`. The shipped footer is `CommandBar`. |
| `components/TabBar.tsx` (the component) | Unused. Only its exported `TABS` array and `ViewId` type are consumed (by `Header` and `App`). Tab rendering lives in `Header`'s `BrandBar`. |
| `components/LogsSearchBar.tsx` | Fully implemented and unit-tested, but **never mounted**. There is no search mode in the app — no `/` search inside logs, no `n`/`N` next/prev. Its own test file states the App integration is not implemented. |
| `App.tsx` `TaskStatsRibbon`, `AgentStatsRibbon`, `LogsStatsRibbon`, `StatsRibbonBase` | Declared, never rendered. The shipped stats ribbon is `Header`'s `StatsBar`. |
| `App.tsx` `runningCount` | Computed, never used. |
| `W` key | `onboarding-config.ts` advertises `W start orchestrator` on the Tasks and Agents tabs, but **no `W` handler exists anywhere**. Watch mode is reachable only via `/watch`. Either implement `W` or drop the hint. |
| `InputMode` value `'new_task'` | Has a full render path (`InputSectionLabel` + `InputPanel`, prefix `▸ `, label `NEW TASK`), but **nothing ever sets it** — `N` opens the task *wizard*. Unreachable. |
| `Header` prop `sparklineData` | Supported by `StatsBar` (renders a `▁▂▃▄▅▆▇█` sparkline) but `App` never passes it. |

---

## 1. Tabs / views

`TABS` (in `components/TabBar.tsx`) is the single source of truth for both rendering and `Tab`-cycle order:

| Index | Hotkey | `ViewId` | Rendered label | Notes |
|---|---|---|---|---|
| 0 | `G` | `goals` | `GOALS` | |
| 1 | `T` | `tasks` | `TASKS` | default active view at startup |
| 2 | `A` | `agents` | `AGENTS` | |
| 3 | `L` | `logs` | `ACTIONS` | **label differs from the id** |

Tab rendering rules:
- Active tab = inverted pill: `backgroundColor = amber`, `color = solidText`, bold, text `" {key} {LABEL}{badge} "`.
- Inactive tab = hotkey in `ghost` + `" " + label.toLowerCase() + badge` in `dim` (asserted literal: `"T tasks"`).
- Tasks tab badge `" (N)"` where `N = sortedTasks.length`, shown **only when `hiddenTaskCount > 0`**. Tests: 15 tasks → `(15)` present; 8 tasks → `(8)` absent.
- `Tab` and `→` cycle forward with wrap; `←` cycles backward with wrap. Asserted cycle: tasks → agents → logs → goals → tasks.
- All view switching (`G/T/A/L`, `Tab`, `←`, `→`) is **blocked while the detail panel is open**.

---

### 1.1 GOALS view

**Data / sort.** `sortedGoals` = `liveGoals` sorted by `GOAL_STATUS_ORDER = { active: 0, paused: 1, achieved: 2, abandoned: 3 }` (stable within a rank). Goals come from `onRefreshGoals()`.

**Row (`GoalRow`), left → right:**

| Column | Width | Content |
|---|---|---|
| cursor | 2 | `▸` (U+25B8) when selected else space, then a space; amber when selected |
| status chip | 11 | `" {icon} {LABEL} "` — `active` → `◉` (U+25C9) `ACTIVE` green on successBg bold; `paused` → `‖` (U+2016) `PAUSED` dim on neutralBg; `achieved` → `✓` `DONE` amber on accentBg bold; `abandoned` → `✕` `DROP` ghost on neutralBg |
| title | `max(10, width - fixedCols)` | truncated with `…`; bold when selected or status is `active`; white when selected, silver when active |
| progress bar | 14, only when ≥1 linked task | 6-cell bar `█` × filled + `░` × empty where `filled = round(done/total * 6)`, then `" {done}/{total}"`. **Hidden entirely at 0 tasks — no `0/0`** |
| assignee chip | 14 | resolved agent **name** via `agentNameMap` (falls back to the raw id) on successBg/green, truncated `…`; `—` in ghost when unassigned |
| age / time | 7, right-aligned | `✓` amber when achieved, `✕` ghost when abandoned, else age from `created_at` as `"{N}d"` or `"<1d"` in dim |

`fixedCols = 2 + 11 + progressW + 14 + 7`.

**Pseudo-rows.** `+ add goal...` at selection index `goals.length`, rendered only when `onCreateGoal` exists. Marker `  ▸ ` when selected else `    `; amber when selected.

**Detail panel** (`GoalDetailPanel`; section label `GOAL: {title}` plus the resize hint). Built as a virtual line list so scrolling does not rebuild JSX. `col1Width = 24`.

1. row: `status` (colored by `getGoalStatusColor`, bold) + `assignee` (agent name or `—`; green when set, dim when not)
2. row: `id` (dim) + `created` (`created_at.slice(0,10)`)
3. row: `Updated` `{updated_at.slice(0,10)}` — **only when `updated_at !== created_at`**
4. row: `tasks` `{count}` (cyan) + per-status breakdown joined with ` · ` (e.g. `2 done · 1 todo`)
5. blank + description lines (silver), hard-wrapped at `max(20, width - 6)` chars per line; `No description.` when empty
6. blank + `SectionDivider "progress"` + progress-report lines (white) from `onGetGoalProgress(goalId)` — **no line cap, the full report renders**
7. blank + `SectionDivider "tasks (N)"` + one line per linked task: `{status.padEnd(12)}` colored by task status + `title.slice(0, max(10, width - 22))`

Both the description and the progress report pass through `capText(s, MAX_PANEL_TEXT = 10_000)`, which appends `"\n…[truncated]"`.

Scrolling: `↑`/`k` and `↓`/`j` scroll the panel, clamped to `allLines.length - height`. When more lines remain below, the last row becomes a hint `  ↓ {N} more — ↑↓ to scroll` (consuming one row). `Esc` closes the panel **and resets `goalDetailScroll` to 0**.

---

### 1.2 TASKS view

**Data / sort.** `sortedTasks` = `liveTasks` sorted by `STATUS_ORDER = { in_progress: 0, retrying: 1, review: 2, todo: 3, done: 4, failed: 5, cancelled: 6 }`.

When `groupByGoal` is enabled, the list is re-ordered: every task whose `goalId` resolves in `goalMap` is grouped with its goal (groups ordered by first appearance, status sort preserved inside each group), then all ungrouped tasks last.

`visibleTasks` = the first `TASK_LIST_LIMIT = 10` unless `showAllTasks`; `hiddenTaskCount = sortedTasks.length - visibleTasks.length`.

**Row (`TaskRow`):**

| Column | Width | Content |
|---|---|---|
| cursor | 2 | `▸` / space |
| status chip | 10 | `in_progress` → spinner + `RUN` green/successBg bold; `retrying` → spinner + `RETRY` yellow/warnBg; `review` → `◈` `REVIEW` blue/toolBg; `todo` → `○` `TODO` dim/neutralBg; `done` → `✓` `DONE` green/successBg; `failed` → `✕` `FAIL` red/errorBg bold; `cancelled` → `─` `OFF` dim/neutralBg |
| priority pips | 4 | `1` → `!!!` red, `2` → `!!` yellow, `3` → `!` dim, `4` → `·` ghost; bold when priority ≤ 2; an unknown priority falls back to the P4 config |
| title | `max(10, width - fixedCols)` | truncated `…`; bold when selected or running; white when selected, silver when running |
| attachments | inline | ` 📎{n}` dim when `attachments.length > 0`; nothing when the field is absent or empty |
| goal badge | 18, only when `goalId` resolves in `goalMap` | `" ⊕ " + capLine(goal.title, 13) + " "` on accentBg/amberDim. **No badge for an orphaned goalId or a missing map** |
| assignee chip | 14 | resolved name on successBg/green, truncated `…`; `—` ghost when none |
| time | 7, right-aligned | `✓` green when done, `✕` red when failed, `formatDuration(now - updated_at)` cyan when running/retrying, else `—` |

`fixedCols = 2 + 10 + 4 + 14 + goalBadgeW + 7`.

**Goal-grouping section headers** (visual only — **they do not consume selection indices**):
- `GoalSectionRow`: `───` + `" ⊕ {TITLE.toUpperCase()} · {N} task|tasks · {P}% done "` on accentBg/amber bold + trailing light rule. `P = round(doneCount / taskCount * 100)`, integer. Singular form `1 task`.
- `UngroupedSectionRow`: `" ⊕ UNGROUPED · {N} task|tasks "` on neutralBg/dim.
- Headers are inserted at goal-boundary transitions; `prevGoalId` is seeded from `tasks[scrollOffset - 1]` so a mid-group scroll never repeats a header.

**Selection index space.** `tasks[0..n-1]`, then the "show all" row at index `n` (present only when `hiddenTaskCount > 0`), then `+ add task...` at index `n + (hasShowAll ? 1 : 0)` (present only when `onCreateTask`). "Show all" row text: `▼ Show all ({hidden} more) — press S`.

**Detail panel** (`DetailPanel`; section label chip ` DETAIL ` amber on accentBg + the task title + the resize hint):

1. row: `status` (status color) + `assignee` (name or `—`)
2. row: `priority` `P{n}` (red when 1, yellow when 2, bold when ≤2) + `attempts` `{attempts}/{max_attempts}`
3. row: `labels` (purple, comma-joined, `—` when none) + `depends` (dim; dependency ids resolved to **titles** via `taskTitleMap`, `—` when none)
4. attachments (when any): blank + `SectionDivider "attachments (N)"` + one cyan line per filename
5. description (when any): blank + silver lines capped to `max(1, ceil((height - 10) * 0.3))`, each passed through `capLine(line, width - 8)`
6. `No description.` when there is neither a description nor an agent summary
7. result (when `task.proof.agent_summary`): blank + `SectionDivider "result"` + white lines
8. files (when `proof.files_changed.length`): `files {N} changed` + ` · {branch}` when a branch exists
9. activity (when the task has messages): blank + `SectionDivider "activity"` + the last `logMaxLines` messages as `{time} {icon} {text}`, text via `capLine(text, max(10, width - 12))`; per-type colors tool→cyan, file→purple, error→red, lifecycle→green bold, system→dim, otherwise the message's own color

**Row-budget algorithm (port verbatim).**
```
usedRows = 3
if attachments:            usedRows += 2 + attachments.length
if description:            usedRows += 1 + min(descLines, max(1, ceil((height-10)*0.3)))
elif not summary:          usedRows += 2
if summary:                usedRows += 2
if files:                  usedRows += 1
if logs:                   usedRows += 2
remainingRows = max(0, height - usedRows)
if summary and logs:  summaryMaxLines = max(1, floor(remainingRows*0.4))
                      logMaxLines     = max(1, remainingRows - summaryMaxLines)
elif summary:         summaryMaxLines = remainingRows
elif logs:            logMaxLines     = remainingRows
```

Task logs are `messages.filter(m => m.taskId === selectedTask.id)` (memoized to avoid an O(500) filter per render).

---

### 1.3 AGENTS view

**Data / sort.** `sortedAgents` = `liveAgents` sorted by: (1) agents belonging to an **active** team first; (2) team name `localeCompare`; (3) `AGENT_STATUS_ORDER = { running: 0, idle: 1, error: 2, disabled: 3 }`.

**Row (`AgentRow`).** The Rust port lays the row out as an aligned table, like the goals and tasks rows (an L2 presentation divergence from the TS row, which ran the task and error inline after the name and had a team badge). `AgentColumns::for_width` (`crates/orch-tui/src/views/agents.rs`) computes one set of column widths per frame, so every row lines up with the one above it. Widths are in display cells, not `char`s.

| Column | Width | Content |
|---|---|---|
| cursor | 2 | `▸` / space |
| status chip | 10 + gap | `running` → spinner + `ACTIVE` green/successBg bold; `idle` → `○` `IDLE` dim/neutralBg; `error` → `✕` `ERROR` red/errorBg bold; `offline` → `○` `OFFLINE` ghost/neutralBg; `disabled` → `○` `OFF` ghost/neutralBg |
| marks | 2 + gap | `⟳` (LOOP U+27F3, cyan) when `autonomous`, else a space; `★` (U+2605, amber) when team lead, else a space. A fixed slot, so every name starts in the same column |
| name | 10 min, grows | Bold when selected or running; white when selected, green when running, else silver |
| adapter/model | 10–20 | `{adapter}/{config.model}`, or just the adapter when no model is set; dim |
| task | 12 min, elastic | `status === 'error' && last_error` → the mapped hint, or the raw message for an unrecognised kind, capped at 30 (`ROW_ERROR_CAP`), red; else running with a current task → its title (or the task id when the task is not mirrored locally), gray; else `—` ghost |
| caps | 10 | `capabilities.join(',')`, dim; `—` ghost when empty |
| where | 12 | `local` dim; remote → `{alias} online` cyan or `{alias} offline` yellow. The alias is cut to fit, never the state word |
| time / stats | 9, right-aligned | running with a `RunningEntry` → `formatDuration(now - started_at)` cyan; else `total_runs > 0` → `"{completed}/{total} ✓"` colored green (success ≥ 80%), yellow (≥ 50%), red (< 50%); else `—` dim |

Each optional column costs its width plus a one-cell gap. **Narrow terminals drop whole columns** rather than squeezing them. Columns are offered in priority order (runs, task, where, model, caps), and each is taken only if it still fits whole. A narrower, lower-priority column can therefore survive where a wider one doesn't: at 60 columns the model shows and the where column doesn't. The name is never dropped. The slack is then shared out in steps: name to 14, task to 30, model to 16, name to 22, model to 20, and the rest to the task (or to the name when the task column is hidden). So the longest mapped hint (`Failed to start the process.`, 28 cells) reads whole from 104 columns, and any row error, including a raw message capped at 30, from **106 columns** (`ERROR_WHOLE_FROM`). Narrower, it ends in `…`, and the full hint is in the detail panel's ErrorHintPanel.

Widths, pinned by `the_documented_column_widths_hold`:

| Terminal width | name | model | task | caps | where | time |
|---|---|---|---|---|---|---|
| 60 | 10 | 10 | 12 | — | — | 9 |
| 70 | 14 | — | 16 | — | 12 | 9 |
| 80 | 14 | 10 | 15 | — | 12 | 9 |
| 90 | 14 | 10 | 14 | 10 | 12 | 9 |
| 100 | 14 | 10 | 24 | 10 | 12 | 9 |
| 104 | 14 | 10 | 28 | 10 | 12 | 9 |
| 106 | 14 | 10 | 30 | 10 | 12 | 9 |
| 120 | 22 | 16 | 30 | 10 | 12 | 9 |
| 160 | 22 | 20 | 66 | 10 | 12 | 9 |

**Hostile text in a cell.** Control characters, such as a newline in a task title, become spaces. Zero-width graphemes are dropped: ZWSP, bidi marks and overrides, soft hyphens, and lone combining marks. ratatui draws them in zero cells, but `text_display_width` counts them as one, so leaving them in would shift every later column. Dropping them also keeps a bidi override in a peer's agent name off the terminal. An overlong value is cut per grapheme with `…`, and a wide glyph is never split.

There is no team column: the team section headers above each group carry the team.

Asserted (`error-hint-panel.test.tsx`): the row shows the **mapped hint, never the raw error message**, for a recognised `kind`; it falls back to the raw message for an unrecognised kind. The role is no longer a row column — it lives in the detail panel.

**Team section headers** (visual only):
- `TeamSectionRow`: `───` + `" ◈ {TEAM.toUpperCase()} · {N} agent|agents[ · ★ {leadName}] "` on accentBg/amber bold + trailing rule.
- `UnassignedSectionRow`: `" ◇ UNASSIGNED · {N} agent|agents "` on neutralBg/dim; inserted on the transition from team agents to unassigned agents.

**Pseudo-row.** `+ add agent...` at index `agents.length`, only when `onAddAgent`.

**Detail panel** (`AgentDetailPanel`; section label chip ` AGENT ` green on successBg + the agent name + the resize hint). `col1Width = 24`.

1. row: `status` (idle→dim, running→green, error→red, disabled→ghost) + `adapter` (cyan)
2. row (AI agents): `model` (`config.model ?? —`) + `task` (current task title or `—`). **For `shell` agents the two swap**: `task` + `command` (cyan, `config.command ?? —`)
3. row: `runs` `{total_runs} ({tasks_completed}/{tasks_failed})` (completed green, failed red when > 0) + `team` (amber name or `—`)
4. row: `auto` `⟳ ON` (cyan) — only when `autonomous`
5. row: `skills` — only when `config.skills?.length > 0`; cyan, `capLine(skills.join(', '), 500)`. Absent for `config: {}` and for `skills: []`
6. **ErrorHintPanel** — only when `agent.last_error`: a single-bordered red box containing `⚠ Error` (red bold), `hint.message` (white), `hint.fix` (cyan), `Diagnostics: orch doctor` (yellow) when `hint.doctorHint`, the raw `capLine(last_error.message, 120)` (dim) **only** when the hint is missing or the kind is `unknown`, and `{formatDurationSince(timestamp)} ago` (dim)
7. blank separator, then role description lines (silver, `capLine(line, 500)`) limited to `max(1, height - (last_error ? 10 : 4))` lines; `No role description.` when there is no role

**`ERROR_HINTS` (port verbatim, from `src/domain/errors.ts`):**

| kind | message | fix | doctorHint |
|---|---|---|---|
| `ADAPTER_NOT_FOUND` | `CLI is not installed.` | `Install it with: npm i -g @anthropic-ai/claude-code` | yes |
| `AUTH_FAILED` | `API key is invalid.` | `Check it with: claude auth status` | no |
| `TIMEOUT` | `Agent timed out.` | `Increase the timeout with: orch config set agent_timeout <ms>` | no |
| `RATE_LIMIT` | `API rate limit reached.` | `Wait and retry with: orch task retry <id>` | no |
| `PROCESS_CRASH` | `Agent process crashed.` | `Try again with: orch task retry <id>` | no |
| `SPAWN_FAILED` | `Failed to start the process.` | `Check PATH and file permissions` | no |
| `UNKNOWN` | `Unknown error.` | `Run: orch doctor` | yes |

---

### 1.4 LOGS view (labelled `ACTIONS`)

Occupies the **entire** content height: `mainH = contentH`, `feedH = 0`. There is no activity feed and no split; `+`, `-` and `M` are ignored here.

**Line 1 — status bar** (`justifyContent: space-between`):
- Left: the tail chip (`successBg`/green: space + braille `Spinner` + `" LIVE "`) when `logSelectedIndex === -1`, otherwise the browse chip (`warnBg`/amber: `" ↑↓ {idx+1}/{filteredCount} "`). Then `" {filteredCount} events"` in dim. Then `" f:{typeFilterLabel}"` in amber **only when the type filter is not "all"**. Then `" {selected}/{total} agents"` in cyan **only when an agent filter is active**.
- `typeFilterLabel` derivation: size ≥ 8 → `all`; exactly `{output}` → `text`; exactly `{error}` → `errors`; has `tool` and not `output` → `tools`; has `lifecycle` and not `output` → `events`; otherwise `"{n} types"`.
- Right: `a filter  f type  F cycle` (keys amber bold, words dim).
- With no filter active the frame contains neither `agent:` nor `type:` (asserted).

**Line 2 — agent chips.** Every agent's name, truncated with `…` to `agentChipLen = min(max(4, floor((width - 20) / max(1, agents.length)) - 1), 10)`; bold and in the agent's color when active (no filter, or present in the filter set), otherwise `ghost`.

**Message rows.** `viewH = height - 2`. Tail mode renders `filteredMessages.slice(-viewH)`; browse mode renders `filteredMessages.slice(scrollOffset, scrollOffset + viewH)`.

**Filtering.** Drop a message when `logAgentFilter.size > 0 && m.agentId && !logAgentFilter.has(m.agentId)` — **messages without an `agentId` are never dropped by the agent filter**. Keep only when `logTypeFilter.has(m.msgType ?? 'info')`.

**Row layout.** `agentColW = min(10, max(6, ...agentNameLengths))`, `prefixW = 11 + agentColW`.

| Col | Width | Content |
|---|---|---|
| 1 | 1 | `┌` on a session start that also shows an agent badge; `│` on a continuation; else space — colored by the agent color |
| 2 | 1 | `▸` amber when selected, else space |
| 3 | 5 | `relTs.padStart(4)`; green when `"now"`, silver when selected, else ghost |
| 4 | `agentColW + 1` | agent name badge (bold, agent color) on a non-continuation; `·` padded on a continuation; blanks otherwise |
| 5 | 3 | `" {icon} "` — red for `error`, else the agent color, else dim |
| 6 | rest | `capLine(text, textW)` with `textW = max(10, (width - 2) - prefixW - badgeW)`; white + bold when selected, else `getMsgTextColor(type, msg.color)`; also bold for `lifecycle` |
| 7 | optional | **only when `width > 80`** and the message has a `taskId`: ` ` + `" #{taskTitle.slice(0,20)} "` dim on the `void` background |

Row background: `infoBg` when selected, `errorBg` when the type is `error`, otherwise none.

"Session start" = the first visible row, or the `agentId` changed, or the same agent after a gap `> 30_000 ms`. "Continuation" = the previous visible row has the same non-empty `agentId`.

**Empty states.** With `messages.length === 0`, a 6-line ASCII box:
```
    ╭──────────────────────────╮
    │                          │
    │  ◇ Waiting for activity  │
    │  │  Run tasks or start   │
    │  │  the orchestrator     │
    │  ◇                       │
    ╰──────────────────────────╯
```
When filters hide everything: `No events for current filter.`

**Relative time.** `relativeTime(ts, now)`: `< 3s` → `now`; `< 60s` → `{n}s`; `< 1h` → `{n}m`; else `{n}h`. Driven by `useNow(5_000)` (a 5-second clock tick).

**Log detail panel** (`LogDetailPanel`, section label `LOG`): a rounded ASCII box. Header line `{time} │ {agentName} │ {icon} {msgType} │ #{taskTitle.slice(0,30)}`; then the message `text.slice(0, maxW)` bold in the message's color; then a divider; then the body = `detail ?? text`, pretty-printed with `JSON.stringify(parsed, null, 2)` when parseable (adding 3-wide right-padded line numbers), sliced to `max(1, height - 4)` lines, each sliced to `maxW - 4` (JSON) or `maxW` (plain), where `maxW = max(4, width - 6)`. Body line colors: cyan when JSON and the line contains `"`; ghost when JSON and the line matches `/^\s*[}\]]/`; red when the line starts with `error` or `Error`; otherwise silver.

---

## 2. Complete keybinding table

### 2.1 Dispatch order

The handler in `App.tsx` is one ordered `if` chain. Order is load-bearing.

1. **Help overlay open** → close it. If the key was `?`, `Esc` or F1 (`\x1bOP`), swallow it. **Any other key dismisses the overlay AND falls through to execute its normal action.**
2. **`showAgentPicker` open** → `return` (all App keys blocked; only the picker's own handler runs). NOTE: `showTypePicker` is **not** blocked here — a quirk. While the type picker is open, global keys such as `q` still reach App.
3. `Ctrl+S` / `Cmd+S` while `inputMode === 'wizard'` and `wizardConfig.kind === 'agent'` → open the Agent Shop wizard.
4. **`inputMode !== 'none'`** (command / wizard / new_task) → the text-input branch (§2.3).
5. Normal-mode chain (§2.2), in source order.

### 2.2 Normal mode (`inputMode === 'none'`)

| Key | View / mode | Action | Guards |
|---|---|---|---|
| `q` / `Q` | all | `exit()` — **checked first, so `q` can never be typed in normal mode** | none |
| `Esc` | all | close the detail panel (+ reset goal detail scroll); else, in logs with a selection, return to tail (`logSelectedIndex = -1`, `logScrollOffset = 0`); else no-op. **Esc never quits.** | |
| `+` / `=` | not logs | if maximized → exit max and set `splitOffset = -floor(contentH/2)`; else `splitOffset = max(-maxSplitOffset, o - 3)` (grow detail) | |
| `-` | not logs | if maximized → exit max and set `splitOffset = +floor(contentH/2)`; else `splitOffset = min(maxSplitOffset, o + 3)` (grow list) | |
| `M` (uppercase only) | not logs | toggle `isDetailMaximized` | |
| `?` | all | open the help overlay | |
| F1 (`\x1bOP`) | all | open the help overlay | |
| `/` | all | enter command mode, buffer pre-seeded `"/"`, `suggestionIndex = 0` | `!detailOpen` |
| `a` / `A` | logs | open the agent filter picker | `!detailOpen && !showAgentPicker && !showTypePicker` |
| `f` (lowercase) | logs | open the type filter picker | same |
| `F` (uppercase) | logs | cycle the log type filter preset (all→text→tools→errors→events→all) | same |
| `z` / `Z` | all | undo the most recent pending deletion (LIFO) | `pendingDeletions.length > 0` |
| `f` / `F` | tasks, agents, goals | cycle the activity-feed filter preset and persist via `onSaveActivityFilter` | `!detailOpen` |
| `n` / `N` | tasks | open the NEW TASK wizard (also clears `pendingAttachments`) | `!detailOpen && onCreateTask` |
| `n` / `N` | agents | open the NEW AGENT wizard | `!detailOpen && onAddAgent` |
| `n` / `N` | goals | open the `New Goal` wizard | `!detailOpen && onCreateGoal` |
| `e` / `E` | goals | open `Edit Goal: {title}` | `selectedGoal && onUpdateGoal` |
| `d` / `D` | goals | soft-delete the selected goal | `selectedGoal && onDeleteGoal` |
| `c` / `C` | goals | `onUpdateGoalStatus(id, 'achieved', { force: true })` — cancels cancellable pending tasks | status is `active` or `paused` |
| `x` / `X` | goals | `onUpdateGoalStatus(id, 'abandoned')` | status is `active` or `paused` |
| `p` / `P` | goals | toggle `paused` ⇄ `active` | status is `active` or `paused` |
| `a` / `A` | tasks | **approve** the selected task — `onApproveTask` | `selectedTask.status === 'review'` — **this overrides the "A = agents tab" binding** |
| `x` / `X` | tasks | reject the selected task | `selectedTask.status === 'review'` |
| `c` / `C` | tasks | cancel the selected task; refuses with `Cannot cancel — status is {s}` for `done`/`failed`/`cancelled` | `selectedTask && onCancelTask` |
| `e` / `E` | tasks | open the EDIT TASK wizard | `selectedTask && onUpdateTask` |
| `e` / `E` | agents | open the EDIT AGENT wizard | `selectedAgent && onUpdateAgent` |
| `s` / `S` | tasks | toggle "Show All"; resets selection and scroll to 0 | always (tasks) |
| `g` / `G` | tasks | toggle group-by-goal; resets selection and scroll. **Overrides "G = goals tab" on the tasks view** | `!detailOpen` |
| `s` / `S` | agents | force-stop the selected agent; refuses with `Agent "{name}" is not running` when neither `state.running` nor `status === 'running'` | `onForceStopAgent` |
| `d` / `D` | tasks | soft-delete the selected task | `status !== 'in_progress' && onDeleteTask` |
| `d` / `D` | agents | soft-delete the selected agent; if it is running and `onForceStopAgent` is missing → `Cannot delete — agent "{name}" is running. Press S to stop first.`; otherwise schedules with `needsForceStop` | `onDeleteAgent` |
| `u` / `U` | agents | toggle autonomous mode | `selectedAgent && onToggleAutonomous` |
| `g` / `G` | any view except tasks | switch to goals | `!detailOpen` |
| `t` / `T` | all | switch to tasks | `!detailOpen` |
| `a` / `A` | all except logs, and except tasks-with-a-review-task | switch to agents | `!detailOpen` |
| `l` / `L` | all | switch to logs | `!detailOpen` |
| `Tab`, `→` | all | next tab (wrap) | `!detailOpen` |
| `←` | all | previous tab (wrap) | `!detailOpen` |
| `Enter` | tasks | on the "show all" row → toggle show-all; on `+ add task...` → task wizard; otherwise toggle the detail panel | |
| `Enter` | goals | on `+ add goal...` → goal wizard; otherwise toggle detail (and reset detail scroll) | |
| `Enter` | agents | on `+ add agent...` → agent wizard; otherwise toggle detail | |
| `Enter` | logs | toggle the log detail panel | `logSelectedIndex >= 0` |
| `r` / `R` | tasks | run the selected task; refuses with `Cannot run "{title}" — status is {s}` unless the status is in `RUNNABLE = {todo, failed, cancelled}` | `selectedTask && onRunTask` |
| `↑` / `k` | goals + detail open | scroll the goal detail up by 1 | |
| `↑` / `k` | goals / tasks / agents | selection − 1 (clamped at 0); the scroll offset follows when `next < offset` | |
| `↑` / `k` | logs | at tail (`-1`) → jump to the last message and set `logScrollOffset = max(0, last - mainH + 2)`; else index − 1 | |
| `↓` / `j` | goals + detail open | scroll the goal detail down by 1 (clamped inside the panel) | |
| `↓` / `j` | goals | selection + 1, max = `sortedGoals.length + (onCreateGoal ? 1 : 0) - 1`; scroll when `next >= offset + mainH` | |
| `↓` / `j` | tasks | max = `visibleTasks.length + (onCreateTask ? 1 : 0) + (hidden > 0 ? 1 : 0) - 1` | |
| `↓` / `j` | agents | max = `sortedAgents.length + (onAddAgent ? 1 : 0) - 1` | |
| `↓` / `j` | logs | at tail → stay at tail; past the last index → return to tail (offset 0); else index + 1 with the offset following (`next - mainH + 2`) | |

**Collisions to resolve explicitly in the port**
- `a` — approve (tasks with a `review` selection) vs. switch-to-agents vs. agent picker (logs).
- `g` — group-by-goal (tasks) vs. switch-to-goals (everywhere else).
- `s` — show-all (tasks) vs. force-stop (agents).
- `c` — cancel task (tasks) vs. achieve goal (goals).
- `x` — reject task (tasks) vs. abandon goal (goals).
- `f` — activity-filter cycle (tasks/agents/goals) vs. type-picker (logs, lowercase) vs. preset cycle (logs, uppercase).
- `q` precedes everything, so it is never typeable in normal mode.
- `M` is uppercase-only; there is no lowercase `m` binding.

### 2.3 Command mode (`inputMode === 'command'`)

| Key | Action |
|---|---|
| `Esc` | leave command mode, clear the buffer, `cmdHistory.reset()` |
| `Enter` | empty buffer → no-op. Otherwise: if a suggestion is highlighted and its `cmd` (stripped of a trailing ` [...]` args hint) starts with the typed value (or the value is exactly `"/"`), use the suggestion. If that suggestion **has subcommands and contains no space yet**, write `"{cmd} "` into the buffer, reset `suggestionIndex`, and **do not execute**. Otherwise: leave command mode, clear the buffer, `cmdHistory.push(cmd)`, `executeCommand(cmd)` |
| `Tab` | if suggestions are visible → fill the buffer with the highlighted `cmd` (plus a trailing space when it has subs) and reset the index. Otherwise apply the ghost suffix from `resolveCompletion` |
| `↑` | suggestions visible → `suggestionIndex = max(0, i - 1)`; otherwise recall `cmdHistory.prev()` |
| `↓` | suggestions visible → `suggestionIndex = min(len - 1, i + 1)`; otherwise `cmdHistory.next() ?? ''` |
| anything else | delegated to `useTextInput.handleInput`; when consumed, `suggestionIndex` resets to 0 |

### 2.4 Text editing (`useTextInput`) — command bar and single-line wizard text steps

| Key | Action |
|---|---|
| `Ctrl+A` | move to start of line |
| `Ctrl+E` | move to end of line |
| `Ctrl+K` | kill to end (fills the kill ring, immediate undo snapshot) |
| `Ctrl+U` | kill to start (kill ring + snapshot) |
| `Ctrl+W` | kill word backward (kill ring + snapshot) |
| `Ctrl+Y` | yank the kill ring at the cursor |
| `Ctrl+Z` | undo — pops one snapshot matching the current text, then pops and applies the next |
| `Ctrl+B` / `Ctrl+F` | move left / right |
| `Ctrl+D` | delete forward |
| `Ctrl+H` | backspace |
| `Cmd+Z` (meta) | undo — pops **all** snapshots matching the current text, then applies the next |
| `Cmd+A` (meta) | move to start (terminal convention, not select-all) |
| `Cmd`/`Option+Backspace` | kill to start |
| `Option+←` / `Meta+B` | word backward |
| `Option+→` / `Meta+F` | word forward |
| `Home` / `End` | start / end |
| `←` / `→` | move one grapheme |
| `Backspace` **or** `Delete` | delete one grapheme **backward** (Ink 6.8 maps the physical `\x7F` to `key.delete`; both must delete backward — forward delete is `Ctrl+D` only) |
| any printable input | insert at the cursor, NFC-normalised, with a debounced undo snapshot |

Not consumed here (left to the parent): `Enter`, `Escape`, `Tab`, `↑`, `↓`. Undo snapshot debounce **500 ms**; max undo depth **50**. Empty-input events from modifier keys / IME switching are explicitly ignored by a guard at the top.

### 2.5 LogsFilterPicker (agent filter)

| Key | Action |
|---|---|
| `↑` | cursor − 1, **wrapping to the last agent** |
| `↓` | cursor + 1, **wrapping to the first agent** |
| `Space` | toggle the agent under the cursor in the draft set. From the implicit-all state (empty set) this produces a **single explicit selection**, not all-minus-one |
| `a` / `A` | reset the draft to the empty set = "all agents / no filter" |
| `Enter` | `onConfirm(new Set(draft))` — **an empty set is a legal confirm value meaning "no filter"** |
| `Esc` | `onCancel()` |

### 2.6 LogsTypeFilterPicker

| Key | Action |
|---|---|
| `↑` / `↓` | cursor ∓ 1 with wrap over the 8 types |
| `Space` | toggle the type under the cursor |
| `a` / `A` | if all 8 are selected → deselect all; otherwise select all 8 |
| `1` | preset **all** (8 types) |
| `2` | preset **text** (`output`) |
| `3` | preset **tools** (`tool`, `result`, `file`) |
| `4` | preset **errors** (`error`) |
| `5` | preset **events** (`lifecycle`, `system`) |
| `Enter` | `onConfirm` — **an empty draft is normalised to all 8 types** (differs from the agent picker) |
| `Esc` | `onCancel()` |

### 2.7 Raw byte sequences the tests depend on

`\r` Enter · `\t` Tab · `\x1B` Esc · `\x7F` Backspace · `\x1B[A/B/C/D` arrows · `\x1bOP` F1 · `\x01` Ctrl+A · `\x05` Ctrl+E · `\x0B` Ctrl+K · `\x15` Ctrl+U · `\x17` Ctrl+W · `\x19` Ctrl+Y · `\x1A` Ctrl+Z · `\x1b[13;2u` Shift+Enter (kitty CSI-u) · `\x1b[13;5u` Ctrl+Enter · `\x1b[105;5u` Ctrl+I · `\x1b[118;5u` Ctrl+V.

The host enables the kitty keyboard protocol:
```js
render(..., { incrementalRendering: true,
              kittyKeyboard: { mode: 'auto', flags: ['disambiguateEscapeCodes'] } })
```
**Ctrl+Enter vs plain Enter disambiguation requires that protocol** — hence the Tab fallback for textarea confirm. In Rust: crossterm `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`, keeping the Tab fallback.

---

## 3. Modes / overlays

### 3.1 Command bar
- **Enter:** `/` from any view while the detail panel is closed; the buffer is pre-seeded with `"/"`.
- **Exit:** `Esc`, or `Enter` which executes.
- **Renders:** a bottom line `/ {value}{ghostCompletion}█  {hint}`; the hint is `  ↑↓ select  Tab fill  Esc ✕` when suggestions are visible, otherwise `  Enter exec  ↑↓ history  Tab complete  Esc ✕`. The value truncates **from the left** with a leading `…` when longer than `maxLen = max(4, width - 6 - hint.length - (completion?.length ?? 0) - 1)`. Above the bar: a `COMMANDS` section label plus the `SuggestionsPanel`.
- **r-orch additions** (PARITY §12.8 row (l)), all three affecting every command rather than only `/msg`:
  - The `█` is the **real cursor**, drawn at `cursor().pos()`, and the left-truncation window is anchored on the cursor rather than on the end. With the cursor at the end — the common case — the line is unchanged, and the window keeps master's `maxLen` columns including the leading `…`. The window is computed by walking out from the cursor until the budget is spent, so its cost is the columns drawn, not the length of the value. With the cursor **mid-value** it holds a third of the budget back for the text after the cursor, and the ghost completion — which belongs at the end of the value — is dropped while that text is clipped, rather than drawn against a truncation.
  - A kept newline in the value (only reachable by paste) draws as a dim `⏎`. Display only: the value and the body sent keep the `\n`. One paste inserts at most **64 KiB** (`keys::MAX_PASTE_BYTES`), cut on a character boundary with `Pasted text cut at 64 KiB — paste the rest separately` in the feed **and drawn above the bar** (the feed is not rendered in command mode), cleared by the next keystroke. A paste larger than roughly 4 KB may also not arrive at all (a bracketed paste is all-or-nothing); nothing is inserted and no keystroke leaks when it does not.
  - For `/msg` with a complete recipient token, a **recipient chip** `→ {name} │ ` is pinned between the `/ ` prefix and the value, measured in display columns, and the value's budget is reduced by its width so the body truncates and the recipient never does. States: `→ {name}` (green), `→ {name} (disabled)` / `→ all agents ({n})` (amber), `⚠ ambiguous: {n} agents` / `⚠ no match — did you mean "{name}"?` / `⚠ no match: "{text}"` (red). While a chip is shown the bar takes the widest hint that still leaves the value window 28 columns — the full hint, else `  Esc ✕`, else nothing — because the full hint at 80 columns left the body 8 columns and dropping it outright took the only *clickable* exit from the bar with it; without a chip the hint goes only below 6, so no other verb's bar moves. Below that the chip is shortened; it is never dropped while a recipient is resolved.
- **Navigate-mode bar** (the default footer) renders, conditionally: `↑↓`, `Tab`/`←→`, `/ cmd`, `N new` (canNew), `R run` (canRun), `C cancel` (amber), `A approve` (green), `X reject` (red), `E edit` (cyan), `S stop` (red), `P pause`/`P resume` (amber), `U auto on`/`U auto off` (cyan), `S show all`/`S collapse`, `D delete` (only when `canDelete && !canApprove`), `Z undo` (yellow), `Esc close` when a detail panel is open, `Enter detail` when no detail panel and the view is tasks/agents/goals, `Q quit`, `? help`. On the right: `{itemCount} {itemLabel}` when the count is > 0 (labels `goals` / `tasks` / `agents` / `events`). `? help` is amber while `onboardingCompleted === false`, gray otherwise.

### 3.2 Help overlay
- **Enter:** `?` or F1 (`\x1bOP`).
- **Exit:** `?`, `Esc`, F1 (all swallowed), or **any other key — which also executes its normal action**.
- **Renders:** replaces both the content area and the bottom panel. Rendered with `width = W, height = H - 7`. A centred amber rounded box: inner width 80 (`COL_W*3 + 3 + 3 + 2`), outer 82; horizontal centring via `paddingX = max(0, floor((W - 82)/2))`; vertical via `marginTop = max(0, floor(((H-7) - (maxRows + 10)) / 2))` with `maxRows = 10`. Layout: top border `╭─…╮`, blank, centred title `KEYBOARD SHORTCUTS`, blank, three centred dividers `NAVIGATION` / `ACTIONS` / `COMMANDS`, blank, 10 shortcut rows (key padded to 10 columns, label padded to 12, column width 22, `│` separators), blank, centred footer `Press any key to dismiss`, blank, bottom border.
- **Content:**
  - NAVIGATION: `↑↓/j/k Navigate`, `Tab/←→ Switch tabs`, `Enter Detail view`, `G Goals tab`, `T Tasks tab`, `A Agents tab`, `L Logs tab`, plus the two r-orch additions `F2 Mouse on/off` and `m Message agent` — nine rows against `maxRows = 10`, which is why they go in this column rather than `ACTIONS`
  - ACTIONS: `N New item`, `E Edit`, `D Delete`, `R Run task`, `S Show / Stop`, `C Cancel`, `A Approve`, `X Reject`, `U Autonomous`, `Z Undo delete`
  - COMMANDS: `/ Command mode`, `/task add Create task`, `/msg Message agent` (r-orch addition), `/run Execute`, `/watch Auto-dispatch`, `/config Settings`, `/help Help`, `/quit Exit`

### 3.3 Logs agent filter picker
- **Enter:** `a` / `A` in the logs view (detail closed, no other picker open).
- **Exit:** `Enter` (confirm) or `Esc` (cancel).
- While open, `LogsContent`'s height shrinks to `max(3, mainH - 16)` and **all App keys are blocked**.
- **Renders:** a rounded amber-bordered box: header ` ◈ Agent Filter` + `· {totalSelected}/{total} selected`; a 36-char light rule; agent rows ` ▸ ` / `   ` + `[✓]` (green) / `[ ]` (ghost) + the name (cursor → agent color, checked → silver, else dim) + ` ·{msgCount}` when > 0; a scroll indicator `   ↑ ↓ {cursor+1}/{total}` when the list overflows; a rule; footer ` Space toggle · a all · Enter confirm · Esc cancel`.
- Sizing: `maxHeight = min(mainH - 4, 18)`; visible rows `listH = max(3, maxHeight - 5)`; the scroll offset centres the cursor (`min(maxOff, max(0, cursor - floor(listH/2)))`).
- An empty selection renders as all-checked and `{total}/{total} selected`.

### 3.4 Logs type filter picker
- **Enter:** `f` (lowercase) in the logs view. **Exit:** `Enter` / `Esc`.
- Fixed 8 rows in order `system, lifecycle, output, tool, result, error, file, info`.
- Labels: `System`, `Lifecycle`, `Output`, `Tool calls`, `Results`, `Errors`, `Files`, `Info`.
- Icons: `◆ ▶ │ ⚙ ← ✕ ✎ │`. Cursor-row colors: purple, cyan, white, blue, green, red, purple, silver.
- Header ` ◈ Type Filter · {n}/8 selected`; a preset line ` 1=all · 2=text · 3=tools · 4=errors · 5=events`; footer identical to the agent picker. Counts render as ` ·{n}`.

### 3.5 Search bar (not wired)
`LogsSearchBar` exists and is tested but is never mounted. If ported: props `query`, `matchCount`, `currentMatch` (**0-based internally, displayed 1-based**), `isInvalid`, `width`. Renders `/{query}▏ {counter} n/N next/prev · Esc close`; the counter is `Invalid regex` / `{cur+1}/{count}` / `No matches` / empty when the query is empty; the cursor glyph is `▏` (U+258F), **not** `█`; long queries truncate from the left.

### 3.6 Onboarding
A 5-state machine plus three box components and a separate per-tab empty-state box — see §8.3.

### 3.7 Toast banner
Rendered between the content spacer and the undo banner. **No key dismisses a toast** — auto-dismiss only. See §8.4.

### 3.8 Form wizard
Occupies the bottom panel (`feedH` rows), replacing the detail panel / activity feed. See §5.

### 3.9 Confirmation prompts
**There are none.** Deletions are soft-deleted with a 5-second undo window (§8.1). Destructive goal transitions (`C` = achieve with force, `X` = abandon) fire immediately with no prompt.

### 3.10 Undo banner
Rendered above the command bar whenever `pendingDeletions.length > 0`; one row per pending deletion:
`✗ ` + **`{Task|Agent|Goal}`** + ` "{name}" — ` + `{N}s` (amber bold) + ` │ ` + `Z` (gray bold) + ` undo` — all on a yellow base. The name is truncated to `nameMax = max(10, (width - 4) - label.length - 30)`. The component owns a 1-second interval purely to re-render the countdown.

---

## 4. Command bar — complete command surface

### 4.1 Registry (`COMMAND_REGISTRY`; insertion order determines suggestion order)

| Verb | Subcommands | Args | Help text | Category |
|---|---|---|---|---|
| `task` | `add, list, show, cancel, retry, assign, approve, reject, delete` | — | `Manage tasks` | MANAGEMENT |
| `agent` | `add, list, disable, enable, delete, autonomous, shop` | — | `Manage agents` | MANAGEMENT |
| `team` | `create, list, join, leave, disband, set-lead` | — | `Manage teams` | MANAGEMENT |
| `goal` | `add, list, show, status, delete` | — | `Manage goals` | MANAGEMENT |
| `run` | — | `[id]` | `Run task (or selected)` | MONITORING |
| `run-all` | — | — | `Run all todo tasks` | MONITORING |
| `watch` | — | — | `Start watch mode (auto-dispatch)` | MONITORING |
| `pause` | — | — | `Pause watch mode` | MONITORING |
| `status` | — | — | `Show orchestrator status` | MONITORING |
| `config` | `palette, activity-filter, max-concurrent, notifications-toast, notifications-bell` | — | `TUI settings` | SETTINGS |
| `help` | — | — | `List all commands` | SETTINGS |
| `mouse` | `on, off, status` | — | `Mouse capture on/off` | SETTINGS |
| `msg` | — | `[agent] [message]` | `Message an agent` | MANAGEMENT |
| `quit` | — | — | `Exit the TUI` | SETTINGS |

`mouse` and `msg` are r-orch additions (PARITY §12.8 rows (j) and (l)); `msg` is registered **after**
`mouse` so `resolveCompletion('/m')` still yields `ouse`.

### 4.2 `resolveCompletion(input) → string | null`
Returns only the ghost **suffix**. Must start with `/`. No space yet → the first registry key with that prefix excluding an exact match, minus the prefix. With a space → the first matching subcommand minus the sub-prefix.

Asserted: `'task'`→`null`, `'/'`→`null`, `'/ta'`→`'sk'`, `'/task'`→`null`, `'/task c'`→`'ancel'`, `'/task cancel'`→`null`, `'/xyz'`→`null`, `'/config pal'`→`'ette'`.

### 4.3 `resolveSuggestions(input) → Suggestion[]`
- Non-`/` input (including `''`) → `[]`.
- Exactly `'/'` → a category-grouped list. For each of MANAGEMENT, MONITORING, SETTINGS (in that order) a separator entry `{ cmd: '', desc: '── {CATEGORY} ──' }` followed by that category's verbs. Exactly 3 separators. Verb entry: `{ cmd: '/{verb}{ ' '+args if any}', desc: help, subs: sub.join(' · ') }`.
- A prefix (`'/ta'`) → matching verbs only, **no separators**. If the prefix exactly equals a verb that has subs, all its subcommands are appended as `{ cmd: '/{verb} {sub}', desc: '{help}: {sub}' }`.
- Inside a command (`'/task '`, `'/task a'`) → only that verb's subcommands, filtered by the sub-prefix.

### 4.4 `SuggestionsPanel` rendering
Visible rows = `min(suggestions.length, feedH)`; scrolls so the selected index stays visible (`scrollStart = selectedIndex - maxVisible + 1`). Each row: `  ▸ ` (amber) or `    ` (ghost); the command padded to `cmdPad = min(32, max(14, ...all cmd lengths + 1))` (white bold when selected, else silver); the description truncated to `max(4, width - cmdPad - subsText.length - 9)` with `…` (dim); then `  {subs}` (ghost).

**Argument rows (r-orch, `/msg` only).** After `/msg ` the panel lists agents instead of `&'static`
subcommands, while the input is still inside or at the end of the recipient token — it closes once the body
starts, so Enter cannot be hijacked mid-body. Groups, concatenated, each in `sorted_agents()` order: name
prefixes, id prefixes, name substrings, then the broadcast row **last**. Row text is `/msg {unescaped name}`
capped to 32 **display columns**, with description `{adapter} · {status} · {id}`; the broadcast row is
`/msg all` with `broadcast · {n} enabled agent(s)`. Two rows that would display the same name are both
prefixed `!`. Capped at 40 agent rows plus an inert `… {n} more — keep typing to filter`. Accepting a row
**fills** the bar with the *quoted* form computed from the agent record (never re-derived from the row text,
which the legacy ` [...]` trim would mangle); its click identity is the agent id. Separator rows are inert
for `Tab` and `Enter` — previously `Tab` on one cleared the input.

**The retained refusal (r-orch, `/msg` only).** When the suggestion list is empty and a `/msg` refusal (or the
broadcast confirm) is keeping the bar open, those rows carry that refusal's lines instead,
**bottom-aligned** so they sit directly above the command they are about, `⚠`-marked on the first line and
capped in display columns. Necessary because the activity feed — where the same lines also land — is **not
rendered while the bar is open**, so the one thing the user needs to read would otherwise be the one thing
off screen. Cleared by the next keystroke, a send, `Esc` or leaving command mode.

### 4.5 `CommandHistory`
`push` ignores empty strings, dedupes **consecutive** duplicates, caps at 100 entries (FIFO `shift`), and resets the cursor to the end. `prev()` returns `null` on an empty history, otherwise decrements (clamped at 0) and returns that entry. `next()` increments while below the last index; at or past the end it resets the cursor to `length` and returns `null` (meaning "clear the input"). `reset()` moves the cursor to the end.

### 4.6 Command semantics (`executeCommand`)
The leading `/` is stripped, the verb lowercased, arguments split on whitespace. When no id is given, commands act on the **current view's selection**.

**Legacy bare shortcuts** (no slash group): `cancel`, `retry`, `assign <agent>`, `disable`, `enable`. Messages: `No task selected` / `No agent selected`; `Usage: assign <agent>`; `Cancelling "{t}"...` → `✓ Cancelled "{t}"`; `Retrying "{t}"...` → `✓ Retried "{t}"`; `Assigning "{t}" to {a}...` → `✓ Assigned "{t}" to {a}`; failures `Failed: {err}`.

**`/task`**
- `add [title...]` — no title → open the task wizard; with a title → `onCreateTask(title)` → `✓ Created "{title}" ({id})`. Missing callback → `Create not available`.
- `list` → one cyan line per task `  {id}  {status.padEnd(11)} {title}`; `No tasks` when empty.
- `show [id]` → `{id}  {status}  P{priority}  "{title}"` plus optional `  agent: {assignee}` and `  {description.slice(0,100)}`.
- `cancel [id]`, `retry [id]` — resolve the id, else the selection.
- `assign [id] <agent>` — if `parts[2]` resolves to a known task id, the agent is `parts[3]`, otherwise `parts[2]`. Missing agent → `Usage: /task assign [id] <agent>`.
- `approve [id]` — requires `status === 'review'`, else `Cannot approve — status is {s}`.
- `reject [id] [feedback...]` — requires `review`, else `Cannot reject — status is {s}`; feedback = the remaining words; `✓ Rejected "{t}" → todo`.
- `delete [id]` — refuses `Cannot delete — task is running` for `in_progress`; otherwise schedules a soft deletion.
- Unknown sub → `Usage: /task add|list|show|cancel|retry|assign|approve|reject|delete`.

**`/agent`**
- `add` with no name → the agent wizard. `add <name> [adapter] [command words...]`: the adapter defaults to `defaultAdapter`; if the adapter is `shell` **and** no command words follow, open the wizard pre-filled `{name, adapter:'shell'}`; otherwise `onAddAgent(name, adapter, adapter === 'shell' ? { command, approval_policy: 'auto' } : undefined)`. Success: `✓ Created agent "{name}" ({id}, {adapter})` plus ` · {capLine(command, 60)}` for shell agents with a command. Missing callback → `Agent creation not available`.
- `list` → `  {id}  {status.padEnd(8)} {name} ({adapter})`; `No agents`.
- `disable` / `enable` `[idOrName]` — **resolves by id or name**, else the selection.
- `delete` (alias `remove`) `[idOrName]` — refuses `Cannot delete — agent is running` for `status === 'running'`; otherwise soft-delete. Missing callback → `Agent deletion not available`.
- `autonomous` (alias `auto`) `[idOrName]` — toggles; messages `⟳ {name} autonomous ON` / `OFF` (cyan). Missing callback → `Autonomous toggle not available`.
- `shop` → the Agent Shop wizard; refused when `defaultAdapter === 'shell'` with `Agent Shop requires an AI provider. Press N to create a Shell agent.`
- Unknown sub → `Usage: /agent add|list|disable|enable|delete|autonomous|shop`.

**`/team`**
- `create` (alias `add`) → the team wizard.
- `list` → `  {id}  {status.padEnd(8)} {name} ({n} members)`; `No teams`.
- `join <teamId> [agentId]` — the agent id defaults to the selected agent; `Usage: /team join <teamId> [agentId]`.
- `leave <teamId> [agentId]` — same shape; `Usage: /team leave <teamId> [agentId]`.
- `disband <teamId>` — `Usage: /team disband <teamId>`.
- `set-lead <teamId> <agentId>` — `Usage: /team set-lead <teamId> <agentId>`.
- Unknown sub → `Usage: /team create|list|join|leave|disband|set-lead`. Every mutation refreshes with `{ includeTeams: true }`.

**`/goal`**
- `add` (alias `create`) `[title...]` — no title → the goal wizard (`title: 'New Goal'`); otherwise `onCreateGoal({ title })`.
- `list` → `  {id}  {status.padEnd(8)} {title}`; `No goals`.
- `show [id]` → `{id}  {status}  "{title}"` plus `  {description.slice(0,100)}`.
- `status [id] <active|paused|achieved|abandoned>` — validated against `GOAL_STATUSES`; otherwise `Usage: /goal status [id] <active|paused|achieved|abandoned>`.
- `delete [id]` → soft delete.
- Unknown sub → `Usage: /goal add|list|show|status|delete`.

**Other**
- `run [id]` — id defaults to the selection; blocked unless the status is in `RUNNABLE` (`Cannot run — status is {s}`); `Running {id}...` → `✓ Dispatched {id}`.
- `run-all` → `Running all todo tasks...` → `✓ Dispatched all todo tasks`.
- `watch` → refuses `Watch mode already active`; otherwise `✓ Watch mode started` and `watchActive = true`.
- `pause` → refuses `Watch mode not active`; otherwise `✓ Watch mode paused`.
- `config` with no sub → `Choose a setting: palette, activity-filter, max-concurrent, notifications-toast, notifications-bell`.
- `config palette <name>` → **direct apply, no wizard**. Unknown → `Unknown palette: {x}. Available: amber, ocean, forest, violet, light`; valid → `changePalette(name)` + `Palette: {name}`.
- `config <validSetting>` → the 1-step config wizard, title `SETTINGS — {SETTING WITH HYPHENS REPLACED BY SPACES, UPPERCASED}` (e.g. `SETTINGS — PALETTE`, `SETTINGS — MAX CONCURRENT`).
- `config <invalid>` → `Unknown setting: {x}. Available: ...`.
- `status` → `{mode}  {running} running  {taskCount} tasks  {agentCount} agents` (mode ∈ observing / watching / idle; no pluralisation).
- `help` → one silver line per registry entry: `  /{verb}{ subs joined by '|' | ' '+args}  — {help}`.
- `quit` → `exit()`.
- Unknown verb → `Unknown: {cmd}. Type /help for commands`.

**`/msg` (r-orch addition, PARITY §12.8 row (l))** — `/msg <agent|all> <message>`. The **only** verb that
does not use `split_whitespace`: it takes the raw tail, because an agent name may contain spaces.
- **Grammar.** One recipient token — bare, `"…"` (with `\"` and `\\`) or `'…'` — then exactly the whitespace
  run that follows it, then the body **verbatim** (its own quotes, `#`, backslashes and whitespace runs are
  untouched). Autocomplete and `m` insert the quoted form; a name is written bare only when it has no
  whitespace, no `"`, no `'`, no `\`, no leading `@` and is not `all`.
- **Resolution** commits only on an exact id, an NFC-exact name, or a *unique* case-insensitive name, and
  never falls back to the selection. Ambiguity lists ids and stops. A **prefix never commits**: it is a
  did-you-mean refusal. An unquoted exact match shadowed by a longer name is refused with both quoted
  readings — both when the body carries the whole of that longer name (`Code` and `Code Reviewer`,
  `/msg Code Reviewer please look`) and when it only *starts* it on whole-word boundaries
  (`/msg Code Rev hi`, which otherwise sent "Rev hi" to `Code`); the second reading's body is the body
  with those words removed. One leading `@` is stripped and retried only if nothing matched as typed.
- **Autocomplete order** is: the NFC-**exact** name, then name prefixes, then id prefixes, then name
  substrings, then the broadcast row. The exact group exists because accepting a row *fills* the bar, so a
  row sorting above the name the user typed would rewrite the recipient (`alice` and `Alice`: `/msg Alice`
  + Tab filled `/msg alice `).
- **Broadcast** is the exact unquoted lowercase `all` only — `ALL`, `All`, `@all` and `"all"` are read as
  **names**, so they reach an agent genuinely called that (`@all` literally, or case-insensitively unique)
  and are otherwise refused by name resolution with `No agent matches "ALL"`. It needs a confirming second
  Enter against byte-identical input **in the same edit state**: the arm is keyed to the bar's
  `TextInput::generation` as well as its text, and is dropped by any non-Enter key, any paste, and every
  exit from command mode (`Esc`, the `✕` zone, re-entering with `/`, running any other verb) — so a
  retype, an `↑` recall or a mouse cancel all have to arm again. A broadcast with nobody to send to is
  refused outright with `No enabled agents to message`.
- **Refusals**: `Usage: /msg <agent|all> <message>`, `Missing message for "{name}"`, `Unclosed quote in
  recipient — write /msg "Code Reviewer" <message>`, `Expected a space after the quoted name`,
  `Recipient is empty`, `Ambiguous recipient "{text}" — {n} agents match: {id} {name}, … (use the id)`,
  `No agent named "{text}" — did you mean "{name}"? …`, `No agent named "{text}" — {n} agents start with
  it: …`, `No agent matches "{text}" — /agent list to see names`, `Messaging not available`. Every refusal
  **keeps the bar open with the text exactly as typed** — trailing space included, which is what keeps the
  suggestion panel closed over a `Missing message` refusal — and so do the broadcast arm and a failed
  `send_message`, which no other verb does. The retained lines own the suggestion rows while they are
  shown: the next keystroke clears them and the list comes back. `/msg` + Enter runs the verb instead of filling from its own
  row — the fill would add only the trailing space — so the Usage line is shown instead of nothing;
  `Tab` still fills, and a recipient row always fills, a finished name included.
- **Success**: `Sending to {name}...` then `✓ Queued for {name} ({msgId}) — delivered on its next run`, or
  `✓ Queued for {n} agents — delivered on their next run`; ` — nothing is dispatching right now; /watch to
  start` is appended when neither watch nor observer mode is active. Advisories: `{name} is disabled — the
  message waits in its inbox`, `Body truncated to 4000 characters` (the engine's cut, reported without
  pre-truncating). No refresh — nothing in `AppState` derives from messages.

**`m` on the Agents tab (r-orch addition)** — with a selection and `can_send_message()`, opens the command
bar prefilled `/msg "{name}" `. Lowercase only; `M` keeps meaning *maximize*. Advertised as `m msg` in the
Agents footer, clickable like its neighbours, and as `m Message agent` in the help overlay's NAVIGATION
column — the footer alone is cut off below ~75 columns.

---

## 5. Wizards

### 5.1 `FormWizard` engine (`components/FormWizard.tsx`)

**Step types:** `text`, `textarea`, `select`, `multiselect`.

**`WizardStep` fields:** `id`, `label`, `type`, `options` / `getOptions(values)`, `placeholder`, `description`, `required`, `defaultValue` / `getDefaultValue(values)`, `skip(values)`, `suggestions` / `getSuggestions(values)`, `suggestionMode` (`'callback' | 'value'`), `suggestionsLabel`, `validate(value) → string | null`.

**Step resolution.** Active steps = `steps.filter(s => !s.skip?.(values))`, recomputed from the current values. Advancing resolves the next non-skipped step from the **full** steps array using the newly merged values (not the stale memoized active list), then maps back to an active index for rendering. Running past the end calls `onComplete(values)`. `resolveDefaultValue(step, values) = step.getDefaultValue?.(values) ?? step.defaultValue ?? ''`.

**Rendering, top to bottom.**
1. `{TITLE}` (amber bold) + ` ── ` + `step {i+1}/{total}` (dim)
2. Progress dots: `●` amber for the current step, `✓` green for completed, `○` ghost for upcoming
3. Step label (white bold) + ` *` (red) when required, else ` (optional, {Enter/Tab | ⌘+Enter/Tab} to skip)` (dim)
4. `description` line (dim) when present
5. The field
6. Inline error (red, `dimColor`) when `dirty` **and** the stored validated value equals the current value; then `Fix the error above` (red) when `enterBlockHint`
7. Hint bar, plus `footerExtra` in amber

**Field renderers.**
- `text` → `TextInput` with prefix `"> "`, width `maxW - 4` where `maxW = max(20, width - 6)`, the placeholder, a red round border on error, an amber block cursor `█`, and a display-width-aware sliding viewport that keeps the cursor visible (CJK/emoji count as 2 cells).
- `textarea` → a line-numbered editor. `taLineNumWidth = len(String(lineCount))`, `taContentWidth = max(1, width - lnw - 4)`. Logical lines are hard-wrapped into visual segments of `taContentWidth`. Each row renders ` {lineNum} ` (dim; blank on wrap continuations) + `│ ` (ghost) + the text, with the cursor row split around an amber `█`. Visible height `max(3, height - 6)`, scrolled so the cursor's visual row is visible. A red round border on error. The placeholder renders only when the content is exactly one empty line.
- `select` → rows `  ▸ ` (amber) when selected, else `  {n} ` (1-based index, right-padded to 2 when there are ≥10 options); label white bold when selected else silver; ` ─ {hint}` dim with newlines flattened to spaces. Visible height `optionsH = max(2, height - 4)`, scrolling when the index exceeds it.
- `multiselect` → rows `  ▸ ` / `    ` + `[✓]` green / `[ ]` dim + label + hint; a trailing `  └ {n} selected` when any are checked.
- `text` + suggestions → a divider `  ─── {suggestionsLabel ?? 'or browse templates'} ───…` then suggestion rows `  ▸ ` / `    ` + label + ` ─ {hint}`. Visible height `max(2, height - 6)`, scrolled while browsing.

**Hint bar by step type.**
- select: `↑↓ select  Enter/Tab confirm`
- multiselect: `↑↓ move  Space toggle  Enter/Tab confirm`
- textarea: `Enter newline  {⌘|Ctrl}+Enter/Tab confirm  ←↑→↓ navigate`
- text while browsing suggestions: `↑↓ browse  Enter select  Tab confirm  ↑ back to input`
- text with suggestions: `←→ move  Enter/Tab confirm  ↓ browse templates`
- plain text: `←→ move  Enter/Tab confirm`
- plus `  {⌘|Ctrl}+V paste image` when `onPasteImage` is set and the step is text/textarea
- plus `  Esc back` (step > 0) or `  Esc cancel` (step 0)
- `CMD_KEY` is `⌘` on darwin, `Ctrl` elsewhere

**Keys.**
- `Esc`: step 0 → `onCancel()`; otherwise go back one step, restoring the stored value (marking `dirty` when one exists) and clearing validation state.
  **IMPORTANT INTEGRATION QUIRK:** in the real App, the App-level `useInput` also receives `Esc` while `inputMode === 'wizard'` and sets `inputMode = 'none'`, which unmounts the wizard entirely. So **in the shipped app, Esc always closes the whole wizard regardless of step**; the per-step "Esc = back" behaviour only manifests when `FormWizard` is used standalone (as the unit tests do). App's path also does **not** clear `pendingAttachments` (only `handleWizardCancel` does) — a leak. Decide this explicitly in the port.
- `Ctrl+V` / `Cmd+V` / `Ctrl+I` on a text or textarea step with `onPasteImage` → `onPasteImage()`. Ignored on select/multiselect; a no-op (no crash) when the prop is absent.
- **text step:** `Enter` or `Tab` confirms the trimmed value; required + empty → mark dirty and block; `validate` is re-run synchronously on confirm and blocks, showing `Fix the error above` for 2000 ms. `↓` with non-empty filtered suggestions enters browse mode. `Backspace` / `Delete` on an **empty** field at step > 0 goes back a step. Everything else is delegated to `useTextInput.handleInput`, which sets `dirty` and exits browse mode.
- **text + browse mode:** `↑` at index 0 exits browse mode, otherwise index − 1; `↓` index + 1 (clamped); `Enter` → when `suggestionMode === 'value'`, advance with `selected.value`; otherwise call `onSuggestionSelected(selected.value)`. Any other key exits browse mode and is processed normally.
- **textarea step:** `Ctrl+Enter` / `Cmd+Enter` / `Tab` confirm (value = lines joined with `\n`, then trimmed). `Enter` (plain or Shift+Enter) inserts a newline, splitting the line at the cursor. `Ctrl+A` → column 0; `Ctrl+E` → end of line; `Ctrl+K` → kill to end of line; `Ctrl+U` → kill to start of line; `Ctrl+W` → kill word back. `Meta+←` / `Meta+B` word back; `Meta+→` / `Meta+F` word forward. `↑` / `↓` move by **visual** row, preserving the visual column. `←` at column 0 wraps to the end of the previous line; `→` at end of line wraps to column 0 of the next. `Backspace` at column 0 merges with the previous line — **the cursor column must be set to `prevLine.length` synchronously** (an explicit regression test: computing it inside the state updater or via `setTimeout` produced `XHelloWorld` instead of `HelloXWorld`). `Backspace` at (0,0) does nothing and **never navigates back** (asserted twice). Printable input inserts at the cursor; multi-line paste splits on `/\r?\n/` (so `\r\n` normalises to `\n`) and leaves the cursor at the end of the last pasted part.
- **select step:** `↑` / `k` index − 1; `↓` / `j` index + 1 (clamped); `Backspace` / `Delete` → previous step; `Enter` / `Tab` confirm the highlighted option, running `validate` **synchronously — there is no debounce for select**; number keys `1`–`9` jump-select and confirm that option immediately, **bypassing `validate`**.
- **multiselect step:** `↑` / `k`, `↓` / `j`, `Backspace` / `Delete` back; `Space` toggles the highlighted option; `Enter` / `Tab` confirm with `Array.from(multiSelected).join(',')` — **the submitted value is a comma-joined string, not an array**.

**Validation.** Debounced **300 ms** for text and textarea (`runValidation`); **synchronous** for select. An error renders only when `dirty` **and** the stored validated value equals the current value, so a stale debounced error is suppressed. The confirm path always re-validates the current value (an explicit stale-error race test: wait 400 ms so the empty-value error lands, type a valid value, wait only 20 ms, press Enter → must succeed). `enterBlockHint` auto-clears after 2000 ms. All of `dirty`, validation state, `enterBlockHint`, browse mode and the suggestion index reset on both forward and backward navigation; pending debounce timers are cleared.

**Per-step state pre-fill.** `text` → `textHook.reset(defaultValue)`. `textarea` → split on `\n`, cursor at the end of the last line. `select` → the index of the option matching `defaultValue`, else 0. `multiselect` → `new Set(defaultValue.split(','))`.

App re-keys the wizard as `` `${wizardConfig.kind}-${wizardConfig.title}` `` so changing kind or title remounts it.

---

### 5.2 Wizard catalogue

#### NEW AGENT — `getAgentWizardSteps(agents, teams, modelCatalog, defaultAdapter = 'claude')`, kind `agent`, title `NEW AGENT`

| id | label | type | default | skip | validation / notes |
|---|---|---|---|---|---|
| `name` | `Agent name` | text, required | — | — | `Name is required` for empty/whitespace; `Agent with this name already exists` when `v.trim()` collides. Placeholder `e.g. alpha, frontend-bot, reviewer`. Suggestions = all 15 Agent Shop templates `{value: key, label: name, hint: description}` in **callback** mode → selecting one calls `onSuggestionSelected`, which opens the pre-filled shop wizard |
| `adapter` | `Provider` | select | `defaultAdapter` | — | Options in order: `claude` **Claude** "Claude Code CLI"; `opencode` **OpenCode** "OpenCode — multi-provider"; `codex` **Codex** "OpenAI Codex CLI"; `cursor` **Cursor** "Cursor Agent CLI"; `pi` **Pi** "Pi coding agent RPC"; `grok` **Grok** "Grok CLI"; `antigravity` **Antigravity** "Google Antigravity CLI (agy)"; `shell` **Shell** "custom shell command". Shell is the 8th → `7 × j` reaches it |
| `command` | `Command` | text, required | — | `adapter !== 'shell'` | `Command is required for Shell`. Placeholder `e.g. npm test or bash scripts/deploy.sh`; description `Runs from task workspace · exit 0 = done · non-zero = failed` |
| `model` | `Model` | text, `suggestionMode: 'value'`, `suggestionsLabel: 'available models'` | — | `adapter === 'shell'` | Suggestions from `getModelOptions(adapter, modelCatalog)`; free text accepts custom model ids. Placeholder `Default, type a model ID, or press ↓ to browse` |
| `effort` | `Reasoning effort` | select | — | `!EFFORT_ADAPTERS.has(adapter)` where `EFFORT_ADAPTERS = {claude, pi, grok}` | Options in value order `['', 'high', 'medium', 'low']` → `Default` "no override — use model default"; `High` "deepest reasoning, best quality, slowest"; `Medium` "balanced speed and quality"; `Low` "fastest responses, minimal reasoning". **Must sit immediately after `model`** (asserted) |
| `role` | `Role / specialization` | select | — | `adapter === 'shell'` | `ROLE_PRESETS`: `''` **Skip** "no role description"; `Full-stack developer` "general purpose"; `Frontend developer` "React, CSS, UI"; `Backend developer` "APIs, databases, services"; `DevOps engineer` "CI/CD, infra, deploys"; `QA / Test engineer` "testing, quality"; `Code reviewer` "review PRs, find bugs"; `Technical writer` "docs, READMEs"; `__custom__` **Custom...** "type your own" |
| `role_custom` | `Describe the role` | textarea | — | `adapter === 'shell' \|\| role !== '__custom__'` | Placeholder `e.g. Specialist in React and TypeScript` |
| `skills` | `Skills (comma-separated)` | text | — | `adapter === 'shell'` | Placeholder `e.g. feature-dev:feature-dev, testing-suite:generate-tests` |
| `approval_policy` | `Approval policy` | text | — | **always skipped** (`skip: () => true`) | Hidden; populated only by shop templates |
| `team` | `Join team` | select | — | `adapter === 'shell' \|\| teamOptions.length <= 1` | `[{'', 'None', 'no team'}, ...active teams as {id, name, '{n} members'}]` |

**`agentWizardToInput(vals, defaultAdapter)`.** `adapter = vals.adapter || defaultAdapter`. For shell: `command = vals.command?.trim() || undefined`; `approval_policy` **forced to `'auto'`** regardless of input; `role`, `model`, `effort`, `skills`, `team_id` all `undefined`. For AI adapters: `role = role === '__custom__' ? (role_custom || undefined) : (role || undefined)`; `skills = vals.skills.split(',').map(trim).filter(Boolean)` or `undefined`; `approval_policy = vals.approval_policy || 'auto'`; `model = vals.model || undefined`; `effort = vals.effort || undefined`; `team_id = vals.team || undefined`.

**Submit** (`kind === 'agent'` or `'agent_from_shop'`): `Creating agent "{name}"...` (amber) → `onAddAgent(name, adapter, { command, model, effort, role, approval_policy, skills })` → `✓ Created agent "{name}" ({id}, {adapter})[ · {command}]` (green); then if `team_id` → `onJoinTeam(team_id, agent.id)` → `✓ Joined team "{name}"` and `refreshAll({ includeTeams: true })`, otherwise `refreshAll()`. Failure → `Failed: {err}` (red).

**Live model refresh.** While an `agent`, `edit_agent` or `agent_from_shop` wizard is open, a `useEffect` on `modelCatalog` **rebuilds the step list in place**, preserving the chosen adapter, the typed name, and the template, so freshly discovered models appear in the already-open wizard.

#### AGENT SHOP — `getShopWizardSteps()`, kind `agent_shop`, title `AGENT SHOP`
A single select step `shop_template`, label `Agent Shop — choose a template`, options = all 15 `AGENT_SHOP_TEMPLATES`:

| key | name | tier | approval_policy |
|---|---|---|---|
| `backend-dev` | Backend Developer | balanced | auto |
| `frontend-dev` | Frontend Developer | balanced | auto |
| `qa-engineer` | QA Engineer | balanced | auto |
| `code-reviewer` | Code Reviewer | capable | suggest |
| `architect` | Architect | capable | suggest |
| `devops-engineer` | DevOps Engineer | balanced | auto |
| `bug-hunter` | Bug Hunter | balanced | auto |
| `tech-writer` | Technical Writer | balanced | auto |
| `marketer` | Marketer | balanced | auto |
| `content-creator` | Content Creator | balanced | auto |
| `growth-hacker` | Growth Hacker | balanced | auto |
| `security-auditor` | Security Auditor | capable | suggest |
| `performance-engineer` | Performance Engineer | balanced | auto |
| `data-engineer` | Data Engineer | balanced | auto |
| `fullstack-dev` | Full-Stack Developer | balanced | auto |

Each option's hint is the template's `description`. Entered via `/agent shop` or `Ctrl+S` / `Cmd+S` inside the NEW AGENT wizard; refused when `defaultAdapter === 'shell'`.

**Submit:** resolve the template, build the base agent steps, run `applyShopTemplate`, then open a **second** wizard titled `NEW AGENT — {template.name}` with kind `agent_from_shop` and `templateKey`. No template → `No template selected` (yellow).

**`applyShopTemplate(baseSteps, template, defaultAdapter)`** injects: `name ← template.name`; `adapter ← defaultAdapter`; `model ← resolveModel(defaultAdapter, template.tier)` when that value exists among the resolved suggestions/options, else the `''` option's value, else the first option's value, else `''`; `role ← '__custom__'`; `role_custom ← template.role`; `skills ← template.skills.join(', ')` with MCP skills (those containing `:`) filtered out unless the adapter is `claude`; `approval_policy ← template.approval_policy`.

#### EDIT AGENT — `getEditAgentWizardSteps(agent, agents, teams, modelCatalog)`, kind `edit_agent`, title `EDIT AGENT`, `targetId`
Same step ids minus `skills` and `approval_policy`. Every skip predicate falls back to `agent.adapter` when `vals.adapter` is absent (`(vals.adapter || agent.adapter)`).

Defaults: `name ← agent.name`; `adapter ← agent.adapter`; `command ← agent.config.command ?? ''`; `model` via `getDefaultValue(vals)` returning `agent.config.model ?? ''` **only while the adapter is unchanged** (changing adapter clears it); `effort ← agent.config.effort ?? ''`; `role ←` the matching preset value, else `'__custom__'` when a non-preset role exists, else `''`; `role_custom ←` the raw role when it is not a preset; `team ←` the id of the team containing this agent, else `''`. Name validation excludes self (`a.id !== agent.id`).

**`editAgentWizardToFields(vals)`:** `command` = trimmed when shell; `''` (clear) when an adapter was chosen and it is not shell; `undefined` when there is no adapter key. `effort` is preserved as `''` to allow **clearing** the field (deliberately different from the create mapper, which drops empties). `role` / `model` / `effort` / `team_id` are `undefined` for shell. `team_id = vals.team || undefined`.

**Submit:** `Updating agent...` → `onUpdateAgent(targetId, { name, adapter, role, command, model, effort })` → `✓ Updated agent "{name}"`. Team reconciliation: `oldTeamId` from `liveTeams`; `newTeamId = values.team === undefined ? oldTeamId : (fields.team_id ?? '')` — **a skipped Team step means "unchanged", not "leave the team"** (explicitly asserted for shell agents: editing a shell agent must call `onUpdateAgent` once and must **not** call `onLeaveTeam`). Then `onLeaveTeam` when `oldTeamId && oldTeamId !== newTeamId` (`✓ Left team "{n}"`), `onJoinTeam` when `newTeamId && newTeamId !== oldTeamId` (`✓ Joined team "{n}"`), then `refreshAll({ includeTeams: teamOps.length > 0 })`. Decision matrix: `('team_1','team_2') → [leave, join]`; `('team_1','') → [leave]`; `('','team_1') → [join]`; `('team_1','team_1') → []`; `('','') → []`.

#### NEW TASK — `getTaskWizardSteps(agents)`, kind `task`, title `NEW TASK`

| id | label | type | default | skip | validation |
|---|---|---|---|---|---|
| `title` | `Task title` | text, required, placeholder `What needs to be done?` | — | — | `Title is required` (trim-aware) |
| `priority` | `Priority` | select | `'3'` | — | `Priority must be 1-4` — `Number(v)`, reject `!Number.isInteger(n) \|\| n < 1 \|\| n > 4`. Options `1` **P1 Critical** "urgent, do first"; `2` **P2 High** "important"; `3` **P3 Medium** "default priority"; `4` **P4 Low** "nice to have" |
| `assignee` | `Assignee` | select | — | `agentOptions.length <= 1` | `[{'', 'Auto-assign', 'orchestrator picks the best agent'}, ...non-disabled agents]`. Each agent hint = `[{adapter}] {first line of role, trimmed}` truncated at 60 chars (59 + `…`), or the bare `{adapter}` when there is no role. Agents with `status === 'disabled'` are excluded |
| `description` | `Description` | textarea, placeholder `Optional details, context, acceptance criteria...` | — | — | — |

**Submit:** `taskWizardToInput` → `{ title, priority: parseInt, assignee, description }`. `pendingAttachments` are snapshotted and cleared. `Creating "{title}"...` → `onCreateTask(title, { priority, description, attachments })` → `✓ Created "{title}" ({id})[ 📎{n}]`; then `onAssignTask(task.id, assignee)` when an assignee was chosen; `refreshAll()`.

#### EDIT TASK — `getEditTaskWizardSteps(task, agents)`, kind `edit_task`, title `EDIT TASK`
The same four steps with defaults `task.title`, `String(task.priority)`, `task.assignee ?? ''`, `task.description || ''`; the assignee sentinel becomes `None / Auto` / `remove assignee`. `editTaskWizardToFields` → `{ title, priority, assignee: vals.assignee || undefined, description: vals.description ?? '' }`.

**Submit:** `Updating task...` → `onUpdateTask(targetId, { ...fields, attachments })` → `✓ Updated "{title}"[ 📎{n}]`; re-assign when `fields.assignee`; `refreshAll()`.

#### NEW TEAM — `getTeamWizardSteps(agents, teams)`, kind `team`, title `NEW TEAM`

| id | label | type | skip | notes |
|---|---|---|---|---|
| `name` | `Team name` | text, required, placeholder `e.g. frontend, backend, qa` | — | `Name is required`; `Team with this name already exists` (trim-aware) |
| `lead` | `Team lead` | select | — | options = non-disabled agents, **no empty sentinel** |
| `members` | `Team members` | multiselect | no agent other than the lead exists | `getOptions` filters out `vals.lead` |
| `description` | `Description` | textarea, placeholder `Optional team purpose...` | — | — |

`teamWizardToInput` → `{ name, lead_agent_id: vals.lead, member_agent_ids: split(',').filter(Boolean) or undefined, description: vals.description || undefined }`.

**Submit:** `Creating team "{name}"...` → `onCreateTeam` → `✓ Created team "{name}" ({id}, {n} members)`; `refreshAll({ includeTeams: true })`.

#### New Goal — `getGoalWizardSteps(agents)`, kind `goal`, title `New Goal`

| id | label | type | description | skip |
|---|---|---|---|---|
| `title` | `Goal title` | text, required, placeholder `e.g. "Implement OAuth2 login with Google and GitHub"` | `Be specific — agents work better with clear, measurable objectives` | — |
| `assignee` | `Assignee` | select | `Assigned agent gets autonomous mode — it will plan and execute without prompts` | `agentOptions.length <= 1` |
| `description` | `Description` | textarea, placeholder `Success criteria, constraints, technical context...` | `Context matters — include tech stack, constraints, and what "done" looks like` | — |

Assignee sentinel: `Any agent` / `auto-assign to autonomous agents`. `goalWizardToInput` → `{ title, assignee || undefined, description || undefined }`. **Submit:** `Creating goal "{title}"...` → `✓ Created goal "{title}" ({id})`.

#### Edit Goal — `getEditGoalWizardSteps(goal, agents)`, kind `edit_goal`, title `Edit Goal: {title}`
The same three steps with goal defaults; the assignee sentinel hint becomes `auto-assign`. `editGoalWizardToFields` → `{ title, assignee || undefined, description ?? '' }`. **Submit:** `Updating goal...` → `✓ Updated goal "{title}"`.

#### SETTINGS — `getConfigWizardSteps(setting, palette, filter, maxConcurrent, notifications)`, kind `config`
**Always exactly one step.**

| setting (kebab) | step id (snake) | label | options |
|---|---|---|---|
| `palette` | `palette` | `Color palette` | `amber` **Amber** "warm ORCH default"; `ocean` **Ocean** "cool blue control room"; `forest` **Forest** "calm green operations"; `violet` **Violet** "high-contrast purple"; `light` **Light** "high-contrast palette for light terminals" |
| `activity-filter` | `activity_filter` | `Activity filter preset` | `all` **All** "show everything"; `text` **Text** "agent output only"; `tools` **Tools** "tool calls, results, files"; `errors` **Errors** "errors only"; `events` **Events** "lifecycle, system events" |
| `max-concurrent` | `max_concurrent` | `Max concurrent agents` | `1, 2, 3, 4, 6, 8, 10` with labels `{n} agent(s)` and hints `~{n*0.5} GB RAM, {n} subprocesses` |
| `notifications-toast` | `notifications_toast` | `Toast notifications` | `true` **On** / `false` **Off** |
| `notifications-bell` | `notifications_bell` | `Bell on completion` | `true` **On** / `false` **Off** |

Defaults are the current live values. **Submit:** palette → `changePalette` + persist; activity filter → set the `MsgType` set and persist the preset label; `max_concurrent` → `parseInt`, applied only when `> 0`, persisted; toast/bell → merge into `notifications` and persist. Always ends with `Settings saved` (green).

### 5.3 Complete validation-message catalogue
`Title is required` · `Priority must be 1-4` · `Name is required` · `Agent with this name already exists` · `Team with this name already exists` · `Command is required for Shell` · `Fix the error above`

### 5.4 Model options
`getModelOptions(adapter, catalog)` returns the catalog entry for a known adapter when non-empty, otherwise `getFallbackModelOptions(adapter)`. The neutral fallback for an unknown adapter is exactly `[{ value: '', label: 'Default', hint: 'use adapter default' }]`. Per-adapter fallbacks (`FALLBACK_MODEL_OPTIONS`): claude `['' Default "use Claude configured default", opus "latest Opus alias", sonnet "latest Sonnet alias"]`; codex `['' Default "use Codex configured default"]`; cursor `['' Default "use Cursor configured default", auto "let Cursor choose"]`; opencode `['' Default "use model configured in OpenCode"]`; pi `['' Default "use Pi configured default; run /login in Pi if unavailable"]`; grok, antigravity `['' Default "use {X} configured default"]`; shell `['' Default "model is not used by the shell adapter"]`. `ModelCatalog` is `Partial<Record<AdapterKind, ModelOption[]>>`; `onLoadModelCatalog(onUpdate?)` streams per-adapter results through `onUpdate` as each CLI responds and resolves with the full catalog.

---

## 6. Layout

**Root.** A column `Box width=W height=H` where `W = stdout.columns ?? 80` and `H = stdout.rows ?? 24`; a `'resize'` listener on stdout updates both.

**Regions, top to bottom.**
1. `Header` — exactly **5 rows**: blank, BrandBar, blank, StatsBar, ScannerBar
2. blank spacer (1 row)
3. the help overlay (replaces everything below when open) **or** the main content region (`mainH` rows)
4. blank spacer (1 row)
5. the bottom panel (`feedH` rows): `FormWizard` **or** `COMMANDS` label + `SuggestionsPanel` **or** the NEW TASK input **or** a task / goal / agent / log detail panel **or** `ACTIVITY` label + `ActivityFeed` **or** an `OnboardingBox`
6. `flexGrow: 1` spacer
7. `ToastBanner` (0–2 rows)
8. `UndoBanner` (one row per pending deletion)
9. `CommandBar` (1 row)

**Sizing math (port verbatim).**
```
fixedRows  = 9                      // Header(5) + gap(1) + SectionLabel(1) + gap(1) + CommandBar(1)
contentH   = max(4, H - 9)
ruleW      = max(10, W - 2)

agentSectionRows = activeTeamCount > 0 ? activeTeamCount + (hasUnassigned ? 1 : 0) : 0
listItemCount    = goals  : sortedGoals.length + 1
                 | tasks  : visibleTasks.length + 1 + (hidden>0 ? 1 : 0) + taskGroupSectionRows
                 | agents : liveAgents.length + 1 + agentSectionRows
                 | logs   : 0
minListH   = min(listItemCount + 1, ceil(contentH * 0.5))   // list never exceeds 50% by default
baseMainH  = logs ? contentH : max(2, min(minListH, contentH - 4))

logs:       mainH = contentH;  feedH = 0
maximized:  mainH = 0;         feedH = max(1, contentH)
otherwise:  mainH = max(3, min(baseMainH + splitOffset, contentH - 4))
            feedH = max(1, contentH - mainH)

maxSplitOffset = contentH - 4 - 3   // list min 3 rows, detail min 4 rows
```
`splitOffset` is re-clamped to ±`maxSplitOffset` on every `contentH` change. `+` / `=` move it by −3, `-` by +3. Exiting maximize via `+` jumps to `-floor(contentH/2)`; via `-` to `+floor(contentH/2)`. All three list scroll offsets are clamped to `max(0, listLength - mainH)` whenever the list length or `mainH` changes. When a logs picker is open, `LogsContent`'s height becomes `max(3, mainH - 16)`.

**Responsive rules.** There are **no breakpoints and no minimum-size guard**. The only width-conditional behaviour anywhere is the logs task-context badge (`width > 80`). Everything else degrades through `Math.max` floors and manual truncation. There is no "terminal too small" screen.

**Truncation.** `capLine(s, n) = s.length > n ? s.slice(0, n - 1) + '…' : s`. `capText(s, max = MAX_PANEL_TEXT = 10_000)` appends `"\n…[truncated]"`. Ink's `wrap="truncate"` is used on many `<Text>` nodes as a second line of defence, but widths are **also** computed manually — the arithmetic is the real specification. Specific caps: goal badge title 13; agent-row error hint 30; toast title 40 (39 + `…`); agent skills 500; role lines 500; error-panel raw message 120; assignee/shop role hint 60 (59 + `…`); logs task badge 20; log-detail task title 30; `/task show` description 100; event summaries 150–200.

**Header contents.**
- BrandBar left: `PulsingDiamond` (`◆`, alternating amber ⇄ amberDim every 10 animation ticks, animating **only** when `stats.running > 0`), ` ORCH` amber bold, ` {version}` ghost, then an update chip when `latestVersion !== version` — `  UPDATE {latest} ` or `  v{latest} INSTALLED — RESTART TO APPLY ` (green on successBg, bold) — then ` · ` and the project name in silver.
- BrandBar centre: the tab pills (§1).
- BrandBar right: the mode chip — `● WATCHING` (green/successBg bold), `● OBSERVING` (amber/accentBg bold) or `○ IDLE` (dim/neutralBg); then, when running, `  {spinner} {n} active ` (green/successBg); then ` {uptime}` ghost, where uptime = `formatDurationSince(state.started_at)`.
- StatsBar left: status chips, each shown only when its count > 0, in order RUN (`▶` + spinner, green/successBg, bold), RETRY (`↻`, yellow/warnBg), REVIEW (`◈`, blue/toolBg), TODO (`○`, dim/neutralBg), DONE (`✓`, green/successBg), FAIL (`✕`, red/errorBg, bold), TEAMS (`◆`, amber/accentBg). When none are shown: ` NO TASKS ` on neutralBg/dim.
- StatsBar right: an optional sparkline using `[' ','▁','▂','▃','▄','▅','▆','▇','█']` (the `sparklineData` prop is never passed — dead); then, when `tokens.total > 0`, a token chip ` ↑{input} ↓{output}[ 🧠{reasoning}] · Σ{total} ` cyan on accentBg, each number via `formatTokens(n) = n >= 1000 ? (n/1000).toFixed(1) + 'k' : String(n)`.
- ScannerBar: `width = max(10, W - 2)`. Idle → a flat ghost heavy rule. Active (`running > 0`) → a bright amber segment of `segLen = max(4, floor(width * 0.08))` bouncing left↔right at `step = 2` characters per 120 ms animation tick; `totalFrames = ceil((width + segLen) / step)`, position `tick % (totalFrames * 2)` with a triangular bounce, rendered as three segments (before / bright / after) to minimise `Text` nodes.

**Section labels.** `SectionLabel`: `━━━` + ` {LABEL} ` (dim bold on neutralBg) + an optional `━━` + a suffix node + a trailing `━…` sized `width - usedLen - gap - suffixLen`. `DetailSectionLabel` / `AgentDetailSectionLabel`: `━━━` + ` DETAIL ` (amber on accentBg) or ` AGENT ` (green on successBg) + `━ ` + the title/name truncated to `width - chip - hint - 10` with `...` + a trailing rule + the resize hint. Resize hint text: `+/- resize │ M max`, or `+/- exit max` when maximized.

**`ActivityFeed`.** Section label suffix ` F:{FILTER.toUpperCase()} │ {filteredCount}/{totalCount}` (`F:` dim, label amber, counts ghost). Rows at `height = max(1, feedH - 1)`: `messages.slice(-height)`, **top-padded with blank rows so exactly `height` rows always render**. `textW = max(10, width - 2 - 17)`. Columns: a 1-char agent accent (`▍` on a new agent group, `▏` on a continuation, space otherwise); a 5-wide relative timestamp (blank on continuations; green when `now`, else dim); a 9-wide agent name (first 8 chars, bold, agent color) or `AGENT_INDENT` (9 spaces); the type icon + a space; then `capLine(text, textW)`. Zebra striping: odd agent groups get `alternatingRowBg`; `error` rows always get `errorBg` and override the stripe.

---

## 7. Live-update mechanics

**Event subscription.** `onSubscribeEvents(handler) → unsubscribe`, held in a `useEffect` that also owns a **150 ms debounced** refresh. For every event: (a) on `agent:started`, record `runId → agentId` and `runId → taskId` **before** formatting so output events resolve correctly; (b) `formatEvent(...)` pushes a message; (c) onboarding transitions; (d) toast + tab flash on `task:status_changed`; (e) `scheduleRefresh()` for `task:status_changed`, `task:created`, `task:assigned`, `agent:started`, `agent:completed`, `run:retry`, `goal:created`, `goal:status_changed`, `goal:updated`, `goal:deleted`.

**Batched message queue.** `addMessage` pushes onto a `pendingMessages` ref, also truncating that staging buffer to the last `MAX_MESSAGES`. `messageBatchMs` defaults to **80 ms** (`0` under VITEST; `0` means flush synchronously). One `setTimeout` per burst. The flush replaces state with `batch.slice(-MAX_MESSAGES)` when the batch alone exceeds the cap, otherwise `prev.slice(-(MAX_MESSAGES - batch.length)).concat(batch)`. The timer is cleared on unmount.

**Caps and limits.**

| Constant | Value | Purpose |
|---|---|---|
| `TASK_LIST_LIMIT` | 10 | collapsed task list size |
| `MAX_RUN_MAP_SIZE` | 500 | runId→agentId / runId→taskId maps; both pruned together, oldest-insertion-first (Map order as a poor-man's LRU) |
| `MAX_DETAIL_LEN` | 2048 | detail strings; `+ '…[truncated]'` in `addMessage`, `+ '…'` in the formatters |
| `MAX_MESSAGES` | 500 | activity feed ring buffer |
| `MAX_TOASTS` | 5 | toast queue (FIFO, oldest dropped) |
| `ToastBanner MAX_VISIBLE` | 2 | toasts actually rendered (the **first** two of the queue) |
| `MAX_PANEL_TEXT` | 10 000 | goal descriptions / progress reports |
| `CommandHistory` | 100 | command history entries |
| undo stack | 50 | text-input undo depth |
| `UNDO_TIMEOUT_MS` | 5 000 | soft-delete window |

**Intervals and animation.**

| Timer | Interval | Notes |
|---|---|---|
| global animation tick | **120 ms** | one module-global `setInterval` in `useAnimTick`; starts on the first subscriber, stops when the last unsubscribes; `useAnimTick(false)` unsubscribes without unmounting; `_resetAnimTick()` for tests |
| spinner frames | — | 10 braille frames `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏` (U+280B, 2819, 2839, 2838, 283C, 2834, 2826, 2827, 2807, 280F), index `tick % 10`; frame 0 is `⠋` |
| message batch flush | 80 ms (0 in tests) | configurable via `messageBatchMs` |
| event-driven refresh debounce | 150 ms | |
| periodic disk poll | 3 000 ms observer / 5 000 ms watch | skipped when `Date.now() - lastRefreshAt < INTERVAL` |
| `useNow` relative clock | 5 000 ms | LogsContent and ActivityFeed |
| undo banner countdown | 1 000 ms | display only |
| pending-deletion sweep | 1 000 ms | subscribed only on the empty↔non-empty transition |
| tab flash | 12 ticks ≈ 1 440 ms | 3 blinks × 2 phases × 2 ticks |
| toast fade | 360 ms | dim for the first and last 360 ms |
| toast auto-dismiss | done 4 000 / failed 8 000 / review 6 000 ms | `onDismiss` fires at `delay + 360` |
| onboarding completed→dismissed | 5 000 ms | |
| delayed update re-check | 5 000 ms | only when the initial check returned nothing |
| undo snapshot debounce | 500 ms | `useTextInput` |
| wizard validation debounce | 300 ms | text/textarea only |
| `enterBlockHint` auto-clear | 2 000 ms | |
| logs session-boundary gap | 30 000 ms | same-agent gap that starts a new visual session |

**`refreshAll({ includeTeams })`.** Runs tasks / agents / state / (teams) / goals in parallel. Tasks, agents and goals are only committed to React state when `entityListChanged(prev, next)` is true: lengths differ, or any `id` / `updated_at` differs, or — **for entities with no `updated_at`, i.e. agents** — a deep `JSON.stringify` comparison differs. State is always committed. `watchActive` is re-synced from `state.pid` **only when `initialWatchActive`** (otherwise a stale lock from another process would flip it).

**History loading.** `onLoadHistory(onBatch)` is called once on mount; each batch is mapped to messages and **prepended** (`[...histMsgs, ...prev]`), then truncated to the last `MAX_MESSAGES`. Mapping rules: `error` → `formatAgentError` (summary = the first non-blank line of the unwrapped message, 200 chars; detail = the raw string capped at 2048) and, when `isModelMetadataWarning(text)`, re-labelled `⚠ {text}` yellow/`info`; otherwise de-duplicated per `runId\0text` so repeated nested provider errors render once, coloured red/`error`. `file_changed` → the path, purple/`file`. `done` → `Completed`, green/`lifecycle`. `tool_call` → `⚙ {name}()`, cyan/`tool`. Everything else → `formatAgentOutput` (a `null` summary skips the entry) + `classifyAgentSummary`. Errors from the promise are written to stderr and never crash the UI. The host (`tui.ts`) loads all runs once, sorts descending by `started_at`, keeps `succeeded`/`failed` runs, and delivers two batches — the first 3 runs immediately, the next 7 in the background — reading 30 events of tail each, sorting ascending, and slicing each batch to the last 200 entries.

**`formatEvent` → message mapping.**

| Event | Text | Color / type |
|---|---|---|
| `agent:started` | `Started task` | green / lifecycle |
| `agent:output` | `formatAgentOutput(data).summary` | from `classifyAgentSummary` |
| `agent:file_changed` | `{path}` | purple / file |
| `agent:completed` | `Completed successfully` or `Failed` | green / red, lifecycle |
| `agent:error` | `formatAgentError(error).summary` | red / error |
| `task:status_changed` | `{from} → {to}` | cyan / system |
| `task:assigned` | `Assigned → {agentId}` | cyan / system |
| `task:created` | `Created: {title}` | amber / system |
| `run:retry` | `Retry #{attempt} ({n}s delay)` | yellow / lifecycle |
| `orchestrator:tick` | `{running} running · {queued} queued` — only when running or queued > 0 | ghost / system |
| `orchestrator:stall_detected` | `Stall detected` | yellow / error |
| `task:cascade_failed` | `Cascade failed (dep: {failedDependencyId})`, `detail = reason` | red / error |

**`classifyAgentSummary(summary)`.** `/^\[[\w.:-]+\]$/` → lifecycle/dim; starts with `⚙` → tool/dim; starts with `$ ` → tool/dim; starts with `←` → result/dim; starts with `✓` → lifecycle/dim; starts with `⚠` → info/yellow; starts with `⏳` → info/silver; otherwise output/silver.

**`formatAgentOutput(raw)`** — a long JSON-shape dispatcher; port as-is. Handles: bracket lifecycle tags; Codex `item.started` / `item.completed` re-entry with the inner item; legacy flat Codex shapes `agent_message` (use `.text`), `reasoning` (**null → skipped**), `command_execution` (`$ {command} → {first line of aggregated_output/result}`), `web_search` (`⚙ web_search({query})`); canonical AgentEvent shapes `{text}`, `{command,result}`, `{paths}` (`✎ {joined}`), `{message}` (`✕ {msg}`), `{result}` (`✓ {res}`); Claude `type:'message', role:'assistant'` and `type:'assistant'` streams via `extractTextFromContent` (**null → skipped**, for tool_use-only messages); `type:'user'` → `← {summarizeToolResult}`; `tool_use` or `{name, input}` → `⚙ {name}({hint})`; `tool_result` → `← …`; `type:'result'` → `✓ {result}` or `✓ Agent finished`; `rate_limit_event` → `⏳ Rate limited ({rateLimitType})`; any `subtype` → nested message text or `[{subtype}]`; a generic `content` field; fallback `[{type}]` or `raw.slice(0,150)`. On a JSON parse failure, `extractSummaryFromTruncated` regex-extracts `subtype` / `type` / `role` / `text` / `name` / `result` and returns `[{subtype}]`, `💬 (assistant)`, `← (tool result)`, `⚙ {name}()`, `✓ {result}` / `✓ Agent finished`, `⏳ Rate limited`, or `[{type}]`.

**Helpers.** `formatToolInput(name, input)`: `file_path` → the last two path segments; `command` → the first 60 chars; `pattern` → `"…"` 40 chars; `glob` → 40 chars; otherwise compact JSON capped at 80 with `...`. `extractTextFromContent(content, maxLen = 200)`: strings are sliced; arrays are walked, taking the first non-blank line of each `text` block, `⚙ {name}({hint})` for `tool_use`, `← (result)` for `tool_result`, `💭 {first line, 60}` for `thinking`, joined with spaces; `null` when nothing was found. `summarizeToolResult(content)`: for a string, the first non-blank line (`{line.slice(0,80)}... ({n} lines)` when > 3 lines, else 120 chars); for an array, per `tool_result` block `✕ error: {60}` when `is_error`, `✓ {n} lines` when > 3 lines, else `✓ {80}`; `(result)` when empty. `firstLineTrunc(s, n)`: fast path when there is no `\n`, otherwise the first line matching `/\S/`.

**Message type system.** `MsgType = 'system' | 'lifecycle' | 'output' | 'tool' | 'result' | 'error' | 'file' | 'info'`. Icons `MSG_ICONS`: system `◆`, lifecycle `▶`, output `│`, tool `⚙`, result `←`, error `✕`, file `✎`, info `│`. `getMsgTextColor`: output → white; tool / result / lifecycle / system → dim; file → gray; error → red; default → the message's own color. `getMsgBg`: error → `errorBg`; everything else → none (a deliberately calm feed).

---

## 8. Distinct features

### 8.1 Soft delete with undo
- `scheduleDeletion(entityType, entityId, entityName, { needsForceStop })` pushes `{ key: ++pendingDeletionSeq, entityType, entityId, entityName, expiresAt: Date.now() + 5000, needsForceStop }` and emits `✗ "{name}" will be deleted in 5s — press Z to undo` (yellow).
- `Z` / `z` pops the **most recent** pending entry (LIFO) and emits `↶ Undo: "{name}" restored` (green). Multiple pending deletions coexist and all appear in the banner simultaneously.
- A 1-second sweep collects expired entries **atomically inside the state updater** (avoiding a stale-closure race), then executes each: `task` → `onDeleteTask`; `agent` → `onForceStopAgent` first when `needsForceStop`, then `onDeleteAgent`; `goal` → `onDeleteGoal`. Then `✓ Deleted "{name}"` (green) + `refreshAll()`, or `Failed to delete "{name}": {err}` (red).
- Guards: tasks with `status === 'in_progress'` can never be deleted; a running agent requires `onForceStopAgent` (otherwise a refusal message). **`onForceStopAgent` is not called while the deletion is still pending** — only at execution time (asserted).
- Entry points: `D` in all three list views, plus `/task delete`, `/agent delete|remove`, `/goal delete`.
- A module-global monotonic `pendingDeletionSeq` with a test-only `_resetPendingDeletionSeq()`.
- Timer semantics asserted with fake timers: nothing after 200 ms; `onDeleteTask('tsk_1')` fires after a further ~6 200 ms (5 000 ms window + the 1 000 ms countdown tick + a microtask), plus ~100 ms to flush `queueMicrotask`.

### 8.2 Paste-image / clipboard
- `onPasteImage` is passed to `FormWizard` only for `task` and `edit_task` wizards (`isPasteCapable`), so the `⌘+V paste image` hint appears only there.
- `handlePasteImage()` lazily imports `infrastructure/clipboard-service.js` and calls `detectClipboardType()`. `'text'` → `Clipboard has text, not image` (dim); `'empty'` → `Clipboard is empty` (dim). For an image: `getClipboardImage()`; on failure `Failed to read clipboard image` (red). On success: `mkdtemp(join(tmpdir(), 'orch-paste-'))`, write `clipboard-{Date.now()}.{img.ext}`, push the path onto `pendingAttachments`, emit `📎 Image attached ({round(bytes/1024)}KB)` (green). Any throw → `Clipboard paste failed` (red).
- Platform support in the service: macOS via `osascript` (`clipboard info` / `clipboard as «class PNGf»`); Linux via `xclip -selection clipboard`; Windows via PowerShell `Get-Clipboard`. `isClipboardToolAvailable()` checks for `xclip` on Linux (macOS and Windows always true). 3-second exec timeout.
- `pendingAttachments` are consumed and cleared by the task / edit-task submit; the count surfaces as `footerExtra = "📎{n}"` in the wizard footer and ` 📎{n}` in the success message. `handleWizardCancel` clears them; **App's Esc path does not** (leak).
- There is no clipboard *writing* anywhere in the TUI.

### 8.3 Onboarding
- **Initial step derivation** (from props): `state.onboardingCompleted` → `dismissed`; `stats.total_tasks_completed > 0` → `dismissed`; a non-empty `state.running` map → `run_started`; `tasks.length > 0` → `task_created`; otherwise `welcome`.
- **Transitions** (event-driven, one-way, never re-entered from `dismissed`): `welcome` →(`task:created`)→ `task_created` →(`agent:started`)→ `run_started` →(`task:status_changed` with `to === 'done'`)→ `completed` →(5-second timer)→ `dismissed`, which also calls `onCompleteOnboarding()` exactly once.
- **Rendering, tasks view only.** `welcome` replaces the task list with `WelcomeScreen` (a box `min(width-4, 50)` wide containing `◆ Welcome to Orch`, `Press N to create your first task`, and the hint `N new task`). `task_created` / `run_started` render `OnboardingNudge` under the list — `◆ Press R to run task` with the hint `R run task`, or `◆ Agent is running your task...` with no hint. `completed` renders `OnboardingToast` (`First task completed!` green bold + `Type / to see all commands`).
- **Separate per-tab empty state.** `OnboardingBox` renders in the bottom panel whenever there are no messages, with `count` = the view's item count. It returns `null` at `count >= 3`; a compact one-line nudge at 1–2 (`◆ {nudge}  {hintKey} {hintLabel}`); the full box at 0 (title, 3 description lines, hotkey hints). Box width `min((width ?? 44) - 4, 50)`, content width `boxW - 6`.
- **Configs** (`onboarding-config.ts`):
  - **Goals** — title `Goals`; description `Define what your team should achieve.` / `The orchestrator breaks goals into tasks` / `and assigns them to agents automatically.`; hints `N new goal`, `/ commands`; nudge `Add more goals to keep your team focused.`
  - **Tasks** — title `Tasks`; description `Units of work dispatched to agents.` / `Create them manually or let goals` / `generate them automatically.`; hints `N new task`, `W start orchestrator`; nudge `Add more tasks to keep agents busy.`
  - **Agents** — title `Agents`; description `AI workers that execute your tasks.` / `Adapters: claude, opencode, codex, cursor,` / `pi, grok, antigravity, shell.`; hints `N new agent`, `W start orchestrator`; nudge `Add more agents to increase parallelism.` **Tested invariant: the description must name every `SUPPORTED_ADAPTERS` entry and stay ≤3 lines.**
- `CommandBar` highlights `? help` in amber while `onboardingCompleted === false`, gray otherwise.

### 8.4 Toasts, bell and tab flash
- Toasts fire from `task:status_changed` for `to ∈ {done, failed, review}` only, and only when `notifications.toast`. The title is the resolved task title (or the raw `taskId` when the task is unknown); the agent name is resolved from the task's assignee.
- Queue cap 5 (oldest dropped); render cap 2 (**the first two of the queue**, not the newest).
- Row: ` {icon} ` + the title (white bold, truncated to 40 = 39 chars + `…`) + ` {message} `, on `successBg` / `errorBg` / `infoBg` with green / red / blue foregrounds. Icons `✓` / `✕` / `◈`. Messages: `Task completed by {agentName ?? 'Agent'}`; `Task failed — press Enter for details`; `Task ready for review — press A to approve`.
- Fade: dimmed for the first 360 ms and the last 360 ms. Auto-dismiss delays done 4 000 / failed 8 000 / review 6 000 ms; `onDismiss(id)` fires at `delay + 360`. An empty toast array renders an **empty string** (nothing at all, not even a blank box).
- Terminal bell: `process.stdout.write('\x07')` when `notifications.bell` **and** the type is `failed` or `review` — **never for `done`**.
- Tab flash: when `notifications.toast` and the active view is not `tasks`, a `task:status_changed` sets `flashTab = { tab: 'tasks', color: green|red|blue }` (done/failed/review). `FlashingTabLabel` blinks 3 times (2 animation ticks per bright/dim phase, 12 ticks total), alternating the inverted pill and the normal inactive style, then calls `onFlashComplete`, which clears the flash. All three of `flashTab` / `flashColor` / `onFlashComplete` must be present; **the active tab never flashes**.

### 8.5 Goal ↔ task linking UI
- `tasksByGoalMap` (goalId → tasks) powers `GoalRow` progress bars; `goalMap` (goalId → Goal) powers `TaskRow` badges and grouping.
- `G` on the tasks view toggles `groupByGoal` (resetting selection and scroll); section headers carry per-goal `{total, done}` counts and a `% done`.
- Goal detail shows linked tasks (`liveTasks.filter(t => t.goalId === goal.id)`), a per-status breakdown, and the async progress report.
- Orphaned `goalId`s (the goal was deleted) render no badge and fall into UNGROUPED.
- **There is no UI to set a task's `goalId`** — no wizard step and no key. Linking happens outside the TUI (goal decomposition in the orchestrator). The Rust port should treat linking as read-only.

### 8.6 Other distinct features
- **Observer mode.** When `orchestrator.startWatch()` fails (a lock held by another process), `tui.ts` constructs a `DiskObserver`, swaps `onSubscribeEvents` to it, sets `observerMode = true`, and suppresses `watchError`. On mount the TUI emits `Observer mode: watching external orchestrator via disk polling.` (amber); otherwise, if `watchError` is set, `Watch mode failed: {err}. Tasks will not auto-dispatch.` (red). The mode drives the header chip and the 3 s vs 5 s poll interval.
- **Auto-update.** `onCheckUpdate` is retried once after 5 s when `latestVersion` was initially absent. The first detected version triggers `onBackgroundInstall(version)` exactly once (guarded by a ref); on success the header chip becomes `v{x} INSTALLED — RESTART TO APPLY`.
- **Runtime palette switching.** `changePalette(next)` mutates the shared `tuiColors` object in place, **remaps every already-stored message color** through `remapTuiColor(color, prev, next)` so semantic colors survive the switch, updates the `palette` state (which flows through `TuiPaletteContext` so `React.memo`'d rows re-render), and persists via `onSavePalette`.
- **Activity filter.** `ACTIVITY_PRESETS`: `all` (all 8 types), `text` (`output`), `tools` (`tool, result, file`), `errors` (`error`), `events` (`lifecycle, system`). `cyclePreset` finds the current preset by exact set equality and advances with wrap; the displayed label is derived the same way, defaulting to `all`. Filtering is skipped entirely when the set is full-size.
- **Watch toggle** is only reachable via `/watch` and `/pause`.
- **Launcher.** `orch tui` takes **no options at all** — `program.command('tui').description('Launch interactive TUI dashboard').action(...)`. Everything comes from project config (`project.name`, `defaults.agent.adapter`, `scheduling.max_concurrent_agents`) and global config (`tui.palette`, `tui.activity_filter`, `tui.notifications`). All 40+ callbacks are wired from the DI container. On exit it stops watch mode or the disk observer and calls `eventBus.clear()`.

---

## 9. Colors / theme — semantic roles

**Palettes.** Five, in this exact order: `amber` (default), `ocean`, `forest`, `violet`, `light`. 26 tokens each: `amber, amberDim, green, red, blue, yellow, cyan, purple, white, silver, gray, dim, ghost, void, errorBg, warnBg, successBg, infoBg, toolBg, accentBg, neutralBg, alternatingRowBg, solidText, pink, olive, orange`.

**Mutation model.** `tuiColors` is a **single mutable object**; `applyTuiPalette(name)` does `Object.assign(tuiColors, TUI_PALETTES[name])` so existing holders observe the new values (object identity is asserted stable). In Rust: one shared mutable/atomic palette, not a copied value.

**`remapTuiColor(hex, from, to)`** finds the token whose `from`-palette value equals the hex and returns the `to`-palette value; an unknown hex passes through unchanged. Token lookup must stay unambiguous — the `light` palette's distinct `gray` (`#5d6678`) and `ghost` (`#667085`) must not be conflated (asserted).

**Semantic roles.**

| Role | Token(s) |
|---|---|
| brand, accent, selection, active tab, cursor, section labels | `amber`; `amberDim` for the de-emphasised brand pulse and goal badges |
| task status | `in_progress`, `done` → green; `retrying` → yellow; `review` → blue; `todo`, `cancelled` → dim; `failed` → red (`getTaskStatusColor`) |
| goal status | `active` → green; `paused` → dim; `achieved` → amber; `abandoned` → ghost (`getGoalStatusColor`) |
| agent status | `idle` → dim; `running` → green; `error` → red; `disabled` → ghost |
| priority | 1 → red; 2 → yellow; 3 → dim; 4 → ghost |
| message text | output → white; tool/result/lifecycle/system → dim; file → gray; error → red; info → the message's own color |
| message background | error → `errorBg` only; selected log row → `infoBg`; odd activity group → `alternatingRowBg` |
| chip backgrounds | `successBg` (running/done/agent chip), `errorBg` (fail), `warnBg` (retry/browse mode), `toolBg` (review), `accentBg` (teams/detail chip/goal badge/token chip/input chip), `neutralBg` (todo/idle/off/section label), `infoBg` (review toast) |
| log type colors (picker) | system purple, lifecycle cyan, output white, tool blue, result green, error red, file purple, info silver |
| per-agent identity | `getAgentColors()` cycling `[green, blue, purple, yellow, cyan, pink, olive, orange]` at index `i % 8`, assigned by position in `liveAgents` |
| borders, rules, placeholders, inactive | `ghost` |
| secondary / tertiary text | `silver` / `dim`; hint-bar keys `gray` |
| foreground on saturated fills | `solidText` — asserted ≥ 4.5:1 WCAG contrast against `amber`, `green`, `red`, `blue` in the `light` palette |
| logs task-badge background | `void` (its only use) |

**Glyph inventory.** `◆` U+25C6 · `◈` U+25C8 · `◇` U+25C7 · `★` U+2605 · `⟳` U+27F3 · `⊕` U+2295 · `◉` U+25C9 · `‖` U+2016 · `●` U+25CF · `○` U+25CB · `✓` U+2713 · `✕` U+2715 · `↻` U+21BB · `↶` U+21B6 · `✗` U+2717 · `▶` U+25B6 · `▸` U+25B8 · `▼` U+25BC · `│` U+2502 · `←` U+2190 · `⚙` U+2699 · `✎` U+270E · `█` U+2588 · `░` U+2591 · `▍` U+258D · `▏` U+258F · `━` U+2501 · `─` U+2500 · `·` U+00B7 · `—` U+2014 · `…` U+2026 · `╭ ╮ ╰ ╯ ├ ┤ ┌` · `Σ` U+03A3 · `↑ ↓ ← →` · `📎` U+1F4CE · `🧠` U+1F9E0 · `💭` U+1F4AD · `💬` U+1F4AC · `⚠` U+26A0 · `⏳` U+23F3 · the 10 braille spinner frames · the `▁▂▃▄▅▆▇█` sparkline ramp.

`heavyRule(n)` / `lightRule(n)` are memoised string caches (a workaround for `Intl.Segmenter` cost in `string-width`) — irrelevant in Rust, but they document that rules are built by character repetition, not by border widgets.

---

## 10. Ink/React-specific behaviour needing re-expression

- **Flexbox layout.** `justifyContent="space-between"`, `flexGrow`, `gap`, `paddingX`, fixed-width `Box` columns and `wrap="truncate"` are Yoga-driven. ratatui needs explicit `Layout`/`Constraint` trees. Critically, the code already computes most widths manually (`fixedCols`, `textW`, `prefixW`), so port the arithmetic and treat Ink's wrapping as a redundant safety net.
- **Full-stdout redraw model.** `incrementalRendering: true`, the single 120 ms animation tick, `messageBatchMs`, the `React.memo` + `useTuiPalette()` context-subscription trick, the `heavyRule` / `lightRule` string caches, and the `agentColorMap` / `taskTitleMap` / `goalMap` / `memoizedTaskLogs` memos are all React render-cost optimisations, not product behaviour. Don't write parity tests against them; **do** keep the caps.
- **Multiple concurrent `useInput` handlers.** Ink dispatches a key to *every* mounted `useInput`. The App handler and the `FormWizard` / `LogsFilterPicker` / `LogsTypeFilterPicker` handlers all fire for the same key. This produces the real quirks documented above: Esc during a wizard is handled by both (App wins and closes it, leaking `pendingAttachments`); App's text-input buffer silently consumes wizard keystrokes; `showTypePicker` does not block App keys while `showAgentPicker` does. In Rust you will have one dispatcher with an explicit modal stack — **decide each of these explicitly** rather than inheriting the accident.
- **`React.memo` + context.** `TaskRow` / `AgentRow` / `GoalRow` / `Header` / `CommandBar` / `ToastBanner` / `HelpOverlay` / `UndoBanner` are memoised, and each row calls `useTuiPalette()` solely to re-subscribe to palette changes. Nothing to port; the tests asserting `typeof HelpOverlay === 'object'` are React-implementation tests and should be dropped.
- **Hook lifecycle / stale-closure workarounds.** Refs mirroring state (`liveTasksRef`, `liveAgentsRef`, `liveTeamsRef`, `notificationsRef`, `activeViewRef`, `executeDeletionRef`, `onGetGoalProgressRef`, `onCheckUpdateRef`, `cursorRef`), functional state updaters, the `[pendingDeletions.length > 0]` effect dependency trick, and `setPendingDeletions` used as an atomic collect-and-filter all vanish in Rust with owned mutable state — but the **semantics** they protect (LIFO undo, atomic expiry sweep, flush-on-unmount, no double-install of an update) must be preserved.
- **Immutable `Cursor` + `Intl.Segmenter`.** Grapheme segmentation, NFC normalisation and the display-width table (`displayWidth`: CJK Unified, Hangul Jamo/Syllables, CJK compatibility, fullwidth forms, and emoji U+1F300–U+1FAFF → 2 columns) must be re-expressed with `unicode-segmentation` + `unicode-width`. `deleteBack` / `deleteForward` returning the *same instance* at a boundary is a JS identity detail — in Rust just no-op. The whole `text-cursor.test.ts` suite is directly portable as pure unit tests.
- **`useTextInput` returning `handleInput`** rather than owning a hook exists only because FormWizard needs dispatch control — naturally a method on an input widget in Rust.
- **ink-testing-library harness assumptions.** Tests assert against a flattened `lastFrame()` string, so ANSI colors are invisible to them (color semantics are only covered by `colors.test.ts`), and many assertions are substring checks that would pass against a differently-laid-out frame. For ratatui, prefer a `TestBackend` buffer with explicit style assertions plus snapshots, and model the key-dispatch table as pure unit tests over a state struct.
- **Terminal size.** `useStdout()` + a `'resize'` listener with `80 × 24` fallbacks, and **no TTY in tests** (so most tests exercise whatever default ink-testing-library picks). In Rust, make the size an explicit input so the layout math is unit-testable at many sizes — the "three consecutive `+`" and "two consecutive `-`" clamp tests exist precisely because this math is fragile.
- **Kitty keyboard protocol.** `kittyKeyboard: { mode: 'auto', flags: ['disambiguateEscapeCodes'] }` is what makes `Ctrl+Enter` distinguishable from `Enter` and `Ctrl+I` from `Tab`. crossterm needs `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` and must keep the `Tab` fallback for terminals that refuse it.
- **Timer-driven animation as component state.** `ToastRow`'s own fade timers, `FlashingTabLabel` deriving elapsed ticks from a render-time ref, `UndoBanner`'s own interval, and `PulsingDiamond` / `ScannerBar` reading the global tick all become a single frame clock plus deadlines in app state. The *durations* are product behaviour and must be preserved; the per-component timer architecture must not be.
- **Lazy dynamic `import()`** (clipboard service, `update-check`, `DiskObserver`, Ink itself) is a Node startup-cost optimisation with no Rust analogue.
- **Direct terminal writes.** `process.stdout.write('\x07')` for the bell and `process.stderr.write` for history-load errors bypass the renderer — keep both as direct terminal writes outside the draw loop.

---

## 11. What the TUI tests assert, behaviourally

### 11.1 Directly portable as pure unit tests
- **`text-cursor.test.ts`** — grapheme segmentation (`'👍'`, `'🇺🇸'`, `'👋🏽'`, `'👨‍👩‍👧'` each length 1); `displayWidth` (ASCII/Cyrillic 1; CJK, Hangul, fullwidth, emoji 2); `textDisplayWidth('hi你!') === 5`; `Cursor` construction (defaults to the end, clamps out-of-range, NFC-normalises); navigation (`moveLeft/Right` clamped, `moveToStart/End`, `moveToWordBack/Forward` semantics); editing (`insert` advances by grapheme count and NFC-normalises; `deleteBack`/`deleteForward` remove one grapheme and return the same reference at a boundary; `clear`; `replaceAll` puts the cursor at the end); kills returning `[cursor, killedText]` tuples; `beforeSegs`/`afterSegs`.
- **`colors.test.ts`** — `applyTuiPalette` preserves object identity while swapping values; derived lookups (`getTaskStatusColor`, `getGoalStatusColor`, `getAgentColors()[0]`) refresh after a switch; `remapTuiColor` round-trips semantic tokens, passes unknown hexes through, and keeps light's `gray`/`ghost` distinct; concrete light values (`white '#1d2939'`, `ghost '#667085'`, `alternatingRowBg '#f8fafc'`); a WCAG ≥ 4.5:1 contrast invariant over `['amber','green','red','blue']` against `solidText` in the light palette.
- **`entity-list-changed.test.ts`** — returns `true` when a timestamp-less entity (an agent) changed any field; returns `false` for structurally identical fresh object instances. So the comparison must be deep, with no reference-equality shortcut.
- **`wizard-validate-functions.test.ts`** — every validator's exact strings and trim behaviour (see §5.3), self-exclusion in the edit-agent name check, and `undefined` collections not crashing.
- **`wizard-configs-team.test.ts`** — team option order `[None, ...active teams]`; `defaultValue` from membership; `skip` when `teamOptions.length <= 1`; only `active` teams listed; hint `{n} members`; the `team: '' → team_id: undefined` mapping; the leave/join decision matrix.
- **`wizard-effort.test.ts`** — the effort step exists with id `effort`, sits at `modelIdx + 1`, skips for codex/shell/opencode/cursor and not for claude/pi/grok, has exactly the 4 values `['', 'high', 'medium', 'low']`; the create mapper drops `''` while the edit mapper preserves it.
- **`wizard-shell.test.ts`** — shell reduces the create wizard to exactly `['name','adapter','command']` even when teams exist; the command step's `required`, `description` and validation; the 4th positional parameter is the default adapter; `agentWizardToInput` strips all AI-only values, trims the command and **forces `approval_policy: 'auto'`**; the edit wizard pre-fills from `config.command` and switching back to an AI adapter sets `command: ''`.
- **`wizard-agent-hint.test.ts`** — assignee hint format `[{adapter}] {first line of role}`; bare `{adapter}` when the role is absent or empty; first line trimmed; truncation at 60 chars to 59 + `…`; exactly 60 not truncated; disabled agents excluded.
- **`wizard-model-catalog.test.ts`** — the model step is `type: 'text'` with `suggestionMode: 'value'`; catalog entries returned verbatim; the empty-catalog fallback includes a `''` sentinel; the unknown-adapter fallback is exactly one `Default` option; suggestions follow the **newly chosen** adapter; `getDefaultValue` clears the model when the adapter changes; custom ids pass through both mappers.
- **`wizard-palette.test.ts`** — every config setting is a 1-step wizard; the kebab→snake id mapping; palette option order `['amber','ocean','forest','violet','light']`; `defaultValue` = the current palette.
- **`onboarding-agents.test.ts`** — the Agents onboarding description must name every `SUPPORTED_ADAPTERS` entry and stay ≤3 lines.
- **`help-overlay.test.tsx`** (the non-render blocks) — `resolveSuggestions` separators and grouping, prefix filtering with no separators, subcommand mode, `subs` presence, the `[id]` args hint baked into `/run`; `resolveCompletion`'s 8 cases; `CommandHistory` push/dedupe/prev/next/reset semantics.

### 11.2 Behavioural, needing a render harness
- **Sorting** — tasks by status rank (`in_progress` before `todo` before `done`); agents by team then status; goals by status rank.
- **Status glyphs** — `in_progress` → braille frame 0 `⠋`; `todo` `○`; `done` `✓`; `failed` `✕`; `review` `◈`; `cancelled` `─`; non-running time column `—`; priority 1 `!!!`, 2 `!!`.
- **Caps** — 500 messages with the feed header saturating at `500/500` and the oldest evicted; a 500-entry runId LRU surviving 600 unique runIds; 2048-char details with the feed summary at `slice(0,150)` and the whole rendered frame staying under 8 KB for a 10 KB error; 5 toasts in state / 2 rendered (10 done events keep tasks 5–9 and render 5 and 6); the 10-task limit with the `S show all` hint appearing at 11 but not at 10; the tab badge only when tasks are hidden.
- **Batching** — with `messageBatchMs: 80`, 100 synchronously emitted events are invisible before the timer fires and all land in one flush (`100/100`); immediate mode drains the pending queue completely (`200/200`).
- **Endurance** — 5000 events round-robin across all `formatEvent` branches leaves the app rendering with the feed capped.
- **History** — the TUI paints before the history promise resolves; `done` → `Completed`; a raw error string renders verbatim; legacy Codex `{"type":"agent_message","text":...}` renders the `text` and **never** `[agent_message]`; nested provider errors unwrap to the innermost message and render **exactly once** per run with no `invalid_request_error` leak; the `text` activity preset hides `[thread.started]`-style lifecycle events; an empty batch leaves `Waiting for activity` intact; two batches accumulate.
- **Soft delete** — `d` schedules without calling the delete callback; `z` and `Z` both restore and emit `restored`; execution lands after ~6.2 s of fake time; multiple deletions coexist and `z` undoes only the most recent; `in_progress` tasks show no banner and never call the callback; a running agent carries `needsForceStop` and neither `onForceStopAgent` nor `onDeleteAgent` fires while pending.
- **Detail resize** — the hints `+/- resize` + `M max` vs `+/- exit max`; `M` toggles; `+`, `=` and `-` all exit maximize; the logs view ignores all three and shows neither hint; `isDetailMaximized` survives tab switches and reopening; three consecutive `+` or two consecutive `-` never break rendering.
- **Error hints** — the row shows the mapped hint and not the raw message for a known kind; an unknown kind falls back to the raw message (truncated by a 22-char role column); the panel shows `⚠ Error`, `hint.message`, `hint.fix`, and `orch doctor` only when `doctorHint` (absent for `PROCESS_CRASH`, present for `ADAPTER_NOT_FOUND` and `UNKNOWN`); `UNKNOWN` also shows the raw message while `AUTH_FAILED` does not; a relative `… ago`; nothing at all when there is no `last_error`.
- **Goal/task linking** — `⊕` only for a resolvable `goalId` (none for a missing goal or a missing map), with a 13-char title cap; the progress bar renders `█`, `░` and `2/3`, is hidden entirely at 0 tasks, and counts only `done`; section headers uppercase the title with `{n} task|tasks` and an integer `% done` (0 %, 50 %, 100 %, and singular `1 task`); `UNGROUPED` likewise; `g` toggles grouping on tasks only and still navigates to GOALS from other views.
- **Command palette** — exact confirmation substrings (`Cancelled`, `Retried`, `Assigned`, `Dispatched all`, `Disabled`, `Enabled`, `Approved`, `Rejected`, `Unknown`, `Usage`, and a rejected handler's message verbatim); tab completion `/ta` + Tab → `/task`; ghost completion `he` → `lp`; `↑` recalls the previous command; `/config palette ocean` calls `onSavePalette('ocean')` and shows `Palette: ocean` while `/config palette` opens a `step 1/1` wizard titled `SETTINGS — PALETTE` (and must not show `step 1/5`); `/config` + Enter opens that same wizard from the highlighted suggestion; `/task add My new task` passes the whole rest of the line; `/agent add runner shell npm run test:unit` calls `onAddAgent('runner','shell',{command:'npm run test:unit',approval_policy:'auto'})`; `/agent add runner` with `defaultAdapter:'shell'` opens a prefilled `NEW AGENT` wizard instead of creating; `/help` lists `/quit`, `watch` and `pause`; `/status` shows `1 running`, `2 tasks`, `1 agents`; `/task list` shows ids.
- **View switching** — `Tab` cycles tasks→agents→logs→goals→tasks; `←` from tasks reaches GOALS, `→` reaches AGENTS; the detail panel blocks switching; a wizard swallows global hotkeys including `q`.
- **Pickers** — wrap-around navigation in both; the agent picker treats an empty set as "all" (`3/3 selected`), collapses the implicit-all state to a single explicit selection on `Space`, resets to all on `a`, and **confirms an empty set as empty**; the type picker's 8 labels, the five numeric presets with their exact selected counts, `a` toggling all↔none, and **confirming an empty draft as all 8**; `·{count}` badges; exact footer strings; `Esc` cancels without confirming in both.
- **Logs status bar** — `l` shows `ACTIONS`; with no filters the bar contains neither `agent:` nor `type:`; `F` produces an `f:`-prefixed marker; `a` → `Space` → `Enter` produces an `agents` count; any active filter also shows an `events` count.
- **Onboarding** — the 5-value step enum in order; `WelcomeScreen` / `OnboardingNudge` / `OnboardingToast` contents; all five initial-state derivations; the three event transitions; `completed → dismissed` after exactly 5 s with exactly one `onCompleteOnboarding()`; no re-entry from `dismissed`.
- **Toasts / bell / flash** — per-type icon, title and message; `toast: false` suppresses; the 5-queue / 2-render caps with the correct eviction window; non-terminal transitions produce no toast; the bell fires for failed and review but not done and not when disabled; an inactive tab renders `T tasks` and flashes to `T TASKS` only when all three flash props are present, never on the active tab, with `onFlashComplete` after ~15 tick advances.
- **Wizard** — type-dependent confirm keys (Enter/Tab for text and select; Ctrl+Enter or Tab for textarea, where plain Enter and Shift+Enter insert newlines); submitted values trimmed; `\r\n` paste normalised; the two independent validation gates (required-empty vs `validate`); the 300 ms debounce proven with fake timers (so `validate` is not called per keystroke); synchronous select validation; re-validation on confirm defeating a stale debounced error; `Fix the error above`; a red round border matching `/[╭╮╰╯─│]/`; errors cleared on both forward and backward navigation; textarea arrow/wrap navigation, the backspace-merge cursor column set synchronously, and backspace never navigating back; multiline paste in the middle of existing text; `Ctrl+I` and `Ctrl+V` firing `onPasteImage` on text and textarea but not select, and no-op when the prop is absent; `footerExtra` `📎2` rendering and `📎` being absent otherwise; suggestion filtering by typed text with `↓` + `Enter` submitting the suggestion's **value**; a live catalog push re-rendering an already-open wizard.
- **Agent creation via the UI** — `a` then `n` → `Agent name`; `7 × j` on the provider step lands on `shell`; the shell path shows `Command` and `exit 0 = done` and skips `Model`; completing calls `onAddAgent('Test Runner','shell',{command:'npm test',approval_policy:'auto'})`.
- **Attachments** — `📎` plus a count on the row when attachments exist and nothing when absent or empty; the detail panel lists an `attachments` section with the filenames, and omits the word entirely when there are none.
- **Agent detail** — the `skills` row with a `, `-joined list, absent for `{}` and `[]`; shell agents showing `command` instead of `model`; editing a shell agent calling `onUpdateAgent` once and **never** `onLeaveTeam`.
- **Goal detail** — the goal id; the status uppercased; the assignee resolved to the agent name, `—` when absent; `created_at` as a date only; `Updated` only when it differs; the full multi-line description and the full progress report with no 3-line cap; all linked tasks and no unrelated ones; `No description`; the header `GOAL:` and `Esc` closing it.
