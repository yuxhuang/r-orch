//! Implementations of the `orch-core` port traits.
//!
//! Module layout follows ARCHITECTURE §3.3. WP-1/WP-2/WP-3 own
//! `storage`, `{adapters,process,models}` and `{template,skills,workspace,
//! clipboard}` respectively; `clock` and `ids` are the two real seams behind
//! `orch_core::{Clock, IdGenerator}` and were added by WP-0b because the
//! walking skeleton needs them to mint ids and timestamps.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

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
