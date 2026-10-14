//! One module per command group, matching PARITY.md §2.2.
//!
//! WP-0b implemented `init`, `agent add`, `task add` and `task list`. WP-5
//! fills in the rest.

pub mod agent;
pub mod config;
pub mod context;
pub mod goal;
pub mod init;
pub mod msg;
pub mod org;
pub mod run;
pub mod status;
pub mod task;
pub mod team;
