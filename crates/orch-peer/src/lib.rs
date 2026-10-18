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
use std::sync::Arc;
use std::time::Duration;

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
        if let Err(error) = messages.purge_expired().await {
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
    pub(crate) async fn local_agents(&self) -> Result<Vec<Agent>> {
        let agents = self.engine.agents.list().await?;
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
        let reason = match self
            .engine
            .message_service
            .ingest_incoming(peer, message)
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

    pub(crate) async fn outbox(&self, peer: &str) -> Result<Vec<Frame>> {
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
        let result = async {
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
        }
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
        if !engine.orchestrator.is_owner() {
            return Err(Error::invalid_arguments(
                "Peer runtime requires the engine owner",
            ));
        }
        let store = FileRemoteStore::new(engine.paths.clone());
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
        let ctx = Context {
            instance_id: store.peer_id().await?,
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
            engine,
            tasks,
            listener_status,
            listener: listener.map(|listener| (worker, listener)),
        })
    }

    /// Begin inbound admission after the owner's initial tick has completed.
    pub fn start_listener(&mut self) {
        if let Some((worker, listener)) = self.listener.take() {
            let status = self.listener_status.clone();
            self.tasks.push(AbortOnDrop(tokio::spawn(async move {
                if let Err(error) = worker.listen(listener).await {
                    tracing::error!(%error, "Peer listener stopped");
                }
                clear_listener_status(&status);
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
}
