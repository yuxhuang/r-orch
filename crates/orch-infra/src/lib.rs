//! Implementations of the `orch-core` port traits.
//!
//! Module layout follows ARCHITECTURE §3.3. WP-1/WP-2/WP-3 own
//! `storage`, `{adapters,process,models}` and `{template,skills,workspace,
//! clipboard}` respectively; `clock` and `ids` are the two real seams behind
//! `orch_core::{Clock, IdGenerator}` and were added by WP-0b because the
//! walking skeleton needs them to mint ids and timestamps.

pub mod adapters;
pub mod clipboard;
pub mod clock;
pub mod ids;
pub mod models;
pub mod process;
pub mod skills;
pub mod storage;
pub mod template;
pub mod workspace;

pub use clock::SystemClock;
pub use ids::NanoIdGenerator;
