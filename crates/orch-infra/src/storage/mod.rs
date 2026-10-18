//! File-based stores (WP-1 extends this; WP-0b wrote the walking-skeleton
//! subset).
//!
//! The three contract rules in `orch_core::storage`'s module docs are
//! implemented here: `save` pushes to the end of `_index.json`, sorts over
//! index order are stable, and every YAML read applies merge keys.

pub mod agent;
pub mod cached;
pub mod config;
pub mod context;
pub mod fs_utils;
pub mod global_config;
pub mod goal;
pub mod index;
pub mod lock;
pub mod message;
pub mod org_templates;
pub mod paths;
pub mod remote;
pub mod run;
pub mod state;
pub mod task;
pub mod team;

pub use agent::FileAgentStore;
pub use cached::{CachedAgentStore, CachedGoalStore, CachedTaskStore};
pub use config::FileConfigStore;
pub use context::FileContextStore;
pub use global_config::FileGlobalConfigStore;
pub use goal::FileGoalStore;
pub use index::IndexManager;
pub use message::FileMessageStore;
pub use paths::{ORCHESTRY_DIR, Paths, find_project_root, sanitize_id, validate_workspace_path};
pub use run::FileRunStore;
pub use state::FileStateStore;
pub use task::FileTaskStore;
pub use team::FileTeamStore;

pub use remote::{FileRemoteStore, RemoteAgentDirectory};

/// Test helpers for the advisory (`flock`) locks the stores take.
#[cfg(test)]
pub(crate) mod test_locks {
    use std::fs::{File, TryLockError};
    use std::time::{Duration, Instant};

    /// Take `file`'s lock, waiting briefly while something else still holds it.
    ///
    /// A test cannot expect `try_lock` to succeed the instant a lock is
    /// released: a child that another test in the same binary is spawning
    /// holds a copy of every open descriptor between `fork` and `exec`, and a
    /// `flock` stays held while any copy is open. The window is short, so poll.
    pub(crate) fn lock_when_free(file: &File) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("lock still held after 5 s: {err:?}"),
            }
        }
    }
}
