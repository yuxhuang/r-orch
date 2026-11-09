use orch_proto::error::{ErrorBody, exit};

/// Failure of a daemon entry point. [`DaemonError::exit_code`] is the process
/// exit code `orch daemon start` reports.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Another process owns the project (exit 4). `None` when the winner died
    /// before publishing its PID: never an invented one.
    #[error("{}", orch_proto::error::already_running(*.0))]
    AlreadyRunning(Option<u32>),
    /// The legacy PID lock names a live process that is not this daemon
    /// (exit 4). It is never evicted by age; the owner verifies and stops it.
    #[error(
        "the project lock .orchestry/orchestry.lock is held by live PID {0}; verify it with \
         `ps -p {0} -o command=` and stop that orch process, then start the daemon again"
    )]
    LegacyLockHeld(u32),
    /// Explicit start from an agent context without `--from-agent` (exit 2).
    #[error("{}", orch_proto::error::START_REFUSED_IN_AGENT_CONTEXT)]
    RefusedInAgentContext,
    /// Invalid options, paths or ownership (exit 2).
    #[error("{0}")]
    InvalidArguments(String),
    /// No `.orchestry/` at the selected root (exit 3).
    #[error("Not initialized")]
    NotInitialized,
    /// Engine, store or recovery failure, with its exit code intact.
    #[error(transparent)]
    Core(#[from] orch_core::error::Error),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    /// A daemon-internal operation was refused or failed.
    #[error("{}", .0.message.as_str())]
    Operation(Box<ErrorBody>),
    /// Shutdown could not stop or reconcile every agent; recovery evidence
    /// is kept (R14).
    #[error("daemon shutdown cleanup was incomplete; recovery evidence is kept")]
    StopIncomplete,
    /// A signature in the PR 1 skeleton whose body is not written yet.
    #[error("{what} is not implemented yet (owner: {owner})")]
    Unimplemented {
        what: &'static str,
        owner: &'static str,
    },
}

impl DaemonError {
    pub fn exit_code(&self) -> i32 {
        match self {
            DaemonError::AlreadyRunning(_) | DaemonError::LegacyLockHeld(_) => exit::LOCK_CONFLICT,
            DaemonError::RefusedInAgentContext | DaemonError::InvalidArguments(_) => exit::INVALID_ARGUMENTS,
            DaemonError::NotInitialized => exit::NOT_INITIALIZED,
            DaemonError::Core(error) => error.exit_code(),
            DaemonError::Operation(body) => body.exit_code,
            DaemonError::Io { .. }
            | DaemonError::StopIncomplete
            | DaemonError::Unimplemented { .. } => exit::GENERAL,
        }
    }

    pub(crate) fn unimplemented(what: &'static str, owner: &'static str) -> Self {
        DaemonError::Unimplemented { what, owner }
    }
}

impl From<&DaemonError> for ErrorBody {
    fn from(error: &DaemonError) -> Self {
        use orch_proto::error::{ErrorCode, Execution};
        let code = match error {
            DaemonError::AlreadyRunning(_) | DaemonError::LegacyLockHeld(_) => {
                ErrorCode::LockConflict
            }
            DaemonError::RefusedInAgentContext | DaemonError::InvalidArguments(_) => ErrorCode::InvalidArguments,
            DaemonError::NotInitialized => ErrorCode::NotInitialized,
            DaemonError::Unimplemented { .. } => ErrorCode::Unimplemented,
            DaemonError::Operation(body) => return (**body).clone(),
            DaemonError::Core(_) | DaemonError::Io { .. } | DaemonError::StopIncomplete => {
                ErrorCode::General
            }
        };
        let mut body = ErrorBody::new(code, &error.to_string(), Execution::Unknown);
        body.exit_code = error.exit_code();
        body
    }
}
