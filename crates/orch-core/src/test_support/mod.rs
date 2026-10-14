//! Test doubles and entity factories, behind the `test-support` feature so
//! they never ship in a release binary.
//!
//! Ported from `test/unit/application/helpers.ts`. The factory defaults are
//! **identical** to the TS ones, including the two that a lot of behaviour
//! hangs off: [`factories::make_agent`] sets `approval_policy: auto` (which is
//! why so many tests observe `review → done` with no explicit approve) and
//! `stall_timeout_ms: 300_000`.
//!
//! Three pieces are shared by every double and are worth knowing about before
//! writing tests against them:
//!
//! - [`Recorder`] records calls, replacing `vi.fn()`'s call assertions.
//! - [`Faults`] injects failures, replacing `mockRejectedValue`.
//! - The store fakes **clone on both read and write**, exactly as the TS fakes
//!   `structuredClone`, so a caller cannot mutate stored state by accident.
//!
//! `build_deps`/`TestOrchestrator` are deliberately **not** here: they need
//! `OrchestratorDeps`, which lives in `orch-engine`. Compose them there from
//! these parts.

pub mod adapter;
pub mod clock;
pub mod factories;
pub mod faults;
pub mod hermetic;
pub mod ids;
pub mod process;
pub mod process_group;
pub mod recorder;
pub mod skills;
pub mod stores;
pub mod template;
pub mod workspace;

pub use adapter::{FakeAdapter, FakeExecuteCall, done_event, error_event, output_event};
pub use clock::{FIXED_NOW_ISO, FIXED_NOW_MS, FixedClock};
pub use factories::{FACTORY_TIMESTAMP, make_agent, make_run, make_task};
pub use faults::Faults;
pub use hermetic::assert_hermetic;
pub use ids::SeqIdGenerator;
pub use process::{FAKE_PID, FakeProcessManager, FakeProcessManagerCall};
pub use process_group::{
    GroupMember, describe, live_group_members, process_group_members, wait_for_no_live_members,
};
pub use recorder::Recorder;
pub use skills::FakeSkillLoader;
pub use stores::{
    AgentStoreCall, ConfigStoreCall, ContextStoreCall, FakeAgentStore, FakeConfigStore,
    FakeContextStore, FakeGlobalConfigStore, FakeGoalStore, FakeMessageStore, FakeRunStore,
    FakeStateStore, FakeTaskStore, FakeTeamStore, GlobalConfigStoreCall, GoalStoreCall,
    MessageStoreCall, RunStoreCall, StateStoreCall, TaskStoreCall, TeamStoreCall,
};
pub use template::{FakeRenderCall, FakeTemplateEngine, RENDERED_PROMPT};
pub use workspace::{FAKE_WORKSPACE, FakeWorkspaceManager, FakeWorkspaceManagerCall};
