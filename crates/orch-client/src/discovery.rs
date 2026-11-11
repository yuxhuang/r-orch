//! Bounded discovery (D1/D4) and the explicit stop owner-release check.
use crate::{ClientError, NoDaemon};
use nix::fcntl::{OFlag, openat};
use nix::sys::stat::Mode;
use nix::unistd::geteuid;
use orch_proto::discovery::{
    DISCOVERY_FILE, DaemonRecord, MAX_DISCOVERY_BYTES, MAX_SOCKET_PATH_BYTES,
};
use orch_proto::error::NoDaemonDetail;
use orch_proto::handshake::ProjectId;
use orch_proto::path::{WirePath, escape_controls};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// An initialized project selected independently of the endpoint. The open
/// state-directory descriptor pins its dev/ino through discovery lookup.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub id: ProjectId,
    state_dir: File,
}

impl Project {
    /// An explicit root overrides cwd; an invalid explicit root never falls
    /// back. Symlink aliases to a root are fine; `.orchestry` itself is not.
    pub fn discover(cwd: &Path, explicit_root: Option<&Path>) -> Result<Self, ClientError> {
        let root = match explicit_root {
            Some(root) => root.to_path_buf(),
            None => cwd
                .ancestors()
                .find(|p| p.join(".orchestry").symlink_metadata().is_ok())
                .ok_or(NoDaemon::NotInitialized)?
                .to_path_buf(),
        };
        let root = root.canonicalize().map_err(|_| NoDaemon::NotInitialized)?;
        let state_dir = OpenOptions::new()
            .read(true)
            .custom_flags((OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC).bits())
            .open(root.join(".orchestry"))
            .map_err(|_| NoDaemon::NotInitialized)?;
        let meta = state_dir.metadata().map_err(|_| NoDaemon::NotInitialized)?;
        if meta.uid() != geteuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err(ClientError::InvalidArguments("project state directory must be owned by this uid and not writable by group or others".into()));
        }
        Ok(Self {
            root,
            id: ProjectId {
                dev: meta.dev(),
                ino: meta.ino(),
            },
            state_dir,
        })
    }

    pub(crate) fn failure(&self, detail: NoDaemonDetail) -> NoDaemon {
        NoDaemon::no_daemon(&self.root.to_string_lossy(), detail)
    }

    pub(crate) fn wrong_project(&self, actual: &WirePath) -> NoDaemon {
        NoDaemon::WrongProject {
            expected: escape_controls(&self.root.to_string_lossy()),
            actual: actual.display_escaped(),
        }
    }

    /// Explicit stop only: observe process exit or briefly probe the stable
    /// guard inode. Never create/unlink the guard or touch its PID contents.
    pub(crate) fn owner_released(&self, pid: u32) -> bool {
        use nix::fcntl::{Flock, FlockArg};
        if let Some(pid) = i32::try_from(pid).ok().filter(|pid| *pid > 0)
            && nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                == Err(nix::errno::Errno::ESRCH)
        {
            return true;
        }
        let Ok(fd) = openat(
            &self.state_dir,
            ".daemon-owner.lock",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) else {
            return false;
        };
        let file = File::from(fd);
        let Ok(meta) = file.metadata() else {
            return false;
        };
        if !meta.is_file() || meta.uid() != geteuid().as_raw() || meta.mode() & 0o077 != 0 {
            return false;
        }
        Flock::lock(file, FlockArg::LockExclusiveNonblock).is_ok()
    }

    /// Read once, without following a substituted directory or discovery
    /// symlink. Nonblocking open also prevents a FIFO from hanging the client.
    pub fn read_record(&self) -> Result<DaemonRecord, ClientError> {
        let fd = openat(
            &self.state_dir,
            DISCOVERY_FILE,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            self.failure(if e == nix::errno::Errno::ENOENT {
                NoDaemonDetail::NoDiscovery
            } else {
                NoDaemonDetail::UnsafeDiscovery
            })
        })?;
        let file = File::from(fd);
        let meta = file
            .metadata()
            .map_err(|_| self.failure(NoDaemonDetail::UnsafeDiscovery))?;
        if !meta.is_file()
            || meta.uid() != geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.len() > MAX_DISCOVERY_BYTES as u64
        {
            return Err(self.failure(NoDaemonDetail::UnsafeDiscovery).into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_DISCOVERY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| self.failure(NoDaemonDetail::UnsafeDiscovery))?;
        let record = DaemonRecord::decode(&bytes)
            .map_err(|_| self.failure(NoDaemonDetail::UnsafeDiscovery))?;
        if record.project_id != self.id {
            return Err(self.wrong_project(&record.project_root).into());
        }
        Ok(record)
    }
}

pub(crate) fn validate_socket(path: &Path, test_root: Option<&Path>) -> Result<(), ClientError> {
    let raw = path.as_os_str().as_encoded_bytes();
    if !path.is_absolute()
        || raw.is_empty()
        || raw.contains(&0)
        || raw.len() > MAX_SOCKET_PATH_BYTES
    {
        return Err(ClientError::InvalidArguments(
            "invalid socket path; set ORCH_SOCKET to a short absolute path (at most 100 bytes)"
                .into(),
        ));
    }
    if let Some(root) = test_root {
        // Canonicalize the parent, not the socket: absent endpoints must still
        // reach NoDaemon, while a symlink parent must never escape the root.
        let confined = root
            .canonicalize()
            .ok()
            .zip(path.parent().and_then(|p| p.canonicalize().ok()))
            .is_some_and(|(root, parent)| parent.starts_with(root));
        if !confined {
            return Err(ClientError::InvalidArguments(
                "refusing to connect outside the test socket root".into(),
            ));
        }
    }
    Ok(())
}
