//! WP-2 — process spawning, the line readers, and the shared adapter spawn
//! path.
//!
//! Ported from `src/infrastructure/process/process-manager.ts`
//! (PARITY.md §8.4, §8.5).

pub mod line_reader;
pub mod manager;
pub mod spawn;

pub use line_reader::{MAX_LINE_LEN, read_lines, read_pi_lines};
pub use manager::SystemProcessManager;
pub use spawn::{ChildIo, ChildReader, ChildWriter, StderrPolicy, StderrTail, spawn_child};
