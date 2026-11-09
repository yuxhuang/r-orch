//! The local daemon protocol: what crosses the project socket between the one
//! `orch` process that owns `.orchestry/` and every client (CLI, TUI, web, MCP).
//!
//! Design D1/D5/D6/D7 in the daemon-owned-state design and spec R6/R7/R20 are
//! the contract. This crate holds data and pure codecs only: no stores, no
//! services, no filesystem access and no process execution, so `orch-client`
//! can depend on it without gaining a store capability.
//!
//! - [`handshake`]: the **frozen** hello/welcome/reject envelope. It tolerates
//!   unknown fields so a stop intent and version diagnostics survive skew.
//! - [`frame`]: session frames after the handshake (requests, terminal
//!   `ok`/`err`, `part` streams, `ev` pushes) and their numeric limits.
//! - [`ops`]: one closed, typed request per operation in the D7 catalogue,
//!   its result type and its authoritative [`ops::OpClass`].
//! - [`error`]: the error envelope, exit codes and the uniform NoDaemon text.
//! - [`discovery`]: the `daemon.json` schema 1 record.
//! - [`ndjson`]: the bounded, transport-neutral newline-delimited JSON codec.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod bounded;
pub mod discovery;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod ndjson;
pub mod ops;
pub mod path;

/// Re-exported so [`for_each_op!`] can name domain result types from any crate.
#[doc(hidden)]
pub use orch_domain;

/// The local protocol revision. Starts at 1 and is bumped for **every**
/// operation wire or semantic change in the stack (spec R32): equal package
/// semver plus equal `LOCAL_PROTOCOL` is the only compatibility signal, and two
/// builds that differ in neither are deliberately indistinguishable.
pub const LOCAL_PROTOCOL: u32 = 1;

/// The exact package semver of this build. Ordinary sessions require it to be
/// equal on both sides (spec R6); only the frozen stop intent crosses skew.
pub const BINARY_VERSION: &str = env!("CARGO_PKG_VERSION");
