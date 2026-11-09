//! Exclusive ownership of a project (design D3, spec R37, rulings G2/SE-15).
//!
//! Two locks, taken in this order and released in the reverse:
//!
//! 1. [`OwnerGuard`]: a non-blocking `flock` on the stable
//!    `.orchestry/.daemon-owner.lock`. The kernel decides the start race; the
//!    file is never unlinked, so there is exactly one inode to lock. The
//!    descriptor is close-on-exec, so agent children never inherit it and a
//!    killed owner's guard is released with its last descriptor.
//! 2. [`PidLock`]: the legacy decimal PID file (`orchestry.lock`), unchanged
//!    in format, so a v0.13 `orch run --watch` still sees a holder. Under the
//!    guard a live PID is never evicted, whatever its mtime.

use crate::DaemonError;
use crate::discovery::io_error;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::libc;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const GUARD_FILE: &str = ".daemon-owner.lock";
/// How long a loser rereads the guard for the winner's PID (SE-15).
const LOSER_REREAD: Duration = Duration::from_secs(2);
const LOSER_POLL: Duration = Duration::from_millis(50);
/// The PID lock refresh period: well inside the legacy 60 s staleness window
/// and independent of dispatch ticks (SE-2).
pub(crate) const PID_REFRESH: Duration = Duration::from_secs(10);

/// The held owner guard. Dropping it releases the lock; the file stays.
#[derive(Debug)]
pub(crate) struct OwnerGuard {
    _lock: Flock<File>,
}

impl OwnerGuard {
    /// Take the guard or report who holds it. Only contention maps to
    /// [`DaemonError::AlreadyRunning`]; any other lock error fails closed.
    pub(crate) async fn acquire(state_dir: &Path) -> Result<OwnerGuard, DaemonError> {
        let path = state_dir.join(GUARD_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|source| io_error(&format!("open {}", path.display()), source))?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(mut lock) => {
                // The PID is published only after the lock is ours, with a
                // terminating newline so a reader never takes a partial write
                // for a PID.
                let write = |file: &mut File| -> io::Result<()> {
                    file.set_len(0)?;
                    file.rewind()?;
                    file.write_all(format!("{}\n", std::process::id()).as_bytes())?;
                    file.sync_data()
                };
                write(&mut lock)
                    .map_err(|source| io_error(&format!("write {}", path.display()), source))?;
                Ok(OwnerGuard { _lock: lock })
            }
            Err((_, Errno::EWOULDBLOCK)) => {
                Err(DaemonError::AlreadyRunning(read_winner(&path).await))
            }
            Err((_, errno)) => Err(DaemonError::InvalidArguments(format!(
                "cannot lock {} ({errno}); the filesystem must support advisory locks",
                path.display()
            ))),
        }
    }
}

/// The winner's PID, if it publishes one within [`LOSER_REREAD`]. `None`
/// when the winner died first: a PID is never invented.
async fn read_winner(path: &Path) -> Option<u32> {
    let deadline = tokio::time::Instant::now() + LOSER_REREAD;
    loop {
        if let Some(pid) = read_terminated_pid(path) {
            return Some(pid);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOSER_POLL).await;
    }
}

fn read_terminated_pid(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    let line = text.strip_suffix('\n')?;
    line.parse().ok().filter(|pid| *pid != 0)
}

/// The legacy PID file, held under the guard.
#[derive(Debug)]
pub(crate) struct PidLock {
    path: PathBuf,
    pid: u32,
}

/// Why a refresh could not prove ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidLockState {
    Held,
    /// The file is gone or names another PID: `ownership_lost`.
    Lost,
}

impl PidLock {
    /// Refuse any live foreign PID regardless of mtime (G2), then create the
    /// file with the existing exclusive-create semantics.
    pub(crate) async fn acquire(path: PathBuf) -> Result<PidLock, DaemonError> {
        let own = std::process::id();
        if let Some(holder) = read_pid(&path)
            && holder != own
            && orch_infra::storage::lock::is_process_alive(holder)
        {
            return Err(DaemonError::LegacyLockHeld(holder));
        }
        let result = orch_infra::storage::lock::acquire_lock(&path).await?;
        match (result.acquired, result.pid) {
            (true, _) => Ok(PidLock { path, pid: own }),
            (false, Some(holder)) => Err(DaemonError::LegacyLockHeld(holder)),
            (false, None) => Err(DaemonError::AlreadyRunning(None)),
        }
    }

    /// Verify the file still names us before refreshing its mtime; never
    /// recreate a missing or foreign lock (SE-2).
    pub(crate) async fn refresh(&self) -> PidLockState {
        if read_pid(&self.path) != Some(self.pid) {
            return PidLockState::Lost;
        }
        orch_infra::storage::lock::touch_lock(&self.path).await;
        PidLockState::Held
    }

    /// Remove the file only while it still names us (synchronous, so a
    /// dropped harness can release it too).
    pub(crate) fn release(&self) {
        if read_pid(&self.path) == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The decimal PID text of the legacy lock (no trailing newline by contract,
/// tolerated if present).
fn read_pid(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn state_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn guard_has_one_winner_and_the_loser_names_its_pid() {
        let dir = state_dir();
        let winner = OwnerGuard::acquire(dir.path()).await.unwrap();
        let path = dir.path().join(GUARD_FILE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // flock is per open file description, so a second open in this
        // process contends exactly like another process.
        let loser = OwnerGuard::acquire(dir.path()).await.unwrap_err();
        assert!(
            matches!(loser, DaemonError::AlreadyRunning(Some(pid)) if pid == std::process::id()),
            "{loser:?}"
        );
        assert_eq!(loser.exit_code(), orch_proto::error::exit::LOCK_CONFLICT);
        drop(winner);
        assert!(path.exists(), "the guard file is never unlinked");
        let _again = OwnerGuard::acquire(dir.path()).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn loser_never_invents_a_pid_when_the_winner_has_not_published() {
        let dir = state_dir();
        let path = dir.path().join(GUARD_FILE);
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let _held = Flock::lock(held, FlockArg::LockExclusiveNonblock).unwrap();
        // A partial (unterminated) write is not a PID.
        std::fs::write(&path, b"12").unwrap();
        let loser = OwnerGuard::acquire(dir.path()).await.unwrap_err();
        assert!(
            matches!(loser, DaemonError::AlreadyRunning(None)),
            "{loser:?}"
        );
        assert!(loser.to_string().contains("PID unavailable"));
    }

    #[test]
    fn guard_descriptor_is_close_on_exec() {
        let dir = state_dir();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = runtime.block_on(OwnerGuard::acquire(dir.path())).unwrap();
        let flags = nix::fcntl::fcntl(&*guard._lock, nix::fcntl::FcntlArg::F_GETFD).unwrap();
        assert!(
            nix::fcntl::FdFlag::from_bits_truncate(flags).contains(nix::fcntl::FdFlag::FD_CLOEXEC)
        );
    }

    #[tokio::test]
    async fn guard_symlink_is_refused() {
        let dir = state_dir();
        std::os::unix::fs::symlink("/dev/null", dir.path().join(GUARD_FILE)).unwrap();
        assert!(matches!(
            OwnerGuard::acquire(dir.path()).await,
            Err(DaemonError::Io { .. })
        ));
    }

    #[tokio::test]
    async fn live_legacy_pid_is_refused_even_when_old_and_dead_pid_is_replaced() {
        let dir = state_dir();
        let path = dir.path().join("orchestry.lock");
        // PID 1 is always alive; its old mtime must not matter.
        std::fs::write(&path, b"1").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let error = PidLock::acquire(path.clone()).await.unwrap_err();
        assert!(matches!(error, DaemonError::LegacyLockHeld(1)), "{error:?}");
        assert!(error.to_string().contains("ps -p 1 -o command="), "{error}");
        assert_eq!(error.exit_code(), orch_proto::error::exit::LOCK_CONFLICT);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1");

        // A PID that cannot exist is replaced with ours, in the legacy format.
        std::fs::write(&path, b"2147483646").unwrap();
        let lock = PidLock::acquire(path.clone()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        assert_eq!(lock.refresh().await, PidLockState::Held);
        lock.release();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn refresh_reports_lost_and_never_recreates() {
        let dir = state_dir();
        let path = dir.path().join("orchestry.lock");
        let lock = PidLock::acquire(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(lock.refresh().await, PidLockState::Lost);
        assert!(!path.exists());
        std::fs::write(&path, b"1").unwrap();
        assert_eq!(lock.refresh().await, PidLockState::Lost);
        lock.release();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "1",
            "a foreign lock is kept"
        );
    }
}
