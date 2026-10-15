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
