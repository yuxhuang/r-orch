//! A test daemon for the in-process dashboard tests (S2c).
//!
//! Every dashboard test talks to a real daemon front end the way `orch tui`
//! does: an in-process [`DaemonHarness`] bound to a short private Unix socket,
//! reached through the production [`Connector::from_config`] dial path
//! (socket validation, peer credentials, handshake). The harness's engine is
//! exposed through `Deref` for **seeding and peeking only**, never as the
//! thing under test. Nothing here starts an `orch` process, and nothing
//! reaches `~/.orchestry` or a real project's daemon: the project is a fresh
//! temp dir, `global.yml` lives inside it, and the client refuses any socket
//! outside the harness's own directory.
#![allow(dead_code, reason = "each test crate uses a subset of this module")]

use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use orch_cli::tui::{Connector, DaemonBackend};
use orch_client::ClientConfig;
use orch_daemon::testing::{DaemonHarness, HarnessBuilder};
use orch_engine::FullEngine;
use orch_proto::handshake::{Caller, CallerKind, SessionMode};

/// A running test daemon over one project.
pub struct TestDaemon {
    harness: DaemonHarness,
    config: ClientConfig,
}

impl Deref for TestDaemon {
    type Target = FullEngine;
    fn deref(&self) -> &FullEngine {
        self.harness.engine()
    }
}

impl TestDaemon {
    /// Start a daemon over `root` (already initialised) with `builder`'s seams
    /// plus a bound socket and `global.yml` kept inside the project.
    pub async fn start(root: &Path, builder: HarnessBuilder) -> TestDaemon {
        let harness = builder
            .global_config_path(root.join(".orchestry/global.yml"))
            .on_socket()
            .start_in(root)
            .await
            .expect("start the test daemon");
        let config = client_config(&harness);
        TestDaemon { harness, config }
    }

    /// The daemon's dispatch is OFF by default (design D8), as on a real
    /// explicit start.
    pub async fn start_default(root: &Path) -> TestDaemon {
        Self::start(root, DaemonHarness::builder().maintenance_timer(false)).await
    }

    pub fn harness(&self) -> &DaemonHarness {
        &self.harness
    }

    pub fn root(&self) -> &Path {
        self.harness.project_root()
    }

    /// The production connector for this daemon's socket.
    pub fn connector(&self) -> Connector {
        Connector::from_config(self.config.clone(), CallerKind::Tui)
    }

    /// The connector `orch web`'s preflight uses for this daemon.
    pub fn web_connector(&self) -> Connector {
        Connector::from_config(self.config.clone(), CallerKind::Web)
    }

    /// The client config a frontend would build for this project.
    pub fn client_config(&self) -> ClientConfig {
        self.config.clone()
    }

    /// Stop the daemon the clean way (what `orch daemon stop` does).
    pub async fn shutdown(self) {
        self.harness.shutdown().await.expect("stop the test daemon");
    }

    /// Launch a dashboard backend against this daemon.
    pub async fn launch(&self) -> DaemonBackend {
        DaemonBackend::launch(
            self.connector(),
            "9.9.9",
            self.root().to_path_buf(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .expect("launch the dashboard backend")
    }
}

/// The client config for `harness`: explicit socket, refused outside its own
/// directory, caller = the dashboard, owner (no agent) context.
pub fn client_config(harness: &DaemonHarness) -> ClientConfig {
    let socket = harness.socket().expect("bound socket").to_path_buf();
    let root = harness.project_root().to_path_buf();
    ClientConfig {
        cwd: root.clone(),
        project_root: Some(root),
        test_socket_root: socket.parent().map(Path::to_path_buf),
        socket: Some(socket),
        caller: Caller {
            kind: CallerKind::Tui,
            agent_id: None,
            pid: None,
        },
        mode: SessionMode::Normal,
        agent_context: false,
    }
}

/// A connector for a project that has **no** daemon: the real dial path
/// against a socket path nothing listens on.
pub fn absent_daemon_connector(root: &Path, scratch: &Path) -> Connector {
    absent_daemon_connector_as(root, scratch, CallerKind::Tui)
}

/// [`absent_daemon_connector`] as `orch web`'s preflight opens it.
pub fn absent_daemon_web_connector(root: &Path, scratch: &Path) -> Connector {
    absent_daemon_connector_as(root, scratch, CallerKind::Web)
}

fn absent_daemon_connector_as(root: &Path, scratch: &Path, kind: CallerKind) -> Connector {
    Connector::from_config(
        ClientConfig {
            cwd: root.to_path_buf(),
            project_root: Some(root.to_path_buf()),
            test_socket_root: Some(scratch.to_path_buf()),
            socket: Some(scratch.join("s")),
            caller: Caller {
                kind,
                agent_id: None,
                pid: None,
            },
            mode: SessionMode::Normal,
            agent_context: false,
        },
        kind,
    )
}

/// A short private scratch directory under `/tmp` (a socket path budget of
/// 104 bytes on macOS rules out `$TMPDIR`).
pub fn short_scratch() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .prefix("otu-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .expect("scratch dir")
}

/// Run `body` on a thread that is not driving the runtime, as the draw loop
/// is, and hand back whatever it returns.
pub async fn on_draw_thread<T, F>(mut backend: DaemonBackend, body: F) -> (DaemonBackend, T)
where
    T: Send + 'static,
    F: FnOnce(&mut DaemonBackend) -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let value = std::thread::scope(|scope| {
            scope
                .spawn(|| body(&mut backend))
                .join()
                .expect("the draw thread panicked")
        });
        (backend, value)
    })
    .await
    .expect("the blocking task panicked")
}
