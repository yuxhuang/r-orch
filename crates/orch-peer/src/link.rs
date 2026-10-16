//! Persistent outbound links and pid-less execution streams.
use crate::protocol::{MAX_REASON_BYTES, MAX_TASK_ID_BYTES, MAX_VERSION_BYTES};
use crate::{
    AbortOnDrop, Context,
    codec::{read_frame, write_frame},
    protocol::{Frame, PROTOCOL},
};
use async_trait::async_trait;
use futures::stream;
use orch_core::{
    error::{Error, Result},
    remote::{
        PendingRemoteCancellation, RemoteBridge, RemoteExecuteHandle, RemoteOutcome,
        RemotePhaseUpdate, RemoteRunEvent, RemoteSubmit,
    },
    storage::MessageStore,
};
use orch_domain::{LinkState, Message, MessageStatus, RemoteConfig, RemotePhase, RemoteStatus};
use orch_infra::storage::remote::{metadata_text, validate_snapshot};

/// A peer's display name and OS, as they appear in `orch remote status`.
const MAX_NAME_BYTES: usize = 64;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::TcpStream,
    sync::{Notify, mpsc},
    time::Instant,
};

struct Execution {
    /// `None` for a rehydrated execution: there is nothing left to send, the
    /// worker already has the request.
    submit: Option<Frame>,
    events: mpsc::Sender<RemoteRunEvent>,
    attempted: bool,
    seq: u64,
    terminal: Option<RemoteRunEvent>,
    delivered: bool,
    hint: Option<String>,
    /// The last phase handed to the collector, so a worker that repeats a
    /// state every tick does not repeat a state write on this side.
    reported: Option<RemotePhaseUpdate>,
}
#[derive(Default)]
struct Queue {
    executions: BTreeMap<String, Execution>,
    commands: VecDeque<Frame>,
    cancellations: BTreeSet<String>,
    online: bool,
    wake: Arc<Notify>,
}

pub(crate) struct LinkHub {
    queues: Mutex<BTreeMap<String, Queue>>,
    messages: Arc<dyn MessageStore>,
    changed: Notify,
}
impl LinkHub {
    pub fn new(messages: Arc<dyn MessageStore>) -> Self {
        Self {
            queues: Mutex::new(BTreeMap::new()),
            messages,
            changed: Notify::new(),
        }
    }
    fn queues(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Queue>> {
        self.queues.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn connected(&self, alias: &str) -> Connected<'_> {
        let mut queues = self.queues();
        let q = queues.entry(alias.into()).or_default();
        q.online = true;
        // Commands may have been consumed by a failed write. Keep cancellation
        // keys independently until CancelAck, including explicit task cancels.
        q.commands
            .retain(|frame| !matches!(frame, Frame::Cancel { .. }));
        for key in &q.cancellations {
            q.commands.push_back(Frame::Cancel {
                exec_key: key.clone(),
            });
        }
        Connected {
            hub: self,
            alias: alias.into(),
            wake: q.wake.clone(),
        }
    }
    fn cancel_ack(&self, alias: &str, key: &str) {
        if let Some(q) = self.queues().get_mut(alias) {
            q.cancellations.remove(key);
        }
        self.changed.notify_one();
    }
    fn pending(&self) -> Vec<PendingRemoteCancellation> {
        self.queues()
            .iter()
            .flat_map(|(alias, q)| {
                q.cancellations.iter().map(|key| PendingRemoteCancellation {
                    alias: alias.clone(),
                    exec_key: key.clone(),
                })
            })
            .collect()
    }
    fn active(&self, alias: &str) -> Vec<String> {
        self.queues()
            .get(alias)
            .map(|q| q.executions.keys().cloned().collect())
            .unwrap_or_default()
    }
    fn attachments(&self, alias: &str) -> Vec<Frame> {
        self.queues()
            .get(alias)
            .map(|q| {
                q.executions
                    .iter()
                    .map(|(key, e)| Frame::Attach {
                        exec_key: key.clone(),
                        after_seq: e.seq,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    fn commands(&self, alias: &str) -> Vec<Frame> {
        self.queues()
            .get_mut(alias)
            .map(|q| q.commands.drain(..).collect())
            .unwrap_or_default()
    }
    fn submits(&self, alias: &str) -> Vec<Frame> {
        let mut guard = self.queues();
        let Some(q) = guard.get_mut(alias) else {
            return vec![];
        };
        q.executions
            .values_mut()
            .filter_map(|e| {
                if e.attempted || e.terminal.is_some() {
                    return None;
                }
                e.submit.clone()
            })
            .collect()
    }
    fn mark_attempted(&self, alias: &str, frame: &Frame) {
        if let Frame::Submit { exec_key, .. } = frame
            && let Some(e) = self
                .queues()
                .get_mut(alias)
                .and_then(|q| q.executions.get_mut(exec_key))
        {
            // Set only for the frame about to be written. A previous command
            // failing must not turn later, wholly unsent requests into Unknown.
            e.attempted = true;
        }
    }

    fn terminal(&self, alias: &str, key: &str, event: RemoteRunEvent) -> bool {
        let mut guard = self.queues();
        let Some(q) = guard.get_mut(alias) else {
            return false;
        };
        let Some(e) = q.executions.get_mut(key) else {
            return false;
        };
        if e.terminal.is_none() {
            e.terminal = Some(event);
        }
        Self::flush_execution(e);
        if e.delivered && matches!(e.terminal, Some(RemoteRunEvent::Unknown)) {
            q.executions.remove(key);
        }
        true
    }
    fn flush_execution(e: &mut Execution) {
        if !e.delivered
            && let Some(event) = &e.terminal
        {
            match e.events.try_send(event.clone()) {
                Ok(()) => e.delivered = true,
                Err(mpsc::error::TrySendError::Closed(_)) => e.delivered = true,
                Err(mpsc::error::TrySendError::Full(_)) => {}
            }
        }
    }
    fn flush(&self, alias: &str) {
        if let Some(q) = self.queues().get_mut(alias) {
            q.executions.retain(|_, e| {
                Self::flush_execution(e);
                !(e.delivered && matches!(e.terminal, Some(RemoteRunEvent::Unknown)))
            });
        }
    }
    fn event(&self, alias: &str, key: &str, seq: u64, event: orch_core::adapter::AgentEvent) {
        if let Some(e) = self
            .queues()
            .get_mut(alias)
            .and_then(|q| q.executions.get_mut(key))
            && seq > e.seq
        {
            e.seq = seq;
            // Progress is best effort. Preserve bounded memory even if a
            // collector stalls; terminal envelopes use the separate retry slot.
            let _ = e.events.try_send(RemoteRunEvent::Event(event));
        }
    }
    fn hint(&self, alias: &str, key: &str, state: &str, timestamp: String) {
        if let Some(e) = self
            .queues()
            .get_mut(alias)
            .and_then(|q| q.executions.get_mut(key))
            && e.hint.as_deref() != Some(state)
        {
            let text = match state {
                "queued" => format!("waiting: queued on {alias}"),
                "running" => format!("running on {alias}"),
                _ => return,
            };
            e.hint = Some(state.into());
            let event = orch_core::adapter::AgentEvent::new(
                orch_core::adapter::AgentEventKind::Output,
                timestamp,
                serde_json::json!({"text": text, "remote_state": state}),
            );
            let _ = e.events.try_send(RemoteRunEvent::Hint(event));
        }
    }

    /// Hand the worker's view of one execution to its collector. Deduplicated,
    /// because a worker repeats a state on every tick of a long run.
    fn phase(&self, alias: &str, key: &str, update: RemotePhaseUpdate) {
        if let Some(e) = self
            .queues()
            .get_mut(alias)
            .and_then(|q| q.executions.get_mut(key))
        {
            Self::report_phase(e, update);
        }
    }

    fn report_phase(e: &mut Execution, mut update: RemotePhaseUpdate) {
        // A worker names the mirror's task id once; later frames about the
        // same execution must not unlearn it.
        if update.worker_task_id.is_none() {
            update.worker_task_id = e
                .reported
                .as_ref()
                .and_then(|last| last.worker_task_id.clone());
        }
        if e.reported.as_ref() == Some(&update) {
            return;
        }
        e.reported = Some(update.clone());
        let _ = e.events.try_send(RemoteRunEvent::Phase(update));
    }

    /// `Submitted`: the worker owns the mirror from here on, whatever happens
    /// to this link. The id it names is peer input, so it is bounded here.
    fn phase_submitted(&self, alias: &str, key: &str, worker_task_id: String) {
        self.phase(
            alias,
            key,
            RemotePhaseUpdate {
                phase: RemotePhase::Queued,
                worker_task_id: crate::protocol::text(Some(worker_task_id), MAX_TASK_ID_BYTES),
                reason: None,
            },
        );
    }

    fn phase_of(state: &str) -> Option<RemotePhase> {
        match state {
            "queued" => Some(RemotePhase::Queued),
            "running" => Some(RemotePhase::Running),
            _ => None,
        }
    }

    fn attempted(&self, alias: &str, key: &str) -> bool {
        self.queues()
            .get(alias)
            .and_then(|q| q.executions.get(key))
            .is_some_and(|e| e.attempted)
    }
    fn remove_alias(&self, alias: &str) {
        self.queues().remove(alias);
    }
}

struct Connected<'a> {
    hub: &'a LinkHub,
    alias: String,
    wake: Arc<Notify>,
}
impl Drop for Connected<'_> {
    fn drop(&mut self) {
        if let Some(q) = self.hub.queues().get_mut(&self.alias) {
            q.online = false;
            // A submit that reached the socket may have been accepted, so the
            // controller no longer knows what the worker is doing: that is
            // `unknown` until a sync resolves it. A submit still queued here
            // never left, so it stays `dispatched` and is written next time.
            for e in q.executions.values_mut() {
                if e.attempted && e.terminal.is_none() {
                    LinkHub::report_phase(e, RemotePhaseUpdate::new(RemotePhase::Unknown));
                }
            }
        }
        self.hub.changed.notify_one();
    }
}

#[async_trait]
impl RemoteBridge for LinkHub {
    fn submit(&self, request: RemoteSubmit) -> Result<RemoteExecuteHandle> {
        let (tx, rx) = mpsc::channel(1024);
        let mut queues = self.queues();
        let queue = queues.entry(request.alias).or_default();
        if queue.executions.contains_key(&request.exec_key) {
            return Err(Error::other("Execution stream already registered"));
        }
        let key = request.exec_key.clone();
        let submit = Frame::Submit {
            exec_key: request.exec_key,
            agent_id: request.agent_id,
            from: request.from,
            prompt: request.prompt,
            title: request.title,
            priority: request.priority,
            review_criteria: request.review_criteria,
            goal_id: request.goal_id,
        };
        if !crate::codec::fits(&submit) {
            // Return a terminal stream rather than Err: Err leaves a durable
            // controller reservation pending for sync, although nothing ran.
            return Ok(RemoteExecuteHandle {
                pid: 0,
                events: Box::pin(stream::once(async {
                    RemoteRunEvent::Finished(RemoteOutcome::Failed {
                        error: "prompt too large for remote frame (1 MiB maximum)".into(),
                        error_kind: None,
                    })
                })),
            });
        }
        queue.executions.insert(
            key,
            Execution {
                submit: Some(submit),
                events: tx,
                attempted: false,
                seq: 0,
                terminal: None,
                delivered: false,
                hint: None,
                reported: None,
            },
        );
        Ok(RemoteExecuteHandle {
            pid: 0,
            events: Box::pin(stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|e| (e, rx))
            })),
        })
    }
    fn rehydrate(&self, alias: &str, exec_key: &str) -> Result<RemoteExecuteHandle> {
        let (tx, rx) = mpsc::channel(1024);
        let mut queues = self.queues();
        let queue = queues.entry(alias.into()).or_default();
        if queue.executions.contains_key(exec_key) {
            return Err(Error::other("Execution stream already registered"));
        }
        queue.executions.insert(
            exec_key.into(),
            Execution {
                // Nothing to send: the worker has the request already, and a
                // second one would be a second effect.
                submit: None,
                events: tx,
                attempted: true,
                seq: 0,
                terminal: None,
                delivered: false,
                hint: None,
                reported: None,
            },
        );
        Ok(RemoteExecuteHandle {
            pid: 0,
            events: Box::pin(stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|e| (e, rx))
            })),
        })
    }
    async fn cancel(&self, alias: &str, exec_key: &str) -> Result<()> {
        let mut queues = self.queues();
        let q = queues.entry(alias.into()).or_default();
        q.executions.remove(exec_key);
        if q.cancellations.insert(exec_key.into()) {
            q.commands.push_back(Frame::Cancel {
                exec_key: exec_key.into(),
            });
        }
        q.wake.notify_one();
        Ok(())
    }
    async fn drain_cancellations(
        &self,
        deadline: Duration,
    ) -> Result<Vec<PendingRemoteCancellation>> {
        let until = Instant::now() + deadline;
        {
            let queues = self.queues();
            for q in queues
                .values()
                .filter(|q| q.online && !q.cancellations.is_empty())
            {
                // Wake the writer now; do not race the periodic tick at exit.
                q.wake.notify_one();
            }
        }
        let wait = async {
            loop {
                let changed = self.changed.notified();
                if !self
                    .queues()
                    .values()
                    .any(|q| q.online && !q.cancellations.is_empty())
                {
                    break;
                }
                changed.await;
            }
        };
        let _ = tokio::time::timeout_at(until, wait).await;
        Ok(self.pending())
    }
    async fn ack_finished(&self, alias: &str, exec_key: &str) -> Result<()> {
        let mut queues = self.queues();
        let q = queues.entry(alias.into()).or_default();
        q.executions.remove(exec_key);
        q.commands.push_back(Frame::FinishedAck {
            exec_key: exec_key.into(),
        });
        Ok(())
    }
    async fn send_msg(&self, peer: &str, message: &Message) -> Result<()> {
        if !message
            .to_agent_id
            .as_deref()
            .is_some_and(|to| to.rsplit_once('@').is_some_and(|(_, p)| p == peer))
        {
            return Err(Error::invalid_arguments(
                "Message recipient does not match peer",
            ));
        }
        // The file outbox is the durable queue, shared with ordinary CLI sends.
        if self.messages.get(&message.id).await?.is_none() {
            self.messages.save(message).await?;
        }
        let wait = async {
            loop {
                if self
                    .messages
                    .get(&message.id)
                    .await?
                    .is_some_and(|m| m.status == MessageStatus::Delivered)
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(15), wait)
            .await
            .map_err(|_| Error::other("Remote message pending acknowledgement"))?
    }
}

/// Longest wait between redials. Short enough that a worker coming back is
/// noticed promptly, long enough that a worker which kills every session is
/// dialled about four times a minute rather than once a second.
const BACKOFF_CAP: Duration = Duration::from_secs(15);
/// The shortest wait between respawns of one alias's link task. A link that
/// panics during startup every time would otherwise respawn on every 250 ms
/// tick, with an `orchestrator:error` each time.
const RESPAWN_FLOOR: Duration = Duration::from_secs(5);
/// One disconnect report per error per window, as on the worker side.
const DISCONNECT_REPORT_EVERY: Duration = Duration::from_secs(30);
/// How long a session must stay up, *after* clearing the sync fence, before it
/// counts as evidence the peer is healthy. A session that dies right after the
/// handshake is the symptom of a worker fault; resetting the backoff on it
/// turns the reconnect loop into a redial storm.
const HEALTHY_SESSION: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Backoff(u32);
impl Backoff {
    fn next(&mut self) -> Duration {
        let seconds = (1u64 << self.0.min(4)).min(BACKOFF_CAP.as_secs());
        self.0 = self.0.saturating_add(1);
        jittered(Duration::from_secs(seconds))
    }
}

/// `base` ±25 %, so links that dropped together do not redial in lockstep.
/// The entropy is the process's hasher seed rather than a `rand` dependency:
/// this needs spread, not statistical quality.
fn jittered(base: Duration) -> Duration {
    use std::hash::{BuildHasher, RandomState};
    let offset = RandomState::new().hash_one(base.as_nanos()) % (base.as_nanos() as u64 / 2 + 1);
    base + Duration::from_nanos(offset) - base / 4
}

/// What a finished session proves about the peer, for the redial backoff.
#[derive(Default)]
struct SessionHealth {
    /// When the worker answered the post-sync Ping, i.e. when the session
    /// became usable rather than merely handshaken.
    fenced_at: Option<Instant>,
    /// The worker refused this controller because it is serving another one.
    /// Worth saying, because nothing is wrong with either side.
    busy: bool,
}

impl SessionHealth {
    /// Only a session that became usable and then stayed up resets the
    /// backoff. Everything else keeps it escalating.
    fn healthy(&self) -> bool {
        self.fenced_at
            .is_some_and(|at| at.elapsed() >= HEALTHY_SESSION)
    }
}

struct Links(BTreeMap<String, (RemoteConfig, AbortOnDrop)>);

pub(crate) async fn manage(ctx: Context, hub: Arc<LinkHub>) {
    let mut links = Links(BTreeMap::new());
    // Per alias: when its link task may be respawned, and how long the next
    // wait is. A link that panics on every attempt must not spin at 4 Hz.
    let mut respawn: BTreeMap<String, (Instant, Backoff)> = BTreeMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        let configs = match ctx.store.list().await {
            Ok(configs) => configs,
            Err(error) => {
                tracing::error!(%error,"Read peer registry");
                continue;
            }
        };
        let removed: Vec<_> = links
            .0
            .iter()
            .filter(|(_, (old, _))| !configs.iter().any(|c| c == old))
            .map(|(alias, _)| alias.clone())
            .collect();
        for alias in removed {
            links.0.remove(&alias);
            if !configs.iter().any(|config| config.alias == alias) {
                hub.remove_alias(&alias);
            }
        }
        for config in configs {
            // `run_link` never returns, so a finished handle means it panicked.
            // Without a respawn that alias stays offline for the life of the
            // process, with nothing in the registry to say why.
            let stopped = links
                .0
                .get(&config.alias)
                .is_some_and(|(_, task)| task.0.is_finished());
            if stopped {
                let due = respawn
                    .entry(config.alias.clone())
                    .or_insert_with(|| (Instant::now(), Backoff::default()));
                if Instant::now() < due.0 {
                    // Still inside this alias's respawn floor. The finished
                    // handle stays, so the next tick reconsiders it.
                    continue;
                }
                due.0 = Instant::now() + due.1.next().max(RESPAWN_FLOOR);
                let error = format!("Peer link task for {} stopped unexpectedly", config.alias);
                tracing::error!(alias = %config.alias, "{error}; restarting");
                ctx.engine
                    .events
                    .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                        error,
                        context: format!(
                            "Peer {}; restarting the link. Dispatch to it was unavailable meanwhile",
                            config.alias
                        ),
                        fatal: false,
                    });
            }
            if stopped || !links.0.contains_key(&config.alias) {
                let alias = config.alias.clone();
                let task = tokio::spawn(run_link(ctx.clone(), hub.clone(), config.clone()));
                links.0.insert(alias, (config, AbortOnDrop(task)));
            }
        }
    }
}

async fn blocked() -> bool {
    if let Some(path) = std::env::var_os("ORCH_TEST_PEER_BLOCK_FILE") {
        tokio::fs::try_exists(path).await.unwrap_or(false)
    } else {
        false
    }
}

async fn run_link(mut ctx: Context, hub: Arc<LinkHub>, config: RemoteConfig) {
    // No other runtime task may keep this link's health lease alive after it
    // is aborted, panics, or is replaced by registry reconciliation.
    ctx.store = orch_infra::storage::remote::FileRemoteStore::new(ctx.engine.paths.clone());
    let mut status = ctx
        .store
        .read_status(&config.alias)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| RemoteStatus {
            alias: config.alias.clone(),
            addr: config.addr.clone(),
            online: false,
            last_seen: None,
            last_error: None,
            peer_name: None,
            instance_id: None,
            os: None,
            agents: vec![],
            peer_version: None,
            link_state: LinkState::Connecting,
            next_retry_at: None,
            attempts: 0,
        });
    status.addr = config.addr.clone();
    status.online = false;
    let mut backoff = Backoff::default();
    let mut reported: Option<(String, Instant)> = None;
    loop {
        while blocked().await {
            status.online = false;
            status.last_error = Some("Peer link blocked by test hook".into());
            persist_status(&ctx, &status).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // Published before the dial, not after it: a link that is failing is
        // the one an operator asks about, and until the dial returns this is
        // all there is to tell them.
        status.link_state = LinkState::Connecting;
        status.next_retry_at = None;
        status.attempts = status.attempts.saturating_add(1);
        persist_status(&ctx, &status).await;
        let mut health = SessionHealth::default();
        let result = session(&ctx, &hub, &config, &mut status, &mut health).await;
        status.online = false;
        ctx.store.mark_link_offline(&config.alias);
        status.last_error = Some(
            result
                .err()
                .map_or_else(|| "Peer disconnected".into(), |e| e.to_string()),
        );
        // Debounced like the worker's own faults: a peer that drops every
        // session would otherwise fill the feed with the same line four
        // times a minute. A *different* error is always worth reporting.
        let error = status.last_error.clone().unwrap_or_default();
        let repeat = reported
            .as_ref()
            .is_some_and(|(last, at): &(String, Instant)| {
                *last == error && at.elapsed() < DISCONNECT_REPORT_EVERY
            });
        if !repeat {
            reported = Some((error.clone(), Instant::now()));
            ctx.engine
                .events
                .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                    error,
                    context: format!(
                        "Peer {} disconnected; dispatch unavailable, reconnecting",
                        config.alias
                    ),
                    fatal: false,
                });
        }
        if health.healthy() {
            backoff = Backoff::default();
            status.attempts = 0;
        }
        let delay = backoff.next();
        status.link_state = if health.busy {
            LinkState::Busy
        } else {
            LinkState::Backoff
        };
        status.next_retry_at = Some(orch_core::clock::iso_from_epoch_ms(
            ctx.engine.clock.now_ms() + delay.as_millis() as i64,
        ));
        persist_status(&ctx, &status).await;
        tracing::debug!(alias=%config.alias,error=?status.last_error,?delay,healthy=health.healthy(),attempts=status.attempts,"Peer reconnect pending");
        tokio::time::sleep(delay).await;
    }
}

async fn persist_status(ctx: &Context, status: &RemoteStatus) {
    // A removal must not recreate its status file after the command deleted it.
    if ctx
        .store
        .list()
        .await
        .is_ok_and(|configs| configs.iter().any(|c| c.alias == status.alias))
        && let Err(error) = ctx.store.write_status(status).await
    {
        ctx.engine.events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
            error: error.to_string(),
            context: format!("Write peer status for {}; remote dispatch unavailable. Check permissions and free space in {}", status.alias, ctx.engine.paths.remotes_dir().display()),
            fatal: false,
        });
    }
}

async fn session(
    ctx: &Context,
    hub: &Arc<LinkHub>,
    config: &RemoteConfig,
    status: &mut RemoteStatus,
    health: &mut SessionHealth,
) -> Result<()> {
    let socket = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&config.addr))
        .await
        .map_err(|_| Error::other("Peer connect timed out"))?
        .map_err(net_error)?;
    socket.set_nodelay(true).map_err(net_error)?;
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);
    let mut local = ctx.local_agents().await?;
    write_frame(
        &mut writer,
        &Frame::Hello {
            proto: PROTOCOL,
            instance_id: ctx.instance_id.clone(),
            name: ctx.name.clone(),
            os: std::env::consts::OS.into(),
            agents: local.clone(),
            version: Some(crate::protocol::version()),
        },
    )
    .await
    .map_err(net_error)?;
    let welcome = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
        .await
        .map_err(|_| Error::other("Peer handshake timed out"))?
        .map_err(net_error)?;
    match welcome {
        Some(Frame::Welcome {
            proto,
            instance_id,
            name,
            os,
            agents,
            version,
        }) => {
            if proto != PROTOCOL {
                return Err(Error::other("Peer protocol mismatch"));
            }
            if instance_id == ctx.instance_id {
                return Err(Error::other("Cannot connect a peer to itself"));
            }
            if !crate::protocol::valid_instance_id(&instance_id)
                || name.is_empty()
                || name.contains('@')
            {
                return Err(Error::other("Invalid peer identity"));
            }
            validate_snapshot(&agents)?;
            status.instance_id = Some(instance_id);
            // Peer-supplied, and bound for a `remote status` line.
            status.peer_name = Some(metadata_text(&name, MAX_NAME_BYTES));
            status.os = Some(metadata_text(&os, MAX_NAME_BYTES));
            status.agents = agents;
            status.peer_version = crate::protocol::text(version, MAX_VERSION_BYTES);
        }
        Some(Frame::Busy { controller }) => {
            health.busy = true;
            return Err(Error::other(format!(
                "Worker busy: controlled by {}",
                peer_text(&controller)
            )));
        }
        Some(Frame::Error { msg }) => return Err(Error::other(peer_text(&msg))),
        _ => return Err(Error::other("Expected peer welcome")),
    }
    // Must be the first post-handshake frame. It also cancels orphan mirrors
    // after a controller restart, whose local recovery ran before this link.
    write_frame(
        &mut writer,
        &Frame::Sync {
            active: hub.active(&config.alias),
        },
    )
    .await
    .map_err(net_error)?;
    // A v1 worker answers Ping only after writing every Sync reply. Wait
    // for that fence before sending new submits: sync_state may be chunked,
    // and a later unknown item must not race a just-attempted submission.
    write_frame(&mut writer, &Frame::Ping)
        .await
        .map_err(net_error)?;
    for frame in hub.attachments(&config.alias) {
        write_frame(&mut writer, &frame).await.map_err(net_error)?;
    }
    status.online = true;
    status.link_state = LinkState::Online;
    status.next_retry_at = None;
    status.last_error = None;
    status.last_seen = Some(ctx.engine.clock.now_iso());
    persist_status(ctx, status).await;
    let (tx, mut frames) = mpsc::channel(128);
    let _reader = AbortOnDrop(tokio::spawn(async move {
        loop {
            let result = read_frame(&mut reader).await;
            let stop = !matches!(result, Ok(Some(_)));
            if tx.send(result).await.is_err() || stop {
                break;
            }
        }
    }));
    let connected = hub.connected(&config.alias);
    let mut synced = false;
    let mut sync_fenced = false;
    let mut last_frame = Instant::now();
    let mut last_ping = Instant::now();
    let mut last_poll = Instant::now() - Duration::from_secs(1);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let frame = frame
                    .ok_or_else(|| Error::other("Peer reader stopped"))?
                    .map_err(net_error)?
                    .ok_or_else(|| Error::other("Peer closed connection"))?;
                last_frame = Instant::now();
                if matches!(&frame, Frame::Pong) && !sync_fenced {
                    sync_fenced = true;
                    health.fenced_at = Some(Instant::now());
                }
                status.last_seen = Some(ctx.engine.clock.now_iso());
                handle_frame(ctx, hub, config, status, frame, &mut writer, &mut synced).await?;
            }
            _ = async {
                tokio::select! {
                    _ = tick.tick() => {},
                    _ = connected.wake.notified() => {},
                }
            } => {
                if blocked().await {
                    return Err(Error::other("Peer link blocked by test hook"));
                }
                if last_frame.elapsed() >= Duration::from_secs(15) {
                    return Err(Error::other("Peer heartbeat timed out"));
                }
                if last_ping.elapsed() >= Duration::from_secs(5) {
                    write_frame(&mut writer, &Frame::Ping).await.map_err(net_error)?;
                    last_ping = Instant::now();
                }
                hub.flush(&config.alias);
                if synced && sync_fenced {
                    let frames = hub.commands(&config.alias).into_iter()
                        .chain(hub.submits(&config.alias));
                    for frame in frames {
                        hub.mark_attempted(&config.alias, &frame);
                        write_frame(&mut writer, &frame).await.map_err(net_error)?;
                    }
                }
                if last_poll.elapsed() >= Duration::from_secs(1) {
                    let agents = ctx.local_agents().await?;
                    if agents != local {
                        local = agents.clone();
                        write_frame(&mut writer, &Frame::Agents { agents }).await.map_err(net_error)?;
                    }
                    for frame in ctx.outbox(&config.alias).await? {
                        write_frame(&mut writer, &frame).await.map_err(net_error)?;
                    }
                    persist_status(ctx, status).await;
                    last_poll = Instant::now();
                }
            }
        }
    }
}
async fn handle_frame(
    ctx: &Context,
    hub: &LinkHub,
    config: &RemoteConfig,
    status: &mut RemoteStatus,
    frame: Frame,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    synced: &mut bool,
) -> Result<()> {
    match frame {
        Frame::Ping => write_frame(writer, &Frame::Pong).await.map_err(net_error)?,
        Frame::Pong => {}
        Frame::Agents { agents } => {
            validate_snapshot(&agents)?;
            status.agents = agents;
            persist_status(ctx, status).await;
        }
        Frame::SyncState { mut items } => {
            *synced = true;
            for item in &mut items {
                bound_item(item);
            }
            if let Some(q) = hub.queues().get(&config.alias) {
                q.wake.notify_one();
            }
            for item in items {
                match item.state.as_str() {
                    "finished" => {
                        let outcome = item
                            .outcome
                            .ok_or_else(|| Error::other("Finished sync item has no outcome"))?;
                        if !hub.terminal(
                            &config.alias,
                            &item.exec_key,
                            RemoteRunEvent::Finished(outcome),
                        ) {
                            write_frame(
                                writer,
                                &Frame::FinishedAck {
                                    exec_key: item.exec_key,
                                },
                            )
                            .await
                            .map_err(net_error)?;
                        }
                    }
                    "unknown" if hub.attempted(&config.alias, &item.exec_key) => {
                        log_reason(config, &item.exec_key, &item.state, item.reason.as_deref());
                        // The phase carries the worker's explanation ahead of
                        // the terminal event, which has nowhere to put it: an
                        // unreadable mirror must reach the operator as more
                        // than "remote lost execution record".
                        hub.phase(
                            &config.alias,
                            &item.exec_key,
                            RemotePhaseUpdate {
                                phase: RemotePhase::Unknown,
                                worker_task_id: item.worker_task_id,
                                reason: item.reason,
                            },
                        );
                        hub.terminal(&config.alias, &item.exec_key, RemoteRunEvent::Unknown);
                    }
                    "queued" | "running" => {
                        log_reason(config, &item.exec_key, &item.state, item.reason.as_deref());
                        hub.hint(
                            &config.alias,
                            &item.exec_key,
                            &item.state,
                            ctx.engine.clock.now_iso(),
                        );
                        if let Some(phase) = LinkHub::phase_of(&item.state) {
                            hub.phase(
                                &config.alias,
                                &item.exec_key,
                                RemotePhaseUpdate {
                                    phase,
                                    worker_task_id: item.worker_task_id,
                                    reason: item.reason,
                                },
                            );
                        }
                    }
                    "unknown" => {
                        log_reason(config, &item.exec_key, &item.state, item.reason.as_deref())
                    }
                    _ => return Err(Error::other("Invalid peer sync state")),
                }
            }
        }
        Frame::Finished { exec_key, outcome } => {
            if !hub.terminal(&config.alias, &exec_key, RemoteRunEvent::Finished(outcome)) {
                tracing::debug!(alias=%config.alias,%exec_key,"Acknowledge untracked remote outcome");
                write_frame(writer, &Frame::FinishedAck { exec_key })
                    .await
                    .map_err(net_error)?;
            }
        }
        Frame::Rejected { exec_key, reason } => {
            hub.terminal(
                &config.alias,
                &exec_key,
                RemoteRunEvent::Finished(RemoteOutcome::Failed {
                    error: format!("Remote rejected submit: {}", peer_text(&reason)),
                    error_kind: None,
                }),
            );
        }
        Frame::Event {
            exec_key,
            seq,
            event,
        } => hub.event(&config.alias, &exec_key, seq, event),
        Frame::Msg { .. } => {
            if let Some(ack) = ctx.ingest(&config.alias, &frame).await? {
                write_frame(writer, &ack).await.map_err(net_error)?;
            }
        }
        Frame::MsgAck { id, reason } => ctx.msg_ack(&config.alias, &id, reason.as_deref()).await?,
        Frame::State {
            exec_key,
            state,
            reason,
            worker_task_id,
        } => {
            let reason = crate::protocol::text(reason, MAX_REASON_BYTES);
            let worker_task_id = crate::protocol::text(worker_task_id, MAX_TASK_ID_BYTES);
            log_reason(config, &exec_key, &state, reason.as_deref());
            hub.hint(&config.alias, &exec_key, &state, ctx.engine.clock.now_iso());
            if let Some(phase) = LinkHub::phase_of(&state) {
                hub.phase(
                    &config.alias,
                    &exec_key,
                    RemotePhaseUpdate {
                        phase,
                        worker_task_id,
                        reason,
                    },
                );
            }
        }
        Frame::CancelAck { exec_key } => hub.cancel_ack(&config.alias, &exec_key),
        // The worker owns the mirror from here on: the request is no longer
        // merely dispatched, whatever happens to this link.
        Frame::Submitted {
            exec_key,
            worker_task_id,
        } => hub.phase_submitted(&config.alias, &exec_key, worker_task_id),
        Frame::Gap { .. } => {}
        Frame::Error { msg } => return Err(Error::other(peer_text(&msg))),
        _ => return Err(Error::other("Unexpected worker frame")),
    }
    Ok(())
}

/// Bound every peer-supplied string on a sync item, once, at ingest: each of
/// them reaches a log line, `state.json` and an operator's terminal, and a
/// frame may carry a megabyte of them.
fn bound_item(item: &mut crate::protocol::SyncItem) {
    item.reason = crate::protocol::text(item.reason.take(), MAX_REASON_BYTES);
    item.worker_task_id = crate::protocol::text(item.worker_task_id.take(), MAX_TASK_ID_BYTES);
}

/// Log a worker's explanation for a state. The phase carries the same reason
/// into task views; the log keeps it for the keys no view covers — an unknown
/// key with no execution of its own, say.
fn log_reason(config: &RemoteConfig, exec_key: &str, state: &str, reason: Option<&str>) {
    if let Some(reason) = reason {
        tracing::warn!(alias = %config.alias, %exec_key, %state, %reason, "Remote execution state");
    }
}

fn net_error(error: std::io::Error) -> Error {
    Error::other(format!("Peer connection: {error}"))
}

/// A worker's free-form error text ends up in `last_error`, a task's error and
/// operator output, so it gets the same bound as every other peer string.
fn peer_text(text: &str) -> String {
    metadata_text(text, MAX_REASON_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SyncItem;
    use futures::StreamExt;
    use orch_core::test_support::FakeMessageStore;
    fn request() -> RemoteSubmit {
        RemoteSubmit {
            alias: "w1".into(),
            exec_key: "run1".into(),
            agent_id: "agent1".into(),
            from: "cli".into(),
            prompt: "p".into(),
            title: "t".into(),
            priority: 2,
            review_criteria: vec![],
            goal_id: None,
        }
    }
    /// A controller context over a real engine in a temporary project.
    async fn controller() -> (tempfile::TempDir, Context) {
        use orch_engine::{EngineSeams, build_full_with};
        use orch_infra::storage::remote::FileRemoteStore;
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
        let ctx = Context {
            store: FileRemoteStore::new(engine.paths.clone()),
            engine,
            instance_id: "controller-id".into(),
            name: "controller".into(),
        };
        (temp, ctx)
    }

    /// Run one real outbound session against a socket the test speaks for.
    fn spawn_session(ctx: Context, hub: Arc<LinkHub>, config: RemoteConfig) -> AbortOnDrop {
        AbortOnDrop(tokio::spawn(async move {
            let mut status = RemoteStatus {
                alias: config.alias.clone(),
                addr: config.addr.clone(),
                online: false,
                last_seen: None,
                last_error: None,
                peer_name: None,
                instance_id: None,
                os: None,
                agents: vec![],
                peer_version: None,
                link_state: LinkState::Connecting,
                next_retry_at: None,
                attempts: 0,
            };
            let _ = session(
                &ctx,
                &hub,
                &config,
                &mut status,
                &mut SessionHealth::default(),
            )
            .await;
        }))
    }

    async fn welcome(read: &mut BufReader<tokio::net::TcpStream>) {
        assert!(matches!(
            read_frame(read).await.unwrap(),
            Some(Frame::Hello { .. })
        ));
        write_frame(
            read.get_mut(),
            &Frame::Welcome {
                proto: PROTOCOL,
                instance_id: "worker-id".into(),
                name: "worker".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn chunked_sync_finishes_before_new_submits_are_attempted() {
        let (_temp, ctx) = controller().await;
        let hub = Arc::new(LinkHub::new(ctx.engine.messages.clone()));
        let _one = hub.submit(request()).unwrap();
        let mut two = request();
        two.exec_key = "run2".into();
        let _two = hub.submit(two).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RemoteConfig {
            alias: "w1".into(),
            addr: listener.local_addr().unwrap().to_string(),
            added_at: "now".into(),
        };
        let _session = spawn_session(ctx, hub.clone(), config);
        tokio::time::timeout(Duration::from_secs(3), async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut read = BufReader::new(socket);
            welcome(&mut read).await;
            assert!(matches!(
                read_frame(&mut read).await.unwrap(),
                Some(Frame::Sync { .. })
            ));
            write_frame(
                read.get_mut(),
                &Frame::SyncState {
                    items: vec![crate::protocol::SyncItem::new("run1", "unknown")],
                },
            )
            .await
            .unwrap();
            let mut attachments = 0;
            let mut fenced = false;
            while attachments < 2 || !fenced {
                match read_frame(&mut read).await.unwrap() {
                    Some(Frame::Attach { .. }) => attachments += 1,
                    Some(Frame::Ping) => fenced = true,
                    other => panic!("submit raced incomplete sync: {other:?}"),
                }
            }
            assert!(!hub.attempted("w1", "run1"));
            assert!(!hub.attempted("w1", "run2"));
            write_frame(
                read.get_mut(),
                &Frame::SyncState {
                    items: vec![crate::protocol::SyncItem::new("run2", "unknown")],
                },
            )
            .await
            .unwrap();
            write_frame(read.get_mut(), &Frame::Pong).await.unwrap();
            let mut keys = Vec::new();
            while keys.len() < 2 {
                match read_frame(&mut read).await.unwrap() {
                    Some(Frame::Submit { exec_key, .. }) => keys.push(exec_key),
                    other => panic!("expected submit after sync fence: {other:?}"),
                }
            }
            keys.sort();
            assert_eq!(keys, ["run1", "run2"]);
        })
        .await
        .expect("chunked sync deadline");
    }

    /// A peer that accepts a connection, handshakes and then goes silent —
    /// SIGSTOPped, half-open, or blocked inside its own session — must not
    /// pin this link. Every write is bounded by the codec's own 5s budget and
    /// the session's 15s heartbeat ends it, so the alias goes offline and the
    /// supervisor can redial.
    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_ends_the_session_and_takes_the_alias_offline() {
        let (_temp, ctx) = controller().await;
        let hub = Arc::new(LinkHub::new(ctx.engine.messages.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RemoteConfig {
            alias: "w1".into(),
            addr: listener.local_addr().unwrap().to_string(),
            added_at: "now".into(),
        };
        ctx.store.add(config.clone()).await.unwrap();
        let (done, mut ended) = mpsc::channel(1);
        let session_ctx = ctx.clone();
        let session_config = config.clone();
        let _session = AbortOnDrop(tokio::spawn(async move {
            let mut status = RemoteStatus {
                alias: session_config.alias.clone(),
                addr: session_config.addr.clone(),
                online: false,
                last_seen: None,
                last_error: None,
                peer_name: None,
                instance_id: None,
                os: None,
                agents: vec![],
                peer_version: None,
                link_state: LinkState::Connecting,
                next_retry_at: None,
                attempts: 1,
            };
            let result = session(
                &session_ctx,
                &hub,
                &session_config,
                &mut status,
                &mut SessionHealth::default(),
            )
            .await;
            let _ = done.send(result.map(|_| ()).unwrap_err().to_string()).await;
        }));
        // Handshake, then never read and never answer another frame.
        let (socket, _) = listener.accept().await.unwrap();
        let mut read = BufReader::new(socket);
        welcome(&mut read).await;
        let error = tokio::time::timeout(Duration::from_secs(120), ended.recv())
            .await
            .expect("a silent peer must not hold the session open")
            .expect("the session reported why it ended");
        assert!(
            error.contains("timed out") || error.contains("Peer connection"),
            "unexpected session end: {error}"
        );
    }

    /// A simulated controller restart: the reservation survived, so the key
    /// must be in the first `Sync` — a live mirror missing from that list is
    /// cancelled by the worker as an orphan — and the request must never be
    /// sent a second time.
    #[tokio::test]
    async fn a_rehydrated_execution_is_in_the_first_sync_and_never_resubmitted() {
        let (_temp, ctx) = controller().await;
        let hub = Arc::new(LinkHub::new(ctx.engine.messages.clone()));
        let mut handle = hub.rehydrate("w1", "run1").unwrap();
        assert_eq!(hub.active("w1"), vec!["run1"]);
        assert!(
            hub.submits("w1").is_empty(),
            "the worker already has the request"
        );
        assert!(hub.attempted("w1", "run1"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = RemoteConfig {
            alias: "w1".into(),
            addr: listener.local_addr().unwrap().to_string(),
            added_at: "now".into(),
        };
        let _session = spawn_session(ctx, hub.clone(), config);
        tokio::time::timeout(Duration::from_secs(3), async {
            let (socket, _) = listener.accept().await.unwrap();
            let mut read = BufReader::new(socket);
            welcome(&mut read).await;
            let Some(Frame::Sync { active }) = read_frame(&mut read).await.unwrap() else {
                panic!("sync must be the first post-handshake frame")
            };
            assert_eq!(active, vec!["run1".to_owned()]);
            // The worker still has it, queued behind its own approval.
            write_frame(
                read.get_mut(),
                &Frame::SyncState {
                    items: vec![SyncItem {
                        worker_task_id: Some("tsk_mirror".into()),
                        ..SyncItem::new("run1", "running")
                    }],
                },
            )
            .await
            .unwrap();
            write_frame(read.get_mut(), &Frame::Pong).await.unwrap();
            // Only the re-attachment and the heartbeat follow it.
            let mut attached = false;
            while !attached {
                match read_frame(&mut read).await.unwrap() {
                    Some(Frame::Attach {
                        exec_key,
                        after_seq,
                    }) => {
                        assert_eq!((exec_key.as_str(), after_seq), ("run1", 0));
                        attached = true;
                    }
                    Some(Frame::Ping) => {}
                    other => panic!("a rehydrated execution was resubmitted: {other:?}"),
                }
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(300), read_frame(&mut read))
                    .await
                    .is_err(),
                "nothing more is owed to the worker"
            );
        })
        .await
        .expect("rehydrated sync deadline");
        // The text hint is progress for the run log; the phase follows it.
        let mut resolved = None;
        while resolved.is_none() {
            match handle.events.next().await {
                Some(RemoteRunEvent::Hint(_)) => {}
                other => resolved = Some(phase(other)),
            }
        }
        let resolved = resolved.unwrap();
        assert_eq!(resolved.phase, RemotePhase::Running);
        assert_eq!(resolved.worker_task_id.as_deref(), Some("tsk_mirror"));
    }

    #[tokio::test]
    async fn oversized_submit_fails_locally_and_next_submit_is_delivered() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let _connected = hub.connected("w1");
        let mut huge = request();
        huge.prompt = "\\".repeat(crate::codec::MAX_FRAME_BYTES / 2);
        let mut handle = hub.submit(huge).unwrap();
        assert!(
            matches!(tokio::time::timeout(Duration::from_secs(1), handle.events.next()).await.expect("local submit rejection deadline"), Some(RemoteRunEvent::Finished(RemoteOutcome::Failed { error, .. })) if error.contains("prompt too large for remote frame"))
        );
        assert!(handle.events.next().await.is_none());
        assert!(hub.active("w1").is_empty());
        assert!(hub.queues().get("w1").unwrap().online);
        let _valid = hub.submit(request()).unwrap();
        let mut bytes = Vec::new();
        for frame in hub.submits("w1") {
            write_frame(&mut bytes, &frame).await.unwrap();
        }
        let mut read = BufReader::new(bytes.as_slice());
        assert!(
            matches!(read_frame(&mut read).await.unwrap(), Some(Frame::Submit { prompt, .. }) if prompt == "p")
        );
    }

    #[tokio::test]
    async fn later_submissions_are_not_uncertain_when_an_earlier_write_fails() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let _one = hub.submit(request()).unwrap();
        let mut two = request();
        two.exec_key = "run2".into();
        let _two = hub.submit(two).unwrap();
        let batch = hub.submits("w1");
        assert_eq!(batch.len(), 2);
        assert!(!hub.attempted("w1", "run1"));
        assert!(!hub.attempted("w1", "run2"));
        hub.mark_attempted("w1", &batch[0]);
        assert!(hub.attempted("w1", "run1"));
        assert!(!hub.attempted("w1", "run2"));
        assert!(
            matches!(&hub.submits("w1")[0], Frame::Submit {exec_key, ..} if exec_key == "run2")
        );
    }

    #[tokio::test]
    async fn a_full_progress_queue_never_drops_the_terminal_envelope() {
        use orch_core::adapter::{AgentEvent, AgentEventKind};
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut handle = hub.submit(request()).unwrap();
        for seq in 1..=1025 {
            hub.event(
                "w1",
                "run1",
                seq,
                AgentEvent::new(
                    AgentEventKind::Output,
                    "now",
                    serde_json::json!({"text": "progress"}),
                ),
            );
        }
        assert!(hub.terminal("w1", "run1", RemoteRunEvent::Unknown));
        assert_eq!(hub.active("w1"), vec!["run1"]);
        for _ in 0..1024 {
            assert!(matches!(
                handle.events.next().await,
                Some(RemoteRunEvent::Event(_))
            ));
        }
        hub.flush("w1");
        assert!(hub.active("w1").is_empty());
        assert!(matches!(
            handle.events.next().await,
            Some(RemoteRunEvent::Unknown)
        ));
    }

    #[tokio::test]
    async fn queued_hints_are_visible_progress_and_only_sent_on_change() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut handle = hub.submit(request()).unwrap();
        hub.hint("w1", "run1", "queued", "now".into());
        hub.hint("w1", "run1", "queued", "later".into());
        // A `Hint`, never an `Event`: this line is the controller talking
        // about its own link, and reading it as the worker's progress is what
        // showed an approval-held mirror as `RUN`.
        match handle.events.next().await.unwrap() {
            RemoteRunEvent::Hint(event) => assert_eq!(event.data["text"], "waiting: queued on w1"),
            other => panic!("expected a queued hint, got {other:?}"),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(5), handle.events.next())
                .await
                .is_err()
        );
    }

    fn phase(event: Option<RemoteRunEvent>) -> RemotePhaseUpdate {
        match event {
            Some(RemoteRunEvent::Phase(update)) => update,
            other => panic!("expected a phase update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_worker_drives_the_phase_and_repeats_are_not_forwarded() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut handle = hub.submit(request()).unwrap();
        hub.phase(
            "w1",
            "run1",
            RemotePhaseUpdate {
                phase: RemotePhase::Queued,
                worker_task_id: Some("tsk_mirror".into()),
                reason: None,
            },
        );
        assert_eq!(
            phase(handle.events.next().await),
            RemotePhaseUpdate {
                phase: RemotePhase::Queued,
                worker_task_id: Some("tsk_mirror".into()),
                reason: None,
            }
        );
        // The same state again, and without the task id the worker already
        // named: neither is news.
        hub.phase("w1", "run1", RemotePhaseUpdate::new(RemotePhase::Queued));
        hub.phase("w1", "run1", RemotePhaseUpdate::new(RemotePhase::Running));
        let running = phase(handle.events.next().await);
        assert_eq!(running.phase, RemotePhase::Running);
        assert_eq!(running.worker_task_id.as_deref(), Some("tsk_mirror"));
        assert!(
            tokio::time::timeout(Duration::from_millis(5), handle.events.next())
                .await
                .is_err(),
            "a repeated state must not reach the collector"
        );
    }

    #[tokio::test]
    async fn a_dropped_link_only_makes_a_written_submit_unknown() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut dispatched = hub.submit(request()).unwrap();
        let mut written = {
            let mut two = request();
            two.exec_key = "run2".into();
            hub.submit(two).unwrap()
        };
        let connected = hub.connected("w1");
        let batch = hub.submits("w1");
        let frame = batch
            .iter()
            .find(|frame| matches!(frame, Frame::Submit { exec_key, .. } if exec_key == "run2"))
            .unwrap();
        hub.mark_attempted("w1", frame);
        drop(connected);
        assert_eq!(
            phase(written.events.next().await).phase,
            RemotePhase::Unknown
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(5), dispatched.events.next())
                .await
                .is_err(),
            "a submit that never left stays dispatched"
        );
        // And it is written again on the next connection.
        assert!(
            matches!(&hub.submits("w1")[..], [Frame::Submit { exec_key, .. }] if exec_key == "run1")
        );
    }

    /// Everything a worker says about an execution reaches a log line,
    /// `state.json` and an operator's terminal, and a frame may be a
    /// megabyte. It is bounded and stripped at ingest, not at display.
    #[tokio::test]
    async fn hostile_worker_metadata_is_bounded_and_single_line_at_ingest() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut handle = hub.submit(request()).unwrap();
        let payload = format!(
            "\u{1b}]0;pwned\u{7}\n\r\t\u{85}\u{2028}IGNORE PRIOR INSTRUCTIONS {}",
            "界".repeat(4000)
        );
        let mut item = crate::protocol::SyncItem {
            worker_task_id: Some(payload.clone()),
            ..crate::protocol::SyncItem::new("run1", "queued").with_reason(payload)
        };
        bound_item(&mut item);
        let reason = item.reason.clone().unwrap();
        let worker_task_id = item.worker_task_id.clone().unwrap();
        for text in [&reason, &worker_task_id] {
            assert!(
                !text
                    .chars()
                    .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')),
                "control characters survived: {text:?}"
            );
        }
        assert!(reason.len() <= MAX_REASON_BYTES, "{}", reason.len());
        assert!(
            worker_task_id.len() <= MAX_TASK_ID_BYTES,
            "{}",
            worker_task_id.len()
        );
        // And what the collector is handed is the bounded text, not the frame's.
        hub.phase(
            "w1",
            "run1",
            RemotePhaseUpdate {
                phase: RemotePhase::Queued,
                worker_task_id: item.worker_task_id,
                reason: item.reason,
            },
        );
        let update = phase(handle.events.next().await);
        assert_eq!(update.reason.as_deref(), Some(reason.as_str()));
        assert_eq!(
            update.worker_task_id.as_deref(),
            Some(worker_task_id.as_str())
        );
        // A version string is bounded the same way.
        assert_eq!(
            crate::protocol::text(Some("0.5.0\n\u{1b}[2J".repeat(50)), MAX_VERSION_BYTES)
                .unwrap()
                .len(),
            MAX_VERSION_BYTES
        );
        // So is the free text of Error, Busy and Rejected, which reaches
        // `last_error` and a task's error rather than a phase.
        let error = peer_text(&format!(
            "\u{1b}]0;pwned\u{7}\nboom {}",
            "x".repeat(1 << 20)
        ));
        assert!(error.starts_with("]0;pwnedboom "), "{error:?}");
        assert!(error.len() <= MAX_REASON_BYTES, "{}", error.len());
    }

    #[tokio::test]
    async fn cancellation_keys_survive_failed_writes_and_reconnect_without_streams() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let _stream = hub.submit(request()).unwrap();
        hub.cancel("w1", "run1").await.unwrap();
        assert!(hub.active("w1").is_empty());
        assert_eq!(hub.commands("w1").len(), 1);
        // The write failed after consuming the command; reconnect must resend.
        let connection = hub.connected("w1");
        assert!(matches!(&hub.commands("w1")[0], Frame::Cancel { exec_key } if exec_key == "run1"));
        drop(connection);
        let pending = hub
            .drain_cancellations(Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(
            pending,
            vec![PendingRemoteCancellation {
                alias: "w1".into(),
                exec_key: "run1".into()
            }]
        );
        hub.cancel_ack("w1", "run1");
        assert!(hub.pending().is_empty());
    }

    #[tokio::test]
    async fn drain_wakes_writer_and_waits_for_cancel_ack_for_prior_explicit_cancel() {
        let hub = Arc::new(LinkHub::new(Arc::new(FakeMessageStore::new())));
        let connection = hub.connected("w1");
        hub.cancel("w1", "run1").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), connection.wake.notified())
            .await
            .unwrap();
        hub.commands("w1");
        let drain = tokio::spawn({
            let hub = hub.clone();
            async move {
                hub.drain_cancellations(Duration::from_secs(3))
                    .await
                    .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(1), connection.wake.notified())
            .await
            .unwrap();
        assert!(
            !drain.is_finished(),
            "socket write is not proof of cancellation"
        );
        hub.cancel_ack("w1", "run1");
        assert!(drain.await.unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn drain_uses_one_global_deadline_for_all_connected_peers_and_keys() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let _one = hub.connected("w1");
        let _two = hub.connected("w2");
        for alias in ["w1", "w2", "offline"] {
            for key in ["run1", "run2"] {
                hub.cancel(alias, key).await.unwrap();
            }
        }
        let started = Instant::now();
        let pending = hub
            .drain_cancellations(Duration::from_secs(3))
            .await
            .unwrap();
        assert_eq!(started.elapsed(), Duration::from_secs(3));
        assert_eq!(pending.len(), 6);
        assert_eq!(
            hub.pending(),
            pending,
            "deadline must retain cancellation keys"
        );
    }

    #[tokio::test]
    async fn drain_returns_pending_immediately_when_connected_peer_drops() {
        let hub = Arc::new(LinkHub::new(Arc::new(FakeMessageStore::new())));
        let connection = hub.connected("w1");
        hub.cancel("w1", "run1").await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), connection.wake.notified())
            .await
            .unwrap();
        let drain = tokio::spawn({
            let hub = hub.clone();
            async move {
                hub.drain_cancellations(Duration::from_secs(3))
                    .await
                    .unwrap()
            }
        });
        tokio::time::timeout(Duration::from_secs(1), connection.wake.notified())
            .await
            .unwrap();
        drop(connection);
        let pending = tokio::time::timeout(Duration::from_millis(100), drain)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn backoff_escalates_to_the_cap_within_a_quarter_either_way() {
        let mut backoff = Backoff::default();
        for base in [1u64, 2, 4, 8, 15, 15, 15, 15] {
            let delay = backoff.next();
            let base = Duration::from_secs(base);
            assert!(
                delay >= base - base / 4 && delay <= base + base / 4,
                "{delay:?} is not {base:?} ±25%"
            );
        }
        // Spread, not a fixed offset: a shared drop must not redial in step.
        let mut backoff = Backoff::default();
        let mut capped = std::collections::BTreeSet::new();
        for _ in 0..16 {
            capped.insert(backoff.next());
        }
        assert!(capped.len() > 1, "jitter produced one value: {capped:?}");
    }

    /// The live failure: a worker that closed every session right after the
    /// handshake saw a redial about every second, because any handshake at all
    /// reset the backoff. Only a session that cleared the sync fence and then
    /// stayed up may reset it.
    #[tokio::test(start_paused = true)]
    async fn only_a_fenced_session_that_stayed_up_resets_the_backoff() {
        let mut dead_on_arrival = SessionHealth::default();
        assert!(
            !dead_on_arrival.healthy(),
            "an unfenced session is no proof"
        );
        dead_on_arrival.fenced_at = Some(Instant::now());
        assert!(
            !dead_on_arrival.healthy(),
            "a session that died at the fence is no proof"
        );
        tokio::time::advance(HEALTHY_SESSION - Duration::from_millis(1)).await;
        assert!(!dead_on_arrival.healthy());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(dead_on_arrival.healthy());
    }
    #[tokio::test]
    async fn streams_survive_reconnect_and_cancel_removes_active_key() {
        let hub = LinkHub::new(Arc::new(FakeMessageStore::new()));
        let mut handle = hub.submit(request()).unwrap();
        assert_eq!(handle.pid, 0);
        assert_eq!(hub.active("w1"), vec!["run1"]);
        let submits = hub.submits("w1");
        assert_eq!(submits.len(), 1);
        assert!(!hub.attempted("w1", "run1"));
        hub.mark_attempted("w1", &submits[0]);
        assert!(hub.submits("w1").is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(5), handle.events.next())
                .await
                .is_err()
        );
        hub.terminal("w1", "run1", RemoteRunEvent::Unknown);
        assert!(
            hub.active("w1").is_empty(),
            "unknown keys must retire after delivery"
        );
        assert!(matches!(
            handle.events.next().await,
            Some(RemoteRunEvent::Unknown)
        ));
        hub.cancel("w1", "run1").await.unwrap();
        assert!(hub.active("w1").is_empty());
        assert!(matches!(&hub.commands("w1")[0],Frame::Cancel{exec_key} if exec_key=="run1"));
    }
}
