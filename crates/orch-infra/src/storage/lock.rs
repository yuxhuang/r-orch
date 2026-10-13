//! PID lock file — ported from `src/infrastructure/storage/lock.ts`.
//!
//! Only one `orch run --watch` may hold the lock; one-shot commands never take
//! it. The file holds the holder's PID as a decimal string with **no trailing
//! newline** (PARITY.md §6.7).
//!
//! Three properties are load-bearing and easy to lose:
//!
//! - **Creation is `O_CREAT|O_EXCL`**, so two processes racing to create it
//!   cannot both win. The liveness check before it is an optimization, not the
//!   guarantee — the TOCTOU window between "no lock" and "create" is closed by
//!   the kernel, not by the check.
//! - **A live PID is not enough.** PIDs recycle, so a lock whose mtime has not
//!   been touched within [`LOCK_STALE`] is treated as abandoned even when its
//!   PID resolves to a running process. `touch_lock` on every tick is what
//!   keeps a real holder's lock fresh.
//! - **`EPERM` means alive.** The process exists; we merely may not signal it.

use std::path::Path;
use std::time::{Duration, SystemTime};

use orch_core::error::{Error, Result};
use orch_domain::DomainError;

/// A lock untouched for longer than this is stale. High enough to survive a
/// slow tick — worktree operations plus adapter spawns.
pub const LOCK_STALE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockResult {
    pub acquired: bool,
    /// The holder's PID: ours when acquired, theirs when not.
    pub pid: Option<u32>,
}

/// Try to acquire the lock, clearing a stale one first.
///
/// Serialized within the process: two concurrent async callers would otherwise
/// race on the same file, and the in-process case is the common one (a tick
/// and a CLI command in the same binary).
pub async fn acquire_lock(lock_path: &Path) -> Result<LockResult> {
    let _guard = acquire_mutex().lock().await;
    do_acquire(lock_path).await
}

fn acquire_mutex() -> &'static tokio::sync::Mutex<()> {
    static MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &MUTEX
}

async fn do_acquire(lock_path: &Path) -> Result<LockResult> {
    if let Some(existing) = read_lock_pid(lock_path).await {
        if is_process_alive(existing) && !is_stale_by_age(lock_path).await {
            return Ok(LockResult {
                acquired: false,
                pid: Some(existing),
            });
        }
        // Dead PID, or alive but untouched for too long: the OS has probably
        // recycled the PID for something unrelated.
        let _ = tokio::fs::remove_file(lock_path).await;
    }

    if let Some(parent) = lock_path.parent() {
        super::fs_utils::ensure_dir(parent).await?;
    }

    let pid = std::process::id();
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
        .await
    {
        Ok(mut file) => {
            use tokio::io::AsyncWriteExt;
            // No trailing newline — the format is contract.
            file.write_all(pid.to_string().as_bytes())
                .await
                .map_err(|err| {
                    Error::io(format!("write {}", lock_path.display()), lock_path, err)
                })?;
            file.flush().await.map_err(|err| {
                Error::io(format!("flush {}", lock_path.display()), lock_path, err)
            })?;
            Ok(LockResult {
                acquired: true,
                pid: Some(pid),
            })
        }
        // Someone else created it between the check and here. The kernel, not
        // the check above, is what makes this safe.
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(LockResult {
            acquired: false,
            pid: read_lock_pid(lock_path).await,
        }),
        Err(err) => Err(Error::io(
            format!("create {}", lock_path.display()),
            lock_path,
            err,
        )),
    }
}

/// Release the lock. A missing file is not an error.
pub async fn release_lock(lock_path: &Path) {
    let _ = tokio::fs::remove_file(lock_path).await;
}

/// Update the lock's mtime to prove the holder is still alive. Call it every
/// tick, or stale detection will eventually evict a healthy holder.
pub async fn touch_lock(lock_path: &Path) {
    // `fs.utimes` has no std/tokio equivalent, and the portable way to bump
    // mtime is a zero-length write through an append handle.
    if let Ok(file) = tokio::fs::OpenOptions::new()
        .append(true)
        .open(lock_path)
        .await
    {
        use tokio::io::AsyncWriteExt;
        let mut file = file;
        let _ = file.write_all(b"").await;
        let _ = file.flush().await;
        // A zero-length write does not always update mtime, so set it
        // explicitly where the platform allows it.
        let _ = filetime_now(lock_path).await;
    }
}

/// Set mtime to now. Separated so the fallback is visible.
async fn filetime_now(lock_path: &Path) -> std::io::Result<()> {
    let path = lock_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = std::fs::OpenOptions::new().write(true).open(&path)?;
        file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Whether the lock is held by a live process.
pub async fn check_lock(lock_path: &Path) -> LockResult {
    match read_lock_pid(lock_path).await {
        None => LockResult {
            acquired: false,
            pid: None,
        },
        Some(pid) if is_process_alive(pid) => LockResult {
            acquired: true,
            pid: Some(pid),
        },
        // Stale.
        Some(_) => LockResult {
            acquired: false,
            pid: None,
        },
    }
}

/// Acquire or fail with `LockConflictError` (exit 4).
///
/// A faithful port of the TS `requireLock`, which throws **only when a holder
/// PID is known**: an acquisition that fails without one — the file exists but
/// holds garbage — returns without error.
///
/// **`requireLock` has no callers in the TypeScript.** It is dead code there,
/// so this quirk describes nothing ORCH actually does, and it must not be read
/// as "ORCH proceeds without the lock on a corrupt lock file" — it does not.
/// The live path is `Orchestrator::with_temporary_lock` / `start_watch`, which
/// call `acquire` and reject **any** failed acquisition. Measured against the
/// oracle: a garbage or empty lock file makes `serve --once` and `run <id>`
/// exit 4 with `Orchestrator already running (PID: undefined)`, leaving the
/// lock file untouched. Kept in step with the TS only so that a future caller
/// there has a matching counterpart here.
pub async fn require_lock(lock_path: &Path) -> Result<()> {
    let result = acquire_lock(lock_path).await?;
    match (result.acquired, result.pid) {
        (false, Some(pid)) => Err(Error::Domain(DomainError::LockConflict(pid))),
        _ => Ok(()),
    }
}

/// The PID in the lock file, or `None` when it is missing or unparseable.
async fn read_lock_pid(lock_path: &Path) -> Option<u32> {
    let content = tokio::fs::read_to_string(lock_path).await.ok()?;
    content.trim().parse::<u32>().ok()
}

async fn is_stale_by_age(lock_path: &Path) -> bool {
    let Ok(meta) = tokio::fs::metadata(lock_path).await else {
        // Unreadable: treat as stale, as the TS `catch` does.
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > LOCK_STALE)
}

/// `kill(pid, 0)` is `Ok`, **or** fails with `EPERM` — the process exists but
/// we may not signal it.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
pub fn is_process_alive(_pid: u32) -> bool {
    // Windows support is out of scope (ARCHITECTURE §2 A3). Reporting every
    // PID as alive is the conservative answer: it refuses the lock rather than
    // stealing it from a live holder.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs};

    /// The lock tests share a process-wide acquire mutex and assert on PIDs,
    /// so they are serialized against each other.
    static LOCK_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A PID that cannot be running: above the maximum any platform assigns.
    const DEAD_PID: u32 = 4_000_000_000;

    fn lock_path(root: &Path) -> std::path::PathBuf {
        clear_ensured_dirs();
        root.join(".orchestry/orchestry.lock")
    }

    #[tokio::test]
    async fn acquires_an_unheld_lock_and_writes_its_pid_without_a_newline() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());

        let result = acquire_lock(&path).await.unwrap();
        assert!(result.acquired);
        assert_eq!(result.pid, Some(std::process::id()));

        // PARITY.md §6.7: decimal PID, no trailing newline.
        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(raw, std::process::id().to_string());
        assert!(!raw.ends_with('\n'));
    }

    #[tokio::test]
    async fn a_lock_held_by_this_live_process_is_refused() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        acquire_lock(&path).await.unwrap();

        let second = acquire_lock(&path).await.unwrap();
        assert!(!second.acquired);
        assert_eq!(second.pid, Some(std::process::id()));
    }

    #[tokio::test]
    async fn require_lock_fails_with_exit_4_and_the_holder_pid() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        require_lock(&path).await.unwrap();

        let err = require_lock(&path).await.expect_err("already held");
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            err.to_string(),
            format!("Orchestrator already running (PID: {})", std::process::id())
        );
        assert_eq!(err.hint().as_deref(), Some("Use: orch status"));
    }

    #[tokio::test]
    async fn a_dead_pid_makes_the_lock_stale_and_stealable() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        atomic_write(&path, &DEAD_PID.to_string()).await.unwrap();

        let result = acquire_lock(&path).await.unwrap();
        assert!(result.acquired, "a dead holder must not keep the lock");
        assert_eq!(result.pid, Some(std::process::id()));
    }

    #[tokio::test]
    async fn a_live_pid_whose_lock_went_untouched_is_treated_as_recycled() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        // Our own PID, so the liveness check passes...
        atomic_write(&path, &std::process::id().to_string())
            .await
            .unwrap();
        // ...but the mtime is older than the staleness window, so the PID is
        // assumed to have been recycled.
        set_mtime_ago(&path, LOCK_STALE + Duration::from_secs(1)).await;

        assert!(acquire_lock(&path).await.unwrap().acquired);
    }

    #[tokio::test]
    async fn touching_the_lock_keeps_a_live_holder_from_being_evicted() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        acquire_lock(&path).await.unwrap();
        set_mtime_ago(&path, LOCK_STALE + Duration::from_secs(1)).await;

        touch_lock(&path).await;
        let result = acquire_lock(&path).await.unwrap();
        assert!(!result.acquired, "a touched lock must still be held");
    }

    #[tokio::test]
    async fn releasing_lets_the_next_caller_acquire() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        acquire_lock(&path).await.unwrap();

        release_lock(&path).await;
        assert!(!tokio::fs::try_exists(&path).await.unwrap());
        assert!(acquire_lock(&path).await.unwrap().acquired);

        // Releasing a missing lock is a no-op.
        release_lock(&path).await;
        release_lock(&path).await;
    }

    #[tokio::test]
    async fn check_lock_reports_held_stale_and_absent() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());

        assert_eq!(
            check_lock(&path).await,
            LockResult {
                acquired: false,
                pid: None
            }
        );

        acquire_lock(&path).await.unwrap();
        assert_eq!(
            check_lock(&path).await,
            LockResult {
                acquired: true,
                pid: Some(std::process::id())
            }
        );

        // A dead PID reads as unlocked, and `check_lock` does not mutate.
        atomic_write(&path, &DEAD_PID.to_string()).await.unwrap();
        assert_eq!(
            check_lock(&path).await,
            LockResult {
                acquired: false,
                pid: None
            }
        );
        assert!(tokio::fs::try_exists(&path).await.unwrap());
    }

    /// A corrupt lock file blocks acquisition **and** reports no holder, so
    /// `require_lock` returns `Ok` without the lock being held.
    ///
    /// That is the original's behaviour, replicated: `readLockPid` yields
    /// `null`, so the stale-removal branch is skipped; `open(path, 'wx')` then
    /// fails `EEXIST` and the result carries no PID; and `requireLock` only
    /// throws when a PID is known. The orchestrator proceeds without mutual
    /// exclusion and says nothing.
    ///
    /// Recorded rather than fixed: diverging here would change `run --watch`
    /// behaviour against the oracle, and the file is only ever corrupt after a
    /// crash mid-write — which `O_CREAT|O_EXCL` plus a single small write
    /// makes very unlikely.
    #[tokio::test]
    async fn a_corrupt_lock_file_blocks_acquisition_without_naming_a_holder() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();

        for content in ["", "   ", "not-a-pid", "12.5", "-1"] {
            atomic_write(&path, content).await.unwrap();
            assert_eq!(read_lock_pid(&path).await, None, "{content:?}");
            assert_eq!(check_lock(&path).await.pid, None, "{content:?}");

            let result = acquire_lock(&path).await.unwrap();
            assert!(!result.acquired, "{content:?}");
            assert_eq!(result.pid, None, "{content:?}");

            // And the consequence: no error, no lock.
            require_lock(&path)
                .await
                .expect("no holder PID, so no conflict is raised");
        }
    }

    #[tokio::test]
    async fn removing_a_corrupt_lock_restores_normal_acquisition() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        atomic_write(&path, "not-a-pid").await.unwrap();
        assert!(!acquire_lock(&path).await.unwrap().acquired);

        release_lock(&path).await;
        assert!(acquire_lock(&path).await.unwrap().acquired);
    }

    #[tokio::test]
    async fn a_pid_with_surrounding_whitespace_still_parses() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        // The TS `parseInt(content.trim())` tolerates a trailing newline, so a
        // lock written by an older build still reads.
        atomic_write(&path, &format!("{}\n", std::process::id()))
            .await
            .unwrap();

        assert_eq!(read_lock_pid(&path).await, Some(std::process::id()));
        assert!(!acquire_lock(&path).await.unwrap().acquired);
    }

    #[tokio::test]
    async fn concurrent_acquires_in_one_process_yield_exactly_one_winner() {
        let _serial = LOCK_TESTS.lock().await;
        let temp = tempfile::tempdir().unwrap();
        let path = lock_path(temp.path());

        let mut handles = Vec::new();
        for _ in 0..16 {
            let path = path.clone();
            handles.push(tokio::spawn(
                async move { acquire_lock(&path).await.unwrap() },
            ));
        }
        let mut acquired = 0;
        for handle in handles {
            if handle.await.unwrap().acquired {
                acquired += 1;
            }
        }
        assert_eq!(
            acquired, 1,
            "the in-process mutex must serialize acquisition"
        );
    }

    #[test]
    fn liveness_treats_this_process_as_alive_and_a_huge_pid_as_dead() {
        assert!(is_process_alive(std::process::id()));
        assert!(!is_process_alive(DEAD_PID));
        // PID 1 exists on every unix and is usually owned by root, so it
        // exercises the EPERM branch when the tests do not run as root.
        assert!(is_process_alive(1));
    }

    /// Backdate a file's mtime. `std::fs::FileTimes` is the portable way in.
    async fn set_mtime_ago(path: &Path, ago: Duration) {
        let when = SystemTime::now() - ago;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(when))
                .unwrap();
        })
        .await
        .unwrap();
    }
}
