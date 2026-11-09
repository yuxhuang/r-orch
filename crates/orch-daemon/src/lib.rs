//! The per-project orch daemon (design D1–D3, spec R1–R7).
//!
//! Exactly one process per project owns `.orchestry/`: its stores, its agent
//! processes, dispatch, expiry and the peer runtime. Every other entry point
//! is a client over the local socket using `orch-proto`.
//!
//! The public surface is deliberately narrow: [`prepare_start`] (and its
//! async wrapper [`start`]), [`initialize`], the runtime `.gitignore` entries
//! `orch init` shares ([`RUNTIME_GITIGNORE_ENTRIES`]) and, behind the `testing`
//! feature, [`testing::DaemonHarness`]. Nothing here re-exports a store implementation or an arbitrary execute closure.
//!
//! **PR 1 skeleton (S1a-api).** Signatures only; bodies return
//! [`DaemonError::Unimplemented`] naming the task that fills them:
//!
//! | Module | Owner task |
//! |---|---|
//! | [`lifecycle`], `guard`, `discovery`, `listener`, `session`, [`testing`] | S1a-impl (senior-eng) |
//! | [`coordinator`], [`ops`] handlers | S1b (senior-eng-2) |
//! | [`dispatch`] loop, [`peer`] runtime, NDJSON lift in orch-peer | S1d (senior-eng) |

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod coordinator;
mod detach;
mod discovery;
mod dispatch;
mod env;
mod error;
mod gitignore;
mod guard;
pub mod lifecycle;
mod listener;
pub mod ops;
mod peer;
mod session;
#[cfg(feature = "testing")]
pub mod testing;

pub use discovery::select_project_root;
pub use error::DaemonError;
pub use gitignore::{
    GitignoreError, GitignoreErrorCode, RUNTIME_GITIGNORE_ENTRIES, RUNTIME_GITIGNORE_HEADER,
    ensure_runtime_gitignore,
};
pub use lifecycle::{
    DaemonExit, DispatchMode, InitOptions, InitOutcome, PreparedStart,
    StartOptions, initialize, prepare_start, start,
};
