//! The port traits every other crate compiles against.
//!
//! `orch-core` holds **only** trait definitions and the types that appear in
//! their signatures. It exists as its own crate because the storage and adapter
//! interfaces are imported by the orchestrator — a cycle in Rust unless the
//! traits live apart. That separation is what lets `orch-engine` be unit-tested
//! with in-memory fakes and `orch-infra` be swapped wholesale.
//!
//! The trait surface is **frozen** after WP-0; see `docs/TRAITS-FROZEN.md`.

// Stricter than the workspace lints, for library code only (integration tests are separate crates and
// `clippy.toml` allows unwrap and print inside `#[cfg(test)]`). See AGENTS.md, "Rust coding standards".
#![deny(
    unreachable_pub,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod adapter;
pub mod clock;
pub mod error;
pub mod ids;
pub mod process;
pub mod remote;
pub mod skills;
pub mod storage;
pub mod template;
pub mod workspace;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use adapter::{
    Adapter, AdapterError, AdapterRegistry, AdapterTestResult, AgentEvent, AgentEventKind,
    ExecuteHandle, ExecuteParams,
};
pub use clock::Clock;
pub use error::{Error, Result};
pub use ids::{ID_ALPHABET_LEN, ID_LENGTH, IdGenerator, is_well_formed_id};
pub use process::{
    DEFAULT_KILL_GRACE, ExitNotifier, ExitOutcome, ExitSignal, KILL_POLL_INTERVAL, ProcessManager,
    Signal, SpawnOptions, SpawnedChild, StdioMode,
};
pub use skills::{SkillLoader, is_valid_skill_name};
pub use storage::{
    AgentStore, ConfigStore, ContextEntry, ContextStore, GlobalConfigStore, GoalFilter, GoalStore,
    MessageStore, RunStore, StateStore, TaskFilter, TaskStore, TeamStore,
};
pub use template::{
    AgentContext, AgentInfo, DEFAULT_RENDER_TIMEOUT_MS, GoalContext, MAX_CONTEXT_ENTRIES,
    MessageContext, ProjectContext, PromptContext, RetryContext, SharedContextEntry, TaskContext,
    TemplateEngine,
};
pub use workspace::{MergeResult, PrepareResult, WorkspaceManager};
