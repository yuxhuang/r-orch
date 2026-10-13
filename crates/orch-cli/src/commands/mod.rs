//! One module per command group, matching PARITY.md §2.2.
//!
//! WP-0b implemented `init`, `agent add`, `task add` and `task list`. WP-5
//! fills in the rest.

pub mod agent;
pub mod init;
pub mod task;
