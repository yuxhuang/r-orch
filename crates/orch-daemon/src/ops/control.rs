//! Dispatch mode, finite observers, and live daemon lifecycle adapters.
use super::{OpContext, ServiceHandler, invalid, service_error};
use crate::coordinator::{Lane, OpResult};
use orch_domain::{Task, is_dispatchable, is_terminal};
use orch_proto::error::{ErrorBody, ErrorCode, Execution};
use orch_proto::frame::{Part, PartKind, Store, limits};
use orch_proto::ops::{args::*, results::*};
use orch_proto::path::WirePath;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub(super) struct ControlState {
    dispatch_on: AtomicBool,
    batch: Mutex<Option<Batch>>,
    next_batch: AtomicU64,
    pub(super) next_subscription: AtomicU64,
    automatic_guard: Mutex<Arc<AtomicBool>>,
    status: Mutex<DaemonStatus>,
    stopped: watch::Sender<Option<bool>>,
    probes: tokio::sync::Semaphore,
    #[cfg(feature = "testing")]
    run_all_pass_hook: Mutex<Option<RunAllPassHook>>,
}
/// Test seam awaited after every finite run-all pass, before the snapshot that
/// decides whether the batch is finished. The argument is whether the pass
/// launched anything.
#[cfg(feature = "testing")]
pub type RunAllPassHook = Arc<
    dyn Fn(bool) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;
struct Batch {
    id: u64,
    cancelled: Arc<AtomicBool>,
}
impl ControlState {
    pub(super) fn new(engine: &orch_engine::container::FullEngine) -> Self {
        let instance = engine.ids.generate("daemon");
        let path = |path| WirePath::new(path).expect("validated daemon path");
        Self {
            dispatch_on: AtomicBool::new(false),
            batch: Mutex::new(None),
            next_batch: AtomicU64::new(1),
            next_subscription: AtomicU64::new(1),
            automatic_guard: Mutex::new(Arc::new(AtomicBool::new(false))),
            stopped: watch::channel(None).0,
            probes: tokio::sync::Semaphore::new(2),
            #[cfg(feature = "testing")]
            run_all_pass_hook: Mutex::new(None),
            status: Mutex::new(DaemonStatus {
                pid: std::process::id(),
                project_root: path(engine.paths.project_root().to_path_buf()),
                socket: path(engine.paths.root().join("run/orch.sock")),
                protocol: orch_proto::LOCAL_PROTOCOL,
                binary: orch_proto::BINARY_VERSION.into(),
                instance,
                executable: path(std::env::current_exe().unwrap_or_default()),
                started_at: engine.clock.now_iso(),
                dispatch: DispatchState {
                    dispatch_on: false,
                    batch_active: false,
                },
                running_tasks: Vec::new(),
                sessions: 0,
                session_cap: limits::MAX_SESSIONS as u32,
                busy_rejections: 0,
                ownership: OwnershipStatus::Held,
                removed_env: Vec::new(),
            }),
        }
    }
    pub(super) fn instance(&self) -> String {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .instance
            .clone()
    }
    pub(super) async fn probe_permit(&self) -> OpResult<tokio::sync::SemaphorePermit<'_>> {
        self.probes.try_acquire().map_err(|_| {
            ErrorBody::new(
                ErrorCode::Overloaded,
                "daemon probe capacity is full",
                Execution::NotExecuted,
            )
        })
    }
}

struct BatchGuard {
    control: Arc<ControlState>,
    engine: Arc<orch_engine::container::FullEngine>,
    id: u64,
}
impl Drop for BatchGuard {
    fn drop(&mut self) {
        let mut batch = self.control.batch.lock().unwrap_or_else(|e| e.into_inner());
        if batch.as_ref().is_some_and(|batch| batch.id == self.id) {
            batch.take();
            self.engine.orchestrator.set_daemon_batch_active(false);
        }
    }
}

#[cfg(feature = "testing")]
impl ServiceHandler {
    pub fn set_run_all_pass_hook(&self, hook: RunAllPassHook) {
        *self
            .control
            .run_all_pass_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }
}
impl ServiceHandler {
    pub fn dispatch_state(&self) -> DispatchState {
        DispatchState {
            dispatch_on: self.control.dispatch_on.load(Ordering::SeqCst),
            batch_active: self
                .control
                .batch
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some(),
        }
    }
    /// Lifecycle owns the actual endpoint, session counters and ownership
    /// heartbeat. Status reads take those live values, never discovery files.
    pub fn configure_status(&self, status: DaemonStatus) {
        *self
            .control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = status;
    }
    pub(crate) fn ownership_lost(&self) -> bool {
        self.control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ownership
            == OwnershipStatus::Lost
    }
    pub async fn set_ownership_lost(&self) -> OpResult<()> {
        let handler = self.clone();
        self.coordinator
            .run_op(Lane::Control, move |permit| {
                Box::pin(async move {
                    permit.invalidate(Store::State);
                    handler.apply_ownership_lost();
                    Ok(())
                })
            })
            .await
            .map(|_| ())
    }
    fn apply_ownership_lost(&self) {
        self.control.dispatch_on.store(false, Ordering::SeqCst);
        self.end_batch();
        self.control
            .automatic_guard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store(true, Ordering::SeqCst);
        self.control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ownership = OwnershipStatus::Lost;
        for cancelled in self
            .observations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            cancelled.store(true, Ordering::SeqCst);
        }
    }
    pub fn update_session_counts(&self, sessions: u32, busy_rejections: u64) {
        let mut status = self
            .control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        status.sessions = sessions;
        status.busy_rejections = busy_rejections;
    }
    /// Listener waits for cleanup outcome, sends the stop terminal frame, then
    /// removes its own endpoint/discovery generation and releases ownership.
    pub fn stopped(&self) -> watch::Receiver<Option<bool>> {
        self.control.stopped.subscribe()
    }
    fn end_batch(&self) {
        if let Some(batch) = self
            .control
            .batch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            batch.cancelled.store(true, Ordering::SeqCst);
            self.engine.orchestrator.set_daemon_batch_active(false);
        }
    }
    pub(super) async fn dispatch_set(
        &self,
        _: &OpContext,
        a: DispatchSetArgs,
    ) -> OpResult<DispatchState> {
        if self
            .control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .ownership
            == OwnershipStatus::Lost
        {
            return Err(conflict("daemon ownership was lost"));
        }
        self.end_batch();
        let mut automatic = self
            .control
            .automatic_guard
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        automatic.store(true, Ordering::SeqCst);
        *automatic = Arc::new(AtomicBool::new(false));
        self.control
            .dispatch_on
            .store(a.dispatch == DispatchSwitch::On, Ordering::SeqCst);
        Ok(self.dispatch_state())
    }
    /// Automatic loop calls this only after checking mode. No lifecycle lock
    /// is reacquired and expiry remains a separate dispatch-OFF operation.
    pub async fn automatic_tick(&self) -> OpResult<()> {
        let guard = {
            let guard = self
                .control
                .automatic_guard
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !self.control.dispatch_on.load(Ordering::SeqCst) || self.coordinator.is_stopping() {
                return Ok(());
            }
            Arc::clone(&guard)
        };
        self.engine
            .orchestrator
            .daemon_tick_with_guard(false, guard)
            .await
            .map_err(service_error)?;
        Ok(())
    }
    /// Dedicated maintenance clock, called even when automatic dispatch is OFF.
    pub async fn expire_context(&self) -> OpResult<u64> {
        let engine = Arc::clone(&self.engine);
        Ok(self
            .engine
            .operation_boundary()
            .with_source(
                orch_engine::container::commit::CommitSource::Expiry,
                self.coordinator.run_op(Lane::Internal, move |_| {
                    Box::pin(async move {
                        Ok(engine
                            .context_service
                            .expire_now()
                            .await
                            .map_err(service_error)?
                            .len() as u64)
                    })
                }),
            )
            .await?
            .value)
    }
    /// Runtime maintenance is independent of the dispatch switch. Lifecycle
    /// starts this owner task after recovery and joins it during shutdown.
    pub async fn run_maintenance(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !self.coordinator.is_stopping() {
            interval.tick().await;
            if self.coordinator.is_stopping() {
                break;
            }
            let engine = Arc::clone(&self.engine);
            if let Err(error) = self
                .coordinator
                .run_op(Lane::Internal, move |_| {
                    Box::pin(async move {
                        engine
                            .orchestrator
                            .daemon_reconcile_dead_local_runs()
                            .await
                            .map_err(service_error)
                    })
                })
                .await
            {
                self.engine
                    .events
                    .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                        error: error.message.to_string(),
                        context: "local run maintenance".into(),
                        fatal: false,
                    });
            }
            if let Err(error) = self.expire_context().await {
                self.engine
                    .events
                    .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                        error: error.message.to_string(),
                        context: "context expiry".into(),
                        fatal: false,
                    });
            }
            self.transfers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .expire();
        }
    }

    pub(super) async fn dispatch_tick(
        &self,
        ctx: &OpContext,
        _: DispatchTickArgs,
    ) -> OpResult<TickResult> {
        let mut events = self.engine.events.subscribe();
        let before = self.read_tasks().await?;
        let skip = self.dispatch_state().batch_active;
        self.engine
            .orchestrator
            .daemon_tick_with_guard(skip, Arc::clone(&ctx.cancelled))
            .await
            .map_err(service_error)?;
        let after = self.read_tasks().await?;
        let launched = after
            .into_iter()
            .filter(|task| {
                before
                    .iter()
                    .any(|old| old.id == task.id && task.attempts > old.attempts)
            })
            .map(|task| task.id)
            .collect();
        self.send_event_parts(ctx, &mut 0, &mut events).await?;
        Ok(TickResult { launched })
    }
    pub(super) async fn dispatch_run_task(
        &self,
        ctx: &OpContext,
        a: DispatchRunTaskArgs,
    ) -> OpResult<RunOutcome> {
        let events = self.engine.events.subscribe();
        let (_, before, _) = self.read_execution_snapshot().await?;
        self.engine
            .orchestrator
            .daemon_launch_task_with_guard(&a.task_id, Arc::clone(&ctx.cancelled), true)
            .await
            .map_err(service_error)?;
        let (_, runs, state) = self.read_execution_snapshot().await?;
        let run = runs
            .iter()
            .find(|run| {
                run.task_id == a.task_id.as_str() && !before.iter().any(|old| old.id == run.id)
            })
            .or_else(|| {
                state
                    .running
                    .get(&*a.task_id)
                    .and_then(|entry| runs.iter().find(|run| run.id == entry.run_id))
            })
            .ok_or_else(|| conflict("task is not eligible for launch"))?;
        self.observe_runs(ctx, vec![(a.task_id.into_inner(), run.id.clone())], events)
            .await
    }
    pub(super) async fn dispatch_run_all(
        &self,
        ctx: &OpContext,
        _: DispatchRunAllArgs,
    ) -> OpResult<RunOutcome> {
        let mut events = self.engine.events.subscribe();
        let mut settled = self.read_execution_snapshot().await?;
        let mut pending = settled
            .0
            .iter()
            .filter(|task| is_dispatchable(task.status) && !task.awaits_approval())
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        let mut targets = Vec::<(String, String)>::new();
        let mut n = 0;
        loop {
            if ctx.is_cancelled() || self.coordinator.is_stopping() {
                return Err(conflict(
                    "observation interrupted; runs continue in the daemon",
                ));
            }
            let mut progressed = false;
            for id in pending.clone() {
                let (_, before, _) = self.read_execution_snapshot().await?;
                self.engine
                    .orchestrator
                    .daemon_launch_task_with_guard(&id, Arc::clone(&ctx.cancelled), false)
                    .await
                    .map_err(service_error)?;
                let (tasks, runs, state) = self.read_execution_snapshot().await?;
                let run = runs
                    .iter()
                    .find(|run| run.task_id == id && !before.iter().any(|old| old.id == run.id))
                    .or_else(|| {
                        state
                            .running
                            .get(&id)
                            .and_then(|entry| runs.iter().find(|run| run.id == entry.run_id))
                    });
                if let Some(run) = run {
                    targets.push((id.clone(), run.id.clone()));
                    pending.retain(|task| task != &id);
                    progressed = true;
                } else if tasks
                    .iter()
                    .find(|task| task.id == id)
                    .is_none_or(|task| !is_dispatchable(task.status))
                {
                    pending.retain(|task| task != &id);
                }
            }
            #[cfg(feature = "testing")]
            {
                let hook = self
                    .control
                    .run_all_pass_hook
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                if let Some(hook) = hook {
                    hook(progressed).await;
                }
            }
            let snapshot = self.read_execution_snapshot().await?;
            // A pass that launched nothing proves the rest ineligible only if
            // the project stayed idle and unchanged since the previous snapshot:
            // a run that ends mid-pass frees capacity (or a dependency) after
            // the refusal was decided, so that work must be tried again.
            let (_, runs, state) = &snapshot;
            let exhausted = !progressed
                && state.running.is_empty()
                && state.claimed.is_empty()
                && snapshot == settled;
            if runs_finished(&targets, runs)? && (pending.is_empty() || exhausted) {
                self.send_event_parts(ctx, &mut n, &mut events).await?;
                let (tasks, runs, _) = snapshot;
                return Ok(run_outcome(&targets, tasks, &runs));
            }
            settled = snapshot;
            self.send_event_parts(ctx, &mut n, &mut events).await?;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub(super) async fn dispatch_until_terminal(
        &self,
        ctx: &OpContext,
        _: DispatchUntilTerminalArgs,
    ) -> OpResult<BatchResult> {
        let control = Arc::clone(&self.control);
        let observation_cancel = Arc::clone(&ctx.cancelled);
        let engine = Arc::clone(&self.engine);
        let (id, cancelled) = self
            .coordinator
            .run_op(Lane::Control, move |_| {
                Box::pin(async move {
                    if control.dispatch_on.load(Ordering::SeqCst) {
                        return Err(invalid("dispatch is ON; serve --once needs dispatch OFF"));
                    }
                    let mut batch = control.batch.lock().unwrap_or_else(|e| e.into_inner());
                    if batch.is_some() {
                        let mut error = conflict("another serve --once batch is active");
                        error.exit_code = 4;
                        return Err(error);
                    }
                    let id = control.next_batch.fetch_add(1, Ordering::Relaxed);
                    let cancelled = observation_cancel;
                    *batch = Some(Batch {
                        id,
                        cancelled: Arc::clone(&cancelled),
                    });
                    engine.orchestrator.set_daemon_batch_active(true);
                    Ok((id, cancelled))
                })
            })
            .await?
            .value;
        let _batch = BatchGuard {
            control: Arc::clone(&self.control),
            engine: Arc::clone(&self.engine),
            id,
        };
        let mut n = 0;
        let mut events = self.engine.events.subscribe();
        loop {
            if ctx.is_cancelled()
                || cancelled.load(Ordering::SeqCst)
                || self.coordinator.is_stopping()
            {
                cancelled.store(true, Ordering::SeqCst);
                return Ok(BatchResult::Interrupted);
            }
            self.engine
                .orchestrator
                .daemon_tick_with_guard(true, Arc::clone(&cancelled))
                .await
                .map_err(service_error)?;
            self.send_event_parts(ctx, &mut n, &mut events).await?;
            let tasks = self.read_tasks().await?;
            if ctx.is_cancelled() || cancelled.load(Ordering::SeqCst) {
                return Ok(BatchResult::Interrupted);
            }
            if tasks.iter().all(|task| is_terminal(task.status)) {
                return Ok(
                    if tasks
                        .iter()
                        .any(|task| task.status == orch_domain::TaskStatus::Failed)
                    {
                        BatchResult::HasFailed
                    } else {
                        BatchResult::AllDone
                    },
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub(super) async fn daemon_status(
        &self,
        _: &OpContext,
        _: DaemonStatusArgs,
    ) -> OpResult<DaemonStatus> {
        let mut status = self
            .control
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        status.dispatch = self.dispatch_state();
        status.running_tasks = self
            .engine
            .state
            .read()
            .await
            .map_err(service_error)?
            .running
            .into_keys()
            .collect();
        Ok(status)
    }
    pub(super) async fn daemon_stop(
        &self,
        _: &OpContext,
        _: DaemonStopArgs,
    ) -> OpResult<StopOutcome> {
        let drain_deadline = tokio::time::Instant::now() + crate::lifecycle::SESSION_DRAIN;
        self.coordinator.stop_admission();
        self.diagnostics.spawn_work().seal();
        self.control.dispatch_on.store(false, Ordering::SeqCst);
        self.end_batch();
        self.control
            .automatic_guard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .store(true, Ordering::SeqCst);
        let result = self.stop_execution().await.map_err(service_error);
        let result = match result {
            Ok(()) => {
                tokio::time::timeout_at(drain_deadline, self.diagnostics.spawn_work().wait_idle())
                    .await
                    .map_err(|_| {
                        ErrorBody::new(
                            ErrorCode::General,
                            "Daemon shutdown incomplete; diagnostic spawn work remains",
                            Execution::Unknown,
                        )
                    })
            }
            Err(error) => Err(error),
        };
        self.control.stopped.send_replace(Some(result.is_ok()));
        result.map(|()| StopOutcome { clean: true })
    }
    pub(super) async fn logs_follow(&self, ctx: &OpContext, a: LogsFollowArgs) -> OpResult<()> {
        let mut events = self.engine.events.subscribe();
        let mut n = 0;
        while !ctx.is_cancelled() && !self.coordinator.is_stopping() {
            tokio::select! {
                event=events.recv()=> {
                    let Some(event)=event else {break};
                    if !self.log_event_matches(&a,event.as_ref()).await? {continue;}
                    self.send_part(ctx,&mut n,event.as_ref()).await?;
                }
                ()=tokio::time::sleep(Duration::from_millis(25))=>{}
            }
        }
        Ok(())
    }
    async fn log_event_matches(
        &self,
        args: &LogsFollowArgs,
        event: &orch_domain::OrchestratorEvent,
    ) -> OpResult<bool> {
        let value = serde_json::to_value(event).map_err(|_| invalid("event cannot be encoded"))?;
        let run_id = value.get("runId").and_then(|v| v.as_str());
        if args.run_id.as_ref().is_some_and(|id| run_id != Some(&**id)) {
            return Ok(false);
        }
        let mut task_id = value
            .get("taskId")
            .and_then(|v| v.as_str())
            .or_else(|| {
                value
                    .get("task")
                    .and_then(|t| t.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(str::to_owned);
        let mut agent_id = value
            .get("agentId")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        if let Some(run_id) = run_id
            && (args.task_id.is_some() && task_id.is_none()
                || args.agent_id.is_some() && agent_id.is_none())
        {
            let engine = Arc::clone(&self.engine);
            let run_id = run_id.to_owned();
            if let Some(run) = self
                .coordinator
                .run_op(Lane::Internal, move |_| {
                    Box::pin(
                        async move { engine.run_service.get(&run_id).await.map_err(service_error) },
                    )
                })
                .await?
                .value
            {
                task_id = Some(run.task_id);
                agent_id = Some(run.agent_id);
            }
        }
        Ok(args
            .task_id
            .as_ref()
            .is_none_or(|id| task_id.as_deref() == Some(&**id))
            && args
                .agent_id
                .as_ref()
                .is_none_or(|id| agent_id.as_deref() == Some(&**id)))
    }
    async fn read_tasks(&self) -> OpResult<Vec<Task>> {
        let engine = Arc::clone(&self.engine);
        Ok(self
            .coordinator
            .run_op(Lane::Internal, move |_| {
                Box::pin(async move {
                    engine
                        .task_service
                        .list(&Default::default())
                        .await
                        .map_err(service_error)
                })
            })
            .await?
            .value)
    }
    async fn read_execution_snapshot(
        &self,
    ) -> OpResult<(
        Vec<Task>,
        Vec<orch_domain::Run>,
        orch_domain::OrchestratorState,
    )> {
        let engine = Arc::clone(&self.engine);
        Ok(self
            .coordinator
            .run_op(Lane::Internal, move |_| {
                Box::pin(async move {
                    Ok((
                        engine
                            .task_service
                            .list(&Default::default())
                            .await
                            .map_err(service_error)?,
                        engine.run_service.list_all().await.map_err(service_error)?,
                        engine.state.read().await.map_err(service_error)?,
                    ))
                })
            })
            .await?
            .value)
    }
    async fn observe_runs(
        &self,
        ctx: &OpContext,
        targets: Vec<(String, String)>,
        mut events: orch_engine::events::EventSubscription,
    ) -> OpResult<RunOutcome> {
        let mut n = 0;
        loop {
            let (tasks, runs, _) = self.read_execution_snapshot().await?;
            self.send_event_parts(ctx, &mut n, &mut events).await?;
            if runs_finished(&targets, &runs)? {
                return Ok(run_outcome(&targets, tasks, &runs));
            }
            if ctx.is_cancelled() || self.coordinator.is_stopping() {
                return Err(conflict(
                    "observation interrupted; runs continue in the daemon",
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    async fn send_event_parts(
        &self,
        ctx: &OpContext,
        n: &mut u64,
        events: &mut orch_engine::events::EventSubscription,
    ) -> OpResult<()> {
        for event in events.try_drain() {
            if ctx.is_cancelled() {
                break;
            }
            self.send_part(ctx, n, event.as_ref()).await?;
        }
        Ok(())
    }
    async fn send_part<T: serde::Serialize>(
        &self,
        ctx: &OpContext,
        n: &mut u64,
        value: &T,
    ) -> OpResult<()> {
        if ctx.is_cancelled() {
            return Ok(());
        }
        if let Some(parts) = &ctx.parts {
            let part = Part {
                n: *n,
                kind: PartKind::Event,
                data: serde_json::value::to_raw_value(value)
                    .map_err(|_| invalid("event cannot be encoded"))?,
            };
            *n += 1;
            if !matches!(
                tokio::time::timeout(orch_proto::frame::WRITE_TIMEOUT, parts.send(part)).await,
                Ok(Ok(()))
            ) {
                ctx.cancel_observation();
            }
        }
        Ok(())
    }
}
fn conflict(message: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::Conflict, message, Execution::NotExecuted)
}

fn runs_finished(targets: &[(String, String)], runs: &[orch_domain::Run]) -> OpResult<bool> {
    for (_, id) in targets {
        let run = runs
            .iter()
            .find(|run| &run.id == id)
            .ok_or_else(|| conflict("observed run record disappeared"))?;
        if matches!(
            run.status,
            orch_domain::RunStatus::Preparing | orch_domain::RunStatus::Running
        ) {
            return Ok(false);
        }
    }
    Ok(true)
}
fn run_outcome(
    targets: &[(String, String)],
    tasks: Vec<Task>,
    runs: &[orch_domain::Run],
) -> RunOutcome {
    RunOutcome {
        tasks: tasks
            .into_iter()
            .filter(|task| targets.iter().any(|(id, _)| id == &task.id))
            .collect(),
        exit_code: i32::from(targets.iter().any(|(_, id)| {
            runs.iter()
                .find(|run| &run.id == id)
                .is_some_and(|run| run.status != orch_domain::RunStatus::Succeeded)
        })),
    }
}
