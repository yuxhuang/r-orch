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
use crate::lifecycle::{BringUp, DispatchMode, Running, internal_context};
use crate::session::Peer;
use orch_core::{Clock, IdGenerator, ProcessManager};
use orch_proto::ops::Request;
use orch_proto::ops::args::DispatchTickArgs;
use orch_proto::ops::results::TickResult;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

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
        let running = Running::bring_up(BringUp {
            project_root: root.clone(),
            socket: socket.clone(),
            dispatch: self.dispatch,
            listen: self.listen,
            peer_name: self.peer_name,
            seams,
            processes: self.processes,
            versions: self.versions,
            snapshot: crate::env::capture(std::env::vars_os()),
        })
        .await?;
        Ok(DaemonHarness {
            project_root: root,
            socket,
            engine: Arc::clone(&running.engine),
            running: Some(running),
            _scratch: scratch,
            project,
        })
    }
}

/// A running in-process daemon. Drop without [`DaemonHarness::shutdown`]
/// aborts it; tests that check cleanup call `shutdown`.
pub struct DaemonHarness {
    project_root: PathBuf,
    socket: Option<PathBuf>,
    engine: Arc<orch_engine::FullEngine>,
    running: Option<Running>,
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

    fn running(&self) -> Result<&Running, DaemonError> {
        self.running
            .as_ref()
            .ok_or_else(|| DaemonError::InvalidArguments("the harness daemon has stopped".into()))
    }

    /// A new client connection (duplex, or the real socket when bound). The
    /// duplex peer is this process, as the kernel would report it.
    pub async fn connect(&self) -> Result<ClientEndpoint, DaemonError> {
        let running = self.running()?;
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
        running.server.admit(
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
            .handler
            .expire_context()
            .await
            .map_err(|error| DaemonError::Operation(Box::new(error)))?;
        Ok(usize::try_from(removed).unwrap_or(usize::MAX))
    }

    /// Run one coordinated scheduling tick regardless of the dispatch switch.
    pub async fn step_dispatch(&self) -> Result<TickReport, DaemonError> {
        let committed = self
            .running()?
            .handler
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
    pub async fn shutdown(mut self) -> Result<(), DaemonError> {
        let Some(running) = self.running.take() else {
            return Ok(());
        };
        if running.shutdown().await.clean {
            Ok(())
        } else {
            Err(DaemonError::StopIncomplete)
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

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            running.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_proto::handshake::{
        Caller, CallerKind, HandshakeReply, Hello, HelloFrame, SessionMode,
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
        // A stop intent is answered with the advertised versions; the
        // harness is not stopped until its own shutdown runs.
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
}
