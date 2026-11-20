# Reads affected by storage ordering and pagination

Audit base: `fb6a1e1` (1.0.0). Paths below are repository-relative; function names
are the lookup key, so the inventory survives line shifts. Scope: production engine,
daemon operation handlers, CLI, TUI, browser frontend and MCP. Test fakes and static
adapter/model catalogues are excluded. The daemon is included because current
frontends read through its operations rather than directly opening file stores.

**O** means observable order matters; **A** means the caller assumes all matching
records are present. Most presentation reads have both properties. A caller that
sorts is still O when its stable sort preserves input order on ties. Pagination
must happen after filtering and global ordering, or be fully drained before these
existing computations. Never replace one complete list with one default page.

## Engine

Paths in this table start at `crates/orch-engine/src/`.

| File | Function(s) | Dependency and port action |
|---|---|---|
| `agent_service.rs` | `list`, `find_best_agent`, `find_best_in` | O/A: idle-agent scoring uses stable score ties in agent list order; explicit assignee resolution also needs the full directory. Preserve scheduler rotation with an explicit scheduling ordinal. |
| `agent_service.rs` | `create_against`, `update_against`, `replace_definition_against` | A/first match: `get_by_name` currently scans the full agent list for conflict checks; use an explicit name query, not a default-page scan. |
| `task_service.rs` | `list`, `resolve_assignee` | O/A: list is returned intact; name/id resolution scans all local agents before remote resolution. |
| `goal_service.rs` | `list`, `list_tasks_for_goal` | O/A: goal/task arrays are returned intact. Query linked tasks in task display order. |
| `goal_service.rs` | `enforce_achieved_guard` | A: every linked task must be terminal; a truncated list can approve an unfinished goal. |
| `goal_service.rs` | `has_active_goal_for_agent`, `cancel_pending_autonomous_tasks` | A: existence and cancellation over all matching goals/tasks; use explicit existence/filter queries or drain pages. |
| `message_service.rs` | `broadcast` | O/A: broadcasts iterate every local agent, then remote recipients; omission loses recipients. |
| `message_service.rs` | `drain_mailbox`, `mark_snapshot_delivered` | O/A: every pending recipient message in creation/index-tie order; delivery may mutate list positions, so do not page by mutable offsets. |
| `message_service.rs` | `list_all`, `list_pending_for_agent`, `list_for_agent` | O/A: return complete creation-ordered message arrays, filtering sent/received records locally. |
| `team_service.rs` | `list`, `find_team_for_agent` | A; `find_team_for_agent` picks the first matching active team. That first match currently follows unspecified filesystem order: do not label it a guaranteed index order. Settle an explicit rule if multiple teams match. |
| `team_service.rs` | `create` | A: `get_by_name` scans every team for a name conflict. |
| `run_service.rs` | `list_all`, `list_for_task`, `list_for_agent` | O/A: complete newest-started-first arrays. |
| `run_service.rs` | `get_last_failed_run_context` | O/A: filters all task runs, stable-sorts by finish time descending, picks first, reads an ordered bounded event tail. Preserve start-order tie input; do not apply a start-time page before the finish-time query. |
| `run_service.rs` | `read_events`, `read_events_tail` | O: append order and chronological-position tail, not sorting by event timestamp. |
| `container.rs` | `Engine::daemon_initialize` | A: warms/rebuilds tasks, agents, runs, goals, teams and messages before startup; paged initialization must not leave a partial cache. |
| `container/tracked.rs` | `Tracked<dyn TaskStore>::list`, `Tracked<dyn AgentStore>::list`, `Tracked<dyn GoalStore>::list`, `Tracked<dyn TeamStore>::list` | O/A: transparent full-list forwarding; update the wrappers with the port. |
| `container/tracked.rs` | `Tracked<dyn RunStore>::{list_all,list_for_task,list_for_agent,read_events,read_events_tail,stream_events}` | O/A: transparent run/event forwarding. |
| `container/tracked.rs` | `Tracked<dyn ContextStore>::{list,get_all}`, `Tracked<dyn MessageStore>::{list,list_pending}` | O/A: forwarding preserves key/message ordering and completeness. |
| `prompt_mode.rs` | `update_project_prompt_mode` | A: rejects incompatible templates across every local agent; a page can miss an offender. |
| `org_templates.rs` | `preflight` | A: checks conflicts against all existing agent names before deployment. |
| `orchestrator/tick.rs` | `tick_body` | O/A: all tasks feed scheduling and dependency maps. |
| `orchestrator/tick.rs` | `reconcile` | A: remote registry list must include every peer used to identify abandoned remote tasks. |
| `orchestrator/tick.rs` | `reconcile_stale_agents` | A: scans all agents for stale busy state. |
| `orchestrator/tick.rs` | `reconcile_orphaned_tasks` | O/A: all tasks and each task's newest-first runs; remote mirrors select maximum attempt (last encountered on a tie). |
| `orchestrator/tick.rs` | `seed_autonomous_tasks` | O/A: all agents, all existing tasks, active goals; loop order and existence tests control autonomous task creation. |
| `orchestrator/dispatch.rs` | `dispatch_agent_directory`, `has_idle_remote_agent` | O/A: full local/remote directory, local order carried into selection. |
| `orchestrator/dispatch.rs` | `dispatch_all`, `dispatch_only_task` | O/A: all tasks before readiness/dependency checks, priority sort and dispatch iteration. Stable task ties retain store order. |
| `orchestrator/dispatch.rs` | `record_dispatch_failure` | A: complete task graph for failure cascade. |
| `orchestrator/dispatch.rs` | `dispatch_claimed` | O/A: merged agent directory and all claimed-task candidates; first highest-scoring idle agent wins. |
| `orchestrator/dispatch.rs` | `prepare_prompt` | O/A: context `get_all`, goal tasks and caller-supplied full roster feed prompts. Context key order and task order are visible; relevance/preview limits apply only after complete input. |
| `orchestrator/collect.rs` | `handle_run_failure_locked` | A: complete task graph for dependent failure propagation. |
| `orchestrator/daemon.rs` | `daemon_tick_with_guard_inner` | O/A: full task lists for draining/filtering and priority order, including a fresh list during the drain. |
| `orchestrator/daemon.rs` | `daemon_cancel_task`, `release_preparation_locked` | A: all task runs, including preparing runs, must be found and stopped/released. |
| `orchestrator/daemon.rs` | `daemon_launch_task_with_guard`, `daemon_task_eligible` | A: full dependency task set; page omissions can change launch eligibility. |
| `orchestrator/daemon/startup.rs` | `daemon_initialize` | A: scans all runs to reconcile persisted active runs. |
| `orchestrator/daemon/shutdown.rs` | `daemon_stop_with_spawn_seal` | A: scans all runs to stop/terminalize active work. |
| `orchestrator/recovery.rs` | `quarantine_worker_mirrors` | A: all tasks/runs must be considered, not one page. |
| `orchestrator/recovery.rs` | `cleanup_stale_running_entries` | A: complete remote registry and all tasks for cascade repair. Also iterates the insertion-ordered running map. |
| `orchestrator/recovery.rs` | `cleanup_orphaned_preparing_runs`, `cleanup_orphaned_running_runs` | A: scan every run to repair orphans. |
| `orchestrator/remote.rs` | `rehydrate_remote_runs` | A: full remote agent directory needed to reconnect running entries. |

`orch-infra` supplies two important upstream dependencies: `storage/cached.rs`
(`CachedTaskStore::list`, `CachedAgentStore::list`, `CachedGoalStore::list`) caches
whole lists; `storage/remote.rs` (`RemoteAgentDirectory::list`, `resolve`) merges
local list order with remote snapshots and resolves first matches. Port these
alongside the engine. `template/context.rs::filter_relevant_context` consumes the
ordered complete context before truncating; `template/eco.rs::roster` sorts a full
roster by ID, while `goal_preview` selects from the complete goal-task set.

## Daemon operations and streams

Paths start at `crates/orch-daemon/src/`.

| File | Function(s) | Dependency and port action |
|---|---|---|
| `ops/core.rs` | `status_get`, `ui_bootstrap` | O/A: full task/agent status arrays; bootstrap roster used by UI. |
| `ops/core.rs` | `task_list`, `agent_list`, `goal_list`, `context_list`, `team_list` | O/A: public array responses; agent list merges remote snapshots. Teams have no specified order. |
| `ops/core.rs` | `goal_get`, `goal_tasks` | O/A: linked tasks assume the entire goal graph. |
| `ops/core.rs` | `message_list`, `message_inbox` | O/A: complete sent/received or pending arrays; creation time plus index-tie ordering. |
| `ops/core.rs` | `run_list` | O/A: reads all runs, filters task/agent/time, stable-sorts started time descending, then truncates optional limit. Push filter/order/limit down together. |
| `ops/core.rs` | `run_events`, `run_events_tail` | O: append-order arrays; tail count is applied to append position. |
| `ops/core.rs` | `team_for_agent` | A/first match: inherits `find_team_for_agent`'s unspecified team enumeration tie. |
| `ops/peripheral.rs` | `run_history` | O/A: all runs, terminal filter, newest 3 then next 7, last 30 events per run, stable timestamp sort and last 200 per batch. A page before terminal filtering changes history. |
| `ops/peripheral.rs` | `run_timings` | A: all runs/tasks/goals plus whole state feed timing derivation. |
| `ops/peripheral.rs` | `remote_list`, `remote_status` | A: complete peer registry for display/status. Remote records are file configuration outside today's trait list; do not omit them from the migration audit. |
| `ops/peripheral.rs` | `org_template_list`, `template_list`, `org_template_agent_list` | O/A: directory/bundle enumeration, shipped/user template precedence and agent arrays. These are external template sources, not an indexed project store. |
| `ops/authored.rs` | `validate_prompt_candidate` | A: checks all agents against an authored prompt candidate. |
| `ops/control.rs` | `read_tasks`, `read_execution_snapshot`, `observe_runs` | O/A: complete tasks/runs feed execution observation and batch control; replacing them with a page can omit active sources. |

`ops/control.rs::logs_follow` reads the event bus and resolves missing selectors
with single-run gets; it is not a full-list reader. State readers throughout orchestrator startup, tick, dispatch,
collection, cancellation, recovery and shutdown read all `running`, `claimed` and
`retry_queue` entries: replacing StateStore requires explicit complete queries,
not a default page. Retry enqueue drops the oldest entry at capacity (100); this
is engine policy rather than a StateStore write constraint.

## CLI

Paths start at `crates/orch-cli/src/`.

| File | Function(s) | Dependency and port action |
|---|---|---|
| `commands/status.rs` | `run` | O/A: complete status arrays for JSON and counts/rendering. |
| `commands/task.rs` | `list`, `show` | O/A: task array and all-agent name resolution. `list` filters and renders the returned task set. |
| `commands/agent.rs` | `list`, `status` | O/A: roster order/counts and status projection; status reads the agent view. |
| `commands/goal.rs` | `list`, `show` | O/A: all goals and linked tasks; goal totals are not page lengths. |
| `commands/team.rs` | `list`, `show` | A: complete team roster and member/task detail arrays; no defined team list order. |
| `commands/context.rs` | `list` | O/A: every live context entry in key order. |
| `commands/msg.rs` | `inbox`, `list` | O/A: complete creation-ordered pending/history arrays. |
| `commands/run.rs` | `log_startup_diagnostics`, `doctor` | A: all agents/status tasks for diagnostics; missing a page can hide incompatibilities. |
| `commands/run/logs.rs` | `RunReads::{list_all,list_for_task,list_for_agent,read_events_tail}` | O/A: typed RPC wrappers preserve complete vectors and ordered event tails. |
| `commands/run/logs.rs` | `show_run_logs` | O: last 50 events, or last 500 before since-filtering. |
| `commands/run/logs.rs` | `show_task_logs` | O/A: all task runs; with since selects **last** 20 of newest-first input, then ordered event tails. Preserve current suffix behaviour, even though it is not newest 20. |
| `commands/run/logs.rs` | `show_agent_logs` | O/A: **last** five of newest-first agent runs, then last events per run. |
| `commands/run/logs.rs` | `show_all_recent_logs` | O/A: all runs, overlap filtering, total count, **first** 20, then ordered event tails. |

## TUI and browser

| File | Function(s) | Dependency and port action |
|---|---|---|
| `crates/orch-cli/src/tui/backend.rs` | `refresh_tasks`, `refresh_agents`, `refresh_goals`, `list_teams`, `refresh_state` | O/A: complete replacement vectors/documents. A page must not overwrite a previously complete UI cache. |
| `crates/orch-cli/src/tui/backend.rs` | `refresh_timings`, `spawn_history_load`, `poll_history` | O/A: whole timing snapshot and ordered bounded history batches from daemon. |
| `crates/orch-tui/src/app.rs` | `new`, `refresh_all`, `refresh_views`, `refresh_timings`, `refresh_open_agent_wizard` | O/A: full backend replacement vectors populate views, grouping, pickers and counts; wizard refresh needs the full roster. |
| `crates/orch-tui/src/state.rs` | `order_goals`, `order_tasks`, `order_agents`, `sorted_agents` | O/A: stable local sorts, grouping and derived sort keys consume store order on ties. |
| `crates/orch-tui/src/state.rs` | `tasks_of_goal`, `resolved_goal_id`, `keyword_counts`, `task_rows`, `agent_rows`, `team_of` | A: complete relationships, counts and row construction; missing pages change badges, groups and totals. |
| `crates/orch-tui/src/state.rs` | `reconcile_list_selection`, `visible_tasks`, `sorted_goal_tasks` | O/A: selections and visible slices depend on full globally ordered entity arrays. |
| `crates/orch-tui/src/backend/mod.rs` | `derive_timings` | A: all runs/tasks/goals and running entries used for timing evidence and aggregation. |
| `crates/orch-cli/src/web/mod.rs`, `web/pty.rs` | `router`, `PtySession::spawn` | Browser renders the PTY TUI, so it inherits all TUI dependencies above; there is no separate browser entity-list reader. |
| `crates/orch-cli/src/web/observation.rs` | `subscribe`, `observe` | No entity-order dependency: observes liveness only, discards store changes. Listed to prevent inventing a second browser cache/pagination path. |

## MCP

Paths start at `crates/orch-cli/src/mcp/`.

| File | Function(s) | Dependency and port action |
|---|---|---|
| `projections.rs` | `status`, `tasks`, `goals`, `goal`, `agents`, `context`, `inbox` | O/A: JSON arrays match CLI shapes and ordering; goal includes every linked task; context hides expiry without deletion. |
| `resources.rs` | `read` | O/A: routes collection and goal-detail resources through those projections. |
| `tools/task.rs`, `tools/goal.rs`, `tools/agent.rs`, `tools/context.rs`, `tools/msg.rs` | `list` (each), `show` in goal, `inbox` in msg | O/A: projection consumers; tool and resource pagination must agree. |
| `tools/msg.rs` | `list` | O/A: direct MessageListArgs read, preserving sent/received creation order. |
| `tools/team.rs` | `list`, `show` | A: complete team collection and detail arrays; unspecified team enumeration. |
| `tools/logs.rs` | `logs` | O/A: ordered event tail; task/agent/since routes preserve CLI's suffix/prefix limits over full run lists. |

## Spot checks and audit method

Five useful review anchors: `AgentService::find_best_in` uses stable scoring;
`FileTaskStore::list` sorts status/time with stable ties;
`RunService::get_last_failed_run_context` re-sorts a full task-run list;
`commands/run/logs.rs::show_agent_logs` takes a suffix of newest-first input;
`ops/peripheral.rs::run_history` filters before selecting 3/7 batches.

The audit followed both sides of each boundary: storage-list/event methods,
engine service and cached/tracked wrappers, daemon typed operation handlers,
then typed RPC request sites and local vector consumers. To re-audit after a
port, search all production sources (including multiline receiver chains) for
`list`, `list_all`, `list_for_task`, `list_for_agent`, `list_pending`, `get_all`,
`read_events`, `read_events_tail`, `stream_events`, `*ListArgs`, `StatusGetArgs`,
`GoalGetArgs`, `RunHistoryArgs` and `RunTimingsArgs`. Exclude test modules and
catalogues only after reading the call. Do not stop at direct store calls: TUI,
web and MCP intentionally do not have those.

Event reads are ordered, but not an unbounded completeness promise: current
file `read_events`/`stream_events` read a bounded tail above 50 MiB and skip
malformed/oversize lines. Retain or explicitly replace those documented bounds
when moving events; do not describe a bounded legacy read as every stored event.
