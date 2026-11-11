//! Test support: call contexts and connectors over an in-process daemon.
//!
//! The daemon is `orch_daemon::testing::DaemonHarness`: the production
//! session code on a duplex stream, a fresh temp project and a harness-owned
//! `global.yml`. Nothing here reads `~/.orchestry` or dials a real daemon.

use std::path::Path;
use std::sync::Arc;

use orch_client::{Client, ClientError, NoDaemon, Project};
use orch_daemon::testing::DaemonHarness;
use orch_proto::bounded::Id;
use orch_proto::error::NoDaemonDetail;
use orch_proto::handshake::{Caller, CallerKind};

use super::identity::Identity;
use super::mode::Mode;
use super::registry::CallContext;
use super::server::Connector;

/// A running harness daemon. Dropping it aborts the daemon.
pub(crate) struct Fixture {
    daemon: Arc<DaemonHarness>,
}

impl Fixture {
    pub(crate) async fn start() -> Fixture {
        let daemon = DaemonHarness::builder()
            .maintenance_timer(false)
            .start()
            .await
            .expect("harness daemon");
        Fixture {
            daemon: Arc::new(daemon),
        }
    }

    /// The daemon's engine, for seeding and peeking from test code only.
    pub(crate) fn engine(&self) -> &orch_engine::FullEngine {
        self.daemon.engine()
    }

    pub(crate) fn root(&self) -> &Path {
        self.daemon.project_root()
    }

    /// Sessions to this daemon as an MCP bridge in `mode` with `agent`.
    pub(crate) fn connector(&self, mode: Mode, agent: Option<&str>) -> Connector {
        let daemon = Arc::clone(&self.daemon);
        let caller = Caller {
            kind: CallerKind::Mcp,
            agent_id: agent.map(|agent| Id::new(agent).expect("short agent id")),
            pid: None,
        };
        Arc::new(move || {
            let (daemon, caller) = (Arc::clone(&daemon), caller.clone());
            Box::pin(async move {
                let endpoint = daemon
                    .connect()
                    .await
                    .map_err(|err| ClientError::InvalidArguments(err.to_string()))?;
                let project = Project::discover(daemon.project_root(), None)?;
                Client::from_stream(project, endpoint.into_stream(), caller, mode.session()).await
            })
        })
    }

    /// A call context with `identity` in `mode`.
    pub(crate) fn context(&self, identity: Option<&str>, mode: Mode) -> Arc<CallContext> {
        Arc::new(CallContext::new(
            self.connector(mode, identity),
            self.root().to_path_buf(),
            Identity::of(identity),
            identity.is_some(),
        ))
    }
}

/// A connector that finds no daemon, as `Client::connect` reports a missing
/// socket.
pub(crate) fn no_daemon(root: &Path) -> Connector {
    let root = root.to_string_lossy().into_owned();
    Arc::new(move || {
        let failure = NoDaemon::no_daemon(&root, NoDaemonDetail::NoSocket);
        Box::pin(async move { Err(failure.into()) })
    })
}
