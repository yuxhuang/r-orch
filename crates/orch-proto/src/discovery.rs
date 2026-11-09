//! `.orchestry/daemon.json`, schema 1 (design D4).
//!
//! The daemon publishes this atomically, mode 0600, at most
//! [`MAX_DISCOVERY_BYTES`]. It is the **only** file a client reads in
//! `.orchestry/`, once per connection attempt, after checking it is a regular
//! file owned by the client's euid with no group/world permissions. Its PID is
//! an untrusted candidate: no client derives liveness or a kill hint from it.

use crate::bounded::{Id, MAX_ID_BYTES};
use crate::handshake::ProjectId;
use crate::path::WirePath;
use serde::{Deserialize, Serialize};

/// File name inside `.orchestry/`.
pub const DISCOVERY_FILE: &str = "daemon.json";
/// Upper bound on the file; a larger file is rejected, and reads stop here
/// even if the file grows while being read.
pub const MAX_DISCOVERY_BYTES: usize = 16 * 1024;
/// The only schema this build writes and reads.
pub const DISCOVERY_SCHEMA: u32 = 1;
/// Longest absolute socket path accepted, in raw bytes, leaving headroom under
/// macOS's 104-byte `sun_path` (design D4).
pub const MAX_SOCKET_PATH_BYTES: usize = 100;
/// Default socket location relative to the project root.
pub const DEFAULT_SOCKET_RELATIVE: &str = ".orchestry/run/orch.sock";
/// Environment variable naming an explicit endpoint.
pub const ENV_SOCKET: &str = "ORCH_SOCKET";
/// Environment variable naming the expected project root.
pub const ENV_PROJECT_ROOT: &str = "ORCH_PROJECT_ROOT";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonRecord {
    /// Always [`DISCOVERY_SCHEMA`]; any other value is a stale or foreign file.
    pub schema: u32,
    pub project_root: WirePath,
    pub project_id: ProjectId,
    /// The bound socket, ≤ [`MAX_SOCKET_PATH_BYTES`].
    pub socket: WirePath,
    pub pid: u32,
    /// Package semver of the daemon binary.
    pub binary: crate::bounded::BoundedString<MAX_ID_BYTES>,
    /// The daemon's `LOCAL_PROTOCOL`.
    pub protocol: u32,
    /// `std::env::current_exe()` of the daemon, for build provenance (R32).
    pub executable: WirePath,
    /// RFC 3339 UTC start time.
    pub started_at: Id,
    /// Daemon lifetime id; equals the welcome's `instance`.
    pub instance: Id,
}

impl DaemonRecord {
    /// Decode a bounded byte slice and check the schema.
    pub fn decode(bytes: &[u8]) -> Result<Self, DiscoveryError> {
        if bytes.len() > MAX_DISCOVERY_BYTES {
            return Err(DiscoveryError::TooLarge(bytes.len()));
        }
        let record: DaemonRecord =
            serde_json::from_slice(bytes).map_err(|_| DiscoveryError::Malformed)?;
        if record.schema != DISCOVERY_SCHEMA {
            return Err(DiscoveryError::Schema(record.schema));
        }
        if record.socket.as_path().as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(DiscoveryError::SocketPathTooLong);
        }
        Ok(record)
    }
}

/// Fixed categories only: the file's contents never appear in an error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("daemon.json is {0} bytes, more than the 16 KiB limit")]
    TooLarge(usize),
    #[error("daemon.json is malformed")]
    Malformed,
    #[error("daemon.json has unsupported schema {0}")]
    Schema(u32),
    #[error("daemon.json names a socket path longer than 100 bytes")]
    SocketPathTooLong,
}
