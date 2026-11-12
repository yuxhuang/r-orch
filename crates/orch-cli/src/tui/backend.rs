//! [`DaemonBackend`] — [`TuiBackend`] over the orch daemon.
//!
//! The trait and its option structs belong to `orch-tui`
//! (`crates/orch-tui/BACKEND.md`); this is the only implementation outside
//! that crate's `FakeBackend`. Every read and write is one typed request to
//! the daemon that owns the project (design D7). Nothing here opens a store,
//! builds an engine or starts a process; the copy transport and the
//! clipboard image read are frontend effects on this machine, not project
//! state (design D1).
//!
//! Rules that hold throughout:
//!
//! 1. **Errors are strings.** The TUI renders `Failed: {err}`, so every
//!    fallible call collapses to [`BackendResult`]. Exit codes are a CLI
//!    concept and have no meaning in a dashboard.
//! 2. **An empty option string means "absent".** `non_empty` and
//!    `parse_optional` drop `""`, and a wizard field the user left blank reads
//!    back as an empty string (`value_of` in `orch-tui`'s wizard configs). The one
//!    exception is [`AgentFields`], where `Some("")` means *clear the stored
//!    value* — `agent.update_fields` has the same three states.
//! 3. **A lost session ends the dashboard.** A connection failure, a reply
//!    that never came (the lane is then poisoned, so the outcome of that
//!    request is unknown) or a daemon that is stopping is recorded in the
//!    shared [`SessionLoss`] and raises the draw loop's stop flag;
//!    `tui::command` then shows the no-daemon screen. Nothing is retried
//!    behind the user's back and nothing is started.
//! 4. **Bounded waits.** Every request carries a deadline, so a daemon that
//!    stops answering cannot freeze the draw thread; an expired deadline is a
//!    lost session with an unknown outcome, never a silent success.
//! 5. **Push, then read.** One observation session (every store plus the
//!    bus) is subscribed *before* the first read, so nothing committed in
//!    between is missed (brief D3). Its pump only fills memory: store
//!    changes into a coalescing [`ObservationQueue`], bus events into the
//!    activity queue. A gap or a daemon resync opens a new subscription
//!    first and only then tells the dashboard to read every view again; a
//!    different daemon instance, or the observation ending, is a lost
//!    session. Nothing on that path replays a request or starts a daemon.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use orch_client::{
    Client, ClientError, NoDaemon, Progress, Subscription, SubscriptionError, SubscriptionEvent,
};
use orch_domain::{
    Agent, CreateGoalInput, CreateTeamInput, Goal, GoalStatus, NotificationPreferences,
    OrchestratorEvent, OrchestratorState, PromptMode, ReasoningEffort, Task, Team, TuiPaletteName,
};
use orch_infra::clipboard::{
    NativeCopyStatus, NativeCopySubmitError, NativeCopyWorker, TextClipboardOpener,
};
use orch_proto::bounded::{BoundedString, BoundedVec, Id};
use orch_proto::error::NoDaemonDetail;
use orch_proto::frame::Store;
use orch_proto::ops::Op;
use orch_proto::ops::args::{self, DispatchSwitch, FieldPatch, ImageFormat, TransferPurpose};
use orch_proto::ops::results;
use orch_proto::path::WirePath;
use orch_tui::backend::{
    AgentFields, AgentOpts, BackendResult, ClipboardPaste, CopyOutcome, CopyResult, CopyTarget,
    CopyTicket, CreateTaskOpts, EntityTiming, GoalFields, HistoryEntry, LaunchContext,
    MessageTarget, ModelCatalog, ModelOption, Observation, ObservedStore, TaskFields,
    TimingSnapshot, TuiBackend, copy,
};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use super::connect::{Connector, is_session_failure};

/// The sender a message from a human carries, matching `orch msg send`'s
/// default (`commands/msg.rs`). It is the one sender id the service accepts
/// without a registered agent behind it.
const CLI_SENDER_ID: &str = "cli";

/// The deadline on an ordinary request: generous for a loaded machine, finite
/// so a daemon that stopped answering cannot hold the draw thread.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The deadline on requests that wait out an agent's kill grace (cancel,
/// force-stop) or a dispatch switch. The daemon bounds each of those itself;
/// this only has to be longer than its bound.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `run_task` / `run_all` wait for the daemon to confirm the launch
/// (its first streamed event, or the refusal) before returning. Workspace
/// preparation can take a while; the run itself is never waited on.
pub const LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Bus events waiting for the next frame, fed by the observation session.
#[derive(Debug, Default)]
struct EventQueue(Mutex<VecDeque<OrchestratorEvent>>);

/// Upper bound on undrained events. The bus the daemon forwards is lossy by
/// design (ARCHITECTURE §6.5); a dashboard that stops drawing drops the oldest
/// rather than growing without bound.
const EVENT_QUEUE_CAP: usize = 4096;

impl EventQueue {
    fn push(&self, event: OrchestratorEvent) {
        let mut queue = self.lock();
        if queue.len() >= EVENT_QUEUE_CAP {
            queue.pop_front();
            tracing::debug!("TUI event queue full; dropped the oldest event");
        }
        queue.push_back(event);
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

/// Store changes waiting for the next frame, coalesced per store: the
/// highest commit ordinal each store has been pushed, plus whether a resync
/// is pending. Bounded by construction (ten stores and a flag), so a
/// dashboard that stops drawing loses no invalidation, it only merges them.
#[derive(Debug, Default)]
struct ObservationQueue(Mutex<PendingObservations>);

#[derive(Debug, Default)]
struct PendingObservations {
    resynced: bool,
    /// Per store, in first-seen order: the newest ordinal, and the entity id
    /// when exactly one change is merged.
    changed: Vec<(ObservedStore, u64, Option<String>)>,
}

impl ObservationQueue {
    fn changed(&self, store: ObservedStore, id: Option<String>, commit_seq: u64) {
        let mut pending = self.lock();
        match pending.changed.iter_mut().find(|(s, _, _)| *s == store) {
            Some(entry) => {
                entry.1 = entry.1.max(commit_seq);
                entry.2 = None;
            }
            None => pending.changed.push((store, commit_seq, id)),
        }
    }

    /// Every view is stale. Changes merged before it are covered by the
    /// refetch; they are kept anyway, since over-reading is harmless.
    fn resynced(&self) {
        self.lock().resynced = true;
    }

    /// A resync first (it makes every view stale), then the changes.
    fn drain(&self) -> Vec<Observation> {
        let pending = std::mem::take(&mut *self.lock());
        let mut out = Vec::with_capacity(pending.changed.len() + 1);
        if pending.resynced {
            out.push(Observation::Resynced);
        }
        out.extend(pending.changed.into_iter().map(|(store, commit_seq, id)| {
            Observation::Changed {
                store,
                id,
                commit_seq,
            }
        }));
        out
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PendingObservations> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The session-loss latch shared by the backend, its background sessions and
/// `tui::command`: the first failure wins, raises `stop`, and is what the
/// no-daemon screen shows.
#[derive(Debug)]
pub struct SessionLoss {
    stop: Arc<AtomicBool>,
    failure: Mutex<Option<String>>,
}

impl SessionLoss {
    pub fn new(stop: Arc<AtomicBool>) -> Arc<SessionLoss> {
        Arc::new(SessionLoss {
            stop,
            failure: Mutex::new(None),
        })
    }

    /// Record a session failure (rendered, frozen text) and stop the draw loop.
    pub fn record(&self, rendered: String) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if failure.is_none() {
            *failure = Some(rendered);
        }
        drop(failure);
        self.stop.store(true, Ordering::SeqCst);
    }

    /// The recorded failure, if the session was lost.
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The daemon, driven from a synchronous caller.
///
/// Construct it on the runtime (so `Handle::current()` resolves), then move it
/// to the thread that owns the draw loop. See the module docs of `tui` for
/// why that thread must not be a runtime thread.
pub struct DaemonBackend {
    connector: Connector,
    /// The request lane the draw thread drives. Background work never uses
    /// it: each job opens its own session, so a slow probe cannot hold a frame.
    client: Client,
    handle: Handle,
    /// The caller's working directory: relative attachment paths are made
    /// absolute against it before they travel (spec R34).
    cwd: PathBuf,
    context: LaunchContext,
    queue: Arc<EventQueue>,
    observations: Arc<ObservationQueue>,
    history: Arc<Mutex<VecDeque<Vec<HistoryEntry>>>>,
    catalog: Arc<Mutex<CatalogState>>,
    copy: CopyTransport,
    /// The project prompt-mode write in flight, if any: its outcome is
    /// stored here by the background write and handed out once by
    /// `poll_prompt_mode`.
    prompt_mode: Arc<Mutex<PromptModeWrite>>,
    /// The effort diagnostics of the agent the last successful `add_agent` /
    /// `update_agent` wrote, handed out once by `take_agent_diagnostics`.
    authored_diagnostics: Vec<String>,
    loss: Arc<SessionLoss>,
    /// The observation pump and any observed launches; aborted on drop so
    /// no session outlives the dashboard.
    tasks: Vec<JoinHandle<()>>,
}

/// The lifecycle of one asynchronous project prompt-mode write.
#[derive(Debug, Default)]
enum PromptModeWrite {
    #[default]
    Idle,
    Writing,
    Finished(BackendResult<PromptMode>),
}

impl std::fmt::Debug for DaemonBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonBackend")
            .field("project", &self.context.project_name)
            .field("dispatch_on", &self.context.initial_watch_active)
            .finish_non_exhaustive()
    }
}

impl Drop for DaemonBackend {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl DaemonBackend {
    /// Subscribe, then connect, read the launch context in one coordinated
    /// `ui.bootstrap`, and hand the observation to its pump. Opening the
    /// dashboard **does not change dispatch**: the header shows the daemon's
    /// switch as it is.
    ///
    /// The subscription comes first so a change committed between it and
    /// the first read is still pushed (brief D3); the pump runs before the
    /// bootstrap so a slow bootstrap cannot overflow the observation buffer.
    ///
    /// Must be called from the runtime. `version` is the CLI's own version,
    /// which the dashboard shows in its header. `stop` is the draw loop's
    /// stop flag: a lost session raises it.
    pub async fn launch(
        connector: Connector,
        version: impl Into<String>,
        cwd: PathBuf,
        stop: Arc<AtomicBool>,
    ) -> Result<DaemonBackend, ClientError> {
        let subscription = subscribe(&connector).await?;
        let instance = subscription.instance().clone();
        let loss = SessionLoss::new(stop);
        let queue = Arc::new(EventQueue::default());
        let observations = Arc::new(ObservationQueue::default());
        let handle = Handle::current();
        // Aborted unless the launch completes: a failed bootstrap must not
        // leave an observation running, and the pump reports a loss only
        // once there is a dashboard to stop (`ready`), never into the
        // no-daemon screen a failed launch is about to show.
        let (ready, launched) = tokio::sync::oneshot::channel();
        let pump = AbortOnDrop(Some(handle.spawn(pump_observations(
            subscription,
            connector.clone(),
            Arc::clone(&queue),
            Arc::clone(&observations),
            Arc::clone(&loss),
            launched,
        ))));

        let mut client = connector.connect().await?;
        if client.welcome.instance != instance {
            // The daemon restarted between the two connects: this launch
            // would mix two lifetimes. Report it like any lost session.
            return Err(lost(connector.project_root()));
        }
        let boot = within_owned(REQUEST_TIMEOUT, &mut client, |c| {
            c.ui_bootstrap(args::UiBootstrapArgs {})
        })
        .await?;
        let pump = pump.release();
        let _ = ready.send(());

        let context = LaunchContext {
            project_name: boot.project_name,
            version: version.into(),
            default_adapter: boot.default_adapter,
            initial_palette: boot.palette,
            initial_activity_filter: boot.activity_filter,
            initial_notifications: boot.notifications,
            initial_max_concurrent: boot.max_concurrent,
            // The header's WATCHING / IDLE chip is the daemon's dispatch
            // switch; `/watch` and `/pause` move it (design D8).
            initial_watch_active: boot.dispatch_on,
            // There is no observer mode: every dashboard is a client.
            observer_mode: false,
            watch_error: None,
            initial_prompt_mode: boot.prompt_mode,
            startup_warnings: boot.startup_warnings,
        };

        let backend = DaemonBackend {
            connector,
            client,
            handle,
            cwd,
            context,
            queue,
            observations,
            history: Arc::new(Mutex::new(VecDeque::new())),
            catalog: Arc::new(Mutex::new(CatalogState::NotStarted)),
            // Opens nothing and starts no thread: the native worker waits for
            // a native copy request that may never come.
            copy: CopyTransport::system(),
            prompt_mode: Arc::new(Mutex::new(PromptModeWrite::Idle)),
            authored_diagnostics: Vec::new(),
            loss,
            tasks: vec![pump],
        };
        // History is loaded eagerly because the activity view wants it on
        // the first frame; the model catalogue waits for the first poll.
        let mut backend = backend;
        backend.spawn_history_load();
        Ok(backend)
    }

    /// The shared loss latch, for the caller that has to know *why* the
    /// dashboard ended after the backend has been moved into the draw loop.
    pub fn session_loss(&self) -> Arc<SessionLoss> {
        Arc::clone(&self.loss)
    }

    /// The rendered failure text for `error` in this session's variant.
    fn render(&self, error: &ClientError) -> String {
        error.render(self.connector.agent_context())
    }

    /// Turn a client error into the dashboard's message, recording a session
    /// failure on the way.
    fn fail(&self, error: ClientError) -> String {
        let error = normalize(error, &self.client.project().root);
        if session_lost(&error) {
            self.loss.record(self.render(&error));
            return "the orch daemon is no longer reachable".to_owned();
        }
        error.to_string()
    }

    /// One typed request on the lane, with the ordinary deadline.
    fn request<A: Op>(&mut self, args: A) -> BackendResult<A::Output> {
        self.request_within(REQUEST_TIMEOUT, args)
    }

    /// One typed request on the lane, with `limit`. Never called from a
    /// runtime thread — see the module docs of `tui`.
    fn request_within<A: Op>(&mut self, limit: Duration, args: A) -> BackendResult<A::Output> {
        if self.loss.failure().is_some() {
            return Err("the orch daemon is no longer reachable".to_owned());
        }
        let handle = self.handle.clone();
        let client = &mut self.client;
        let root = client.project().root.clone();
        let outcome =
            handle.block_on(async { tokio::time::timeout(limit, client.request(args)).await });
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(self.fail(error)),
            // The request may have reached the daemon: its outcome is
            // unknown and the lane is poisoned.
            Err(_) => Err(self.fail(unknown_outcome(&root))),
        }
    }

    /// Keep the authored agent's diagnostics for `take_agent_diagnostics`
    /// and hand back the agent.
    fn authored(&mut self, authored: results::AuthoredAgent) -> Agent {
        self.authored_diagnostics = authored.diagnostics.iter().map(diagnostic_line).collect();
        authored.agent
    }

    /// Make `path` absolute against the caller's cwd (spec R34: the daemon
    /// rejects relative sources).
    fn wire_paths(&self, paths: Vec<String>) -> BackendResult<BoundedVec<WirePath, 64>> {
        let paths = paths
            .into_iter()
            .map(|path| {
                let path = Path::new(&path);
                let absolute = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.cwd.join(path)
                };
                WirePath::new(absolute).map_err(|err| format!("Invalid attachment path: {err}"))
            })
            .collect::<BackendResult<Vec<_>>>()?;
        BoundedVec::new(paths).map_err(|_| "Too many attachments (at most 64)".to_owned())
    }

    // ── Background work, each on its own session ─────────────────────────

    /// The two activity-history batches (`run.history`: the newest three
    /// finished runs, then the next seven), delivered in order.
    fn spawn_history_load(&mut self) {
        let connector = self.connector.clone();
        let history = Arc::clone(&self.history);
        let loss = Arc::clone(&self.loss);
        let task = self.handle.spawn(async move {
            let result = background(&connector, |mut client| async move {
                within_owned(REQUEST_TIMEOUT, &mut client, |c| {
                    c.run_history(args::RunHistoryArgs {})
                })
                .await
            })
            .await;
            match result {
                Ok(found) => {
                    let mut queue = history.lock().unwrap_or_else(|p| p.into_inner());
                    for batch in found.batches {
                        queue.push_back(batch.into_iter().map(history_entry).collect());
                    }
                }
                Err(error) if session_lost(&error) => {
                    record_loss(&loss, &connector, error);
                }
                // History is a convenience: a refused read leaves it empty.
                Err(error) => tracing::debug!(%error, "TUI history load failed"),
            }
        });
        self.tasks.push(task);
    }

    /// Ask the daemon's bounded catalogue worker for every adapter's models.
    fn spawn_catalog_load(&mut self) {
        let connector = self.connector.clone();
        let catalog = Arc::clone(&self.catalog);
        let task = self.handle.spawn(async move {
            let result = background(&connector, |mut client| async move {
                within_owned(CONTROL_TIMEOUT, &mut client, |c| {
                    c.models_catalog(args::ModelsCatalogArgs { adapters: None })
                })
                .await
            })
            .await;
            // A failed probe still settles the state, as an empty catalogue,
            // so the wizard falls back to free text instead of waiting.
            let found = result.unwrap_or_else(|error| {
                tracing::debug!(%error, "TUI model catalogue load failed");
                Vec::new()
            });
            let converted = found
                .into_iter()
                .map(|adapter| {
                    (
                        adapter.adapter.as_str().to_owned(),
                        adapter
                            .models
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
            *catalog.lock().unwrap_or_else(|p| p.into_inner()) = CatalogState::Ready(converted);
        });
        self.tasks.push(task);
    }

    /// `dispatch.run_task` / `dispatch.run_all` on a session of their own.
    ///
    /// Both stream until their runs are terminal, which a frame cannot wait
    /// for, so the call returns as soon as the daemon confirms the launch —
    /// its first streamed event — or refuses it; the rest of the stream is
    /// drained in the background and discarded (the observation's bus
    /// already carries every event). Ending the dashboard closes that session,
    /// which ends the daemon's *observation* only: runs continue (design D8).
    fn launch_observed(&mut self, request: LaunchRequest) -> BackendResult<()> {
        if self.loss.failure().is_some() {
            return Err("the orch daemon is no longer reachable".to_owned());
        }
        let (confirm, confirmed) = tokio::sync::oneshot::channel();
        let connector = self.connector.clone();
        let loss = Arc::clone(&self.loss);
        let task = self.handle.spawn(async move {
            let mut confirm = Some(confirm);
            let result = observe_launch(&connector, request, &mut confirm).await;
            match (confirm, result) {
                // Not confirmed yet: the caller reports it.
                (Some(confirm), Err(error)) => {
                    let _ = confirm.send(Err(error));
                }
                // Confirmed, then the session went away under the observer.
                (None, Err(error)) if session_lost(&error) => {
                    record_loss(&loss, &connector, error);
                }
                _ => {}
            }
        });
        self.tasks.push(task);
        let handle = self.handle.clone();
        let root = self.connector.project_root().to_path_buf();
        match handle
            .block_on(async { tokio::time::timeout(LAUNCH_CONFIRM_TIMEOUT, confirmed).await })
        {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(self.fail(error)),
            // The observer ended without a word: treat as unknown.
            Ok(Err(_)) => Err(self.fail(unknown_outcome(&root))),
            // Still preparing: the launch was accepted and is in progress;
            // its events arrive through the feed like any other run's.
            Err(_) => Ok(()),
        }
    }

    fn set_dispatch(&mut self, switch: DispatchSwitch) -> BackendResult<()> {
        let handle = self.handle.clone();
        let client = &mut self.client;
        let root = client.project().root.clone();
        if self.loss.failure().is_some() {
            return Err("the orch daemon is no longer reachable".to_owned());
        }
        let outcome = handle.block_on(async {
            tokio::time::timeout(CONTROL_TIMEOUT, client.dispatch(switch)).await
        });
        match outcome {
            Ok(Ok(state)) => {
                let wanted = matches!(switch, DispatchSwitch::On);
                if state.dispatch_on == wanted {
                    Ok(())
                } else {
                    Err(format!(
                        "the daemon reports dispatch {}",
                        if state.dispatch_on { "ON" } else { "OFF" }
                    ))
                }
            }
            Ok(Err(error)) => Err(self.fail(error)),
            Err(_) => Err(self.fail(unknown_outcome(&root))),
        }
    }

    fn patch_preferences(&mut self, patch: args::UiPreferencesPatchArgs) -> BackendResult<()> {
        self.request(patch).map(|_| ())
    }
}

/// Which observed launch to start.
#[derive(Debug, Clone)]
enum LaunchRequest {
    Task(Id),
    All,
}

/// Open a session, start the launch, report the first frame through
/// `confirm`, then drain the rest.
async fn observe_launch(
    connector: &Connector,
    request: LaunchRequest,
    confirm: &mut Option<tokio::sync::oneshot::Sender<Result<(), ClientError>>>,
) -> Result<(), ClientError> {
    let mut client = connector.connect().await?;
    let call = match request {
        LaunchRequest::Task(task_id) => {
            drain_launch(
                client.start(args::DispatchRunTaskArgs { task_id }).await?,
                confirm,
            )
            .await
        }
        LaunchRequest::All => {
            drain_launch(client.start(args::DispatchRunAllArgs {}).await?, confirm).await
        }
    };
    call.map(|_| ())
}

async fn drain_launch(
    mut call: orch_client::Call<'_, results::RunOutcome>,
    confirm: &mut Option<tokio::sync::oneshot::Sender<Result<(), ClientError>>>,
) -> Result<results::RunOutcome, ClientError> {
    loop {
        match call.next().await? {
            Progress::Event(_) => {
                if let Some(confirm) = confirm.take() {
                    let _ = confirm.send(Ok(()));
                }
            }
            Progress::Done(outcome) => {
                if let Some(confirm) = confirm.take() {
                    let _ = confirm.send(Ok(()));
                }
                return Ok(outcome);
            }
        }
    }
}

/// Every store the dashboard shows something of, plus the ones it does not
/// (messages, context) so the subscription matches the brief's list and a
/// later view needs no protocol change.
const OBSERVED: [Store; 10] = [
    Store::Tasks,
    Store::Goals,
    Store::Agents,
    Store::Messages,
    Store::Context,
    Store::Teams,
    Store::Runs,
    Store::Config,
    Store::State,
    Store::Remote,
];

/// Open a fresh session and turn it into the dashboard's observation: every
/// store, unfiltered, with the bus.
async fn subscribe(connector: &Connector) -> Result<Subscription, ClientError> {
    let client = connector.connect().await?;
    let stores = BoundedVec::new(OBSERVED.to_vec())
        .map_err(|_| ClientError::InvalidArguments("too many observed stores".into()))?;
    client
        .into_subscription(args::SubscribeArgs {
            stores,
            filter: None,
            bus: true,
        })
        .await
}

fn observed_store(store: Store) -> ObservedStore {
    match store {
        Store::Tasks => ObservedStore::Tasks,
        Store::Goals => ObservedStore::Goals,
        Store::Agents => ObservedStore::Agents,
        Store::Messages => ObservedStore::Messages,
        Store::Context => ObservedStore::Context,
        Store::Teams => ObservedStore::Teams,
        Store::Runs => ObservedStore::Runs,
        Store::Config => ObservedStore::Config,
        Store::State => ObservedStore::State,
        Store::Remote => ObservedStore::Remote,
    }
}

/// Fill the queues from the observation until it ends. A resync (gap, lag,
/// uncertain commit, client overflow) subscribes again on a new session and
/// only then queues [`Observation::Resynced`], so every read the dashboard
/// makes for it is covered by the new subscription. Anything else that ends
/// the observation — EOF, a malformed frame, a refusal, or a resubscription
/// that reaches a different daemon instance — is a lost session.
async fn pump_observations(
    mut subscription: Subscription,
    connector: Connector,
    events: Arc<EventQueue>,
    observations: Arc<ObservationQueue>,
    loss: Arc<SessionLoss>,
    launched: tokio::sync::oneshot::Receiver<()>,
) {
    let root = connector.project_root().to_path_buf();
    let instance = subscription.instance().clone();
    let error = loop {
        match subscription.next().await {
            Ok(SubscriptionEvent::Change {
                store,
                id,
                commit_seq,
                ..
            }) => observations.changed(
                observed_store(store),
                id.map(|id| id.as_str().to_owned()),
                commit_seq,
            ),
            Ok(SubscriptionEvent::Bus { event, .. }) => events.push(*event),
            Ok(SubscriptionEvent::ResyncRequired { reason }) => {
                tracing::debug!(?reason, "TUI observation resync; subscribing again");
                subscription = match subscribe(&connector).await {
                    Ok(next) if *next.instance() == instance => next,
                    // A new daemon lifetime: the request lane and every
                    // cached view belong to the old one (brief D7).
                    Ok(_) => break lost(&root),
                    Err(error) => break normalize(error, &root),
                };
                observations.resynced();
            }
            Err(error) => break observation_error(error, &root),
        }
    };
    if launched.await.is_ok() {
        loss.record(error.render(connector.agent_context()));
    }
}

/// An ended observation, in the uniform session-loss text.
fn observation_error(error: SubscriptionError, root: &Path) -> ClientError {
    match error {
        SubscriptionError::Remote(body) => normalize(ClientError::Remote(*body), root),
        SubscriptionError::TransportClosed
        | SubscriptionError::InvalidFrame
        | SubscriptionError::InvalidAck
        | SubscriptionError::CompletionTimeout
        | SubscriptionError::BufferOverflow
        | SubscriptionError::Ended => lost(root),
    }
}

/// "The daemon this session talked to is gone", as the no-daemon screen
/// shows it.
fn lost(root: &Path) -> ClientError {
    ClientError::Connection(NoDaemon::no_daemon(
        &root.to_string_lossy(),
        NoDaemonDetail::ConnectionLost,
    ))
}

/// Aborts the task unless [`AbortOnDrop::release`]d.
struct AbortOnDrop(Option<JoinHandle<()>>);

impl AbortOnDrop {
    fn release(mut self) -> JoinHandle<()> {
        self.0.take().expect("released once")
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Open a background session and run `job` on it.
async fn background<T, F, Fut>(connector: &Connector, job: F) -> Result<T, ClientError>
where
    F: FnOnce(Client) -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>>,
{
    let client = connector.connect().await?;
    job(client).await
}

/// `job` on `client` with `limit`; an expired deadline is an unknown outcome
/// (the request may have reached the daemon).
async fn within_owned<'c, T, F, Fut>(
    limit: Duration,
    client: &'c mut Client,
    job: F,
) -> Result<T, ClientError>
where
    F: FnOnce(&'c mut Client) -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>> + 'c,
{
    let root = client.project().root.clone();
    tokio::time::timeout(limit, job(client))
        .await
        .unwrap_or_else(|_| Err(unknown_outcome(&root)))
}

fn unknown_outcome(root: &Path) -> ClientError {
    ClientError::Connection(NoDaemon::no_daemon(
        &root.to_string_lossy(),
        NoDaemonDetail::RequestOutcomeUnknown,
    ))
}

/// A daemon that answers "shutting down" is, for this session, a daemon that
/// is going away: show the uniform NoDaemon text (detail `daemon is
/// stopping`) rather than the bare refusal.
fn normalize(error: ClientError, root: &Path) -> ClientError {
    match error {
        ClientError::Remote(body) if body.code == orch_proto::error::ErrorCode::ShuttingDown => {
            ClientError::Connection(NoDaemon::no_daemon(
                &root.to_string_lossy(),
                NoDaemonDetail::Stopping,
            ))
        }
        other => other,
    }
}

/// A session failure, or the daemon refusing because it is stopping or full
/// (both carry exit 7 and mean this session cannot go on).
fn session_lost(error: &ClientError) -> bool {
    is_session_failure(error) || matches!(error, ClientError::Remote(body) if body.exit_code == 7)
}

/// Record a background session's failure, in the uniform text.
fn record_loss(loss: &SessionLoss, connector: &Connector, error: ClientError) {
    let error = normalize(error, connector.project_root());
    loss.record(error.render(connector.agent_context()));
}

fn history_entry(entry: results::HistoryEntry) -> HistoryEntry {
    HistoryEntry {
        timestamp: entry.timestamp,
        run_id: entry.run_id,
        agent_id: entry.agent_id,
        task_id: entry.task_id,
        event_type: entry.event_type,
        data: entry.data,
    }
}

fn timing(timing: results::EntityTiming) -> EntityTiming {
    EntityTiming {
        started_at_ms: timing.started_at_ms,
        elapsed_ms: timing.elapsed_ms,
    }
}

impl TuiBackend for DaemonBackend {
    // Every `can_*` flag stays at its `true` default: the one conditional
    // capability (image paste) is gated on the open wizard's kind, which is
    // the dashboard's own state rather than anything the daemon knows.

    fn context(&self) -> LaunchContext {
        self.context.clone()
    }

    /// `remote.status`, in the shape the header has always shown:
    /// `listen <addr> · controller <name> · <alias> <link> …`.
    fn peer_status(&mut self) -> BackendResult<String> {
        let report = self.request(args::RemoteStatusArgs {})?;
        let now = now_ms();
        let mut status = format!(
            "listen {} · controller {}",
            crate::commands::remote::describe_listen(
                report.listen.as_deref(),
                report.listen_configured.as_deref(),
            ),
            report.controller.as_deref().unwrap_or("none"),
        );
        for remote in &report.remotes {
            let version = remote
                .peer_version
                .as_ref()
                .map(|version| format!(" v{version}"))
                .unwrap_or_default();
            let link = crate::commands::remote::describe_link(remote, now);
            status.push_str(&format!(" · {} {link}{version}", remote.alias));
        }
        Ok(status)
    }

    // ── Refresh: one read per view a pushed change made stale ────────────

    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.request(args::TaskListArgs {
            status: None,
            goal_id: None,
        })
    }

    /// Local agents plus the cached remote ones, as `orch agent list` shows.
    fn refresh_agents(&mut self) -> BackendResult<Vec<Agent>> {
        self.request(args::AgentListArgs {})
    }

    fn refresh_goals(&mut self) -> BackendResult<Vec<Goal>> {
        self.request(args::GoalListArgs { status: None })
    }

    fn refresh_state(&mut self) -> BackendResult<OrchestratorState> {
        self.request(args::StateGetArgs {})
    }

    /// The request lane's last terminal `commit_seq`: what the read just
    /// made observed (the daemon holds the operation boundary across it).
    fn last_read_seq(&self) -> Option<u64> {
        self.client.last_commit_seq()
    }

    /// `daemon.status`: the switch lives in daemon memory, not a store.
    fn refresh_dispatch(&mut self) -> BackendResult<bool> {
        self.request(args::DaemonStatusArgs {})
            .map(|status| status.dispatch.dispatch_on)
    }

    fn list_teams(&mut self) -> BackendResult<Vec<Team>> {
        self.request(args::TeamListArgs {})
    }

    /// `run.timings`: the daemon derives every task's and goal's timing from
    /// every retained run record in one coordinated read, so the start a task
    /// shows is its earliest run, not the ten-run history sample. A read
    /// failure is reported, so the dashboard shows the timings as unknown.
    fn refresh_timings(
        &mut self,
        _tasks: &[Task],
        _goals: &[Goal],
        _orch_state: Option<&OrchestratorState>,
        _now_ms: i64,
    ) -> BackendResult<TimingSnapshot> {
        let snapshot = self.request(args::RunTimingsArgs {})?;
        Ok(TimingSnapshot {
            tasks: snapshot
                .tasks
                .into_iter()
                .map(|(id, value)| (id, timing(value)))
                .collect(),
            goals: snapshot
                .goals
                .into_iter()
                .map(|(id, value)| (id, timing(value)))
                .collect(),
            sampled_at_ms: snapshot.sampled_at_ms,
        })
    }

    // ── Tasks ────────────────────────────────────────────────────────────

    fn create_task(&mut self, title: &str, opts: CreateTaskOpts) -> BackendResult<Task> {
        let attachments = opts.attachments.map(|a| self.wire_paths(a)).transpose()?;
        self.request(args::TaskCreateArgs {
            title: bounded(title, "title")?,
            description: opts
                .description
                .map(|d| bounded(d, "description"))
                .transpose()?,
            priority: opts.priority,
            assignee: None,
            labels: None,
            depends_on: None,
            max_attempts: None,
            workspace_mode: None,
            review_criteria: None,
            scope: None,
            goal_id: None,
            attachments,
        })
    }

    fn update_task(&mut self, id: &str, fields: TaskFields) -> BackendResult<Task> {
        let attachments = fields.attachments.map(|a| self.wire_paths(a)).transpose()?;
        self.request(args::TaskUpdateFieldsArgs {
            id: id_of(id)?,
            title: fields.title.map(|t| bounded(t, "title")).transpose()?,
            description: fields
                .description
                .map(|d| bounded(d, "description"))
                .transpose()?,
            priority: fields.priority,
            labels: None,
            attachments,
        })
    }

    fn delete_task(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::TaskDeleteArgs { id: id_of(id)? })
    }

    /// `review → done` directly (`task.review_complete`), not through the
    /// CLI's hold-release-or-review approval.
    fn approve_task(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::TaskReviewCompleteArgs { id: id_of(id)? })
            .map(|_| ())
    }

    fn reject_task(&mut self, id: &str, feedback: Option<&str>) -> BackendResult<()> {
        self.request(args::TaskRejectArgs {
            id: id_of(id)?,
            feedback: feedback.map(|f| bounded(f, "feedback")).transpose()?,
        })
        .map(|_| ())
    }

    fn retry_task(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::TaskRetryArgs { id: id_of(id)? })
            .map(|_| ())
    }

    fn assign_task(&mut self, task_id: &str, agent_id: &str) -> BackendResult<()> {
        self.request(args::TaskAssignArgs {
            id: id_of(task_id)?,
            agent_id: id_of(agent_id)?,
        })
        .map(|_| ())
    }

    /// Returns once the daemon confirms the launch (or refuses it, with the
    /// reason): the dashboard has no renderer for `orchestrator:error`, so a
    /// pre-dispatch failure must come back here (BACKEND.md §3). The run is
    /// never waited on.
    fn run_task(&mut self, id: &str) -> BackendResult<()> {
        let id = id_of(id)?;
        self.launch_observed(LaunchRequest::Task(id))
    }

    fn run_all(&mut self) -> BackendResult<()> {
        self.launch_observed(LaunchRequest::All)
    }

    /// `task.cancel` in the daemon, which stops a live agent process before
    /// it answers (spec R10), whichever client asked.
    fn cancel_task(&mut self, id: &str) -> BackendResult<()> {
        self.request_within(CONTROL_TIMEOUT, args::TaskCancelArgs { id: id_of(id)? })
            .map(|_| ())
    }

    // ── Agents ───────────────────────────────────────────────────────────

    /// An empty `adapter` falls through to `defaults.agent.adapter` on the
    /// daemon side. Defaults and the final-candidate check run there under
    /// the project definition lock, the path `orch agent add` takes.
    fn add_agent(&mut self, name: &str, adapter: &str, opts: AgentOpts) -> BackendResult<Agent> {
        self.authored_diagnostics.clear();
        let effort = parse_optional::<ReasoningEffort>(opts.effort.as_deref(), "effort")?;
        let approval_policy = parse_optional(opts.approval_policy.as_deref(), "approval policy")?;
        let prompt_mode = parse_optional::<PromptMode>(opts.prompt_mode.as_deref(), "prompt mode")?;
        let adapter = if adapter.is_empty() {
            self.context.default_adapter.clone()
        } else {
            adapter.to_owned()
        };
        let skills = opts
            .skills
            .map(|skills| {
                let ids = skills
                    .into_iter()
                    .map(|skill| id_of(&skill))
                    .collect::<BackendResult<Vec<_>>>()?;
                BoundedVec::new(ids).map_err(|_| "Too many skills".to_owned())
            })
            .transpose()?;
        let authored = self.request(args::AgentCreateArgs {
            name: bounded(name, "name")?,
            adapter: id_of(&adapter)?,
            role: non_empty(opts.role)
                .map(|r| bounded(r, "role"))
                .transpose()?,
            command: non_empty(opts.command)
                .map(|c| bounded(c, "command"))
                .transpose()?,
            model: non_empty(opts.model).map(|m| id_of(&m)).transpose()?,
            effort,
            approval_policy,
            max_turns: None,
            timeout_ms: None,
            stall_timeout_ms: None,
            env: None,
            system_prompt: None,
            workspace_mode: None,
            skills,
            prompt_mode,
            role_summary: non_empty(opts.role_summary)
                .map(|s| bounded(s, "role summary"))
                .transpose()?,
        })?;
        Ok(self.authored(authored))
    }

    /// `agent.update_fields`, checked daemon-side against the freshly read
    /// project config under the definition lock, like `orch agent update`.
    fn update_agent(&mut self, id: &str, fields: AgentFields) -> BackendResult<Agent> {
        self.authored_diagnostics.clear();
        // `Some("")` clears, so an empty string must not be parsed as a value.
        let effort = match fields.effort.as_deref() {
            None => None,
            Some("") => Some(FieldPatch::Clear),
            Some(value) => Some(FieldPatch::Set(parse_one::<ReasoningEffort>(
                value, "effort",
            )?)),
        };
        // `Some("")` removes the override: the agent inherits the project mode.
        let prompt_mode = match fields.prompt_mode.as_deref() {
            None => None,
            Some("") => Some(FieldPatch::Clear),
            Some(value) => Some(FieldPatch::Set(parse_one::<PromptMode>(
                value,
                "prompt mode",
            )?)),
        };
        let role_summary = match fields.role_summary {
            None => None,
            Some(summary) if summary.trim().is_empty() => Some(FieldPatch::Clear),
            Some(summary) => Some(FieldPatch::Set(bounded(summary, "role summary")?)),
        };
        let model = match fields.model {
            None => None,
            Some(model) if model.is_empty() => Some(FieldPatch::Clear),
            Some(model) => Some(FieldPatch::Set(id_of(&model)?)),
        };
        let authored = self.request(args::AgentUpdateFieldsArgs {
            id: id_of(id)?,
            name: fields.name.map(|n| bounded(n, "name")).transpose()?,
            adapter: fields.adapter.map(|a| id_of(&a)).transpose()?,
            role: fields.role.map(|r| bounded(r, "role")).transpose()?,
            command: fields.command.map(|c| bounded(c, "command")).transpose()?,
            model,
            effort,
            // The only caller — the edit-agent wizard — never sends an
            // approval policy. Editing it is `orch agent edit`'s job.
            approval_policy: None,
            prompt_mode,
            role_summary,
        })?;
        Ok(self.authored(authored))
    }

    fn take_agent_diagnostics(&mut self) -> Vec<String> {
        std::mem::take(&mut self.authored_diagnostics)
    }

    fn delete_agent(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::AgentRemoveArgs { id: id_of(id)? })
    }

    /// `agent.stop`: the daemon owns the process and its kill grace.
    fn force_stop_agent(&mut self, id: &str) -> BackendResult<()> {
        self.request_within(CONTROL_TIMEOUT, args::AgentStopArgs { id: id_of(id)? })
    }

    fn toggle_autonomous(&mut self, id: &str, enabled: bool) -> BackendResult<Agent> {
        self.request(args::AgentAutonomousArgs {
            id: id_of(id)?,
            enabled: Some(enabled),
        })
    }

    fn disable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::AgentDisableArgs { id: id_of(id)? })
            .map(|_| ())
    }

    fn enable_agent(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::AgentEnableArgs { id: id_of(id)? })
            .map(|_| ())
    }

    // ── Teams ────────────────────────────────────────────────────────────

    fn create_team(&mut self, input: CreateTeamInput) -> BackendResult<Team> {
        let members = input
            .member_agent_ids
            .map(|ids| {
                let ids = ids
                    .iter()
                    .map(|id| id_of(id))
                    .collect::<BackendResult<Vec<_>>>()?;
                BoundedVec::new(ids).map_err(|_| "Too many team members".to_owned())
            })
            .transpose()?;
        let config = input.config.unwrap_or_default();
        self.request(args::TeamCreateArgs {
            name: bounded(input.name, "name")?,
            description: input
                .description
                .map(|d| bounded(d, "description"))
                .transpose()?,
            lead_agent_id: id_of(&input.lead_agent_id)?,
            member_agent_ids: members,
            max_concurrent_tasks: config.max_concurrent_tasks,
            auto_claim: config.auto_claim,
            message_ttl_ms: config.message_ttl_ms,
        })
    }

    fn join_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.request(args::TeamJoinArgs {
            team_id: id_of(team_id)?,
            agent_id: id_of(agent_id)?,
        })
    }

    fn leave_team(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.request(args::TeamLeaveArgs {
            team_id: id_of(team_id)?,
            agent_id: id_of(agent_id)?,
        })
    }

    fn disband_team(&mut self, team_id: &str) -> BackendResult<()> {
        self.request(args::TeamDisbandArgs {
            id: id_of(team_id)?,
        })
    }

    fn set_team_lead(&mut self, team_id: &str, agent_id: &str) -> BackendResult<Team> {
        self.request(args::TeamSetLeadArgs {
            team_id: id_of(team_id)?,
            agent_id: id_of(agent_id)?,
        })
    }

    // ── Goals ────────────────────────────────────────────────────────────

    fn create_goal(&mut self, input: CreateGoalInput) -> BackendResult<Goal> {
        self.request(args::GoalCreateArgs {
            title: bounded(input.title, "title")?,
            description: input
                .description
                .map(|d| bounded(d, "description"))
                .transpose()?,
            assignee: input.assignee.map(|a| id_of(&a)).transpose()?,
        })
    }

    fn update_goal(&mut self, id: &str, fields: GoalFields) -> BackendResult<Goal> {
        self.request(args::GoalUpdateArgs {
            id: id_of(id)?,
            title: fields.title.map(|t| bounded(t, "title")).transpose()?,
            description: fields
                .description
                .map(|d| bounded(d, "description"))
                .transpose()?,
            assignee: fields.assignee.map(|a| id_of(&a)).transpose()?,
        })
    }

    /// `goal.status` — a pause is atomic with dispatch in the daemon (spec
    /// R9): no task of the goal starts after the pause was decided.
    fn update_goal_status(
        &mut self,
        id: &str,
        status: GoalStatus,
        force: bool,
    ) -> BackendResult<Goal> {
        self.request_within(
            CONTROL_TIMEOUT,
            args::GoalStatusArgs {
                id: id_of(id)?,
                status,
                force,
            },
        )
    }

    fn delete_goal(&mut self, id: &str) -> BackendResult<()> {
        self.request(args::GoalDeleteArgs { id: id_of(id)? })
    }

    /// An absent progress note is **not** a failure: the key simply has not
    /// been written yet, so it reports an empty report rather than an error
    /// the dashboard would render as `Failed:`.
    fn goal_progress(&mut self, id: &str) -> BackendResult<String> {
        self.request(args::GoalProgressArgs { id: id_of(id)? })
            .map(Option::unwrap_or_default)
    }

    // ── Messaging ────────────────────────────────────────────────────────

    /// The dashboard's half of `orch msg send` / `orch msg broadcast`:
    /// sender `cli`, no subject (the service substitutes `(no subject)`) and
    /// the default 24-hour TTL, exactly as the CLI leaves them when no flag is
    /// given (`commands/msg.rs`). The body travels raw: the service's
    /// `truncate_utf16` is the single place the 4000-unit cut happens.
    fn send_message(&mut self, target: MessageTarget, body: &str) -> BackendResult<Vec<String>> {
        let from_agent_id = id_of(CLI_SENDER_ID)?;
        let subject = bounded("", "subject")?;
        let body_text = bounded(body, "message body")?;
        let (messages, to) = match target {
            MessageTarget::Agent(agent_id) => (
                self.request(args::MessageSendArgs {
                    from_agent_id,
                    to_agent_id: id_of(&agent_id)?,
                    subject,
                    body: body_text,
                    ttl_ms: None,
                    reply_to: None,
                })?,
                agent_id,
            ),
            MessageTarget::Broadcast => (
                self.request(args::MessageBroadcastArgs {
                    from_agent_id,
                    subject,
                    body: body_text,
                    ttl_ms: None,
                    team_id: None,
                })?,
                "*".to_owned(),
            ),
        };
        let ids: Vec<String> = messages.into_iter().map(|message| message.id).collect();
        // The body is deliberately not logged — only that it went, and to how
        // many mailboxes.
        tracing::debug!(to, count = ids.len(), "TUI sent a message");
        Ok(ids)
    }

    // ── Watch mode = the daemon's dispatch switch (design D8) ────────────

    /// `/watch`: `dispatch.set on`. No process lifecycle: the daemon is
    /// already running (or this dashboard would not be open).
    fn start_watch(&mut self) -> BackendResult<()> {
        self.set_dispatch(DispatchSwitch::On)
    }

    /// `/pause`: `dispatch.set off`. Running agents continue; only future
    /// automatic dispatch stops.
    fn stop_watch(&mut self) -> BackendResult<()> {
        self.set_dispatch(DispatchSwitch::Off)
    }

    // ── Settings ─────────────────────────────────────────────────────────

    /// Field patches (`ui.preferences.patch`), never a client read-modify-write
    /// of the whole global document.
    fn save_palette(&mut self, palette: TuiPaletteName) -> BackendResult<()> {
        self.patch_preferences(args::UiPreferencesPatchArgs {
            palette: Some(palette),
            activity_filter: None,
            notifications: None,
        })
    }

    /// The daemon validates the preset against the known five.
    fn save_activity_filter(&mut self, preset: &str) -> BackendResult<()> {
        self.patch_preferences(args::UiPreferencesPatchArgs {
            palette: None,
            activity_filter: Some(id_of(preset)?),
            notifications: None,
        })
    }

    fn save_notifications(&mut self, notifications: NotificationPreferences) -> BackendResult<()> {
        self.patch_preferences(args::UiPreferencesPatchArgs {
            palette: None,
            activity_filter: None,
            notifications: Some(args::NotificationsPatch {
                toast: Some(notifications.toast),
                bell: Some(notifications.bell),
            }),
        })
    }

    /// `config.set scheduling.max_concurrent_agents`, which deep-merges and so
    /// preserves keys r-orch does not model. The daemon's scheduler reads the
    /// new cap on its next dispatch.
    fn save_max_concurrent(&mut self, value: u32) -> BackendResult<()> {
        self.request(args::ConfigSetArgs {
            key: id_of("scheduling.max_concurrent_agents")?,
            value: bounded(value.to_string(), "value")?,
        })
        .map(|_| ())
    }

    /// The stored value, re-read on every call so another client's change
    /// shows up.
    fn project_prompt_mode(&mut self) -> BackendResult<PromptMode> {
        self.request(args::ConfigPromptModeGetArgs {})
    }

    /// On its own session so the frame keeps drawing while the daemon takes
    /// the definition lock and checks every agent against custom templates.
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
        let connector = self.connector.clone();
        let slot = Arc::clone(&self.prompt_mode);
        let loss = Arc::clone(&self.loss);
        let task = self.handle.spawn(async move {
            let result = background(&connector, |mut client| async move {
                within_owned(REQUEST_TIMEOUT, &mut client, |c| async move {
                    match target {
                        Some(mode) => {
                            c.config_prompt_mode_set(args::ConfigPromptModeSetArgs { mode })
                                .await
                        }
                        None => {
                            c.config_prompt_mode_toggle(args::ConfigPromptModeToggleArgs {})
                                .await
                        }
                    }
                })
                .await
            })
            .await;
            let result = result.map_err(|error| {
                let message = error.to_string();
                if session_lost(&error) {
                    record_loss(&loss, &connector, error);
                }
                message
            });
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                PromptModeWrite::Finished(result);
        });
        self.tasks.push(task);
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
        self.request(args::StateCompleteOnboardingArgs {})
    }

    // ── Polled streams ───────────────────────────────────────────────────

    /// Every bus event the observation delivered since the last frame,
    /// oldest first.
    fn poll_events(&mut self) -> Vec<OrchestratorEvent> {
        self.queue.drain()
    }

    /// History batches that have arrived since the last frame.
    /// Store changes pushed since the last frame, coalesced per store.
    fn poll_observations(&mut self) -> Vec<Observation> {
        self.observations.drain()
    }

    fn poll_history(&mut self) -> Vec<Vec<HistoryEntry>> {
        self.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect()
    }

    /// The discovered model catalog, once the daemon has answered.
    ///
    /// The first call **starts** the discovery and returns `None`; later calls
    /// return the catalog once, then `None` again.
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

    /// Accept one copy request, routed by [`CopyTransport`]. A frontend
    /// effect on this machine; it never reaches the daemon.
    fn copy_text_via(&mut self, text: &str, target: CopyTarget) -> BackendResult<CopyTicket> {
        self.copy.copy_text_via(text, target)
    }

    /// Copy outcomes that have become known since the last frame.
    fn poll_copy_results(&mut self) -> Vec<CopyResult> {
        self.copy.poll_copy_results()
    }

    /// Read the clipboard here (a frontend effect), then hand an image to the
    /// daemon as a sealed transfer; `attachment.stage_image` keeps it in a
    /// daemon-owned private file whose path the task then attaches.
    fn paste_image(&mut self) -> BackendResult<ClipboardPaste> {
        use orch_infra::clipboard::ClipboardContentType;

        let handle = self.handle.clone();
        match handle.block_on(orch_infra::clipboard::detect_clipboard_type()) {
            ClipboardContentType::Text => Ok(ClipboardPaste::Text),
            ClipboardContentType::Empty => Ok(ClipboardPaste::Empty),
            ClipboardContentType::Image => {
                let Some(image) = handle.block_on(orch_infra::clipboard::get_clipboard_image())
                else {
                    return Err("Failed to read clipboard image".to_owned());
                };
                if image.ext != "png" {
                    return Err(format!("Unsupported clipboard image format: {}", image.ext));
                }
                let bytes = image.data.len();
                let staged = self.stage_image(&image.data)?;
                Ok(ClipboardPaste::Image {
                    path: staged.path.into_path_buf().display().to_string(),
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

impl DaemonBackend {
    /// Upload `png` and stage it, both on the request lane (a transfer token
    /// is only valid on the session that sealed it).
    fn stage_image(&mut self, png: &[u8]) -> BackendResult<results::StagedAttachment> {
        if self.loss.failure().is_some() {
            return Err("the orch daemon is no longer reachable".to_owned());
        }
        let handle = self.handle.clone();
        let client = &mut self.client;
        let root = client.project().root.clone();
        let outcome = handle.block_on(async {
            tokio::time::timeout(CONTROL_TIMEOUT, async {
                let token = client.upload(TransferPurpose::ClipboardImage, png).await?;
                client
                    .attachment_stage_image(args::AttachmentStageImageArgs {
                        input_transfer: token,
                        format: ImageFormat::Png,
                    })
                    .await
            })
            .await
        });
        match outcome {
            Ok(Ok(staged)) => Ok(staged),
            Ok(Err(error)) => Err(self.fail(error)),
            Err(_) => Err(self.fail(unknown_outcome(&root))),
        }
    }
}

/// Milliseconds since the epoch, for the remote link's "retry in" text.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
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

/// One diagnostic as the dashboard shows it, at launch and after authoring:
/// `name (id): message`, the shape `orch agent add/show` prints.
fn diagnostic_line(diagnostic: &results::AgentDiagnostic) -> String {
    format!(
        "{} ({}): {}",
        diagnostic.agent_name, diagnostic.agent_id, diagnostic.message
    )
}

/// A bounded protocol string, or the dashboard's message for an over-long one.
fn bounded<const MAX: usize>(
    value: impl Into<String>,
    what: &str,
) -> BackendResult<BoundedString<MAX>> {
    BoundedString::new(value).map_err(|_| format!("The {what} is too long (at most {MAX} bytes)"))
}

/// An identifier argument.
fn id_of(value: &str) -> BackendResult<Id> {
    bounded(value, "identifier")
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
