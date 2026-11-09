//! The in-process daemon harness (design D12, harness-design §1). Behind the
//! `testing` feature: a dev-dependency of `orch-cli` and this crate's tests,
//! never compiled into a release binary.
//!
//! The harness is the real daemon front end mounted on a byte stream. By
//! default each [`DaemonHarness::connect`] hands out a `tokio::io::duplex`
//! pair whose server half runs the production session code (handshake,
//! framing, validation, dispatch). [`HarnessBuilder::on_socket`] binds a real
//! Unix listener at a short path for the transport, credential and lifecycle
//! tests that need one. Seams map to existing engine injections.
//!
//! Hermetic by construction: the project is a fresh temp dir (or the
//! caller's), `global.yml` lives in a harness-owned temp dir unless the test
//! names one, and a bound socket sits at a short path under `/tmp`. Nothing
//! here reads `~/.orchestry` or dials a daemon the harness did not start.

use crate::DaemonError;
use crate::lifecycle::{BringUp, DaemonExit, DispatchMode, Running, internal_context};
use crate::ops::ServiceHandler;
use crate::session::{Peer, Server};
use orch_core::{Clock, IdGenerator, ProcessManager};
use orch_proto::ops::Request;
use orch_proto::ops::args::DispatchTickArgs;
use orch_proto::ops::results::TickResult;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;

/// A client's byte stream to a harness daemon: a duplex half, or a connected
/// Unix stream when the harness runs [`HarnessBuilder::on_socket`]. The
/// handshake has **not** been sent; the client (or test) speaks first.
pub struct ClientEndpoint {
    stream: Box<dyn ClientStream>,
}

/// Any bidirectional async byte stream a client can use.
pub trait ClientStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> ClientStream for T {}

impl ClientEndpoint {
    pub fn into_stream(self) -> Box<dyn ClientStream> {
        self.stream
    }
}

/// Seams for a harness daemon.
#[derive(Default)]
pub struct HarnessBuilder {
    clock: Option<Arc<dyn Clock>>,
    maintenance_timer: Option<bool>,
    ids: Option<Arc<dyn IdGenerator>>,
    processes: Option<Arc<dyn ProcessManager>>,
    global_config_path: Option<PathBuf>,
    dispatch: DispatchMode,
    versions: Option<(u32, String)>,
    on_socket: bool,
    listen: Option<String>,
    peer_name: Option<String>,
}

/// The duplex buffer per direction: comfortably above one 1 MiB frame so a
/// test can write a whole maximal frame before the daemon reads it.
const DUPLEX_BYTES: usize = 2 * 1024 * 1024;

impl HarnessBuilder {
    /// Inject the clock (context expiry, retry timing).
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Enable periodic context expiry and transfer sweeps (enabled by default).
    /// Disable with an injected clock to drive expiry explicitly via
    /// [`DaemonHarness::step_expiry`] or `maintenance.expire_context`.
    pub fn maintenance_timer(mut self, enabled: bool) -> Self {
        self.maintenance_timer = Some(enabled);
        self
    }

    /// Inject the id generator (deterministic output in snapshot tests).
    pub fn ids(mut self, ids: Arc<dyn IdGenerator>) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Inject the process manager (scripted agents for dispatch/cancel).
    pub fn processes(mut self, processes: Arc<dyn ProcessManager>) -> Self {
        self.processes = Some(processes);
        self
    }

    /// Where `global.yml` lives; tests must never use the real one.
    pub fn global_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.global_config_path = Some(path.into());
        self
    }

    /// Initial dispatch state; defaults to off, as the real daemon.
    pub fn dispatch(mut self, dispatch: DispatchMode) -> Self {
        self.dispatch = dispatch;
        self
    }

    /// Advertise these versions in the welcome instead of this build's, for
    /// the skew matrix. Affects the handshake only.
    pub fn versions(mut self, protocol: u32, binary: impl Into<String>) -> Self {
        self.versions = Some((protocol, binary.into()));
        self
    }

    /// Start the peer runtime listening on `addr` (`--listen`), e.g.
    /// `127.0.0.1:0`.
    pub fn listen(mut self, addr: impl Into<String>) -> Self {
        self.listen = Some(addr.into());
        self
    }

    /// The peer name (`--peer-name`).
    pub fn peer_name(mut self, name: impl Into<String>) -> Self {
        self.peer_name = Some(name.into());
        self
    }

    /// Also bind a real Unix listener at a short path (≤100 bytes) under a
    /// fresh 0700 temp dir.
    pub fn on_socket(mut self) -> Self {
        self.on_socket = true;
        self
    }

    /// Start on a fresh temp project (the `orch init` equivalent: an empty
    /// `.orchestry/`, which is all the engine requires).
    pub async fn start(self) -> Result<DaemonHarness, DaemonError> {
        let project = tempfile::Builder::new()
            .prefix("orch-harness-")
            .tempdir()
            .map_err(|source| crate::discovery::io_error("create the harness project", source))?;
        let root = project
            .path()
            .canonicalize()
            .map_err(|source| crate::discovery::io_error("resolve the harness project", source))?;
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(".orchestry"))
                .map_err(|source| crate::discovery::io_error("create .orchestry", source))?;
        }
        self.launch(root, Some(project)).await
    }

    /// Start on a project the test pre-seeded.
    pub async fn start_in(self, project_root: &Path) -> Result<DaemonHarness, DaemonError> {
        let root = project_root
            .canonicalize()
            .map_err(|_| DaemonError::NotInitialized)?;
        self.launch(root, None).await
    }

    async fn launch(
        self,
        root: PathBuf,
        project: Option<tempfile::TempDir>,
    ) -> Result<DaemonHarness, DaemonError> {
        // Short, private and harness-owned: the socket (when bound) and the
        // default global.yml.
        let scratch = tempfile::Builder::new()
            .prefix("od-")
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir_in("/tmp")
            .map_err(|source| {
                crate::discovery::io_error("create the harness scratch dir", source)
            })?;
        let socket = self.on_socket.then(|| scratch.path().join("s"));
        let defaults = orch_engine::container::EngineSeams::default();
        let seams = orch_engine::container::EngineSeams {
            clock: self.clock.unwrap_or(defaults.clock),
            ids: self.ids.unwrap_or(defaults.ids),
            global_config_path: Some(
                self.global_config_path
                    .unwrap_or_else(|| scratch.path().join("global.yml")),
            ),
        };
        let running = Running::bring_up_with_maintenance_timer(
            BringUp {
                project_root: root.clone(),
                socket: socket.clone(),
                dispatch: self.dispatch,
                listen: self.listen,
                peer_name: self.peer_name,
                seams,
                processes: self.processes,
                versions: self.versions,
                snapshot: crate::env::capture(std::env::vars_os()),
            },
            self.maintenance_timer.unwrap_or(true),
        )
        .await?;
        Ok(DaemonHarness {
            project_root: root,
            socket,
            engine: Arc::clone(&running.engine),
            handler: running.handler.clone(),
            server: Arc::clone(&running.server),
            lifecycle: Some(Lifecycle::spawn(running)),
            _scratch: scratch,
            project,
        })
    }
}

/// The harness's stand-in for [`crate::start`]'s wait: one task owns the
/// daemon's stop, so a frozen stop intent, `daemon.stop` and
/// [`DaemonHarness::shutdown`] all take the one clean path
/// ([`Running::shutdown`]) exactly once, as in production.
struct Lifecycle {
    /// The daemon until a stop trigger takes it; `Drop` aborts what is left.
    running: Arc<Mutex<Option<Running>>>,
    shutdown: watch::Sender<bool>,
    exit: watch::Receiver<Option<DaemonExit>>,
    task: tokio::task::JoinHandle<()>,
}

impl Lifecycle {
    fn spawn(running: Running) -> Lifecycle {
        let mut requested = running.server.stop.requested();
        let mut stopped = running.handler.stopped();
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let (exit_tx, exit) = watch::channel(None);
        let running = Arc::new(Mutex::new(Some(running)));
        let slot = Arc::clone(&running);
        let task = tokio::spawn(async move {
            let reason = tokio::select! {
                () = crate::session::until_set(&mut requested) => "stop intent",
                _ = stopped.wait_for(Option::is_some) => "daemon.stop",
                () = crate::session::until_set(&mut shutdown_rx) => "harness shutdown",
            };
            let taken = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(running) = taken {
                tracing::info!(reason, "stopping");
                exit_tx.send_replace(Some(running.shutdown().await));
            }
        });
        Lifecycle {
            running,
            shutdown,
            exit,
            task,
        }
    }

    /// Whether a stop trigger has begun (or finished) the teardown.
    fn stopping(&self) -> bool {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    }

    /// Trigger the clean stop unless one already ran; idempotent.
    async fn finish(mut self) -> Option<DaemonExit> {
        self.shutdown.send_replace(true);
        match self.exit.wait_for(Option::is_some).await {
            Ok(exit) => *exit,
            // The task ended without an exit: it panicked mid-shutdown.
            Err(_) => None,
        }
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.task.abort();
        let taken = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut running) = taken {
            running.abort();
        }
    }
}

/// A running in-process daemon. Drop without [`DaemonHarness::shutdown`]
/// aborts it; tests that check cleanup call `shutdown`.
pub struct DaemonHarness {
    project_root: PathBuf,
    socket: Option<PathBuf>,
    engine: Arc<orch_engine::FullEngine>,
    handler: ServiceHandler,
    server: Arc<Server>,
    /// `None` once [`DaemonHarness::shutdown`] consumed it.
    lifecycle: Option<Lifecycle>,
    _scratch: tempfile::TempDir,
    project: Option<tempfile::TempDir>,
}

/// What one [`DaemonHarness::step_dispatch`] tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Task ids launched by this tick.
    pub launched: Vec<String>,
}

/// A project after [`DaemonHarness::quiesce`]: the daemon has stopped
/// cleanly and released ownership, so a test may open the files offline.
pub struct QuiescedProject {
    project_root: PathBuf,
    _project: Option<tempfile::TempDir>,
}

impl QuiescedProject {
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}

impl DaemonHarness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    /// Fresh temp project on the duplex transport with default seams.
    pub async fn start() -> Result<DaemonHarness, DaemonError> {
        HarnessBuilder::default().start().await
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The bound socket when started [`HarnessBuilder::on_socket`].
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    /// The daemon's handler while it serves; an error once a stop began.
    fn running(&self) -> Result<&ServiceHandler, DaemonError> {
        match &self.lifecycle {
            Some(lifecycle) if !lifecycle.stopping() => Ok(&self.handler),
            _ => Err(DaemonError::InvalidArguments(
                "the harness daemon has stopped".into(),
            )),
        }
    }

    /// A new client connection (duplex, or the real socket when bound). The
    /// duplex peer is this process, as the kernel would report it.
    pub async fn connect(&self) -> Result<ClientEndpoint, DaemonError> {
        self.running()?;
        if let Some(socket) = &self.socket {
            let stream = tokio::net::UnixStream::connect(socket)
                .await
                .map_err(|source| {
                    crate::discovery::io_error("connect to the harness socket", source)
                })?;
            return Ok(ClientEndpoint {
                stream: Box::new(stream),
            });
        }
        let (client, server) = tokio::io::duplex(DUPLEX_BYTES);
        self.server.admit(
            server,
            Peer {
                uid: Some(nix::unistd::geteuid().as_raw()),
                pid: Some(std::process::id()),
            },
        );
        Ok(ClientEndpoint {
            stream: Box::new(client),
        })
    }

    /// The engine, for seeding and peeking from **test code only**.
    pub fn engine(&self) -> &orch_engine::FullEngine {
        &self.engine
    }

    /// Run one coordinated context-expiry step against the injected clock;
    /// returns the number of keys removed.
    pub async fn step_expiry(&self) -> Result<usize, DaemonError> {
        let removed = self
            .running()?
            .expire_context()
            .await
            .map_err(|error| DaemonError::Operation(Box::new(error)))?;
        Ok(usize::try_from(removed).unwrap_or(usize::MAX))
    }

    /// Run one coordinated scheduling tick regardless of the dispatch switch.
    pub async fn step_dispatch(&self) -> Result<TickReport, DaemonError> {
        let committed = self
            .running()?
            .execute_request(
                &internal_context(),
                Request::DispatchTick(DispatchTickArgs {}),
            )
            .await
            .map_err(|error| DaemonError::Operation(Box::new(error)))?;
        let result: TickResult = serde_json::from_str(committed.value.get()).map_err(|_| {
            DaemonError::InvalidArguments("dispatch.tick returned an unexpected shape".into())
        })?;
        Ok(TickReport {
            launched: result.launched,
        })
    }

    /// The same clean path as SIGTERM: stop admission and dispatch, kill and
    /// reconcile agents, remove own endpoint/discovery, release ownership.
    /// After a stop intent or `daemon.stop` already ran that path, this
    /// reports its outcome instead of stopping twice.
    pub async fn shutdown(mut self) -> Result<(), DaemonError> {
        let Some(lifecycle) = self.lifecycle.take() else {
            return Ok(());
        };
        match lifecycle.finish().await {
            Some(exit) if exit.clean => Ok(()),
            _ => Err(DaemonError::StopIncomplete),
        }
    }

    /// [`DaemonHarness::shutdown`], then hand back the project for offline
    /// inspection (entity files vs. index) with no daemon running.
    pub async fn quiesce(mut self) -> Result<QuiescedProject, DaemonError> {
        let project = self.project.take();
        let project_root = self.project_root.clone();
        self.shutdown().await?;
        Ok(QuiescedProject {
            project_root,
            _project: project,
        })
    }
}

/// Dropping without [`DaemonHarness::shutdown`] aborts the daemon via
/// [`Lifecycle`]'s `Drop`; a stop already tearing down is cancelled with it.
impl Drop for DaemonHarness {
    fn drop(&mut self) {
        drop(self.lifecycle.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_proto::handshake::{
        Caller, CallerKind, HandshakeReply, Hello, HelloFrame, SessionMode, StopState,
    };
    use orch_proto::path::WirePath;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    async fn roundtrip(harness: &DaemonHarness) -> serde_json::Value {
        let stream = harness.connect().await.unwrap().into_stream();
        let mut stream = BufReader::new(stream);
        let (project_id, _) = crate::discovery::project_identity(harness.project_root()).unwrap();
        let hello = HelloFrame {
            hello: Hello::session(
                WirePath::new(harness.project_root()).unwrap(),
                project_id,
                &Caller {
                    kind: CallerKind::Cli,
                    agent_id: None,
                    pid: None,
                },
                SessionMode::Normal,
            ),
        };
        let mut frame = serde_json::to_vec(&hello).unwrap();
        frame.extend_from_slice(b"\n{\"id\":1,\"req\":{\"op\":\"daemon.status\",\"args\":{}}}\n");
        stream.get_mut().write_all(&frame).await.unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let reply: HandshakeReply = serde_json::from_str(&line).unwrap();
        assert!(reply.welcome.unwrap().compatible);
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplex_harness_serves_the_production_session_and_shuts_down_cleanly() {
        let harness = DaemonHarness::start().await.unwrap();
        assert!(harness.socket().is_none());
        let status = roundtrip(&harness).await;
        assert_eq!(status["id"], 1);
        assert_eq!(status["ok"]["dispatch"]["dispatch_on"], false);
        assert_eq!(
            harness.step_dispatch().await.unwrap(),
            TickReport::default()
        );
        assert_eq!(harness.step_expiry().await.unwrap(), 0);
        let root = harness.project_root().to_path_buf();
        let quiesced = harness.quiesce().await.unwrap();
        assert_eq!(quiesced.project_root(), root);
        assert!(!root.join(".orchestry/orchestry.lock").exists());
        // Offline inspection works once the daemon is gone.
        orch_engine::container::build_light(quiesced.project_root())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn socket_harness_binds_a_short_private_path_and_publishes_discovery() {
        let harness = DaemonHarness::builder().on_socket().start().await.unwrap();
        let socket = harness.socket().unwrap().to_path_buf();
        assert!(socket.as_os_str().len() <= 100);
        let record = crate::discovery::read_record(harness.project_root())
            .unwrap()
            .unwrap();
        assert_eq!(record.socket.as_path(), socket);
        assert_eq!(roundtrip(&harness).await["id"], 1);
        harness.shutdown().await.unwrap();
        assert!(!socket.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropped_harness_releases_ownership_for_a_successor() {
        let mut first = DaemonHarness::start().await.unwrap();
        let root = first.project_root().to_path_buf();
        // A second owner of the same root loses the guard while the first runs.
        let error = DaemonHarness::builder()
            .start_in(&root)
            .await
            .err()
            .expect("one owner per project");
        assert!(matches!(error, DaemonError::AlreadyRunning(_)), "{error:?}");
        // Keep the temp project alive past the drop.
        let held = first.project.take();
        drop(first);
        assert!(!root.join(".orchestry/orchestry.lock").exists());
        let second = DaemonHarness::builder().start_in(&root).await.unwrap();
        second.shutdown().await.unwrap();
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn harness_versions_only_change_the_handshake() {
        let harness = DaemonHarness::builder()
            .versions(7, "7.0.0")
            .start()
            .await
            .unwrap();
        let stream = harness.connect().await.unwrap().into_stream();
        let mut stream = BufReader::new(stream);
        let (project_id, _) = crate::discovery::project_identity(harness.project_root()).unwrap();
        let hello = HelloFrame {
            hello: Hello::stop(WirePath::new(harness.project_root()).unwrap(), project_id),
        };
        let mut frame = serde_json::to_vec(&hello).unwrap();
        frame.push(b'\n');
        // A stop intent is answered with the advertised versions, then
        // stops the engine; the harness's shutdown stays idempotent.
        stream.get_mut().write_all(&frame).await.unwrap();
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let welcome = serde_json::from_str::<HandshakeReply>(&line)
            .unwrap()
            .welcome
            .unwrap();
        assert_eq!((welcome.protocol, welcome.binary.as_str()), (7, "7.0.0"));
        harness.shutdown().await.unwrap();
    }

    async fn read_reply(stream: &mut BufReader<Box<dyn ClientStream>>) -> HandshakeReply {
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            stream.read_line(&mut line),
        )
        .await
        .expect("the daemon answers within the deadline")
        .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// The live `orch daemon stop` against a harness socket: the stop intent
    /// completes on its own (no harness `shutdown` running) with the full
    /// production teardown, and the later `shutdown` is a clean no-op.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn frozen_stop_intent_tears_the_harness_down_and_shutdown_is_idempotent() {
        let mut harness = DaemonHarness::builder().on_socket().start().await.unwrap();
        let root = harness.project_root().to_path_buf();
        let socket = harness.socket().unwrap().to_path_buf();
        let stream = harness.connect().await.unwrap().into_stream();
        let mut stream = BufReader::new(stream);
        let (project_id, _) = crate::discovery::project_identity(&root).unwrap();
        let hello = HelloFrame {
            hello: Hello::stop(WirePath::new(&root).unwrap(), project_id),
        };
        let mut frame = serde_json::to_vec(&hello).unwrap();
        frame.push(b'\n');
        stream.get_mut().write_all(&frame).await.unwrap();
        let accepted = read_reply(&mut stream).await.welcome.unwrap();
        assert_eq!(accepted.stop_state, Some(StopState::Accepted));
        let completed = read_reply(&mut stream).await.welcome.unwrap();
        assert_eq!(completed.stop_state, Some(StopState::Completed));
        assert_eq!(completed.instance, accepted.instance);

        // Torn down before `shutdown` is called, as `start` would.
        let state = root.join(".orchestry");
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while state.join("orchestry.lock").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("ownership is released after the stop");
        assert!(!socket.exists());
        assert!(!state.join("daemon.json").exists());
        assert!(harness.connect().await.is_err());
        assert!(harness.step_dispatch().await.is_err());

        let held = harness.project.take();
        harness.shutdown().await.unwrap();
        let successor = DaemonHarness::builder().start_in(&root).await.unwrap();
        successor.shutdown().await.unwrap();
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_stop_request_tears_the_harness_down() {
        let harness = DaemonHarness::start().await.unwrap();
        let root = harness.project_root().to_path_buf();
        let stream = harness.connect().await.unwrap().into_stream();
        let mut stream = BufReader::new(stream);
        let (project_id, _) = crate::discovery::project_identity(&root).unwrap();
        let hello = HelloFrame {
            hello: Hello::session(
                WirePath::new(&root).unwrap(),
                project_id,
                &Caller {
                    kind: CallerKind::Cli,
                    agent_id: None,
                    pid: None,
                },
                SessionMode::Normal,
            ),
        };
        let mut frame = serde_json::to_vec(&hello).unwrap();
        frame.extend_from_slice(b"\n{\"id\":1,\"req\":{\"op\":\"daemon.stop\",\"args\":{}}}\n");
        stream.get_mut().write_all(&frame).await.unwrap();
        assert!(read_reply(&mut stream).await.welcome.unwrap().compatible);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["ok"]["clean"], true, "{reply}");

        let lock = root.join(".orchestry/orchestry.lock");
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while lock.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon.stop runs the full teardown");
        assert!(harness.connect().await.is_err());
        harness.shutdown().await.unwrap();
    }
}
