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
    client: Mutex<Client>,
}

impl Rpc {
    pub async fn connect(cli: &crate::tree::Cli, cwd: &Path) -> Result<Self> {
        let config = ClientConfig::from_env(cwd.to_path_buf())
            .and_then(|config| config.with_agent(cli.agent.as_deref()))
            .map_err(client_error)?;
        let project_root = config.project().map_err(client_error)?.root;
        let client = Client::connect(&config).await.map_err(client_error)?;
        Ok(Self {
            project_root,
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
}

pub fn arguments<T: DeserializeOwned>(input: impl Serialize) -> Result<T> {
    let value = serde_json::to_value(input)
        .map_err(|error| Error::other(format!("cannot encode daemon arguments: {error}")))?;
    serde_json::from_value(value).map_err(|error| Error::invalid_arguments(error.to_string()))
}

pub fn argument_error(error: impl std::fmt::Display) -> Error {
    Error::invalid_arguments(error.to_string())
}

pub fn client_error(error: ClientError) -> Error {
    Error::Io {
        context: "daemon request failed".into(),
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
