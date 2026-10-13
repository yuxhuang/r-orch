//! File-based stores (WP-1 extends this; WP-0b wrote the walking-skeleton
//! subset).
//!
//! The three contract rules in `orch_core::storage`'s module docs are
//! implemented here: `save` pushes to the end of `_index.json`, sorts over
//! index order are stable, and every YAML read applies merge keys.

pub mod agent;
pub mod config;
pub mod fs_utils;
pub mod index;
pub mod paths;
pub mod state;
pub mod task;

pub use agent::FileAgentStore;
pub use config::FileConfigStore;
pub use index::IndexManager;
pub use paths::{ORCHESTRY_DIR, Paths, find_project_root, sanitize_id, validate_workspace_path};
pub use state::FileStateStore;
pub use task::FileTaskStore;
