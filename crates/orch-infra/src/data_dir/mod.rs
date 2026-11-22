//! Private, descriptor-confined state roots for the SQLite adapter.
pub mod confined;
pub mod paths;

use confined::{ConfinedDir, ConfinedError, FileAccess};
use nix::fcntl::{Flock, FlockArg};
use nix::sys::stat::fstat;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockKind {
    Daemon,
    Import,
    Restore,
    Vacuum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holder {
    pub kind: LockKind,
    pub pid: u32,
    pub started_at: String,
    pub binary_version: String,
    pub executable: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    #[error(transparent)]
    Confined(#[from] ConfinedError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("directory must be owned by this user with no group or other access")]
    Permissions,
    #[error("network or unrecognised remote filesystem refused")]
    NetworkFilesystem,
    #[error("data directory held by {0}")]
    Held(String),
    #[error("legacy daemon holds the project owner lock")]
    LegacyHeld,
    #[error("invalid data location: {0}")]
    InvalidLocation(String),
}

impl DataDirError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Held(_) | Self::LegacyHeld => 4,
            Self::InvalidLocation(_) | Self::Permissions => 2,
            _ => 1,
        }
    }
}

/// Holds the stable lock inode for the lifetime of a daemon or offline command.
/// This is an additive startup primitive; T24 wires it into the daemon.
#[derive(Debug)]
pub struct DataDir {
    path: PathBuf,
    root: ConfinedDir,
    _lock: Flock<File>,
}

fn private_dir(dir: &ConfinedDir) -> Result<(), DataDirError> {
    let stat = fstat(dir).map_err(io::Error::from)?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
        return Err(DataDirError::Permissions);
    }
    Ok(())
}

impl DataDir {
    pub fn acquire(path: &Path, holder: &Holder) -> Result<Self, DataDirError> {
        Self::acquire_with_probe(path, holder, local_filesystem)
    }

    /// A filesystem probe is a seam, so refusal can be tested without mounting
    /// a real network filesystem or reaching the owner's data.
    pub fn acquire_with_probe(
        path: &Path,
        holder: &Holder,
        probe: impl FnOnce(&ConfinedDir) -> io::Result<bool>,
    ) -> Result<Self, DataDirError> {
        if !path.is_absolute() {
            return Err(DataDirError::InvalidLocation(
                "data directory must be absolute".into(),
            ));
        }
        let root = match ConfinedDir::create(path) {
            Ok(root) => root,
            Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                ConfinedDir::open(path)?
            }
            Err(e) => return Err(e.into()),
        };
        private_dir(&root)?;
        if !probe(&root)? {
            return Err(DataDirError::NetworkFilesystem);
        }
        // No other entry is opened until the exclusive lock is ours.
        let file = match root.create_file("orch.lock") {
            Ok(file) => file,
            Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                root.open_file("orch.lock", FileAccess::Modify)?
            }
            Err(e) => return Err(e.into()),
        };
        let stat = fstat(&file).map_err(io::Error::from)?;
        if stat.st_uid != nix::unistd::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
            return Err(DataDirError::Permissions);
        }
        let mut lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => lock,
            Err((mut file, nix::errno::Errno::EWOULDBLOCK)) => {
                // The winner may still be publishing its record. Never invent
                // a holder or modify its inode when the bounded reread ends.
                let mut text = String::new();
                for _ in 0..40 {
                    text.clear();
                    file.rewind()?;
                    (&mut file).take(4097).read_to_string(&mut text)?;
                    if serde_json::from_str::<Holder>(&text).is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                return Err(DataDirError::Held(format!("{text:?}")));
            }
            Err((_, e)) => return Err(io::Error::from(e).into()),
        };
        let record = serde_json::to_vec(holder).map_err(io::Error::other)?;
        if record.len() > 4096 {
            return Err(DataDirError::InvalidLocation(
                "holder record exceeds 4096 bytes".into(),
            ));
        }
        lock.set_len(0)?;
        lock.rewind()?;
        lock.write_all(&record)?;
        lock.sync_all()?;
        for name in ["blobs", "repos", "spool", "backups", "run"] {
            private_dir(&root.ensure_dir(name)?)?;
        }
        let blobs = root.open_dir("blobs")?;
        for name in ["sha256", "tmp"] {
            private_dir(&blobs.ensure_dir(name)?)?;
        }
        private_dir(&root.open_dir("spool")?.ensure_dir("finish")?)?;
        root.sync()?;
        Ok(Self {
            path: path.canonicalize()?,
            root,
            _lock: lock,
        })
    }

    pub fn root(&self) -> &ConfinedDir {
        &self.root
    }

    /// SQLite's fixed-path exception to R102. The root remains locked and private.
    pub fn database_path(&self) -> PathBuf {
        self.path.join("orch.db")
    }
}

#[cfg(target_os = "macos")]
fn local_filesystem(dir: &ConfinedDir) -> io::Result<bool> {
    use nix::sys::statfs::fstatfs;
    // Fail closed for remote and user-mounted types; these are the local macOS
    // filesystems on which this adapter supports WAL. No mount privilege is used.
    Ok(matches!(
        fstatfs(dir)
            .map_err(io::Error::from)?
            .filesystem_type_name(),
        "apfs" | "hfs" | "ufs" | "tmpfs"
    ))
}

#[cfg(target_os = "linux")]
fn local_filesystem(dir: &ConfinedDir) -> io::Result<bool> {
    let fs = nix::sys::statfs::fstatfs(dir).map_err(io::Error::from)?;
    // FUSE and 9p can be remote too; refuse rather than claiming WAL is safe.
    let remote = [
        0x6969, 0x517b, 0x564c, 0x73757245, 0xff534d42, 0xfe534d42, 0x65735546, 0x01021997,
        0x00c36400, 0x0bd00bd0,
    ];
    Ok(!remote.contains(&fs.filesystem_type().0))
}

/// Shared with a 1.0 daemon, but never created, truncated or rewritten by 1.1.
#[derive(Debug)]
pub struct LegacyGuard {
    _lock: Flock<File>,
}

impl LegacyGuard {
    /// None means no legacy lock exists. The caller must then use the socket
    /// liveness check; exclusion in that case is procedural (R31).
    pub fn acquire(legacy_root: &Path) -> Result<Option<Self>, DataDirError> {
        let root = ConfinedDir::open(legacy_root)?;
        let file = match root.open_file(".daemon-owner.lock", FileAccess::Untrusted) {
            Ok(file) => file,
            Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => Ok(Some(Self { _lock: lock })),
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => Err(DataDirError::LegacyHeld),
            Err((_, e)) => Err(io::Error::from(e).into()),
        }
    }
}

impl DataDir {
    /// SQLite's backup API opens this pre-created fixed database name itself.
    /// The directory name is made here, never read from a row or a manifest.
    pub(crate) fn create_migration_backup(
        &self,
        version: usize,
    ) -> Result<(String, ConfinedDir, PathBuf), DataDirError> {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let name = format!("pre-migration-v{version}-{at}");
        let dir = self.root.open_dir("backups")?.create_dir(&name)?;
        dir.create_file("orch.db")?;
        let sqlite_path = self.path.join("backups").join(&name).join("orch.db");
        Ok((name, dir, sqlite_path))
    }
}
