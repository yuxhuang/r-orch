//! Request arguments: one closed struct per operation (spec R7).
//!
//! Every struct rejects unknown and duplicate fields, uses no `flatten` and no
//! untyped `serde_json::Value`, and bounds every string and list by type.
//! Authored documents travel as bounded **text** plus a format, parsed by the
//! daemon under the existing document limits (1 MiB, depth 64, no duplicate
//! keys); larger inputs reference a sealed [`TransferToken`].
//!
//! Field meanings follow the existing service inputs and CLI/MCP flags; the
//! source is named on each struct. Caller defaults (`--from`, `--agent`,
//! `ORCH_AGENT_ID`) are resolved client-side before a request is built.

use crate::bounded::{BoundedString, BoundedVec, Id, ShortText, Text};
use crate::frame::{RequestId, Store};
use crate::path::WirePath;
use orch_domain::{
    AdapterKind, ApprovalPolicy, GoalStatus, PromptMode, ReasoningEffort, ReviewCriterion,
    TaskStatus, TuiPaletteName, WorkspaceMode,
};
use serde::{Deserialize, Serialize};

/// Largest inline authored document (the existing definition-document limit).
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// An inline authored document.
pub type DocumentText = BoundedString<MAX_DOCUMENT_BYTES>;
/// Largest base64 chunk of a transfer: 256 KiB raw, base64-expanded.
pub const MAX_CHUNK_BASE64_BYTES: usize = 256 * 1024 / 3 * 4 + 4;

/// Declare an argument struct with the closed-DTO attributes.
macro_rules! args {
    ($(#[$meta:meta])* $name:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name { $($(#[$fmeta])* pub $field: $ty,)* }
    };
}

// ── Shared argument shapes ─────────────────────────────────────────────────

/// An opaque, connection-local handle to a sealed inbound transfer
/// (design D6). Consumed once by the request that names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransferToken(pub Id);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentFormat {
    Json,
    Yaml,
}

/// `update` replaces the authored document; `patch` is an RFC 7396 merge
/// patch over the current authored subset (CLI `MutationKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    Update,
    Patch,
}

/// A field that can be set or explicitly cleared. `None` around it means
/// "leave unchanged". Wire: `{"set": <value>}` or `"clear"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldPatch<T> {
    Set(T),
    Clear,
}

args! {
    /// An authored document, inline or by transfer (exactly one).
    DocumentInput {
        #[serde(default)]
        document: Option<DocumentText>,
        #[serde(default)]
        input_transfer: Option<TransferToken>,
        format: DocumentFormat,
    }
}

args! {
    /// A definition mutation. `base_digest` is the opaque digest returned by
    /// the matching `*.definition.get`; a changed authored base is a
    /// `conflict` error, never an overwrite (design D7 "Interactive edit").
    DefinitionInput {
        input: DocumentInput,
        kind: MutationKind,
        #[serde(default)]
        base_digest: Option<Id>,
    }
}

// ── Status and bootstrap ───────────────────────────────────────────────────

args! { StatusGetArgs {} }
args! { StateGetArgs {} }
args! { UiBootstrapArgs {} }

// ── Tasks (TaskService; CLI `task …`; MCP `task_*`; TUI) ───────────────────

args! {
    /// `CreateTaskInput`.
    TaskCreateArgs {
        title: ShortText,
        #[serde(default)]
        description: Option<Text>,
        #[serde(default)]
        priority: Option<i64>,
        #[serde(default)]
        assignee: Option<Id>,
        #[serde(default)]
        labels: Option<BoundedVec<ShortText>>,
        #[serde(default)]
        depends_on: Option<BoundedVec<Id>>,
        #[serde(default)]
        max_attempts: Option<u32>,
        #[serde(default)]
        workspace_mode: Option<WorkspaceMode>,
        #[serde(default)]
        review_criteria: Option<BoundedVec<ReviewCriterion, 8>>,
        #[serde(default)]
        scope: Option<BoundedVec<ShortText>>,
        #[serde(default)]
        goal_id: Option<Id>,
        /// Absolute source paths (CLI/TUI: against caller cwd; MCP: against
        /// the project root). The daemon rejects relative paths (SE-10).
        #[serde(default)]
        attachments: Option<BoundedVec<WirePath, 64>>,
    }
}

args! {
    /// `TaskFilter`.
    TaskListArgs {
        #[serde(default)]
        status: Option<TaskStatus>,
        #[serde(default)]
        goal_id: Option<Id>,
    }
}

args! { TaskGetArgs { id: Id } }
args! { TaskAssignArgs { id: Id, agent_id: Id } }
args! {
    /// Routes through the live orchestrator: a local live child is stopped
    /// before success (R9–R12).
    TaskCancelArgs { id: Id }
}
args! {
    /// CLI/MCP approve: hold-release-or-review semantics.
    TaskApproveArgs { id: Id }
}
args! {
    /// TUI's direct review → done transition.
    TaskReviewCompleteArgs { id: Id }
}
args! {
    TaskRejectArgs {
        id: Id,
        #[serde(default)]
        feedback: Option<Text>,
    }
}
args! { TaskRetryArgs { id: Id } }
args! { TaskDeleteArgs { id: Id } }

args! {
    /// `UpdateTaskFields`. `None` leaves a field alone; `attachments` are
    /// absolute source paths **appended** to the task's attachments.
    TaskUpdateFieldsArgs {
        id: Id,
        #[serde(default)]
        title: Option<ShortText>,
        #[serde(default)]
        description: Option<Text>,
        #[serde(default)]
        priority: Option<i64>,
        #[serde(default)]
        labels: Option<BoundedVec<ShortText>>,
        #[serde(default)]
        attachments: Option<BoundedVec<WirePath, 64>>,
    }
}

args! {
    /// Authored subset: `title`, `description`, `priority`, `labels`.
    TaskDefinitionGetArgs { id: Id }
}
args! { TaskDefinitionApplyArgs { id: Id, definition: DefinitionInput } }

args! {
    /// One attachment of a task, by its stored file name (no path).
    TaskAttachmentReadArgs { task_id: Id, filename: Id }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
}

args! {
    /// Stage clipboard image bytes (read by the frontend, uploaded through a
    /// transfer) as a daemon-owned temporary attachment reference.
    AttachmentStageImageArgs {
        input_transfer: TransferToken,
        format: ImageFormat,
    }
}

// ── Agents (AgentService; CLI `agent …`; MCP `agent_*`; TUI) ───────────────

args! {
    /// One environment variable of an agent's configuration.
    EnvVar { name: Id, value: Text }
}

args! {
    /// `CreateAgentInput`.
    AgentCreateArgs {
        name: ShortText,
        adapter: Id,
        #[serde(default)]
        role: Option<Text>,
        #[serde(default)]
        command: Option<ShortText>,
        #[serde(default)]
        model: Option<Id>,
        #[serde(default)]
        effort: Option<ReasoningEffort>,
        #[serde(default)]
        approval_policy: Option<ApprovalPolicy>,
        #[serde(default)]
        max_turns: Option<u32>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        #[serde(default)]
        stall_timeout_ms: Option<u64>,
        #[serde(default)]
        env: Option<BoundedVec<EnvVar, 256>>,
        #[serde(default)]
        system_prompt: Option<Text>,
        #[serde(default)]
        workspace_mode: Option<WorkspaceMode>,
        #[serde(default)]
        skills: Option<BoundedVec<Id>>,
        #[serde(default)]
        prompt_mode: Option<PromptMode>,
        #[serde(default)]
        role_summary: Option<ShortText>,
    }
}

args! { AgentListArgs {} }
args! {
    /// `effective` adds the resolved prompt mode and its source.
    AgentGetArgs {
        id: Id,
        #[serde(default)]
        effective: bool,
    }
}

args! {
    /// `UpdateAgentFields`. `None` leaves a field alone.
    AgentUpdateFieldsArgs {
        id: Id,
        #[serde(default)]
        name: Option<ShortText>,
        #[serde(default)]
        adapter: Option<Id>,
        #[serde(default)]
        role: Option<Text>,
        #[serde(default)]
        command: Option<ShortText>,
        #[serde(default)]
        model: Option<FieldPatch<Id>>,
        #[serde(default)]
        effort: Option<FieldPatch<ReasoningEffort>>,
        #[serde(default)]
        approval_policy: Option<ApprovalPolicy>,
        #[serde(default)]
        prompt_mode: Option<FieldPatch<PromptMode>>,
        #[serde(default)]
        role_summary: Option<FieldPatch<ShortText>>,
    }
}

args! { AgentRemoveArgs { id: Id } }
args! { AgentEnableArgs { id: Id } }
args! { AgentDisableArgs { id: Id } }
args! {
    /// `enabled: None` toggles, as `agent autonomous` with neither flag.
    AgentAutonomousArgs {
        id: Id,
        #[serde(default)]
        enabled: Option<bool>,
    }
}
args! {
    /// Live `force_stop_agent`.
    AgentStopArgs { id: Id }
}
args! {
    /// Authored subset: `name`, `adapter`, `role`, `config`.
    AgentDefinitionGetArgs { id: Id }
}
args! {
    /// The CLI/MCP flag form (`--prompt-mode`, `--effort`, `--role-summary`)
    /// is compiled client-side to a `patch` of `{"config": {...}}`.
    AgentDefinitionApplyArgs { id: Id, definition: DefinitionInput }
}

// ── Goals (GoalService) ────────────────────────────────────────────────────

args! {
    /// `CreateGoalInput`.
    GoalCreateArgs {
        title: ShortText,
        #[serde(default)]
        description: Option<Text>,
        #[serde(default)]
        assignee: Option<Id>,
    }
}
args! {
    /// `GoalFilter`.
    GoalListArgs {
        #[serde(default)]
        status: Option<GoalStatus>,
    }
}
args! {
    /// Goal plus its tasks and progress report, in one coordinated read.
    GoalGetArgs { id: Id }
}
args! {
    /// `UpdateGoalFields`; `assignee: Some("")` clears the assignee.
    GoalUpdateArgs {
        id: Id,
        #[serde(default)]
        title: Option<ShortText>,
        #[serde(default)]
        description: Option<Text>,
        #[serde(default)]
        assignee: Option<Id>,
    }
}
args! {
    /// `force` affects only the achieved guard; `force: true` from MCP
    /// requires `allow_destructive` (checked daemon-side).
    GoalStatusArgs {
        id: Id,
        status: GoalStatus,
        #[serde(default)]
        force: bool,
    }
}
args! { GoalDeleteArgs { id: Id } }
args! { GoalTasksArgs { id: Id } }
args! { GoalProgressArgs { id: Id } }

// ── Messages (MessageService) ──────────────────────────────────────────────

args! {
    /// Direct message (`CreateMessageInput`, channel `direct`).
    MessageSendArgs {
        from_agent_id: Id,
        to_agent_id: Id,
        #[serde(default)]
        subject: ShortText,
        body: Text,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        reply_to: Option<Id>,
    }
}
args! {
    /// Broadcast; recipients are expanded daemon-side.
    MessageBroadcastArgs {
        from_agent_id: Id,
        #[serde(default)]
        subject: ShortText,
        body: Text,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        team_id: Option<Id>,
    }
}
args! {
    /// Pending messages for an agent, **without** marking them.
    MessageInboxArgs { agent_id: Id }
}
args! {
    /// All messages, or those sent or received by `agent_id`.
    MessageListArgs {
        #[serde(default)]
        agent_id: Option<Id>,
    }
}
args! {
    /// Mark messages delivered to `agent_id`.
    MessageMarkReadArgs { agent_id: Id, ids: BoundedVec<Id> }
}

// ── Context (ContextStore) ─────────────────────────────────────────────────

args! {
    ContextSetArgs {
        key: Id,
        value: Text,
        /// 1 ms to 30 days; `None` never expires.
        #[serde(default)]
        ttl_ms: Option<u64>,
    }
}
args! {
    /// Reads filter expired values but never delete them.
    ContextGetArgs { key: Id }
}
args! { ContextListArgs {} }
args! { ContextDeleteArgs { key: Id } }
args! {
    /// Owner-only explicit expiry; the maintenance timer runs the same handler.
    MaintenanceExpireContextArgs {}
}

// ── Teams (TeamService) ────────────────────────────────────────────────────

args! {
    /// `CreateTeamInput` with `PartialTeamConfig` inlined.
    TeamCreateArgs {
        name: ShortText,
        #[serde(default)]
        description: Option<Text>,
        lead_agent_id: Id,
        #[serde(default)]
        member_agent_ids: Option<BoundedVec<Id>>,
        #[serde(default)]
        max_concurrent_tasks: Option<u32>,
        #[serde(default)]
        auto_claim: Option<bool>,
        #[serde(default)]
        message_ttl_ms: Option<u64>,
    }
}
args! { TeamListArgs {} }
args! { TeamGetArgs { id: Id } }
args! { TeamJoinArgs { team_id: Id, agent_id: Id } }
args! { TeamLeaveArgs { team_id: Id, agent_id: Id } }
args! { TeamAddTaskArgs { team_id: Id, task_id: Id } }
args! { TeamRemoveTaskArgs { team_id: Id, task_id: Id } }
args! { TeamSetLeadArgs { team_id: Id, agent_id: Id } }
args! { TeamDisbandArgs { id: Id } }
args! { TeamForAgentArgs { agent_id: Id } }

// ── Config ─────────────────────────────────────────────────────────────────

args! {
    /// Dot-path key into the project `config.yml`.
    ConfigGetArgs { key: Id }
}
args! {
    /// The value is parsed as JSON first, else kept as a string (as
    /// `config set`); prompt-mode keys are validated daemon-side.
    ConfigSetArgs { key: Id, value: Text }
}
args! { ConfigDefinitionGetArgs {} }
args! { ConfigDefinitionApplyArgs { definition: DefinitionInput } }
args! {
    /// Machine-global key (`palette`, `activity_filter`).
    ConfigGlobalGetArgs { key: Id }
}
args! { ConfigGlobalSetArgs { key: Id, value: ShortText } }
args! { ConfigGlobalShowArgs {} }

args! {
    /// TUI notification toggles; `None` leaves one alone.
    NotificationsPatch {
        #[serde(default)]
        toast: Option<bool>,
        #[serde(default)]
        bell: Option<bool>,
    }
}
args! {
    /// Field patch of the global TUI preferences; never a whole-file save.
    UiPreferencesPatchArgs {
        #[serde(default)]
        palette: Option<TuiPaletteName>,
        #[serde(default)]
        activity_filter: Option<Id>,
        #[serde(default)]
        notifications: Option<NotificationsPatch>,
    }
}
args! { ConfigPromptModeGetArgs {} }
args! { ConfigPromptModeSetArgs { mode: PromptMode } }
args! {
    /// Resolves the current mode inside the coordinator, then flips it.
    ConfigPromptModeToggleArgs {}
}
args! { StateCompleteOnboardingArgs {} }

// ── Runs and logs (RunService) ─────────────────────────────────────────────

args! {
    /// All runs, or those of a task or an agent; `since_ms` keeps runs
    /// overlapping the last N ms; `limit` keeps the newest N.
    RunListArgs {
        #[serde(default)]
        task_id: Option<Id>,
        #[serde(default)]
        agent_id: Option<Id>,
        #[serde(default)]
        since_ms: Option<u64>,
        #[serde(default)]
        limit: Option<u32>,
    }
}
args! { RunGetArgs { id: Id } }
args! { RunEventsArgs { run_id: Id } }
args! { RunEventsTailArgs { run_id: Id, count: u32 } }
args! {
    /// TUI activity history: newest finished runs, in batches.
    RunHistoryArgs {}
}
args! {
    /// TUI timing snapshot at the daemon clock.
    RunTimingsArgs {}
}
args! {
    /// Stream orchestrator events as `part` frames (kind `event`) until
    /// `request.cancel` or disconnect.
    LogsFollowArgs {
        #[serde(default)]
        run_id: Option<Id>,
        #[serde(default)]
        task_id: Option<Id>,
        #[serde(default)]
        agent_id: Option<Id>,
    }
}

// ── Dispatch (design D8) ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchSwitch {
    On,
    Off,
}

args! {
    /// Toggle future automatic dispatch. Off never kills running agents.
    DispatchSetArgs { dispatch: DispatchSwitch }
}
args! {
    /// One scheduling tick (`run --once`); events stream as parts.
    DispatchTickArgs {}
}
args! {
    /// Explicit launch of one task (`run <id>`), observed until terminal.
    /// Disconnecting the observer does not kill the run.
    DispatchRunTaskArgs { task_id: Id }
}
args! {
    /// Finite batch of currently eligible tasks (`run --all`).
    DispatchRunAllArgs {}
}
args! {
    /// `serve --once`: requires global dispatch off; see the D8 batch policy.
    DispatchUntilTerminalArgs {}
}

// ── Daemon ─────────────────────────────────────────────────────────────────

args! { DaemonStatusArgs {} }
args! {
    /// Compatible-version stop; across skew use the frozen hello intent.
    DaemonStopArgs {}
}

// ── Remote peers ───────────────────────────────────────────────────────────

args! {
    /// `addr` is `host[:port]`; the default port is applied daemon-side.
    RemoteAddArgs { alias: Id, addr: ShortText }
}
args! { RemoteRemoveArgs { alias: Id } }
args! { RemoteListArgs {} }
args! { RemoteStatusArgs {} }
args! {
    /// Sets `peer.listen`; `addr` only with `enabled`. Does not affect a
    /// running listener (an explicit start option).
    RemoteListenArgs {
        enabled: bool,
        #[serde(default)]
        addr: Option<ShortText>,
    }
}

// ── Doctor, catalogues, update ─────────────────────────────────────────────

args! { DoctorRunArgs {} }
args! {
    /// `None` discovers every supported adapter.
    ModelsCatalogArgs {
        #[serde(default)]
        adapters: Option<BoundedVec<AdapterKind, 32>>,
    }
}
args! { CatalogAgentsArgs {} }
args! { CatalogTemplatesArgs {} }
args! { UpdateCheckArgs {} }

// ── Org and org templates ──────────────────────────────────────────────────

args! {
    /// `[user:|builtin:]key`, optionally under an explicit template
    /// directory (absolutized client-side).
    TemplateSelector {
        #[serde(default)]
        name: Option<ShortText>,
        #[serde(default)]
        template_dir: Option<WirePath>,
    }
}

args! { OrgListArgs {} }
args! {
    OrgDeployArgs {
        selector: TemplateSelector,
        #[serde(default)]
        adapter: Option<Id>,
        #[serde(default)]
        model: Option<Id>,
        #[serde(default)]
        goal_id: Option<Id>,
    }
}
args! {
    OrgTemplateListArgs {
        #[serde(default)]
        template_dir: Option<WirePath>,
    }
}
args! {
    OrgTemplateShowArgs {
        selector: TemplateSelector,
        #[serde(default)]
        resolved: bool,
    }
}
args! {
    /// Never overwrites an existing template.
    OrgTemplateInitArgs {
        name: Id,
        #[serde(default)]
        from: Option<ShortText>,
        #[serde(default)]
        template_dir: Option<WirePath>,
    }
}
args! { OrgTemplateDefinitionGetArgs { selector: TemplateSelector } }
args! { OrgTemplateDefinitionApplyArgs { selector: TemplateSelector, definition: DefinitionInput } }
args! { OrgTemplateAgentListArgs { selector: TemplateSelector } }
args! { OrgTemplateAgentAddArgs { selector: TemplateSelector, input: DocumentInput } }
args! { OrgTemplateAgentDefinitionGetArgs { selector: TemplateSelector, agent: Id } }
args! {
    OrgTemplateAgentDefinitionApplyArgs {
        selector: TemplateSelector,
        agent: Id,
        definition: DefinitionInput,
    }
}
args! { OrgTemplateAgentDeleteArgs { selector: TemplateSelector, agent: Id } }

// ── Transport controls (design D6) ─────────────────────────────────────────

args! {
    /// Typed optional ids, never an executable predicate.
    SubscriptionFilter {
        #[serde(default)]
        ids: Option<BoundedVec<Id>>,
        #[serde(default)]
        goal_id: Option<Id>,
        #[serde(default)]
        agent_id: Option<Id>,
    }
}
args! {
    SubscribeArgs {
        stores: BoundedVec<Store, 16>,
        #[serde(default)]
        filter: Option<SubscriptionFilter>,
        #[serde(default)]
        bus: bool,
    }
}
args! { UnsubscribeArgs { subscription: u64 } }
args! {
    /// Stops observation/queued work of `id` only; never cancels a running
    /// agent (use `task.cancel`).
    RequestCancelArgs { id: RequestId }
}

/// What an inbound transfer will be consumed by; checked against the
/// session's mode (read-only sessions have no inbound mutating transfers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPurpose {
    Document,
    Text,
    ClipboardImage,
}

args! { TransferBeginArgs { purpose: TransferPurpose, total_bytes: u64 } }
args! {
    TransferChunkArgs {
        token: TransferToken,
        n: u64,
        data_base64: BoundedString<MAX_CHUNK_BASE64_BYTES>,
    }
}
args! {
    /// `sha256`: lowercase hex of the assembled bytes.
    TransferFinishArgs { token: TransferToken, sha256: Id }
}
