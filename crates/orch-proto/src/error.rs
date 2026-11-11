//! The error envelope, exit codes and the one place connection-failure text is
//! worded (design D5, D13; spec R3/R36).
//!
//! Frontends may style the text (colour, glyph folding) but never re-word it:
//! [`ConnectFailure::render`] is the only builder of the NoDaemon message, so
//! the CLI, TUI, web and MCP say the same thing.

use crate::bounded::BoundedString;
use crate::handshake::VersionSkew;
use crate::path::escape_controls;
use orch_domain::DomainError;
use serde::{Deserialize, Serialize};

/// Process exit codes. 1–6 are the existing codes (`DomainError::exit_code`);
/// 7 is added by the daemon protocol.
pub mod exit {
    pub const GENERAL: i32 = 1;
    /// Also: explicit start refused in an agent context without `--from-agent`.
    pub const INVALID_ARGUMENTS: i32 = 2;
    pub const NOT_INITIALIZED: i32 = 3;
    /// Also: a second explicit start while a daemon owns the project.
    pub const LOCK_CONFLICT: i32 = 4;
    pub const AGENT_ADAPTER: i32 = 5;
    pub const WORKSPACE: i32 = 6;
    /// No daemon, incompatible/busy daemon, wrong project or unknown request outcome.
    pub const NO_DAEMON: i32 = 7;
}

/// Safe error categories. Each maps to one exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    General,
    InvalidArguments,
    NotInitialized,
    LockConflict,
    AgentAdapter,
    Workspace,
    /// A task, agent, goal, team, message or other entity does not exist.
    NotFound,
    /// The state does not allow it: invalid transition, already running,
    /// pending tasks, or an authored document changed since its base digest.
    Conflict,
    /// The session's mode or the operation's arguments are not admitted for
    /// this caller (e.g. `task.cancel` from a read-only MCP session).
    NotPermitted,
    /// Admission queue or byte budget full; the operation did not run.
    Overloaded,
    /// The daemon is stopping and admits no new work.
    ShuttingDown,
    /// The operation is in the catalogue but this build does not implement it
    /// yet (PR 1 skeleton only).
    Unimplemented,
}

impl ErrorCode {
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::InvalidArguments | ErrorCode::NotPermitted => exit::INVALID_ARGUMENTS,
            ErrorCode::NotInitialized => exit::NOT_INITIALIZED,
            ErrorCode::LockConflict => exit::LOCK_CONFLICT,
            ErrorCode::AgentAdapter => exit::AGENT_ADAPTER,
            ErrorCode::Workspace => exit::WORKSPACE,
            ErrorCode::Overloaded | ErrorCode::ShuttingDown => exit::NO_DAEMON,
            ErrorCode::General
            | ErrorCode::NotFound
            | ErrorCode::Conflict
            | ErrorCode::Unimplemented => exit::GENERAL,
        }
    }
}

/// Whether the operation touched state. After partial storage work this is
/// `Unknown`: the daemon never implies a rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    NotExecuted,
    Completed,
    Unknown,
}

/// Longest error message on the wire.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 64 * 1024;

/// `err` of an [`crate::frame::ErrFrame`]. The message goes only to the
/// requesting client; daemon logs record `code`/`execution`, never `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: BoundedString<MAX_ERROR_MESSAGE_BYTES>,
    /// The second line of the CLI error block, as `DomainError::hint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<BoundedString<MAX_ERROR_MESSAGE_BYTES>>,
    pub exit_code: i32,
    pub execution: Execution,
}

impl ErrorBody {
    /// Over-long text is cut at a char boundary rather than failing the error.
    pub fn new(code: ErrorCode, message: &str, execution: Execution) -> Self {
        ErrorBody {
            code,
            message: bounded(message),
            hint: None,
            exit_code: code.exit_code(),
            execution,
        }
    }

    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(bounded(hint));
        self
    }

    /// Map a service error, keeping its message, hint and exit code.
    pub fn from_domain(error: &DomainError, execution: Execution) -> Self {
        let code = match error {
            DomainError::InvalidArguments(_) => ErrorCode::InvalidArguments,
            DomainError::NotInitialized => ErrorCode::NotInitialized,
            DomainError::LockConflict(_) => ErrorCode::LockConflict,
            DomainError::AgentAdapter { .. } => ErrorCode::AgentAdapter,
            DomainError::Workspace { .. } => ErrorCode::Workspace,
            DomainError::TaskNotFound(_)
            | DomainError::AgentNotFound(_)
            | DomainError::GoalNotFound(_)
            | DomainError::TeamNotFound(_)
            | DomainError::MessageNotFound(_) => ErrorCode::NotFound,
            DomainError::TaskAlreadyRunning { .. }
            | DomainError::InvalidTransition { .. }
            | DomainError::GoalHasPendingTasks { .. } => ErrorCode::Conflict,
            DomainError::NoAgents | DomainError::Other(_) => ErrorCode::General,
        };
        let mut body = ErrorBody::new(code, &error.to_string(), execution);
        // The variant decides the exit code, as it does in the CLI today.
        body.exit_code = error.exit_code();
        body.hint = error.hint().as_deref().map(bounded);
        body
    }

    /// The PR 1 skeleton's answer for an operation nobody implemented yet.
    pub fn unimplemented(op: &str) -> Self {
        ErrorBody::new(
            ErrorCode::Unimplemented,
            &format!("{op} is not implemented by this daemon build"),
            Execution::NotExecuted,
        )
    }
}

fn bounded(text: &str) -> BoundedString<MAX_ERROR_MESSAGE_BYTES> {
    let mut end = text.len().min(MAX_ERROR_MESSAGE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    BoundedString::new(&text[..end]).expect("cut to the bound above")
}

/// Why no compatible session could be opened. Detail strings are safe: fixed
/// categories plus escaped paths and version numbers, never `daemon.json`
/// contents or a peer's message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoDaemonDetail {
    /// No `daemon.json` and no explicit endpoint.
    NoDiscovery,
    /// `daemon.json` failed its type/owner/mode/size checks.
    UnsafeDiscovery,
    /// The socket path does not exist.
    NoSocket,
    /// The socket exists but nothing accepts on it.
    ConnectionRefused,
    /// Connect or handshake deadline expired.
    Timeout,
    /// The server's uid differs from ours, or credentials were unavailable.
    PermissionDenied,
    /// The handshake reply was malformed, or the daemon sent
    /// `handshake_failed`.
    HandshakeFailed,
    /// An established session lost its connection.
    ConnectionLost,
    /// A request may have reached the daemon, but no valid terminal reply arrived.
    RequestOutcomeUnknown,
    /// A frozen stop was accepted, but its completion could not be confirmed.
    StopOutcomeUnknown,
    /// The daemon is stopping.
    Stopping,
}

impl NoDaemonDetail {
    pub fn as_str(&self) -> &'static str {
        match self {
            NoDaemonDetail::NoDiscovery => "no daemon.json",
            NoDaemonDetail::UnsafeDiscovery => "daemon.json failed its safety checks",
            NoDaemonDetail::NoSocket => "no socket",
            NoDaemonDetail::ConnectionRefused => "connection refused",
            NoDaemonDetail::Timeout => "timed out",
            NoDaemonDetail::PermissionDenied => "permission denied",
            NoDaemonDetail::HandshakeFailed => "handshake failed",
            NoDaemonDetail::ConnectionLost => "connection lost",
            NoDaemonDetail::RequestOutcomeUnknown => "request outcome unknown",
            NoDaemonDetail::StopOutcomeUnknown => "daemon accepted stop; completion is unknown",
            NoDaemonDetail::Stopping => "daemon is stopping",
        }
    }
}

/// A client-side failure to reach a compatible daemon. Every variant exits 7
/// except [`ConnectFailure::NotInitialized`] (3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectFailure {
    NoDaemon {
        project_root: String,
        detail: NoDaemonDetail,
    },
    /// Versions differ. Only `orch daemon stop` (frozen stop intent) works.
    VersionSkew {
        project_root: String,
        skew: VersionSkew,
    },
    /// Capacity, not absence: no NoDaemon headline and no start hint.
    Busy {
        project_root: String,
        pid: Option<u32>,
        sessions: u32,
        cap: u32,
    },
    /// The endpoint's daemon owns a different project. No delete/kill hint.
    WrongProject { expected: String, actual: String },
    /// Stop was accepted but the owner did not finish within the completion deadline.
    StopTimeout { project_root: String, pid: u32 },
    /// No initialized project was found for this caller.
    NotInitialized,
}

/// The one start command named in every hint.
pub const START_COMMAND: &str = "orch daemon start";
/// The skew recovery procedure.
pub const RESTART_COMMANDS: &str = "orch daemon stop, then orch daemon start";

impl ConnectFailure {
    /// Construct with a raw path; control characters are escaped here.
    pub fn no_daemon(project_root: &str, detail: NoDaemonDetail) -> Self {
        ConnectFailure::NoDaemon {
            project_root: escape_controls(project_root),
            detail,
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            ConnectFailure::NotInitialized => exit::NOT_INITIALIZED,
            _ => exit::NO_DAEMON,
        }
    }

    /// The `<detail>` text in parentheses.
    pub fn detail(&self) -> String {
        match self {
            ConnectFailure::NoDaemon { detail, .. } => detail.as_str().to_owned(),
            ConnectFailure::VersionSkew { skew, .. } => format!(
                "daemon is orch {} protocol {}, this client is orch {} protocol {}",
                escape_controls(&skew.daemon_binary),
                skew.daemon_protocol,
                escape_controls(&skew.client_binary),
                skew.client_protocol
            ),
            ConnectFailure::Busy {
                pid, sessions, cap, ..
            } => format!(
                "orch daemon PID {} is running; {sessions}/{cap} sessions",
                pid.map_or_else(|| "unavailable".to_owned(), |p| p.to_string())
            ),
            ConnectFailure::WrongProject { expected, actual } => {
                format!("expected {expected}, daemon serves {actual}")
            }
            ConnectFailure::StopTimeout { pid, .. } => {
                format!("daemon accepted stop but PID {pid} is still running after 30 s")
            }
            ConnectFailure::NotInitialized => "not initialized".to_owned(),
        }
    }

    /// The complete two-line message, without a trailing newline.
    ///
    /// `agent_context` is true when `ORCH_TASK_ID` or `ORCH_AGENT_ID` is set,
    /// or for an MCP bridge with a resolved agent identity; it replaces every
    /// start/restart hint with an instruction to stop and report.
    pub fn render(&self, agent_context: bool) -> String {
        match self {
            ConnectFailure::StopTimeout { project_root, .. }
            | ConnectFailure::NoDaemon {
                project_root,
                detail: NoDaemonDetail::RequestOutcomeUnknown | NoDaemonDetail::StopOutcomeUnknown,
            } => {
                let agent_hint = if agent_context {
                    " Report this to the owner and stop."
                } else {
                    ""
                };
                format!(
                    "  ✕ Cannot confirm the orch request outcome for {project_root} ({})\n    the request may have been applied; inspect state before retrying.{agent_hint}",
                    self.detail()
                )
            }
            ConnectFailure::NotInitialized => {
                let error = DomainError::NotInitialized;
                format!("  ✕ {error}\n    {}", error.hint().unwrap_or_default())
            }
            ConnectFailure::Busy { project_root, .. } => format!(
                "  ✕ daemon_busy for {project_root} ({})\n    Retry when a session closes.",
                self.detail()
            ),
            ConnectFailure::WrongProject { .. } => format!(
                "  ✕ wrong_project ({})\n    Check ORCH_SOCKET and ORCH_PROJECT_ROOT.",
                self.detail()
            ),
            ConnectFailure::NoDaemon { project_root, .. }
            | ConnectFailure::VersionSkew { project_root, .. } => {
                if agent_context {
                    return format!(
                        "  ✕ The orch daemon for {project_root} is not running. Do not start it; \
                         report this to the owner and stop.\n    Detail: {}",
                        self.detail()
                    );
                }
                let hint = match self {
                    ConnectFailure::VersionSkew { .. } => RESTART_COMMANDS,
                    _ => START_COMMAND,
                };
                format!(
                    "  ✕ No orch daemon is running for {project_root} ({})\n    Start it with: {hint}",
                    self.detail()
                )
            }
        }
    }
}

/// Text for the loser of a start race (exit 4). `pid` is `None` when the
/// winner died before publishing it: never an invented PID.
pub fn already_running(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("orch daemon already running (PID {pid})"),
        None => "orch daemon already running (PID unavailable)".to_owned(),
    }
}

/// Text for an explicit start refused in an agent context (exit 2).
pub const START_REFUSED_IN_AGENT_CONTEXT: &str = "Refusing to start the orch daemon from an \
     agent context (ORCH_TASK_ID or ORCH_AGENT_ID is set). The owner starts the daemon; \
     pass --from-agent only if the owner deliberately intends this.";

#[cfg(test)]
mod tests {
    use super::*;

    fn skew() -> VersionSkew {
        VersionSkew {
            client_protocol: 2,
            client_binary: "0.15.0".into(),
            daemon_protocol: 1,
            daemon_binary: "0.14.0".into(),
        }
    }

    #[test]
    fn no_daemon_text_is_the_central_wording() {
        let failure = ConnectFailure::no_daemon("/p", NoDaemonDetail::NoSocket);
        assert_eq!(
            failure.render(false),
            "  ✕ No orch daemon is running for /p (no socket)\n    Start it with: orch daemon start"
        );
        assert_eq!(failure.exit_code(), 7);
    }

    #[test]
    fn skew_names_both_versions_and_the_restart_procedure() {
        let text = ConnectFailure::VersionSkew {
            project_root: "/p".into(),
            skew: skew(),
        }
        .render(false);
        assert!(text.contains("orch 0.14.0 protocol 1"), "{text}");
        assert!(text.contains("orch 0.15.0 protocol 2"), "{text}");
        assert!(text.ends_with("Start it with: orch daemon stop, then orch daemon start"));
    }

    #[test]
    fn agent_context_never_offers_a_start_hint() {
        for failure in [
            ConnectFailure::no_daemon("/p", NoDaemonDetail::ConnectionRefused),
            ConnectFailure::VersionSkew {
                project_root: "/p".into(),
                skew: skew(),
            },
        ] {
            let text = failure.render(true);
            assert!(text.contains("Do not start it"), "{text}");
            assert!(!text.contains("Start it with"), "{text}");
        }
    }

    #[test]
    fn busy_is_not_absence() {
        let failure = ConnectFailure::Busy {
            project_root: "/p".into(),
            pid: None,
            sessions: 64,
            cap: 64,
        };
        let text = failure.render(false);
        assert!(text.contains("orch daemon PID unavailable is running; 64/64 sessions"));
        assert!(!text.contains("No orch daemon"));
        assert!(!text.contains(START_COMMAND));
        assert_eq!(failure.exit_code(), 7);
    }

    #[test]
    fn paths_are_escaped() {
        let text =
            ConnectFailure::no_daemon("/p\u{1b}]0;x", NoDaemonDetail::NoSocket).render(false);
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn domain_errors_keep_exit_code_and_hint() {
        let body =
            ErrorBody::from_domain(&DomainError::LockConflict(Some(9)), Execution::NotExecuted);
        assert_eq!(body.code, ErrorCode::LockConflict);
        assert_eq!(body.exit_code, 4);
        assert_eq!(body.hint.as_deref(), Some("Use: orch status"));
    }
}
