//! The **frozen** handshake (design D5/D6, spec R6).
//!
//! Every field and enum value in this module is frozen from
//! [`LOCAL_PROTOCOL`](crate::LOCAL_PROTOCOL) 1: later protocols may add fields
//! but never rename, retype or remove these. That is what lets a client of any
//! version read a daemon's versions and send it a stop intent.
//!
//! Unlike every other frame, the envelopes here **tolerate unknown fields**
//! (but still reject duplicate known fields and invalid types). The session
//! fields of a hello (`caller`, `mode`) are carried undecoded and only parsed
//! by [`Hello::session_identity`] after versions are known to be equal, so a future
//! caller kind degrades to a version-skew message, not `handshake_failed`.
//!
//! Order on a fresh connection: the client checks the server uid, writes one
//! [`HelloFrame`] (≤ [`MAX_HANDSHAKE_BYTES`], within [`HANDSHAKE_TIMEOUT`]);
//! the daemon checks peer uid and [`ProjectId`], then answers with one
//! [`HandshakeReply`]: a welcome (compatible or not) or a reject.

use crate::bounded::{BoundedString, Id, MAX_ID_BYTES};
use crate::path::WirePath;
use crate::{BINARY_VERSION, LOCAL_PROTOCOL};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::time::Duration;

/// Largest hello, welcome or reject frame, excluding the LF.
pub const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
/// Client connect deadline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// Deadline for the whole hello → welcome/reject exchange, on both sides.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a stop client waits for `stop_state: completed` after `accepted`.
/// Expiry is an unknown outcome; it never triggers a kill, start or replay.
pub const STOP_COMPLETION_TIMEOUT: Duration = Duration::from_secs(30);

/// Identity of a project: `(st_dev, st_ino)` of `<root>/.orchestry`, taken
/// with `fstat` on an `O_DIRECTORY | O_NOFOLLOW` descriptor (spec R33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId {
    pub dev: u64,
    pub ino: u64,
}

/// What a connection is for. Frozen values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// An ordinary request session; requires equal versions.
    #[default]
    Session,
    /// Clean shutdown of the daemon. Crosses version skew after the uid and
    /// project checks (design "Frozen stop intent").
    Stop,
    /// A value from a newer client. The daemon rejects it with
    /// [`RejectCode::HandshakeFailed`] after reporting its versions.
    #[serde(other)]
    Unknown,
}

/// `{"hello": {...}}`, the first client frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloFrame {
    pub hello: Hello,
}

/// The frozen client hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub binary: BoundedString<MAX_ID_BYTES>,
    /// The project the client expects, for display and wrong-project text.
    pub project_root: WirePath,
    /// The project the client expects; this, not the path, is compared.
    pub project_id: ProjectId,
    #[serde(default)]
    pub intent: Intent,
    /// [`Caller`], decoded by [`Hello::session_identity`] only once versions match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caller: Option<Box<RawValue>>,
    /// [`SessionMode`], decoded by [`Hello::session_identity`] only once versions match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<Box<RawValue>>,
}

/// Who opened a session. Fixed for the session's lifetime; requests carry no
/// per-request caller. Identity is attribution, not authentication (A4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    pub kind: CallerKind,
    /// Resolved client-side: `--agent` over `ORCH_AGENT_ID` over per-tool
    /// defaults. `None` when the frontend has no identity.
    #[serde(default)]
    pub agent_id: Option<Id>,
    /// Claimed by the client and never trusted; the daemon uses the kernel
    /// peer pid where one is available.
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallerKind {
    Cli,
    Tui,
    Web,
    Mcp,
    Agent,
}

/// The admission mode of a session (design "Authoritative operation classes
/// and modes"). Only MCP sessions use anything but `Normal`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Normal,
    ReadOnly,
    AllowDestructive,
}

/// The decoded session identity of a compatible hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub caller: Caller,
    pub mode: SessionMode,
}

/// Versions of both ends, for the skew text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSkew {
    pub client_protocol: u32,
    pub client_binary: String,
    pub daemon_protocol: u32,
    pub daemon_binary: String,
}

/// A hello that cannot open a session.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    #[error("session hello has no caller")]
    MissingCaller,
    #[error("session hello has an invalid caller or mode")]
    InvalidSession,
}

impl Hello {
    /// A session hello from this build.
    pub fn session(
        project_root: WirePath,
        project_id: ProjectId,
        caller: &Caller,
        mode: SessionMode,
    ) -> Self {
        Hello {
            protocol: LOCAL_PROTOCOL,
            binary: this_binary(),
            project_root,
            project_id,
            intent: Intent::Session,
            caller: serde_json::value::to_raw_value(caller).ok(),
            mode: serde_json::value::to_raw_value(&mode).ok(),
        }
    }

    /// A stop hello from this build. Carries no session fields.
    pub fn stop(project_root: WirePath, project_id: ProjectId) -> Self {
        Hello {
            protocol: LOCAL_PROTOCOL,
            binary: this_binary(),
            project_root,
            project_id,
            intent: Intent::Stop,
            caller: None,
            mode: None,
        }
    }

    /// `Err` when this hello's versions differ from this build's, in either
    /// direction. Call before [`Hello::session_identity`].
    pub fn check_versions(&self) -> Result<(), VersionSkew> {
        if self.protocol == LOCAL_PROTOCOL && self.binary.as_str() == BINARY_VERSION {
            return Ok(());
        }
        Err(VersionSkew {
            client_protocol: self.protocol,
            client_binary: self.binary.to_string(),
            daemon_protocol: LOCAL_PROTOCOL,
            daemon_binary: BINARY_VERSION.to_owned(),
        })
    }

    /// Strictly decode the session fields. Only meaningful once
    /// [`Hello::check_versions`] passed: an equal-version client uses exactly
    /// this schema, so anything unknown here is a malformed hello.
    pub fn session_identity(&self) -> Result<Session, HandshakeError> {
        let caller = self.caller.as_ref().ok_or(HandshakeError::MissingCaller)?;
        let caller: Caller =
            serde_json::from_str(caller.get()).map_err(|_| HandshakeError::InvalidSession)?;
        let mode = match &self.mode {
            Some(mode) => {
                serde_json::from_str(mode.get()).map_err(|_| HandshakeError::InvalidSession)?
            }
            None => SessionMode::Normal,
        };
        Ok(Session { caller, mode })
    }
}

fn this_binary() -> BoundedString<MAX_ID_BYTES> {
    BoundedString::new(BINARY_VERSION).expect("the package version is far below 256 bytes")
}

/// Progress of a stop intent, sent in a [`Welcome`]. Frozen values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopState {
    /// Sent within the hello deadline; the daemon is STOPPING. Not completion.
    Accepted,
    /// Sent after R14 cleanup, just before close and ownership release.
    Completed,
    #[serde(other)]
    Unknown,
}

/// The frozen daemon welcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub binary: BoundedString<MAX_ID_BYTES>,
    pub project_root: WirePath,
    pub project_id: ProjectId,
    /// Distinguishes daemon lifetimes, not projects.
    pub instance: Id,
    pub pid: u32,
    /// `false` on version skew: the daemon closes before decoding any request.
    pub compatible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_state: Option<StopState>,
}

/// Frozen reject codes. Unknown future codes decode as [`RejectCode::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    HandshakeFailed,
    WrongProject,
    Busy,
    StopFailed,
    #[serde(other)]
    Unknown,
}

/// The frozen pre-session reject. Fields are bounded and safe: no payload is
/// ever echoed back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reject {
    pub code: RejectCode,
    pub message: BoundedString<1024>,
    /// `busy`: live ordinary sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<u32>,
    /// `busy`: the session cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<u32>,
    /// `wrong_project`: the daemon's own project root, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<WirePath>,
}

/// The daemon's first frame: exactly one of `welcome` or `reject`. A struct
/// rather than an enum so unknown sibling keys stay tolerated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome: Option<Welcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<Reject>,
}

/// The two valid shapes of a [`HandshakeReply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Welcome(Welcome),
    Reject(Reject),
}

impl HandshakeReply {
    pub fn welcome(welcome: Welcome) -> Self {
        HandshakeReply {
            welcome: Some(welcome),
            reject: None,
        }
    }

    pub fn reject(reject: Reject) -> Self {
        HandshakeReply {
            welcome: None,
            reject: Some(reject),
        }
    }

    /// `None` unless exactly one of the two is present.
    pub fn into_reply(self) -> Option<Reply> {
        match (self.welcome, self.reject) {
            (Some(welcome), None) => Some(Reply::Welcome(welcome)),
            (None, Some(reject)) => Some(Reply::Reject(reject)),
            _ => None,
        }
    }
}
