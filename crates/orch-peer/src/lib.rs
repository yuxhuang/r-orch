//! Engine-owner TCP peers. Socket loss never decides an execution's outcome.
pub mod codec;
mod link;
pub mod protocol;
mod worker;

use orch_core::error::{Error, Result};
use orch_domain::{Agent, Message, MessageChannel, MessageStatus};
use orch_engine::FullEngine;
use orch_infra::storage::remote::{FileRemoteStore, default_peer_name, validate_snapshot};
use protocol::Frame;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct Context {
    pub engine: Arc<FullEngine>,
    pub store: FileRemoteStore,
    pub instance_id: String,
    pub name: String,
}

impl Context {
    pub async fn local_agents(&self) -> Result<Vec<Agent>> {
        let agents = self.engine.agents.list().await?;
        validate_snapshot(&agents)?;
        Ok(agents)
    }

    pub async fn ingest(&self, peer: &str, frame: &Frame) -> Result<Frame> {
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
        self.engine
            .message_service
            .ingest_incoming(peer, message)
            .await?;
        Ok(Frame::MsgAck { id: id.clone() })
    }

    pub async fn outbox(&self, peer: &str) -> Result<Vec<Frame>> {
        self.engine.messages.purge_expired().await?;
        Ok(self
            .engine
            .messages
            .list()
            .await?
            .into_iter()
            .filter_map(|message| {
                if message.status != MessageStatus::Pending {
                    return None;
                }
                let (to, alias) = message.to_agent_id.as_deref()?.rsplit_once('@')?;
                if alias != peer {
                    return None;
                }
                Some(Frame::Msg {
                    id: message.id,
                    from: message.from_agent_id,
                    to: to.to_owned(),
                    subject: message.subject,
                    body: message.body,
                    reply_to: message.reply_to,
                })
            })
            .collect())
    }

    pub async fn msg_ack(&self, peer: &str, id: &str) -> Result<()> {
        if let Some(message) = self.engine.messages.get(id).await?
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
}

impl PeerRuntime {
    /// Called after owner-lock acquisition and recovery, before first dispatch.
    pub async fn start(engine: Arc<FullEngine>) -> Result<Self> {
        if !engine.orchestrator.is_owner() {
            return Err(Error::invalid_arguments(
                "Peer runtime requires the engine owner",
            ));
        }
        let store = FileRemoteStore::new(engine.paths.clone());
        for config in store.list().await? {
            if let Some(mut status) = store.read_status(&config.alias).await? {
                status.online = false;
                if let Err(error) = store.write_status(&status).await {
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
        let mut tasks = Vec::new();
        if let Some(listener) = listener {
            tasks.push(AbortOnDrop(tokio::spawn(async move {
                if let Err(error) = worker.listen(listener).await {
                    tracing::error!(%error, "Peer listener stopped");
                }
            })));
        }
        tasks.push(AbortOnDrop(tokio::spawn(link::manage(ctx, hub))));
        Ok(Self { engine, tasks })
    }
}

impl Drop for PeerRuntime {
    fn drop(&mut self) {
        self.tasks.clear();
        self.engine
            .orchestrator
            .set_remote_bridge(Arc::new(orch_core::remote::NoopRemoteBridge));
        self.engine.orchestrator.clear_run_event_tap();
    }
}
