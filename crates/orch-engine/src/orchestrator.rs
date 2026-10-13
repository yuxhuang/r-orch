//! The orchestrator — ported from `src/application/orchestrator.ts`.
//!
//! Conceptually **reconcile → dispatch → collect**, but only the first two are
//! tick phases: collection runs as a detached task per run and completion is
//! handled by [`Orchestrator::handle_run_success`] /
//! [`Orchestrator::handle_run_failure`], each of which takes the state lock
//! (PARITY.md §7.3).
//!
//! # Concurrency
//!
//! The TS code keeps two things that must stay coupled: a promise-chain mutex
//! (`withStateLock`) and the state object it guards (`this.state`). Here they
//! are **one** `tokio::sync::Mutex<Option<OrchestratorState>>`:
//!
//! - Every public entry point that the TS wraps in `withStateLock` — `tick`,
//!   `stop`, `cancel_task`, `force_stop_agent`, `fresh_dispatch`,
//!   `handle_run_success`, `handle_run_failure` — acquires the guard and holds
//!   it across its store I/O, exactly as the TS does. That serialization is
//!   load-bearing: the whole `activeCollectors` / early-`delete running[id]` /
//!   re-read-before-save bug history is about it.
//! - Every internal phase takes `&mut OrchestratorState` as a parameter rather
//!   than re-locking, so the compiler *proves* the TS deadlock that
//!   `_handleRunFailure` exists to avoid cannot be reintroduced.
//!
//! One consequence is worth stating: `collect_events` updates
//! `running[task].last_event_at` under the same guard, so that write waits for
//! an in-flight phase instead of interleaving with it as it does in TS. The
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

/// Max serialized event data written to JSONL (8 KB) — PARITY.md §7.20 layer 2.
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

/// Everything the orchestrator needs. Mirrors the TS `OrchestratorDeps`, with
/// `lockPath: string` replaced by an injected [`ProjectLock`] and the two
/// determinism seams added (ARCHITECTURE §5.6).
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
    /// not shell out to `npm`, and because the differential harness forbids
    /// the oracle spawning it at all (PARITY.md V31).
    pub command_runner: Arc<dyn crate::review_runner::CommandRunner>,
    pub config: OrchestratorConfig,
    pub project_root: PathBuf,
    /// Optional in TS, and the test helper leaves several of them out.
    pub context_store: Option<Arc<dyn orch_core::storage::ContextStore>>,
    pub message_service: Option<Arc<MessageService>>,
    pub goal_store: Option<Arc<dyn GoalStore>>,
    pub skill_loader: Option<Arc<dyn SkillLoader>>,
}

#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// `tick()` skips `seed_autonomous_tasks` — `serve --once` sets it.
    pub skip_autonomous_seeding: bool,
    /// Install `SIGINT`/`SIGTERM` arms in the watch loop. Tests turn it off
    /// and drive [`Orchestrator::handle_signal`] directly, because a real
    /// signal would hit the test process itself.
    pub handle_signals: bool,
}

impl WatchOptions {
    /// The production default: signals handled, seeding on.
    pub fn watching() -> Self {
        WatchOptions {
            skip_autonomous_seeding: false,
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
    /// Per-agent auto-seed cooldown. In-memory, as in TS — a restart reseeds.
    last_auto_seed_at: Mutex<HashMap<String, i64>>,

    lazy_save: Mutex<LazySave>,
    immediate_dispatch: Mutex<Option<tokio::task::JoinHandle<()>>>,
    watch_task: Mutex<Option<tokio::task::JoinHandle<()>>>,

    lock_acquired: AtomicBool,
    shutting_down: AtomicBool,
    tick_in_progress: AtomicBool,
    skip_autonomous_seeding: AtomicBool,
    consecutive_tick_failures: AtomicU32,

    /// Cancels the watch loop.
    shutdown: CancellationToken,
    /// Flips to `true` once `stop` has finished, releasing `wait_for_stop`.
    stopped: watch::Sender<bool>,

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
                deps,
                state: Mutex::new(None),
                active_collectors: Mutex::new(HashSet::new()),
                cancels: Mutex::new(HashMap::new()),
                single_task_runs: Mutex::new(HashSet::new()),
                last_auto_seed_at: Mutex::new(HashMap::new()),
                lazy_save: Mutex::new(LazySave::default()),
                immediate_dispatch: Mutex::new(None),
                watch_task: Mutex::new(None),
                lock_acquired: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                tick_in_progress: AtomicBool::new(false),
                skip_autonomous_seeding: AtomicBool::new(false),
                consecutive_tick_failures: AtomicU32::new(0),
                shutdown: CancellationToken::new(),
                stopped,
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

    fn emit(&self, event: OrchestratorEvent) {
        self.inner.deps.events.emit(event);
    }

    /// `orchestrator:error { fatal: false }` — the TS code's universal
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
            return Err(Error::Domain(DomainError::LockConflict(
                outcome.pid.unwrap_or(0),
            )));
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
        *slot = Some(self.deps().state_store.read().await?);
        Ok(())
    }

    async fn save_state(&self, state: &OrchestratorState) -> Result<()> {
        self.deps().state_store.write(state).await
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
    /// Returns once the loop is running — like the TS method, which sets an
    /// interval and resolves. Use [`Orchestrator::wait_for_stop`] to block.
    pub async fn start_watch(&self, options: WatchOptions) -> Result<()> {
        self.inner
            .skip_autonomous_seeding
            .store(options.skip_autonomous_seeding, Ordering::SeqCst);

        let outcome = self.deps().lock.acquire().await;
        if !outcome.acquired {
            return Err(Error::Domain(DomainError::LockConflict(
                outcome.pid.unwrap_or(0),
            )));
        }
        self.inner.lock_acquired.store(true, Ordering::SeqCst);

        {
            let mut slot = self.inner.state.lock().await;
            self.load_state(&mut slot).await?;
            let state = slot.as_mut().expect("state loaded");
            // A previous process may have died mid-run: drop its running
            // entries and cancel the work rather than retrying it, so agents
            // do not redo already-committed work (PARITY.md §7.6).
            self.cleanup_stale_running_entries(state).await?;
            state.pid = Some(std::process::id());
            state.started_at = Some(self.deps().clock.now_iso());
            self.save_state(state).await?;
        }

        // Reactive dispatch: a task created by any process in this binary
        // should not wait for the next poll.
        let reactive = self.clone();
        let _unsub = self.deps().events.on(
            orch_domain::OrchestratorEventType::TaskCreated,
            move |_event| {
                reactive.schedule_immediate_dispatch(0);
            },
        );

        self.tick_once().await?;
        self.spawn_watch_loop(options.handle_signals, _unsub);
        Ok(())
    }

    /// Resolves once [`Orchestrator::stop`] has completed. Returns immediately
    /// when shutdown has already begun, matching the TS `waitForStop`.
    pub async fn wait_for_stop(&self) {
        if self.is_shutting_down() {
            return;
        }
        let mut stopped = self.inner.stopped.subscribe();
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

    /// Stop the loop and clean up. Idempotent.
    ///
    /// Every running agent is aborted, killed with a 10 s grace, its run
    /// finished as `cancelled` and its task sent to `resolve_failure_status` so
    /// it retries if budget remains (PARITY.md §7.14).
    pub async fn stop(&self) -> Result<()> {
        if self.inner.shutting_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        self.inner.shutdown.cancel();
        // Taken either way, so the loop is detached; aborted **only** from
        // outside it. Both the signal arms and the tick circuit breaker call
        // `stop()` from inside the watch task, where `abort()` would cancel
        // the caller at its next pending await — the state save — leaving the
        // lock held and `stopped` unsignalled, so `wait_for_stop` would never
        // return and `serve`/`run --watch` would ignore Ctrl+C. The loop does
        // not need aborting from there anyway: `shutdown.cancel()` above
        // breaks it, and the arm that called us breaks immediately after.
        if let Some(task) = self.inner.watch_task.lock().await.take()
            && !in_watch_loop()
        {
            task.abort();
        }
        if let Some(timer) = self.inner.immediate_dispatch.lock().await.take() {
            timer.abort();
        }
        self.cancel_lazy_save().await;

        // The save is allowed to fail without aborting the rest of the
        // shutdown. The TS code `await`s it inside the critical section and
        // lets a rejection propagate, which skips the lock release and wedges
        // the project for every later invocation — reachable here because the
        // tick circuit breaker calls `stop()` precisely when the state store
        // is the thing that is broken. The error is still returned, so a
        // caller sees it; it just no longer costs the lock.
        let saved = {
            let mut slot = self.inner.state.lock().await;
            match slot.as_mut() {
                Some(state) => {
                    self.shutdown_running_entries(state).await;
                    state.running.clear();
                    state.claimed.clear();
                    state.pid = None;
                    state.started_at = None;
                    self.save_state(state).await
                }
                None => Ok(()),
            }
        };

        if self.inner.lock_acquired.swap(false, Ordering::SeqCst) {
            self.deps().lock.release().await;
        }

        let _ = self.inner.stopped.send(true);
        if let Err(err) = &saved {
            self.emit_error(err, "stop: persist the cleared state");
        }
        saved
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
            self.take_cancel(task_id).await;
            self.deps()
                .processes
                .kill_with_grace(entry.pid, CANCEL_KILL_GRACE)
                .await;
            if let Err(err) = self
                .deps()
                .run_service
                .finish(&entry.run_id, orch_domain::RunStatus::Cancelled, None, None)
                .await
            {
                self.emit_error(err, format!("cancelTask finish run {}", entry.run_id));
            }
            if let Err(err) = self
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
            self.take_cancel(&task_id).await;
            self.deps()
                .processes
                .kill_with_grace(entry.pid, CANCEL_KILL_GRACE)
                .await;
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

        self.deps()
            .agent_service
            .set_status(agent_id, orch_domain::AgentStatus::Idle)
            .await?;
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

    /// Remove the token and cancel it if there was one — the TS
    /// `abortControllers.get(id)?.abort(); abortControllers.delete(id)` pair.
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

    fn spawn_watch_loop(&self, handle_signals: bool, unsubscribe: crate::events::Unsubscribe) {
        let orchestrator = self.clone();
        let interval_ms = self.config().scheduling.poll_interval_ms.max(1);
        let shutdown = self.inner.shutdown.clone();

        let task = tokio::spawn(IN_WATCH_LOOP.scope((), async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
            // `setInterval` does not catch up on missed ticks; tokio's default
            // `Burst` would fire several back-to-back after a slow tick and
            // change dispatch timing.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first `tick()` on a fresh interval resolves immediately and
            // `start_watch` has already run one tick, so discard it.
            ticker.tick().await;

            let mut signals = if handle_signals {
                SignalArms::install()
            } else {
                SignalArms::disabled()
            };

            loop {
                tokio::select! {
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
                    _ = shutdown.cancelled() => break,
                }
            }
            unsubscribe.unsubscribe();
        }));

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            *inner.watch_task.lock().await = Some(task);
        });
    }

    /// The circuit breaker: five consecutive rejections shut the orchestrator
    /// down, and every failure reports its running count so a watcher can see
    /// the breaker approaching (PARITY.md §7.3).
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

tokio::task_local! {
    /// Set for the lifetime of the watch-loop task, so [`Orchestrator::stop`]
    /// can tell "called from the loop" apart from "called by the CLI" and not
    /// abort the task it is running in. See the comment in `stop`.
    static IN_WATCH_LOOP: ();
}

/// Whether this is the watch-loop task.
fn in_watch_loop() -> bool {
    IN_WATCH_LOOP.try_with(|()| ()).is_ok()
}

/// The `SIGINT`/`SIGTERM` arms of the watch loop, replacing the TS
/// add/remove-listener dance. Dropping it removes the handlers.
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
