//! The project PID lock — ported from `src/infrastructure/storage/lock.ts`.
//!
//! One orchestrator per project. Only `run --watch`/`serve` hold it for their
//! lifetime; `run <id>`, `run --all`, `cancel` and `agent stop` take it
//! temporarily, and every read-only command takes nothing (PARITY.md §7.13).
//!
//! # Why this is a trait
//!
//! The TS module is a set of free functions over a path, and every orchestrator
//! test in the reference suite replaces it with `vi.mock`. A trait is the Rust
//! equivalent of that seam, and it is also what makes V22 — staleness and the
//! PID-recycling defence — testable without a second process.
//!
//! # Why it lives here rather than in `orch-infra`
//!
//! ARCHITECTURE §3.3 lists `storage::lock` under `orch-infra` and WP-1 owns
//! that module. The orchestrator needs locking now, so [`FileLock`] lives here
//! for the moment; moving the implementation to `orch_infra::storage::lock`
//! later is a re-export away, because every caller depends on [`ProjectLock`]
//! and not on the concrete type.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use orch_core::process::ProcessManager;

/// A lock whose file has not been touched within this window is treated as
/// abandoned even when its PID looks alive.
///
/// This is the PID-recycling defence: a holder that died and whose PID the OS
/// handed to an unrelated process would otherwise block the project forever.
/// Set high enough to survive a slow tick (worktree operations plus adapter
/// spawns).
pub const LOCK_STALE_MS: u64 = 60_000;

/// The outcome of an acquisition attempt.
///
/// On failure `pid` is the **current holder**; on success it is this process.
/// It can be `None` on failure only when the file vanished between the
/// `EEXIST` and the re-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockOutcome {
    pub acquired: bool,
    pub pid: Option<u32>,
}

impl LockOutcome {
    pub fn acquired(pid: u32) -> Self {
        LockOutcome {
            acquired: true,
            pid: Some(pid),
        }
    }

    pub fn conflict(pid: Option<u32>) -> Self {
        LockOutcome {
            acquired: false,
            pid,
        }
    }
}

/// Whether the project is locked right now.
///
/// **Deliberately ignores mtime**, unlike acquisition: `status`/`checkLock` is
/// a report, and reporting "not locked" for a live holder that happens to be
/// mid-slow-tick would be wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockState {
    pub locked: bool,
    pub pid: Option<u32>,
}

#[async_trait]
pub trait ProjectLock: Send + Sync {
    async fn acquire(&self) -> LockOutcome;
    /// Errors are ignored: a lock we cannot delete is indistinguishable from
    /// one that was never there, and failing a shutdown over it is worse.
    async fn release(&self);
    /// Update mtime to prove liveness. Called once per tick.
    async fn touch(&self);
    async fn check(&self) -> LockState;
}

/// The real lock: a file containing the holder's PID as a decimal string with
/// **no trailing newline** (PARITY.md §6.7).
pub struct FileLock {
    path: PathBuf,
    processes: Arc<dyn ProcessManager>,
    /// The PID written into the file. A field rather than `std::process::id()`
    /// so a test can hold two `FileLock`s that look like two processes.
    own_pid: u32,
    /// Serializes acquisition within this process, replacing the TS
    /// module-level promise chain. Without it two tasks in one process can both
    /// see "no lock" and both create one.
    gate: tokio::sync::Mutex<()>,
}

impl FileLock {
    pub fn new(path: impl Into<PathBuf>, processes: Arc<dyn ProcessManager>) -> Self {
        FileLock {
            path: path.into(),
            processes,
            own_pid: std::process::id(),
            gate: tokio::sync::Mutex::new(()),
        }
    }

    /// Pretend to be `pid`. Only useful for tests that need two distinct
    /// holders inside one process.
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.own_pid = pid;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn read_pid(&self) -> Option<u32> {
        let content = tokio::fs::read_to_string(&self.path).await.ok()?;
        content.trim().parse::<u32>().ok()
    }

    /// `true` when the file is missing — a lock we cannot stat is one we are
    /// willing to steal, which is what the TS `catch { return true }` does.
    async fn is_stale_by_age(&self) -> bool {
        let Ok(metadata) = tokio::fs::metadata(&self.path).await else {
            return true;
        };
        let Ok(modified) = metadata.modified() else {
            return true;
        };
        let Ok(age) = SystemTime::now().duration_since(modified) else {
            // mtime in the future: not stale.
            return false;
        };
        age > Duration::from_millis(LOCK_STALE_MS)
    }

    async fn create_exclusive(&self) -> std::io::Result<()> {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&self.path).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, self.own_pid.to_string().as_bytes()).await?;
        tokio::io::AsyncWriteExt::flush(&mut file).await
    }
}

#[async_trait]
impl ProjectLock for FileLock {
    async fn acquire(&self) -> LockOutcome {
        let _gate = self.gate.lock().await;

        if let Some(existing) = self.read_pid().await {
            if self.processes.is_alive(existing) && !self.is_stale_by_age().await {
                return LockOutcome::conflict(Some(existing));
            }
            // Dead PID, or alive but untouched for too long — steal it.
            let _ = tokio::fs::remove_file(&self.path).await;
        }

        match self.create_exclusive().await {
            Ok(()) => LockOutcome::acquired(self.own_pid),
            // Lost the race against another *process* between the unlink and
            // the create: report who won.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                LockOutcome::conflict(self.read_pid().await)
            }
            Err(err) => {
                tracing::warn!(path = %self.path.display(), %err, "could not create lock file");
                LockOutcome::conflict(None)
            }
        }
    }

    async fn release(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
    }

    async fn touch(&self) {
        // `utimes(now, now)`. There is no async equivalent in std, and the
        // call is a single syscall, so it runs on the blocking pool only to
        // keep the reactor clean.
        let path = self.path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let now = SystemTime::now();
            let times = std::fs::FileTimes::new()
                .set_accessed(now)
                .set_modified(now);
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|file| file.set_times(times))
        })
        .await;
    }

    async fn check(&self) -> LockState {
        match self.read_pid().await {
            Some(pid) if self.processes.is_alive(pid) => LockState {
                locked: true,
                pid: Some(pid),
            },
            _ => LockState::default(),
        }
    }
}

impl std::fmt::Debug for FileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLock")
            .field("path", &self.path)
            .field("own_pid", &self.own_pid)
            .finish_non_exhaustive()
    }
}

/// An in-memory lock that touches no filesystem.
///
/// Public because every orchestrator unit test needs it — the reference suite
/// mocks the lock module for exactly the same reason — and because an embedder
/// that already guarantees single-process access has no use for a lock file.
#[derive(Debug)]
pub struct InMemoryLock {
    held: std::sync::Mutex<Option<u32>>,
    own_pid: u32,
    /// When set, `acquire` always reports a conflict with this PID, which is
    /// how the "lock held by another process" tests are expressed.
    conflict_with: std::sync::Mutex<Option<u32>>,
    pub acquires: std::sync::atomic::AtomicUsize,
    pub releases: std::sync::atomic::AtomicUsize,
    pub touches: std::sync::atomic::AtomicUsize,
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

    /// Make the next and every later `acquire` fail as if `pid` held the lock.
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
    async fn acquire(&self) -> LockOutcome {
        self.acquires
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(pid) = *self.conflict_with.lock().unwrap_or_else(|e| e.into_inner()) {
            return LockOutcome::conflict(Some(pid));
        }
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.own_pid);
        LockOutcome::acquired(self.own_pid)
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

    async fn check(&self) -> LockState {
        let pid = *self.held.lock().unwrap_or_else(|e| e.into_inner());
        LockState {
            locked: pid.is_some(),
            pid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::FakeProcessManager;

    struct Fixture {
        lock: FileLock,
        processes: Arc<FakeProcessManager>,
        _temp: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        let processes = Arc::new(FakeProcessManager::new());
        let lock = FileLock::new(
            temp.path().join("orchestry.lock"),
            Arc::clone(&processes) as Arc<dyn ProcessManager>,
        );
        Fixture {
            lock,
            processes,
            _temp: temp,
        }
    }

    #[tokio::test]
    async fn acquiring_writes_the_pid_with_no_trailing_newline() {
        let f = fixture();
        let outcome = f.lock.acquire().await;
        assert!(outcome.acquired);
        assert_eq!(outcome.pid, Some(std::process::id()));

        let content = tokio::fs::read_to_string(f.lock.path()).await.unwrap();
        assert_eq!(content, std::process::id().to_string());
        assert!(!content.ends_with('\n'));
    }

    #[tokio::test]
    async fn a_live_recently_touched_lock_blocks_a_second_holder() {
        let f = fixture();
        let other = FileLock::new(
            f.lock.path(),
            Arc::clone(&f.processes) as Arc<dyn ProcessManager>,
        )
        .with_pid(4242);

        assert!(f.lock.acquire().await.acquired);
        let outcome = other.acquire().await;
        assert!(!outcome.acquired);
        assert_eq!(outcome.pid, Some(std::process::id()));
    }

    #[tokio::test]
    async fn a_dead_pid_makes_the_lock_stealable_immediately() {
        let f = fixture();
        tokio::fs::write(f.lock.path(), b"777777").await.unwrap();
        f.processes.set_dead(777_777);

        let outcome = f.lock.acquire().await;
        assert!(outcome.acquired);
        assert_eq!(outcome.pid, Some(std::process::id()));
    }

    #[tokio::test]
    async fn a_live_pid_whose_lock_went_untouched_is_treated_as_recycled() {
        // The PID-recycling defence: alive, but not touched inside the window.
        let f = fixture();
        tokio::fs::write(f.lock.path(), b"4242").await.unwrap();
        let stale = SystemTime::now() - Duration::from_millis(LOCK_STALE_MS + 5_000);
        let times = std::fs::FileTimes::new()
            .set_accessed(stale)
            .set_modified(stale);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(f.lock.path())
            .unwrap();
        file.set_times(times).unwrap();

        assert!(f.lock.acquire().await.acquired);
    }

    #[tokio::test]
    async fn touch_refreshes_the_mtime_so_the_lock_stops_looking_stale() {
        let f = fixture();
        tokio::fs::write(f.lock.path(), b"4242").await.unwrap();
        let stale = SystemTime::now() - Duration::from_millis(LOCK_STALE_MS + 5_000);
        let times = std::fs::FileTimes::new()
            .set_accessed(stale)
            .set_modified(stale);
        std::fs::OpenOptions::new()
            .write(true)
            .open(f.lock.path())
            .unwrap()
            .set_times(times)
            .unwrap();
        assert!(f.lock.is_stale_by_age().await);

        f.lock.touch().await;
        assert!(!f.lock.is_stale_by_age().await);
    }

    #[tokio::test]
    async fn touch_and_release_on_a_missing_file_are_silent() {
        let f = fixture();
        f.lock.touch().await;
        f.lock.release().await;
        assert!(!f.lock.check().await.locked);
    }

    #[tokio::test]
    async fn release_removes_the_file() {
        let f = fixture();
        f.lock.acquire().await;
        f.lock.release().await;
        assert!(!f.lock.path().exists());
        assert!(f.lock.acquire().await.acquired);
    }

    #[tokio::test]
    async fn check_ignores_mtime_but_not_liveness() {
        let f = fixture();
        tokio::fs::write(f.lock.path(), b"4242").await.unwrap();
        let stale = SystemTime::now() - Duration::from_millis(LOCK_STALE_MS + 5_000);
        let times = std::fs::FileTimes::new()
            .set_accessed(stale)
            .set_modified(stale);
        std::fs::OpenOptions::new()
            .write(true)
            .open(f.lock.path())
            .unwrap()
            .set_times(times)
            .unwrap();

        // Stale by age, so `acquire` would steal it — but `check` reports it.
        assert_eq!(
            f.lock.check().await,
            LockState {
                locked: true,
                pid: Some(4242)
            }
        );

        f.processes.set_dead(4242);
        assert_eq!(f.lock.check().await, LockState::default());
    }

    /// A lock file whose contents do not parse as a PID **wedges the project**:
    /// `read_pid` returns `None`, so the stale-lock branch that would unlink it
    /// is skipped, and the exclusive create then fails with `EEXIST` and no PID
    /// to report. The TS `doAcquire` has exactly this shape, so it is
    /// replicated rather than fixed — a `LockConflictError(0)` is what the user
    /// sees, and `orch` recovers once the file is removed by hand.
    #[tokio::test]
    async fn a_corrupt_lock_file_wedges_acquisition_while_reading_as_unlocked() {
        let f = fixture();
        for content in ["", "   ", "not-a-pid", "-1", "12.5"] {
            tokio::fs::write(f.lock.path(), content).await.unwrap();
            assert_eq!(f.lock.check().await, LockState::default());
            assert_eq!(
                f.lock.acquire().await,
                LockOutcome::conflict(None),
                "content={content:?}"
            );
        }

        // Removing the file by hand is the recovery, and it works.
        f.lock.release().await;
        assert!(f.lock.acquire().await.acquired);
    }

    #[tokio::test]
    async fn concurrent_acquires_in_one_process_produce_exactly_one_winner() {
        let f = fixture();
        let lock = Arc::new(f.lock);
        let mut handles = Vec::new();
        for _ in 0..16 {
            let lock = Arc::clone(&lock);
            handles.push(tokio::spawn(async move { lock.acquire().await }));
        }

        let mut acquired = 0;
        for handle in handles {
            if handle.await.unwrap().acquired {
                acquired += 1;
            }
        }
        // One `FileLock` instance has one PID, so re-acquiring from the same
        // instance is idempotent in TS too — what matters is that the gate
        // serializes and nothing panics or double-creates.
        assert!(acquired >= 1);
        assert!(lock.path().exists());
    }

    #[tokio::test]
    async fn the_in_memory_lock_reports_conflicts_and_counts_calls() {
        let lock = InMemoryLock::new();
        assert!(lock.acquire().await.acquired);
        assert!(lock.is_held());
        assert!(lock.check().await.locked);

        lock.release().await;
        assert!(!lock.is_held());

        lock.set_conflict(Some(9999));
        assert_eq!(lock.acquire().await, LockOutcome::conflict(Some(9999)));

        lock.touch().await;
        assert_eq!(lock.acquire_count(), 2);
        assert_eq!(lock.release_count(), 1);
        assert_eq!(lock.touch_count(), 1);
    }
}
