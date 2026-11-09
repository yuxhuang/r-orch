//! Engine-owner TCP peers. Socket loss never decides an execution's outcome.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod codec;
mod link;
pub mod protocol;
mod worker;

use orch_core::error::{Error, Result};
use orch_core::storage::MessageStore;
use orch_domain::{Agent, Message, MessageChannel, MessageStatus};
use orch_engine::{EventBus, FullEngine};
use orch_infra::storage::remote::{
    FileRemoteStore, default_peer_name, public_snapshot, validate_snapshot,
};
use protocol::Frame;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// The warning for a listen address other hosts may reach, or `None` for a
/// loopback IP. Anything that does not parse as one (a hand-edited
/// `peer.listen` such as `localhost:7690`) warns rather than guess.
pub fn exposure_warning(addr: &str) -> Option<String> {
    let parsed = addr.parse::<std::net::SocketAddr>().ok();
    if parsed.is_some_and(|parsed| parsed.ip().is_loopback()) {
        return None;
    }
    let reach = if parsed.is_some_and(|parsed| parsed.ip().is_unspecified()) {
        "on ALL network interfaces"
    } else {
        "beyond this host"
    };
    Some(format!(
        "Peer listener {addr} accepts connections {reach} with no authentication \
         or encryption: any host that can reach it can run tasks on this instance's \
         agents, including shell commands. Use it only on a trusted network; bind \
         127.0.0.1 to keep it local."
    ))
}

/// While an owner is running, expired messages (including removed aliases)
/// are physically pruned at startup and every 30 seconds, plus storage/lock
/// latency. No quota is imposed on valid pending messages; without a TTL they
/// remain until acknowledged/deleted. Missed sweeps do not accumulate bursts.
const MESSAGE_EXPIRY_INTERVAL: Duration = Duration::from_secs(30);

async fn expire_messages(messages: Arc<dyn MessageStore>, events: Arc<EventBus>) {
    let mut interval = tokio::time::interval(MESSAGE_EXPIRY_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        // A purge is a complete state operation (design D2): it orders with
        // every other reader and writer of the project and advances the
        // commit sequence, instead of rewriting the store under one of them.
        let purged = events
            .operation_boundary()
            .run(true, messages.purge_expired())
            .await
            .value;
        if let Err(error) = purged {
            events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
                error: error.to_string(),
                context: "Expire queued messages; check message storage permissions and free space. Cleanup will retry in 30 seconds".into(),
                fatal: false,
            });
        }
    }
}

#[derive(Clone)]
pub(crate) struct Context {
    pub engine: Arc<FullEngine>,
    pub store: FileRemoteStore,
    pub instance_id: String,
    pub name: String,
}

impl Context {
    /// Run one complete peer state phase — read, validate and write — inside
    /// the project's operation boundary (design D2, spec R12), so it orders
    /// with RPC handlers, dispatch and every other writer and its commit is
    /// sequenced. Callers keep network writes, acknowledgement waits and
    /// process kills outside: the permit is never held across a peer. A
    /// phase reached from an operation that already holds the permit (the
    /// worker's [`handle_ordered`](worker::Worker)) reuses it.
    pub(crate) async fn ordered<T>(&self, mutation: bool, phase: impl Future<Output = T>) -> T {
        self.engine
            .events
            .operation_boundary()
            .run(mutation, phase)
            .await
            .value
    }

    pub(crate) async fn local_agents(&self) -> Result<Vec<Agent>> {
        let agents = self.ordered(false, self.engine.agents.list()).await?;
        validate_snapshot(&agents)?;
        Ok(public_snapshot::project(&agents))
    }

    pub(crate) async fn ingest(&self, peer: &str, frame: &Frame) -> Result<Option<Frame>> {
        let Frame::Msg {
            id,
            from,
            to,
            subject,
            body,
            reply_to,
        } = frame
        else {
            return Err(Error::invalid_arguments("Expected peer message"));
        };
        let message = Message {
            id: id.clone(),
            channel: MessageChannel::Direct,
            from_agent_id: from.clone(),
            to_agent_id: Some(to.clone()),
            subject: subject.clone(),
            body: body.clone(),
            reply_to: reply_to.clone(),
            created_at: self.engine.clock.now_iso(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
        };
        // The acknowledgement is returned, not written: the caller sends it
        // after the permit is released, and only once the message committed.
        let reason = match self
            .ordered(
                true,
                self.engine.message_service.ingest_incoming(peer, message),
            )
            .await
        {
            Ok(_) => None,
            Err(error) => {
                self.message_error(peer, "ingest", &error);
                // Storage/lock failures must remain pending and retryable.
                // Never disclose a local path or error chain to the peer.
                match &error {
                    Error::Domain(orch_domain::DomainError::InvalidArguments(detail)) => Some(
                        if detail == "Incoming message recipient must be a local agent" {
                            "unknown recipient: recipient must be a local agent"
                        } else {
                            "invalid incoming message"
                        }
                        .into(),
                    ),
                    _ => return Ok(None),
                }
            }
        };
        Ok(Some(Frame::MsgAck {
            id: id.clone(),
            reason,
        }))
    }

    fn message_error(&self, peer: &str, operation: &str, error: &Error) {
        self.engine
            .events
            .emit(orch_domain::OrchestratorEvent::OrchestratorError {
                error: error.to_string(),
                context: format!("Peer {peer} message {operation}; session remains connected"),
                fatal: false,
            });
    }

    /// The frames to send `peer` now. Expiry, the read and oversize
    /// rejections are one ordered phase; the caller writes the frames after.
    pub(crate) async fn outbox(&self, peer: &str) -> Result<Vec<Frame>> {
        self.ordered(true, self.outbox_phase(peer)).await
    }

    async fn outbox_phase(&self, peer: &str) -> Result<Vec<Frame>> {
        if let Err(error) = self.engine.messages.purge_expired().await {
            self.message_error(peer, "expire outbox", &error);
            return Ok(vec![]);
        }
        let messages = match self.engine.messages.list().await {
            Ok(messages) => messages,
            Err(error) => {
                self.message_error(peer, "read outbox", &error);
                return Ok(vec![]);
            }
        };
        let mut frames = Vec::new();
        for message in messages {
            if message.status != MessageStatus::Pending {
                continue;
            }
            let Some((to, alias)) = message
                .to_agent_id
                .as_deref()
                .and_then(|to| to.rsplit_once('@'))
            else {
                continue;
            };
            if alias != peer {
                continue;
            }
            let frame = Frame::Msg {
                id: message.id.clone(),
                from: message.from_agent_id,
                to: to.into(),
                subject: message.subject,
                body: message.body,
                reply_to: message.reply_to,
            };
            if crate::codec::fits(&frame) {
                frames.push(frame);
            } else {
                self.msg_ack(
                    peer,
                    &message.id,
                    Some("message too large for remote frame"),
                )
                .await?;
            }
        }
        Ok(frames)
    }

    pub(crate) async fn msg_ack(&self, peer: &str, id: &str, reason: Option<&str>) -> Result<()> {
        let result = self
            .ordered(true, async {
                if let Some(reason) = reason {
                    let reason = orch_infra::storage::remote::metadata_text(reason, 256);
                    orch_infra::storage::message::FileMessageStore::new(
                        self.engine.paths.clone(),
                        self.engine.clock.clone(),
                    )
                    .reject_remote_message(id, peer, &reason)
                    .await?;
                } else if let Some(message) = self.engine.messages.get(id).await?
                    && message
                        .to_agent_id
                        .as_deref()
                        .and_then(|to| to.rsplit_once('@'))
                        .is_some_and(|(_, alias)| alias == peer)
                {
                    self.engine.messages.mark_delivered(id).await?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = result {
            self.message_error(peer, "acknowledge", &error);
        }
        Ok(())
    }
}

pub(crate) struct AbortOnDrop(pub tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Held by an engine owner through shutdown. Constructing light command
/// containers never creates a runtime or opens any sockets.
pub struct PeerRuntime {
    engine: Arc<FullEngine>,
    tasks: Vec<AbortOnDrop>,
    listener_status: std::path::PathBuf,
    listener: Option<(Arc<worker::Worker>, tokio::net::TcpListener)>,
}

impl PeerRuntime {
    /// Start all peer services for an owner that has completed startup.
    pub async fn start(engine: Arc<FullEngine>) -> Result<Self> {
        let mut runtime = Self::prepare(engine).await?;
        runtime.start_listener();
        Ok(runtime)
    }

    /// Bind the configured port and install owner services before first dispatch.
    /// Call `start_listener` only after watch startup succeeds: accepting a flood
    /// earlier can exhaust descriptors during startup's fallible filesystem work.
    pub async fn prepare(engine: Arc<FullEngine>) -> Result<Self> {
        // A daemon owns the project through its own locks, not watch mode.
        if !engine.orchestrator.is_owner() && !engine.orchestrator.is_daemon_mode() {
            return Err(Error::invalid_arguments(
                "Peer runtime requires the engine owner",
            ));
        }
        let store = FileRemoteStore::new(engine.paths.clone());
        // Status reset, inbound refresh and the listener record are one
        // ordered phase (design D2); binding and linking happen after it.
        let boundary = engine.events.operation_boundary();
        let (instance_id, name) = boundary
            .run(true, async {
                Self::reset_published_state(&engine, &store).await?;
                let name = engine
                    .config
                    .peer
                    .name
                    .clone()
                    .unwrap_or_else(default_peer_name);
                if name.is_empty() || name.contains('@') {
                    return Err(Error::invalid_arguments(
                        "Peer name must be nonempty and contain no @",
                    ));
                }
                Ok((store.peer_id().await?, name))
            })
            .await
            .value?;
        let ctx = Context {
            instance_id,
            name,
            store,
            engine: engine.clone(),
        };
        let listener = if let Some(addr) = &engine.config.peer.listen {
            Some(
                tokio::net::TcpListener::bind(addr)
                    .await
                    .map_err(|e| Error::other(format!("Listen on {addr}: {e}")))?,
            )
        } else {
            None
        };
        let hub = Arc::new(link::LinkHub::new(engine.messages.clone()));
        let worker = worker::Worker::new(ctx.clone());
        engine.orchestrator.set_remote_bridge(hub.clone());
        engine.orchestrator.set_run_event_tap(worker.clone());
        // Before the first link: an execution this controller does not
        // re-register is missing from the first `Sync`, and the worker reads
        // a live mirror absent from that list as an orphan and cancels it.
        engine.orchestrator.rehydrate_remote_runs().await?;
        let tasks = vec![
            AbortOnDrop(tokio::spawn(expire_messages(
                engine.messages.clone(),
                engine.events.clone(),
            ))),
            AbortOnDrop(tokio::spawn(link::manage(ctx, hub))),
        ];
        Ok(Self {
            listener_status: engine.paths.remotes_dir().join("_listener.json"),
            engine,
            tasks,
            listener: listener.map(|listener| (worker, listener)),
        })
    }

    /// Withdraw what a previous owner published about its peers.
    async fn reset_published_state(engine: &FullEngine, store: &FileRemoteStore) -> Result<()> {
        for config in store.list().await? {
            if let Some(mut status) = store.read_status(&config.alias).await? {
                status.online = false;
                // A `remote remove` racing this reset revokes it rather than
                // seeing its snapshot recreated.
                if let Err(error) = store.publish_registered_status(&config, &status).await {
                    // Old snapshots cannot grant health without a live lease.
                    // Keep local dispatch and reconnect alive during a disk fault.
                    engine.events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
                        error: error.to_string(),
                        context: format!("Reset peer status for {}; remote dispatch unavailable. Check permissions and free space in {}", config.alias, engine.paths.remotes_dir().display()),
                        fatal: false,
                    });
                }
            }
        }
        // Refresh retained inbound discovery too, even when listening is now
        // disabled, so legacy snapshots no longer retain private agent config.
        if let Some(inbound) = store.read_inbound().await?
            && let Err(error) = store.write_inbound(&inbound).await
        {
            engine.events.emit(orch_domain::OrchestratorEvent::OrchestratorError {
                error: error.to_string(),
                context: format!("Refresh public inbound peer snapshot; legacy private data may remain on disk. Check permissions and free space in {}", engine.paths.remotes_dir().display()),
                fatal: false,
            });
        }
        let listener_status = engine.paths.remotes_dir().join("_listener.json");
        match tokio::fs::remove_file(&listener_status).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::io(
                    "Remove stale listener status",
                    &listener_status,
                    e,
                ));
            }
        }
        Ok(())
    }

    /// Begin inbound admission after the owner's initial tick has completed.
    pub fn start_listener(&mut self) {
        if let Some((worker, listener)) = self.listener.take() {
            let status = self.listener_status.clone();
            let events = self.engine.events.clone();
            self.tasks.push(AbortOnDrop(tokio::spawn(async move {
                if let Err(error) = worker.listen(listener).await {
                    tracing::error!(%error, "Peer listener stopped");
                }
                events
                    .operation_boundary()
                    .run(true, async { clear_listener_status(&status) })
                    .await;
            })));
        }
    }
}

/// Remove this process's published listener address once its socket closes,
/// so status never reports a closed listener (for example after TUI `/pause`).
/// A record stamped by another process's later listener is left alone.
fn clear_listener_status(path: &std::path::Path) {
    #[derive(serde::Deserialize)]
    struct Stamp {
        pid: Option<u32>,
    }
    let ours = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stamp>(&bytes).ok())
        .is_some_and(|stamp| stamp.pid == Some(std::process::id()));
    if ours
        && let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "Clear peer listener status");
    }
}

impl Drop for PeerRuntime {
    fn drop(&mut self) {
        self.tasks.clear();
        clear_listener_status(&self.listener_status);
        self.engine
            .orchestrator
            .set_remote_bridge(Arc::new(orch_core::remote::NoopRemoteBridge));
        self.engine.orchestrator.clear_run_event_tap();
        // The listener is gone with this runtime, so no controller is served
        // any more. Leaving the last one published would let its mirrors
        // dispatch with no session to report them to (TUI `/pause`).
        self.engine.orchestrator.set_inbound_controller(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct ExpiryStore(AtomicUsize);

    #[async_trait::async_trait]
    impl MessageStore for ExpiryStore {
        async fn save(&self, _: &Message) -> Result<()> {
            unreachable!()
        }
        async fn get(&self, _: &str) -> Result<Option<Message>> {
            unreachable!()
        }
        async fn list(&self) -> Result<Vec<Message>> {
            unreachable!()
        }
        async fn list_pending(&self, _: &str) -> Result<Vec<Message>> {
            unreachable!()
        }
        async fn mark_delivered(&self, _: &str) -> Result<()> {
            unreachable!()
        }
        async fn delete(&self, _: &str) -> Result<()> {
            unreachable!()
        }
        async fn purge_expired(&self) -> Result<usize> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(Error::other("injected storage fault"))
            } else {
                Ok(0)
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_runs_without_connections_and_retries_storage_errors_every_thirty_seconds() {
        let store = Arc::new(ExpiryStore::default());
        let events = Arc::new(EventBus::new());
        let mut receiver = events.subscribe();
        let cleanup = AbortOnDrop(tokio::spawn(expire_messages(store.clone(), events)));
        tokio::task::yield_now().await;
        assert_eq!(store.0.load(Ordering::SeqCst), 1);
        assert!(matches!(
            receiver.recv().await.unwrap().as_ref(),
            orch_domain::OrchestratorEvent::OrchestratorError { fatal: false, .. }
        ));
        tokio::time::advance(MESSAGE_EXPIRY_INTERVAL - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.0.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.0.load(Ordering::SeqCst), 2);
        tokio::time::advance(MESSAGE_EXPIRY_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(store.0.load(Ordering::SeqCst), 3);
        drop(cleanup);
        tokio::time::advance(MESSAGE_EXPIRY_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(store.0.load(Ordering::SeqCst), 3);
    }

    /// The standalone sweep is a complete operation (design D2): it never
    /// purges while another operation holds the project permit, and its purge
    /// commits with an advanced sequence once the permit is free.
    #[tokio::test(start_paused = true)]
    async fn expiry_waits_for_a_held_operation_and_commits_after_it() {
        let store = Arc::new(ExpiryStore(AtomicUsize::new(1)));
        let events = Arc::new(EventBus::new());
        let boundary = events.operation_boundary();
        let (entered, release) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let held = {
            let (boundary, entered, release) = (boundary.clone(), entered.clone(), release.clone());
            tokio::spawn(async move {
                boundary
                    .run(false, async {
                        entered.notify_one();
                        release.notified().await;
                    })
                    .await;
            })
        };
        entered.notified().await;
        let seq = boundary.current_commit_seq();
        let cleanup = AbortOnDrop(tokio::spawn(expire_messages(store.clone(), events)));
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            store.0.load(Ordering::SeqCst),
            1,
            "purged under a held permit"
        );
        assert_eq!(boundary.current_commit_seq(), seq);
        release.notify_one();
        held.await.unwrap();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(store.0.load(Ordering::SeqCst), 2);
        assert_eq!(boundary.current_commit_seq(), seq + 1);
        drop(cleanup);
    }
}
