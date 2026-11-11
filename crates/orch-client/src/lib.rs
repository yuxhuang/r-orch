//! The daemon client every frontend uses: discovery, the connect state
//! machine (design D5), the typed operation surface (D7, in `api`) and the
//! frozen stop. No store fallback and no lifecycle side effects: nothing here
//! starts, forks or executes a process (R4, enforced by `cargo xtask
//! crate-graph`). Production connections verify kernel credentials before
//! sending any bytes.
//!
//! ```ignore
//! let config = ClientConfig::from_env(cwd)?.with_agent(cli.agent.as_deref())?;
//! let mut client = match Client::connect(&config).await {
//!     Ok(client) => client,
//!     Err(error) => {
//!         eprintln!("{}", error.render(config.agent_context));
//!         return Err(error.exit_code()); // 7 for every NoDaemon form
//!     }
//! };
//! let task = client.task_get(args::TaskGetArgs { id: Id::new("tsk_1")? }).await?;
//! ```
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

mod api;
mod call;
mod discovery;
pub use call::{Call, MAX_STREAMED_RESULT_BYTES, Progress};
pub use discovery::Project;
pub use orch_proto::error::ConnectFailure as NoDaemon;

use orch_proto::error::{ErrorBody, NoDaemonDetail};
use orch_proto::frame::limits;
use orch_proto::handshake::{
    self, Caller, CallerKind, HandshakeReply, Hello, HelloFrame, Intent, Reply, SessionMode,
    StopState, VersionSkew, Welcome,
};
use orch_proto::ndjson::{self, FrameReader};
use orch_proto::ops::{Request, args, results};
use orch_proto::path::WirePath;
use serde::de::DeserializeOwned;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, BufReader, ReadHalf, WriteHalf};

use tokio::net::UnixStream;

type Reader = FrameReader<BufReader<ReadHalf<Box<dyn Transport>>>>;

/// Safe, frontend-neutral failures; connection text is owned by orch-proto.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{}", .0.render(false))]
    Connection(NoDaemon),
    #[error("{0}")]
    InvalidArguments(String),
    #[error("{}", orch_proto::path::escape_controls(.0.message.as_str()))]
    Remote(ErrorBody),
}

impl From<NoDaemon> for ClientError {
    fn from(value: NoDaemon) -> Self {
        Self::Connection(value)
    }
}

impl ClientError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Connection(e) => e.exit_code(),
            Self::InvalidArguments(_) => 2,
            Self::Remote(e) => match e.exit_code {
                1..=7 => e.exit_code,
                _ => 1,
            },
        }
    }
    pub fn render(&self, agent_context: bool) -> String {
        match self {
            Self::Connection(e) => e.render(agent_context),
            Self::Remote(e) => match &e.hint {
                Some(hint) => format!(
                    "  ✕ {self}\n    {}",
                    orch_proto::path::escape_controls(hint)
                ),
                None => format!("  ✕ {self}"),
            },
            _ => format!("  ✕ {self}"),
        }
    }
}

/// Immutable caller routing inputs, captured once. Tests pass these directly
/// rather than changing process-global environment variables.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub test_socket_root: Option<PathBuf>,
    pub caller: Caller,
    pub mode: SessionMode,
    pub agent_context: bool,
}

impl ClientConfig {
    pub fn from_env(cwd: PathBuf) -> Result<Self, ClientError> {
        let agent_context = std::env::var_os("ORCH_TASK_ID").is_some()
            || std::env::var_os("ORCH_AGENT_ID").is_some();
        let agent_id = std::env::var("ORCH_AGENT_ID")
            .ok()
            .map(orch_proto::bounded::Id::new)
            .transpose()
            .map_err(|_| ClientError::InvalidArguments("invalid ORCH_AGENT_ID".into()))?;
        Ok(Self {
            cwd,
            project_root: std::env::var_os("ORCH_PROJECT_ROOT").map(PathBuf::from),
            socket: std::env::var_os("ORCH_SOCKET").map(PathBuf::from),
            test_socket_root: std::env::var_os("ORCH_TEST_SOCKET_ROOT").map(PathBuf::from),
            caller: Caller {
                kind: if agent_context {
                    CallerKind::Agent
                } else {
                    CallerKind::Cli
                },
                agent_id,
                pid: None,
            },
            mode: SessionMode::Normal,
            agent_context,
        })
    }
    pub fn project(&self) -> Result<Project, ClientError> {
        Project::discover(&self.cwd, self.project_root.as_deref())
    }

    /// R8: an explicit `--agent` wins over `ORCH_AGENT_ID`; `None` keeps the
    /// environment's identity. Identity is fixed for the session's lifetime.
    pub fn with_agent(mut self, flag: Option<&str>) -> Result<Self, ClientError> {
        if let Some(flag) = flag {
            let id = orch_proto::bounded::Id::new(flag)
                .map_err(|_| ClientError::InvalidArguments("invalid --agent".into()))?;
            self.caller.agent_id = Some(id);
        }
        let kind = self.caller.kind;
        Ok(self.with_frontend(kind))
    }

    /// The frontend opening the session. The daemon admits an MCP session
    /// only to the MCP surface, so the bridge must say so. An MCP bridge with
    /// a resolved agent identity renders failures in the agent variant.
    pub fn with_frontend(mut self, kind: CallerKind) -> Self {
        self.caller.kind = kind;
        if kind == CallerKind::Mcp && self.caller.agent_id.is_some() {
            self.agent_context = true;
        }
        self
    }

    /// Session admission mode; only MCP uses anything but `Normal`.
    pub fn with_mode(mut self, mode: SessionMode) -> Self {
        self.mode = mode;
        self
    }
}

/// Injectable byte stream for an explicitly constructed test transport. Real
/// socket callers must use `connect`, which authenticates the peer first.
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Transport for T {}

/// One serialized request lane. Exclusive `&mut self` calls mean at most one
/// outstanding id, so bounded reads and response correlation need no queue.
/// A failed lane is closed permanently; no mutation is ever replayed.
pub struct Client {
    io: Reader,
    writer: WriteHalf<Box<dyn Transport>>,
    project: Project,
    pub welcome: Welcome,
    next_id: u64,
    last_commit_seq: Option<u64>,
    // Set before writing: cancellation or a partial write leaves the outcome
    // unknown and the lane poisoned until a correlated terminal reply arrives.
    request_may_have_been_sent: bool,
}

/// How long `connect` keeps retrying a daemon that answers busy before it
/// returns `daemon_busy`: the spec's admission deadline. One budget covers
/// every attempt; a slow or absent daemon is never retried.
pub const BUSY_WAIT: Duration = limits::ADMISSION_TIMEOUT;
const BUSY_BACKOFF_FIRST: Duration = Duration::from_millis(25);
const BUSY_BACKOFF_CAP: Duration = Duration::from_millis(500);

impl Client {
    /// Busy (session cap, or the pre-handshake budget full and the connection
    /// dropped before any reply) is decided before a request frame exists, so
    /// redialling cannot apply anything twice. Every other failure returns at
    /// once, with the latency it had before.
    pub async fn connect(config: &ClientConfig) -> Result<Self, ClientError> {
        let deadline = tokio::time::Instant::now() + BUSY_WAIT;
        let mut backoff = BUSY_BACKOFF_FIRST;
        loop {
            let busy = match Self::connect_once(config).await {
                Err(busy @ ClientError::Connection(NoDaemon::Busy { .. })) => busy,
                other => return other,
            };
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Err(busy);
            }
            tokio::time::sleep(jittered(backoff).min(left)).await;
            backoff = (backoff * 2).min(BUSY_BACKOFF_CAP);
        }
    }

    async fn connect_once(config: &ClientConfig) -> Result<Self, ClientError> {
        let (project, stream, pid) = dial(config).await?;
        Self::handshake(
            project,
            Box::new(stream),
            pid,
            &config.caller,
            config.mode,
            Intent::Session,
        )
        .await
    }

    /// Harness seam: the creator authenticates this private transport. It
    /// still passes the real bounded handshake, project and version checks.
    pub async fn from_stream(
        project: Project,
        stream: impl Transport + 'static,
        caller: Caller,
        mode: SessionMode,
    ) -> Result<Self, ClientError> {
        Self::handshake(
            project,
            Box::new(stream),
            None,
            &caller,
            mode,
            Intent::Session,
        )
        .await
    }

    async fn handshake(
        project: Project,
        stream: Box<dyn Transport>,
        pid: Option<u32>,
        caller: &Caller,
        mode: SessionMode,
        intent: Intent,
    ) -> Result<Self, ClientError> {
        let path = WirePath::new(project.root.clone()).map_err(|_| {
            ClientError::InvalidArguments("project path exceeds the protocol limit".into())
        })?;
        let hello = if intent == Intent::Stop {
            Hello::stop(path, project.id)
        } else {
            Hello::session(path, project.id, caller, mode)
        };
        let (reader, mut writer) = tokio::io::split(stream);
        let mut io = FrameReader::new(BufReader::new(reader));
        let welcome = tokio::time::timeout(handshake::HANDSHAKE_TIMEOUT, async {
            ndjson::write_frame(
                &mut writer,
                &HelloFrame { hello },
                handshake::MAX_HANDSHAKE_BYTES,
            )
            .await
            .map_err(|_| -> ClientError {
                if intent == Intent::Stop {
                    project
                        .failure(NoDaemonDetail::RequestOutcomeUnknown)
                        .into()
                } else {
                    closed_before_reply(&project, pid)
                }
            })?;
            read_reply(&mut io, &project, pid, intent)
                .await
                .map_err(|error| {
                    if intent == Intent::Stop {
                        unknown_outcome(error, &project, NoDaemonDetail::RequestOutcomeUnknown)
                    } else {
                        error
                    }
                })
        })
        .await
        .map_err(|_| {
            project.failure(if intent == Intent::Stop {
                NoDaemonDetail::RequestOutcomeUnknown
            } else {
                NoDaemonDetail::Timeout
            })
        })??;
        if intent == Intent::Session {
            if welcome.protocol != orch_proto::LOCAL_PROTOCOL
                || welcome.binary.as_str() != orch_proto::BINARY_VERSION
            {
                return Err(NoDaemon::VersionSkew {
                    project_root: orch_proto::path::escape_controls(
                        &project.root.to_string_lossy(),
                    ),
                    skew: VersionSkew {
                        client_protocol: orch_proto::LOCAL_PROTOCOL,
                        client_binary: orch_proto::BINARY_VERSION.into(),
                        daemon_protocol: welcome.protocol,
                        daemon_binary: welcome.binary.to_string(),
                    },
                }
                .into());
            }
            if !welcome.compatible || welcome.stop_state.is_some() {
                return Err(project.failure(NoDaemonDetail::HandshakeFailed).into());
            }
        } else if welcome.stop_state != Some(StopState::Accepted) {
            return Err(project
                .failure(NoDaemonDetail::RequestOutcomeUnknown)
                .into());
        }
        Ok(Self {
            io,
            writer,
            project,
            welcome,
            next_id: 1,
            last_commit_seq: None,
            request_may_have_been_sent: false,
        })
    }

    /// The project this session is bound to.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The daemon-instance-scoped commit ordinal of the last completed
    /// request: the one assigned to a mutation or observed by a read.
    pub fn last_commit_seq(&self) -> Option<u64> {
        self.last_commit_seq
    }

    pub async fn status(&mut self) -> Result<results::DaemonStatus, ClientError> {
        self.control_call(Request::DaemonStatus(args::DaemonStatusArgs {}))
            .await
    }
    pub async fn dispatch(
        &mut self,
        dispatch: args::DispatchSwitch,
    ) -> Result<results::DispatchState, ClientError> {
        self.control_call(Request::DispatchSet(args::DispatchSetArgs { dispatch }))
            .await
    }
    /// Compatible-session stop. CLI stop uses `stop` below so recovery also
    /// works when a daemon advertises a different protocol or binary.
    pub async fn daemon_stop(&mut self) -> Result<results::StopOutcome, ClientError> {
        self.control_call(Request::DaemonStop(args::DaemonStopArgs {}))
            .await
    }

    // Control calls have bounded completion; later long-running D7 operations
    // use the generic lane and their own explicit observation/cancel contract.
    async fn control_call<T: DeserializeOwned>(&mut self, req: Request) -> Result<T, ClientError> {
        tokio::time::timeout(handshake::STOP_COMPLETION_TIMEOUT, self.call(req))
            .await
            .map_err(|_| self.project.failure(NoDaemonDetail::RequestOutcomeUnknown))?
    }
}

/// Frozen stop intent: versions are reported but never prevent shutdown.
pub async fn stop(config: &ClientConfig) -> Result<results::StopOutcome, ClientError> {
    let (project, stream, pid) = dial(config).await?;
    let mut client = Client::handshake(
        project,
        Box::new(stream),
        pid,
        &config.caller,
        config.mode,
        Intent::Stop,
    )
    .await?;
    // One deadline covers both the terminal frame and release of ownership.
    let deadline = tokio::time::Instant::now() + handshake::STOP_COMPLETION_TIMEOUT;
    let owner_pid = client.welcome.pid;
    let completed = tokio::time::timeout_at(
        deadline,
        read_reply(&mut client.io, &client.project, pid, Intent::Stop),
    )
    .await
    .map_err(|_| stop_timeout(&client.project, owner_pid))?
    .map_err(|error| unknown_outcome(error, &client.project, NoDaemonDetail::StopOutcomeUnknown))?;
    if completed.instance != client.welcome.instance
        || completed.pid != owner_pid
        || completed.stop_state != Some(StopState::Completed)
    {
        return Err(client
            .project
            .failure(NoDaemonDetail::StopOutcomeUnknown)
            .into());
    }
    let project = client.project;
    // Closing both halves lets the daemon drain this session before it drops
    // the owner guard. Waiting with the lane open would delay shutdown itself.
    drop(client.io);
    drop(client.writer);
    wait_for_owner_release(project, owner_pid, deadline).await?;
    Ok(results::StopOutcome { clean: true })
}

/// Equal jitter: half of `delay` plus a random share of the other half, so a
/// burst of clients turned away together does not redial in lockstep.
fn jittered(delay: Duration) -> Duration {
    use std::hash::BuildHasher;
    let half = delay / 2;
    let span = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX).max(1);
    let random = std::collections::hash_map::RandomState::new().hash_one(std::time::Instant::now());
    half + Duration::from_nanos(random % span)
}

fn unknown_outcome(error: ClientError, project: &Project, detail: NoDaemonDetail) -> ClientError {
    match error {
        ClientError::Connection(NoDaemon::NoDaemon { .. }) => project.failure(detail).into(),
        // Explicit rejections preserve their definite execution classification.
        other => other,
    }
}

fn stop_timeout(project: &Project, pid: u32) -> ClientError {
    NoDaemon::StopTimeout {
        project_root: orch_proto::path::escape_controls(&project.root.to_string_lossy()),
        pid,
    }
    .into()
}

async fn wait_for_owner_release(
    mut project: Project,
    pid: u32,
    deadline: tokio::time::Instant,
) -> Result<(), ClientError> {
    let timeout = stop_timeout(&project, pid);
    tokio::time::timeout_at(deadline, async {
        loop {
            // Nonblocking flock and PID inspection still use filesystem/syscall
            // APIs; keep them off Tokio's executor threads.
            let probe_error = project.failure(NoDaemonDetail::StopOutcomeUnknown);
            let (returned, released) = tokio::task::spawn_blocking(move || {
                let released = project.owner_released(pid);
                (project, released)
            })
            .await
            .map_err(|_| probe_error)?;
            project = returned;
            if released {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| timeout)?
}

async fn dial(config: &ClientConfig) -> Result<(Project, UnixStream, Option<u32>), ClientError> {
    let project = config.project()?;
    let socket = match &config.socket {
        Some(path) => path.clone(),
        None => project.read_record()?.socket.into_path_buf(),
    };
    discovery::validate_socket(&socket, config.test_socket_root.as_deref())?;
    let meta = std::fs::symlink_metadata(&socket).map_err(|e| {
        project.failure(if e.kind() == std::io::ErrorKind::NotFound {
            NoDaemonDetail::NoSocket
        } else {
            NoDaemonDetail::PermissionDenied
        })
    })?;
    if !meta.file_type().is_socket()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(project.failure(NoDaemonDetail::PermissionDenied).into());
    }
    let stream = tokio::time::timeout(handshake::CONNECT_TIMEOUT, UnixStream::connect(&socket))
        .await
        .map_err(|_| project.failure(NoDaemonDetail::Timeout))?
        .map_err(|e| {
            project.failure(match e.kind() {
                std::io::ErrorKind::ConnectionRefused => NoDaemonDetail::ConnectionRefused,
                std::io::ErrorKind::NotFound => NoDaemonDetail::NoSocket,
                _ => NoDaemonDetail::PermissionDenied,
            })
        })?;
    let pid = authenticate_peer(&stream, &project, nix::unistd::geteuid().as_raw())?;
    Ok((project, stream, pid))
}

fn authenticate_peer(
    stream: &UnixStream,
    project: &Project,
    expected_uid: u32,
) -> Result<Option<u32>, ClientError> {
    check_peer(stream.peer_cred(), project, expected_uid)
}

fn check_peer(
    cred: std::io::Result<tokio::net::unix::UCred>,
    project: &Project,
    expected_uid: u32,
) -> Result<Option<u32>, ClientError> {
    // macOS answers LOCAL_PEEREPID with ENOTCONN once the accepted peer has
    // closed. Nothing has been sent, and absence is already ruled out.
    let cred = cred.map_err(|_| closed_before_reply(project, None))?;
    if cred.uid() != expected_uid {
        return Err(project.failure(NoDaemonDetail::PermissionDenied).into());
    }
    Ok(cred.pid().and_then(|p| u32::try_from(p).ok()))
}

/// `connect` succeeded, so a listener was alive: a peer that closes before any
/// handshake reply is capacity, never absence. The daemon drops connections
/// beyond its pre-handshake budget without replying (SE-6), so the session
/// count is unknown here.
fn closed_before_reply(project: &Project, pid: Option<u32>) -> ClientError {
    NoDaemon::Busy {
        project_root: orch_proto::path::escape_controls(&project.root.to_string_lossy()),
        pid,
        sessions: 0,
        cap: 0,
    }
    .into()
}

async fn read_reply(
    io: &mut Reader,
    project: &Project,
    pid: Option<u32>,
    intent: Intent,
) -> Result<Welcome, ClientError> {
    let reply: HandshakeReply = match io.read(handshake::MAX_HANDSHAKE_BYTES).await {
        Ok(Some(reply)) => reply,
        // A stop hello may already have been acted on; its callers classify
        // this as an unknown outcome instead.
        Ok(None) | Err(ndjson::CodecError::Io(_)) if intent == Intent::Session => {
            return Err(closed_before_reply(project, pid));
        }
        Ok(None) | Err(_) => return Err(project.failure(NoDaemonDetail::HandshakeFailed).into()),
    };
    match reply.into_reply() {
        Some(Reply::Welcome(welcome)) => {
            if welcome.project_id != project.id {
                return Err(project.wrong_project(&welcome.project_root).into());
            }
            Ok(welcome)
        }
        Some(Reply::Reject(reject)) => {
            use handshake::RejectCode;
            Err(match reject.code {
                RejectCode::Busy => NoDaemon::Busy {
                    project_root: orch_proto::path::escape_controls(
                        &project.root.to_string_lossy(),
                    ),
                    pid,
                    sessions: reject.sessions.unwrap_or(0),
                    cap: reject.cap.unwrap_or(0),
                }
                .into(),
                RejectCode::WrongProject => project
                    .wrong_project(
                        &reject
                            .project_root
                            .ok_or_else(|| project.failure(NoDaemonDetail::HandshakeFailed))?,
                    )
                    .into(),
                RejectCode::StopFailed => ClientError::Remote(ErrorBody::new(
                    orch_proto::error::ErrorCode::General,
                    "daemon shutdown cleanup failed; recovery evidence was retained",
                    orch_proto::error::Execution::Unknown,
                )),
                _ => project.failure(NoDaemonDetail::HandshakeFailed).into(),
            })
        }
        None => Err(project.failure(NoDaemonDetail::HandshakeFailed).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kernel_uid_mismatch_is_rejected_before_any_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".orchestry")).unwrap();
        let project = Project::discover(root.path(), None).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        assert!(authenticate_peer(&client, &project, uid).is_ok());
        let error = authenticate_peer(&client, &project, uid.wrapping_add(1)).unwrap_err();
        assert_eq!(error.exit_code(), 7);
        assert!(error.to_string().contains("permission denied"));
        let mut byte = [0];
        assert_eq!(
            server.try_read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn lost_peer_credential_after_connect_is_busy_not_absence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".orchestry")).unwrap();
        let project = Project::discover(root.path(), None).unwrap();
        let uid = nix::unistd::geteuid().as_raw();
        let lost = std::io::Error::from(std::io::ErrorKind::NotConnected);
        let error = check_peer(Err(lost), &project, uid).unwrap_err();
        assert_eq!(error.exit_code(), 7);
        for agent_context in [false, true] {
            let text = error.render(agent_context);
            assert!(text.contains("daemon_busy"), "{text}");
            assert!(text.contains("PID unavailable"), "{text}");
            assert!(!text.contains("No orch daemon is running"), "{text}");
            assert!(!text.contains("permission denied"), "{text}");
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/tripwire.rs"]
mod process_tripwire;
