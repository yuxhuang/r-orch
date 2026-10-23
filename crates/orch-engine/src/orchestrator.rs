//! The orchestrator.
//!
//! Conceptually **reconcile → dispatch → collect**, but only the first two are
//! tick phases: collection runs as a detached task per run and completion is
//! handled by [`Orchestrator::handle_run_success`] /
//! [`Orchestrator::handle_run_failure`], each of which takes the state lock.
//!
//! # Concurrency
//!
//! A state lock and the state object it guards must stay coupled, so they are
//! **one** `tokio::sync::Mutex<Option<OrchestratorState>>`:
//!
//! - Every public entry point that takes the state lock — `tick`,
//!   `stop`, `cancel_task`, `force_stop_agent`, `fresh_dispatch`,
//!   `handle_run_success`, `handle_run_failure` — acquires the guard and holds
//!   it across its store I/O. That serialization is
//!   load-bearing: the whole `activeCollectors` / early-`delete running[id]` /
//!   re-read-before-save bug history is about it.
//! - Every internal phase takes `&mut OrchestratorState` as a parameter rather
//!   than re-locking, so the compiler *proves* a re-entrant-lock deadlock
//!   cannot be reintroduced.
//!
//! One consequence is worth stating: `collect_events` updates
//! `running[task].last_event_at` under the same guard, so that write waits for
//! an in-flight phase instead of interleaving with it. The
//! write is already debounced by [`Orchestrator::save_state_lazy`] and
//! reconcile skips stall checks for tasks with an active collector, so no
//! observable behaviour depends on the interleaving — and a torn read is
//! impossible by construction.
//!
//! The other three pieces of shared state have their own locks, acquired in
//! this documented order so deadlock is impossible: **`state` →
//! `active_collectors` → `cancels`**. `last_auto_seed_at` and `single_task_runs`
//! are leaves and may be taken at any point.

mod collect;
mod dispatch;
mod recovery;
mod remote;
mod tick;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use orch_core::adapter::AdapterRegistry;
use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::process::ProcessManager;
use orch_core::skills::SkillLoader;
use orch_core::storage::{AgentStore, GoalStore, RunStore, StateStore, TaskStore};
use orch_core::template::TemplateEngine;
use orch_core::workspace::WorkspaceManager;
use orch_domain::{DomainError, OrchestratorConfig, OrchestratorEvent, OrchestratorState};
use orch_infra::storage::cached::{CachedAgentStore, CachedGoalStore, CachedTaskStore};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use crate::agent_service::AgentService;
use crate::events::EventBus;
use crate::lock::ProjectLock;
use crate::message_service::MessageService;
use crate::run_service::RunService;
use crate::task_service::TaskService;

/// Max serialized event data written to JSONL (8 KB) — layer 2.
const MAX_EVENT_DATA_LEN: usize = 8192;
/// Max event data forwarded to the bus for TUI consumption (4 KB) — layer 3.
const MAX_BUS_DATA_LEN: usize = 4096;
/// Consecutive tick rejections before the orchestrator shuts itself down.
const MAX_CONSECUTIVE_TICK_FAILURES: u32 = 5;
/// The retry queue is deduplicated by task id and bounded to this many entries;
/// on overflow the oldest is dropped.
const MAX_RETRY_QUEUE_SIZE: usize = 100;
/// Minimum interval between auto-seeded tasks for the same agent.
const AUTO_SEED_COOLDOWN_MS: i64 = 30_000;
/// The reactive-dispatch debounce, and the debounce on the lazy state write.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// How many times a reactive dispatch reschedules itself while a tick is in
/// flight before giving up (≈5 s).
const MAX_IMMEDIATE_DISPATCH_RETRIES: u32 = 10;
/// Grace period for the kill in `cancel_task` / `force_stop_agent`.
const CANCEL_KILL_GRACE: Duration = Duration::from_millis(3_000);
/// Grace period for the kill that follows stall detection.
const STALL_KILL_GRACE: Duration = Duration::from_millis(5_000);
/// `proof.agent_summary` cap.
const MAX_AGENT_SUMMARY_LEN: usize = 2000;
/// `last_error.message` cap.
const MAX_LAST_ERROR_LEN: usize = 500;
/// How long a submit may sit unacknowledged *while its peer is online*
/// before the controller gives up on it. Offline time does not count: a
/// worker that is simply unreachable has not failed to answer.
const REMOTE_SUBMIT_DEADLINE_MS: i64 = 5 * 60_000;
/// The floor on a phase clock's per-sample credit, for a configuration whose
/// poll interval is tiny. Two reconciles' worth of the default interval.
const REMOTE_CLOCK_MIN_SAMPLE_MS: i64 = 10_000;
/// How long a mirror may stay queued on its worker before the controller
/// says so, and how often it repeats. A queued mirror is usually waiting for
/// the worker owner's `orch task approve`, which no deadline here can grant,
/// so this only ever warns.
const REMOTE_QUEUED_WARN_MS: i64 = 60 * 60_000;
/// Per-wait budget for watch lifecycle synchronization. Expiry leaves the
/// current cycle intact; callers may retry once startup/cleanup has progressed.
const WATCH_LIFECYCLE_WAIT: Duration = Duration::from_secs(3);

/// Everything the orchestrator needs: the project lock is an injected
/// [`ProjectLock`], and there are two determinism seams (ARCHITECTURE §5.6).
pub struct OrchestratorDeps {
    pub task_store: Arc<dyn TaskStore>,
    pub agent_store: Arc<dyn AgentStore>,
    pub run_store: Arc<dyn RunStore>,
    pub state_store: Arc<dyn StateStore>,
    pub adapters: Arc<AdapterRegistry>,
    pub workspace: Arc<dyn WorkspaceManager>,
    pub templates: Arc<dyn TemplateEngine>,
    pub processes: Arc<dyn ProcessManager>,
    pub events: Arc<EventBus>,
    pub task_service: Arc<TaskService>,
    pub agent_service: Arc<AgentService>,
    pub run_service: Arc<RunService>,
    pub clock: Arc<dyn Clock>,
    pub lock: Arc<dyn ProjectLock>,
    /// How auto-review criteria are executed. A seam because a unit test must
    /// not shell out to `npm`.
    pub command_runner: Arc<dyn crate::review_runner::CommandRunner>,
    pub config: OrchestratorConfig,
    pub project_root: PathBuf,
    /// Optional; tests leave several of them out.
    pub context_store: Option<Arc<dyn orch_core::storage::ContextStore>>,
    pub message_service: Option<Arc<MessageService>>,
    pub goal_store: Option<Arc<dyn GoalStore>>,
    pub skill_loader: Option<Arc<dyn SkillLoader>>,
}

/// Deliberately **not** `Default`: `handle_signals` would default to `false`,
/// so a caller writing `..Default::default()` would get a watch loop that
/// ignores `SIGTERM` — a daemon that cannot be shut down by its supervisor,
/// with nothing at the call site to hint at it. Use the named constructors;
/// a caller that wants something else spells both fields out.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    /// `tick()` skips `seed_autonomous_tasks` — `serve --once` sets it.
    pub skip_autonomous_seeding: bool,
    /// Install `SIGINT`/`SIGTERM` arms in the watch loop. Tests turn it off
    /// and drive [`Orchestrator::handle_signal`] directly, because a real
    /// signal would hit the test process itself.
    pub handle_signals: bool,
}

impl WatchOptions {
    /// Long-running watch mode (`orch serve`, `orch run --watch`, the TUI):
    /// The default watch options.
    pub fn watching() -> Self {
        WatchOptions {
            skip_autonomous_seeding: false,
            handle_signals: true,
        }
    }

    /// `serve --once`: drain the queue and exit, seeding nothing new.
    /// Skips autonomous seeding.
    pub fn once() -> Self {
        WatchOptions {
            skip_autonomous_seeding: true,
            handle_signals: true,
        }
    }
}

/// The debounced state write (`saveStateLazy` / `flushStateLazy`).
#[derive(Default)]
struct LazySave {
    dirty: bool,
    timer: Option<tokio::task::JoinHandle<()>>,
}

/// Release waiters even if shutdown is cancelled or unwinds before cleanup ends.
struct StopNotification(watch::Sender<bool>);

impl Drop for StopNotification {
    fn drop(&mut self) {
        // Retain the value even when no waiter has subscribed yet.
        self.0.send_replace(true);
    }
}

struct Inner {
    deps: OrchestratorDeps,

    /// `stateMutex` **and** `this.state`. See the module docs.
    state: Mutex<Option<OrchestratorState>>,
    /// Task ids whose `collect_events` task is still running. Reconcile skips
    /// the PID and stall checks for these, because the child may have exited
    /// cleanly while the success handler is still queued for the state lock.
    active_collectors: Mutex<HashSet<String>>,
    /// `abortControllers`.
    cancels: Mutex<HashMap<String, CancellationToken>>,
    /// Task ids started by `run <id>`; these must not trigger a reactive
    /// dispatch of anything else.
    single_task_runs: Mutex<HashSet<String>>,
    /// Per-agent auto-seed cooldown. In-memory — a restart reseeds.
    last_auto_seed_at: Mutex<HashMap<String, i64>>,

    remote_state_dirty: AtomicBool,
    remote_bridge: std::sync::RwLock<Arc<dyn orch_core::remote::RemoteBridge>>,
    /// Where dispatch re-reads the project prompt mode; see
    /// [`Orchestrator::set_config_store`].
    config_store: std::sync::RwLock<Option<Arc<dyn orch_core::storage::ConfigStore>>>,
    event_tap: std::sync::RwLock<Option<Arc<dyn orch_core::remote::RunEventTap>>>,
    /// The instance id of the controller connected to this instance's peer
    /// listener, if any. Dispatch consults it before starting a mirror.
    inbound_controller: std::sync::RwLock<Option<String>>,
    /// Per-task phase clocks for the remote deadlines. In memory on purpose:
    /// a restart rehydrates every remote entry as `unknown`, which neither
    /// deadline applies to, so there is nothing to carry across one.
    remote_clocks: Mutex<HashMap<String, remote::RemoteClock>>,
    lazy_save: Mutex<LazySave>,
    immediate_dispatch: Mutex<Option<tokio::task::JoinHandle<()>>>,
    watch_task: Mutex<Option<tokio::task::JoinHandle<()>>>,

    /// Serializes watch starts, including joining the previous watch loop.
    watch_start: Mutex<()>,
    /// Serializes startup with the selection of a stop owner.
    lifecycle: Mutex<()>,
    lock_acquired: AtomicBool,
    shutting_down: AtomicBool,
    tick_in_progress: AtomicBool,
    skip_autonomous_seeding: AtomicBool,
    consecutive_tick_failures: AtomicU32,

    /// Cancels the watch loop.
    shutdown: std::sync::Mutex<CancellationToken>,
    /// Latched by a signal on the standalone paths (`run <id>`, `run --all`);
    /// see [`Orchestrator::interrupt`]. Never cancelled in watch mode, whose
    /// signal arms go through [`Orchestrator::stop`] instead.
    interrupt: CancellationToken,
    /// Flips to `true` once `stop` completes or is aborted, releasing waiters.
    stopped: std::sync::Mutex<watch::Sender<bool>>,

    /// Tick-scoped memoized reads. Invalidated at the top of every tick and of
    /// every `fresh_dispatch`.
    cached_tasks: CachedTaskStore,
    cached_agents: CachedAgentStore,
    cached_goals: Option<CachedGoalStore>,
}

/// The core state machine.
///
/// Cheap to clone — every clone shares one state lock — which is what lets a
/// detached collector task hold one.
#[derive(Clone)]
pub struct Orchestrator {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator")
            .field("is_owner", &self.is_owner())
            .field("shutting_down", &self.is_shutting_down())
            .finish_non_exhaustive()
    }
}

impl Orchestrator {
    pub fn new(deps: OrchestratorDeps) -> Self {
        let cached_tasks = CachedTaskStore::new(Arc::clone(&deps.task_store));
        let cached_agents = CachedAgentStore::new(Arc::clone(&deps.agent_store));
        let cached_goals = deps
            .goal_store
            .as_ref()
            .map(|goals| CachedGoalStore::new(Arc::clone(goals)));
        let (stopped, _) = watch::channel(false);

        Orchestrator {
            inner: Arc::new(Inner {
                remote_state_dirty: AtomicBool::new(false),
                remote_bridge: std::sync::RwLock::new(Arc::new(
                    orch_core::remote::NoopRemoteBridge,
                )),
                config_store: std::sync::RwLock::new(None),
                event_tap: std::sync::RwLock::new(None),
                inbound_controller: std::sync::RwLock::new(None),
                deps,
                state: Mutex::new(None),
                active_collectors: Mutex::new(HashSet::new()),
                cancels: Mutex::new(HashMap::new()),
                single_task_runs: Mutex::new(HashSet::new()),
                last_auto_seed_at: Mutex::new(HashMap::new()),
                remote_clocks: Mutex::new(HashMap::new()),
                lazy_save: Mutex::new(LazySave::default()),
                immediate_dispatch: Mutex::new(None),
                watch_task: Mutex::new(None),
                watch_start: Mutex::new(()),
                lifecycle: Mutex::new(()),
                lock_acquired: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                tick_in_progress: AtomicBool::new(false),
                skip_autonomous_seeding: AtomicBool::new(false),
                consecutive_tick_failures: AtomicU32::new(0),
                shutdown: std::sync::Mutex::new(CancellationToken::new()),
                interrupt: CancellationToken::new(),
                stopped: std::sync::Mutex::new(stopped),
                cached_tasks,
                cached_agents,
                cached_goals,
            }),
        }
    }

    pub(crate) fn deps(&self) -> &OrchestratorDeps {
        &self.inner.deps
    }

    fn config(&self) -> &OrchestratorConfig {
        &self.inner.deps.config
    }

    /// Let dispatch re-read `defaults.agent.prompt_mode` from the project
    /// config before each prompt assembly, so a mode switch applies to the
    /// next dispatch. Without it the startup config's mode is used. Only that
    /// one key is refreshed; every other setting keeps the startup snapshot.
    pub fn set_config_store(&self, store: Arc<dyn orch_core::storage::ConfigStore>) {
        *self
            .inner
            .config_store
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    fn config_store(&self) -> Option<Arc<dyn orch_core::storage::ConfigStore>> {
        self.inner
            .config_store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn emit(&self, event: OrchestratorEvent) {
        self.inner.deps.events.emit(event);
    }

    /// `orchestrator:error { fatal: false }` — the universal
    /// "something went wrong but the loop continues" channel. Every call site
    /// names its context, because that string is how `serve --json` and the
    /// TUI tell the failures apart.
    fn emit_error(&self, error: impl std::fmt::Display, context: impl Into<String>) {
        self.emit(OrchestratorEvent::OrchestratorError {
            error: error.to_string(),
            context: context.into(),
            fatal: false,
        });
    }

    /// Whether this instance owns the lock, and so may mutate state.
    pub fn is_owner(&self) -> bool {
        self.inner.lock_acquired.load(Ordering::SeqCst)
    }

    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }

    /// Whether a standalone signal has been latched by [`Orchestrator::interrupt`].
    pub fn is_interrupted(&self) -> bool {
        self.inner.interrupt.is_cancelled()
    }

    // ── locking ──────────────────────────────────────────────────────────

    /// Acquire the lock, run `body`, release. Used by the single-shot commands
    /// (`run <id>`, `run --all`, `cancel`, `agent stop`) that do not go
    /// through `start_watch`. The lock is released even when `body` fails.
    async fn with_temporary_lock<F, Fut>(&self, body: F) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let outcome = self.deps().lock.acquire().await;
        if !outcome.acquired {
            return Err(Error::Domain(DomainError::LockConflict(outcome.pid)));
        }
        self.inner.lock_acquired.store(true, Ordering::SeqCst);
        let result = body().await;
        self.inner.lock_acquired.store(false, Ordering::SeqCst);
        self.deps().lock.release().await;
        result
    }

    // ── state ────────────────────────────────────────────────────────────

    /// Replace the in-memory state from the store. Always called at the start
    /// of a critical section, never outside one.
    async fn load_state(&self, slot: &mut Option<OrchestratorState>) -> Result<()> {
        // Do not reload an old running entry after an outcome's final write
        // failed. Persist the in-memory removal before any tick can replace it.
        if self.inner.remote_state_dirty.load(Ordering::SeqCst)
            && let Some(state) = slot.as_ref()
        {
            self.save_state(state).await?;
        }
        *slot = Some(self.deps().state_store.read().await?);
        Ok(())
    }

    async fn save_state(&self, state: &OrchestratorState) -> Result<()> {
        self.deps().state_store.write(state).await?;
        self.inner.remote_state_dirty.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn save_remote_state(&self, state: &OrchestratorState) -> Result<()> {
        self.inner.remote_state_dirty.store(true, Ordering::SeqCst);
        self.save_state(state).await
    }

    /// Debounced write, for the non-critical `last_event_at` updates
    /// `collect_events` produces. Coalesces everything inside one 500 ms
    /// window into a single store write.
    async fn save_state_lazy(&self) {
        let mut lazy = self.inner.lazy_save.lock().await;
        lazy.dirty = true;
        if lazy.timer.is_some() {
            return;
        }
        let orchestrator = self.clone();
        lazy.timer = Some(tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            let dirty = {
                let mut lazy = orchestrator.inner.lazy_save.lock().await;
                lazy.timer = None;
                std::mem::take(&mut lazy.dirty)
            };
            if !dirty {
                return;
            }
            let state = orchestrator.inner.state.lock().await;
            if let Some(state) = state.as_ref()
                && let Err(err) = orchestrator.save_state(state).await
            {
                orchestrator.emit_error(err, "debounced state save");
            }
        }));
    }

    /// Force the debounced write out before a critical transition.
    ///
    /// Takes the already-held state rather than locking it: every caller runs
    /// inside the state lock, so locking here would deadlock.
    async fn flush_state_lazy(&self, state: &OrchestratorState) {
        let dirty = {
            let mut lazy = self.inner.lazy_save.lock().await;
            if let Some(timer) = lazy.timer.take() {
                timer.abort();
            }
            std::mem::take(&mut lazy.dirty)
        };
        if dirty && let Err(err) = self.save_state(state).await {
            self.emit_error(err, "debounced state save");
        }
    }

    /// Cancel the debounce without writing. Used by `stop`, which persists the
    /// cleared state itself a moment later.
    async fn cancel_lazy_save(&self) {
        let mut lazy = self.inner.lazy_save.lock().await;
        if let Some(timer) = lazy.timer.take() {
            timer.abort();
        }
        lazy.dirty = false;
    }

    // ── public API ───────────────────────────────────────────────────────

    /// Run one task by id.
    ///
    /// Dispatches inline when watch mode already holds the lock; otherwise
    /// takes a temporary one.
    pub async fn run_task(&self, task_id: &str) -> Result<()> {
        if self.is_owner() {
            return self.fresh_dispatch_only(task_id).await;
        }
        self.with_temporary_lock(|| self.fresh_dispatch_only(task_id))
            .await
    }

    /// Run every dispatchable task.
    pub async fn run_all(&self) -> Result<()> {
        if self.is_owner() {
            return self.fresh_dispatch_all().await;
        }
        self.with_temporary_lock(|| self.fresh_dispatch_all()).await
    }

    /// Start watch mode: acquire the lock, recover from a previous process,
    /// run one tick immediately, then poll.
    ///
    /// Returns once the loop is running. Use [`Orchestrator::wait_for_stop`] to block.
    pub async fn start_watch(&self, options: WatchOptions) -> Result<()> {
        self.start_watch_with(options, || async { Ok(()) }).await
    }

    /// Install owner-only runtime services after recovery and before the first
    /// dispatch. A failed setup releases the owner lock like any startup error.
    pub async fn start_watch_with<F, Fut>(&self, options: WatchOptions, prepare: F) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let _start = self
            .wait_for_watch_lifecycle("watch start", self.inner.watch_start.lock())
            .await?;
        let mut lifecycle = self
            .wait_for_watch_lifecycle("watch startup", self.inner.lifecycle.lock())
            .await?;
        if self.is_shutting_down() {
            let stopped = self
                .inner
                .stopped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .subscribe();
            drop(lifecycle);
            self.wait_for_watch_lifecycle("previous shutdown", Self::wait_for_completion(stopped))
                .await?;
            // An interrupted stop releases waiters, but may leave resources
            // owned. Do not turn incomplete cleanup into a fresh watch cycle.
            if self.is_owner() {
                return Err(Error::other(
                    "Previous orchestrator shutdown did not complete",
                ));
            }
            // stop cannot join its own watch task (signals and the circuit
            // breaker run there). Join it here before resetting the lifecycle,
            // so its final tick, stop calls and subscription cannot outlive it.
            {
                let mut previous = self
                    .wait_for_watch_lifecycle("previous watch handle", self.inner.watch_task.lock())
                    .await?;
                if let Some(task) = previous.as_mut() {
                    // Poll in place: timeout or caller cancellation must retain
                    // the old task, so a retry cannot start an overlapping loop.
                    let _ = self
                        .wait_for_watch_lifecycle("previous watch loop", task)
                        .await?;
                    previous.take();
                }
            }
            lifecycle = self
                .wait_for_watch_lifecycle("watch restart", self.inner.lifecycle.lock())
                .await?;
        }
        let _lifecycle = lifecycle;
        self.inner
            .skip_autonomous_seeding
            .store(options.skip_autonomous_seeding, Ordering::SeqCst);

        let outcome = self.deps().lock.acquire().await;
        if !outcome.acquired {
            return Err(Error::Domain(DomainError::LockConflict(outcome.pid)));
        }
        self.inner.lock_acquired.store(true, Ordering::SeqCst);
        if self.is_shutting_down() {
            *self
                .inner
                .shutdown
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = CancellationToken::new();
            // Each cycle has its own completion channel. Existing waiters keep
            // the completed channel even if a new watch starts before they run.
            *self.inner.stopped.lock().unwrap_or_else(|e| e.into_inner()) = watch::channel(false).0;
            self.inner.shutting_down.store(false, Ordering::SeqCst);
            self.inner
                .consecutive_tick_failures
                .store(0, Ordering::SeqCst);
        }

        // **Installed before the first tick**, which is the whole point: until
        // these exist, `SIGTERM` and `SIGINT` keep their default disposition —
        // immediate death, with no `stop()`, no state save and no lock
        // release, leaving the children orphaned with live pids in
        // `state.running` so the next startup's staleness filter skips them.
        // The first tick reconciles and dispatches everything ready (worktree
        // creation, agent spawns), so that window is seconds to minutes, and
        // `systemctl restart` during startup walks straight into it. Both
        // handlers are therefore registered before the first tick.
        //
        // Creating the arm is what replaces the disposition — a signal that
        // arrives during the first tick is held by tokio's signal driver and
        // handled as soon as the loop polls, rather than killing the process.
        let signals = if options.handle_signals {
            SignalArms::install()
        } else {
            SignalArms::disabled()
        };

        // Everything from here can fail, and failing with the lock still held
        // is worse than not starting: `serve` and `run --watch` exit, so their
        // dead pid makes the file stealable, but the TUI stays up in observer
        // mode holding a lock stamped with its own *live* pid, which nothing
        // will reclaim for the full staleness window.
        // Reactive dispatch: a task created by any process in this binary
        // should not wait for the next poll.
        let reactive = self.clone();
        let mut unsubscribe = Some(self.deps().events.on(
            orch_domain::OrchestratorEventType::TaskCreated,
            move |_event| {
                reactive.schedule_immediate_dispatch(0);
            },
        ));

        match self
            .start_watch_prepared(signals, &mut unsubscribe, prepare)
            .await
        {
            Ok(()) => Ok(()),
            Err(err) => {
                if let Some(unsubscribe) = unsubscribe.take() {
                    unsubscribe.unsubscribe();
                }
                self.inner.lock_acquired.store(false, Ordering::SeqCst);
                self.deps().lock.release().await;
                Err(err)
            }
        }
    }

    /// The fallible half of [`Orchestrator::start_watch`], split out so one
    /// `match` can release the lock on every error path.
    async fn start_watch_prepared<F, Fut>(
        &self,
        signals: SignalArms,
        unsubscribe: &mut Option<crate::events::Unsubscribe>,
        prepare: F,
    ) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        {
            let mut slot = self.inner.state.lock().await;
            self.load_state(&mut slot).await?;
            let state = slot.as_mut().expect("state loaded");
            // A previous process may have died mid-run: drop its running
            // entries and cancel the work rather than retrying it, so agents
            // do not redo already-committed work.
            self.cleanup_stale_running_entries(state).await?;
            // A crash between `adapter.execute` and `save_state` leaves a run
            // `running` with no entry here and an unparented child; nothing
            // else sweeps those.
            self.cleanup_orphaned_running_runs(state).await?;
            state.pid = Some(std::process::id());
            state.started_at = Some(self.deps().clock.now_iso());
            self.save_state(state).await?;
        }

        prepare().await?;
        self.tick_once().await?;
        self.spawn_watch_loop(signals, unsubscribe).await?;
        Ok(())
    }

    /// Resolve once every dispatched run's collector has finished.
    ///
    /// The CLI's replacement for node's handle accounting. `run_task` and
    /// `run_all` return as soon as the dispatch is done, and the collector
    /// that follows a run — the task's status change, the agent's stats, the
    /// workspace merge and cleanup — is a detached task. Node keeps the
    /// process alive for it because the agent's pipes and the fs operations
    /// are open handles; a Rust `main` that returned there would drop the
    /// runtime and leave the task `in_progress` with its worktree still on
    /// disk, which is exactly what `v7-run-single-task` measured.
    ///
    /// Polled rather than awaited: the collectors are detached, and a 25 ms
    /// poll on a set that is almost always empty costs nothing next to a run.
    pub async fn wait_for_collectors(&self) {
        loop {
            if self.inner.active_collectors.lock().await.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Resolves once stop has completed (or the orchestrator is dropped).
    /// Cancellation or panic in [`Orchestrator::stop`] also releases waiters;
    /// in that case cleanup may be incomplete.
    pub async fn wait_for_stop(&self) {
        let stopped = self
            .inner
            .stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .subscribe();
        Self::wait_for_completion(stopped).await;
    }

    async fn wait_for_watch_lifecycle<T>(
        &self,
        context: &'static str,
        wait: impl Future<Output = T>,
    ) -> Result<T> {
        tokio::time::timeout(WATCH_LIFECYCLE_WAIT, wait)
            .await
            .map_err(|_| {
                let error = Error::other(format!("Timed out waiting for {context}"));
                self.emit_error(&error, context);
                error
            })
    }

    async fn wait_for_completion(mut stopped: watch::Receiver<bool>) {
        while !*stopped.borrow_and_update() {
            if stopped.changed().await.is_err() {
                return;
            }
        }
    }

    /// What a `SIGINT`/`SIGTERM` arm does. Public so a test can exercise it
    /// without signalling the test process.
    pub async fn handle_signal(&self, signal: &str) {
        self.emit(OrchestratorEvent::OrchestratorShutdown {
            reason: format!("Received {signal}"),
        });
        if let Err(err) = self.stop().await {
            self.emit_error(err, format!("stop after {signal} signal"));
        }
    }

    /// Stop the loop and clean up. Idempotent: the first call owns the
    /// cleanup; every other call — concurrent or later — returns `Ok(())` only
    /// once the owner's cleanup has completed (see
    /// [`Orchestrator::wait_for_stop`]), so a caller may drop the peer runtime
    /// or exit as soon as it returns. Only the owner sees a cleanup error.
    ///
    /// Every running agent is aborted, killed with a 10 s grace, its run
    /// finished as `cancelled` and its task sent to `resolve_failure_status` so
    /// it retries if budget remains.
    ///
    /// The in-engine callers — the signal arm and the tick circuit breaker —
    /// may be non-owners and then wait inside the watch task. That cannot
    /// deadlock: both call from the watch loop's select body with no lock,
    /// guard or tick in flight, and the owner's cleanup never awaits the watch
    /// task (the next start joins it instead).
    pub async fn stop(&self) -> Result<()> {
        let lifecycle = self
            .wait_for_watch_lifecycle("watch stop", self.inner.lifecycle.lock())
            .await?;
        let stopped = self
            .inner
            .stopped
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if self.inner.shutting_down.swap(true, Ordering::SeqCst) {
            drop(lifecycle);
            Self::wait_for_completion(stopped.subscribe()).await;
            return Ok(());
        }
        let _stopped = StopNotification(stopped);
        drop(lifecycle);

        // `clearInterval(this.timer)`: stop *future* ticks and let the one in
        // flight finish. Retain the handle for the next start to join, rather
        // than awaiting or aborting it here, for two reasons found the hard way.
        //
        // Aborting it cancels the caller: both the signal arms and the tick
        // circuit breaker call `stop()` **from inside the watch task**, so
        // the abort would land at the next await that pends — the state save,
        // before the lock release — releasing `wait_for_stop` with cleanup
        // still incomplete.
        //
        // Aborting it also cancels whatever the *tick* is doing, and a tick
        // cancelled between `atomic_write`'s temp file and its rename leaves
        // `.orchestry/.state.json.<hash>.tmp` behind — litter
        // nobody wants.
        self.inner
            .shutdown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel();
        // Synchronize with installation without awaiting the task itself.
        drop(self.inner.watch_task.lock().await);
        if let Some(timer) = self.inner.immediate_dispatch.lock().await.take() {
            timer.abort();
        }
        self.cancel_lazy_save().await;

        // The save is allowed to fail without aborting the rest of the
        // shutdown. Letting a rejection propagate out of the critical section
        // would skip the lock release and wedge
        // the project for every later invocation — reachable here because the
        // tick circuit breaker calls `stop()` precisely when the state store
        // is the thing that is broken. The error is still returned, so a
        // caller sees it; it just no longer costs the lock.
        let saved = {
            let mut slot = self.inner.state.lock().await;
            match slot.as_mut() {
                Some(state) => {
                    // Detached remote entries stay: their worker is still
                    // executing them, and the next start rehydrates them.
                    let detached = self.shutdown_running_entries(state).await;
                    state
                        .running
                        .retain(|task_id, _| detached.contains(task_id));
                    state.claimed.clear();
                    state.pid = None;
                    state.started_at = None;
                    self.save_state(state).await
                }
                None => Ok(()),
            }
        };

        self.drain_remote_cancellations().await;

        if self.inner.lock_acquired.swap(false, Ordering::SeqCst) {
            self.deps().lock.release().await;
        }

        if let Err(err) = &saved {
            self.emit_error(err, "stop: persist the cleared state");
        }
        saved
    }

    /// Latch a `SIGINT`/`SIGTERM` received by a standalone `run <id>` or
    /// `run --all` (gate-resilience F1, lead ruling 19). Synchronous and
    /// idempotent; it aborts nothing.
    ///
    /// From here no new dispatch starts: the scheduling pass stops at its next
    /// candidate and the reactive dispatch finds nothing to do. A dispatch
    /// already inside its critical section runs to its end, bounded: an
    /// effort preparation in flight (OpenCode's wire probe) is cancelled, so
    /// its process group is killed and reaped now instead of at its deadline,
    /// and the dispatch then fails before any model child exists through the
    /// ordinary pre-spawn failure path. Its caller still awaits that dispatch
    /// to completion, then calls [`Orchestrator::stop_interrupted_runs`].
    ///
    /// Watch mode does not use this; its signal arms call
    /// [`Orchestrator::stop`].
    pub fn interrupt(&self) {
        self.inner.interrupt.cancel();
    }

    /// The second half of a standalone interrupt: once the in-flight dispatch
    /// has settled, stop every run **this process** started and wait for
    /// their collectors.
    ///
    /// The same wind-down `stop` applies to watch mode's runs, restricted to
    /// this process's own collectors, so a concurrent `orch run` in another
    /// terminal keeps its run: each local child is killed with the default
    /// grace, its run finished `cancelled`, its task sent to
    /// `resolve_failure_status` and its agent returned to idle; a remote
    /// execution is detached exactly as `stop` detaches it. The project lock
    /// is taken for the write when it is free, and the state is then re-read
    /// first; when another process owns it, the write goes through the state
    /// this process holds, the way its collectors would have written the same
    /// outcomes. The collector wait is bounded by the kill grace.
    pub async fn stop_interrupted_runs(&self, signal: &str) -> Result<()> {
        self.interrupt();
        self.emit(OrchestratorEvent::OrchestratorShutdown {
            reason: format!("Received {signal}"),
        });
        if self.is_owner() {
            // Watch mode owns the lock and its own stop path.
            return Ok(());
        }
        let lock = self.deps().lock.acquire().await;
        let saved = self.stop_interrupted_runs_locked(lock.acquired).await;
        if lock.acquired {
            self.deps().lock.release().await;
        }
        let collectors = tokio::time::timeout(
            orch_core::process::DEFAULT_KILL_GRACE,
            self.wait_for_collectors(),
        )
        .await;
        if collectors.is_err() {
            self.emit_error(
                "collectors did not finish within the kill grace",
                format!("stop after {signal} signal"),
            );
        }
        if let Err(err) = &saved {
            self.emit_error(err, format!("stop after {signal} signal"));
        }
        saved
    }

    async fn stop_interrupted_runs_locked(&self, reload: bool) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        // Read under the state lock: a dispatch registers its collector before
        // it releases that lock, so nothing this process started is missed.
        let owned: HashSet<String> = self.inner.active_collectors.lock().await.clone();
        if owned.is_empty() {
            return Ok(());
        }
        if reload || slot.is_none() {
            self.load_state(&mut slot).await?;
        }
        let state = slot.as_mut().expect("state loaded");
        let detached = self
            .shutdown_running_entries_where(state, |task_id| owned.contains(task_id))
            .await;
        state
            .running
            .retain(|task_id, _| !owned.contains(task_id) || detached.contains(task_id));
        state.claimed.retain(|task_id| !owned.contains(task_id));
        self.inner
            .single_task_runs
            .lock()
            .await
            .retain(|task_id| !owned.contains(task_id));
        self.save_state(state).await
    }

    /// Cancel a running task: abort, kill, finish the run, mark cancelled.
    pub async fn cancel_task(&self, task_id: &str) -> Result<()> {
        if !self.is_owner() {
            return self
                .with_temporary_lock(|| self.cancel_task_locked(task_id))
                .await;
        }
        self.cancel_task_locked(task_id).await
    }

    async fn cancel_task_locked(&self, task_id: &str) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        self.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");

        let entry = state.running.get(task_id).cloned();
        if let Some(entry) = entry {
            if self.worker_signal_unverified(&entry).await {
                return Err(orch_core::error::Error::other(
                    "Worker execution ownership cannot verify; not signalled; reservation retained",
                ));
            }
            self.take_cancel(task_id).await;
            if let Some(remote) = &entry.remote {
                self.cancel_remote(remote).await;
            } else {
                self.deps()
                    .processes
                    .kill_with_grace(entry.pid, CANCEL_KILL_GRACE)
                    .await;
            }
            if let Err(err) = self
                .deps()
                .run_service
                .finish(&entry.run_id, orch_domain::RunStatus::Cancelled, None, None)
                .await
            {
                self.emit_error(err, format!("cancelTask finish run {}", entry.run_id));
            }
            if entry.remote.is_none()
                && let Err(err) = self
                    .deps()
                    .agent_service
                    .set_status(&entry.agent_id, orch_domain::AgentStatus::Idle)
                    .await
            {
                self.emit_error(
                    err,
                    format!("cancelTask setStatus idle for agent {}", entry.agent_id),
                );
            }

            state.running.shift_remove(task_id);
            self.save_state(state).await?;
        }

        state.retry_queue.retain(|retry| retry.task_id != task_id);

        // `cancel` refuses a terminal task; `update_status` refuses the same
        // transition, so both failing means it was already terminal and there
        // is nothing to do.
        if self.deps().task_service.cancel(task_id).await.is_err() {
            let _ = self
                .deps()
                .task_service
                .update_status(task_id, orch_domain::TaskStatus::Cancelled)
                .await;
        }

        self.save_state(state).await
    }

    /// Force-stop one agent: kill each of its runs, fail their tasks, release
    /// the agent.
    pub async fn force_stop_agent(&self, agent_id: &str) -> Result<()> {
        if !self.is_owner() {
            return self
                .with_temporary_lock(|| self.force_stop_agent_locked(agent_id))
                .await;
        }
        self.force_stop_agent_locked(agent_id).await
    }

    async fn force_stop_agent_locked(&self, agent_id: &str) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        self.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");

        let owned: Vec<(String, orch_domain::RunningEntry)> = state
            .running
            .iter()
            .filter(|(_, entry)| entry.agent_id == agent_id)
            .map(|(task_id, entry)| (task_id.clone(), entry.clone()))
            .collect();

        for (task_id, entry) in owned {
            if self.worker_signal_unverified(&entry).await {
                return Err(orch_core::error::Error::other(
                    "Worker execution ownership cannot verify; not signalled; reservation retained",
                ));
            }
            self.take_cancel(&task_id).await;
            if let Some(remote) = &entry.remote {
                self.cancel_remote(remote).await;
            } else {
                self.deps()
                    .processes
                    .kill_with_grace(entry.pid, CANCEL_KILL_GRACE)
                    .await;
            }
            self.deps()
                .run_service
                .finish(&entry.run_id, orch_domain::RunStatus::Cancelled, None, None)
                .await?;
            // The transition may be invalid (already terminal); that is not an
            // error here.
            let _ = self
                .deps()
                .task_service
                .update_status(&task_id, orch_domain::TaskStatus::Failed)
                .await;
            state.running.shift_remove(&task_id);
        }

        if !agent_id.contains('@') {
            self.deps()
                .agent_service
                .set_status(agent_id, orch_domain::AgentStatus::Idle)
                .await?;
        }
        self.save_state(state).await
    }

    // ── cancellation tokens ──────────────────────────────────────────────

    async fn register_cancel(&self, task_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        self.inner
            .cancels
            .lock()
            .await
            .insert(task_id.to_owned(), token.clone());
        token
    }

    /// Remove the token and cancel it if there was one.
    async fn take_cancel(&self, task_id: &str) {
        if let Some(token) = self.inner.cancels.lock().await.remove(task_id) {
            token.cancel();
        }
    }

    /// Remove the token **without** cancelling — the rollback path, where the
    /// process never launched.
    async fn drop_cancel(&self, task_id: &str) {
        self.inner.cancels.lock().await.remove(task_id);
    }

    // ── watch loop ───────────────────────────────────────────────────────

    async fn spawn_watch_loop(
        &self,
        signals: SignalArms,
        unsubscribe: &mut Option<crate::events::Unsubscribe>,
    ) -> Result<()> {
        // Acquire before spawning: installation failure must not detach a loop.
        let mut watch_task = self
            .wait_for_watch_lifecycle("watch loop installation", self.inner.watch_task.lock())
            .await?;
        let unsubscribe = unsubscribe.take().expect("startup subscription");
        let orchestrator = self.clone();
        let interval_ms = self.config().scheduling.poll_interval_ms.max(1);
        let shutdown = self
            .inner
            .shutdown
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
            // `setInterval` does not catch up on missed ticks; tokio's default
            // `Burst` would fire several back-to-back after a slow tick and
            // change dispatch timing.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first `tick()` on a fresh interval resolves immediately and
            // `start_watch` has already run one tick, so discard it.
            ticker.tick().await;

            // Installed by `start_watch` *before* the first tick, so a signal
            // that arrived during it is already queued here.
            let mut signals = signals;

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        match orchestrator.tick_once().await {
                            Ok(()) => {
                                orchestrator
                                    .inner
                                    .consecutive_tick_failures
                                    .store(0, Ordering::SeqCst);
                            }
                            Err(err) => orchestrator.on_tick_failure(err).await,
                        }
                    }
                    Some(signal) = signals.next() => {
                        orchestrator.handle_signal(signal).await;
                        break;
                    }
                }
            }
            unsubscribe.unsubscribe();
        });

        *watch_task = Some(task);
        Ok(())
    }

    /// The circuit breaker: five consecutive rejections shut the orchestrator
    /// down, and every failure reports its running count so a watcher can see
    /// the breaker approaching.
    async fn on_tick_failure(&self, err: Error) {
        let count = self
            .inner
            .consecutive_tick_failures
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        let fatal = count >= MAX_CONSECUTIVE_TICK_FAILURES;
        self.emit(OrchestratorEvent::OrchestratorError {
            error: err.to_string(),
            context: "tick".into(),
            fatal,
        });
        if fatal {
            self.emit(OrchestratorEvent::OrchestratorShutdown {
                reason: format!("{count} consecutive tick failures"),
            });
            if let Err(err) = self.stop().await {
                self.emit_error(err, "stop after consecutive tick failures");
            }
        }
    }
}

/// The `SIGINT`/`SIGTERM` arms of the watch loop. Dropping it removes the handlers.
struct SignalArms {
    interrupt: Option<tokio::signal::unix::Signal>,
    terminate: Option<tokio::signal::unix::Signal>,
}

impl SignalArms {
    fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        let interrupt = signal(SignalKind::interrupt())
            .inspect_err(|err| tracing::warn!(%err, "could not install the SIGINT handler"))
            .ok();
        let terminate = signal(SignalKind::terminate())
            .inspect_err(|err| tracing::warn!(%err, "could not install the SIGTERM handler"))
            .ok();
        SignalArms {
            interrupt,
            terminate,
        }
    }

    fn disabled() -> Self {
        SignalArms {
            interrupt: None,
            terminate: None,
        }
    }

    /// `None` forever when signals are disabled, which makes the `select!` arm
    /// permanently pending rather than instantly ready.
    async fn next(&mut self) -> Option<&'static str> {
        match (self.interrupt.as_mut(), self.terminate.as_mut()) {
            (Some(interrupt), Some(terminate)) => tokio::select! {
                _ = interrupt.recv() => Some("SIGINT"),
                _ = terminate.recv() => Some("SIGTERM"),
            },
            (Some(interrupt), None) => interrupt.recv().await.map(|()| "SIGINT"),
            (None, Some(terminate)) => terminate.recv().await.map(|()| "SIGTERM"),
            (None, None) => std::future::pending().await,
        }
    }
}
