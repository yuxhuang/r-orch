//! The ported `test/unit/application/orchestrator*.test.ts` suites.
//!
//! One module per TS file, plus [`harness`], which is the Rust equivalent of
//! the TS `buildDeps()` — complete `OrchestratorDeps` with **real** services,
//! event bus and registry, and only the stores and I/O edges faked.
//!
//! Two deliberate differences from the TS helper, both from ARCHITECTURE §5.6:
//! the clock and the id generator are injected, so a test can age a run by
//! exactly `n` milliseconds instead of sleeping, and ids are predictable.

mod auto_review_approve;
mod autonomous;
mod bugfixes_2026_03;
mod core;
mod defensive_validation;
mod dispatch_worktree;
mod endurance;
mod error_kind_propagation;
mod force_review;
pub(crate) mod harness;
mod mutex;
mod parallel_race;
mod perf_opts;
mod resilience;
mod retry_race;
mod skills;
