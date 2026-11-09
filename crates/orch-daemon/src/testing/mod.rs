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
//! Bodies are owned by S1a-impl (senior-eng); test vectors by T1.

use crate::DaemonError;
use crate::lifecycle::DispatchMode;
use orch_core::{Clock, IdGenerator, ProcessManager};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

const OWNER: &str = "S1a-impl";

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
}

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

    /// Also bind a real Unix listener at a short path (≤100 bytes) under a
    /// fresh 0700 temp dir.
    pub fn on_socket(mut self) -> Self {
        self.on_socket = true;
        self
    }

    /// Start on a fresh temp project (the `orch init` equivalent).
    pub async fn start(self) -> Result<DaemonHarness, DaemonError> {
        let _ = self;
        Err(DaemonError::unimplemented("HarnessBuilder::start", OWNER))
    }

    /// Start on a project the test pre-seeded.
    pub async fn start_in(self, project_root: &Path) -> Result<DaemonHarness, DaemonError> {
        let _ = (self, project_root);
        Err(DaemonError::unimplemented(
            "HarnessBuilder::start_in",
            OWNER,
        ))
    }
}

/// A running in-process daemon. Drop without [`DaemonHarness::shutdown`]
/// aborts it; tests that check cleanup call `shutdown`.
pub struct DaemonHarness {
    project_root: PathBuf,
    socket: Option<PathBuf>,
    engine: Arc<orch_engine::FullEngine>,
}

/// What one [`DaemonHarness::step_dispatch`] tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Task ids launched by this tick.
    pub launched: Vec<String>,
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

    /// A new client connection (duplex, or the real socket when bound).
    pub async fn connect(&self) -> Result<ClientEndpoint, DaemonError> {
        Err(DaemonError::unimplemented("DaemonHarness::connect", OWNER))
    }

    /// The engine, for seeding and peeking from **test code only**.
    pub fn engine(&self) -> &orch_engine::FullEngine {
        &self.engine
    }

    /// Run one coordinated context-expiry step against the injected clock;
    /// returns the number of keys removed.
    pub async fn step_expiry(&self) -> Result<usize, DaemonError> {
        Err(DaemonError::unimplemented(
            "DaemonHarness::step_expiry",
            OWNER,
        ))
    }

    /// Run one coordinated scheduling tick regardless of the dispatch switch.
    pub async fn step_dispatch(&self) -> Result<TickReport, DaemonError> {
        Err(DaemonError::unimplemented(
            "DaemonHarness::step_dispatch",
            OWNER,
        ))
    }

    /// The same clean path as SIGTERM: stop admission and dispatch, kill and
    /// reconcile agents, remove own endpoint/discovery, release ownership.
    pub async fn shutdown(self) -> Result<(), DaemonError> {
        let _ = self;
        Err(DaemonError::unimplemented("DaemonHarness::shutdown", OWNER))
    }
}
