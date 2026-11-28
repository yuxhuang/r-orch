//! Private, descriptor-confined state roots for the SQLite adapter.
pub mod backup;
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

/// Read-only exclusion for an existing offline import. Consuming publication
/// retains the original locked open description; it never reacquires the lock.
#[derive(Debug)]
pub struct ImportGuard {
    path: PathBuf,
    root: ConfinedDir,
    lock: Flock<File>,
}

/// Distinguishes a read-only validation refusal from a publication that may
/// already have changed the destination when an I/O failure is reported.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ImportTransitionError {
    #[source]
    pub error: DataDirError,
    pub may_have_written: bool,
}

impl ImportGuard {
    /// Missing, unsafe and contended roots refuse without creation or repair.
    pub fn acquire(path: &Path) -> Result<Self, DataDirError> {
        if !path.is_absolute() {
            return Err(DataDirError::InvalidLocation(
                "data directory must be absolute".into(),
            ));
        }
        let root = ConfinedDir::open(path)?;
        private_dir(&root)?;
        if !local_filesystem(&root)? {
            return Err(DataDirError::NetworkFilesystem);
        }
        let file = root.open_file("orch.lock", FileAccess::Untrusted)?;
        private_file(&file)?;
        let lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => lock,
            Err((_, nix::errno::Errno::EWOULDBLOCK)) => {
                // Do not read holder bytes: even atime must survive refusal.
                return Err(DataDirError::Held("existing import destination".into()));
            }
            Err((_, error)) => return Err(io::Error::from(error).into()),
        };
        let guard = Self {
            path: path.canonicalize()?,
            root,
            lock,
        };
        guard.check_identity()?;
        guard.check_existing_entries()?;
        Ok(guard)
    }

    fn check_identity(&self) -> Result<(), DataDirError> {
        let named = ConfinedDir::open(&self.path)?;
        same_inode(&self.root, &named)?;
        let named_lock = self.root.open_file("orch.lock", FileAccess::Untrusted)?;
        private_file(&named_lock)?;
        same_inode(&*self.lock, &named_lock)
    }

    fn check_existing_entries(&self) -> Result<(), DataDirError> {
        use nix::fcntl::AtFlags;
        use nix::sys::stat::fstatat;
        for name in ["orch.db", "orch.db-wal", "orch.db-shm", "orch.db-journal"] {
            let stat = match fstatat(&self.root, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(nix::errno::Errno::ENOENT) => continue,
                Err(e) => return Err(io::Error::from(e).into()),
            };
            self.root.check_file(name, FileAccess::Untrusted)?;
            if stat.st_uid != nix::unistd::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
                return Err(DataDirError::Permissions);
            }
        }
        for name in ["blobs", "repos", "spool", "backups", "run"] {
            let dir = match self.root.open_dir(name) {
                Ok(dir) => dir,
                Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            private_dir(&dir)?;
            let children: &[&str] = match name {
                "blobs" => &["sha256", "tmp"],
                "spool" => &["finish"],
                _ => &[],
            };
            for child in children {
                match dir.open_dir(child) {
                    Ok(dir) => private_dir(&dir)?,
                    Err(ConfinedError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    }

    /// Called only after whole-source preflight and the first checked load.
    /// Publication uses a writable descriptor to the still-locked inode.
    pub fn into_data_dir(self, holder: &Holder) -> Result<DataDir, ImportTransitionError> {
        let (record, mut writable) = (|| -> Result<(Vec<u8>, File), DataDirError> {
            let record = serde_json::to_vec(holder).map_err(io::Error::other)?;
            if record.len() > 4096 {
                return Err(DataDirError::InvalidLocation(
                    "holder record exceeds 4096 bytes".into(),
                ));
            }
            self.check_identity()?;
            self.check_existing_entries()?;
            let writable = self.root.open_file("orch.lock", FileAccess::Modify)?;
            private_file(&writable)?;
            same_inode(&*self.lock, &writable)?;
            Ok((record, writable))
        })()
        .map_err(|error| ImportTransitionError {
            error,
            may_have_written: false,
        })?;
        // From the first publication syscall, an error cannot certify zero writes.
        (|| -> Result<DataDir, DataDirError> {
            writable.set_len(0)?;
            writable.rewind()?;
            writable.write_all(&record)?;
            writable.sync_all()?;
            for name in ["blobs", "repos", "spool", "backups", "run"] {
                private_dir(&self.root.ensure_dir(name)?)?;
            }
            let blobs = self.root.open_dir("blobs")?;
            for name in ["sha256", "tmp"] {
                private_dir(&blobs.ensure_dir(name)?)?;
            }
            private_dir(&self.root.open_dir("spool")?.ensure_dir("finish")?)?;
            self.root.sync()?;
            Ok(DataDir {
                path: self.path,
                root: self.root,
                _lock: self.lock,
            })
        })()
        .map_err(|error| ImportTransitionError {
            error,
            may_have_written: true,
        })
    }
}

fn private_file(file: &File) -> Result<(), DataDirError> {
    let stat = fstat(file).map_err(io::Error::from)?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() || stat.st_mode & 0o077 != 0 {
        return Err(DataDirError::Permissions);
    }
    Ok(())
}

fn same_inode(a: &impl std::os::fd::AsFd, b: &impl std::os::fd::AsFd) -> Result<(), DataDirError> {
    let a = fstat(a).map_err(io::Error::from)?;
    let b = fstat(b).map_err(io::Error::from)?;
    if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino) {
        return Err(DataDirError::InvalidLocation(
            "import guard inode changed".into(),
        ));
    }
    Ok(())
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
    Ok(supported_linux_filesystem(fs.filesystem_type().0))
}

#[cfg(any(target_os = "linux", test))]
fn supported_linux_filesystem(magic: i64) -> bool {
    // Supported local filesystems only; network and unknown types fail closed.
    // Magic values: Linux UAPI include/uapi/linux/magic.h. Overlay is used by
    // isolated Linux CI; ext2/3/4 share one value.
    matches!(
        magic,
        0xef53 | 0x58465342 | 0x9123683e | 0xf2f52010 | 0x01021994 | 0x794c7630
    )
}

#[cfg(test)]
mod tests {
    use super::supported_linux_filesystem;

    #[test]
    fn linux_filesystems_fail_closed_for_remote_and_unknown_types() {
        for magic in [
            0x5346414f, 0x6969, 0xff534d42, 0x65735546, 0x01021997, 0x12345678,
        ] {
            assert!(!supported_linux_filesystem(magic), "{magic:#x}");
        }
        for magic in [
            0xef53, 0x58465342, 0x9123683e, 0xf2f52010, 0x794c7630, 0x01021994,
        ] {
            assert!(supported_linux_filesystem(magic), "{magic:#x}");
        }
    }
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
        Self::acquire_from(&root)
    }

    pub(crate) fn acquire_from(root: &ConfinedDir) -> Result<Option<Self>, DataDirError> {
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
    pub(crate) fn verify(&self, root: &ConfinedDir) -> Result<(), DataDirError> {
        let named = root.open_file(".daemon-owner.lock", FileAccess::Untrusted)?;
        same_inode(&*self._lock, &named)
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

#[cfg(test)]
mod import_guard_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn holder(pid: u32) -> Holder {
        Holder {
            kind: LockKind::Import,
            pid,
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "test".into(),
        }
    }
    fn metadata(path: &Path) -> (u64, u64, u32, i64, i64, i64, i64, i64, i64) {
        let m = std::fs::metadata(path).unwrap();
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.atime(),
            m.atime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    }
    fn contender(path: &Path) -> bool {
        Flock::lock(
            File::open(path.join("orch.lock")).unwrap(),
            FlockArg::LockExclusiveNonblock,
        )
        .is_ok()
    }
    #[test]
    fn acquisition_refusal_and_drop_do_not_publish_or_change_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        drop(DataDir::acquire(&path, &holder(11)).unwrap());
        let lock = path.join("orch.lock");
        let bytes = std::fs::read(&lock).unwrap();
        let before = (metadata(&path), metadata(&lock));
        let guard = ImportGuard::acquire(&path).unwrap();
        assert!(!contender(&path));
        assert!(ImportGuard::acquire(&path).is_err());
        assert_eq!((metadata(&path), metadata(&lock)), before);
        drop(guard);
        assert_eq!((metadata(&path), metadata(&lock)), before);
        assert_eq!(std::fs::read(lock).unwrap(), bytes);
        assert!(contender(&path));
    }
    #[test]
    fn transfer_publishes_on_same_inode_and_retains_exclusion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        drop(DataDir::acquire(&path, &holder(11)).unwrap());
        let before = metadata(&path.join("orch.lock"));
        let guard = ImportGuard::acquire(&path).unwrap();
        assert!(!contender(&path));
        let data = guard.into_data_dir(&holder(22)).unwrap();
        assert!(!contender(&path));
        let after = metadata(&path.join("orch.lock"));
        assert_eq!((before.0, before.1), (after.0, after.1));
        assert_eq!(
            serde_json::from_slice::<Holder>(&std::fs::read(path.join("orch.lock")).unwrap())
                .unwrap()
                .pid,
            22
        );
        drop(data);
        assert!(contender(&path));
    }
    #[test]
    fn replacement_and_unsafe_entries_refuse_without_repair() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        assert!(ImportGuard::acquire(&path).is_err());
        assert!(!path.exists());
        drop(DataDir::acquire(&path, &holder(11)).unwrap());
        let guard = ImportGuard::acquire(&path).unwrap();
        std::fs::rename(path.join("orch.lock"), path.join("old.lock")).unwrap();
        std::fs::write(path.join("orch.lock"), b"replacement").unwrap();
        std::fs::set_permissions(
            path.join("orch.lock"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let error = guard.into_data_dir(&holder(22)).unwrap_err();
        assert!(!error.may_have_written);
        assert_eq!(
            std::fs::read(path.join("orch.lock")).unwrap(),
            b"replacement"
        );
        std::fs::remove_file(path.join("orch.lock")).unwrap();
        std::os::unix::fs::symlink("old.lock", path.join("orch.lock")).unwrap();
        assert!(ImportGuard::acquire(&path).is_err());
        std::fs::remove_file(path.join("orch.lock")).unwrap();
        std::fs::hard_link(path.join("old.lock"), path.join("orch.lock")).unwrap();
        assert!(ImportGuard::acquire(&path).is_err());
    }
    #[tokio::test]
    async fn native_writer_keeps_original_description_until_drained() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        drop(DataDir::acquire(&path, &holder(11)).unwrap());
        let guard = ImportGuard::acquire(&path).unwrap();
        let data = std::sync::Arc::new(guard.into_data_dir(&holder(22)).unwrap());
        let storage = crate::sqlite::SqliteStorage::open(
            data.clone(),
            crate::sqlite::StorageOptions::default(),
        )
        .unwrap();
        assert!(!contender(&path));
        storage.close().await.unwrap();
        // The caller retains its guard until the close receipt has drained.
        assert!(!contender(&path));
        drop(storage);
        assert!(!contender(&path));
        drop(data);
        assert!(contender(&path));
    }
}
