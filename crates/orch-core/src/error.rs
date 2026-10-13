//! The error type every port trait returns.
//!
//! [`DomainError`] is the user-facing contract (its `Display` strings are
//! PARITY.md §3.1 verbatim). `Error` wraps it and adds the infrastructure
//! cases, keeping a `source` chain so `ORCHESTRY_DEBUG` can print it.
//!
//! Exit-code mapping is preserved: a wrapped [`DomainError`] keeps its code,
//! and everything else is exit 1 — which is exactly what the TS CLI does with
//! a `throw` that is not an `OrchestryError` (PARITY.md §3).

use std::path::{Path, PathBuf};

use orch_domain::DomainError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A domain error, with its exit code and hint intact.
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error("{context}")]
    Io {
        context: String,
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    /// A JSON, JSONL or YAML (de)serialization failure.
    #[error("{context}")]
    Serde {
        context: String,
        path: Option<PathBuf>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An external command (git, an adapter binary) failed.
    #[error("{command} exited with {status}: {output}")]
    Command {
        command: String,
        status: String,
        output: String,
    },

    #[error(transparent)]
    Adapter(#[from] crate::adapter::AdapterError),
}

impl Error {
    /// Process exit code (PARITY.md §3). Anything that is not a domain error
    /// exits 1, matching the TS catch-all.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Domain(err) => err.exit_code(),
            _ => 1,
        }
    }

    /// The hint line, if any (PARITY.md §3.1).
    pub fn hint(&self) -> Option<String> {
        match self {
            Error::Domain(err) => err.hint(),
            _ => None,
        }
    }

    pub fn io(context: impl Into<String>, path: impl AsRef<Path>, source: std::io::Error) -> Error {
        Error::Io {
            context: context.into(),
            path: Some(path.as_ref().to_path_buf()),
            source,
        }
    }

    pub fn serde(
        context: impl Into<String>,
        path: impl AsRef<Path>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Error {
        Error::Serde {
            context: context.into(),
            path: Some(path.as_ref().to_path_buf()),
            source: Box::new(source),
        }
    }

    /// Shorthand for the TS `throw new Error(msg)` case: exit 1, no hint.
    pub fn other(message: impl Into<String>) -> Error {
        Error::Domain(DomainError::Other(message.into()))
    }

    /// Shorthand for an `InvalidArgumentsError` (exit 2).
    pub fn invalid_arguments(message: impl Into<String>) -> Error {
        Error::Domain(DomainError::InvalidArguments(message.into()))
    }

    /// The path the failure concerned, when it is known.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Error::Io { path, .. } | Error::Serde { path, .. } => path.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_domain_error_keeps_its_exit_code_and_hint() {
        let err = Error::from(DomainError::NotInitialized);
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.hint().as_deref(), Some("Run: orch init"));
        assert_eq!(err.to_string(), "Not initialized");
    }

    #[test]
    fn infrastructure_errors_exit_1_without_a_hint() {
        let err = Error::io(
            "read .orchestry/state.json",
            "/tmp/state.json",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.hint(), None);
        assert_eq!(err.to_string(), "read .orchestry/state.json");
        assert_eq!(err.path(), Some(Path::new("/tmp/state.json")));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn invalid_arguments_shorthand_exits_2() {
        assert_eq!(
            Error::invalid_arguments("Task title is required").exit_code(),
            2
        );
    }
}
