//! The worker owns mirrors and execution. A disconnected controller never
//! cancels a process; only an explicit cancel or a successful sync can do so.
//!
//! # Error barrier
//!
//! A mirror's storage fault degrades that key, never the link. One mirror this
//! instance cannot read used to error the whole tick, which closed the socket;
//! the controller then redialled, hit the same mirror, and the peer looked
//! permanently offline while the worker logged nothing. So every fallible read
//! is caught by class instead:
//!
//! - per key (`item`): report `unknown` with a reason, and warn once per fault;
//! - per session (`mirrors`, `local_agents`, `outbox`, `updates`): log, emit a
//!   debounced `OrchestratorError`, skip this tick, keep the session;
//! - fatal, closing the session: transport, the handshake, and "expected sync".

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orch_core::adapter::AgentEvent;
use orch_core::error::{Error, Result};
use orch_core::remote::{RemoteOutcome, RunEventTap};
use orch_core::storage::TaskFilter;
use orch_domain::{
    AWAITING_APPROVAL_LABEL, AWAITING_APPROVAL_REASON, AgentStatus, ApprovalPolicy,
    CreateTaskInput, InboundStatus, Run, RunStatus, Task, TaskStatus,
};
use orch_infra::storage::{
    fs_utils::{owner_only, read_json, write_json_with_mode},
    remote::validate_snapshot,
};
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

use crate::codec::{read_frame, write_frame};
use crate::protocol::{Frame, PROTOCOL, SyncItem};
use crate::{AbortOnDrop, Context};

const RING_CAPACITY: usize = 1000;
const DEAD_AFTER: Duration = Duration::from_secs(15);
const PING_EVERY: Duration = Duration::from_secs(5);
// Includes the controller and replaced sessions still shutting down. Admission
// happens before accept/spawn, so incomplete hellos cannot each allocate 1 MiB
// or consume a descriptor without a bound. An active controller leaves room
// for reconnects and same-instance takeover.
const MAX_CONNECTIONS: usize = 16;
const ACCEPT_RETRY_MIN: Duration = Duration::from_millis(100);
const ACCEPT_RETRY_MAX: Duration = Duration::from_secs(2);
/// One `OrchestratorError` per fault kind per window. A fault that recurs on
/// every one-second tick must stay visible without flooding the feed.
const FAULT_REPORT_EVERY: Duration = Duration::from_secs(30);
/// How often the orphan sweep runs, besides once at `listen()`.
const ORPHAN_GC_EVERY: Duration = Duration::from_secs(3_600);
/// How long a controller may stay away before its queued mirrors are
/// cancelled. Long enough to cover an overnight laptop and a weekend's
/// worth of a paused controller; short enough that a decommissioned one
/// does not hold a local agent's queue for ever.
const CONTROLLER_ABSENCE_LIMIT_MS: i64 = 24 * 3_600_000;
/// The longest wait between attempts at a cancel that will not apply.
const CANCEL_RETRY_MAX: Duration = Duration::from_secs(60);
/// How often a cancel that still will not apply is warned about again.
const CANCEL_REWARN_EVERY: Duration = Duration::from_secs(3_600);
/// Deferred cancels are peer-driven, so the map is bounded.
const MAX_UNAPPLIED_CANCELS: usize = 1024;

#[derive(Default)]
struct EventRing {
    seq: u64,
    events: VecDeque<(u64, AgentEvent)>,
}

impl EventRing {
    fn push(&mut self, event: AgentEvent) -> u64 {
        self.seq += 1;
        if self.events.len() == RING_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back((self.seq, event));
        self.seq
    }

    fn after(&self, seq: u64) -> Vec<(u64, AgentEvent)> {
        self.events
            .iter()
            .filter(|(n, _)| *n > seq)
            .cloned()
            .collect()
    }
}

#[derive(Default)]
struct TapState {
    /// task id -> (execution key, controller instance id).
    tasks: HashMap<String, (String, String)>,
    rings: HashMap<String, EventRing>,
}

struct Controller {
    instance_id: String,
    name: String,
    generation: u64,
    cancel: watch::Sender<bool>,
}

#[derive(Default)]
struct SessionSlot {
    generation: u64,
    controller: Option<Controller>,
}

impl SessionSlot {
    fn claim(
        &mut self,
        instance_id: &str,
        name: &str,
    ) -> std::result::Result<(u64, watch::Receiver<bool>), String> {
        if let Some(current) = &self.controller {
            if current.instance_id != instance_id {
                return Err(current.name.clone());
            }
            let _ = current.cancel.send(true);
        }
        self.generation += 1;
        let (cancel, receiver) = watch::channel(false);
        self.controller = Some(Controller {
            instance_id: instance_id.into(),
            name: name.into(),
            generation: self.generation,
            cancel,
        });
        Ok((self.generation, receiver))
    }

    fn is_current(&self, generation: u64) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|c| c.generation == generation)
    }
}

/// One cancel awaiting a retry.
struct PendingCancel {
    /// When the next attempt is due.
    next_at: Instant,
    /// Consecutive failures, which set the delay.
    attempts: u32,
    /// When this key was last warned about, so a stuck one is re-warned
    /// hourly rather than once ever.
    warned_at: Instant,
}

/// `1s · 2^(attempts-1)`, capped. A mirror whose record is damaged is not
/// going to be readable a millisecond later.
fn cancel_retry_delay(attempts: u32) -> Duration {
    Duration::from_secs(1u64 << attempts.min(6).saturating_sub(1)).min(CANCEL_RETRY_MAX)
}

/// The same bounds `Submit` puts on an execution key. It reaches a log line,
/// a task label and the pending-cancel map, all from peer input.
fn valid_exec_key(exec_key: &str) -> bool {
    !exec_key.is_empty() && exec_key.len() <= 256
}

#[derive(Default)]
struct Faults {
    /// Execution key -> the reason last reported for it, so a key that stays
    /// unreadable is warned about once rather than on every tick.
    degraded: HashMap<String, String>,
    /// Fault kind -> when it was last reported to the event feed.
    reported: HashMap<&'static str, Instant>,
    /// Cancels this instance accepted but could not apply, usually because
    /// the mirror's record was unreadable at the time. Retried on their own
    /// backoff until they take: a mirror the controller has given up on must
    /// not keep a local agent busy, and only an explicit cancel clears one
    /// between reconnects.
    unapplied_cancels: BTreeMap<String, PendingCancel>,
    /// Why this instance's last controller session ended, published in
    /// `_listener.json` so `orch remote status` can explain a peer that keeps
    /// reconnecting.
    last_session_error: Option<(String, String)>,
}

pub struct Worker {
    ctx: Context,
    tap: Mutex<TapState>,
    events: broadcast::Sender<(String, Frame)>,
    /// Serializes takeover and protocol mutations, so the replaced session
    /// cannot mutate a mirror after the new controller's sync.
    session: tokio::sync::Mutex<SessionSlot>,
    faults: Mutex<Faults>,
}

impl Worker {
    pub fn new(ctx: Context) -> Arc<Self> {
        let (events, _) = broadcast::channel(RING_CAPACITY);
        Arc::new(Self {
            ctx,
            tap: Mutex::new(TapState::default()),
            events,
            session: tokio::sync::Mutex::new(SessionSlot::default()),
            faults: Mutex::new(Faults::default()),
        })
    }

    fn faults(&self) -> std::sync::MutexGuard<'_, Faults> {
        self.faults.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Emit at most one `OrchestratorError` per `kind` per
    /// [`FAULT_REPORT_EVERY`].
    fn report_fault(&self, kind: &'static str, error: &Error, context: String) {
        let due = {
            let mut faults = self.faults();
            let due = faults
                .reported
                .get(kind)
                .is_none_or(|at| at.elapsed() >= FAULT_REPORT_EVERY);
            if due {
                faults.reported.insert(kind, Instant::now());
            }
            due
        };
        if due {
            self.ctx
                .engine
                .events
                .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                    error: error.to_string(),
                    context,
                    fatal: false,
                });
        }
    }

    /// Log a fault in this instance's own bookkeeping and report it. The
    /// caller skips the work it was doing; the session stays up.
    fn session_fault(&self, operation: &'static str, error: &Error) {
        tracing::warn!(%error, operation, "Peer worker degraded; session kept");
        self.report_fault(
            operation,
            error,
            format!(
                "Peer worker {operation}; the controller session is kept and retries. \
                 Check permissions and free space in {}",
                self.ctx.engine.paths.root().display()
            ),
        );
    }

    fn register(&self, task: &Task) {
        if task
            .labels
            .iter()
            .any(|label| label == "remote-finished-ack")
        {
            return;
        }
        if let (Some(key), Some(controller)) = (label(task, "exec:"), label(task, "remote-ctl:")) {
            self.tap
                .lock()
                .expect("worker event ring poisoned")
                .tasks
                .insert(task.id.clone(), (key.into(), controller.into()));
        }
    }

    fn controllers_path(&self) -> std::path::PathBuf {
        self.ctx
            .engine
            .paths
            .remotes_dir()
            .join("_controllers.json")
    }

    /// When each controller was last admitted here. Owner-only, like every
    /// other file under `remotes/`.
    async fn record_controller(&self, instance_id: &str) -> Result<()> {
        let path = self.controllers_path();
        let mut seen: BTreeMap<String, String> = read_json(&path).await?.unwrap_or_default();
        seen.insert(instance_id.into(), self.ctx.engine.clock.now_iso());
        write_json_with_mode(&path, &seen, owner_only()).await
    }

    /// Cancel the mirrors of controllers that have not come back.
    ///
    /// A mirror only runs for the controller currently connected, so one left
    /// behind by a controller that never returns sits `todo` for ever, in the
    /// queue of a local agent, counted against nothing. After a day without
    /// its controller it is cancelled and said so on the task.
    ///
    /// Only `todo` mirrors: an `in_progress` one has a live local process and
    /// belongs to the quarantine path, which owns that pid. A controller this
    /// instance has never recorded starts its clock now rather than being
    /// cancelled on sight — including every mirror that predates this sweep.
    async fn collect_orphan_mirrors(&self) -> Result<usize> {
        let current = self
            .session
            .lock()
            .await
            .controller
            .as_ref()
            .map(|controller| controller.instance_id.clone());
        let path = self.controllers_path();
        let mut seen: BTreeMap<String, String> = read_json(&path).await?.unwrap_or_default();
        let now = self.ctx.engine.clock.now_ms();
        let tasks = self.ctx.engine.tasks.list(&TaskFilter::default()).await?;
        let mut orphans: Vec<(Task, String, String)> = Vec::new();
        let mut known: Vec<String> = Vec::new();
        if let Some(current) = &current {
            seen.insert(current.clone(), self.ctx.engine.clock.now_iso());
            known.push(current.clone());
        }
        for task in tasks {
            let Some(controller) = label(&task, "remote-ctl:").map(str::to_owned) else {
                continue;
            };
            known.push(controller.clone());
            if current.as_ref() == Some(&controller) || task.status != TaskStatus::Todo {
                continue;
            }
            let Some(last_seen) = seen.get(&controller).cloned() else {
                seen.insert(controller, self.ctx.engine.clock.now_iso());
                continue;
            };
            if orch_core::clock::epoch_ms_from_iso(&last_seen)
                .is_some_and(|at| now.saturating_sub(at) >= CONTROLLER_ABSENCE_LIMIT_MS)
            {
                orphans.push((task, controller, last_seen));
            }
        }
        // A controller with no mirrors left needs no record either.
        seen.retain(|instance_id, _| known.contains(instance_id));
        write_json_with_mode(&path, &seen, owner_only()).await?;

        let mut cancelled = 0;
        for (task, controller, last_seen) in orphans {
            match self
                .cancel_mirror(
                    &task,
                    format!(
                        "orphaned remote mirror: controller {controller} absent since {last_seen}"
                    ),
                )
                .await
            {
                Ok(()) => cancelled += 1,
                Err(error) => self.session_fault("cancel orphan mirror", &error),
            }
        }
        Ok(cancelled)
    }

    async fn listener_status(&self, addr: &str, controller: Option<&str>) -> Result<()> {
        let last_error = self.faults().last_session_error.clone();
        // Owner-only like every file under remotes/ (security review F7).
        write_json_with_mode(
            &self.ctx.engine.paths.remotes_dir().join("_listener.json"),
            &serde_json::json!({
                "listen_addr": addr,
                "controller_name": controller,
                "pid": std::process::id(),
                "last_session_error": last_error.as_ref().map(|(error, _)| error),
                "last_session_error_at": last_error.as_ref().map(|(_, at)| at),
            }),
            owner_only(),
        )
        .await
    }

    /// Record and report why a controller session ended. The session error is
    /// the one thing a worker owner needs when a controller keeps redialling,
    /// and until now the worker logged it at `debug!` with no identity.
    async fn session_ended(&self, instance_id: &str, name: &str, generation: u64, error: &Error) {
        tracing::warn!(
            controller = %name,
            %instance_id,
            generation,
            %error,
            "Peer worker session ended"
        );
        self.faults().last_session_error =
            Some((error.to_string(), self.ctx.engine.clock.now_iso()));
        self.report_fault(
            "session end",
            error,
            format!("Peer worker session with controller {name} ended; awaiting its reconnect"),
        );
    }

    pub async fn listen(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        let addr = listener
            .local_addr()
            .map_err(|e| Error::other(format!("peer listener: {e}")))?
            .to_string();
        for task in self.ctx.engine.tasks.list(&TaskFilter::default()).await? {
            self.register(&task);
        }
        self.listener_status(&addr, None).await?;
        let mut sessions = JoinSet::new();
        let mut accept_after = Instant::now();
        let mut accept_backoff = ACCEPT_RETRY_MIN;
        // Ticks immediately, so the sweep also runs once at startup: a
        // controller that went away while this instance was down is exactly
        // the case that leaves mirrors behind.
        let mut orphan_gc = tokio::time::interval(ORPHAN_GC_EVERY);
        orphan_gc.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                // Reap completed tasks before accepting more, including under
                // a constant stream of short-lived connections.
                Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
                _ = orphan_gc.tick() => {
                    match self.collect_orphan_mirrors().await {
                        Ok(0) => {}
                        Ok(cancelled) => tracing::info!(cancelled, "Cancelled orphaned remote mirrors"),
                        Err(error) => self.session_fault("sweep orphaned mirrors", &error),
                    }
                }
                accepted = async {
                    tokio::time::sleep_until(accept_after).await;
                    listener.accept().await
                }, if sessions.len() < MAX_CONNECTIONS => {
                    let (socket, _) = match accepted {
                        Ok(accepted) => {
                            accept_backoff = ACCEPT_RETRY_MIN;
                            accepted
                        }
                        Err(error) => {
                            // EMFILE/ENFILE and other temporary resource faults
                            // must not drop the listener or active controller.
                            let retry_ms = accept_backoff.as_millis();
                            tracing::warn!(%error, retry_ms, "Peer accept failed; retrying");
                            self.ctx.engine.events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
                                error: format!("peer accept: {error}"),
                                context: format!("Peer listener {addr}; retrying in {retry_ms} ms. Check available file descriptors and memory"),
                                fatal: false,
                            });
                            accept_after = Instant::now() + accept_backoff;
                            accept_backoff = (accept_backoff * 2).min(ACCEPT_RETRY_MAX);
                            continue;
                        }
                    };
                    let worker = self.clone();
                    let addr = addr.clone();
                    sessions.spawn(async move {
                        if let Err(error) = worker.connection(socket, &addr).await {
                            tracing::debug!(%error, "Peer worker session closed");
                        }
                    });
                }
            }
        }
    }

    async fn connection(&self, socket: TcpStream, addr: &str) -> Result<()> {
        socket
            .set_nodelay(true)
            .map_err(|e| Error::other(format!("peer socket: {e}")))?;
        let (read, mut write) = socket.into_split();
        let mut read = BufReader::new(read);
        let hello = timeout(DEAD_AFTER, read_frame(&mut read))
            .await
            .map_err(|_| Error::other("peer handshake timed out"))?
            .map_err(|e| Error::other(format!("peer hello: {e}")))?;
        let Some(Frame::Hello {
            proto,
            instance_id,
            name,
            agents,
            version,
            ..
        }) = hello
        else {
            self.write(
                &mut write,
                &Frame::Error {
                    msg: "expected hello".into(),
                },
            )
            .await?;
            return Ok(());
        };
        if proto != PROTOCOL
            || instance_id.is_empty()
            || name.is_empty()
            || name.contains('@')
            || validate_snapshot(&agents).is_err()
        {
            self.write(
                &mut write,
                &Frame::Error {
                    msg: "unsupported protocol or invalid hello".into(),
                },
            )
            .await?;
            return Ok(());
        }
        if self
            .ctx
            .store
            .list()
            .await?
            .iter()
            .any(|config| config.alias == name)
        {
            let error = "Inbound controller name collides with a configured outbound alias";
            tracing::warn!("{error}; rejecting peer hello");
            self.ctx.engine.events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
                error: error.into(),
                context: "Peer admission rejected; choose a controller name distinct from outbound aliases".into(),
                fatal: false,
            });
            self.write(&mut write, &Frame::Error { msg: error.into() })
                .await?;
            return Ok(());
        }
        let (generation, mut cancelled) = {
            let mut slot = self.session.lock().await;
            if let Some(current) = &slot.controller
                && current.instance_id != instance_id
            {
                let controller = current.name.clone();
                drop(slot);
                self.write(&mut write, &Frame::Busy { controller }).await?;
                return Ok(());
            }
            self.ctx
                .store
                .write_inbound(&InboundStatus {
                    controller_name: name.clone(),
                    instance_id: instance_id.clone(),
                    connected_since: self.ctx.engine.clock.now_iso(),
                    agents,
                    controller_version: version,
                })
                .await?;
            self.listener_status(addr, Some(&name)).await?;
            // Seen now: the orphan sweep dates a controller's absence from
            // the last time it was admitted here.
            if let Err(error) = self.record_controller(&instance_id).await {
                self.session_fault("record controller", &error);
            }
            let claimed = slot
                .claim(&instance_id, &name)
                .expect("admission checked while holding session lock");
            // Mirrors belong to the controller that submitted them, and only
            // this session can be told that one started running.
            self.ctx
                .engine
                .orchestrator
                .set_inbound_controller(Some(instance_id.clone()));
            claimed
        };
        let result = async {
            let mut agents = self.ctx.local_agents().await?;
            self.write(
                &mut write,
                &Frame::Welcome {
                    proto: PROTOCOL,
                    instance_id: self.ctx.instance_id.clone(),
                    name: self.ctx.name.clone(),
                    os: std::env::consts::OS.into(),
                    agents: agents.clone(),
                    version: Some(crate::protocol::version()),
                },
            )
            .await?;
            self.write(
                &mut write,
                &Frame::Agents {
                    agents: agents.clone(),
                },
            )
            .await?;
            // The reader owns its partial line across timer/notification wakeups.
            let (incoming, mut frames) = mpsc::channel(32);
            let _reader = AbortOnDrop(tokio::spawn(async move {
                loop {
                    let frame = read_frame(&mut read).await;
                    let ended = !matches!(&frame, Ok(Some(_)));
                    if incoming.send(frame).await.is_err() || ended {
                        break;
                    }
                }
            }));
            let mut events = self.events.subscribe();
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_frame = Instant::now();
            let mut last_ping = Instant::now();
            let mut synced = false;
            let mut sent = HashMap::<String, String>::new();
            loop {
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => return Ok(()),
                    frame = frames.recv() => {
                        let Some(frame) = frame else {
                            return Ok(());
                        };
                        let Some(frame) = frame
                            .map_err(|e| Error::other(format!("peer frame: {e}")))?
                        else {
                            return Ok(());
                        };
                        last_frame = Instant::now();
                        if !synced && !matches!(&frame, Frame::Sync { .. }) {
                            self.write(
                                &mut write,
                                &Frame::Error {
                                    msg: "expected sync".into(),
                                },
                            )
                            .await?;
                            return Ok(());
                        }
                        let replies = {
                            let slot = self.session.lock().await;
                            if !slot.is_current(generation) {
                                return Ok(());
                            }
                            if matches!(&frame, Frame::Sync { .. }) {
                                synced = true;
                                sent.clear();
                            }
                            self.handle(&instance_id, &name, frame).await?
                        };
                        for frame in replies {
                            self.write(&mut write, &frame).await?;
                        }
                    }
                    event = events.recv(), if synced => {
                        match event {
                            Ok((controller, frame)) if controller == instance_id => {
                                self.write(&mut write, &frame).await?;
                            }
                            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return Ok(()),
                        }
                    }
                    _ = ticker.tick() => {
                        if last_frame.elapsed() >= DEAD_AFTER {
                            return Err(Error::other("peer heartbeat timed out"));
                        }
                        if last_ping.elapsed() >= PING_EVERY {
                            self.write(&mut write, &Frame::Ping).await?;
                            last_ping = Instant::now();
                        }
                        if !synced {
                            continue;
                        }
                        // Everything below reads this instance's own records.
                        // A fault there skips the tick, never the session.
                        match self.ctx.local_agents().await {
                            Ok(snapshot) if snapshot != agents => {
                                self.write(
                                    &mut write,
                                    &Frame::Agents {
                                        agents: snapshot.clone(),
                                    },
                                )
                                .await?;
                                agents = snapshot;
                            }
                            Ok(_) => {}
                            Err(error) => self.session_fault("read local agents", &error),
                        }
                        let updates = {
                            let slot = self.session.lock().await;
                            if !slot.is_current(generation) {
                                return Ok(());
                            }
                            self.updates(&instance_id, &mut sent).await
                        };
                        for frame in updates {
                            self.write(&mut write, &frame).await?;
                        }
                        match self.ctx.outbox(&name).await {
                            Ok(frames) => {
                                for frame in frames {
                                    self.write(&mut write, &frame).await?;
                                }
                            }
                            Err(error) => self.session_fault("read outbox", &error),
                        }
                        // A cancel that could not be applied when it arrived.
                        for frame in self.retry_cancels(&instance_id, &name).await {
                            self.write(&mut write, &frame).await?;
                        }
                    }
                }
            }
        }
        .await;
        if let Err(error) = &result {
            self.session_ended(&instance_id, &name, generation, error)
                .await;
        }
        let mut slot = self.session.lock().await;
        if slot.is_current(generation) {
            slot.controller = None;
            self.ctx.engine.orchestrator.set_inbound_controller(None);
            self.listener_status(addr, None).await?;
        }
        result
    }

    async fn write<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        write: &mut W,
        frame: &Frame,
    ) -> Result<()> {
        timeout(DEAD_AFTER, write_frame(write, frame))
            .await
            .map_err(|_| Error::other("peer write timed out"))?
            .map_err(|e| Error::other(format!("peer write: {e}")))
    }

    async fn mirrors(&self, controller: &str) -> Result<Vec<Task>> {
        let ctl = format!("remote-ctl:{controller}");
        let tasks = self.ctx.engine.tasks.list(&TaskFilter::default()).await?;
        let mirrors: Vec<_> = tasks
            .into_iter()
            .filter(|task| task.labels.contains(&ctl))
            .collect();
        for task in &mirrors {
            self.register(task);
        }
        Ok(mirrors)
    }

    /// The controller's view of one execution key, and the error barrier for
    /// it: a mirror this instance cannot read is reported as `unknown` with a
    /// reason instead of failing the session. The same fault is warned about
    /// once, not once per tick.
    async fn item(&self, key: &str, task: Option<&Task>) -> SyncItem {
        match self.read_item(key, task).await {
            Ok(item) => {
                self.faults().degraded.remove(key);
                item
            }
            Err(error) => {
                let reason = format!("worker storage error: {}", fault_kind(&error));
                let first = self
                    .faults()
                    .degraded
                    .insert(key.into(), reason.clone())
                    .as_deref()
                    != Some(reason.as_str());
                if first {
                    tracing::warn!(
                        exec_key = %key,
                        task_id = task.map(|task| task.id.as_str()).unwrap_or_default(),
                        %error,
                        "Mirror state unreadable; reporting the key as unknown"
                    );
                }
                SyncItem {
                    worker_task_id: task.map(|task| task.id.clone()),
                    ..SyncItem::new(key, "unknown").with_reason(reason)
                }
            }
        }
    }

    async fn read_item(&self, key: &str, task: Option<&Task>) -> Result<SyncItem> {
        let Some(task) = task else {
            return Ok(SyncItem::new(key, "unknown"));
        };
        let state = self.ctx.engine.state.read().await?;
        // A quarantined mirror is terminal even while its unverified saved
        // PID reserves the workspace. Report its non-retryable outcome now.
        let quarantined = task.status == TaskStatus::Cancelled
            && task.feedback.as_deref().is_some_and(|error| {
                error.starts_with("Worker execution collector lost; outcome unknown")
            });
        if !quarantined
            && (state.running.contains_key(&task.id) || state.claimed.contains(&task.id))
        {
            return Ok(SyncItem {
                worker_task_id: Some(task.id.clone()),
                ..SyncItem::new(key, "running")
            });
        }
        // The caller's snapshot may predate the final state write. Reading
        // the task after the reservation disappeared observes post-processing.
        let latest = self.ctx.engine.tasks.get(&task.id).await?;
        let Some(task) = latest.as_ref() else {
            return Ok(SyncItem::new(key, "unknown"));
        };
        let run = self
            .ctx
            .engine
            .runs
            .list_for_task(&task.id)
            .await?
            .into_iter()
            .max_by_key(|run| run.attempt);
        if run.is_none() && matches!(task.status, TaskStatus::Done | TaskStatus::Review) {
            return Ok(SyncItem::new(key, "unknown").with_reason("worker has no execution record"));
        }
        let outcome = derive_outcome(task, run.as_ref());
        let state = if outcome.is_some() {
            "finished"
        } else if matches!(
            task.status,
            TaskStatus::InProgress | TaskStatus::Review | TaskStatus::Done | TaskStatus::Failed
        ) {
            "running"
        } else {
            "queued"
        };
        let item = SyncItem {
            outcome,
            worker_task_id: Some(task.id.clone()),
            ..SyncItem::new(key, state)
        };
        // Security F4 holds this mirror until the worker's owner approves it.
        // Naming the command is the only way the controller's owner can tell
        // this apart from a worker that is merely busy.
        Ok(if task.awaits_approval() {
            item.with_reason(format!(
                "{AWAITING_APPROVAL_REASON} run `orch task approve {}` on this worker",
                task.id
            ))
        } else {
            item
        })
    }

    async fn handle(&self, controller: &str, name: &str, frame: Frame) -> Result<Vec<Frame>> {
        match frame {
            Frame::Sync { active } => {
                // The controller's first post-handshake frame. It must always
                // be answered: without a SyncState the controller cannot tell
                // a degraded worker from a lost one.
                let mirrors = match self.mirrors(controller).await {
                    Ok(mirrors) => mirrors,
                    Err(error) => {
                        self.session_fault("read mirrors for sync", &error);
                        let reason = format!("worker storage error: {}", fault_kind(&error));
                        return Ok(vec![Frame::SyncState {
                            items: active
                                .iter()
                                .map(|key| {
                                    SyncItem::new(key, "unknown").with_reason(reason.clone())
                                })
                                .collect(),
                        }]);
                    }
                };
                // Cleanup never gates the reply: an orphan this instance
                // cannot cancel is retried on the controller's next sync.
                //
                // A key the controller no longer lists is one it has already
                // accounted for — a finished or failed execution — so the
                // mirror is dead work. The *reason* goes on the task: this
                // sweep used to cancel silently, and a mirror found cancelled
                // with no explanation cannot be attributed afterwards.
                let mut orphans = 0;
                for task in &mirrors {
                    if let Some(key) = label(task, "exec:")
                        && !active.iter().any(|k| k == key)
                        && matches!(
                            task.status,
                            TaskStatus::Todo | TaskStatus::InProgress | TaskStatus::Retrying
                        )
                        && let Err(error) = self
                            .cancel_mirror(
                                task,
                                format!(
                                    "orphaned remote mirror: controller {controller} no longer tracks execution {key}"
                                ),
                            )
                            .await
                    {
                        orphans += 1;
                        tracing::warn!(task_id = %task.id, exec_key = %key, %error, "Orphan mirror not cancelled; retrying on the next sync");
                    }
                }
                if orphans > 0 {
                    self.report_fault(
                        "orphan cleanup",
                        &Error::other(format!("{orphans} orphan mirror(s) could not be cancelled")),
                        format!(
                            "Peer worker sync for controller {name}; the mirrors stay as they are \
                             and cleanup retries on the next sync"
                        ),
                    );
                }
                let mut items = Vec::with_capacity(active.len());
                for key in active {
                    // Re-read after cancellation and any concurrently completed run.
                    let mirror = mirrors
                        .iter()
                        .find(|t| label(t, "exec:") == Some(key.as_str()));
                    let task = match mirror {
                        // A read failure is handed on rather than swallowed:
                        // `item` meets the same fault and degrades this key
                        // alone to `unknown` with a reason.
                        Some(mirror) => self
                            .ctx
                            .engine
                            .tasks
                            .get(&mirror.id)
                            .await
                            .unwrap_or_else(|_| Some(mirror.clone())),
                        None => None,
                    };
                    items.push(self.item(&key, task.as_ref()).await);
                }
                let mut replies = vec![Frame::SyncState { items }];
                replies.extend(self.updates(controller, &mut HashMap::new()).await);
                Ok(replies)
            }
            Frame::Submit {
                exec_key,
                agent_id,
                from,
                prompt,
                title,
                priority,
                review_criteria,
                goal_id,
            } => {
                // Rejected rather than skipped: without the duplicate check
                // this request could create a second mirror for one key.
                let tasks = match self.ctx.engine.tasks.list(&TaskFilter::default()).await {
                    Ok(tasks) => tasks,
                    Err(error) => {
                        self.session_fault("read tasks for submit", &error);
                        return Ok(vec![Frame::Rejected {
                            exec_key,
                            reason: format!("worker storage error: {}", fault_kind(&error)),
                        }]);
                    }
                };
                if let Some(task) = tasks
                    .iter()
                    .find(|t| label(t, "exec:") == Some(exec_key.as_str()))
                {
                    if label(task, "remote-ctl:") != Some(controller) {
                        return Ok(vec![Frame::Rejected {
                            exec_key,
                            reason: "execution key belongs to a different controller".into(),
                        }]);
                    }
                    self.register(task);
                    return Ok(vec![Frame::Submitted {
                        exec_key,
                        worker_task_id: task.id.clone(),
                    }]);
                }
                let agent = if agent_id.contains('@') {
                    None
                } else {
                    match self.ctx.engine.agents.get(&agent_id).await {
                        Ok(agent) => agent,
                        Err(error) => {
                            self.session_fault("read agent for submit", &error);
                            return Ok(vec![Frame::Rejected {
                                exec_key,
                                reason: format!("worker storage error: {}", fault_kind(&error)),
                            }]);
                        }
                    }
                };
                if exec_key.is_empty()
                    || exec_key.len() > 256
                    || from.is_empty()
                    || from.contains('@')
                    || agent
                        .as_ref()
                        .is_none_or(|a| a.status == AgentStatus::Disabled)
                {
                    return Ok(vec![Frame::Rejected {
                        exec_key,
                        reason: "invalid submit or unknown/disabled local agent".into(),
                    }]);
                }
                let mut labels = vec![
                    format!("remote:{name}"),
                    format!("remote-ctl:{controller}"),
                    format!("exec:{exec_key}"),
                    format!("remote-from:{from}"),
                ];
                // Security F4: a peer must not make a `suggest`/`manual` agent
                // run unattended. The mirror waits, reported as queued, until
                // this instance's owner runs `orch task approve`.
                let held = agent.as_ref().and_then(|a| a.config.approval_policy)
                    != Some(ApprovalPolicy::Auto);
                if held {
                    labels.push(AWAITING_APPROVAL_LABEL.into());
                }
                let input = CreateTaskInput {
                    title,
                    description: Some(prompt),
                    priority: Some(priority),
                    assignee: Some(agent_id),
                    labels: Some(labels),
                    max_attempts: Some(1),
                    review_criteria: Some(review_criteria),
                    goal_id,
                    ..Default::default()
                };
                match self.ctx.engine.task_service.create(input).await {
                    Ok(task) => {
                        self.register(&task);
                        if held {
                            tracing::info!(task_id = %task.id, "Remote task awaits approval: orch task approve");
                        }
                        let worker_task_id = task.id;
                        Ok(vec![
                            Frame::Submitted {
                                exec_key: exec_key.clone(),
                                worker_task_id: worker_task_id.clone(),
                            },
                            Frame::State {
                                exec_key,
                                state: "queued".into(),
                                reason: held.then(|| {
                                    format!(
                                        "{AWAITING_APPROVAL_REASON} run `orch task approve {worker_task_id}` on this worker"
                                    )
                                }),
                                worker_task_id: Some(worker_task_id),
                            },
                        ])
                    }
                    // A class, never the error's own text: that can carry a
                    // local path or an error chain, and neither belongs on
                    // the wire (as for every other reason this worker sends).
                    Err(error) => Ok(vec![Frame::Rejected {
                        exec_key,
                        reason: format!(
                            "worker could not create the mirror: {}",
                            fault_kind(&error)
                        ),
                    }]),
                }
            }
            Frame::Cancel { exec_key } => {
                // Unacknowledged on a fault: an ack the controller cannot
                // trust would retire a cancellation key that never took. The
                // key is kept instead and retried every tick, because nothing
                // else clears a mirror inside a live session — the orphan
                // sweep only runs on a `Sync`, i.e. after a reconnect.
                if !valid_exec_key(&exec_key) {
                    // Peer input, and the key reaches a log line and the
                    // pending map: bound it exactly as `Submit` does.
                    tracing::warn!("Ignoring a cancel with an invalid execution key");
                    return Ok(vec![]);
                }
                let mirrors = match self.mirrors(controller).await {
                    Ok(mirrors) => mirrors,
                    Err(error) => {
                        self.session_fault("read mirrors for cancel", &error);
                        self.defer_cancel(&exec_key, &error);
                        return Ok(vec![]);
                    }
                };
                match self
                    .cancel_key(
                        &mirrors,
                        &exec_key,
                        format!("cancelled by controller {name}"),
                    )
                    .await
                {
                    Ok(()) => {
                        self.faults().unapplied_cancels.remove(&exec_key);
                        Ok(vec![Frame::CancelAck { exec_key }])
                    }
                    Err(error) => {
                        self.defer_cancel(&exec_key, &error);
                        Ok(vec![])
                    }
                }
            }
            Frame::FinishedAck { exec_key } => {
                // Not recording the ack costs one resend of the outcome on
                // the next sync, so every fault here simply retries.
                let mirrors = match self.mirrors(controller).await {
                    Ok(mirrors) => mirrors,
                    Err(error) => {
                        self.session_fault("read mirrors for finished ack", &error);
                        return Ok(vec![]);
                    }
                };
                if let Some(mut task) = mirrors
                    .into_iter()
                    .find(|t| label(t, "exec:") == Some(exec_key.as_str()))
                    && self.item(&exec_key, Some(&task)).await.outcome.is_some()
                    && !task.labels.iter().any(|l| l == "remote-finished-ack")
                {
                    task.labels.push("remote-finished-ack".into());
                    if let Err(error) = self.ctx.engine.tasks.save(&task).await {
                        self.session_fault("record finished ack", &error);
                        return Ok(vec![]);
                    }
                    let mut tap = self.tap.lock().expect("worker event ring poisoned");
                    tap.rings.remove(&exec_key);
                    tap.tasks.remove(&task.id);
                }
                Ok(vec![])
            }
            Frame::Attach {
                exec_key,
                after_seq,
            } => {
                // A Gap alone is a valid answer: the controller re-attaches
                // and the live event stream is unaffected.
                let mirrors = self.mirrors(controller).await.unwrap_or_else(|error| {
                    self.session_fault("read mirrors for attach", &error);
                    Vec::new()
                });
                let mut replay = Vec::new();
                if mirrors
                    .iter()
                    .any(|t| label(t, "exec:") == Some(exec_key.as_str()))
                {
                    let tap = self.tap.lock().expect("worker event ring poisoned");
                    if let Some(ring) = tap.rings.get(&exec_key) {
                        replay.extend(ring.after(after_seq).into_iter().map(|(seq, event)| {
                            Frame::Event {
                                exec_key: exec_key.clone(),
                                seq,
                                event,
                            }
                        }));
                    }
                }
                replay.push(Frame::Gap { exec_key });
                Ok(replay)
            }
            Frame::Agents { agents } => {
                // The snapshot itself is peer input, so an invalid one stays
                // fatal; failing to *store* a valid one only costs staleness.
                validate_snapshot(&agents)?;
                if let Err(error) = async {
                    if let Some(mut inbound) = self.ctx.store.read_inbound().await? {
                        inbound.agents = agents;
                        self.ctx.store.write_inbound(&inbound).await?;
                    }
                    Ok::<(), Error>(())
                }
                .await
                {
                    self.session_fault("record inbound agents", &error);
                }
                Ok(vec![])
            }
            frame @ Frame::Msg { .. } => {
                Ok(self.ctx.ingest(name, &frame).await?.into_iter().collect())
            }
            Frame::MsgAck { id, reason } => {
                self.ctx.msg_ack(name, &id, reason.as_deref()).await?;
                Ok(vec![])
            }
            Frame::Ping => Ok(vec![Frame::Pong]),
            Frame::Pong => Ok(vec![]),
            _ => Err(Error::other("unexpected controller frame")),
        }
    }

    /// Cancel one mirror and record **why** on the task.
    ///
    /// Every worker-side cancel goes through here. A mirror that simply turns
    /// `cancelled` with no reason is unexplainable afterwards: the live
    /// incident was a mirror cancelled at startup with no feedback and no log
    /// line, which took an evening to attribute.
    async fn cancel_mirror(&self, task: &Task, reason: String) -> Result<()> {
        tracing::info!(
            task_id = %task.id,
            exec_key = label(task, "exec:").unwrap_or_default(),
            status = %task.status,
            %reason,
            "Cancelling remote mirror"
        );
        // Feedback first: `cancel_task` re-reads the record, so writing it
        // here survives the status change, and a failure below still leaves
        // the explanation behind.
        let mut fresh = self
            .ctx
            .engine
            .tasks
            .get(&task.id)
            .await?
            .unwrap_or_else(|| task.clone());
        fresh.feedback = Some(reason);
        self.ctx.engine.tasks.save(&fresh).await?;
        self.ctx.engine.orchestrator.cancel_task(&task.id).await?;
        // `cancel_task` reports success even when its final status write
        // failed, because a refused transition usually means the task was
        // already terminal. Here it is the difference between a mirror that
        // stopped and one still waiting for a local agent, so confirm it
        // from the record instead of trusting the call.
        match self.ctx.engine.tasks.get(&task.id).await? {
            Some(fresh) if !terminal(fresh.status) => Err(Error::other(format!(
                "mirror {} is still {}",
                fresh.id, fresh.status
            ))),
            _ => Ok(()),
        }
    }

    /// Cancel the mirror of one execution key, from a mirror list this tick
    /// has already read. Idempotent: a mirror that is gone or already
    /// terminal is cancelled as far as the controller is concerned, so it is
    /// acknowledged rather than retried.
    async fn cancel_key(&self, mirrors: &[Task], exec_key: &str, reason: String) -> Result<()> {
        let Some(task) = mirrors
            .iter()
            .find(|task| label(task, "exec:") == Some(exec_key))
            .filter(|task| !terminal(task.status))
        else {
            return Ok(());
        };
        self.cancel_mirror(task, reason).await
    }

    /// Note a cancel this instance could not apply, warning once per key.
    fn defer_cancel(&self, exec_key: &str, error: &Error) {
        let now = Instant::now();
        let mut faults = self.faults();
        match faults.unapplied_cancels.get_mut(exec_key) {
            Some(pending) => {
                pending.attempts = pending.attempts.saturating_add(1);
                pending.next_at = now + cancel_retry_delay(pending.attempts);
                // A key that cannot be applied for an hour is said again:
                // silence after the first warning hides a stuck mirror.
                if pending.warned_at.elapsed() >= CANCEL_REWARN_EVERY {
                    pending.warned_at = now;
                    tracing::warn!(%exec_key, %error, attempts = pending.attempts, "Cancel still not applicable");
                }
                return;
            }
            None => {
                if faults.unapplied_cancels.len() >= MAX_UNAPPLIED_CANCELS {
                    // Bounded: peer input decides how many keys exist. The
                    // one whose retry is furthest out makes way.
                    if let Some(evicted) = faults
                        .unapplied_cancels
                        .iter()
                        .max_by_key(|(_, pending)| pending.next_at)
                        .map(|(key, _)| key.clone())
                    {
                        tracing::warn!(exec_key = %evicted, "Dropping a deferred cancel: too many pending");
                        faults.unapplied_cancels.remove(&evicted);
                    }
                }
                faults.unapplied_cancels.insert(
                    exec_key.into(),
                    PendingCancel {
                        next_at: now + cancel_retry_delay(1),
                        attempts: 1,
                        warned_at: now,
                    },
                );
            }
        }
        drop(faults);
        self.session_fault("cancel mirror", error);
    }

    /// Retry the cancels this instance could not apply, once per tick, each
    /// on its own backoff. A mirror is typically unreadable for as long as
    /// its file is damaged; once it is readable again the next due attempt
    /// retires the key.
    async fn retry_cancels(&self, controller: &str, name: &str) -> Vec<Frame> {
        let now = Instant::now();
        let due: Vec<String> = self
            .faults()
            .unapplied_cancels
            .iter()
            .filter(|(_, pending)| pending.next_at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        if due.is_empty() {
            return Vec::new();
        }
        // One listing for the whole tick, not one per key.
        let mirrors = match self.mirrors(controller).await {
            Ok(mirrors) => mirrors,
            Err(error) => {
                self.session_fault("read mirrors for deferred cancel", &error);
                return Vec::new();
            }
        };
        let mut replies = Vec::new();
        for exec_key in due {
            match self
                .cancel_key(
                    &mirrors,
                    &exec_key,
                    format!("cancelled by controller {name}"),
                )
                .await
            {
                Ok(()) => {
                    self.faults().unapplied_cancels.remove(&exec_key);
                    tracing::info!(%exec_key, "Deferred cancel applied");
                    replies.push(Frame::CancelAck { exec_key });
                }
                Err(error) => self.defer_cancel(&exec_key, &error),
            }
        }
        replies
    }

    async fn updates(&self, controller: &str, sent: &mut HashMap<String, String>) -> Vec<Frame> {
        let mut replies = Vec::new();
        // Nothing to send this tick rather than an ended session: the next
        // tick, one second later, tries again.
        let mirrors = match self.mirrors(controller).await {
            Ok(mirrors) => mirrors,
            Err(error) => {
                self.session_fault("read mirrors for updates", &error);
                return replies;
            }
        };
        for task in mirrors {
            if task.labels.iter().any(|l| l == "remote-finished-ack") {
                continue;
            }
            let Some(key) = label(&task, "exec:") else {
                continue;
            };
            let item = self.item(key, Some(&task)).await;
            if sent.get(key) == Some(&item.state) {
                continue;
            }
            sent.insert(key.into(), item.state.clone());
            if item.state == "unknown" {
                replies.push(Frame::SyncState { items: vec![item] });
            } else if let Some(outcome) = item.outcome {
                replies.push(Frame::Finished {
                    exec_key: key.into(),
                    outcome,
                });
            } else {
                replies.push(Frame::State {
                    exec_key: key.into(),
                    state: item.state,
                    reason: item.reason,
                    worker_task_id: item.worker_task_id,
                });
            }
        }
        replies
    }
}

/// A coarse class for a storage fault, for the `reason` the controller shows
/// its owner. Never the error's own text: that can carry a local path or an
/// error chain, and neither belongs on the wire (see `Context::ingest`).
fn fault_kind(error: &Error) -> &'static str {
    match error {
        Error::Io { .. } => "unreadable file",
        Error::Serde { .. } => "corrupt record",
        Error::Command { .. } => "command failed",
        Error::Adapter(_) => "adapter failed",
        // Everything else, including the YAML size and alias guards, which
        // report through `Error::other`.
        Error::Domain(_) => "read failed",
    }
}

impl RunEventTap for Worker {
    fn record(&self, task_id: &str, _run_id: &str, event: &AgentEvent) {
        let mut tap = self.tap.lock().expect("worker event ring poisoned");
        let Some((key, controller)) = tap.tasks.get(task_id).cloned() else {
            return;
        };
        let event = crate::codec::bounded_event(&key, event.clone());
        let seq = tap
            .rings
            .entry(key.clone())
            .or_default()
            .push(event.clone());
        let _ = self.events.send((
            controller,
            Frame::Event {
                exec_key: key,
                seq,
                event: event.clone(),
            },
        ));
    }
}

/// A mirror in this state is finished as far as a controller's cancel goes.
fn terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

fn label<'a>(task: &'a Task, prefix: &str) -> Option<&'a str> {
    task.labels
        .iter()
        .find_map(|label| label.strip_prefix(prefix))
}

fn derive_outcome(task: &Task, run: Option<&Run>) -> Option<RemoteOutcome> {
    if task.status == TaskStatus::Cancelled {
        return Some(RemoteOutcome::Cancelled {
            reason: run
                .and_then(|r| r.error.clone())
                .or_else(|| task.feedback.clone())
                .unwrap_or_else(|| "Worker task cancelled".into()),
        });
    }
    if !matches!(
        task.status,
        TaskStatus::Done | TaskStatus::Review | TaskStatus::Failed
    ) {
        return None;
    }
    if task.status == TaskStatus::Failed && run.is_none() {
        return Some(RemoteOutcome::Failed {
            error: "Worker task failed before an execution record was created".into(),
            error_kind: None,
        });
    }
    let run = run?;
    match run.status {
        RunStatus::Failed | RunStatus::TimedOut => Some(RemoteOutcome::Failed {
            error: run
                .error
                .clone()
                .unwrap_or_else(|| "Worker execution failed".into()),
            error_kind: run.error_kind,
        }),
        RunStatus::Cancelled => Some(RemoteOutcome::Cancelled {
            reason: run
                .error
                .clone()
                .unwrap_or_else(|| "Worker run cancelled".into()),
        }),
        RunStatus::Succeeded => {
            let proof = task.proof.clone().unwrap_or_default();
            let summary = proof.agent_summary.unwrap_or_default();
            if summary.starts_with("MERGE CONFLICT:") || summary.starts_with("MERGE ERROR:") {
                return Some(RemoteOutcome::IntegrationFailed {
                    info: summary.clone(),
                    summary,
                });
            }
            if let Some(results) = &task.review_results
                && results.iter().any(|result| !result.passed)
            {
                return Some(RemoteOutcome::ReviewFailed {
                    summary,
                    review_results: results.clone(),
                });
            }
            // A review record can be persisted before criteria start. Never
            // turn that transient state into a successful outcome.
            if task.review_criteria.as_ref().is_some_and(|criteria| {
                criteria.iter().any(|criterion| {
                    !task.review_results.as_ref().is_some_and(|results| {
                        results.iter().any(|result| result.criterion == *criterion)
                    })
                })
            }) {
                return None;
            }
            Some(RemoteOutcome::Succeeded {
                summary,
                files_changed: proof.files_changed,
                branch: proof.branch,
                review_results: task.review_results.clone(),
                tokens: run.tokens,
            })
        }
        RunStatus::Preparing | RunStatus::Running => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::adapter::AgentEventKind;
    use orch_domain::{CreateAgentInput, ReviewCriterion, ReviewResult, TaskProof, TokenUsage};
    use orch_engine::{EngineSeams, build_full_with};
    use orch_infra::storage::remote::FileRemoteStore;

    async fn fixture() -> (tempfile::TempDir, Arc<Worker>, String) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let engine = Arc::new(
            build_full_with(
                temp.path(),
                EngineSeams {
                    global_config_path: Some(temp.path().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        // The tests drive `handle` directly, so nothing claims a session slot.
        // A mirror only runs while its controller is connected, which here is
        // the `"controller"` every test submits as.
        engine
            .orchestrator
            .set_inbound_controller(Some("controller".into()));
        let agent = engine
            .agent_service
            .create(CreateAgentInput {
                name: "worker".into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let worker = Worker::new(Context {
            store: FileRemoteStore::new(engine.paths.clone()),
            engine,
            instance_id: "worker-instance".into(),
            name: "W1".into(),
        });
        (temp, worker, agent.id)
    }

    fn submit(key: &str, agent: &str) -> Frame {
        Frame::Submit {
            exec_key: key.into(),
            agent_id: agent.into(),
            from: "lead".into(),
            prompt: "  exact\nprompt\n".into(),
            title: "delegated".into(),
            priority: 2,
            review_criteria: vec![],
            goal_id: None,
        }
    }

    async fn mirror(worker: &Worker, key: &str) -> Task {
        worker
            .ctx
            .engine
            .tasks
            .list(&TaskFilter::default())
            .await
            .unwrap()
            .into_iter()
            .find(|task| label(task, "exec:") == Some(key))
            .unwrap()
    }

    async fn terminal(worker: &Worker, key: &str, status: TaskStatus) -> (Task, Run) {
        let mut task = mirror(worker, key).await;
        task.status = status;
        task.proof = Some(TaskProof {
            agent_summary: Some("result".into()),
            files_changed: vec!["worker.txt".into()],
            ..Default::default()
        });
        worker.ctx.engine.tasks.save(&task).await.unwrap();
        let run = Run {
            id: format!("run_{key}"),
            task_id: task.id.clone(),
            agent_id: task.assignee.clone().unwrap(),
            attempt: 1,
            status: RunStatus::Succeeded,
            started_at: "now".into(),
            finished_at: Some("later".into()),
            workspace_path: "/worker".into(),
            prompt: "prompt".into(),
            pid: None,
            error: None,
            error_kind: None,
            tokens: Some(TokenUsage {
                input: 5,
                output: 2,
                total: 7,
                ..Default::default()
            }),
        };
        worker.ctx.engine.runs.save(&run).await.unwrap();
        (task, run)
    }

    #[test]
    fn session_busy_and_same_instance_takeover_preserve_new_generation() {
        let mut slot = SessionSlot::default();
        let (first, mut cancellation) = slot.claim("instance-1", "C1").unwrap();
        assert_eq!(slot.claim("instance-2", "C2").unwrap_err(), "C1");
        assert!(slot.is_current(first));
        let (second, _) = slot.claim("instance-1", "C1-restarted").unwrap();
        assert!(*cancellation.borrow_and_update());
        assert!(!slot.is_current(first));
        assert!(slot.is_current(second));
    }

    #[test]
    fn ring_keeps_last_thousand_full_events_and_replays_strictly_after_sequence() {
        let mut ring = EventRing::default();
        let payload = "x".repeat(9000);
        for n in 0..1002 {
            let mut event = AgentEvent::new(
                AgentEventKind::Output,
                n.to_string(),
                serde_json::json!(payload),
            );
            event.tokens = Some(TokenUsage {
                total: 7,
                ..Default::default()
            });
            assert_eq!(ring.push(event), n + 1);
        }
        assert_eq!(ring.events.len(), 1000);
        assert_eq!(ring.events.front().unwrap().0, 3);
        let replay = ring.after(1000);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].0, 1001);
        assert_eq!(replay[0].1.data.as_str(), Some(payload.as_str()));
        assert_eq!(replay[0].1.tokens.unwrap().total, 7);
        assert!(ring.after(1002).is_empty());
    }

    #[tokio::test]
    async fn inbound_alias_collision_gets_error_and_cannot_receive_queued_messages() {
        let (_temp, worker, _agent) = fixture().await;
        worker
            .ctx
            .store
            .add(orch_domain::RemoteConfig {
                alias: "w1".into(),
                addr: "127.0.0.1:1".into(),
                added_at: "now".into(),
            })
            .await
            .unwrap();
        let message = orch_domain::Message {
            id: "msg_collision".into(),
            channel: orch_domain::MessageChannel::Direct,
            from_agent_id: "local".into(),
            to_agent_id: Some("agt_remote@w1".into()),
            subject: "private".into(),
            body: "must not leak".into(),
            reply_to: None,
            created_at: "now".into(),
            expires_at: None,
            status: orch_domain::MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
        };
        worker.ctx.engine.messages.save(&message).await.unwrap();
        let mut events = worker.ctx.engine.events.subscribe();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_worker = worker.clone();
        let mut server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            server_worker.connection(socket, "test").await.unwrap();
        });
        let mut socket = TcpStream::connect(addr).await.unwrap();
        write_frame(
            &mut socket,
            &Frame::Hello {
                proto: PROTOCOL,
                instance_id: "impostor".into(),
                name: "w1".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
        let mut reader = BufReader::new(socket);
        let first = timeout(Duration::from_secs(5), read_frame(&mut reader))
            .await
            .unwrap()
            .unwrap();
        if !matches!(&first, Some(Frame::Error { msg }) if msg.contains("outbound alias")) {
            server.abort();
            let _ = server.await;
            panic!("expected collision error, got {first:?}");
        }
        let completed = timeout(Duration::from_secs(5), &mut server).await;
        if completed.is_err() {
            server.abort();
            let _ = server.await;
        }
        assert!(matches!(completed, Ok(Ok(()))));
        assert!(
            timeout(Duration::from_secs(5), read_frame(&mut reader))
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(worker.session.lock().await.controller.is_none());
        assert!(worker.ctx.store.read_inbound().await.unwrap().is_none());
        assert_eq!(
            worker
                .ctx
                .engine
                .messages
                .get(&message.id)
                .await
                .unwrap()
                .unwrap()
                .status,
            orch_domain::MessageStatus::Pending
        );
        assert!(
            matches!(timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap().as_ref(), orch_domain::OrchestratorEvent::OrchestratorError { error, fatal: false, .. } if error.contains("outbound alias"))
        );
    }

    #[tokio::test]
    async fn protocol_mismatch_gets_error_then_eof() {
        let (_temp, worker, _agent) = fixture().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_worker = worker.clone();
        let server = tokio::spawn(async move {
            server_worker.listen(listener).await.unwrap();
        });
        let mut socket = TcpStream::connect(addr).await.unwrap();
        write_frame(
            &mut socket,
            &Frame::Hello {
                proto: 99,
                instance_id: "controller".into(),
                name: "C".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
        let mut reader = BufReader::new(socket);
        assert!(matches!(
            timeout(Duration::from_secs(2), read_frame(&mut reader))
                .await
                .unwrap()
                .unwrap(),
            Some(Frame::Error { .. })
        ));
        assert!(
            timeout(Duration::from_secs(2), read_frame(&mut reader))
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(worker.session.lock().await.controller.is_none());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn live_session_rejects_other_identity_but_same_identity_takes_over() {
        let (_temp, worker, _agent) = fixture().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_worker = worker.clone();
        let server = tokio::spawn(async move {
            server_worker.listen(listener).await.unwrap();
        });
        async fn hello(addr: std::net::SocketAddr, id: &str, name: &str) -> BufReader<TcpStream> {
            let mut socket = TcpStream::connect(addr).await.unwrap();
            write_frame(
                &mut socket,
                &Frame::Hello {
                    proto: PROTOCOL,
                    instance_id: id.into(),
                    name: name.into(),
                    os: "test".into(),
                    agents: vec![],
                    version: None,
                },
            )
            .await
            .unwrap();
            BufReader::new(socket)
        }
        async fn next(reader: &mut BufReader<TcpStream>) -> Option<Frame> {
            timeout(Duration::from_secs(2), read_frame(reader))
                .await
                .unwrap()
                .unwrap()
        }
        let mut first = hello(addr, "controller", "C").await;
        assert!(matches!(
            next(&mut first).await,
            Some(Frame::Welcome { .. })
        ));
        assert!(matches!(next(&mut first).await, Some(Frame::Agents { .. })));
        let mut other = hello(addr, "other", "C2").await;
        assert!(
            matches!(next(&mut other).await, Some(Frame::Busy { controller }) if controller == "C")
        );
        assert!(next(&mut other).await.is_none());
        let mut replacement = hello(addr, "controller", "C-restarted").await;
        assert!(matches!(
            next(&mut replacement).await,
            Some(Frame::Welcome { .. })
        ));
        assert!(matches!(
            next(&mut replacement).await,
            Some(Frame::Agents { .. })
        ));
        assert!(next(&mut first).await.is_none());
        write_frame(replacement.get_mut(), &Frame::Sync { active: vec![] })
            .await
            .unwrap();
        assert!(matches!(
            next(&mut replacement).await,
            Some(Frame::SyncState { .. })
        ));
        let status: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(worker.ctx.engine.paths.remotes_dir().join("_listener.json"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(status["controller_name"], "C-restarted");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let listener = worker.ctx.engine.paths.remotes_dir().join("_listener.json");
            let mode = std::fs::metadata(&listener).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "_listener.json must be owner-only (F7)");
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn submit_is_idempotent_and_preserves_mirror_contract() {
        let (_temp, worker, agent) = fixture().await;
        for _ in 0..2 {
            worker
                .handle("controller", "C", submit("key", &agent))
                .await
                .unwrap();
        }
        let tasks = worker.mirrors("controller").await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].description, "  exact\nprompt\n");
        assert_eq!(tasks[0].assignee.as_deref(), Some(agent.as_str()));
        assert_eq!(tasks[0].max_attempts, 1);
        for expected in [
            "remote:C",
            "remote-ctl:controller",
            "exec:key",
            "remote-from:lead",
        ] {
            assert!(tasks[0].labels.iter().any(|label| label == expected));
        }
        let replies = worker
            .handle("other-controller", "other", submit("key", &agent))
            .await
            .unwrap();
        assert!(matches!(&replies[0], Frame::Rejected { .. }));
        assert_eq!(
            worker
                .ctx
                .engine
                .tasks
                .list(&TaskFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// Configures the fixture agent with `policy` and a bounded shell run, then
    /// submits `key` from a remote controller.
    async fn submit_with_policy(
        temp: &tempfile::TempDir,
        worker: &Worker,
        agent_id: &str,
        policy: orch_domain::ApprovalPolicy,
    ) -> Vec<Frame> {
        let mut agent = worker
            .ctx
            .engine
            .agents
            .get(agent_id)
            .await
            .unwrap()
            .unwrap();
        agent.config.approval_policy = Some(policy);
        agent.config.workspace_mode = Some(orch_domain::WorkspaceMode::Shared);
        agent.config.timeout_ms = Some(5_000);
        agent.config.env =
            Some(std::iter::once(("HOME".into(), temp.path().to_str().unwrap().into())).collect());
        worker.ctx.engine.agents.save(&agent).await.unwrap();
        worker
            .handle("controller", "C", submit("key", agent_id))
            .await
            .unwrap()
    }

    /// Dispatch every ready task once (`orch run --all`) and the mirror by id
    /// (`orch run <id>`), then wait for whatever started to be collected.
    async fn dispatch_mirror(worker: &Worker, task_id: &str) {
        timeout(Duration::from_secs(30), async {
            worker.ctx.engine.orchestrator.run_all().await?;
            worker.ctx.engine.orchestrator.run_task(task_id).await?;
            worker.ctx.engine.orchestrator.wait_for_collectors().await;
            Result::<()>::Ok(())
        })
        .await
        .expect("bounded dispatch")
        .unwrap();
    }

    async fn assert_succeeded_and_reported(worker: &Worker) {
        let completed = mirror(worker, "key").await;
        assert_eq!(completed.status, TaskStatus::Done);
        let runs = worker
            .ctx
            .engine
            .runs
            .list_for_task(&completed.id)
            .await
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Succeeded);
        // The controller receives the outcome on its next update.
        let frames = worker.updates("controller", &mut HashMap::new()).await;
        assert!(
            frames.iter().any(|frame| matches!(
                frame,
                Frame::Finished {
                    exec_key,
                    outcome: RemoteOutcome::Succeeded { .. },
                } if exec_key == "key"
            )),
            "{frames:?}"
        );
    }

    #[tokio::test]
    async fn mirror_for_auto_policy_agent_runs_without_owner_approval() {
        let (temp, worker, agent_id) = fixture().await;
        let replies =
            submit_with_policy(&temp, &worker, &agent_id, orch_domain::ApprovalPolicy::Auto).await;
        assert!(matches!(&replies[0], Frame::Submitted { .. }));
        let task = mirror(&worker, "key").await;
        assert!(!task.awaits_approval());
        assert!(task.review_criteria.as_ref().unwrap().is_empty());

        dispatch_mirror(&worker, &task.id).await;
        worker.ctx.engine.orchestrator.stop().await.unwrap();
        assert_succeeded_and_reported(&worker).await;
    }

    /// Security F4: a peer cannot make a `suggest`/`manual` agent run
    /// unattended. The mirror is held (reported to the controller as queued,
    /// never running) until the worker owner approves it; it then runs once
    /// and its outcome reaches the controller.
    async fn mirror_waits_for_owner_approval(policy: orch_domain::ApprovalPolicy) {
        let (temp, worker, agent_id) = fixture().await;
        let replies = submit_with_policy(&temp, &worker, &agent_id, policy).await;
        let Frame::Submitted { worker_task_id, .. } = &replies[0] else {
            panic!("{replies:?}")
        };
        let task = mirror(&worker, "key").await;
        assert_eq!(worker_task_id, &task.id);
        assert!(task.awaits_approval(), "{:?}", task.labels);
        // The controller can only name the command its owner has to run here
        // if this frame carries both the reason and the mirror's id.
        let Frame::State {
            state,
            reason,
            worker_task_id,
            ..
        } = &replies[1]
        else {
            panic!("{replies:?}")
        };
        assert_eq!(state, "queued");
        assert_eq!(worker_task_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(
            reason.as_deref(),
            Some(
                format!(
                    "{AWAITING_APPROVAL_REASON} run `orch task approve {}` on this worker",
                    task.id
                )
                .as_str()
            )
        );
        // And so does every later sync of the same key.
        let item = worker.item("key", Some(&task)).await;
        assert_eq!(
            (
                item.state.as_str(),
                item.reason.as_deref(),
                item.worker_task_id.as_deref()
            ),
            ("queued", reason.as_deref(), Some(task.id.as_str()))
        );

        dispatch_mirror(&worker, &task.id).await;
        let held = mirror(&worker, "key").await;
        assert_eq!(held.status, TaskStatus::Todo);
        assert!(
            worker
                .ctx
                .engine
                .runs
                .list_for_task(&held.id)
                .await
                .unwrap()
                .is_empty(),
            "held mirror must not execute"
        );
        let item = worker.item("key", Some(&held)).await;
        assert_eq!((item.state.as_str(), item.outcome), ("queued", None));

        // `orch task approve <id>` on the worker.
        worker
            .ctx
            .engine
            .task_service
            .approve(&task.id)
            .await
            .unwrap();
        dispatch_mirror(&worker, &task.id).await;
        worker.ctx.engine.orchestrator.stop().await.unwrap();
        assert_succeeded_and_reported(&worker).await;
    }

    #[tokio::test]
    async fn mirror_for_suggest_policy_agent_waits_for_owner_approval() {
        mirror_waits_for_owner_approval(orch_domain::ApprovalPolicy::Suggest).await;
    }

    #[tokio::test]
    async fn mirror_for_manual_policy_agent_waits_for_owner_approval() {
        mirror_waits_for_owner_approval(orch_domain::ApprovalPolicy::Manual).await;
    }

    #[tokio::test]
    async fn submit_rejects_overlay_unknown_disabled_and_invalid_priority() {
        let (_temp, worker, agent) = fixture().await;
        for invalid in ["missing", "worker@nested"] {
            assert!(matches!(
                &worker
                    .handle("controller", "C", submit("key", invalid))
                    .await
                    .unwrap()[0],
                Frame::Rejected { .. }
            ));
        }
        let mut request = submit("priority", &agent);
        if let Frame::Submit { priority, .. } = &mut request {
            *priority = 5;
        }
        assert!(matches!(
            &worker.handle("controller", "C", request).await.unwrap()[0],
            Frame::Rejected { .. }
        ));
        let mut disabled = worker.ctx.engine.agents.get(&agent).await.unwrap().unwrap();
        disabled.status = AgentStatus::Disabled;
        worker.ctx.engine.agents.save(&disabled).await.unwrap();
        assert!(matches!(
            &worker
                .handle("controller", "C", submit("disabled", &agent))
                .await
                .unwrap()[0],
            Frame::Rejected { .. }
        ));
        assert!(
            worker
                .ctx
                .engine
                .tasks
                .list(&TaskFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn sync_cancels_only_untracked_work_for_that_controller_and_reports_unknown() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("tracked", &agent))
            .await
            .unwrap();
        worker
            .handle("controller", "C", submit("abandoned", &agent))
            .await
            .unwrap();
        worker
            .handle("other", "C2", submit("foreign", &agent))
            .await
            .unwrap();
        let frames = worker
            .handle(
                "controller",
                "C",
                Frame::Sync {
                    active: vec!["tracked".into(), "missing".into()],
                },
            )
            .await
            .unwrap();
        let Frame::SyncState { items } = &frames[0] else {
            panic!("sync state expected")
        };
        assert_eq!(items[0].state, "queued");
        assert_eq!(items[1].state, "unknown");
        assert_eq!(mirror(&worker, "tracked").await.status, TaskStatus::Todo);
        let abandoned = mirror(&worker, "abandoned").await;
        assert_eq!(abandoned.status, TaskStatus::Cancelled);
        // The live incident: a mirror found `cancelled` with no feedback and
        // no log line could not be attributed to any path afterwards.
        assert_eq!(
            abandoned.feedback.as_deref(),
            Some(
                "orphaned remote mirror: controller controller no longer tracks execution abandoned"
            )
        );
        assert_eq!(mirror(&worker, "foreign").await.status, TaskStatus::Todo);
        assert!(matches!(
            &worker
                .handle(
                    "controller",
                    "C",
                    Frame::Cancel {
                        exec_key: "missing".into()
                    }
                )
                .await
                .unwrap()[0],
            Frame::CancelAck { .. }
        ));
        // An explicit cancel says so on the mirror as well.
        assert!(matches!(
            &worker
                .handle(
                    "controller",
                    "C",
                    Frame::Cancel {
                        exec_key: "tracked".into()
                    }
                )
                .await
                .unwrap()[0],
            Frame::CancelAck { .. }
        ));
        let cancelled = mirror(&worker, "tracked").await;
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert_eq!(
            cancelled.feedback.as_deref(),
            Some("cancelled by controller C")
        );
        // A key no `Submit` would have accepted is not even recorded.
        assert!(
            worker
                .handle(
                    "controller",
                    "C",
                    Frame::Cancel {
                        exec_key: "x".repeat(257)
                    }
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert!(worker.faults().unapplied_cancels.is_empty());
    }

    #[tokio::test]
    async fn persisted_finished_is_recovered_resent_on_sync_and_stops_after_ack() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("key", &agent))
            .await
            .unwrap();
        terminal(&worker, "key", TaskStatus::Done).await;
        // Fresh transport state, same durable worker records.
        let recovered = Worker::new(worker.ctx.clone());
        for _ in 0..2 {
            let frames = recovered
                .handle(
                    "controller",
                    "C",
                    Frame::Sync {
                        active: vec!["key".into()],
                    },
                )
                .await
                .unwrap();
            assert!(frames.iter().any(|frame| matches!(
                frame,
                Frame::Finished {
                    outcome: RemoteOutcome::Succeeded {
                        tokens: Some(TokenUsage { total: 7, .. }),
                        ..
                    },
                    ..
                }
            )));
        }
        recovered.record(
            &mirror(&recovered, "key").await.id,
            "run",
            &AgentEvent::new(AgentEventKind::Output, "now", serde_json::json!("event")),
        );
        assert_eq!(recovered.tap.lock().unwrap().rings.len(), 1);
        recovered
            .handle(
                "controller",
                "C",
                Frame::FinishedAck {
                    exec_key: "key".into(),
                },
            )
            .await
            .unwrap();
        assert!(recovered.tap.lock().unwrap().rings.is_empty());
        let frames = recovered
            .handle("controller", "C", Frame::Sync { active: vec![] })
            .await
            .unwrap();
        assert!(
            !frames
                .iter()
                .any(|frame| matches!(frame, Frame::Finished { .. }))
        );
    }

    #[tokio::test]
    async fn attach_replays_full_events_then_gap_and_never_leaks_other_controller() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("key", &agent))
            .await
            .unwrap();
        let task = mirror(&worker, "key").await;
        for n in 0..3 {
            worker.record(
                &task.id,
                "run",
                &AgentEvent::new(AgentEventKind::Output, "now", serde_json::json!(n)),
            );
        }
        let frames = worker
            .handle(
                "controller",
                "C",
                Frame::Attach {
                    exec_key: "key".into(),
                    after_seq: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(frames.len(), 3);
        assert!(matches!(&frames[0], Frame::Event { seq: 2, .. }));
        assert!(matches!(&frames[1], Frame::Event { seq: 3, .. }));
        assert!(matches!(&frames[2], Frame::Gap { .. }));
        let foreign = worker
            .handle(
                "other",
                "C2",
                Frame::Attach {
                    exec_key: "key".into(),
                    after_seq: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(foreign.len(), 1);
        assert!(matches!(&foreign[0], Frame::Gap { .. }));
    }

    #[tokio::test]
    async fn outcome_requires_post_processing_and_preserves_review_and_merge_failures() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("key", &agent))
            .await
            .unwrap();
        let (mut task, mut run) = terminal(&worker, "key", TaskStatus::Review).await;
        task.review_criteria = Some(vec![ReviewCriterion::TestPass]);
        assert!(derive_outcome(&task, Some(&run)).is_none());
        task.review_results = Some(vec![]);
        assert!(derive_outcome(&task, Some(&run)).is_none());
        task.review_results = Some(vec![ReviewResult {
            criterion: ReviewCriterion::Lint,
            passed: true,
            output: "passed unrelated check".into(),
        }]);
        assert!(derive_outcome(&task, Some(&run)).is_none());
        task.review_results = Some(vec![ReviewResult {
            criterion: ReviewCriterion::TestPass,
            passed: false,
            output: "failure".into(),
        }]);
        assert!(matches!(
            derive_outcome(&task, Some(&run)),
            Some(RemoteOutcome::ReviewFailed { .. })
        ));
        task.proof.as_mut().unwrap().agent_summary = Some("MERGE CONFLICT: worker branch".into());
        assert!(matches!(
            derive_outcome(&task, Some(&run)),
            Some(RemoteOutcome::IntegrationFailed { .. })
        ));
        task.proof.as_mut().unwrap().agent_summary = Some("MERGE ERROR: cannot merge".into());
        assert!(matches!(
            derive_outcome(&task, Some(&run)),
            Some(RemoteOutcome::IntegrationFailed { .. })
        ));
        run.status = RunStatus::Failed;
        run.error = Some("worker failed".into());
        run.error_kind = Some(orch_domain::AdapterErrorKind::RateLimit);
        assert!(matches!(
            derive_outcome(&task, Some(&run)),
            Some(RemoteOutcome::Failed {
                error_kind: Some(orch_domain::AdapterErrorKind::RateLimit),
                ..
            })
        ));
        task.status = TaskStatus::Cancelled;
        assert!(matches!(
            derive_outcome(&task, None),
            Some(RemoteOutcome::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn failed_preparation_has_an_outcome_but_missing_success_record_is_unknown() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("key", &agent))
            .await
            .unwrap();
        let mut task = mirror(&worker, "key").await;
        task.status = TaskStatus::Failed;
        worker.ctx.engine.tasks.save(&task).await.unwrap();
        assert!(matches!(
            worker.item("key", Some(&task)).await.outcome,
            Some(RemoteOutcome::Failed { .. })
        ));
        task.status = TaskStatus::Review;
        worker.ctx.engine.tasks.save(&task).await.unwrap();
        let item = worker.item("key", Some(&task)).await;
        assert_eq!(item.state, "unknown");
        assert!(item.outcome.is_none());
        let updates = worker.updates("controller", &mut HashMap::new()).await;
        assert!(matches!(&updates[0], Frame::SyncState { items } if items[0].state == "unknown"));
    }

    #[tokio::test]
    async fn a_persisted_running_reservation_prevents_early_finished_envelope() {
        let (_temp, worker, agent) = fixture().await;
        worker
            .handle("controller", "C", submit("key", &agent))
            .await
            .unwrap();
        let (task, _) = terminal(&worker, "key", TaskStatus::Review).await;
        let mut state = worker.ctx.engine.state.read().await.unwrap();
        state.claimed.insert(task.id.clone());
        worker.ctx.engine.state.write(&state).await.unwrap();
        let item = worker.item("key", Some(&task)).await;
        assert_eq!(item.state, "running");
        assert!(item.outcome.is_none());
        state.claimed.clear();
        worker.ctx.engine.state.write(&state).await.unwrap();
        assert!(worker.item("key", Some(&task)).await.outcome.is_some());
    }

    include!("worker_hardening_tests.rs");
    include!("worker_env_tests.rs");
}
