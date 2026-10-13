//! The project-lock seam.
//!
//! The lock *mechanism* lives in [`orch_infra::storage::lock`] — the PID file
//! format is L0 (PARITY.md §6.7) and belongs with the other on-disk formats.
//! What lives here is the **seam**: a trait the orchestrator depends on, so a
//! unit test can run `start_watch`/`stop` without touching the filesystem.
//! Every orchestrator test in the reference suite does the equivalent with
//! `vi.mock('…/storage/lock.js')`.
//!
//! Who takes the lock (PARITY.md §7.13):
//!
//! | Caller | Behaviour |
//! |---|---|
//! | `run --watch`, `serve` | held for the process lifetime |
//! | `run <id>`, `run --all`, `cancel`, `agent stop` | acquired for the call, then released |
//! | every read-only command | no lock |

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use orch_infra::storage::lock;

pub use orch_infra::storage::lock::{LOCK_STALE, LockResult};

#[async_trait]
pub trait ProjectLock: Send + Sync {
    /// On failure `pid` is the current holder, which is what
    /// `LockConflictError(pid)` reports.
    async fn acquire(&self) -> LockResult;
    /// Errors are swallowed: a lock we cannot unlink is indistinguishable from
    /// one that was never there, and failing a shutdown over it is worse.
    async fn release(&self);
    /// Refresh the mtime to prove liveness. Called once per tick — it is what
    /// keeps a live holder's lock from looking recycled.
    async fn touch(&self);
    /// Whether the project is locked. **Ignores mtime**, unlike `acquire`:
    /// `status` reports, and calling a mid-slow-tick holder "not locked" would
    /// be wrong.
    async fn check(&self) -> LockResult;
}

/// The real lock, over `.orchestry/orchestry.lock`.
#[derive(Debug, Clone)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileLock { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl ProjectLock for FileLock {
    async fn acquire(&self) -> LockResult {
        match lock::acquire_lock(&self.path).await {
            Ok(result) => result,
            // An I/O failure is not "someone else holds it", but the caller's
            // only move is the same either way, and reporting no PID yields
            // `LockConflictError(0)` — the same thing the TS code produces
            // when the holder's PID cannot be read.
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "could not acquire the project lock");
                LockResult {
                    acquired: false,
                    pid: None,
                }
            }
        }
    }

    async fn release(&self) {
        lock::release_lock(&self.path).await;
    }

    async fn touch(&self) {
        lock::touch_lock(&self.path).await;
    }

    async fn check(&self) -> LockResult {
        lock::check_lock(&self.path).await
    }
}

/// An in-memory lock that touches no filesystem.
///
/// Public, not `#[cfg(test)]`: the orchestrator's integration tests live
/// outside this module, and an embedder that already guarantees single-process
/// access has no use for a lock file.
#[derive(Debug)]
pub struct InMemoryLock {
    held: std::sync::Mutex<Option<u32>>,
    own_pid: u32,
    /// When set, every `acquire` reports a conflict with this PID. This is how
    /// "the lock is held by another process" is expressed in tests.
    conflict_with: std::sync::Mutex<Option<u32>>,
    acquires: std::sync::atomic::AtomicUsize,
    releases: std::sync::atomic::AtomicUsize,
    touches: std::sync::atomic::AtomicUsize,
}

impl Default for InMemoryLock {
    fn default() -> Self {
        InMemoryLock::new()
    }
}

impl InMemoryLock {
    pub fn new() -> Self {
        InMemoryLock {
            held: std::sync::Mutex::new(None),
            own_pid: std::process::id(),
            conflict_with: std::sync::Mutex::new(None),
            acquires: std::sync::atomic::AtomicUsize::new(0),
            releases: std::sync::atomic::AtomicUsize::new(0),
            touches: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Report a conflict with `pid` from now on. `None` restores normal
    /// behaviour.
    pub fn set_conflict(&self, pid: Option<u32>) {
        *self.conflict_with.lock().unwrap_or_else(|e| e.into_inner()) = pid;
    }

    pub fn is_held(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn acquire_count(&self) -> usize {
        self.acquires.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn release_count(&self) -> usize {
        self.releases.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn touch_count(&self) -> usize {
        self.touches.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ProjectLock for InMemoryLock {
    async fn acquire(&self) -> LockResult {
        self.acquires
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(pid) = *self.conflict_with.lock().unwrap_or_else(|e| e.into_inner()) {
            return LockResult {
                acquired: false,
                pid: Some(pid),
            };
        }
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.own_pid);
        LockResult {
            acquired: true,
            pid: Some(self.own_pid),
        }
    }

    async fn release(&self) {
        self.releases
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    async fn touch(&self) {
        self.touches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    async fn check(&self) -> LockResult {
        let pid = *self.held.lock().unwrap_or_else(|e| e.into_inner());
        LockResult {
            acquired: pid.is_some(),
            pid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_lock_round_trips_through_the_real_lock_module() {
        let temp = tempfile::tempdir().unwrap();
        let file_lock = FileLock::new(temp.path().join("orchestry.lock"));

        let outcome = file_lock.acquire().await;
        assert!(outcome.acquired);
        assert_eq!(outcome.pid, Some(std::process::id()));
        assert_eq!(
            tokio::fs::read_to_string(file_lock.path()).await.unwrap(),
            std::process::id().to_string()
        );

        // Our own live, freshly written lock blocks a second acquisition.
        assert!(!file_lock.acquire().await.acquired);

        file_lock.touch().await;
        assert!(file_lock.check().await.acquired);

        file_lock.release().await;
        assert!(!file_lock.path().exists());
        assert!(!file_lock.check().await.acquired);
    }

    #[tokio::test]
    async fn the_in_memory_lock_reports_conflicts_and_counts_calls() {
        let lock = InMemoryLock::new();
        assert!(lock.acquire().await.acquired);
        assert!(lock.is_held());
        assert!(lock.check().await.acquired);

        lock.release().await;
        assert!(!lock.is_held());
        assert!(!lock.check().await.acquired);

        lock.set_conflict(Some(9999));
        let outcome = lock.acquire().await;
        assert!(!outcome.acquired);
        assert_eq!(outcome.pid, Some(9999));

        lock.touch().await;
        assert_eq!(lock.acquire_count(), 2);
        assert_eq!(lock.release_count(), 1);
        assert_eq!(lock.touch_count(), 1);

        // Clearing the conflict lets it acquire again.
        lock.set_conflict(None);
        assert!(lock.acquire().await.acquired);
    }
}
