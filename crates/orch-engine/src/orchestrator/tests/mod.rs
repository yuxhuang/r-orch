//! The orchestrator test suites.
//!
//! One module per suite, plus [`harness`], which builds complete
//! `OrchestratorDeps` with **real** services,
//! event bus and registry, and only the stores and I/O edges faked.
//!
//! Two seams from ARCHITECTURE §5.6:
//! the clock and the id generator are injected, so a test can age a run by
//! exactly `n` milliseconds instead of sleeping, and ids are predictable.

mod auto_review_approve;
mod autonomous;
mod bugfixes_2026_03;
mod collector_publication;
mod core;
mod defensive_validation;
mod dispatch_worktree;
mod endurance;
mod error_kind_propagation;
mod force_review;
pub(crate) mod harness;
mod mutex;
mod noop_publication;
mod parallel_race;
mod pause_prelaunch;
mod perf_opts;
mod prelaunch_final_list_error;
mod prelaunch_record_errors;
mod resilience;
mod retry_backoff;
mod retry_race;
mod skills;
mod stop_launch;
mod token_eco;

mod remote;
mod watch_restart;
