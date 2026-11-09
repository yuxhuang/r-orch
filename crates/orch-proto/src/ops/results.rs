//! Result types that are not already `orch-domain` records.
//!
//! Domain records (`Task`, `Agent`, `Goal`, `Message`, `Team`, `Run`, …) are
//! returned unchanged. The types here mirror engine/infra/CLI/TUI projections
//! that have no serde or live outside the domain crate; the source is named on
//! each. Frontend-only printing stays at the edge.

use crate::bounded::Id;
use crate::path::WirePath;
use orch_domain::org_templates::{AgentDefinition, Bundle};
use orch_domain::{
    Agent, ApprovalPolicy, CreateAgentInput, InboundStatus, ModelTier, NotificationPreferences,
    OrchestratorState, PromptMode, PromptModeSource, RemoteStatus, Task, Team, TuiPaletteName,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `status.get` (CLI `StatusJson`): local tasks, local agents and state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSummary {
    pub project_name: String,
    pub tasks: Vec<Task>,
    pub agents: Vec<Agent>,
    pub state: OrchestratorState,
}

/// `ui.bootstrap` (TUI `LaunchContext`), in one coordinated read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchContext {
    pub project_name: String,
    pub version: String,
    pub default_adapter: String,
    pub palette: TuiPaletteName,
    pub activity_filter: String,
    pub notifications: NotificationPreferences,
    pub max_concurrent: u32,
    /// Whether automatic dispatch is on (replaces `initial_watch_active`).
    pub dispatch_on: bool,
    pub prompt_mode: PromptMode,
    /// `startup_diagnostics` lines: `"{agent_name} ({agent_id}): {message}"`.
    pub startup_warnings: Vec<String>,
}

/// An authored document and its opaque base digest (design D7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionDocument {
    /// JSON text of the authored-only fields; runtime-owned fields excluded.
    pub document: String,
    pub digest: Id,
}

/// `task.attachment.read`. Large results stream as `result_chunk` parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentBytes {
    pub filename: String,
    pub size: u64,
    pub data_base64: String,
}

/// `attachment.stage_image`: a daemon-owned temporary file, used as an
/// attachment source path in a later create/update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedAttachment {
    pub path: WirePath,
    pub bytes: u64,
}

/// `AgentDiagnostic` (orch-engine prompt_mode.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDiagnostic {
    pub agent_id: String,
    pub agent_name: String,
    pub message: String,
}

/// `AuthoredAgent` (orch-engine agent_service.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoredAgent {
    pub agent: Agent,
    pub diagnostics: Vec<AgentDiagnostic>,
}

/// `EffectiveConfig` (CLI agent.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub prompt_mode: PromptMode,
    pub prompt_mode_source: PromptModeSource,
}

/// `agent.get`; `effective_config` only when requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config: Option<EffectiveConfig>,
}

/// `goal.get` (CLI/MCP goal projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalView {
    pub goal: orch_domain::Goal,
    pub tasks: Vec<Task>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
}

/// `ContextEntry` (orch-core storage.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEntry {
    pub key: String,
    pub value: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// `maintenance.expire_context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiryReport {
    pub removed: u64,
}

/// `config.get` / `config.set` / `config.global.*` (CLI `KeyValue`). A
/// missing key has no `value`. Config values are arbitrary authored YAML, so
/// this **result** carries JSON; request DTOs never do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValue {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// One TUI activity-history row (TUI `HistoryEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub timestamp: String,
    pub run_id: String,
    pub agent_id: String,
    pub task_id: String,
    pub event_type: String,
    pub data: String,
}

/// `run.history`: batches of history rows, newest runs first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHistory {
    pub batches: Vec<Vec<HistoryEntry>>,
}

/// TUI `EntityTiming`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTiming {
    pub started_at_ms: Option<i64>,
    pub elapsed_ms: Option<i64>,
}

/// `run.timings` (TUI `TimingSnapshot`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingSnapshot {
    pub tasks: BTreeMap<String, EntityTiming>,
    pub goals: BTreeMap<String, EntityTiming>,
    pub sampled_at_ms: i64,
}

/// `dispatch.set` and the dispatch field of status surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchState {
    pub dispatch_on: bool,
    /// A `dispatch.until_terminal` batch is active.
    pub batch_active: bool,
}

/// `dispatch.tick`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickResult {
    pub launched: Vec<String>,
}

/// `dispatch.run_task` / `dispatch.run_all`: terminal tasks and the exit
/// status the CLI reports (0, or 1 on failure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub tasks: Vec<Task>,
    pub exit_code: i32,
}

/// `dispatch.until_terminal` (`serve --once` `OnceResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchResult {
    AllDone,
    HasFailed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipStatus {
    Held,
    /// The PID lock went missing or foreign; dispatch is disabled.
    Lost,
}

/// `daemon.status`: live, never derived from `daemon.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub project_root: WirePath,
    pub socket: WirePath,
    pub protocol: u32,
    pub binary: String,
    pub instance: String,
    pub executable: WirePath,
    pub started_at: String,
    pub dispatch: DispatchState,
    pub running_tasks: Vec<String>,
    pub sessions: u32,
    pub session_cap: u32,
    /// Authenticated connections rejected `busy` since start.
    pub busy_rejections: u64,
    pub ownership: OwnershipStatus,
    /// Names (never values) removed from the agent environment snapshot.
    pub removed_env: Vec<String>,
}

/// `daemon.stop`. `clean: false` keeps recovery evidence (R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopOutcome {
    pub clean: bool,
}

/// `remote.status` (CLI/MCP remote status JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStatusReport {
    pub listen: Option<String>,
    pub listen_configured: Option<String>,
    pub controller: Option<String>,
    pub last_session_error: Option<String>,
    pub inbound: Option<InboundStatus>,
    pub remotes: Vec<RemoteStatus>,
}

/// `DoctorCheck` status (orch-engine doctor_service.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `DoctorReport` (orch-engine doctor_service.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub adapters_ready: u64,
    pub adapters_total: u64,
}

/// `ModelOption` (orch-infra models/discovery.rs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// One adapter's models in `models.catalog`, in discovery order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterModels {
    pub adapter: orch_domain::AdapterKind,
    pub models: Vec<ModelOption>,
}

/// `AgentShopTemplate` (orch-domain shop, `&'static` there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentShopTemplate {
    pub key: String,
    pub name: String,
    pub description: String,
    pub tier: ModelTier,
    pub approval_policy: ApprovalPolicy,
    pub skills: Vec<String>,
    pub role: String,
}

/// `OrgAgentEntry` (orch-domain shop).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgAgentEntry {
    pub shop_key: String,
    pub name: String,
}

/// `OrgTemplate` (orch-domain shop), also the `org.list` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgTemplate {
    pub key: String,
    pub name: String,
    pub description: String,
    pub agents: Vec<OrgAgentEntry>,
    pub lead_index: u64,
}

/// `org.deploy` (`Deployment` / CLI `DeployJson`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    pub team: Team,
    #[serde(rename = "agentIds")]
    pub agent_ids: Vec<String>,
    #[serde(rename = "goalId", default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
}

/// Where an org template came from (orch-infra `Source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TemplateSource {
    Builtin,
    Legacy,
    User { path: WirePath },
    Directory { path: WirePath },
}

/// `org.template.list` row (CLI `ListEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateListEntry {
    pub name: String,
    pub source: TemplateSource,
    pub shadowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `org.template.show` (orch-infra `Loaded`, without `flatten`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateView {
    pub source: TemplateSource,
    pub bundle: Bundle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_agents: Option<Vec<CreateAgentInput>>,
}

/// `org.template.init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateInit {
    pub name: String,
    pub path: WirePath,
}

/// `org.template.agent.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateAgentDeleted {
    pub deleted: String,
}

/// `update.check`: version metadata and install instructions only; this goal
/// adds no binary install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub current_version: String,
    pub instructions: String,
}

/// `subscribe`: sent before any event of the subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeAck {
    pub subscription: u64,
    pub instance: String,
    /// The baseline commit ordinal; apply invalidations after it.
    pub seq: u64,
}

/// `transfer.begin` / `transfer.finish`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferHandle {
    pub token: super::args::TransferToken,
    /// `true` after `transfer.finish`: the token may now be consumed once.
    pub sealed: bool,
}

/// Results that are lists of authored org-template agents.
pub type TemplateAgents = Vec<AgentDefinition>;
