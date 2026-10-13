//! Services, the orchestrator, the event bus and the container.
//!
//! WP-0b built the walking-skeleton subset: the event bus, `TaskService` and
//! `AgentService` create/list/get, and `container::build_light`. WP-4 extends
//! these in place and adds the remaining services, the orchestrator and
//! `build_full`.

pub mod agent_factory;
pub mod agent_service;
pub mod container;
pub mod doctor_service;
pub mod events;
pub mod goal_service;
pub mod message_service;
pub mod orchestrator;
pub mod review_runner;
pub mod run_service;
pub mod task_service;
pub mod team_service;

pub use agent_service::AgentService;
pub use container::{Engine, EngineSeams, build_light, build_light_with};
pub use events::{EventBus, EventSubscription};
pub use task_service::TaskService;
