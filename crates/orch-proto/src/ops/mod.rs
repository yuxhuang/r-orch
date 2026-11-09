//! The complete operation catalogue (design D7) and its authoritative classes.
//!
//! [`for_each_op!`](crate::for_each_op) is the single list: every other
//! per-operation construct — the [`Request`] enum, [`Request::class`], the
//! [`Op`] impls here, and the daemon's handler trait — is generated from it,
//! so an operation cannot exist in one place and be missing in another. There
//! is no generic invocation, file-read or store-save operation.

pub mod args;
pub mod results;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Admission class of an operation (design "Authoritative operation classes
/// and modes"). The daemon checks the session's kind and mode against this
/// before admission; `Control` is not blanket permission and has its own
/// allowlist daemon-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpClass {
    Read,
    Mutate,
    Destructive,
    Control,
}

/// Invoke `$callback!` with every operation as
/// `(Variant, method_name, "wire.name", ArgsType, OutputType, Class)`.
///
/// Adding an operation here adds it to [`Request`], gives it an [`Op`] impl
/// and a handler method in `orch-daemon`; changing any entry's wire shape or
/// meaning requires bumping [`LOCAL_PROTOCOL`](crate::LOCAL_PROTOCOL).
#[macro_export]
macro_rules! for_each_op {
    ($callback:ident) => {
        $callback! {
            (StatusGet, status_get, "status.get", $crate::ops::args::StatusGetArgs, $crate::ops::results::StatusSummary, Read),
            (StateGet, state_get, "state.get", $crate::ops::args::StateGetArgs, $crate::orch_domain::OrchestratorState, Read),
            (UiBootstrap, ui_bootstrap, "ui.bootstrap", $crate::ops::args::UiBootstrapArgs, $crate::ops::results::LaunchContext, Read),
            (TaskCreate, task_create, "task.create", $crate::ops::args::TaskCreateArgs, $crate::orch_domain::Task, Mutate),
            (TaskList, task_list, "task.list", $crate::ops::args::TaskListArgs, Vec<$crate::orch_domain::Task>, Read),
            (TaskGet, task_get, "task.get", $crate::ops::args::TaskGetArgs, $crate::orch_domain::Task, Read),
            (TaskAssign, task_assign, "task.assign", $crate::ops::args::TaskAssignArgs, $crate::orch_domain::Task, Mutate),
            (TaskCancel, task_cancel, "task.cancel", $crate::ops::args::TaskCancelArgs, $crate::orch_domain::Task, Destructive),
            (TaskApprove, task_approve, "task.approve", $crate::ops::args::TaskApproveArgs, $crate::orch_domain::Task, Mutate),
            (TaskReviewComplete, task_review_complete, "task.review_complete", $crate::ops::args::TaskReviewCompleteArgs, $crate::orch_domain::Task, Mutate),
            (TaskReject, task_reject, "task.reject", $crate::ops::args::TaskRejectArgs, $crate::orch_domain::Task, Mutate),
            (TaskRetry, task_retry, "task.retry", $crate::ops::args::TaskRetryArgs, $crate::orch_domain::Task, Mutate),
            (TaskDelete, task_delete, "task.delete", $crate::ops::args::TaskDeleteArgs, (), Destructive),
            (TaskUpdateFields, task_update_fields, "task.update_fields", $crate::ops::args::TaskUpdateFieldsArgs, $crate::orch_domain::Task, Mutate),
            (TaskDefinitionGet, task_definition_get, "task.definition.get", $crate::ops::args::TaskDefinitionGetArgs, $crate::ops::results::DefinitionDocument, Read),
            (TaskDefinitionApply, task_definition_apply, "task.definition.apply", $crate::ops::args::TaskDefinitionApplyArgs, $crate::orch_domain::Task, Mutate),
            (TaskAttachmentRead, task_attachment_read, "task.attachment.read", $crate::ops::args::TaskAttachmentReadArgs, $crate::ops::results::AttachmentBytes, Read),
            (AttachmentStageImage, attachment_stage_image, "attachment.stage_image", $crate::ops::args::AttachmentStageImageArgs, $crate::ops::results::StagedAttachment, Mutate),
            (AgentCreate, agent_create, "agent.create", $crate::ops::args::AgentCreateArgs, $crate::ops::results::AuthoredAgent, Mutate),
            (AgentList, agent_list, "agent.list", $crate::ops::args::AgentListArgs, Vec<$crate::orch_domain::Agent>, Read),
            (AgentGet, agent_get, "agent.get", $crate::ops::args::AgentGetArgs, $crate::ops::results::AgentView, Read),
            (AgentUpdateFields, agent_update_fields, "agent.update_fields", $crate::ops::args::AgentUpdateFieldsArgs, $crate::ops::results::AuthoredAgent, Mutate),
            (AgentRemove, agent_remove, "agent.remove", $crate::ops::args::AgentRemoveArgs, (), Destructive),
            (AgentEnable, agent_enable, "agent.enable", $crate::ops::args::AgentEnableArgs, $crate::orch_domain::Agent, Mutate),
            (AgentDisable, agent_disable, "agent.disable", $crate::ops::args::AgentDisableArgs, $crate::orch_domain::Agent, Mutate),
            (AgentAutonomous, agent_autonomous, "agent.autonomous", $crate::ops::args::AgentAutonomousArgs, $crate::orch_domain::Agent, Mutate),
            (AgentStop, agent_stop, "agent.stop", $crate::ops::args::AgentStopArgs, (), Destructive),
            (AgentDefinitionGet, agent_definition_get, "agent.definition.get", $crate::ops::args::AgentDefinitionGetArgs, $crate::ops::results::DefinitionDocument, Read),
            (AgentDefinitionApply, agent_definition_apply, "agent.definition.apply", $crate::ops::args::AgentDefinitionApplyArgs, $crate::ops::results::AuthoredAgent, Mutate),
            (GoalCreate, goal_create, "goal.create", $crate::ops::args::GoalCreateArgs, $crate::orch_domain::Goal, Mutate),
            (GoalList, goal_list, "goal.list", $crate::ops::args::GoalListArgs, Vec<$crate::orch_domain::Goal>, Read),
            (GoalGet, goal_get, "goal.get", $crate::ops::args::GoalGetArgs, $crate::ops::results::GoalView, Read),
            (GoalUpdate, goal_update, "goal.update", $crate::ops::args::GoalUpdateArgs, $crate::orch_domain::Goal, Mutate),
            (GoalStatus, goal_status, "goal.status", $crate::ops::args::GoalStatusArgs, $crate::orch_domain::Goal, Mutate),
            (GoalDelete, goal_delete, "goal.delete", $crate::ops::args::GoalDeleteArgs, (), Destructive),
            (GoalTasks, goal_tasks, "goal.tasks", $crate::ops::args::GoalTasksArgs, Vec<$crate::orch_domain::Task>, Read),
            (GoalProgress, goal_progress, "goal.progress", $crate::ops::args::GoalProgressArgs, Option<String>, Read),
            (MessageSend, message_send, "message.send", $crate::ops::args::MessageSendArgs, Vec<$crate::orch_domain::Message>, Mutate),
            (MessageBroadcast, message_broadcast, "message.broadcast", $crate::ops::args::MessageBroadcastArgs, Vec<$crate::orch_domain::Message>, Mutate),
            (MessageInbox, message_inbox, "message.inbox", $crate::ops::args::MessageInboxArgs, Vec<$crate::orch_domain::Message>, Read),
            (MessageList, message_list, "message.list", $crate::ops::args::MessageListArgs, Vec<$crate::orch_domain::Message>, Read),
            (MessageMarkRead, message_mark_read, "message.mark_read", $crate::ops::args::MessageMarkReadArgs, Vec<$crate::orch_domain::Message>, Mutate),
            (ContextSet, context_set, "context.set", $crate::ops::args::ContextSetArgs, Option<$crate::ops::results::ContextEntry>, Mutate),
            (ContextGet, context_get, "context.get", $crate::ops::args::ContextGetArgs, Option<$crate::ops::results::ContextEntry>, Read),
            (ContextList, context_list, "context.list", $crate::ops::args::ContextListArgs, Vec<$crate::ops::results::ContextEntry>, Read),
            (ContextDelete, context_delete, "context.delete", $crate::ops::args::ContextDeleteArgs, (), Destructive),
            (MaintenanceExpireContext, maintenance_expire_context, "maintenance.expire_context", $crate::ops::args::MaintenanceExpireContextArgs, $crate::ops::results::ExpiryReport, Destructive),
            (TeamCreate, team_create, "team.create", $crate::ops::args::TeamCreateArgs, $crate::orch_domain::Team, Mutate),
            (TeamList, team_list, "team.list", $crate::ops::args::TeamListArgs, Vec<$crate::orch_domain::Team>, Read),
            (TeamGet, team_get, "team.get", $crate::ops::args::TeamGetArgs, $crate::orch_domain::Team, Read),
            (TeamJoin, team_join, "team.join", $crate::ops::args::TeamJoinArgs, $crate::orch_domain::Team, Mutate),
            (TeamLeave, team_leave, "team.leave", $crate::ops::args::TeamLeaveArgs, $crate::orch_domain::Team, Mutate),
            (TeamAddTask, team_add_task, "team.add_task", $crate::ops::args::TeamAddTaskArgs, $crate::orch_domain::Team, Mutate),
            (TeamRemoveTask, team_remove_task, "team.remove_task", $crate::ops::args::TeamRemoveTaskArgs, $crate::orch_domain::Team, Mutate),
            (TeamSetLead, team_set_lead, "team.set_lead", $crate::ops::args::TeamSetLeadArgs, $crate::orch_domain::Team, Mutate),
            (TeamDisband, team_disband, "team.disband", $crate::ops::args::TeamDisbandArgs, (), Destructive),
            (TeamForAgent, team_for_agent, "team.for_agent", $crate::ops::args::TeamForAgentArgs, Option<$crate::orch_domain::Team>, Read),
            (ConfigGet, config_get, "config.get", $crate::ops::args::ConfigGetArgs, $crate::ops::results::ConfigValue, Read),
            (ConfigSet, config_set, "config.set", $crate::ops::args::ConfigSetArgs, $crate::ops::results::ConfigValue, Mutate),
            (ConfigDefinitionGet, config_definition_get, "config.definition.get", $crate::ops::args::ConfigDefinitionGetArgs, $crate::ops::results::DefinitionDocument, Read),
            (ConfigDefinitionApply, config_definition_apply, "config.definition.apply", $crate::ops::args::ConfigDefinitionApplyArgs, $crate::ops::results::DefinitionDocument, Mutate),
            (ConfigGlobalGet, config_global_get, "config.global.get", $crate::ops::args::ConfigGlobalGetArgs, $crate::ops::results::ConfigValue, Read),
            (ConfigGlobalSet, config_global_set, "config.global.set", $crate::ops::args::ConfigGlobalSetArgs, $crate::ops::results::ConfigValue, Mutate),
            (ConfigGlobalShow, config_global_show, "config.global.show", $crate::ops::args::ConfigGlobalShowArgs, $crate::orch_domain::GlobalConfig, Read),
            (UiPreferencesPatch, ui_preferences_patch, "ui.preferences.patch", $crate::ops::args::UiPreferencesPatchArgs, $crate::orch_domain::GlobalConfig, Mutate),
            (ConfigPromptModeGet, config_prompt_mode_get, "config.prompt_mode.get", $crate::ops::args::ConfigPromptModeGetArgs, $crate::orch_domain::PromptMode, Read),
            (ConfigPromptModeSet, config_prompt_mode_set, "config.prompt_mode.set", $crate::ops::args::ConfigPromptModeSetArgs, $crate::orch_domain::PromptMode, Mutate),
            (ConfigPromptModeToggle, config_prompt_mode_toggle, "config.prompt_mode.toggle", $crate::ops::args::ConfigPromptModeToggleArgs, $crate::orch_domain::PromptMode, Mutate),
            (StateCompleteOnboarding, state_complete_onboarding, "state.complete_onboarding", $crate::ops::args::StateCompleteOnboardingArgs, (), Mutate),
            (RunList, run_list, "run.list", $crate::ops::args::RunListArgs, Vec<$crate::orch_domain::Run>, Read),
            (RunGet, run_get, "run.get", $crate::ops::args::RunGetArgs, Option<$crate::orch_domain::Run>, Read),
            (RunEvents, run_events, "run.events", $crate::ops::args::RunEventsArgs, Vec<$crate::orch_domain::RunEvent>, Read),
            (RunEventsTail, run_events_tail, "run.events_tail", $crate::ops::args::RunEventsTailArgs, Vec<$crate::orch_domain::RunEvent>, Read),
            (RunHistory, run_history, "run.history", $crate::ops::args::RunHistoryArgs, $crate::ops::results::RunHistory, Read),
            (RunTimings, run_timings, "run.timings", $crate::ops::args::RunTimingsArgs, $crate::ops::results::TimingSnapshot, Read),
            (LogsFollow, logs_follow, "logs.follow", $crate::ops::args::LogsFollowArgs, (), Read),
            (DispatchSet, dispatch_set, "dispatch.set", $crate::ops::args::DispatchSetArgs, $crate::ops::results::DispatchState, Control),
            (DispatchTick, dispatch_tick, "dispatch.tick", $crate::ops::args::DispatchTickArgs, $crate::ops::results::TickResult, Control),
            (DispatchRunTask, dispatch_run_task, "dispatch.run_task", $crate::ops::args::DispatchRunTaskArgs, $crate::ops::results::RunOutcome, Control),
            (DispatchRunAll, dispatch_run_all, "dispatch.run_all", $crate::ops::args::DispatchRunAllArgs, $crate::ops::results::RunOutcome, Control),
            (DispatchUntilTerminal, dispatch_until_terminal, "dispatch.until_terminal", $crate::ops::args::DispatchUntilTerminalArgs, $crate::ops::results::BatchResult, Control),
            (DaemonStatus, daemon_status, "daemon.status", $crate::ops::args::DaemonStatusArgs, $crate::ops::results::DaemonStatus, Read),
            (DaemonStop, daemon_stop, "daemon.stop", $crate::ops::args::DaemonStopArgs, $crate::ops::results::StopOutcome, Control),
            (RemoteAdd, remote_add, "remote.add", $crate::ops::args::RemoteAddArgs, $crate::orch_domain::RemoteConfig, Mutate),
            (RemoteRemove, remote_remove, "remote.remove", $crate::ops::args::RemoteRemoveArgs, (), Destructive),
            (RemoteList, remote_list, "remote.list", $crate::ops::args::RemoteListArgs, Vec<$crate::orch_domain::RemoteConfig>, Read),
            (RemoteStatus, remote_status, "remote.status", $crate::ops::args::RemoteStatusArgs, $crate::ops::results::RemoteStatusReport, Read),
            (RemoteListen, remote_listen, "remote.listen", $crate::ops::args::RemoteListenArgs, (), Mutate),
            (DoctorRun, doctor_run, "doctor.run", $crate::ops::args::DoctorRunArgs, $crate::ops::results::DoctorReport, Read),
            (ModelsCatalog, models_catalog, "models.catalog", $crate::ops::args::ModelsCatalogArgs, Vec<$crate::ops::results::AdapterModels>, Read),
            (CatalogAgents, catalog_agents, "catalog.agents", $crate::ops::args::CatalogAgentsArgs, Vec<$crate::ops::results::AgentShopTemplate>, Read),
            (CatalogTemplates, catalog_templates, "catalog.templates", $crate::ops::args::CatalogTemplatesArgs, Vec<$crate::ops::results::OrgTemplate>, Read),
            (OrgList, org_list, "org.list", $crate::ops::args::OrgListArgs, Vec<$crate::ops::results::OrgTemplate>, Read),
            (OrgDeploy, org_deploy, "org.deploy", $crate::ops::args::OrgDeployArgs, $crate::ops::results::Deployment, Mutate),
            (OrgTemplateList, org_template_list, "org.template.list", $crate::ops::args::OrgTemplateListArgs, Vec<$crate::ops::results::TemplateListEntry>, Read),
            (OrgTemplateShow, org_template_show, "org.template.show", $crate::ops::args::OrgTemplateShowArgs, $crate::ops::results::TemplateView, Read),
            (OrgTemplateInit, org_template_init, "org.template.init", $crate::ops::args::OrgTemplateInitArgs, $crate::ops::results::TemplateInit, Mutate),
            (OrgTemplateDefinitionGet, org_template_definition_get, "org.template.definition.get", $crate::ops::args::OrgTemplateDefinitionGetArgs, $crate::ops::results::DefinitionDocument, Read),
            (OrgTemplateDefinitionApply, org_template_definition_apply, "org.template.definition.apply", $crate::ops::args::OrgTemplateDefinitionApplyArgs, $crate::orch_domain::org_templates::OrgDefinition, Mutate),
            (OrgTemplateAgentList, org_template_agent_list, "org.template.agent.list", $crate::ops::args::OrgTemplateAgentListArgs, Vec<$crate::orch_domain::org_templates::AgentDefinition>, Read),
            (OrgTemplateAgentAdd, org_template_agent_add, "org.template.agent.add", $crate::ops::args::OrgTemplateAgentAddArgs, $crate::orch_domain::org_templates::AgentDefinition, Mutate),
            (OrgTemplateAgentDefinitionGet, org_template_agent_definition_get, "org.template.agent.definition.get", $crate::ops::args::OrgTemplateAgentDefinitionGetArgs, $crate::ops::results::DefinitionDocument, Read),
            (OrgTemplateAgentDefinitionApply, org_template_agent_definition_apply, "org.template.agent.definition.apply", $crate::ops::args::OrgTemplateAgentDefinitionApplyArgs, $crate::orch_domain::org_templates::AgentDefinition, Mutate),
            (OrgTemplateAgentDelete, org_template_agent_delete, "org.template.agent.delete", $crate::ops::args::OrgTemplateAgentDeleteArgs, $crate::ops::results::TemplateAgentDeleted, Destructive),
            (UpdateCheck, update_check, "update.check", $crate::ops::args::UpdateCheckArgs, $crate::ops::results::UpdateInfo, Read),
            (Subscribe, subscribe, "subscribe", $crate::ops::args::SubscribeArgs, $crate::ops::results::SubscribeAck, Control),
            (Unsubscribe, unsubscribe, "unsubscribe", $crate::ops::args::UnsubscribeArgs, (), Control),
            (RequestCancel, request_cancel, "request.cancel", $crate::ops::args::RequestCancelArgs, (), Control),
            (TransferBegin, transfer_begin, "transfer.begin", $crate::ops::args::TransferBeginArgs, $crate::ops::results::TransferHandle, Control),
            (TransferChunk, transfer_chunk, "transfer.chunk", $crate::ops::args::TransferChunkArgs, (), Control),
            (TransferFinish, transfer_finish, "transfer.finish", $crate::ops::args::TransferFinishArgs, $crate::ops::results::TransferHandle, Control),
        }
    };
}

/// A typed operation: its arguments type identifies it.
pub trait Op: Serialize + DeserializeOwned + Send + 'static {
    /// The wire name, e.g. `task.get`.
    const NAME: &'static str;
    const CLASS: OpClass;
    /// The `ok` payload of a successful response.
    type Output: Serialize + DeserializeOwned + Send + 'static;
    fn into_request(self) -> Request;
}

macro_rules! define_catalogue {
    ($( ($variant:ident, $method:ident, $name:literal, $args:ty, $out:ty, $class:ident) ),* $(,)?) => {
        /// One request: `{"op": "<wire name>", "args": {...}}`. Closed: an
        /// unknown op, a missing `args` or an extra key is a decode error.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(tag = "op", content = "args", deny_unknown_fields)]
        pub enum Request {
            $( #[serde(rename = $name)] $variant($args), )*
        }

        impl Request {
            /// The wire name.
            pub fn name(&self) -> &'static str {
                match self { $( Request::$variant(_) => $name, )* }
            }

            /// The authoritative class: an exhaustive match, no default arm.
            pub fn class(&self) -> OpClass {
                match self { $( Request::$variant(_) => OpClass::$class, )* }
            }
        }

        /// Every operation's wire name and class, in catalogue order.
        pub const CATALOGUE: &[(&str, OpClass)] = &[ $( ($name, OpClass::$class), )* ];

        $(
            impl Op for $args {
                const NAME: &'static str = $name;
                const CLASS: OpClass = OpClass::$class;
                type Output = $out;
                fn into_request(self) -> Request {
                    Request::$variant(self)
                }
            }
        )*
    };
}

crate::for_each_op!(define_catalogue);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The normative class table of the design, verbatim.
    const READ: &str = "status.get state.get ui.bootstrap task.list task.get task.definition.get task.attachment.read agent.list agent.get agent.definition.get goal.list goal.get goal.tasks goal.progress message.inbox message.list context.get context.list team.list team.get team.for_agent config.get config.definition.get config.global.get config.global.show config.prompt_mode.get run.list run.get run.events run.events_tail run.history run.timings logs.follow daemon.status remote.list remote.status doctor.run models.catalog catalog.agents catalog.templates org.list org.template.list org.template.show org.template.definition.get org.template.agent.list org.template.agent.definition.get update.check";
    const MUTATE: &str = "task.create task.assign task.approve task.review_complete task.reject task.retry task.update_fields task.definition.apply attachment.stage_image agent.create agent.update_fields agent.enable agent.disable agent.autonomous agent.definition.apply goal.create goal.update goal.status message.send message.broadcast message.mark_read context.set team.create team.join team.leave team.add_task team.remove_task team.set_lead config.set config.definition.apply config.global.set ui.preferences.patch config.prompt_mode.set config.prompt_mode.toggle state.complete_onboarding remote.add remote.listen org.deploy org.template.init org.template.definition.apply org.template.agent.add org.template.agent.definition.apply";
    const DESTRUCTIVE: &str = "task.cancel task.delete agent.remove agent.stop goal.delete context.delete team.disband remote.remove org.template.agent.delete maintenance.expire_context";
    const CONTROL: &str = "dispatch.set dispatch.tick dispatch.run_task dispatch.run_all dispatch.until_terminal daemon.stop subscribe unsubscribe request.cancel transfer.begin transfer.chunk transfer.finish";

    #[test]
    fn catalogue_matches_the_normative_class_table() {
        let expected: BTreeSet<(&str, OpClass)> = [
            (READ, OpClass::Read),
            (MUTATE, OpClass::Mutate),
            (DESTRUCTIVE, OpClass::Destructive),
            (CONTROL, OpClass::Control),
        ]
        .into_iter()
        .flat_map(|(names, class)| names.split(' ').map(move |name| (name, class)))
        .collect();
        let actual: BTreeSet<(&str, OpClass)> = CATALOGUE.iter().copied().collect();
        assert_eq!(actual.len(), CATALOGUE.len(), "duplicate op name");
        assert_eq!(actual, expected);
    }
}
