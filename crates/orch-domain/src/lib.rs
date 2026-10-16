//! Pure domain types and functions, ported from `src/domain/*.ts`.
//!
//! No I/O and no async: every item here is either a serializable record or a
//! pure function. The serde attributes on the records are the on-disk and
//! `--json` wire contract (PARITY.md §6.4, §4.2) — field order mirrors the
//! TypeScript interface declaration order and every TS `?` field is skipped
//! when absent. The one exception is [`Message::to_agent_id`], whose TS type
//! is `string | null` and which therefore emits `null`.

pub mod agent;
pub mod config;
pub mod error;
pub mod events;
pub mod global_config;
pub mod goal;
pub mod message;
pub mod model_tiers;
pub mod remote;
pub mod run;
pub mod scope;
pub mod shop;
pub mod state;
pub mod task;
pub mod team;
pub mod transitions;

pub use agent::{
    Agent, AgentConfig, AgentLastError, AgentStats, AgentStatus, ApprovalPolicy, CreateAgentInput,
    ReasoningEffort,
};
pub use config::{
    AgentDefaults, Defaults, OrchestratorConfig, PeerConfig, ProjectConfig, PromptConfig,
    SchedulingConfig, TaskDefaults, deep_merge, default_config, get_by_path, set_by_path,
};
pub use error::{
    AdapterErrorHint, AdapterErrorKind, DomainError, ERROR_HINTS, Result, classify_adapter_error,
    error_hint, extract_error_message, is_model_metadata_warning,
};
pub use events::{OrchestratorEvent, OrchestratorEventType};
pub use global_config::{
    ACTIVITY_FILTER_PRESETS, ActivityFilterPreset, GlobalConfig, NotificationPreferences,
    TUI_PALETTE_NAMES, TuiPaletteName, TuiPreferences, default_global_config, is_tui_palette_name,
};
pub use goal::{
    CreateGoalInput, GOAL_STATUSES, Goal, GoalStatus, goal_status_order, is_goal_terminal,
};
pub use message::{
    CreateMessageInput, DEFAULT_MESSAGE_TTL_MS, MAX_MESSAGE_TTL_MS, Message, MessageChannel,
    MessageStatus,
};
pub use model_tiers::{
    AdapterKind, MODEL_TIER_MAP, ModelTier, SUPPORTED_ADAPTERS, default_model_for_adapter,
    is_adapter_kind, is_model_tier, resolve_model, tier_map_for,
};
pub use run::{
    Run, RunEvent, RunEventType, RunStatus, TokenUsage, TokenUsageOpts, create_token_usage,
};
pub use scope::{ScopeIndex, scopes_overlap};
pub use shop::{
    AGENT_CREATOR_ROLE, AGENT_SHOP_TEMPLATES, AgentShopTemplate, ORG_TEMPLATES, OrgAgentEntry,
    OrgTemplate, default_agents, get_org_template_by_key, get_shop_template_by_key,
};
pub use state::{OrchestratorState, RetryEntry, RunningEntry, StateStats, default_state};
pub use task::{
    AUTONOMOUS_LABEL, AWAITING_APPROVAL_LABEL, CreateTaskInput, ReviewCriterion, ReviewResult,
    Task, TaskProof, TaskStatus, WorkspaceMode,
};
pub use team::{
    CreateTeamInput, DEFAULT_TEAM_CONFIG, PartialTeamConfig, Team, TeamConfig, TeamMember,
    TeamMemberRole, TeamStatus,
};
pub use transitions::{
    TaskSource, calculate_retry_delay, can_transition, is_blocked, is_dispatchable, is_terminal,
    resolve_completion_status, resolve_failure_status,
};

pub use remote::{
    AWAITING_APPROVAL_REASON, InboundStatus, RemoteConfig, RemotePhase, RemoteRef, RemoteRunRef,
    RemoteStatus,
};
