//! The CLI's serialized socket lane and typed error bridge.
//!
//! Commands retain the presentation layer's core `Result` while the single
//! exit boundary recovers the client error from its source. No error is
//! classified by parsing text, and unknown mutation outcomes are never retried.
use std::path::{Path, PathBuf};

use orch_client::{Client, ClientConfig, ClientError};
use orch_core::error::{Error, Result};
use orch_proto::ops::Op;
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{Mutex, MutexGuard};

pub struct Rpc {
    pub project_root: PathBuf,
    pub agent_id: Option<String>,
    client: Mutex<Client>,
}

impl Rpc {
    pub async fn connect(cli: &crate::tree::Cli, cwd: &Path) -> Result<Self> {
        let config = ClientConfig::from_env(cwd.to_path_buf())
            .and_then(|config| config.with_agent(cli.agent.as_deref()))
            .map_err(client_error)?;
        let agent_id = config
            .caller
            .agent_id
            .as_ref()
            .map(|id| id.as_str().to_owned());
        let project_root = config.project().map_err(client_error)?.root;
        let client = Client::connect(&config).await.map_err(client_error)?;
        Ok(Self {
            project_root,
            agent_id,
            client: Mutex::new(client),
        })
    }

    pub async fn request<A: Op>(&self, args: A) -> Result<A::Output> {
        self.client
            .lock()
            .await
            .request(args)
            .await
            .map_err(client_error)
    }

    /// Convert a presentation/domain input to a closed, bounded protocol DTO
    /// before it reaches the wire. This also rejects unrecognized fields.
    pub async fn call<A: Op + DeserializeOwned>(&self, input: impl Serialize) -> Result<A::Output> {
        self.request::<A>(arguments(input)?).await
    }

    pub(crate) async fn lane(&self) -> MutexGuard<'_, Client> {
        self.client.lock().await
    }

    /// Wrap an already-connected client (the in-process daemon harness).
    #[cfg(test)]
    pub(crate) fn from_client(project_root: PathBuf, client: Client) -> Self {
        Self {
            project_root,
            agent_id: None,
            client: Mutex::new(client),
        }
    }
}

pub fn arguments<T: DeserializeOwned>(input: impl Serialize) -> Result<T> {
    let value = serde_json::to_value(input)
        .map_err(|error| Error::other(format!("cannot encode daemon arguments: {error}")))?;
    serde_json::from_value(value).map_err(|error| Error::invalid_arguments(error.to_string()))
}

pub fn argument_error(error: impl std::fmt::Display) -> Error {
    Error::invalid_arguments(error.to_string())
}

/// The display keeps the client's own text, so a command that reports a
/// failure inline (`Failed to create agent "x": …`) still says why.
pub fn client_error(error: ClientError) -> Error {
    Error::Io {
        context: error.to_string(),
        path: None,
        source: std::io::Error::other(error),
    }
}

pub(crate) fn recover_client_error(error: &Error) -> Option<&ClientError> {
    match error {
        Error::Io { source, .. } => source.get_ref()?.downcast_ref::<ClientError>(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_proto::{error::NoDaemonDetail, ops::args};

    #[test]
    fn the_exit_boundary_recovers_the_original_typed_no_daemon_error() {
        let failure = ClientError::Connection(orch_client::NoDaemon::no_daemon(
            "/tmp/project",
            NoDaemonDetail::NoSocket,
        ));
        let expected = failure.render(true);
        let error = client_error(failure);
        let recovered = recover_client_error(&error).unwrap();
        assert_eq!(recovered.exit_code(), 7);
        assert_eq!(recovered.render(true), expected);
        assert!(recover_client_error(&Error::other("daemon request failed")).is_none());
    }

    #[test]
    fn an_inline_report_of_a_daemon_refusal_keeps_the_daemon_text() {
        let refusal = ClientError::Remote(orch_proto::error::ErrorBody::new(
            orch_proto::error::ErrorCode::Conflict,
            "agent \"x\" inherits project eco",
            orch_proto::error::Execution::NotExecuted,
        ));
        let error = client_error(refusal);
        assert_eq!(error.to_string(), "agent \"x\" inherits project eco");
        assert_eq!(recover_client_error(&error).unwrap().exit_code(), 1);
    }

    #[test]
    fn presentation_inputs_are_bounded_and_closed_before_a_request_is_sent() {
        assert!(arguments::<args::TaskGetArgs>(serde_json::json!({"id":"tsk_1"})).is_ok());
        let invalid =
            arguments::<args::TaskGetArgs>(serde_json::json!({"id":"tsk_1","extra":true}))
                .unwrap_err();
        assert_eq!(invalid.exit_code(), 2);
        assert!(arguments::<args::TaskGetArgs>(serde_json::json!({"id":"a".repeat(257)})).is_err());
    }
}
