//! Local daemon control plane. No store fallback and no lifecycle side effects.
//! Production connections verify kernel credentials before sending any bytes.
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

mod discovery;
pub use discovery::Project;
pub use orch_proto::error::ConnectFailure as NoDaemon;

use orch_proto::error::{ErrorBody, NoDaemonDetail};
use orch_proto::frame::{MAX_FRAME_BYTES, RequestFrame, RequestId, ServerFrame};
use orch_proto::handshake::{
    self, Caller, CallerKind, HandshakeReply, Hello, HelloFrame, Intent, Reply, SessionMode,
    StopState, VersionSkew, Welcome,
};
use orch_proto::ndjson::{self, FrameReader};
use orch_proto::ops::{Request, args, results};
use orch_proto::path::WirePath;
use serde::de::DeserializeOwned;
use std::num::NonZeroU64;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
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
    failed: bool,
}

impl Client {
    pub async fn connect(config: &ClientConfig) -> Result<Self, ClientError> {
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
            .map_err(|_| project.failure(NoDaemonDetail::HandshakeFailed))?;
            read_reply(&mut io, &project, pid).await
        })
        .await
        .map_err(|_| project.failure(NoDaemonDetail::Timeout))??;
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
            return Err(project.failure(NoDaemonDetail::HandshakeFailed).into());
        }
        Ok(Self {
            io,
            writer,
            project,
            welcome,
            next_id: 1,
            failed: false,
        })
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
            .map_err(|_| self.project.failure(NoDaemonDetail::Timeout))?
    }

    /// Generic terminal response plumbing for closed D7 requests. Streaming
    /// and subscription APIs are added with their consumers in PR 2/3.
    pub async fn call<T: DeserializeOwned>(&mut self, req: Request) -> Result<T, ClientError> {
        if self.failed {
            return Err(self.project.failure(NoDaemonDetail::ConnectionLost).into());
        }
        // Reserve the id and poison the lane before the first await: cancelling
        // this future must never let a later call consume its unknown reply.
        let id = NonZeroU64::new(self.next_id)
            .ok_or_else(|| self.project.failure(NoDaemonDetail::ConnectionLost))?;
        self.next_id = self.next_id.checked_add(1).unwrap_or(0);
        self.failed = true;
        let frame = RequestFrame {
            id: RequestId(id),
            req,
        };
        ndjson::write_frame(&mut self.writer, &frame, MAX_FRAME_BYTES)
            .await
            .map_err(|_| self.project.failure(NoDaemonDetail::ConnectionLost))?;
        let bytes = self
            .io
            .read_line(MAX_FRAME_BYTES)
            .await
            .map_err(|_| self.project.failure(NoDaemonDetail::ConnectionLost))?
            .ok_or_else(|| self.project.failure(NoDaemonDetail::ConnectionLost))?;
        ndjson::check_depth(&bytes, orch_proto::frame::MAX_DEPTH)
            .map_err(|_| self.project.failure(NoDaemonDetail::ConnectionLost))?;
        let frame = ServerFrame::decode(&bytes)
            .map_err(|_| self.project.failure(NoDaemonDetail::ConnectionLost))?;
        match frame {
            ServerFrame::Ok(frame) if frame.id.0 == id => {
                let result = ndjson::decode(frame.ok.get().as_bytes())
                    .map_err(|_| self.project.failure(NoDaemonDetail::ConnectionLost))?;
                self.failed = false;
                Ok(result)
            }
            ServerFrame::Err(frame) if frame.id.0 == id => {
                self.failed = false;
                Err(ClientError::Remote(frame.err))
            }
            _ => Err(self.project.failure(NoDaemonDetail::ConnectionLost).into()),
        }
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
    let completed = tokio::time::timeout(
        handshake::STOP_COMPLETION_TIMEOUT,
        read_reply(&mut client.io, &client.project, pid),
    )
    .await
    .map_err(|_| client.project.failure(NoDaemonDetail::Timeout))??;
    if completed.instance != client.welcome.instance
        || completed.stop_state != Some(StopState::Completed)
    {
        return Err(client
            .project
            .failure(NoDaemonDetail::HandshakeFailed)
            .into());
    }
    Ok(results::StopOutcome { clean: true })
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
    let cred = stream
        .peer_cred()
        .map_err(|_| project.failure(NoDaemonDetail::PermissionDenied))?;
    if cred.uid() != expected_uid {
        return Err(project.failure(NoDaemonDetail::PermissionDenied).into());
    }
    Ok(cred.pid().and_then(|p| u32::try_from(p).ok()))
}

async fn read_reply(
    io: &mut Reader,
    project: &Project,
    pid: Option<u32>,
) -> Result<Welcome, ClientError> {
    let reply: HandshakeReply = io
        .read(handshake::MAX_HANDSHAKE_BYTES)
        .await
        .map_err(|_| project.failure(NoDaemonDetail::HandshakeFailed))?
        .ok_or_else(|| project.failure(NoDaemonDetail::HandshakeFailed))?;
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
}
