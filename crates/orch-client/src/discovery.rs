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
            let cause = state_dir_refusal(&root, meta.uid(), geteuid().as_raw(), meta.mode());
            return Err(ClientError::InvalidArguments(format!(
                "{cause} A refused project does not prove a daemon is absent."
            )));
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

fn state_dir_refusal(root: &Path, uid: u32, euid: u32, mode: u32) -> String {
    let state = root.join(".orchestry");
    let project_display = orch_proto::path::escape_controls(&root.to_string_lossy());
    let state_display = orch_proto::path::escape_controls(&state.to_string_lossy());
    if uid != euid {
        return format!(
            "Refusing project `{project_display}`: `{state_display}` is owned by uid {uid}, \
             but this process runs as uid {euid}; run as the project owner/select your project. \
             No permissions were changed."
        );
    }
    format!(
        "Refusing project `{project_display}`: `{state_display}` is group/world-writable \
         (mode {:04o}). No permissions were changed. For an owner-only project, run \
         `chmod 700 {}` as its owner, keep its parent directories and ACLs under trusted \
         control, then start explicitly with `(cd {} && orch daemon start)`. If other \
         accounts could already write state, verify or restore it from trusted data before starting.",
        mode & 0o7777,
        shell_quote_path(&state),
        shell_quote_path(root),
    )
}

// Both callers quote directories. Keep advice printable even when the CLI
// escapes terminal controls; fixed octal operands also preserve non-UTF-8.
fn shell_quote_path(path: &Path) -> String {
    if let Some(path) = path.to_str()
        && !path.chars().any(char::is_control)
    {
        return format!("'{}'", path.replace('\'', "'\\''"));
    }
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut encoded = String::new();
    for byte in bytes {
        encoded.push_str(&format!("\\{byte:03o}"));
    }
    // Shell substitution strips trailing newlines. A directory's `/.` alias
    // preserves its identity and keeps that newline inside the quoted word.
    if bytes.ends_with(b"\n") {
        encoded.push_str("\\057\\056");
    }
    format!("\"$(printf '{encoded}')\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_advice_quotes_path_bytes_and_escapes_prose_controls() {
        for (path, expected) in [
            ("/tmp/owner's project", "'/tmp/owner'\\''s project'"),
            (
                "/tmp/line\nnext",
                r#""$(printf '\057\164\155\160\057\154\151\156\145\012\156\145\170\164')""#,
            ),
            ("-leading-dash", "'-leading-dash'"),
            ("/tmp/$(echo bad);`id`", "'/tmp/$(echo bad);`id`'"),
        ] {
            assert_eq!(shell_quote_path(Path::new(path)), expected);
        }
        use std::os::unix::ffi::OsStrExt;
        let raw = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff'\n"));
        assert_eq!(
            shell_quote_path(raw),
            r#""$(printf '\057\164\155\160\057\377\047\012\057\056')""#
        );
        let message = state_dir_refusal(Path::new("/tmp/owner's\nproject"), 42, 42, 0o775);
        for part in [
            "Refusing project `/tmp/owner's\\u{a}project`",
            "`/tmp/owner's\\u{a}project/.orchestry`",
            "is group/world-writable (mode 0775)",
            "No permissions were changed.",
            "`chmod 700 ",
            " && orch daemon start)`",
            "as its owner",
            "keep its parent directories and ACLs under trusted control",
            "verify or restore it from trusted data before starting",
        ] {
            assert!(message.contains(part), "missing {part:?}: {message}");
        }
        assert!(!message.contains('\n'), "{message}");
        let owner = state_dir_refusal(Path::new("/tmp/project"), 41, 42, 0o775);
        for part in [
            "/tmp/project/.orchestry",
            "owned by uid 41",
            "runs as uid 42",
            "run as the project owner/select your project",
            "No permissions were changed.",
        ] {
            assert!(owner.contains(part), "{owner}");
        }
        assert!(!owner.contains("chmod"));
        assert!(!owner.contains("sudo"));
        assert!(!owner.contains("chown"));
    }
}
