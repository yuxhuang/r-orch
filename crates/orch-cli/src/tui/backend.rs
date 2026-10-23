//! [`EngineBackend`] — [`TuiBackend`] over [`FullEngine`].
//!
//! The trait and its option structs belong to
//! `orch-tui` (`crates/orch-tui/BACKEND.md`) — this is the only
//! implementation outside that crate's `FakeBackend`.
//!
//! Two rules hold throughout:
//!
//! 1. **Errors are strings.** The TUI renders `Failed: {err}`, so every
//!    fallible call collapses to [`BackendResult`]. Exit codes are a CLI
//!    concept and have no meaning in a dashboard.
//! 2. **An empty option string means "absent".** `non_empty` and
//!    `parse_optional` drop `""`, and a wizard field the user left blank reads
//!    back as an empty string (`value_of` in `orch-tui`'s wizard configs). The one
//!    exception is [`AgentFields`], where `Some("")` means *clear the stored
//!    value* — the engine's `UpdateAgentFields` has the same three states.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use orch_core::storage::{GoalFilter, TaskFilter};
use orch_domain::{
    Agent, CreateAgentInput, CreateGoalInput, CreateMessageInput, CreateTaskInput, CreateTeamInput,
    Goal, GoalStatus, MessageChannel, NotificationPreferences, OrchestratorEvent,
    OrchestratorState, PromptMode, ReasoningEffort, Run, RunStatus, Task, TaskStatus, Team,
    TuiPaletteName,
};
use orch_engine::agent_service::AuthoredAgent;
use orch_engine::prompt_mode::{AgentDiagnostic, DefinitionLock};
use orch_engine::{
    FullEngine, UpdateAgentFields, UpdateGoalFields, UpdateTaskFields, WatchOptions,
};
use orch_infra::clipboard::{
    NativeCopyStatus, NativeCopySubmitError, NativeCopyWorker, TextClipboardOpener,
};
use orch_tui::backend::{
    AgentFields, AgentOpts, BackendResult, ClipboardPaste, CopyOutcome, CopyResult, CopyTarget,
    CopyTicket, CreateTaskOpts, GoalFields, HistoryEntry, LaunchContext, MessageTarget,
    ModelCatalog, ModelOption, TaskFields, TimingSnapshot, TuiBackend, copy, derive_timings,
};
use tokio::runtime::Handle;

/// Runs whose events the history loader reads, and the size of the first
/// batch.
const HISTORY_FIRST_BATCH: usize = 3;
const HISTORY_TOTAL_RUNS: usize = 10;
/// Trailing events read per run.
const HISTORY_EVENTS_PER_RUN: usize = 30;
/// Each delivered batch is capped at its last N entries.
const HISTORY_BATCH_CAP: usize = 200;

/// The sender a message from a human carries, matching `orch msg send`'s
/// default (`commands/msg.rs`). It is the one sender
/// id the service accepts without a registered agent behind it.
const CLI_SENDER_ID: &str = "cli";

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
    model_discovery: Arc<orch_infra::models::ModelDiscovery>,
    watch_active: bool,
    peer_runtime: Arc<Mutex<Option<orch_peer::PeerRuntime>>>,
    copy: CopyTransport,
    /// The project prompt-mode write in flight, if any: its outcome is
    /// stored here by the spawned write and handed out once by
    /// `poll_prompt_mode`.
    prompt_mode: Arc<Mutex<PromptModeWrite>>,
    /// The effort diagnostics of the agent the last successful `add_agent` /
    /// `update_agent` wrote, handed out once by `take_agent_diagnostics`.
    authored_diagnostics: Vec<String>,
}

/// The lifecycle of one asynchronous project prompt-mode write.
#[derive(Debug, Default)]
enum PromptModeWrite {
    #[default]
    Idle,
    Writing,
    Finished(BackendResult<PromptMode>),
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
    /// Launch: try watch mode, and on refusal fall back to observer mode.
    ///
    /// Must be called from the runtime. `version` is the CLI's own version
    /// string, which the dashboard shows in its header.
    pub async fn launch(engine: Arc<FullEngine>, version: impl Into<String>) -> EngineBackend {
        Self::launch_with(engine, version, false).await
    }

    /// [`EngineBackend::launch`], optionally skipping watch mode.
    ///
    /// `observe` is `orch tui --observe`: it goes
    /// straight to the observer fallback below without trying the lock, so
    /// this process never starts the orchestrator. That is not a read-only
    /// view: task and agent mutations and `run` still work from it.
    pub async fn launch_with(
        engine: Arc<FullEngine>,
        version: impl Into<String>,
        observe: bool,
    ) -> EngineBackend {
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
            initial_prompt_mode: engine.config.defaults.agent.prompt_mode,
            startup_warnings: match engine.agent_service.list().await {
                Ok(agents) => {
                    orch_engine::prompt_mode::startup_diagnostics(&engine.config, &agents)
                        .into_iter()
                        .map(|d| diagnostic_line(&d))
                        .collect()
                }
                Err(err) => vec![format!("Could not check agent diagnostics: {err}")],
            },
        };

        // Watch mode auto-starts so the orchestrator is live, with seeding on —
        // `start_watch` with no options.
        let started = if observe {
            Err(None)
        } else {
            crate::serve::peer::start_watch(Arc::clone(&engine), WatchOptions::watching())
                .await
                .map_err(Some)
        };
        let peer_runtime = Arc::new(Mutex::new(None));
        let watch_active = match started {
            Ok(runtime) => {
                *peer_runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(runtime);
                true
            }
            Err(None) => {
                tracing::info!("--observe: not starting watch mode; entering observer mode");
                context.observer_mode = true;
                false
            }
            Err(Some(err)) => {
                // Another process holds the lock. The dashboard becomes a
                // read-only observer of *their* orchestrator, and the message
                // is **suppressed** in the UI: the
                // header shows the observer badge rather than an error.
                //
                // The holder's pid therefore survives only in this record,
                // which reaches a terminal only when diagnostics are on
                // (`RUST_LOG`, or `ORCHESTRY_DEBUG`) — an earlier version of
                // this comment claimed it was how the user learns the pid,
                // which was wrong twice over: nothing installed a subscriber
                // at all until the CLI grew one, and even now the default is
                // silent. `orch status` is where a user looks.
                tracing::info!(%err, "watch mode refused; entering observer mode");
                if !matches!(
                    err,
                    orch_core::Error::Domain(orch_domain::DomainError::LockConflict(_))
                ) {
                    context.watch_error = Some(err.to_string());
                }
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
            model_discovery: Arc::new(orch_infra::models::ModelDiscovery::new()),
            watch_active,
            peer_runtime,
            // Opens nothing and starts no thread: the native worker waits for
            // a native copy request that may never come.
            copy: CopyTransport::system(),
            prompt_mode: Arc::new(Mutex::new(PromptModeWrite::Idle)),
            authored_diagnostics: Vec::new(),
        };
        // History is loaded eagerly because the dashboard's activity view
        // wants it on the first frame. The model catalog starts on the first
        // `poll_model_catalog` instead — which, since `App::tick` polls every
        // tick, is the first frame anyway. The deferral is not a
        // behavioural difference, it only spares a facade consumer that never
        // polls the eight adapter spawns a probe costs.
        backend.spawn_history_load();
        backend
    }

    /// Replace the model discovery the catalog load uses. Tests pass one whose
    /// runtime execs nothing, so no test reaches a real agent CLI or `~/.codex`.
    pub fn with_model_discovery(mut self, discovery: orch_infra::models::ModelDiscovery) -> Self {
        self.model_discovery = Arc::new(discovery);
        self
    }

    pub fn is_watch_active(&self) -> bool {
        self.watch_active
    }

    pub(crate) fn startup_error(&self) -> Option<&str> {
        self.context.watch_error.as_deref()
    }

    pub(crate) fn peer_runtime_guard(&self) -> Arc<Mutex<Option<orch_peer::PeerRuntime>>> {
        Arc::clone(&self.peer_runtime)
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

    /// Keep the authored agent's diagnostics for `take_agent_diagnostics`
    /// and hand back the agent.
    fn authored(&mut self, authored: AuthoredAgent) -> Agent {
        self.authored_diagnostics = authored.diagnostics.iter().map(diagnostic_line).collect();
        authored.agent
    }

    // There was a `report` helper here that put a background failure on the
    // bus as `orchestrator:error`. Nothing is left to call it, which is the
    // point: every method that can fail is awaited now, because the dashboard
    // has no renderer for that event and a failure reported only there is a
    // failure nobody sees.

    /// Read-modify-write: the engine's `GlobalConfigStore` is whole-document.
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

    /// All runs, newest first, keeping only
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

    /// Probe every supported adapter and
    /// publish the catalog once they have all answered.
    fn spawn_catalog_load(&self) {
        let catalog = Arc::clone(&self.catalog);
        let discovery = Arc::clone(&self.model_discovery);
        self.handle.spawn(async move {
            let discovered = discovery
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
    // Every `can_*` flag stays at its `true` default: the one conditional
    // capability (image paste) is gated on the open wizard's kind, which is
    // the dashboard's own state rather than anything the engine knows.
    //
    // `can_send_message` stays at its `true` default too; overriding it to
    // `false` here is the kill switch if messaging has to be disabled without
    // a revert.

    fn context(&self) -> LaunchContext {
        self.context.clone()
    }

    fn peer_status(&mut self) -> BackendResult<String> {
        let own_runtime = self
            .peer_runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        self.block_on(async {
            let store =
                orch_infra::storage::remote::FileRemoteStore::new(self.engine.paths.clone());
            let listener =
                crate::commands::remote::listener_status(&self.engine.paths, own_runtime).await?;
            let mut status = format!(
                "listen {} · controller {}",
                crate::commands::remote::describe_listen(
                    listener.addr.as_deref(),
                    self.engine.config.peer.listen.as_deref(),
                ),
                listener.controller.as_deref().unwrap_or("none"),
            );
            let now = self.engine.clock.now_ms();
            for remote in store.list().await? {
                let link = match store.read_status(&remote.alias).await? {
                    Some(status) => {
                        let version = status
                            .peer_version
                            .as_ref()
                            .map(|version| format!(" v{version}"))
                            .unwrap_or_default();
                        format!(
                            "{}{version}",
                            crate::commands::remote::describe_link(&status, now)
                        )
                    }
                    None => "offline".into(),
                };
                status.push_str(&format!(" · {} {link}", remote.alias));
            }
            Ok::<_, orch_core::Error>(status)
        })
        .map_err(to_message)
    }

    // ── Refresh ──────────────────────────────────────────────────────────

    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.block_on(self.engine.task_service.list(&TaskFilter::default()))
            .map_err(to_message)
    }

    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        self.block_on(async {
            let local = self.engine.agent_service.list().await?;
            orch_infra::storage::remote::RemoteAgentDirectory::new(self.engine.paths.clone())
                .list(local)
                .await
        })
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

    /// Every retained run record, read once per refresh — not the ten-run
    /// history sample, which cannot tell a task's earliest start. A read
    /// failure is reported, so the dashboard shows the timings as unknown
    /// rather than claiming nothing ever started.
    fn refresh_timings(
        &mut self,
        tasks: &[Task],
        goals: &[Goal],
        orch_state: Option<&OrchestratorState>,
        now_ms: i64,
    ) -> BackendResult<TimingSnapshot> {
        let runs = self
            .block_on(self.engine.run_service.list_all())
            .map_err(to_message)?;
        Ok(derive_timings(&runs, tasks, goals, orch_state, now_ms))
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

    /// `review → done` directly, not through an approval service.
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
    ///
    /// In observer mode another process (`serve`, `run --watch`) holds the
    /// lock, so the orchestrator cannot take it. The task is then cancelled in
    /// the store only, as `orch task cancel` does, and the owner's next
    /// reconcile sees it terminal and stops the run itself.
    fn cancel_task(&mut self, id: &str) -> BackendResult<()> {
        self.block_on(async {
            match self.engine.orchestrator.cancel_task(id).await {
                Err(orch_core::Error::Domain(orch_domain::DomainError::LockConflict(_))) => {
                    self.engine.task_service.cancel(id).await.map(|_| ())
                }
                result => result,
            }
        })
        .map_err(to_message)
    }

    // ── Agents ───────────────────────────────────────────────────────────

    /// An empty `adapter` falls through to `defaults.agent.adapter`, which the
    /// engine's `AgentService::create_locked` already does.
    ///
    /// Taken under `.definition.lock`, with defaults and the final-candidate
    /// check against the project config read inside it (ruling 21/22b), the
    /// same path `orch agent add` takes. A concurrent F3 write holds the same
    /// lock, so this waits for it (bounded at the lock's 5 s timeout).
    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent> {
        self.authored_diagnostics.clear();
        let effort = parse_optional(opts.effort.as_deref(), "effort")?;
        let approval_policy = parse_optional(opts.approval_policy.as_deref(), "approval policy")?;
        let prompt_mode = parse_optional(opts.prompt_mode.as_deref(), "prompt mode")?;
        let input = CreateAgentInput {
            name: name.to_owned(),
            adapter: adapter.to_owned(),
            command: non_empty(opts.command),
            model: non_empty(opts.model),
            effort,
            role: non_empty(opts.role),
            approval_policy,
            skills: opts.skills,
            prompt_mode,
            role_summary: non_empty(opts.role_summary),
            ..Default::default()
        };
        let engine = Arc::clone(&self.engine);
        let authored = self
            .block_on(async move {
                // One block: the lock is per open file and not re-entrant,
                // and is dropped before the call returns.
                let lock = DefinitionLock::acquire(engine.paths.project_root()).await?;
                engine.agent_service.create_locked(&lock, input).await
            })
            .map_err(to_message)?;
        Ok(self.authored(authored))
    }

    /// Under `.definition.lock` against the freshly read project config, like
    /// [`EngineBackend::add_agent`] and `orch agent update`.
    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent> {
        self.authored_diagnostics.clear();
        // `Some("")` clears, so an empty string must not be parsed as a value.
        let effort = match fields.effort.as_deref() {
            None => None,
            Some("") => Some(None),
            Some(value) => Some(Some(parse_one::<ReasoningEffort>(value, "effort")?)),
        };
        // `Some("")` removes the override: the agent inherits the project mode.
        let prompt_mode = match fields.prompt_mode.as_deref() {
            None => None,
            Some("") => Some(None),
            Some(value) => Some(Some(parse_one::<PromptMode>(value, "prompt mode")?)),
        };
        let role_summary = fields
            .role_summary
            .map(|summary| Some(summary).filter(|s| !s.trim().is_empty()));
        let update = UpdateAgentFields {
            name: fields.name,
            adapter: fields.adapter,
            role: fields.role,
            command: fields.command,
            model: fields.model,
            effort,
            // The only caller — the edit-agent wizard — never sends an
            // approval policy, so the trait leaves it out. Editing it is
            // `orch agent edit`'s job.
            approval_policy: None,
            prompt_mode,
            role_summary,
        };
        let engine = Arc::clone(&self.engine);
        let id = id.to_owned();
        let authored = self
            .block_on(async move {
                let lock = DefinitionLock::acquire(engine.paths.project_root()).await?;
                engine.agent_service.update_locked(&lock, &id, update).await
            })
            .map_err(to_message)?;
        Ok(self.authored(authored))
    }

    fn take_agent_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.authored_diagnostics)
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

    // ── Messaging ────────────────────────────────────────────────────────

    /// The dashboard's half of `orch msg send` / `orch msg broadcast`:
    /// sender `cli`, no subject (the service substitutes `(no subject)`) and
    /// the default 24-hour TTL, exactly as the CLI leaves them when no flag is
    /// given (`commands/msg.rs`).
    ///
    /// Works in observer mode too. Messages are files written through the
    /// message store and `send` takes no project lock, so a dashboard watching
    /// another process's orchestrator can still post to an agent's mailbox —
    /// that orchestrator drains it on its next dispatch.
    fn send_message(&mut self, target: MessageTarget, body: &str) -> BackendResult<Vec<String>> {
        let (channel, to_agent_id) = match target {
            MessageTarget::Agent(agent_id) => (MessageChannel::Direct, Some(agent_id)),
            MessageTarget::Broadcast => (MessageChannel::Broadcast, None),
        };
        let ids: Vec<String> = self
            .block_on(self.engine.message_service.send(CreateMessageInput {
                channel,
                from_agent_id: CLI_SENDER_ID.to_owned(),
                to_agent_id: to_agent_id.clone(),
                // Empty rather than derived from the body, so a TUI-sent and a
                // CLI-sent message are identical on disk for the same intent.
                subject: String::new(),
                // Raw: the service's `truncate_utf16` is the single place the
                // 4000-unit cut happens.
                body: body.to_owned(),
                ttl_ms: None,
                team_id: None,
                reply_to: None,
            }))
            .map_err(to_message)?
            .into_iter()
            .map(|message| message.id)
            .collect();
        // The body is deliberately not logged — only that it went, and to how
        // many mailboxes, which is the half that is otherwise unrecoverable in
        // observer mode, where `message:sent` reaches this process's bus and
        // not the queue the dashboard drains.
        tracing::debug!(
            ?channel,
            to = to_agent_id.as_deref().unwrap_or("*"),
            count = ids.len(),
            "TUI sent a message"
        );
        Ok(ids)
    }

    // ── Watch mode ───────────────────────────────────────────────────────

    /// Awaited for the same reason as [`EngineBackend::run_task`]: it acquires
    /// the lock, recovers stale state and runs one tick, then returns with the
    /// loop running. The failure that matters — another process holding the
    /// lock — is knowable before any agent starts, and `/watch` is the one
    /// command whose whole purpose is to report it.
    fn start_watch(&mut self) -> BackendResult<()> {
        let runtime = self
            .block_on(crate::serve::peer::start_watch(
                Arc::clone(&self.engine),
                WatchOptions::watching(),
            ))
            .map_err(to_message)?;
        *self.peer_runtime.lock().unwrap_or_else(|e| e.into_inner()) = Some(runtime);
        self.watch_active = true;
        Ok(())
    }

    /// Awaited, like its siblings. `stop` can take up to the kill grace, so
    /// this holds a frame for that long. The alternative is worse: spawned, a
    /// failure could only be reported as `orchestrator:error`, which the
    /// dashboard has no renderer for, so `/pause` would claim to have paused a
    /// watch loop that is still running.
    ///
    /// `watch_active` therefore flips **only on success**.
    fn stop_watch(&mut self) -> BackendResult<()> {
        self.block_on(self.engine.orchestrator.stop())
            .map_err(to_message)?;
        self.peer_runtime
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.watch_active = false;
        Ok(())
    }

    // ── Settings ─────────────────────────────────────────────────────────

    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()> {
        self.mutate_global(|global| global.tui.palette = palette)
    }

    /// Stored **as given**. `ActivityFilterPreset` is a plain string and is not
    /// validated on write, so a preset r-orch does not know is persisted rather than
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
    /// cap applies from the next `start_watch`.
    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()> {
        self.block_on(
            self.engine
                .config_store
                .set("scheduling.max_concurrent_agents", value.into()),
        )
        .map_err(to_message)
    }

    /// The stored value: the raw file merged over the defaults, re-read on
    /// every call so another process's change shows up.
    fn project_prompt_mode(&mut self) -> BackendResult<PromptMode> {
        let paths = self.engine.paths.clone();
        self.block_on(async move {
            let raw = orch_infra::storage::FileConfigStore::new(paths)
                .read_raw()
                .await?;
            orch_engine::prompt_mode::effective_config_of_raw(&raw)
        })
        .map(|config| config.defaults.agent.prompt_mode)
        .map_err(to_message)
    }

    /// Spawned on the runtime so the frame keeps drawing while the lock is
    /// taken; the engine's key-only write does the locked re-read, the
    /// custom-template check over every agent, and the atomic publish.
    fn request_prompt_mode(&mut self, target: Option<PromptMode>) -> BackendResult<()> {
        {
            let mut write = self
                .prompt_mode
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if matches!(*write, PromptModeWrite::Writing) {
                return Err("A prompt mode change is already in progress".into());
            }
            *write = PromptModeWrite::Writing;
        }
        let engine = Arc::clone(&self.engine);
        let slot = Arc::clone(&self.prompt_mode);
        self.handle.spawn(async move {
            let root = engine.paths.project_root();
            let result = match target {
                Some(mode) => {
                    orch_engine::prompt_mode::set_project_prompt_mode(
                        root,
                        engine.agents.as_ref(),
                        mode,
                    )
                    .await
                }
                None => {
                    orch_engine::prompt_mode::toggle_project_prompt_mode(
                        root,
                        engine.agents.as_ref(),
                    )
                    .await
                }
            };
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                PromptModeWrite::Finished(result.map_err(to_message));
        });
        Ok(())
    }

    fn poll_prompt_mode(&mut self) -> Option<BackendResult<PromptMode>> {
        let mut write = self
            .prompt_mode
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match std::mem::take(&mut *write) {
            PromptModeWrite::Finished(result) => Some(result),
            other => {
                *write = other;
                None
            }
        }
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
    /// rather than growing without bound (ARCHITECTURE §6.5). A drop is logged,
    /// never silent.
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
    /// adapter probes off the startup path; they are only needed when a wizard
    /// needs models.
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

    /// Accept one copy request, routed by [`CopyTransport`].
    ///
    /// A *request*, like every other method here: the native route returns a
    /// ticket and answers through [`TuiBackend::poll_copy_results`], because
    /// an OS clipboard call can take seconds and a frame cannot wait for it.
    fn copy_text_via(&mut self, text: &str, target: CopyTarget) -> BackendResult<CopyTicket> {
        self.copy.copy_text_via(text, target)
    }

    /// Copy outcomes that have become known since the last frame.
    fn poll_copy_results(&mut self) -> Vec<CopyResult> {
        self.copy.poll_copy_results()
    }

    /// Read the clipboard, writing any image to a temp file.
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

// ── Copy transport (design D5) ───────────────────────────────────────────────

/// `ESC ] 5 2 ; c ;` — OSC 52 "set clipboard", selection `c`.
///
/// Written bare, with no tmux (`\x1bPtmux;…`) or screen (`\x1bP…\x1b\\`)
/// passthrough wrapper: a wrapper that does not match the real outer
/// multiplexer corrupts the sequence, and the hints cannot tell us the
/// nesting. Under a multiplexer the request may simply be ignored, which is
/// what the guidance in `copy::terminal_guidance` says.
const OSC52_SET_CLIPBOARD: &[u8] = b"\x1b]52;c;";
/// `BEL` — the sequence terminator, and the lone byte written to close a
/// sequence a failed write may have left open.
const BEL: &[u8] = b"\x07";

/// The routing hints, read once per request.
///
/// Per request and never cached: `TMUX` and `SSH_*` change under a running
/// dashboard when the user re-attaches, and a stale snapshot would route to a
/// destination that is no longer there.
trait CopyEnvSource: Send {
    fn env(&self) -> copy::CopyEnv;
}

struct ProcessEnv;

impl CopyEnvSource for ProcessEnv {
    fn env(&self) -> copy::CopyEnv {
        copy::CopyEnv::current()
    }
}

/// Where an OSC 52 request is written.
trait TerminalSink: Send {
    /// Write one complete sequence and flush it.
    ///
    /// Whole-sequence rather than byte-wise on purpose: a terminal that sees
    /// half an OSC 52 treats the next frame as clipboard payload, so the
    /// framing and the body must not be separable by the caller.
    fn write_sequence(&mut self, bytes: &[u8]) -> std::io::Result<()>;
}

/// The real terminal. The lock is taken for the single write so no draw can
/// interleave into the middle of the sequence.
struct StdoutSink;

impl TerminalSink for StdoutSink {
    fn write_sequence(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(bytes)?;
        stdout.flush()
    }
}

/// Monotonic now, for the native timeout.
trait CopyClock: Send {
    fn now(&self) -> Instant;
}

struct SystemClock;

impl CopyClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A native write the worker has not answered yet.
struct PendingCopy {
    ticket: CopyTicket,
    /// When the UI stops waiting. The OS call keeps going; only the waiting
    /// ends.
    deadline: Instant,
    /// Captured when the request was accepted, so a later environment change
    /// cannot relabel it.
    guidance: Option<String>,
}

/// The CLI's half of the copy contract: it owns the native worker, writes
/// OSC 52, and turns both into tickets and [`CopyResult`]s.
///
/// Routing itself lives in `orch_tui::backend::copy` and is a pure function;
/// what is here is the part that touches the machine. Every such part is a
/// seam — environment, terminal, clipboard opener, clock — so the tests below
/// never write a real clipboard, never open `arboard` and never put a byte on
/// the real stdout.
struct CopyTransport {
    env: Box<dyn CopyEnvSource>,
    sink: Box<dyn TerminalSink>,
    clock: Box<dyn CopyClock>,
    /// Taken on the first native request. `None` afterwards, which is what
    /// makes "one worker, never replaced" structural rather than a rule: a
    /// timed-out worker cannot be answered by starting a second one, because
    /// two writers would fight over one X11 selection.
    opener: Option<TextClipboardOpener>,
    worker: Option<NativeCopyWorker>,
    next_ticket: u64,
    pending: Vec<PendingCopy>,
    /// Finished tickets waiting for the next poll. They still count against
    /// the ticket bound: capacity frees only once the UI has seen them.
    completed: Vec<CopyResult>,
    /// Learned from a result, never guessed. Only this — definite knowledge
    /// from an earlier open failure — lets Auto fall back to the terminal.
    native_unavailable: bool,
    /// Jobs the UI gave up on and the worker has not answered. While any
    /// exist the thread may still be inside an uncancellable OS call, so a
    /// native request is refused instead of queueing behind it.
    abandoned: usize,
}

impl CopyTransport {
    /// The production wiring. Touches nothing until a request arrives.
    fn system() -> CopyTransport {
        CopyTransport::new(
            Box::new(ProcessEnv),
            Box::new(StdoutSink),
            Box::new(SystemClock),
            orch_infra::clipboard::system_text_clipboard_opener(),
        )
    }

    fn new(
        env: Box<dyn CopyEnvSource>,
        sink: Box<dyn TerminalSink>,
        clock: Box<dyn CopyClock>,
        opener: TextClipboardOpener,
    ) -> CopyTransport {
        CopyTransport {
            env,
            sink,
            clock,
            opener: Some(opener),
            worker: None,
            next_ticket: 1,
            pending: Vec::new(),
            completed: Vec::new(),
            native_unavailable: false,
            abandoned: 0,
        }
    }

    fn copy_text_via(&mut self, text: &str, target: CopyTarget) -> BackendResult<CopyTicket> {
        let env = self.env.env();
        if self.pending.len() + self.completed.len() >= copy::MAX_COPY_TICKETS {
            return Err(copy::FAIL_BUSY.to_owned());
        }
        match copy::route(target, env) {
            copy::CopyRoute::Terminal => self.request_terminal(text, copy::terminal_guidance(env)),
            copy::CopyRoute::Native => self.write_native(text, env),
            // Auto falls back only on knowledge it already had, and only
            // before the request is accepted. Once accepted as native, a
            // failure — unavailable, busy or timeout — is reported as a
            // failure: no second destination, no dual write (D5).
            copy::CopyRoute::NativeFirst if self.native_unavailable => {
                self.request_terminal(text, copy::GUIDANCE_AUTO_FALLBACK)
            }
            copy::CopyRoute::NativeFirst => self.write_native(text, env),
        }
    }

    /// One OSC 52 request. Completes immediately: the terminal never answers,
    /// which is why the outcome is `TerminalRequested` and not "written".
    fn request_terminal(
        &mut self,
        text: &str,
        guidance: &'static str,
    ) -> BackendResult<CopyTicket> {
        if text.len() > copy::TERMINAL_COPY_MAX_BYTES {
            // Nothing is written at all. A truncated clipboard is worse than
            // a refused one, and terminals drop over-long sequences anyway.
            return Err(copy::FAIL_TERMINAL_TOO_LARGE.to_owned());
        }
        // Base64 is what makes the body safe: the payload is pure
        // `A-Za-z0-9+/=`, so text containing `ESC`, `BEL` or a C1 string
        // terminator cannot close the sequence early or inject another.
        let payload = STANDARD.encode(text.as_bytes());
        let mut sequence =
            Vec::with_capacity(OSC52_SET_CLIPBOARD.len() + payload.len() + BEL.len());
        sequence.extend_from_slice(OSC52_SET_CLIPBOARD);
        sequence.extend_from_slice(payload.as_bytes());
        sequence.extend_from_slice(BEL);

        if let Err(error) = self.sink.write_sequence(&sequence) {
            // A partial write can leave the terminal inside an OSC, where it
            // would eat the next frame as clipboard payload. A lone BEL ends
            // it; the source text is never written a second time.
            let _ = self.sink.write_sequence(BEL);
            tracing::warn!(%error, "OSC 52 copy request could not be written");
            return Err(copy::terminal_failure(&error.to_string()));
        }

        let (ticket, next) = self.reserve_ticket()?;
        self.next_ticket = next;
        self.completed.push(CopyResult {
            ticket,
            outcome: CopyOutcome::TerminalRequested,
            guidance: Some(guidance.to_owned()),
        });
        Ok(ticket)
    }

    /// One native write, submitted to the worker.
    ///
    /// This route never writes OSC 52 — not on a full queue, not on an
    /// unavailable clipboard, not on a timeout. A copy the user aimed at this
    /// host must not silently become a request to their terminal, which may
    /// be a different machine's clipboard.
    fn write_native(&mut self, text: &str, env: copy::CopyEnv) -> BackendResult<CopyTicket> {
        if text.len() > copy::NATIVE_COPY_MAX_BYTES {
            return Err(copy::FAIL_NATIVE_TOO_LARGE.to_owned());
        }
        if self.abandoned > 0 {
            return Err(copy::FAIL_BUSY.to_owned());
        }
        if self.native_unavailable {
            return Err(copy::FAIL_NATIVE_UNAVAILABLE.to_owned());
        }

        let (ticket, next) = self.reserve_ticket()?;
        let deadline = self.clock.now() + Duration::from_millis(copy::NATIVE_COPY_TIMEOUT_MS);
        let guidance = copy::native_guidance(env).map(str::to_owned);

        let Some(worker) = self.native_worker() else {
            // The opener is spent, so the worker it started is the only one
            // there will ever be — and it is gone.
            return Err(copy::FAIL_NATIVE_UNAVAILABLE.to_owned());
        };
        // The job id *is* the ticket: one request, one job, so the completion
        // needs no second mapping.
        match worker.submit(ticket.0, text.to_owned()) {
            Ok(()) => {}
            Err(NativeCopySubmitError::Busy) => return Err(copy::FAIL_BUSY.to_owned()),
            Err(NativeCopySubmitError::Gone) => {
                tracing::error!("the clipboard writer thread is gone");
                self.native_unavailable = true;
                return Err(copy::FAIL_NATIVE_UNAVAILABLE.to_owned());
            }
        }

        self.next_ticket = next;
        self.pending.push(PendingCopy {
            ticket,
            deadline,
            guidance,
        });
        Ok(ticket)
    }

    /// The worker, started on the first native request.
    ///
    /// `None` only once the opener has been spent, which cannot happen while
    /// a worker is still held: the two are set together.
    fn native_worker(&mut self) -> Option<&NativeCopyWorker> {
        if self.worker.is_none() {
            self.worker = Some(NativeCopyWorker::spawn(self.opener.take()?));
        }
        self.worker.as_ref()
    }

    /// The id the next accepted request carries, and the counter after it.
    ///
    /// Reserving and committing are separate because the native path has to
    /// submit the job under the id before it knows the request was accepted;
    /// a refused request leaves the counter alone, so ids stay gapless as
    /// well as unique and increasing. Checked, so an exhausted counter
    /// refuses rather than reissuing a live ticket.
    fn reserve_ticket(&self) -> BackendResult<(CopyTicket, u64)> {
        let id = self.next_ticket;
        let next = id
            .checked_add(1)
            .ok_or_else(|| copy::FAIL_BUSY.to_owned())?;
        Ok((CopyTicket(id), next))
    }

    fn poll_copy_results(&mut self) -> Vec<CopyResult> {
        let done = self
            .worker
            .as_ref()
            .map(NativeCopyWorker::drain_done)
            .unwrap_or_default();
        for completion in done {
            let Some(index) = self
                .pending
                .iter()
                .position(|pending| pending.ticket.0 == completion.id)
            else {
                // A job the UI already gave up on. Its ticket failed with a
                // timeout and must not be reported twice — but the answer
                // proves the thread is out of the OS call and usable again.
                self.abandoned = self.abandoned.saturating_sub(1);
                if let NativeCopyStatus::Unavailable(reason) = &completion.status {
                    tracing::warn!(%reason, "the host clipboard is unavailable for writing");
                    self.native_unavailable = true;
                }
                continue;
            };
            let pending = self.pending.remove(index);
            // Guidance describes a write that happened; a failure carries
            // only the failure, so nothing claims "copied on this host".
            let (outcome, guidance) = match completion.status {
                NativeCopyStatus::Written => (CopyOutcome::NativeWritten, pending.guidance),
                NativeCopyStatus::Unavailable(reason) => {
                    tracing::warn!(%reason, "the host clipboard is unavailable for writing");
                    self.native_unavailable = true;
                    (
                        CopyOutcome::Failed(copy::FAIL_NATIVE_UNAVAILABLE.to_owned()),
                        None,
                    )
                }
                NativeCopyStatus::Failed(reason) => {
                    tracing::warn!(%reason, "the host clipboard refused a write");
                    (CopyOutcome::Failed(copy::native_failure(&reason)), None)
                }
            };
            self.completed.push(CopyResult {
                ticket: pending.ticket,
                outcome,
                guidance,
            });
        }

        let now = self.clock.now();
        let (expired, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|pending| now >= pending.deadline);
        self.pending = live;
        for pending in expired {
            // The OS call cannot be cancelled, so the worker is left strictly
            // alone and only counted: see `write_native`.
            self.abandoned += 1;
            tracing::warn!(ticket = pending.ticket.0, "native copy timed out");
            self.completed.push(CopyResult {
                ticket: pending.ticket,
                outcome: CopyOutcome::Failed(copy::FAIL_NATIVE_TIMEOUT.to_owned()),
                guidance: None,
            });
        }

        // Oldest ticket first, whatever order the answers arrived in. Stable,
        // though tickets are unique so it only fixes the order of the two.
        let mut results = std::mem::take(&mut self.completed);
        results.sort_by_key(|result| result.ticket.0);
        results
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

/// One diagnostic as the dashboard shows it, at launch and after authoring:
/// `name (id): message`, the shape `orch agent add/show` prints.
fn diagnostic_line(diagnostic: &AgentDiagnostic) -> String {
    format!(
        "{} ({}): {}",
        diagnostic.agent_name, diagnostic.agent_id, diagnostic.message
    )
}

/// The TUI only ever renders the message.
fn to_message(err: orch_core::error::Error) -> String {
    err.to_string()
}

/// An empty string is absent.
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

/// [`CopyTransport`] against fakes.
///
/// Nothing here touches the machine running the suite: the host clipboard, the
/// terminal, the environment and the clock are all injected, so a test can
/// assert the exact bytes of an OSC 52 request and drive a 3 s timeout without
/// waiting 3 s or changing a developer's clipboard.
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;

    use orch_infra::clipboard::TextClipboard;
    use orch_tui::backend::copy::{
        self, CopyEnv, FAIL_BUSY, FAIL_NATIVE_TIMEOUT, FAIL_NATIVE_TOO_LARGE,
        FAIL_NATIVE_UNAVAILABLE, FAIL_TERMINAL_TOO_LARGE, GUIDANCE_AUTO_FALLBACK,
        GUIDANCE_NATIVE_REMOTE, GUIDANCE_STY, GUIDANCE_STY_TMUX, GUIDANCE_TERMINAL, GUIDANCE_TMUX,
        NATIVE_COPY_MAX_BYTES, NATIVE_COPY_TIMEOUT_MS, TERMINAL_COPY_MAX_BYTES,
    };

    use super::*;

    /// The bound on waiting for the worker *thread*, which is real. It is
    /// never a bound on the fake clock, which only moves when a test moves it.
    const WAIT: Duration = Duration::from_secs(5);

    // ── Fakes ───────────────────────────────────────────────────────────────

    struct FixedEnv(CopyEnv);

    impl CopyEnvSource for FixedEnv {
        fn env(&self) -> CopyEnv {
            self.0
        }
    }

    #[derive(Default)]
    struct SinkLog {
        /// One entry per attempted write, holding the bytes the sink accepted.
        writes: Vec<Vec<u8>>,
        /// Accept this many bytes of the next write, then fail it.
        fail_next: Option<usize>,
    }

    impl SinkLog {
        fn only(&self) -> &[u8] {
            assert_eq!(
                self.writes.len(),
                1,
                "expected exactly one write, got {:?}",
                self.writes
            );
            &self.writes[0]
        }
    }

    struct FakeSink(Arc<Mutex<SinkLog>>);

    impl TerminalSink for FakeSink {
        fn write_sequence(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            let mut log = self.0.lock().unwrap();
            if let Some(accepted) = log.fail_next.take() {
                let accepted = accepted.min(bytes.len());
                log.writes.push(bytes[..accepted].to_vec());
                return Err(std::io::Error::other("tty gone"));
            }
            log.writes.push(bytes.to_vec());
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeClock {
        base: Instant,
        offset_ms: Arc<AtomicU64>,
    }

    impl FakeClock {
        fn new() -> FakeClock {
            FakeClock {
                base: Instant::now(),
                offset_ms: Arc::new(AtomicU64::new(0)),
            }
        }

        fn advance_ms(&self, ms: u64) {
            self.offset_ms.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl CopyClock for FakeClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    #[derive(Default)]
    struct NativeLog {
        /// Every text the fake host clipboard accepted, in order.
        written: Vec<String>,
        /// How many times the opener ran.
        opens: usize,
    }

    struct FakeNative {
        log: Arc<Mutex<NativeLog>>,
        /// Fail every write with this reason.
        reject: Option<String>,
        /// Announce entry into `set_text`, so "in flight" is a fact.
        entered: Option<mpsc::Sender<()>>,
        /// Hold every write until the test releases it.
        gate: Option<mpsc::Receiver<()>>,
    }

    impl TextClipboard for FakeNative {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            if let Some(entered) = &self.entered {
                let _ = entered.send(());
            }
            if let Some(gate) = &self.gate {
                // `Err` means the test has gone; never stay wedged after it.
                let _ = gate.recv();
            }
            if let Some(reason) = &self.reject {
                return Err(reason.clone());
            }
            self.log.lock().unwrap().written.push(text.to_owned());
            Ok(())
        }
    }

    fn native_log() -> Arc<Mutex<NativeLog>> {
        Arc::new(Mutex::new(NativeLog::default()))
    }

    /// An opener for a host clipboard that accepts writes.
    fn accepting(log: &Arc<Mutex<NativeLog>>) -> TextClipboardOpener {
        let log = Arc::clone(log);
        Box::new(move || {
            log.lock().unwrap().opens += 1;
            Ok(Box::new(FakeNative {
                log,
                reject: None,
                entered: None,
                gate: None,
            }) as Box<dyn TextClipboard>)
        })
    }

    /// An opener for a host clipboard that is open but refuses writes.
    fn rejecting(log: &Arc<Mutex<NativeLog>>, reason: &str) -> TextClipboardOpener {
        let log = Arc::clone(log);
        let reason = reason.to_owned();
        Box::new(move || {
            log.lock().unwrap().opens += 1;
            Ok(Box::new(FakeNative {
                log,
                reject: Some(reason),
                entered: None,
                gate: None,
            }) as Box<dyn TextClipboard>)
        })
    }

    /// An opener that fails, as on a headless host.
    fn unopenable(log: &Arc<Mutex<NativeLog>>) -> TextClipboardOpener {
        let log = Arc::clone(log);
        Box::new(move || {
            log.lock().unwrap().opens += 1;
            Err("no display".into())
        })
    }

    /// An opener whose writes wedge until released: a job stuck in the OS.
    fn wedging(
        log: &Arc<Mutex<NativeLog>>,
    ) -> (TextClipboardOpener, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let log = Arc::clone(log);
        let (entered, entries) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let opener: TextClipboardOpener = Box::new(move || {
            log.lock().unwrap().opens += 1;
            Ok(Box::new(FakeNative {
                log,
                reject: None,
                entered: Some(entered),
                gate: Some(gate),
            }) as Box<dyn TextClipboard>)
        });
        (opener, entries, release)
    }

    fn fixture(
        env: CopyEnv,
        opener: TextClipboardOpener,
    ) -> (CopyTransport, Arc<Mutex<SinkLog>>, FakeClock) {
        let sink = Arc::new(Mutex::new(SinkLog::default()));
        let clock = FakeClock::new();
        let transport = CopyTransport::new(
            Box::new(FixedEnv(env)),
            Box::new(FakeSink(Arc::clone(&sink))),
            Box::new(clock.clone()),
            opener,
        );
        (transport, sink, clock)
    }

    fn hints(ssh: bool, tmux: bool, sty: bool) -> CopyEnv {
        CopyEnv {
            web: false,
            ssh,
            tmux,
            sty,
        }
    }

    fn local() -> CopyEnv {
        CopyEnv::default()
    }

    // ── Assertions on captured bytes ────────────────────────────────────────

    /// The decoded body of one captured sequence, after checking the framing.
    fn decode_sequence(sequence: &[u8]) -> Vec<u8> {
        let payload = sequence
            .strip_prefix(OSC52_SET_CLIPBOARD)
            .and_then(|rest| rest.strip_suffix(BEL))
            .unwrap_or_else(|| panic!("not an OSC 52 set-clipboard sequence: {sequence:?}"));
        assert!(
            payload.iter().all(|byte| byte.is_ascii_alphanumeric()
                || *byte == b'+'
                || *byte == b'/'
                || *byte == b'='),
            "the payload must be pure base64 ASCII, got {payload:?}"
        );
        STANDARD
            .decode(payload)
            .expect("the payload must be decodable base64")
    }

    /// Poll until the worker thread has answered. Bounded, because a hung
    /// worker must fail the test rather than hang the suite.
    fn poll_until_result(transport: &mut CopyTransport) -> Vec<CopyResult> {
        let deadline = Instant::now() + WAIT;
        loop {
            let results = transport.poll_copy_results();
            if !results.is_empty() {
                return results;
            }
            assert!(Instant::now() < deadline, "the worker never answered");
            std::thread::yield_now();
        }
    }

    fn one_result(transport: &mut CopyTransport) -> CopyResult {
        let mut results = poll_until_result(transport);
        assert_eq!(results.len(), 1, "expected one result, got {results:?}");
        results.remove(0)
    }

    // ── C1: tickets, routing and the native worker ──────────────────────────

    #[test]
    fn nothing_is_opened_or_spawned_until_a_native_request() {
        let log = native_log();
        let (mut transport, _sink, _clock) = fixture(local(), accepting(&log));

        transport
            .copy_text_via("body", CopyTarget::Terminal)
            .unwrap();

        assert_eq!(log.lock().unwrap().opens, 0);
        assert!(
            transport.worker.is_none(),
            "a terminal request must not start the clipboard thread"
        );
    }

    #[test]
    fn a_native_copy_writes_the_exact_text_and_nothing_to_the_terminal() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), accepting(&log));

        let ticket = transport
            .copy_text_via("hello\n\u{1b}🦀", CopyTarget::Native)
            .unwrap();

        assert_eq!(ticket, CopyTicket(1));
        assert_eq!(
            one_result(&mut transport),
            CopyResult {
                ticket,
                outcome: CopyOutcome::NativeWritten,
                guidance: None,
            }
        );
        assert_eq!(log.lock().unwrap().written, vec!["hello\n\u{1b}🦀"]);
        assert!(
            sink.lock().unwrap().writes.is_empty(),
            "a native copy must never write OSC 52"
        );
    }

    #[test]
    fn a_native_copy_over_ssh_carries_the_remote_guidance() {
        let log = native_log();
        let (mut transport, _sink, _clock) = fixture(hints(true, false, false), accepting(&log));

        transport.copy_text_via("body", CopyTarget::Native).unwrap();

        assert_eq!(
            one_result(&mut transport).guidance.as_deref(),
            Some(GUIDANCE_NATIVE_REMOTE)
        );
    }

    #[test]
    fn an_unopenable_host_clipboard_fails_as_unavailable() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), unopenable(&log));

        let ticket = transport.copy_text_via("body", CopyTarget::Native).unwrap();

        assert_eq!(
            one_result(&mut transport),
            CopyResult {
                ticket,
                outcome: CopyOutcome::Failed(FAIL_NATIVE_UNAVAILABLE.to_owned()),
                guidance: None,
            }
        );
        assert!(sink.lock().unwrap().writes.is_empty());
        // Now *known* unavailable, so an explicit native request is refused
        // outright rather than queued behind another doomed open.
        assert_eq!(
            transport.copy_text_via("body", CopyTarget::Native),
            Err(FAIL_NATIVE_UNAVAILABLE.to_owned())
        );
    }

    #[test]
    fn an_explicit_native_write_failure_never_reaches_the_terminal() {
        let log = native_log();
        // Under tmux, where Auto would have gone to the terminal: an explicit
        // native request must not borrow that destination on failure.
        let (mut transport, sink, _clock) = fixture(
            hints(true, true, false),
            rejecting(&log, "selection refused"),
        );

        let ticket = transport.copy_text_via("body", CopyTarget::Native).unwrap();

        assert_eq!(
            one_result(&mut transport),
            CopyResult {
                ticket,
                outcome: CopyOutcome::Failed(copy::native_failure("selection refused")),
                guidance: None,
            }
        );
        assert!(
            sink.lock().unwrap().writes.is_empty(),
            "a failed native copy must not fall back to OSC 52"
        );
    }

    #[test]
    fn a_third_native_ticket_is_refused_until_the_first_two_are_drained() {
        let log = native_log();
        let accept = accepting(&log);
        let (started, opening) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let opener: TextClipboardOpener = Box::new(move || {
            started.send(()).unwrap();
            // Hold the worker before its first receive, independent of scheduling.
            let _ = gate.recv();
            accept()
        });
        let (mut transport, sink, _clock) = fixture(local(), opener);

        let first = transport.copy_text_via("a", CopyTarget::Native).unwrap();
        opening.recv_timeout(WAIT).expect("the opener started");
        let second = transport.copy_text_via("b", CopyTarget::Native).unwrap();
        assert_eq!(
            transport.copy_text_via("c", CopyTarget::Native),
            Err(FAIL_BUSY.to_owned())
        );

        release.send(()).unwrap();
        let deadline = Instant::now() + WAIT;
        let mut results = Vec::new();
        while results.len() < 2 {
            results.extend(transport.poll_copy_results());
            assert!(Instant::now() < deadline, "the worker never answered twice");
            std::thread::yield_now();
        }
        assert_eq!(
            results.iter().map(|r| r.ticket).collect::<Vec<_>>(),
            vec![first, second],
            "results come back oldest ticket first"
        );
        assert_eq!(
            transport.copy_text_via("c", CopyTarget::Native),
            Ok(CopyTicket(3))
        );
        assert_eq!(
            one_result(&mut transport).outcome,
            CopyOutcome::NativeWritten
        );
        assert_eq!(log.lock().unwrap().written, vec!["a", "b", "c"]);
        assert_eq!(log.lock().unwrap().opens, 1);
        assert!(sink.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn two_undrained_terminal_tickets_also_fill_the_bound() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(hints(false, true, false), accepting(&log));

        transport.copy_text_via("a", CopyTarget::Terminal).unwrap();
        transport.copy_text_via("b", CopyTarget::Terminal).unwrap();
        assert_eq!(
            transport.copy_text_via("c", CopyTarget::Terminal),
            Err(FAIL_BUSY.to_owned()),
            "a completed ticket occupies the bound until it is drained"
        );
        assert_eq!(
            sink.lock().unwrap().writes.len(),
            2,
            "the refused request must write nothing"
        );

        let results = transport.poll_copy_results();
        assert_eq!(
            results.iter().map(|r| r.ticket.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            transport.copy_text_via("c", CopyTarget::Terminal),
            Ok(CopyTicket(3)),
            "tickets keep increasing after a drain"
        );
    }

    #[test]
    fn a_timed_out_native_copy_fails_and_the_worker_is_reused_once_it_answers() {
        let log = native_log();
        let (opener, entries, release) = wedging(&log);
        let (mut transport, sink, clock) = fixture(local(), opener);

        let stuck = transport
            .copy_text_via("wedged", CopyTarget::Native)
            .unwrap();
        entries.recv_timeout(WAIT).expect("the write started");
        assert!(
            transport.poll_copy_results().is_empty(),
            "nothing is decided before the deadline"
        );

        clock.advance_ms(NATIVE_COPY_TIMEOUT_MS);
        assert_eq!(
            transport.poll_copy_results(),
            vec![CopyResult {
                ticket: stuck,
                outcome: CopyOutcome::Failed(FAIL_NATIVE_TIMEOUT.to_owned()),
                guidance: None,
            }]
        );

        // The thread may still be inside the OS call, so native is busy — and
        // the fix is never a second worker.
        assert_eq!(
            transport.copy_text_via("next", CopyTarget::Native),
            Err(FAIL_BUSY.to_owned())
        );
        assert_eq!(log.lock().unwrap().opens, 1);

        // The late answer is discarded: the ticket already failed and must not
        // be reported twice. But it frees the worker.
        release.send(()).unwrap();
        let deadline = Instant::now() + WAIT;
        while transport.abandoned > 0 {
            assert!(
                transport.poll_copy_results().is_empty(),
                "a late completion must not be reported"
            );
            assert!(Instant::now() < deadline, "the late answer never arrived");
            std::thread::yield_now();
        }

        let reused = transport
            .copy_text_via("after", CopyTarget::Native)
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(
            one_result(&mut transport),
            CopyResult {
                ticket: reused,
                outcome: CopyOutcome::NativeWritten,
                guidance: None,
            }
        );
        assert_eq!(log.lock().unwrap().opens, 1, "still the one handle");
        assert_eq!(
            log.lock().unwrap().written,
            vec!["wedged".to_owned(), "after".to_owned()]
        );
        assert!(sink.lock().unwrap().writes.is_empty());
    }

    #[test]
    fn auto_goes_native_while_the_host_is_unproven_and_falls_back_once_it_is_not() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), unopenable(&log));

        // Unknown availability is not a reason to ask the terminal.
        let first = transport.copy_text_via("body", CopyTarget::Auto).unwrap();
        assert!(
            sink.lock().unwrap().writes.is_empty(),
            "Auto must not write both destinations"
        );
        assert_eq!(
            one_result(&mut transport).outcome,
            CopyOutcome::Failed(FAIL_NATIVE_UNAVAILABLE.to_owned())
        );

        // Now it is definite, so the fallback is allowed — and it is labelled
        // as a fallback, not as a plain terminal request.
        let second = transport.copy_text_via("body", CopyTarget::Auto).unwrap();
        assert_ne!(second, first);
        assert_eq!(
            one_result(&mut transport),
            CopyResult {
                ticket: second,
                outcome: CopyOutcome::TerminalRequested,
                guidance: Some(GUIDANCE_AUTO_FALLBACK.to_owned()),
            }
        );
        let log = sink.lock().unwrap();
        assert_eq!(decode_sequence(log.only()), b"body");
    }

    // ── C2: the body cannot escape the framing ──────────────────────────────

    #[test]
    fn a_hostile_body_cannot_escape_the_osc_52_framing() {
        let bodies = [
            "\x1b]52;c;ATTACK\x07\x1b\\",
            "two\nlines",
            "semi;colons;everywhere",
            "c1 terminator \u{9c} here",
            "héllo 🦀 ünicode",
            "",
        ];
        for body in bodies {
            let log = native_log();
            let (mut transport, sink, _clock) = fixture(local(), accepting(&log));

            transport
                .copy_text_via(body, CopyTarget::Terminal)
                .unwrap_or_else(|err| panic!("{body:?} was refused: {err}"));

            let sink = sink.lock().unwrap();
            let sequence = sink.only();
            assert_eq!(
                decode_sequence(sequence),
                body.as_bytes(),
                "{body:?} must arrive byte-exact"
            );
            // The only control bytes on the wire are the framing constants.
            assert_eq!(
                sequence
                    .iter()
                    .filter(|byte| matches!(byte, 0x1b | 0x07 | 0x9c))
                    .count(),
                2,
                "{body:?} put control bytes in the payload: {sequence:?}"
            );
            assert!(sequence.starts_with(OSC52_SET_CLIPBOARD));
            assert!(sequence.ends_with(BEL));
            assert_eq!(
                log.lock().unwrap().opens,
                0,
                "a terminal request must not touch the host clipboard"
            );
        }
    }

    // ── C3: the size limits ─────────────────────────────────────────────────

    #[test]
    fn sixty_four_kib_is_allowed_and_one_byte_more_is_refused() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), accepting(&log));

        let body = "a".repeat(TERMINAL_COPY_MAX_BYTES);
        transport
            .copy_text_via(&body, CopyTarget::Terminal)
            .unwrap();
        {
            let sink = sink.lock().unwrap();
            let sequence = sink.only();
            assert_eq!(
                sequence.len(),
                OSC52_SET_CLIPBOARD.len() + 87_384 + BEL.len(),
                "65,536 bytes encode to an 87,384-byte payload"
            );
            assert_eq!(decode_sequence(sequence).len(), TERMINAL_COPY_MAX_BYTES);
        }

        let over = "a".repeat(TERMINAL_COPY_MAX_BYTES + 1);
        assert_eq!(
            transport.copy_text_via(&over, CopyTarget::Terminal),
            Err(FAIL_TERMINAL_TOO_LARGE.to_owned())
        );
        assert_eq!(
            sink.lock().unwrap().writes.len(),
            1,
            "an over-sized body writes nothing at all"
        );
    }

    #[test]
    fn an_over_sized_native_body_is_refused_without_starting_the_worker() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), accepting(&log));

        let over = "a".repeat(NATIVE_COPY_MAX_BYTES + 1);
        assert_eq!(
            transport.copy_text_via(&over, CopyTarget::Native),
            Err(FAIL_NATIVE_TOO_LARGE.to_owned())
        );

        assert_eq!(log.lock().unwrap().opens, 0);
        assert!(transport.worker.is_none());
        assert!(sink.lock().unwrap().writes.is_empty());
        assert_eq!(
            transport.copy_text_via("small", CopyTarget::Terminal),
            Ok(CopyTicket(1)),
            "a refused request consumes no ticket"
        );
    }

    // ── C4: the environment matrix ──────────────────────────────────────────

    /// Every hint combination, for all three targets: exactly one destination
    /// gets the bytes, and the guidance is the constant for that environment.
    #[test]
    fn the_env_matrix_routes_each_target_to_exactly_one_destination() {
        let cases = [
            ("ssh only", hints(true, false, false), GUIDANCE_TERMINAL),
            ("tmux only", hints(false, true, false), GUIDANCE_TMUX),
            ("screen only", hints(false, false, true), GUIDANCE_STY),
            (
                "tmux and screen",
                hints(false, true, true),
                GUIDANCE_STY_TMUX,
            ),
            (
                "stale ssh with tmux",
                hints(true, true, false),
                GUIDANCE_TMUX,
            ),
            (
                "stale ssh with screen",
                hints(true, false, true),
                GUIDANCE_STY,
            ),
            (
                "stale ssh with tmux and screen",
                hints(true, true, true),
                GUIDANCE_STY_TMUX,
            ),
            (
                "orch web",
                CopyEnv {
                    web: true,
                    ..CopyEnv::default()
                },
                GUIDANCE_TERMINAL,
            ),
            (
                "orch web inside tmux",
                CopyEnv {
                    web: true,
                    tmux: true,
                    ..CopyEnv::default()
                },
                GUIDANCE_TMUX,
            ),
            ("no hints", local(), GUIDANCE_TERMINAL),
        ];

        for (name, env, guidance) in cases {
            // Explicit Terminal: always OSC 52, with this environment's note.
            let log = native_log();
            let (mut transport, sink, _clock) = fixture(env, accepting(&log));
            let ticket = transport
                .copy_text_via("body 🦀", CopyTarget::Terminal)
                .unwrap();
            assert_eq!(
                transport.poll_copy_results(),
                vec![CopyResult {
                    ticket,
                    outcome: CopyOutcome::TerminalRequested,
                    guidance: Some(guidance.to_owned()),
                }],
                "{name}: explicit Terminal"
            );
            assert_eq!(
                decode_sequence(sink.lock().unwrap().only()),
                "body 🦀".as_bytes(),
                "{name}: explicit Terminal bytes"
            );
            assert_eq!(
                log.lock().unwrap().opens,
                0,
                "{name}: Terminal stayed off the host"
            );

            // Explicit Native: always the host clipboard, never OSC 52.
            let log = native_log();
            let (mut transport, sink, _clock) = fixture(env, accepting(&log));
            let ticket = transport
                .copy_text_via("body 🦀", CopyTarget::Native)
                .unwrap();
            assert_eq!(
                one_result(&mut transport),
                CopyResult {
                    ticket,
                    outcome: CopyOutcome::NativeWritten,
                    guidance: copy::native_guidance(env).map(str::to_owned),
                },
                "{name}: explicit Native"
            );
            assert_eq!(
                log.lock().unwrap().written,
                vec!["body 🦀".to_owned()],
                "{name}: explicit Native bytes"
            );
            assert!(
                sink.lock().unwrap().writes.is_empty(),
                "{name}: Native wrote OSC 52"
            );

            // Auto: the terminal under any hint, this host when there is none.
            let log = native_log();
            let (mut transport, sink, _clock) = fixture(env, accepting(&log));
            let ticket = transport
                .copy_text_via("body 🦀", CopyTarget::Auto)
                .unwrap();
            if env.prefers_terminal() {
                assert_eq!(
                    transport.poll_copy_results(),
                    vec![CopyResult {
                        ticket,
                        outcome: CopyOutcome::TerminalRequested,
                        guidance: Some(guidance.to_owned()),
                    }],
                    "{name}: Auto"
                );
                assert_eq!(
                    decode_sequence(sink.lock().unwrap().only()),
                    "body 🦀".as_bytes(),
                    "{name}: Auto bytes"
                );
                assert_eq!(
                    log.lock().unwrap().opens,
                    0,
                    "{name}: Auto stayed off the host"
                );
            } else {
                assert_eq!(
                    one_result(&mut transport).outcome,
                    CopyOutcome::NativeWritten,
                    "{name}: Auto"
                );
                assert_eq!(
                    log.lock().unwrap().written,
                    vec!["body 🦀".to_owned()],
                    "{name}: Auto bytes"
                );
                assert!(
                    sink.lock().unwrap().writes.is_empty(),
                    "{name}: Auto wrote OSC 52"
                );
            }
        }
    }

    #[test]
    fn a_failed_terminal_write_closes_the_sequence_and_never_repeats_the_text() {
        let log = native_log();
        let (mut transport, sink, _clock) = fixture(local(), accepting(&log));
        // Half the framing went out before the tty died, which is the case a
        // bare BEL has to clean up.
        sink.lock().unwrap().fail_next = Some(OSC52_SET_CLIPBOARD.len());

        assert_eq!(
            transport.copy_text_via("secret", CopyTarget::Terminal),
            Err(copy::terminal_failure("tty gone"))
        );

        {
            let sink = sink.lock().unwrap();
            assert_eq!(
                sink.writes,
                vec![OSC52_SET_CLIPBOARD.to_vec(), BEL.to_vec()],
                "recovery is one bare BEL and nothing else"
            );
            let after_failure = &sink.writes[1];
            assert!(
                !after_failure.windows(6).any(|window| window == b"secret")
                    && !after_failure
                        .windows(8)
                        .any(|window| window == STANDARD.encode("secret").as_bytes()),
                "the source text must never be written again"
            );
        }
        // No ticket was issued, and no result is owed.
        assert!(transport.poll_copy_results().is_empty());
        assert_eq!(
            transport.copy_text_via("later", CopyTarget::Terminal),
            Ok(CopyTicket(1))
        );
    }
}

/// Wizard add/edit through the real engine: validated against the project
/// config read under `.definition.lock`, and the authored agent's effort
/// diagnostics handed to the dashboard (gate C1/QA-1, C2/QA-2).
#[cfg(test)]
mod authoring_tests {
    use orch_core::storage::ConfigStore;
    use orch_domain::{WorkspaceMode, default_config};
    use orch_engine::{EngineSeams, build_full_with};
    use orch_infra::storage::{FileConfigStore, Paths};

    use super::*;

    /// A project with a custom combined template, in normal mode, plus an
    /// engine over it whose global config stays inside the project.
    async fn custom_template_project() -> (tempfile::TempDir, Arc<FullEngine>) {
        let temp = tempfile::tempdir().expect("tempdir");
        orch_core::test_support::assert_hermetic(temp.path());
        orch_infra::storage::fs_utils::clear_ensured_dirs();
        let paths = Paths::new(temp.path());
        tokio::fs::create_dir_all(paths.root())
            .await
            .expect("create .orchestry");
        let mut config = default_config();
        config.project.name = "authoring".into();
        config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
        config.defaults.agent.prompt_mode = PromptMode::Normal;
        config.prompt = Some(orch_domain::PromptConfig {
            template: Some("custom {{ task.title }}".into()),
            ..Default::default()
        });
        config.scheduling.poll_interval_ms = 100_000;
        ConfigStore::write(&FileConfigStore::new(paths), &config)
            .await
            .expect("write config.yml");
        let engine = build_full_with(
            temp.path(),
            EngineSeams {
                global_config_path: Some(temp.path().join(".orchestry/global.yml")),
                ..EngineSeams::default()
            },
        )
        .await
        .expect("build_full");
        (temp, Arc::new(engine))
    }

    /// Run `body` on a thread that is not driving the runtime, as the draw
    /// loop is, so the backend's `Handle::block_on` is legal.
    async fn on_draw_thread<T, F>(mut backend: EngineBackend, body: F) -> (EngineBackend, T)
    where
        T: Send + 'static,
        F: FnOnce(&mut EngineBackend) -> T + Send + 'static,
    {
        tokio::task::spawn_blocking(move || {
            let value = std::thread::scope(|scope| {
                scope
                    .spawn(|| body(&mut backend))
                    .join()
                    .expect("the draw thread panicked")
            });
            (backend, value)
        })
        .await
        .expect("the blocking task panicked")
    }

    fn agent_file(temp: &tempfile::TempDir, id: &str) -> Vec<u8> {
        std::fs::read(temp.path().join(format!(".orchestry/agents/{id}.yml"))).expect("agent file")
    }

    /// QA-1: the session started in normal; the project switched to eco
    /// after launch (as F3 does). An edit to "Inherit project" is checked
    /// against the eco config read under the lock, not the launch snapshot,
    /// and refused with the definition untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_inherit_edit_after_the_project_switched_to_eco_is_refused_against_the_fresh_config()
    {
        let (temp, engine) = custom_template_project().await;
        let backend = EngineBackend::launch_with(Arc::clone(&engine), "9.9.9", true).await;

        let (backend, pinned) = on_draw_thread(backend, |backend| {
            backend
                .add_agent(
                    "pinned",
                    "claude",
                    AgentOpts {
                        prompt_mode: Some("normal".into()),
                        ..Default::default()
                    },
                )
                .expect("add a normal-pinned agent")
        })
        .await;
        // Every agent overrides normal, so the switch is legitimate.
        orch_engine::prompt_mode::set_project_prompt_mode(
            engine.paths.project_root(),
            engine.agents.as_ref(),
            PromptMode::Eco,
        )
        .await
        .expect("switch the project to eco");
        let before = agent_file(&temp, &pinned.id);

        let id = pinned.id.clone();
        let (backend, edited) = on_draw_thread(backend, move |backend| {
            let result = backend.update_agent(
                &id,
                AgentFields {
                    prompt_mode: Some(String::new()),
                    ..Default::default()
                },
            );
            (result, backend.take_agent_diagnostics())
        })
        .await;
        let (result, diagnostics) = edited;
        let error = result.expect_err("inherit must be refused under eco + custom template");
        assert!(
            error.contains("incompatible with custom templates"),
            "{error}"
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            agent_file(&temp, &pinned.id),
            before,
            "a refused edit writes nothing"
        );

        // A new agent that would inherit eco is refused the same way.
        let (backend, added) = on_draw_thread(backend, |backend| {
            backend.add_agent("inherits", "claude", AgentOpts::default())
        })
        .await;
        let error = added.expect_err("an inheriting agent must be refused");
        assert!(
            error.contains("incompatible with custom templates"),
            "{error}"
        );
        drop(backend);
        drop(temp);
    }

    /// QA-2: Codex with effort and no model, created then edited from the
    /// wizard, yields the managed-model warning for the dashboard, once per
    /// write.
    #[tokio::test(flavor = "multi_thread")]
    async fn codex_effort_with_no_model_hands_the_managed_model_warning_to_the_dashboard() {
        let (temp, engine) = custom_template_project().await;
        let backend = EngineBackend::launch_with(Arc::clone(&engine), "9.9.9", true).await;

        let (backend, (agent, created, drained)) = on_draw_thread(backend, |backend| {
            let agent = backend
                .add_agent(
                    "Wizard 🧪",
                    "codex",
                    AgentOpts {
                        effort: Some("high".into()),
                        prompt_mode: Some("normal".into()),
                        ..Default::default()
                    },
                )
                .expect("add codex");
            let created = backend.take_agent_diagnostics();
            let drained = backend.take_agent_diagnostics();
            (agent, created, drained)
        })
        .await;
        assert_eq!(created.len(), 1, "{created:?}");
        assert!(
            created[0].starts_with(&format!("Wizard 🧪 ({}): ", agent.id))
                && created[0].contains("codex effort with no model"),
            "{created:?}"
        );
        assert!(drained.is_empty(), "handed out once: {drained:?}");

        let id = agent.id.clone();
        let (_backend, edited) = on_draw_thread(backend, move |backend| {
            backend
                .update_agent(
                    &id,
                    AgentFields {
                        role_summary: Some("測試 🧪".into()),
                        ..Default::default()
                    },
                )
                .expect("edit codex");
            backend.take_agent_diagnostics()
        })
        .await;
        assert_eq!(edited, created, "the edit reports the same warning");
        drop(temp);
    }
}
