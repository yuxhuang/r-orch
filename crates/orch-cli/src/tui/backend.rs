//! [`EngineBackend`] — [`TuiBackend`] over [`FullEngine`].
//!
//! Every method here is the Rust of a closure in
//! `parity/orch-ts/src/cli/commands/tui.ts`; the line references in the
//! doc comments point at it. The trait and its option structs belong to
//! `orch-tui` (`crates/orch-tui/BACKEND.md`) — this is the only
//! implementation outside that crate's `FakeBackend`.
//!
//! Two rules hold throughout:
//!
//! 1. **Errors are strings.** The TUI renders `Failed: {err}`, so every
//!    fallible call collapses to [`BackendResult`]. Exit codes are a CLI
//!    concept and have no meaning in a dashboard.
//! 2. **An empty option string means "absent".** The TS writes
//!    `opts?.command || undefined`, and JS treats `""` as falsy. The one
//!    exception is [`AgentFields`], where `Some("")` means *clear the stored
//!    value* — the engine's `UpdateAgentFields` has the same three states.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use orch_core::storage::{GoalFilter, TaskFilter};
use orch_domain::{
    Agent, CreateAgentInput, CreateGoalInput, CreateTaskInput, CreateTeamInput, Goal, GoalStatus,
    NotificationPreferences, OrchestratorEvent, OrchestratorState, ReasoningEffort, Run, RunStatus,
    Task, TaskStatus, Team, TuiPaletteName,
};
use orch_engine::{
    FullEngine, UpdateAgentFields, UpdateGoalFields, UpdateTaskFields, WatchOptions,
};
use orch_tui::backend::{
    AgentFields, AgentOpts, BackendResult, ClipboardPaste, CreateTaskOpts, GoalFields,
    HistoryEntry, LaunchContext, ModelCatalog, ModelOption, TaskFields, TuiBackend,
};
use tokio::runtime::Handle;

/// Runs whose events the history loader reads, and the size of the first
/// batch (`tui.ts:145-146`).
const HISTORY_FIRST_BATCH: usize = 3;
const HISTORY_TOTAL_RUNS: usize = 10;
/// Trailing events read per run (`tui.ts:151`).
const HISTORY_EVENTS_PER_RUN: usize = 30;
/// Each delivered batch is capped at its last N entries (`tui.ts:166`).
const HISTORY_BATCH_CAP: usize = 200;

/// Where the model-catalog probe has got to.
///
/// `Loading` and `Delivered` are the same observable state — `poll` yields
/// nothing — but keeping them apart is what stops a second probe being
/// started after the catalog has been handed over.
#[derive(Debug)]
enum CatalogState {
    NotStarted,
    Loading,
    Ready(ModelCatalog),
    Delivered,
}

/// Events waiting for the next frame.
///
/// One queue for both sources. In watch mode it is fed by draining the event
/// bus; in observer mode a `DiskObserver` handler pushes into it. The two
/// never overlap — an in-process orchestrator and an external one are mutually
/// exclusive — so the dashboard cannot tell them apart.
#[derive(Debug, Default)]
struct EventQueue(Mutex<VecDeque<OrchestratorEvent>>);

impl EventQueue {
    fn push(&self, event: OrchestratorEvent) {
        self.lock().push_back(event);
    }

    fn drain(&self) -> Vec<OrchestratorEvent> {
        self.lock().drain(..).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<OrchestratorEvent>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The engine, driven from a synchronous caller.
///
/// Construct it on the runtime (so `Handle::current()` resolves), then move it
/// to the thread that owns the draw loop. See the module docs for why that
/// thread must not be a runtime thread.
pub struct EngineBackend {
    engine: Arc<FullEngine>,
    handle: Handle,
    context: LaunchContext,
    /// `None` in observer mode, where there is no in-process bus traffic.
    subscription: Option<orch_engine::EventSubscription>,
    queue: Arc<EventQueue>,
    history: Arc<Mutex<VecDeque<Vec<HistoryEntry>>>>,
    catalog: Arc<Mutex<CatalogState>>,
    watch_active: bool,
}

impl std::fmt::Debug for EngineBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBackend")
            .field("project", &self.context.project_name)
            .field("observer_mode", &self.context.observer_mode)
            .field("watch_active", &self.watch_active)
            .finish_non_exhaustive()
    }
}

impl EngineBackend {
    /// Launch, following `tui.ts:239-268`: try watch mode, and on refusal fall
    /// back to observer mode.
    ///
    /// Must be called from the runtime. `version` is the CLI's own version
    /// string, which the dashboard shows in its header.
    pub async fn launch(engine: Arc<FullEngine>, version: impl Into<String>) -> EngineBackend {
        // Subscribe *before* starting watch mode: a broadcast receiver sees
        // nothing emitted earlier, so the first tick's events would be lost.
        let subscription = engine.events.subscribe();
        // The dashboard attaches a handler per view; the default of 10 would
        // warn for no reason.
        engine.events.set_max_listeners(0);

        let global = &engine.global_config;
        let mut context = LaunchContext {
            project_name: engine.config.project.name.clone(),
            version: version.into(),
            default_adapter: engine.config.defaults.agent.adapter.clone(),
            initial_palette: global.tui.palette,
            initial_activity_filter: global.tui.activity_filter.as_str().to_owned(),
            initial_notifications: global.tui.notifications,
            initial_max_concurrent: engine.config.scheduling.max_concurrent_agents as u32,
            initial_watch_active: false,
            observer_mode: false,
            watch_error: None,
        };

        // Watch mode auto-starts so the orchestrator is live, with seeding on —
        // `startWatch()` with no options (`tui.ts:254`).
        let watch_active = match engine
            .orchestrator
            .start_watch(WatchOptions::watching())
            .await
        {
            Ok(()) => true,
            Err(err) => {
                // Another process holds the lock. The dashboard becomes a
                // read-only observer of *their* orchestrator, and the message
                // is **suppressed**: the TS passes
                // `observerMode ? undefined : watchError` (`tui.ts:314`), so
                // the header shows the observer badge instead of an error. It
                // is logged rather than dropped, since otherwise nothing tells
                // the user which PID holds the lock.
                tracing::info!(%err, "watch mode refused; entering observer mode");
                context.observer_mode = true;
                false
            }
        };
        context.initial_watch_active = watch_active;

        let backend = EngineBackend {
            engine,
            handle: Handle::current(),
            context,
            subscription: Some(subscription),
            queue: Arc::new(EventQueue::default()),
            history: Arc::new(Mutex::new(VecDeque::new())),
            catalog: Arc::new(Mutex::new(CatalogState::NotStarted)),
            watch_active,
        };
        // History is loaded eagerly because the dashboard's activity view
        // wants it on the first frame. The model catalog is **not**: probing
        // eight adapter CLIs costs eight process spawns, and the TS only pays
        // that when a wizard with a model step opens — so it starts on the
        // first `poll_model_catalog`.
        backend.spawn_history_load();
        backend
    }

    pub fn is_watch_active(&self) -> bool {
        self.watch_active
    }

    /// Hand an event from an external source — the `DiskObserver` — to the
    /// same queue `poll_events` drains. The observer's handler closes over the
    /// queue returned here.
    pub fn observer_sink(&self) -> Arc<dyn Fn(OrchestratorEvent) + Send + Sync> {
        let queue = Arc::clone(&self.queue);
        Arc::new(move |event| queue.push(event))
    }

    /// Push one externally observed event. The simple form of
    /// [`EngineBackend::observer_sink`], for a caller that already holds
    /// `&self`.
    pub fn push_observed_event(&self, event: OrchestratorEvent) {
        self.queue.push(event);
    }

    /// Run a short call to completion. Never called from a runtime thread —
    /// see the module docs.
    fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.handle.block_on(future)
    }

    /// Report a background failure the way the dashboard can see it: the TS
    /// callbacks `await` and surface a rejection, but a spawned call has no
    /// caller left to reject to, so it goes on the bus instead.
    fn report(engine: &FullEngine, err: impl std::fmt::Display, context: &str) {
        engine.events.emit(OrchestratorEvent::OrchestratorError {
            error: err.to_string(),
            context: context.to_owned(),
            fatal: false,
        });
    }

    /// Read-modify-write: the engine's `GlobalConfigStore` is whole-document,
    /// where the TS store has a per-key `set`.
    fn mutate_global(
        &self,
        apply: impl FnOnce(&mut orch_domain::GlobalConfig),
    ) -> BackendResult<()> {
        self.block_on(async {
            let mut global = self.engine.global_config_store.read().await?;
            apply(&mut global);
            self.engine.global_config_store.write(&global).await
        })
        .map_err(to_message)
    }

    // ── Background loads ─────────────────────────────────────────────────

    /// `onLoadHistory` (`tui.ts:133-175`): all runs, newest first, keeping only
    /// the succeeded and failed ones; the first three are delivered as one
    /// batch and the next seven as a second, each sorted oldest-first and
    /// capped at its last 200 entries.
    fn spawn_history_load(&self) {
        let engine = Arc::clone(&self.engine);
        let history = Arc::clone(&self.history);
        self.handle.spawn(async move {
            let Ok(mut runs) = engine.run_service.list_all().await else {
                return;
            };
            // Descending by start time. Stable, so runs sharing a timestamp
            // keep store order.
            runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            runs.retain(|run| matches!(run.status, RunStatus::Succeeded | RunStatus::Failed));

            let first = runs.iter().take(HISTORY_FIRST_BATCH);
            let rest = runs
                .iter()
                .skip(HISTORY_FIRST_BATCH)
                .take(HISTORY_TOTAL_RUNS - HISTORY_FIRST_BATCH);
            for batch in [first.collect::<Vec<_>>(), rest.collect::<Vec<_>>()] {
                if batch.is_empty() {
                    continue;
                }
                let mut entries = Vec::new();
                for run in batch {
                    entries.extend(history_entries(&engine, run).await);
                }
                // Oldest first, then keep the tail.
                entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                if entries.len() > HISTORY_BATCH_CAP {
                    entries.drain(..entries.len() - HISTORY_BATCH_CAP);
                }
                history
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push_back(entries);
            }
        });
    }

    /// `onLoadModelCatalog` (`tui.ts:227`): probe every supported adapter and
    /// publish the catalog once they have all answered.
    fn spawn_catalog_load(&self) {
        let catalog = Arc::clone(&self.catalog);
        self.handle.spawn(async move {
            let discovered = orch_infra::models::ModelDiscovery::new()
                .load_catalog(&orch_domain::SUPPORTED_ADAPTERS, |_, _| {})
                .await;
            let converted = discovered
                .into_iter()
                .map(|(adapter, options)| {
                    (
                        adapter.as_str().to_owned(),
                        options
                            .into_iter()
                            .map(|option| ModelOption {
                                value: option.value,
                                label: option.label,
                                hint: option.hint.unwrap_or_default(),
                            })
                            .collect(),
                    )
                })
                .collect();
            *catalog
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = CatalogState::Ready(converted);
        });
    }
}

impl TuiBackend for EngineBackend {
    // Every `can_*` flag stays at its `true` default: the TS host passes
    // all ~45 callbacks unconditionally, and the one conditional prop
    // (`onPasteImage`) is gated on the open wizard's kind, which is the
    // dashboard's own state rather than anything the engine knows
    // (`App.tsx:2526`).

    fn context(&self) -> LaunchContext {
        self.context.clone()
    }

    // ── Refresh (`tui.ts:72-82`, `203`, `181`) ───────────────────────────

    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.block_on(self.engine.task_service.list(&TaskFilter::default()))
            .map_err(to_message)
    }

    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        self.block_on(self.engine.agent_service.list())
            .map_err(to_message)
    }

    fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>> {
        self.block_on(self.engine.goal_service.list(&GoalFilter::default()))
            .map_err(to_message)
    }

    /// Read straight from the store every time. In observer mode that is the
    /// *only* source — the running orchestrator is in another process — and in
    /// watch mode it is still correct, because the orchestrator persists after
    /// every phase.
    fn refresh_state(&mut self) -> BackendResult<OrchestratorState> {
        self.block_on(self.engine.state.read()).map_err(to_message)
    }

    fn list_teams(&mut self) -> BackendResult<Vec<Team>> {
        self.block_on(self.engine.team_service.list())
            .map_err(to_message)
    }

    // ── Tasks ────────────────────────────────────────────────────────────

    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        self.block_on(self.engine.task_service.create(CreateTaskInput {
            title: title.to_owned(),
            priority: opts.priority,
            description: opts.description,
            attachments: opts.attachments,
            ..Default::default()
        }))
        .map_err(to_message)
    }

    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task> {
        self.block_on(
            self.engine.task_service.update(
                id,
                UpdateTaskFields {
                    title: fields.title,
                    description: fields.description,
                    priority: fields.priority,
                    labels: None,
                    attachments: fields
                        .attachments
                        .map(|paths| paths.into_iter().map(std::path::PathBuf::from).collect()),
                },
            ),
        )
        .map_err(to_message)
    }

    fn delete_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.task_service.delete(id))
            .map_err(to_message)
    }

    /// `review → done` directly, not through an approval service — the TS
    /// calls `updateStatus(taskId, 'done')` (`tui.ts:106`).
    fn approve_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.task_service.update_status(id, TaskStatus::Done))
            .map(|_| ())
            .map_err(to_message)
    }

    fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()> {
        self.block_on(
            self.engine
                .task_service
                .reject(id, feedback.map(str::to_owned)),
        )
        .map(|_| ())
        .map_err(to_message)
    }

    fn retry_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.task_service.retry(id))
            .map(|_| ())
            .map_err(to_message)
    }

    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()> {
        self.block_on(self.engine.task_service.assign(task_id, agent_id))
            .map(|_| ())
            .map_err(to_message)
    }

    /// Spawned, not awaited: dispatching takes a lock, prepares a workspace and
    /// starts a child. A lock conflict therefore arrives as an
    /// `orchestrator:error`, not as this call's return value.
    /// Awaited, not spawned. Both of these dispatch and return — they never
    /// wait on an agent, because collection happens on a later tick — so the
    /// frame cost is bounded, and awaiting is the only way the dashboard can
    /// see a pre-dispatch failure: it has no renderer for
    /// `orchestrator:error`, so an error delivered through the event stream
    /// alone is invisible (BACKEND.md §3). A lock conflict naming the holder's
    /// PID is exactly such a failure.
    fn run_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.orchestrator.run_task(id))
            .map_err(to_message)
    }

    fn run_all(&mut self) -> BackendResult<()> {
        self.block_on(self.engine.orchestrator.run_all())
            .map_err(to_message)
    }

    /// Awaited: cancelling is bounded by the 3 s kill grace, and the dashboard
    /// wants to know whether the task really was cancelled.
    fn cancel_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.orchestrator.cancel_task(id))
            .map_err(to_message)
    }

    // ── Agents ───────────────────────────────────────────────────────────

    /// An empty `adapter` falls through to `defaults.agent.adapter`, which the
    /// engine's `AgentService::create` already does — the TS spells it out at
    /// the call site instead (`tui.ts:87`).
    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent> {
        let effort = parse_optional(opts.effort.as_deref(), "effort")?;
        let approval_policy = parse_optional(opts.approval_policy.as_deref(), "approval policy")?;
        self.block_on(self.engine.agent_service.create(CreateAgentInput {
            name: name.to_owned(),
            adapter: adapter.to_owned(),
            command: non_empty(opts.command),
            model: non_empty(opts.model),
            effort,
            role: non_empty(opts.role),
            approval_policy,
            skills: opts.skills,
            ..Default::default()
        }))
        .map_err(to_message)
    }

    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent> {
        // `Some("")` clears, so an empty string must not be parsed as a value.
        let effort = match fields.effort.as_deref() {
            None => None,
            Some("") => Some(None),
            Some(value) => Some(Some(parse_one::<ReasoningEffort>(value, "effort")?)),
        };
        self.block_on(self.engine.agent_service.update(
            id,
            UpdateAgentFields {
                name: fields.name,
                adapter: fields.adapter,
                role: fields.role,
                command: fields.command,
                model: fields.model,
                effort,
                // The TS *prop type* carries `approval_policy`, but the only
                // caller — the edit-agent wizard — never sends it
                // (`App.tsx:1242`), so the trait leaves it out. Editing it is
                // `orch agent edit`'s job.
                approval_policy: None,
            },
        ))
        .map_err(to_message)
    }

    fn delete_agent(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.agent_service.remove(id))
            .map_err(to_message)
    }

    /// Awaited: bounded by the same 3 s grace as `cancel_task`.
    fn force_stop_agent(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.orchestrator.force_stop_agent(id))
            .map_err(to_message)
    }

    fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent> {
        self.block_on(self.engine.agent_service.set_autonomous(id, enabled))
            .map_err(to_message)
    }

    fn disable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.agent_service.disable(id))
            .map(|_| ())
            .map_err(to_message)
    }

    fn enable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.agent_service.enable(id))
            .map(|_| ())
            .map_err(to_message)
    }

    // ── Teams ────────────────────────────────────────────────────────────

    fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team> {
        self.block_on(self.engine.team_service.create(input))
            .map_err(to_message)
    }

    fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.block_on(self.engine.team_service.join(team_id, agent_id))
            .map_err(to_message)
    }

    fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.block_on(self.engine.team_service.leave(team_id, agent_id))
            .map_err(to_message)
    }

    fn disband_team(&mut self, team_id: &str) -> BackendResult<()> {
        self.block_on(self.engine.team_service.disband(team_id))
            .map_err(to_message)
    }

    fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.block_on(self.engine.team_service.set_lead(team_id, agent_id))
            .map_err(to_message)
    }

    // ── Goals ────────────────────────────────────────────────────────────

    fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal> {
        self.block_on(self.engine.goal_service.create(input))
            .map_err(to_message)
    }

    fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal> {
        self.block_on(self.engine.goal_service.update(
            id,
            &UpdateGoalFields {
                title: fields.title,
                description: fields.description,
                assignee: fields.assignee,
            },
        ))
        .map_err(to_message)
    }

    fn update_goal_status(
        &mut self,
        id: &str,
        status: GoalStatus,
        force: bool,
    ) -> BackendResult<Goal> {
        self.block_on(self.engine.goal_service.update_status(id, status, force))
            .map_err(to_message)
    }

    fn delete_goal(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(self.engine.goal_service.delete(id))
            .map_err(to_message)
    }

    /// An absent progress note is **not** a failure: the key simply has not
    /// been written yet, so it reports an empty report rather than an error
    /// the dashboard would render as `Failed:`.
    fn goal_progress(&mut self, id: &str) -> BackendResult<String> {
        self.block_on(self.engine.goal_service.get_progress_report(id))
            .map(Option::unwrap_or_default)
            .map_err(to_message)
    }

    // ── Watch mode ───────────────────────────────────────────────────────

    /// Awaited for the same reason as [`EngineBackend::run_task`]: it acquires
    /// the lock, recovers stale state and runs one tick, then returns with the
    /// loop running. The failure that matters — another process holding the
    /// lock — is knowable before any agent starts, and `/watch` is the one
    /// command whose whole purpose is to report it.
    fn start_watch(&mut self) -> BackendResult<()> {
        self.block_on(
            self.engine
                .orchestrator
                .start_watch(WatchOptions::watching()),
        )
        .map_err(to_message)?;
        self.watch_active = true;
        Ok(())
    }

    /// Spawned: `stop` kills every running agent with a 10 s grace, which must
    /// not hold a frame. The dashboard learns it finished from
    /// `orchestrator:shutdown`.
    fn stop_watch(&mut self) -> BackendResult<()> {
        let engine = Arc::clone(&self.engine);
        self.handle.spawn(async move {
            if let Err(err) = engine.orchestrator.stop().await {
                EngineBackend::report(&engine, err, "tui stop watch");
            }
        });
        self.watch_active = false;
        Ok(())
    }

    // ── Settings (`tui.ts:330-350`) ──────────────────────────────────────

    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()> {
        self.mutate_global(|global| global.tui.palette = palette)
    }

    /// Stored **as given**. `ActivityFilterPreset` is a plain string and the
    /// TS writes `globalConfigStore.set('activity_filter', preset)` without
    /// validating, so a preset r-orch does not know is persisted rather than
    /// refused — the dashboard only ever sends one of its own five. An unknown
    /// one is logged so it is not silently invisible.
    fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()> {
        if !orch_domain::ACTIVITY_FILTER_PRESETS.contains(&preset) {
            tracing::warn!(preset, "storing an unrecognized TUI activity filter");
        }
        let preset = preset.to_owned();
        self.mutate_global(|global| global.tui.activity_filter = preset)
    }

    fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()> {
        self.mutate_global(|global| global.tui.notifications = notifications)
    }

    /// Writes `scheduling.max_concurrent_agents` through the config store,
    /// which deep-merges and so preserves keys r-orch does not model.
    ///
    /// A **running** orchestrator keeps its own clone of the config, so the new
    /// cap applies from the next `start_watch` — which is the TS behaviour too
    /// (it mutates only the container's copy).
    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()> {
        self.block_on(
            self.engine
                .config_store
                .set("scheduling.max_concurrent_agents", value.into()),
        )
        .map_err(to_message)
    }

    fn complete_onboarding(&mut self) -> BackendResult<()> {
        self.block_on(async {
            let mut state = self.engine.state.read().await?;
            state.onboarding_completed = Some(true);
            self.engine.state.write(&state).await
        })
        .map_err(to_message)
    }

    // ── Polled streams ───────────────────────────────────────────────────

    /// Everything since the last frame, oldest first.
    ///
    /// Lossy by design: the bus is a 4096-slot broadcast channel, so a
    /// dashboard that stalls under a flood of `agent:output` drops the oldest
    /// rather than growing without bound. The TS accepts the same trade-off
    /// through its 80 ms batching and 2 KB caps (ARCHITECTURE §6.5). A drop is
    /// logged, never silent.
    fn poll_events(&mut self) -> Vec<OrchestratorEvent> {
        if let Some(subscription) = self.subscription.as_mut() {
            for event in subscription.try_drain() {
                self.queue.push((*event).clone());
            }
        }
        self.queue.drain()
    }

    /// History batches that have arrived since the last frame.
    fn poll_history(&mut self) -> Vec<Vec<HistoryEntry>> {
        self.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect()
    }

    /// The discovered model catalog, once every adapter has answered.
    ///
    /// The first call **starts** the discovery and returns `None`; later calls
    /// return the catalog once, then `None` again. That keeps the eight
    /// adapter probes off the startup path, where the TS does not pay them
    /// either — it only calls `onLoadModelCatalog` when a wizard needs models.
    fn poll_model_catalog(&mut self) -> Option<ModelCatalog> {
        let mut state = self
            .catalog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::mem::replace(&mut *state, CatalogState::Loading) {
            CatalogState::NotStarted => {
                drop(state);
                self.spawn_catalog_load();
                None
            }
            CatalogState::Ready(catalog) => {
                *state = CatalogState::Delivered;
                Some(catalog)
            }
            // Still probing, or already delivered.
            other => {
                *state = other;
                None
            }
        }
    }

    /// Read the clipboard, writing any image to a temp file
    /// (`docs/parity/tui.md` §8.2).
    ///
    /// The directory is `mkdtemp`-style with an `orch-paste-` prefix and the
    /// file is `clipboard-{now}.{ext}`; the timestamp comes from the injected
    /// clock, so a test can predict it.
    fn paste_image(&mut self) -> BackendResult<ClipboardPaste> {
        use orch_infra::clipboard::ClipboardContentType;

        match self.block_on(orch_infra::clipboard::detect_clipboard_type()) {
            ClipboardContentType::Text => Ok(ClipboardPaste::Text),
            ClipboardContentType::Empty => Ok(ClipboardPaste::Empty),
            ClipboardContentType::Image => {
                let Some(image) = self.block_on(orch_infra::clipboard::get_clipboard_image())
                else {
                    return Err("Failed to read clipboard image".to_owned());
                };
                // `mkdtemp(join(tmpdir(), 'orch-paste-'))`, kept rather than
                // dropped: the dashboard attaches the path to a task, so the
                // file has to outlive this call.
                let dir = tempfile::Builder::new()
                    .prefix("orch-paste-")
                    .tempdir()
                    .map_err(|err: std::io::Error| err.to_string())?
                    .keep();
                let path = dir.join(format!(
                    "clipboard-{}.{}",
                    self.engine.clock.now_ms(),
                    image.ext
                ));
                let bytes = image.data.len();
                std::fs::write(&path, &image.data).map_err(|err| err.to_string())?;
                Ok(ClipboardPaste::Image {
                    path: path.display().to_string(),
                    bytes,
                })
            }
        }
    }

    /// Ring the terminal bell — a direct write outside the draw loop.
    fn bell(&mut self) {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(b"\x07");
        let _ = stdout.flush();
    }
}

/// One run's trailing events as history entries.
async fn history_entries(engine: &FullEngine, run: &Run) -> Vec<HistoryEntry> {
    // A missing events file is normal for a run that never emitted.
    let events = engine
        .run_service
        .read_events_tail(&run.id, HISTORY_EVENTS_PER_RUN)
        .await
        .unwrap_or_default();
    events
        .into_iter()
        .map(|event| HistoryEntry {
            timestamp: event.timestamp,
            run_id: run.id.clone(),
            agent_id: run.agent_id.clone(),
            task_id: run.task_id.clone(),
            event_type: event.event_type.as_str().to_owned(),
            // The orchestrator already serialized the payload to a string, so
            // this unwraps that rather than re-encoding it.
            data: match event.data {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            },
        })
        .collect()
}

/// The TUI only ever renders the message.
fn to_message(err: orch_core::error::Error) -> String {
    err.to_string()
}

/// JS `value || undefined`: an empty string is absent.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}

/// Parse an optional stringly-typed enum, treating `""` as absent.
fn parse_optional<T: std::str::FromStr>(
    value: Option<&str>,
    what: &str,
) -> BackendResult<Option<T>> {
    match value.filter(|text| !text.is_empty()) {
        None => Ok(None),
        Some(text) => parse_one(text, what).map(Some),
    }
}

fn parse_one<T: std::str::FromStr>(value: &str, what: &str) -> BackendResult<T> {
    value
        .parse()
        .map_err(|_| format!("Unknown {what}: \"{value}\""))
}
