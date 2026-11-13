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
use orch_engine::container::commit::{CommitSource, CommitStore};
use orch_engine::{EventBus, FullEngine};
use orch_infra::storage::remote::{
    FileRemoteStore, default_peer_name, public_snapshot, validate_snapshot,
};
use protocol::Frame;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

pub use orch_domain::remote::exposure_warning;

/// While an owner is running, expired messages (including removed aliases)
/// are physically pruned at startup and every 30 seconds, plus storage/lock
/// latency. No quota is imposed on valid pending messages; without a TTL they
/// remain until acknowledged/deleted. Missed sweeps do not accumulate bursts.
const MESSAGE_EXPIRY_INTERVAL: Duration = Duration::from_secs(30);

async fn expire_messages(messages: Arc<dyn MessageStore>, events: Arc<EventBus>, stop: Shutdown) {
    let mut interval = tokio::time::interval(MESSAGE_EXPIRY_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => return,
            _ = interval.tick() => {}
        }
        // A purge is a complete state operation (design D2): it orders with
        // every other reader and writer of the project. Tracked storage
        // declares real or possibly partial writes; an empty purge is silent.
        let purged = events
            .operation_boundary()
            .run_from(CommitSource::Expiry, false, messages.purge_expired())
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

/// Cancellation is observed only at admission, idle and network waits. An
/// ordered state phase is always awaited to completion, including filesystem
/// work Tokio has already handed to its blocking pool.
#[derive(Clone)]
pub(crate) struct Shutdown(tokio::sync::watch::Sender<bool>);

impl Default for Shutdown {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }
}

impl Shutdown {
    fn stop(&self) {
        self.0.send_replace(true);
    }

    fn is_stopping(&self) -> bool {
        *self.0.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|stopping| *stopping).await;
    }

    async fn io<T>(&self, wait: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
        tokio::select! {
            biased;
            () = self.cancelled() => Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "peer runtime stopping")),
            result = wait => result,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Context {
    stop: Shutdown,
    pub engine: Arc<FullEngine>,
    pub store: FileRemoteStore,
    pub instance_id: String,
    pub name: String,
}

impl Context {
    async fn write<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        frame: &Frame,
    ) -> std::io::Result<()> {
        self.stop.io(codec::write_frame(writer, frame)).await
    }

    /// Run one complete peer state phase — read, validate and write — inside
    /// the project's operation boundary (design D2, spec R12), so it orders
    /// with RPC handlers, dispatch and every other writer and its commit is
    /// sequenced. Callers keep network writes, acknowledgement waits and
    /// process kills outside: the permit is never held across a peer. A
    /// phase reached from an operation that already holds the permit (the
    /// worker's [`handle_ordered`](worker::Worker)) reuses it. `mutation = false`
    /// means there are no undeclared writes: tracked adapters or explicit
    /// store declarations account for every possible write. Reads still take
    /// the permit.
    pub(crate) async fn ordered<T>(&self, mutation: bool, phase: impl Future<Output = T>) -> T {
        let boundary = self.engine.events.operation_boundary();
        let phase = Box::pin(phase);
        boundary
            .run_from(CommitSource::Peer, mutation, async {
                // Peer phases also write concrete remote stores that bypass
                // the engine adapters. Declare them before any possible write.
                if mutation {
                    boundary.note_store(CommitStore::Remote);
                }
                phase.await
            })
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
                false,
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
        self.ordered(false, self.outbox_phase(peer)).await
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
            .ordered(false, async {
                // Decide no-ops while holding the project permit. The raw
                // rejection writer is idempotent and can also repair an index
                // after an earlier partial write; neither case writes Remote.
                let Some(message) = self.engine.messages.get(id).await? else {
                    return Ok(());
                };
                if message
                    .to_agent_id
                    .as_deref()
                    .and_then(|to| to.rsplit_once('@'))
                    .is_none_or(|(_, alias)| alias != peer)
                {
                    return Ok(());
                }
                if let Some(reason) = reason {
                    if message.status == MessageStatus::Delivered
                        && message.body.starts_with(&format!("[rejected by {peer}: "))
                    {
                        // Match the store's already-rejected guard, but read
                        // the existing index without rebuilding it. A missing,
                        // corrupt or stale index must still reach the writer
                        // below, with its repair declared BEFORE any I/O.
                        let index = orch_infra::storage::index::IndexManager::<Message>::new(
                            self.engine.paths.messages_dir(),
                            orch_infra::storage::index::ItemFormat::Json,
                        );
                        let indexed: Result<Option<Vec<Message>>> =
                            orch_infra::storage::fs_utils::read_json(&index.index_path()).await;
                        if indexed
                            .is_ok_and(|items| items.is_some_and(|items| items.contains(&message)))
                        {
                            return Ok(());
                        }
                    }
                    self.engine
                        .events
                        .operation_boundary()
                        .note_store(CommitStore::Messages);
                    let reason = orch_infra::storage::remote::metadata_text(reason, 256);
                    orch_infra::storage::message::FileMessageStore::new(
                        self.engine.paths.clone(),
                        self.engine.clock.clone(),
                    )
                    .reject_remote_message(id, peer, &reason)
                    .await?;
                } else {
                    // The tracked adapter declares Messages before delivery,
                    // including a possible entity-only/index-failed write.
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
    stop: Shutdown,
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
            .run_from(CommitSource::Peer, true, async {
                boundary.note_store(CommitStore::Remote);
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
        let stop = Shutdown::default();
        let ctx = Context {
            stop: stop.clone(),
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
                stop.clone(),
            ))),
            AbortOnDrop(tokio::spawn(link::manage(ctx, hub))),
        ];
        Ok(Self {
            listener_status: engine.paths.remotes_dir().join("_listener.json"),
            engine,
            tasks,
            stop,
            listener: listener.map(|listener| (worker, listener)),
        })
    }

    /// Quiesce every peer writer before the owner releases its project locks.
    /// No task is aborted here: dropping a filesystem future could leave an
    /// already-started blocking rename running after the final cleanup.
    pub async fn shutdown(mut self) {
        self.stop.stop();
        self.listener.take();
        for task in &mut self.tasks {
            if let Err(error) = (&mut task.0).await {
                tracing::error!(%error, "Peer service failed during shutdown");
            }
        }
        self.tasks.clear();
        let boundary = self.engine.events.operation_boundary();
        boundary
            .run_from(CommitSource::Peer, true, async {
                boundary.note_store(CommitStore::Remote);
                clear_listener_status(&self.listener_status);
            })
            .await;
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
                let boundary = events.operation_boundary();
                boundary
                    .run_from(CommitSource::Peer, true, async {
                        boundary.note_store(CommitStore::Remote);
                        clear_listener_status(&status);
                    })
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
        self.stop.stop();
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

    /// A test filesystem executor: once entered, this real atomic rename is
    /// already running on the blocking pool and cannot be aborted by Tokio.
    /// Only the selected listener publication uses it; no production hook or
    /// environment variable is compiled into release builds.
    pub(crate) struct PublicationPause {
        entered: tokio::sync::oneshot::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
        published: tokio::sync::oneshot::Sender<()>,
    }

    impl PublicationPause {
        pub(crate) async fn publish(
            self,
            path: std::path::PathBuf,
            status: serde_json::Value,
        ) -> Result<()> {
            tokio::task::spawn_blocking(move || {
                let staged = path.with_extension("staged");
                std::fs::write(&staged, serde_json::to_vec(&status).unwrap()).unwrap();
                self.entered.send(()).unwrap();
                // Bound a failed test's worker lifetime so a red assertion
                // cannot hang the test runtime's blocking-pool shutdown.
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                std::fs::rename(staged, path).unwrap();
                let _ = self.published.send(());
            })
            .await
            .unwrap();
            Ok(())
        }
    }

    /// Drive the original pre-Sync Ping through a real admitted session,
    /// then stall its final publication after the blocking job has started.
    /// Stop must join runtime -> listener -> session -> filesystem worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_joins_a_started_publication_before_final_cleanup() {
        use codec::{read_frame, write_frame};
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let mut engine = orch_engine::build_full_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(temp.path().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        engine.engine.config.peer.listen = Some("127.0.0.1:0".into());
        engine.orchestrator.enable_daemon_mode();
        let engine = Arc::new(engine);
        let mut runtime = PeerRuntime::prepare(engine).await.unwrap();
        let path = runtime.listener_status.clone();
        let (worker, listener) = runtime.listener.as_ref().unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = worker.clone();
        runtime.start_listener();
        let socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut peer = tokio::io::BufReader::new(socket);
        write_frame(
            peer.get_mut(),
            &Frame::Hello {
                proto: protocol::PROTOCOL,
                instance_id: "old".into(),
                name: "old".into(),
                os: "test".into(),
                agents: vec![],
                version: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut peer).await.unwrap(),
            Some(Frame::Welcome { .. })
        ));
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, stalled) = std::sync::mpsc::channel();
        let (published, publication) = tokio::sync::oneshot::channel();
        *worker.publication_pause.lock().unwrap() = Some(PublicationPause {
            entered,
            release: stalled,
            published,
        });
        write_frame(peer.get_mut(), &Frame::Ping).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), started)
            .await
            .unwrap()
            .unwrap();
        let stop = runtime.stop.clone();
        let stopping = tokio::spawn(async move {
            runtime.shutdown().await;
        });
        tokio::time::timeout(Duration::from_secs(5), stop.cancelled())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stopped_early = stopping.is_finished();
        release.send(()).unwrap();
        publication.await.unwrap();
        stopping.await.unwrap();
        assert!(
            !stopped_early && !path.exists(),
            "stop returned early={stopped_early}; post-stop listener publication={}",
            path.exists()
        );
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }

    async fn metadata_context() -> (tempfile::TempDir, Context) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let engine = Arc::new(
            orch_engine::build_full_with(
                temp.path(),
                orch_engine::EngineSeams {
                    global_config_path: Some(temp.path().join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let ctx = Context {
            stop: Shutdown::default(),
            store: FileRemoteStore::new(engine.paths.clone()),
            instance_id: "instance_test".into(),
            name: "test".into(),
            engine,
        };
        (temp, ctx)
    }

    fn capture_commits(
        events: &EventBus,
    ) -> Arc<std::sync::Mutex<Vec<orch_engine::container::commit::CommitRecord>>> {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = records.clone();
        events
            .operation_boundary()
            .set_publisher(Some(Arc::new(move |p| {
                if let orch_engine::container::commit::Publication::Commit(record) = p {
                    sink.lock().unwrap().push(record.clone());
                }
            })));
        records
    }

    #[tokio::test]
    async fn peer_phase_records_source_and_remote_before_a_failed_write_phase() {
        use orch_engine::container::commit::{CommitChange, CommitSource, CommitStore};
        let (_temp, ctx) = metadata_context().await;
        let records = capture_commits(&ctx.engine.events);
        let status = orch_domain::InboundStatus {
            controller_name: "controller".into(),
            instance_id: "instance_controller".into(),
            connected_since: ctx.engine.clock.now_iso(),
            agents: vec![],
            controller_version: None,
        };
        let result: Result<()> = ctx
            .ordered(true, async {
                ctx.store.write_inbound(&status).await?;
                Err(Error::other("injected error after remote persistence"))
            })
            .await;
        assert!(result.is_err());
        assert!(ctx.store.read_inbound().await.unwrap().is_some());
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, CommitSource::Peer);
        assert!(!records[0].store_set_uncertain);
        assert_eq!(
            records[0].changes,
            vec![CommitChange::store_wide(CommitStore::Remote)]
        );
    }

    #[tokio::test]
    async fn peer_read_only_phase_produces_no_commit_and_nested_peer_reuses_tick() {
        use orch_engine::container::commit::{CommitSource, CommitStore};
        let (_temp, ctx) = metadata_context().await;
        let records = capture_commits(&ctx.engine.events);
        let boundary = ctx.engine.events.operation_boundary();
        let before = boundary.current_commit_seq();
        ctx.ordered(false, ctx.engine.agents.list()).await.unwrap();
        assert!(records.lock().unwrap().is_empty());
        assert_eq!(boundary.current_commit_seq(), before);
        boundary
            .run_from(CommitSource::Tick, true, async {
                ctx.ordered(true, async {}).await;
            })
            .await;
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1, "nested peer phase is not a second commit");
        assert_eq!(records[0].source, CommitSource::Tick);
        assert!(
            records[0]
                .changes
                .iter()
                .any(|c| c.store == CommitStore::Remote),
            "nested remote mutation must be declared"
        );
    }

    fn queued_message(id: &str) -> Message {
        Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "sender".into(),
            to_agent_id: Some("recipient@controller".into()),
            subject: String::new(),
            body: String::new(),
            reply_to: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
        }
    }

    #[tokio::test]
    async fn rejection_and_oversize_outbox_declare_only_messages() {
        let (_temp, ctx) = metadata_context().await;
        let mut message = queued_message("msg_reject");
        ctx.engine.messages.save(&message).await.unwrap();
        let records = capture_commits(&ctx.engine.events);
        ctx.msg_ack("controller", &message.id, Some("unknown recipient"))
            .await
            .unwrap();
        {
            let records = records.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].source, CommitSource::Peer);
            assert!(!records[0].store_set_uncertain);
            assert_eq!(
                records[0].changes,
                vec![orch_engine::container::commit::CommitChange::store_wide(
                    CommitStore::Messages
                )]
            );
        }
        message.id = "msg_oversize".into();
        message.body = "x".repeat(codec::MAX_FRAME_BYTES + 1);
        ctx.engine.messages.save(&message).await.unwrap();
        records.lock().unwrap().clear();
        assert!(ctx.outbox("controller").await.unwrap().is_empty());
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, CommitSource::Peer);
        assert_eq!(
            records[0].changes,
            vec![orch_engine::container::commit::CommitChange::store_wide(
                CommitStore::Messages
            )]
        );
        assert!(!records[0].store_set_uncertain);
    }

    #[tokio::test]
    async fn rejection_noops_are_silent_but_stale_and_missing_indexes_are_repaired() {
        let (_temp, ctx) = metadata_context().await;
        let message = queued_message("msg_idempotent");
        ctx.engine.messages.save(&message).await.unwrap();
        let records = capture_commits(&ctx.engine.events);
        for reason in [None, Some("no recipient")] {
            for (peer, id) in [("controller", "absent"), ("other", "msg_idempotent")] {
                ctx.msg_ack(peer, id, reason).await.unwrap();
                assert!(records.lock().unwrap().is_empty(), "no-op ack {peer}/{id}");
            }
        }
        ctx.msg_ack("controller", &message.id, Some("no recipient"))
            .await
            .unwrap();
        assert_eq!(records.lock().unwrap().len(), 1);
        records.lock().unwrap().clear();
        ctx.msg_ack("controller", &message.id, Some("no recipient"))
            .await
            .unwrap();
        assert!(
            records.lock().unwrap().is_empty(),
            "already rejected and indexed"
        );
        let index = ctx.engine.paths.messages_dir().join("_index.json");
        for bytes in [Some(b"[]".as_slice()), None, Some(b"corrupt".as_slice())] {
            if let Some(bytes) = bytes {
                std::fs::write(&index, bytes).unwrap();
            } else {
                std::fs::remove_file(&index).unwrap();
            }
            records.lock().unwrap().clear();
            ctx.msg_ack("controller", &message.id, Some("no recipient"))
                .await
                .unwrap();
            let records = records.lock().unwrap();
            assert_eq!(records.len(), 1, "index repair must be declared");
            assert!(!records[0].store_set_uncertain);
            assert!(
                records[0]
                    .changes
                    .iter()
                    .all(|c| c.store == CommitStore::Messages)
            );
        }
    }

    #[tokio::test]
    async fn failed_raw_rejection_still_declares_messages() {
        use orch_engine::container::commit::CommitChange;
        let (_temp, ctx) = metadata_context().await;
        let message = queued_message("partial_rejection");
        ctx.engine.messages.save(&message).await.unwrap();
        let index = ctx.engine.paths.messages_dir().join("_index.json");
        std::fs::remove_file(&index).unwrap();
        std::fs::create_dir(&index).unwrap();
        let records = capture_commits(&ctx.engine.events);
        let boundary = ctx.engine.operation_boundary();
        let seq = boundary.current_commit_seq();
        ctx.msg_ack("controller", &message.id, Some("no recipient"))
            .await
            .unwrap();
        assert!(index.is_dir(), "index update really failed");
        let rejected = ctx.engine.messages.get(&message.id).await.unwrap().unwrap();
        assert_eq!(rejected.status, MessageStatus::Delivered);
        assert!(rejected.subject.starts_with("[rejected] "));
        assert!(rejected.body.starts_with("[rejected by controller: "));
        assert_eq!(boundary.current_commit_seq(), seq + 1);
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].source, CommitSource::Peer);
        assert!(!records[0].store_set_uncertain);
        assert_eq!(
            records[0].changes,
            vec![CommitChange::store_wide(CommitStore::Messages)]
        );
    }

    #[tokio::test]
    async fn real_expiry_sweep_invalidates_once_and_zero_sweep_is_silent() {
        let (_temp, ctx) = metadata_context().await;
        let mut message = queued_message("msg_expired");
        message.expires_at = Some("2000-01-01T00:00:00.000Z".into());
        ctx.engine.messages.save(&message).await.unwrap();
        let records = capture_commits(&ctx.engine.events);
        let boundary = ctx.engine.events.operation_boundary();
        let mut commits = boundary.subscribe_commits();
        let stop = Shutdown::default();
        let cleanup = tokio::spawn(expire_messages(
            ctx.engine.messages.clone(),
            ctx.engine.events.clone(),
            stop.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(5), commits.recv())
            .await
            .unwrap()
            .unwrap();
        stop.0.send_replace(true);
        cleanup.await.unwrap();
        assert!(
            ctx.engine
                .messages
                .get(&message.id)
                .await
                .unwrap()
                .is_none()
        );
        {
            let records = records.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].source, CommitSource::Expiry);
            assert_eq!(
                records[0].changes,
                vec![orch_engine::container::commit::CommitChange::store_wide(
                    CommitStore::Messages
                )]
            );
            assert!(!records[0].store_set_uncertain);
        }
        let before = boundary.current_commit_seq();
        let stop = Shutdown::default();
        let cleanup = tokio::spawn(expire_messages(
            ctx.engine.messages.clone(),
            ctx.engine.events.clone(),
            stop.clone(),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), commits.recv())
                .await
                .is_err(),
            "empty startup sweep committed"
        );
        stop.0.send_replace(true);
        cleanup.await.unwrap();
        assert_eq!(boundary.current_commit_seq(), before);
    }

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
        let cleanup = AbortOnDrop(tokio::spawn(expire_messages(
            store.clone(),
            events,
            Shutdown::default(),
        )));
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

    /// The standalone sweep waits for the permit; a real expired record is
    /// removed and committed only after the holder releases it.
    #[tokio::test]
    async fn expiry_waits_for_a_held_operation_and_commits_after_it() {
        let (_temp, ctx) = metadata_context().await;
        let mut message = queued_message("msg_held_expired");
        message.expires_at = Some("2000-01-01T00:00:00.000Z".into());
        ctx.engine.messages.save(&message).await.unwrap();
        let boundary = ctx.engine.events.operation_boundary();
        let records = capture_commits(&ctx.engine.events);
        let seq = boundary.current_commit_seq();
        let mut commits = boundary.subscribe_commits();
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
        let stop = Shutdown::default();
        let cleanup = tokio::spawn(expire_messages(
            ctx.engine.messages.clone(),
            ctx.engine.events.clone(),
            stop.clone(),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), commits.recv())
                .await
                .is_err(),
            "purged under held permit"
        );
        assert!(
            ctx.engine
                .messages
                .get(&message.id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(boundary.current_commit_seq(), seq);
        release.notify_one();
        held.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), commits.recv())
            .await
            .unwrap()
            .unwrap();
        stop.0.send_replace(true);
        cleanup.await.unwrap();
        assert!(
            ctx.engine
                .messages
                .get(&message.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(boundary.current_commit_seq(), seq + 1);
        assert_eq!(records.lock().unwrap()[0].source, CommitSource::Expiry);
    }
}
