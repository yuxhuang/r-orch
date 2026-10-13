//! Services, the orchestrator, the event bus and the container.
//!
//! The engine is the application layer: it owns every behaviour that is not
//! persistence, process management or presentation. Each service is a direct
//! port of its `src/application/*.ts` counterpart and depends only on
//! `orch-core` port traits, which is what lets the whole layer be unit-tested
//! against in-memory fakes.

pub mod agent_factory;
pub mod agent_service;
pub mod container;
pub mod doctor_service;
pub mod events;
pub mod goal_service;
pub mod lock;
pub mod message_service;
pub mod orchestrator;
pub mod review_runner;
pub mod run_service;
pub mod task_service;
pub mod team_service;

pub use agent_factory::{is_mcp_skill, template_to_agent_input};
pub use agent_service::{AgentService, AgentStatsUpdate, UpdateAgentFields};
pub use container::{
    Engine, EngineSeams, FullEngine, build_full, build_full_with, build_light, build_light_with,
};
pub use doctor_service::{CheckStatus, DoctorCheck, DoctorReport, DoctorService};
pub use events::{EventBus, EventHandler, EventSubscription, Unsubscribe};
pub use goal_service::{GoalService, UpdateGoalFields};
pub use lock::{FileLock, InMemoryLock, LockResult, ProjectLock};
pub use message_service::MessageService;
pub use orchestrator::{Orchestrator, OrchestratorDeps, WatchOptions};
pub use review_runner::{
    CommandOutcome, CommandRunner, ProcessCommandRunner, ReviewRunner, ReviewRunnerOptions,
    all_passed, format_report,
};
pub use run_service::{CreateRunInput, RetryContext, RunService};
pub use task_service::{TaskService, UpdateTaskFields};
pub use team_service::TeamService;
