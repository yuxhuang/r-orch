//! The worker owns mirrors and execution. A disconnected controller never
//! cancels a process; only an explicit cancel or a successful sync can do so.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orch_core::adapter::AgentEvent;
use orch_core::error::{Error, Result};
use orch_core::remote::{RemoteOutcome, RunEventTap};
use orch_core::storage::TaskFilter;
use orch_domain::{AgentStatus, CreateTaskInput, InboundStatus, Run, RunStatus, Task, TaskStatus};
use orch_infra::storage::{fs_utils::write_json, remote::validate_snapshot};
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

pub struct Worker {
    ctx: Context,
    tap: Mutex<TapState>,
    events: broadcast::Sender<(String, Frame)>,
    /// Serializes takeover and protocol mutations, so the replaced session
    /// cannot mutate a mirror after the new controller's sync.
    session: tokio::sync::Mutex<SessionSlot>,
}

impl Worker {
    pub fn new(ctx: Context) -> Arc<Self> {
        let (events, _) = broadcast::channel(RING_CAPACITY);
        Arc::new(Self {
            ctx,
            tap: Mutex::new(TapState::default()),
            events,
            session: tokio::sync::Mutex::new(SessionSlot::default()),
        })
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

    async fn listener_status(&self, addr: &str, controller: Option<&str>) -> Result<()> {
        write_json(
            &self.ctx.engine.paths.remotes_dir().join("_listener.json"),
            &serde_json::json!({"listen_addr": addr, "controller_name": controller}),
        )
        .await
    }

    pub async fn listen(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        let addr = listener
            .local_addr()
            .map_err(|e| Error::other(format!("peer listener: {e}")))?
            .to_string();
        self.listener_status(&addr, None).await?;
        for task in self.ctx.engine.tasks.list(&TaskFilter::default()).await? {
            self.register(&task);
        }
        let mut sessions = JoinSet::new();
        let mut accept_after = Instant::now();
        let mut accept_backoff = ACCEPT_RETRY_MIN;
        loop {
            tokio::select! {
                biased;
                // Reap completed tasks before accepting more, including under
                // a constant stream of short-lived connections.
                Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
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
                })
                .await?;
            self.listener_status(addr, Some(&name)).await?;
            slot.claim(&instance_id, &name)
                .expect("admission checked while holding session lock")
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
                        let snapshot = self.ctx.local_agents().await?;
                        if snapshot != agents {
                            self.write(
                                &mut write,
                                &Frame::Agents {
                                    agents: snapshot.clone(),
                                },
                            )
                            .await?;
                            agents = snapshot;
                        }
                        let updates = {
                            let slot = self.session.lock().await;
                            if !slot.is_current(generation) {
                                return Ok(());
                            }
                            self.updates(&instance_id, &mut sent).await?
                        };
                        for frame in updates {
                            self.write(&mut write, &frame).await?;
                        }
                        for frame in self.ctx.outbox(&name).await? {
                            self.write(&mut write, &frame).await?;
                        }
                    }
                }
            }
        }
        .await;
        let mut slot = self.session.lock().await;
        if slot.is_current(generation) {
            slot.controller = None;
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

    async fn item(&self, key: &str, task: Option<&Task>) -> Result<SyncItem> {
        let Some(task) = task else {
            return Ok(SyncItem {
                exec_key: key.into(),
                state: "unknown".into(),
                outcome: None,
            });
        };
        let state = self.ctx.engine.state.read().await?;
        if state.running.contains_key(&task.id) || state.claimed.contains(&task.id) {
            return Ok(SyncItem {
                exec_key: key.into(),
                state: "running".into(),
                outcome: None,
            });
        }
        // The caller's snapshot may predate the final state write. Reading
        // the task after the reservation disappeared observes post-processing.
        let latest = self.ctx.engine.tasks.get(&task.id).await?;
        let Some(task) = latest.as_ref() else {
            return Ok(SyncItem {
                exec_key: key.into(),
                state: "unknown".into(),
                outcome: None,
            });
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
            return Ok(SyncItem {
                exec_key: key.into(),
                state: "unknown".into(),
                outcome: None,
            });
        }
        let outcome = derive_outcome(task, run.as_ref());
        Ok(SyncItem {
            exec_key: key.into(),
            state: if outcome.is_some() {
                "finished"
            } else if matches!(
                task.status,
                TaskStatus::InProgress | TaskStatus::Review | TaskStatus::Done | TaskStatus::Failed
            ) {
                "running"
            } else {
                "queued"
            }
            .into(),
            outcome,
        })
    }

    async fn handle(&self, controller: &str, name: &str, frame: Frame) -> Result<Vec<Frame>> {
        match frame {
            Frame::Sync { active } => {
                let mirrors = self.mirrors(controller).await?;
                for task in &mirrors {
                    if let Some(key) = label(task, "exec:")
                        && !active.iter().any(|k| k == key)
                        && matches!(
                            task.status,
                            TaskStatus::Todo | TaskStatus::InProgress | TaskStatus::Retrying
                        )
                    {
                        self.ctx.engine.orchestrator.cancel_task(&task.id).await?;
                    }
                }
                let mut items = Vec::with_capacity(active.len());
                for key in active {
                    // Re-read after cancellation and any concurrently completed run.
                    let task = mirrors
                        .iter()
                        .find(|t| label(t, "exec:") == Some(key.as_str()));
                    let task = if let Some(task) = task {
                        self.ctx.engine.tasks.get(&task.id).await?
                    } else {
                        None
                    };
                    items.push(self.item(&key, task.as_ref()).await?);
                }
                let mut replies = vec![Frame::SyncState { items }];
                replies.extend(self.updates(controller, &mut HashMap::new()).await?);
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
                let tasks = self.ctx.engine.tasks.list(&TaskFilter::default()).await?;
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
                    self.ctx.engine.agents.get(&agent_id).await?
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
                let input = CreateTaskInput {
                    title,
                    description: Some(prompt),
                    priority: Some(priority),
                    assignee: Some(agent_id),
                    labels: Some(vec![
                        format!("remote:{name}"),
                        format!("remote-ctl:{controller}"),
                        format!("exec:{exec_key}"),
                        format!("remote-from:{from}"),
                    ]),
                    max_attempts: Some(1),
                    review_criteria: Some(review_criteria),
                    goal_id,
                    ..Default::default()
                };
                match self.ctx.engine.task_service.create(input).await {
                    Ok(task) => {
                        self.register(&task);
                        Ok(vec![
                            Frame::Submitted {
                                exec_key: exec_key.clone(),
                                worker_task_id: task.id,
                            },
                            Frame::State {
                                exec_key,
                                state: "queued".into(),
                            },
                        ])
                    }
                    Err(error) => Ok(vec![Frame::Rejected {
                        exec_key,
                        reason: error.to_string(),
                    }]),
                }
            }
            Frame::Cancel { exec_key } => {
                if let Some(task) = self
                    .mirrors(controller)
                    .await?
                    .iter()
                    .find(|t| label(t, "exec:") == Some(exec_key.as_str()))
                    && !matches!(
                        task.status,
                        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
                    )
                {
                    self.ctx.engine.orchestrator.cancel_task(&task.id).await?;
                }
                Ok(vec![Frame::CancelAck { exec_key }])
            }
            Frame::FinishedAck { exec_key } => {
                if let Some(mut task) = self
                    .mirrors(controller)
                    .await?
                    .into_iter()
                    .find(|t| label(t, "exec:") == Some(exec_key.as_str()))
                    && self.item(&exec_key, Some(&task)).await?.outcome.is_some()
                    && !task.labels.iter().any(|l| l == "remote-finished-ack")
                {
                    task.labels.push("remote-finished-ack".into());
                    self.ctx.engine.tasks.save(&task).await?;
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
                let mirrors = self.mirrors(controller).await?;
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
                validate_snapshot(&agents)?;
                if let Some(mut inbound) = self.ctx.store.read_inbound().await? {
                    inbound.agents = agents;
                    self.ctx.store.write_inbound(&inbound).await?;
                }
                Ok(vec![])
            }
            frame @ Frame::Msg { .. } => Ok(vec![self.ctx.ingest(name, &frame).await?]),
            Frame::MsgAck { id } => {
                self.ctx.msg_ack(name, &id).await?;
                Ok(vec![])
            }
            Frame::Ping => Ok(vec![Frame::Pong]),
            Frame::Pong => Ok(vec![]),
            _ => Err(Error::other("unexpected controller frame")),
        }
    }

    async fn updates(
        &self,
        controller: &str,
        sent: &mut HashMap<String, String>,
    ) -> Result<Vec<Frame>> {
        let mut replies = Vec::new();
        for task in self.mirrors(controller).await? {
            if task.labels.iter().any(|l| l == "remote-finished-ack") {
                continue;
            }
            let Some(key) = label(&task, "exec:") else {
                continue;
            };
            let item = self.item(key, Some(&task)).await?;
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
                });
            }
        }
        Ok(replies)
    }
}

impl RunEventTap for Worker {
    fn record(&self, task_id: &str, _run_id: &str, event: &AgentEvent) {
        let mut tap = self.tap.lock().expect("worker event ring poisoned");
        let Some((key, controller)) = tap.tasks.get(task_id).cloned() else {
            return;
        };
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

    #[tokio::test]
    async fn mirror_without_criteria_autoapproves_even_when_agent_policy_is_suggest() {
        let (temp, worker, agent_id) = fixture().await;
        let mut agent = worker
            .ctx
            .engine
            .agents
            .get(&agent_id)
            .await
            .unwrap()
            .unwrap();
        agent.config.approval_policy = Some(orch_domain::ApprovalPolicy::Suggest);
        agent.config.workspace_mode = Some(orch_domain::WorkspaceMode::Shared);
        agent.config.timeout_ms = Some(1_000);
        agent.config.env =
            Some(std::iter::once(("HOME".into(), temp.path().to_str().unwrap().into())).collect());
        worker.ctx.engine.agents.save(&agent).await.unwrap();
        worker
            .handle("controller", "C", submit("key", &agent_id))
            .await
            .unwrap();
        let task = mirror(&worker, "key").await;
        assert!(task.review_criteria.as_ref().unwrap().is_empty());

        let execution = timeout(Duration::from_secs(5), async {
            worker.ctx.engine.orchestrator.run_task(&task.id).await?;
            worker.ctx.engine.orchestrator.wait_for_collectors().await;
            Result::<()>::Ok(())
        })
        .await;
        // Shutdown also kills the bounded shell process on a timeout/error.
        let stopped = worker.ctx.engine.orchestrator.stop().await;
        execution.unwrap().unwrap();
        stopped.unwrap();

        let completed = mirror(&worker, "key").await;
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
        assert!(matches!(
            worker.item("key", Some(&completed)).await.unwrap().outcome,
            Some(RemoteOutcome::Succeeded { .. })
        ));
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
        assert_eq!(
            mirror(&worker, "abandoned").await.status,
            TaskStatus::Cancelled
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
            worker.item("key", Some(&task)).await.unwrap().outcome,
            Some(RemoteOutcome::Failed { .. })
        ));
        task.status = TaskStatus::Review;
        worker.ctx.engine.tasks.save(&task).await.unwrap();
        let item = worker.item("key", Some(&task)).await.unwrap();
        assert_eq!(item.state, "unknown");
        assert!(item.outcome.is_none());
        let updates = worker
            .updates("controller", &mut HashMap::new())
            .await
            .unwrap();
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
        let item = worker.item("key", Some(&task)).await.unwrap();
        assert_eq!(item.state, "running");
        assert!(item.outcome.is_none());
        state.claimed.clear();
        worker.ctx.engine.state.write(&state).await.unwrap();
        assert!(
            worker
                .item("key", Some(&task))
                .await
                .unwrap()
                .outcome
                .is_some()
        );
    }

    include!("worker_hardening_tests.rs");
}
