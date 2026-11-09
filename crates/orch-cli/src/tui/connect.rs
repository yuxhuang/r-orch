//! How the dashboard reaches the daemon: a [`Connector`] opens one fresh,
//! authenticated [`Client`] session per call.
//!
//! The dashboard needs more than one session at a time — the request lane the
//! draw thread drives, a `logs.follow` stream feeding the activity pane, and a
//! short-lived session per background job (history, model catalogue, a
//! prompt-mode write, an observed `run`) — so it holds a way to *open*
//! sessions rather than one session.
//!
//! Opening a session is the only thing a connector does. It never starts,
//! forks or executes anything (goal row 3, spec R4): production connectors go
//! through [`Client::connect`], which only dials the socket recorded for the
//! project and fails with the frozen NoDaemon error when nothing answers.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use orch_client::{Client, ClientConfig, ClientError};
use orch_proto::handshake::CallerKind;

/// A boxed connect future.
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<Client, ClientError>> + Send>>;

/// Opens daemon sessions for one project. Cheap to clone.
#[derive(Clone)]
pub struct Connector {
    open: Arc<dyn Fn() -> ConnectFuture + Send + Sync>,
    project_root: PathBuf,
    agent_context: bool,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("project_root", &self.project_root)
            .field("agent_context", &self.agent_context)
            .finish_non_exhaustive()
    }
}

impl Connector {
    /// The production connector: [`Client::connect`] with `config`, opened as
    /// `kind` (the TUI, or `orch web`'s preflight).
    ///
    /// The project root is resolved once here (discovery reads only the
    /// directory tree, never the daemon), so every message names the real
    /// root even from a subdirectory; the cwd stands in when it cannot be.
    pub fn from_config(config: ClientConfig, kind: CallerKind) -> Connector {
        let config = Arc::new(config.with_frontend(kind));
        let agent_context = config.agent_context;
        let project_root = config
            .project()
            .map(|project| project.root)
            .unwrap_or_else(|_| {
                config
                    .project_root
                    .clone()
                    .unwrap_or_else(|| config.cwd.clone())
            });
        Connector {
            open: Arc::new(move || {
                let config = Arc::clone(&config);
                Box::pin(async move { Client::connect(&config).await })
            }),
            project_root,
            agent_context,
        }
    }

    /// Any other way of opening a session — the in-process daemon harness in
    /// tests. `open` must itself never start a daemon.
    pub fn new(
        project_root: PathBuf,
        agent_context: bool,
        open: impl Fn() -> ConnectFuture + Send + Sync + 'static,
    ) -> Connector {
        Connector {
            open: Arc::new(open),
            project_root,
            agent_context,
        }
    }

    /// Open one new session.
    pub async fn connect(&self) -> Result<Client, ClientError> {
        (self.open)().await
    }

    /// Whether failures render in the agent variant (RT4/SE-1).
    pub fn agent_context(&self) -> bool {
        self.agent_context
    }

    /// The project this connector was built for, as given.
    pub fn project_root(&self) -> &std::path::Path {
        &self.project_root
    }
}

/// Is this a failure of the *session* — no daemon, a lost connection, a
/// poisoned lane — rather than an ordinary refusal of one request?
///
/// A session failure ends the dashboard and brings back the no-daemon
/// screen; an ordinary refusal is shown as `Failed: …` and the dashboard
/// carries on.
pub fn is_session_failure(error: &ClientError) -> bool {
    matches!(error, ClientError::Connection(_))
}
