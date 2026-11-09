//! Explicit owner start and bootstrap initialization (design D3, D8; spec
//! R4/R5/R14). Nothing a client runs may call into this module: only
//! `orch daemon start`, `serve`/`run --watch` (PR 2) and `orch init` do.
//!
//! ```text
//! explicit start → agent-context / developmental gates → root, euid, endpoint
//!  → owner guard → legacy PID lock → engine + recovery + index rebuild
//!  → context expiry → private run/ dir → stale-socket check → bind (0600)
//!  → runtime .gitignore entries → daemon.json → `listening <socket>`
//!  → READY (dispatch OFF unless --dispatch on)
//! stop intent / daemon.stop / TERM / INT / HUP → STOPPING: admission and
//!  dispatch off, agents killed with grace and reconciled → sessions answer
//!  and close → own socket and daemon.json removed → PID lock → guard LAST
//! ```

use crate::DaemonError;
use crate::coordinator::ProjectCoordinator;
use crate::discovery::{self, SocketState};
use crate::guard::{OwnerGuard, PID_REFRESH, PidLock, PidLockState};
use crate::ops::{OpContext, ServiceHandler};
use crate::session::{Server, ServerConfig};
use orch_core::ProcessManager;
use orch_engine::container::{EngineSeams, FullEngine};
use orch_proto::handshake::{Caller, CallerKind, ProjectId, Session, SessionMode};
use orch_proto::ops::Request;
use orch_proto::ops::args::{DaemonStopArgs, DispatchSetArgs, DispatchSwitch};
use orch_proto::ops::results::{DaemonStatus, DispatchState, OwnershipStatus, StopOutcome};
use orch_proto::path::WirePath;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Whether automatic dispatch runs after READY. A fresh daemon defaults to
/// `Off` so the owner can queue work first (R5); `serve`/`run --watch` pass
/// `On`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DispatchMode {
    #[default]
    Off,
    On,
}

/// Options of an explicit daemon start.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// The owning project root, already selected by the caller per D4
    /// (`ORCH_PROJECT_ROOT` over the cwd ancestor walk).
    pub project_root: PathBuf,
    /// An explicit endpoint (`ORCH_SOCKET`). `None` uses the default
    /// `.orchestry/run/orch.sock`, or the `$TMPDIR/orch-<hash>/orch.sock`
    /// fallback when that exceeds 100 bytes.
    pub socket: Option<PathBuf>,
    /// `--detach`: new session via setsid, stdin `/dev/null`, output to the
    /// private capped `.orchestry/daemon.log`; the parent returns only after
    /// the child is READY, and a child failure is the parent's failure.
    pub detach: bool,
    /// `--dispatch on|off`.
    pub dispatch: DispatchMode,
    /// `--from-agent`: the owner deliberately starts from an agent context
    /// (`ORCH_TASK_ID`/`ORCH_AGENT_ID` set). Without it such a start exits 2.
    pub from_agent: bool,
    /// `--listen <addr>`: accept remote peer connections on this address.
    /// A busy address fails the start, as it fails `serve` (R19).
    pub listen: Option<String>,
    /// `--peer-name <name>`: this instance's peer name, overriding
    /// `peer.name` in config.yml for this run only.
    pub peer_name: Option<String>,
    /// PR 1 developmental gate (SE-20): the caller read
    /// `ORCH_DAEMON_DEVELOPMENTAL=1`. Without it start refuses with exit 2;
    /// with it start prints the banner that legacy writers are not excluded.
    pub developmental: bool,
}

/// How a daemon run ended. `code` is the process exit status for
/// `orch daemon start`: 0 after a clean stop, otherwise the failure's code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonExit {
    pub code: i32,
    /// `false` when shutdown cleanup was incomplete (R14): the evidence is
    /// kept and the stop is not reported as clean.
    pub clean: bool,
}

impl DaemonExit {
    pub const CLEAN: DaemonExit = DaemonExit {
        code: 0,
        clean: true,
    };
}

/// The banner every developmental start prints on stderr (SE-20).
pub const DEVELOPMENTAL_BANNER: &str = "orch daemon: developmental build. The CLI, TUI, web \
and MCP still write .orchestry/ directly and are NOT excluded by this daemon; use it only on \
development or test projects.";

/// How long shutdown waits for sessions to deliver terminal frames.
const SESSION_DRAIN: Duration = Duration::from_secs(10);
/// How long a stop already started by `daemon.stop` may take to report.
const STOP_WAIT: Duration = Duration::from_secs(300);

/// Run an explicitly started daemon until stop, SIGTERM, SIGINT or SIGHUP:
/// [`prepare_start`] then [`PreparedStart::run`].
///
/// In the foreground this prints exactly `listening <socket>` once recovery,
/// binding and discovery publication are done, then the dispatch state. With
/// `detach` it returns as soon as the detached child reports READY.
///
/// Fails with [`DaemonError::AlreadyRunning`] (exit 4) when another process
/// owns the project and [`DaemonError::RefusedInAgentContext`] (exit 2) from
/// an agent context without `from_agent`. Never called from a client path.
///
/// Called from inside a runtime, the environment snapshot is taken after
/// the runtime exists; `main` should call [`prepare_start`] first (D11).
pub async fn start(options: StartOptions) -> Result<DaemonExit, DaemonError> {
    prepare_start(options)?.run().await
}

/// The synchronous half of an explicit start (D11): call it before building
/// the tokio runtime. It captures the sanitized agent environment, enforces
/// the agent-context and developmental gates, prints the banner and resolves
/// the root, its ownership and the endpoint. The process environment is
/// read, never written. Nothing is locked, bound or spawned until
/// [`PreparedStart::run`].
pub fn prepare_start(options: StartOptions) -> Result<PreparedStart, DaemonError> {
    prepare(options, std::env::vars_os(), |name| std::env::var_os(name))
}

/// A start whose environment and gates were settled by [`prepare_start`].
#[derive(Debug)]
#[must_use = "a prepared start does nothing until run"]
pub struct PreparedStart {
    options: StartOptions,
    root: PathBuf,
    socket: PathBuf,
    snapshot: crate::env::Snapshot,
    /// The detached child's capped log; `None` in any other start.
    log: Option<crate::detach::DetachedLog>,
}

fn prepare(
    options: StartOptions,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<PreparedStart, DaemonError> {
    // R21: the agent environment is decided by what started the daemon, read
    // once and never written.
    let snapshot = crate::env::capture(vars);
    if in_agent_context(&var) && !options.from_agent {
        return Err(DaemonError::RefusedInAgentContext);
    }
    if !options.developmental {
        return Err(DaemonError::DevelopmentalGateClosed);
    }
    let log = if var(crate::detach::DETACHED_CHILD_ENV).is_some() {
        crate::detach::enter_own_session();
        Some(crate::detach::DetachedLog::capture(&options.project_root)?)
    } else {
        None
    };
    {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{DEVELOPMENTAL_BANNER}");
    }

    let root = options
        .project_root
        .canonicalize()
        .map_err(|_| DaemonError::NotInitialized)?;
    let (_, meta) = discovery::project_identity(&root)?;
    discovery::check_state_dir_owner(&root, &meta)?;
    let socket = discovery::endpoint(&root, options.socket.as_deref(), &std::env::temp_dir())?;
    Ok(PreparedStart {
        options,
        root,
        socket,
        snapshot,
        log,
    })
}

impl PreparedStart {
    /// Detach, or bring the daemon up in the foreground and serve until a
    /// stop trigger; see [`start`].
    pub async fn run(mut self) -> Result<DaemonExit, DaemonError> {
        if self.options.detach {
            return crate::detach::spawn_detached(&self.options, &self.root).await;
        }
        // Dropped last, on every path: drains the log before the exit.
        let mut log = self.log.take();
        let socket = self.socket.clone();
        let running = Running::bring_up(self.bring_up()).await?;

        let mut signals = Signals::install()?;
        {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "listening {}", socket.display());
            let _ = writeln!(
                out,
                "dispatch {}",
                dispatch_word(running.handler.dispatch_state())
            );
            let _ = out.flush();
        }
        if let Some(log) = log.as_mut() {
            log.release_readiness_pipe();
        }

        let reason = running.wait_for_stop(Some(&mut signals)).await;
        tracing::info!(reason, "stopping");
        let exit = running.shutdown().await;
        drop(log);
        Ok(exit)
    }

    fn bring_up(self) -> BringUp {
        BringUp {
            project_root: self.root,
            socket: Some(self.socket),
            dispatch: self.options.dispatch,
            listen: self.options.listen,
            peer_name: self.options.peer_name,
            seams: EngineSeams::default(),
            processes: None,
            versions: None,
            snapshot: self.snapshot,
        }
    }
}

/// R31: `ORCH_TASK_ID` or `ORCH_AGENT_ID` set (non-empty) is an agent
/// context, where an explicit start needs `--from-agent`.
fn in_agent_context(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["ORCH_TASK_ID", "ORCH_AGENT_ID"]
        .iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

async fn until_stopped(stopped: &mut watch::Receiver<Option<bool>>) {
    let _ = stopped.wait_for(Option::is_some).await;
}

fn dispatch_word(state: DispatchState) -> &'static str {
    if state.dispatch_on { "on" } else { "off" }
}

/// TERM, INT and HUP all mean a clean stop (D3: foreground SIGHUP included).
pub(crate) struct Signals {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

impl Signals {
    fn install() -> Result<Signals, DaemonError> {
        use tokio::signal::unix::{SignalKind, signal};
        let install = |kind| {
            signal(kind).map_err(|source| discovery::io_error("install signal handlers", source))
        };
        Ok(Signals {
            term: install(SignalKind::terminate())?,
            int: install(SignalKind::interrupt())?,
            hup: install(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.term.recv() => "SIGTERM",
            _ = self.int.recv() => "SIGINT",
            _ = self.hup.recv() => "SIGHUP",
        }
    }
}

/// What [`Running::bring_up`] needs; production and the harness differ only
/// in these values.
pub(crate) struct BringUp {
    pub(crate) project_root: PathBuf,
    /// `None`: no listener (the harness's duplex transport).
    pub(crate) socket: Option<PathBuf>,
    pub(crate) dispatch: DispatchMode,
    pub(crate) listen: Option<String>,
    pub(crate) peer_name: Option<String>,
    pub(crate) seams: EngineSeams,
    /// `None`: the system process manager over the environment snapshot.
    pub(crate) processes: Option<Arc<dyn ProcessManager>>,
    /// Advertised `(protocol, binary)`; `None` is this build's.
    pub(crate) versions: Option<(u32, String)>,
    pub(crate) snapshot: crate::env::Snapshot,
}

struct Endpoint {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

/// A READY daemon: everything shutdown must undo, in order.
pub(crate) struct Running {
    #[cfg_attr(
        not(feature = "testing"),
        allow(dead_code, reason = "used by the testing harness only")
    )]
    pub(crate) engine: Arc<FullEngine>,
    pub(crate) handler: ServiceHandler,
    pub(crate) server: Arc<Server>,
    project_root: PathBuf,
    instance: String,
    endpoint: Option<Endpoint>,
    quit: watch::Sender<bool>,
    loops: Vec<tokio::task::JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
    pid_lock: Arc<PidLock>,
    guard: Option<OwnerGuard>,
    /// Dropped after the engine stops, as `serve` drops its runtime.
    peer: Option<crate::peer::HostedPeer>,
}

impl Running {
    pub(crate) async fn bring_up(spec: BringUp) -> Result<Running, DaemonError> {
        Self::bring_up_with_maintenance_timer(spec, true).await
    }

    /// The harness may disable periodic expiry and transfer sweeps.
    pub(crate) async fn bring_up_with_maintenance_timer(
        spec: BringUp,
        maintenance_timer: bool,
    ) -> Result<Running, DaemonError> {
        let root = spec.project_root.clone();
        let state_dir = root.join(".orchestry");
        let (project_id, meta) = discovery::project_identity(&root)?;
        discovery::check_state_dir_owner(&root, &meta)?;

        // Ownership first: the kernel lock decides a start race, then the
        // legacy PID file keeps v0.13 orchestrators out.
        let guard = OwnerGuard::acquire(&state_dir).await?;
        let pid_lock = Arc::new(
            PidLock::acquire(orch_infra::storage::paths::Paths::new(&root).lock_path()).await?,
        );
        match Self::build(
            root,
            state_dir,
            project_id,
            spec,
            Arc::clone(&pid_lock),
            maintenance_timer,
        )
        .await
        {
            Ok(mut running) => {
                running.guard = Some(guard);
                Ok(running)
            }
            Err(error) => {
                pid_lock.release();
                drop(guard);
                Err(error)
            }
        }
    }

    async fn build(
        root: PathBuf,
        state_dir: PathBuf,
        project_id: ProjectId,
        spec: BringUp,
        pid_lock: Arc<PidLock>,
        maintenance_timer: bool,
    ) -> Result<Running, DaemonError> {
        let child_env = orch_infra::process::EnvSnapshot {
            base: spec.snapshot.base.clone(),
            forced: crate::env::forced(&root, spec.socket.as_deref()),
        };
        let diagnostics = Arc::new(orch_infra::models::SnapshotDiscoveryRuntime::new(
            child_env.clone(),
        ));
        let command_runner = Arc::new(crate::ops::SnapshotCommandRunner(Arc::clone(&diagnostics)));
        let processes = spec.processes.unwrap_or_else(|| {
            Arc::new(orch_infra::process::SnapshotEnvProcessManager::new(
                child_env,
            ))
        });
        let peer = orch_domain::PeerConfig {
            name: spec.peer_name,
            listen: spec.listen,
        };
        let engine = Arc::new(
            orch_engine::container::build_full_with_children(
                root.clone(),
                spec.seams,
                peer,
                processes,
                command_runner,
            )
            .await?,
        );
        let coordinator = Arc::new(ProjectCoordinator::new(engine.operation_boundary()));
        // Startup recovery (claims, running entries, orphan runs) and the
        // index read/rebuild, before anything is served. Fatal on error.
        engine.daemon_initialize().await?;
        let handler =
            ServiceHandler::with_diagnostics(Arc::clone(&engine), coordinator, diagnostics);
        // Explicit expiry after reconciliation; its failure is reported and
        // never blocks READY (B0d).
        if let Err(error) = handler.expire_context().await {
            tracing::warn!(code = ?error.code, "startup context expiry failed");
        }

        // Before anything is served and before the first link: the runtime
        // re-registers remote runs recovery left registered (R19).
        let peer = Some(crate::peer::HostedPeer::prepare(Arc::clone(&engine)).await?);

        // `.orchestry/run` is the private home of the default socket and of
        // transfer staging, whichever endpoint is used.
        discovery::prepare_private_dir(&state_dir.join(discovery::RUN_DIR))?;
        if let Err(error) = handler.cleanup_stale_transfers() {
            tracing::warn!(code = ?error.code, "stale transfer cleanup failed");
        }

        let instance = engine.ids.generate("daemon");
        let started_at = engine.clock.now_iso();
        let (protocol, binary) = spec.versions.unwrap_or_else(|| {
            (
                orch_proto::LOCAL_PROTOCOL,
                orch_proto::BINARY_VERSION.to_owned(),
            )
        });
        let executable = std::env::current_exe().unwrap_or_default();

        let mut endpoint = None;
        let mut listener = None;
        if let Some(path) = &spec.socket {
            let parent = path.parent().unwrap_or(Path::new("/"));
            discovery::prepare_private_dir(parent)?;
            match discovery::classify_socket(path).await? {
                SocketState::Absent => {}
                SocketState::Stale { dev, ino } => {
                    tracing::info!("replacing a stale socket");
                    discovery::remove_if_same(path, dev, ino)?;
                }
                SocketState::Live => {
                    return Err(DaemonError::InvalidArguments(format!(
                        "{} already has a live listener; another daemon or program uses it. \
                         Set ORCH_SOCKET to a different path",
                        path.display()
                    )));
                }
                SocketState::Indeterminate(kind) => {
                    return Err(DaemonError::InvalidArguments(format!(
                        "{} could not be probed ({kind}), so it may still have a live listener; \
                         it was left alone. Check its owner and mode, or set ORCH_SOCKET to a \
                         different path",
                        path.display()
                    )));
                }
                SocketState::Unsafe(what) => {
                    return Err(DaemonError::InvalidArguments(format!(
                        "{} is {what}; it is never removed automatically. Remove it yourself \
                         or set ORCH_SOCKET to a different path",
                        path.display()
                    )));
                }
            }
            let (std_listener, dev, ino) = discovery::bind_private(path)?;
            listener = Some(
                tokio::net::UnixListener::from_std(std_listener)
                    .map_err(|source| discovery::io_error("register the listener", source))?,
            );
            endpoint = Some(Endpoint {
                path: path.clone(),
                dev,
                ino,
            });
        }

        let config = ServerConfig {
            project_root: root.clone(),
            project_id,
            instance: instance.clone(),
            pid: std::process::id(),
            protocol,
            binary: binary.clone(),
            euid: nix::unistd::geteuid().as_raw(),
        };
        let wire = |path: &Path| {
            WirePath::new(path).map_err(|_| {
                DaemonError::InvalidArguments(format!("{} is not a valid path", path.display()))
            })
        };
        let socket_path = spec.socket.clone().unwrap_or_else(|| {
            state_dir
                .join(discovery::RUN_DIR)
                .join(discovery::SOCKET_NAME)
        });
        handler.configure_status(DaemonStatus {
            pid: config.pid,
            project_root: wire(&root)?,
            socket: wire(&socket_path)?,
            protocol,
            binary: binary.clone(),
            instance: instance.clone(),
            executable: wire(&executable)?,
            started_at: started_at.clone(),
            dispatch: handler.dispatch_state(),
            running_tasks: Vec::new(),
            sessions: 0,
            session_cap: orch_proto::frame::limits::MAX_SESSIONS as u32,
            busy_rejections: 0,
            ownership: OwnershipStatus::Held,
            removed_env: spec.snapshot.removed.clone(),
        });
        if spec.dispatch == DispatchMode::On {
            handler
                .execute_request(
                    &internal_context(),
                    Request::DispatchSet(DispatchSetArgs {
                        dispatch: DispatchSwitch::On,
                    }),
                )
                .await
                .map_err(|error| DaemonError::Operation(Box::new(error)))?;
        }

        let server = Server::new(config, handler.clone());
        let (quit, _) = watch::channel(false);
        let mut running = Running {
            engine,
            handler: handler.clone(),
            server: Arc::clone(&server),
            project_root: root.clone(),
            instance: instance.clone(),
            endpoint,
            quit,
            loops: Vec::new(),
            runtime: tokio::runtime::Handle::current(),
            pid_lock: Arc::clone(&pid_lock),
            guard: None,
            peer,
        };
        if let Some(path) = &spec.socket {
            // Keep the runtime files out of a project that tracks parts of
            // `.orchestry` (R2). Hygiene: its failure never blocks READY.
            if let Err(error) = crate::gitignore::ensure_runtime_gitignore(&root) {
                tracing::warn!(op = error.op, code = ?error.code, "runtime .gitignore entries not ensured");
            }
            let record = orch_proto::discovery::DaemonRecord {
                schema: orch_proto::discovery::DISCOVERY_SCHEMA,
                project_root: wire(&root)?,
                project_id,
                socket: wire(path)?,
                pid: std::process::id(),
                binary: bounded_id(&binary)?,
                protocol,
                executable: wire(&executable)?,
                started_at: bounded_id(&started_at)?,
                instance: bounded_id(&instance)?,
            };
            if let Err(error) = discovery::publish(&root, &record) {
                running.remove_endpoint();
                return Err(error);
            }
        }

        if let Some(listener) = listener {
            running
                .loops
                .push(tokio::spawn(crate::listener::accept_loop(
                    listener,
                    Arc::clone(&server),
                )));
        }
        // Maintenance (expiry, transfer sweeps) runs whatever the dispatch
        // switch says; it ends itself once admission stops.
        if maintenance_timer {
            let maintenance = handler.clone();
            running.loops.push(tokio::spawn(
                async move { maintenance.run_maintenance().await },
            ));
        }
        running.loops.push(tokio::spawn(heartbeat(
            pid_lock,
            handler.clone(),
            running.quit.subscribe(),
        )));
        running.loops.push(tokio::spawn(crate::dispatch::run_loop(
            handler,
            running.quit.subscribe(),
        )));
        if let Some(peer) = running.peer.as_mut() {
            peer.open();
        }
        Ok(running)
    }

    /// Block until a stop intent, `daemon.stop`, or (foreground) a signal.
    pub(crate) async fn wait_for_stop(&self, signals: Option<&mut Signals>) -> &'static str {
        let mut requested = self.server.stop.requested();
        let mut stopped = self.handler.stopped();
        let signal = async {
            match signals {
                Some(signals) => signals.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = crate::session::until_set(&mut requested) => "stop intent",
            () = until_stopped(&mut stopped) => "daemon.stop",
            name = signal => name,
        }
    }

    /// The one clean path for every stop trigger (R14). Returns a failure
    /// exit when agent cleanup was incomplete, never a false clean stop.
    pub(crate) async fn shutdown(mut self) -> DaemonExit {
        let mut clean = self.stop_engine().await;
        drop(self.peer.take());
        self.server.begin_drain();
        self.handler.seal_requests();
        self.quit.send_replace(true);
        // These loops can be inside external preparation or a partial commit.
        // Aborting or detaching them on a deadline cannot revoke that authority.
        while let Some(task) = self.loops.last_mut() {
            if let Err(error) = task.await {
                clean = false;
                tracing::warn!(%error, "daemon background task failed during drain");
            }
            self.loops.pop();
        }
        self.handler.drain_requests().await;
        // Stop-intent callers get completion only after accepted work is gone.
        self.server.stop.finish(clean);
        if !self.server.wait_idle(SESSION_DRAIN).await {
            tracing::warn!("sessions did not close within the drain deadline");
        }
        self.remove_endpoint();
        discovery::remove_own_record(&self.project_root, &self.instance);
        self.pid_lock.release();
        // The guard is released last, so a successor cannot start while this
        // process still holds any of the project.
        drop(self.guard.take());
        tracing::info!(clean, "stopped");
        DaemonExit {
            code: if clean {
                0
            } else {
                orch_proto::error::exit::GENERAL
            },
            clean,
        }
    }

    async fn stop_engine(&self) -> bool {
        if self.handler.coordinator().is_stopping() {
            // `daemon.stop` is already running the same handler.
            let mut stopped = self.handler.stopped();
            return match tokio::time::timeout(STOP_WAIT, stopped.wait_for(Option::is_some)).await {
                Ok(Ok(value)) => value.unwrap_or(false),
                _ => false,
            };
        }
        match self
            .handler
            .execute_request(&internal_context(), Request::DaemonStop(DaemonStopArgs {}))
            .await
        {
            Ok(committed) => serde_json::from_str::<StopOutcome>(committed.value.get())
                .map(|outcome| outcome.clean)
                .unwrap_or(false),
            Err(error) => {
                tracing::warn!(code = ?error.code, execution = ?error.execution, "shutdown cleanup failed");
                false
            }
        }
    }

    fn remove_endpoint(&mut self) {
        if let Some(endpoint) = self.endpoint.take()
            && let Err(error) =
                discovery::remove_if_same(&endpoint.path, endpoint.dev, endpoint.ino)
        {
            tracing::warn!(%error, "could not remove the socket");
        }
    }

    /// Best-effort teardown for a harness dropped without `shutdown`:
    /// ownership is retained by an asynchronous drain until accepted work ends.
    #[cfg_attr(
        not(feature = "testing"),
        allow(dead_code, reason = "used by the testing harness only")
    )]
    pub(crate) fn abort(&mut self) {
        self.server.begin_drain();
        self.handler.coordinator().stop_admission();
        self.handler.seal_requests();
        drop(self.peer.take());
        self.quit.send_replace(true);
        let loops = std::mem::take(&mut self.loops);
        let handler = self.handler.clone();
        self.remove_endpoint();
        discovery::remove_own_record(&self.project_root, &self.instance);
        let pid_lock = Arc::clone(&self.pid_lock);
        let guard = self.guard.take();
        // A synchronous drop cannot join asynchronous work. Move the ownership
        // resources into the drain instead of making a successor race old writes.
        self.runtime.spawn(async move {
            let _ = handler.engine.orchestrator.daemon_stop().await;
            for task in loops {
                let _ = task.await;
            }
            handler.drain_requests().await;
            pid_lock.release();
            drop(guard);
        });
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Includes cancellation of the shutdown future itself. Joined loop
        // handles stay in `self` until drain completes so this fallback owns them.
        if self.guard.is_some() {
            self.abort();
        }
    }
}

/// The daemon's own requests (stop on a signal, `--dispatch on`): an owner
/// CLI identity with the full mode.
pub(crate) fn internal_context() -> OpContext {
    OpContext::new(Session {
        caller: Caller {
            kind: CallerKind::Cli,
            agent_id: None,
            pid: Some(std::process::id()),
        },
        mode: SessionMode::AllowDestructive,
    })
}

fn bounded_id(text: &str) -> Result<orch_proto::bounded::Id, DaemonError> {
    orch_proto::bounded::Id::new(text)
        .map_err(|_| DaemonError::InvalidArguments(format!("{text:?} is too long")))
}

/// Refresh the legacy PID lock on its own timer, independent of dispatch
/// (SE-2). A missing or foreign lock is never recreated: ownership is lost,
/// dispatch goes off and status says so.
async fn heartbeat(lock: Arc<PidLock>, handler: ServiceHandler, mut quit: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            () = crate::session::until_set(&mut quit) => return,
            () = tokio::time::sleep(PID_REFRESH) => {}
        }
        if handler.coordinator().is_stopping() {
            return;
        }
        if lock.refresh().await == PidLockState::Lost {
            tracing::error!(
                "ownership_lost: the project PID lock is missing or names another process"
            );
            if let Err(error) = handler.set_ownership_lost().await {
                tracing::error!(code = ?error.code, "could not record ownership loss");
            }
            return;
        }
    }
}

/// Options of `orch init`, the only project-creation path. Mirrors the CLI's
/// existing `InitArgs`.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub project_root: PathBuf,
    /// `--name`.
    pub name: Option<String>,
    /// `--adapter`.
    pub adapter: Option<String>,
    /// `--prompt-mode`; omitted writes `normal`.
    pub prompt_mode: Option<orch_domain::PromptMode>,
    /// Both stdout and stdin are a TTY (gates the multi-adapter prompt in the
    /// CLI; the daemon API itself never prompts).
    pub interactive: bool,
}

/// What [`initialize`] did, so the CLI can print the start hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Re-run on an initialized project: nothing was overwritten.
    pub already_initialized: bool,
    pub adapter: String,
}

/// Create `.orchestry/` (idempotently). Starts no server, never mutates a
/// live daemon's state, and leaves printing `orch daemon start` to the CLI.
pub async fn initialize(options: InitOptions) -> Result<InitOutcome, DaemonError> {
    let _ = options;
    Err(DaemonError::unimplemented(
        "orch_daemon::initialize",
        "S1a-impl",
    ))
}

/// Test daemons for this crate's unit tests: a temp project, a private
/// scratch dir under `/tmp` (socket, `global.yml`), production code paths.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) struct TestDaemon {
        pub(crate) project: tempfile::TempDir,
        pub(crate) scratch: tempfile::TempDir,
        pub(crate) running: Running,
    }

    impl TestDaemon {
        pub(crate) fn root(&self) -> PathBuf {
            self.project.path().canonicalize().unwrap()
        }
    }

    pub(crate) fn project() -> tempfile::TempDir {
        let project = tempfile::tempdir().unwrap();
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(project.path().join(".orchestry"))
            .unwrap();
        project
    }

    pub(crate) fn spec(
        root: &Path,
        scratch: &Path,
        socket: bool,
        versions: Option<(u32, String)>,
    ) -> BringUp {
        BringUp {
            project_root: root.canonicalize().unwrap(),
            socket: socket.then(|| scratch.join("s")),
            dispatch: DispatchMode::Off,
            listen: None,
            peer_name: None,
            seams: EngineSeams {
                global_config_path: Some(scratch.join("global.yml")),
                ..EngineSeams::default()
            },
            processes: None,
            versions,
            snapshot: crate::env::Snapshot::default(),
        }
    }

    pub(crate) fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("od-")
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap()
    }

    pub(crate) async fn start(socket: bool, versions: Option<(u32, String)>) -> TestDaemon {
        let project = project();
        let scratch = scratch();
        let running = Running::bring_up(spec(project.path(), scratch.path(), socket, versions))
            .await
            .unwrap();
        TestDaemon {
            project,
            scratch,
            running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{project, scratch, spec};
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bring_up_publishes_private_files_and_shutdown_releases_ownership_last() {
        let daemon = super::test_support::start(true, None).await;
        let root = daemon.root();
        let state = root.join(".orchestry");
        let socket = daemon.scratch.path().join("s");

        let record = discovery::read_record(&root).unwrap().expect("daemon.json");
        assert_eq!(record.socket.as_path(), socket);
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.instance.as_str(), daemon.running.instance);
        assert_eq!(record.project_root.as_path(), root);
        let mode = |path: &Path| std::fs::symlink_metadata(path).unwrap().mode() & 0o777;
        assert_eq!(mode(&state.join("daemon.json")), 0o600);
        assert_eq!(mode(&socket), 0o600);
        assert_eq!(mode(&state.join("run")), 0o700);
        assert_eq!(
            std::fs::read_to_string(state.join("orchestry.lock")).unwrap(),
            std::process::id().to_string()
        );

        // A second owner for the same project loses on the guard and names us.
        let scratch = scratch();
        let error = Running::bring_up(spec(&root, scratch.path(), true, None))
            .await
            .err()
            .expect("second start must fail");
        assert!(
            matches!(error, DaemonError::AlreadyRunning(Some(pid)) if pid == std::process::id()),
            "{error:?}"
        );
        assert!(
            socket.exists(),
            "the loser never touches the winner's socket"
        );

        let exit = daemon.running.shutdown().await;
        assert_eq!(exit, DaemonExit::CLEAN);
        assert!(!socket.exists());
        assert!(!state.join("daemon.json").exists());
        assert!(!state.join("orchestry.lock").exists());
        assert!(
            state.join(crate::guard::GUARD_FILE).exists(),
            "the guard inode is never unlinked"
        );
        // Ownership is free again.
        let again = Running::bring_up(spec(&root, scratch.path(), false, None))
            .await
            .unwrap();
        assert_eq!(again.shutdown().await, DaemonExit::CLEAN);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_socket_is_replaced_and_a_regular_file_is_refused() {
        let project = project();
        let scratch = scratch();
        let socket = scratch.path().join("s");
        // Stale: bound once, listener gone.
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let running = Running::bring_up(spec(project.path(), scratch.path(), true, None))
            .await
            .unwrap();
        assert!(tokio::net::UnixStream::connect(&socket).await.is_ok());
        assert_eq!(running.shutdown().await, DaemonExit::CLEAN);

        std::fs::write(&socket, b"not a socket").unwrap();
        let error = Running::bring_up(spec(project.path(), scratch.path(), true, None))
            .await
            .err()
            .expect("a regular file is never replaced");
        assert!(error.to_string().contains("a regular file"), "{error}");
        assert_eq!(std::fs::read(&socket).unwrap(), b"not a socket");
        // The failed start released ownership.
        std::fs::remove_file(&socket).unwrap();
        let running = Running::bring_up(spec(project.path(), scratch.path(), true, None))
            .await
            .unwrap();
        assert_eq!(running.shutdown().await, DaemonExit::CLEAN);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_foreign_listener_is_never_replaced() {
        let project = project();
        let scratch = scratch();
        let socket = scratch.path().join("s");
        let _other = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let error = Running::bring_up(spec(project.path(), scratch.path(), true, None))
            .await
            .err()
            .expect("a live listener is never replaced");
        assert!(error.to_string().contains("live listener"), "{error}");
        assert!(socket.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn status_reports_provenance_and_removed_env_names() {
        let project = project();
        let scratch = scratch();
        let mut spec = spec(project.path(), scratch.path(), false, None);
        spec.snapshot = crate::env::capture([
            ("PATH".into(), "/bin".into()),
            ("ORCH_TASK_ID".into(), "tsk_x".into()),
        ]);
        let running = Running::bring_up(spec).await.unwrap();
        let status = running
            .handler
            .execute_request(
                &internal_context(),
                Request::DaemonStatus(orch_proto::ops::args::DaemonStatusArgs {}),
            )
            .await
            .unwrap();
        let status: DaemonStatus = serde_json::from_str(status.value.get()).unwrap();
        assert_eq!(status.pid, std::process::id());
        assert_eq!(status.instance, running.instance);
        assert_eq!(
            status.executable.as_path(),
            std::env::current_exe().unwrap()
        );
        assert_eq!(status.removed_env, ["ORCH_TASK_ID"]);
        assert!(
            !status.dispatch.dispatch_on,
            "a fresh daemon starts with dispatch off"
        );
        assert_eq!(status.ownership, OwnershipStatus::Held);
        assert_eq!(running.shutdown().await, DaemonExit::CLEAN);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_on_at_start_is_visible() {
        let project = project();
        let scratch = scratch();
        let mut spec = spec(project.path(), scratch.path(), false, None);
        spec.dispatch = DispatchMode::On;
        let running = Running::bring_up(spec).await.unwrap();
        assert!(running.handler.dispatch_state().dispatch_on);
        assert_eq!(running.shutdown().await, DaemonExit::CLEAN);
    }

    #[test]
    fn agent_context_is_either_identity_variable() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        assert!(!in_agent_context(env(&[("PATH", "/bin")])));
        assert!(in_agent_context(env(&[("ORCH_TASK_ID", "tsk_1")])));
        assert!(in_agent_context(env(&[("ORCH_AGENT_ID", "agt_1")])));
        assert!(!in_agent_context(env(&[("ORCH_AGENT_ID", "")])));
    }

    #[tokio::test]
    async fn agent_context_and_developmental_gate_refuse_with_exit_2() {
        let refused = start(StartOptions {
            developmental: false,
            from_agent: true,
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(refused, DaemonError::DevelopmentalGateClosed));
        assert_eq!(refused.exit_code(), 2);
        assert!(refused.to_string().contains("ORCH_DAEMON_DEVELOPMENTAL=1"));
        assert_eq!(DaemonError::RefusedInAgentContext.exit_code(), 2);
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect()
    }

    fn lookup(
        pairs: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// D11: preparation is synchronous (no runtime exists in this test) and
    /// fixes the agent environment and start options before `run`.
    #[test]
    fn prepare_captures_the_snapshot_and_options_before_any_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let project = project();
        let scratch = scratch();
        let env = vars(&[("PATH", "/bin"), ("DEBUG", "1"), ("ORCH_TASK_ID", "tsk_x")]);
        let prepared = prepare(
            StartOptions {
                project_root: project.path().to_path_buf(),
                socket: Some(scratch.path().join("s")),
                from_agent: true,
                listen: Some("127.0.0.1:0".into()),
                peer_name: Some("worker".into()),
                developmental: true,
                ..Default::default()
            },
            env.clone(),
            lookup(env),
        )
        .unwrap();
        assert!(prepared.log.is_none());
        let spec = prepared.bring_up();
        assert_eq!(spec.project_root, project.path().canonicalize().unwrap());
        assert_eq!(spec.socket, Some(scratch.path().join("s")));
        assert_eq!(spec.snapshot.removed, ["DEBUG", "ORCH_TASK_ID"]);
        assert_eq!(spec.snapshot.base, vars(&[("PATH", "/bin")]));
        assert_eq!(spec.peer_name.as_deref(), Some("worker"));
        assert_eq!(spec.listen.as_deref(), Some("127.0.0.1:0"));
        assert_eq!(spec.dispatch, DispatchMode::Off);
    }

    #[test]
    fn prepare_enforces_the_gates_from_the_given_environment() {
        let project = project();
        let options = || StartOptions {
            project_root: project.path().to_path_buf(),
            developmental: true,
            ..Default::default()
        };
        let agent = vars(&[("ORCH_AGENT_ID", "agt_1")]);
        assert!(matches!(
            prepare(options(), agent.clone(), lookup(agent)),
            Err(DaemonError::RefusedInAgentContext)
        ));
        assert!(matches!(
            prepare(
                StartOptions {
                    developmental: false,
                    ..options()
                },
                Vec::new(),
                lookup(Vec::new())
            ),
            Err(DaemonError::DevelopmentalGateClosed)
        ));
        assert!(matches!(
            prepare(
                StartOptions {
                    project_root: project.path().join("missing"),
                    ..options()
                },
                Vec::new(),
                lookup(Vec::new())
            ),
            Err(DaemonError::NotInitialized)
        ));
    }

    #[test]
    fn foreign_euid_owner_and_uninitialized_roots_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        assert!(matches!(
            discovery::select_project_root(Some(temp.path()), Path::new("/")),
            Err(DaemonError::NotInitialized)
        ));
        assert!(matches!(
            discovery::select_project_root(Some(Path::new("relative")), Path::new("/")),
            Err(DaemonError::InvalidArguments(_))
        ));
        std::fs::create_dir(temp.path().join(".orchestry")).unwrap();
        let nested = temp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            discovery::select_project_root(None, &nested).unwrap(),
            temp.path().canonicalize().unwrap()
        );
    }
}
