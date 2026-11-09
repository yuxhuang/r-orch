//! Service adapters for state entities. The generated Handler implementation
//! admits the entire body once; nested engine operations reuse its context.
use super::{OpContext, ServiceHandler, invalid, service_error};
use crate::coordinator::OpResult;
use orch_core::storage::{GoalFilter, TaskFilter};
use orch_domain::*;
use orch_engine::goal_service::UpdateGoalFields;
use orch_engine::task_service::UpdateTaskFields;
use orch_proto::ops::{args::*, results::*};

impl ServiceHandler {
    pub(super) async fn status_get(
        &self,
        _: &OpContext,
        _: StatusGetArgs,
    ) -> OpResult<StatusSummary> {
        Ok(StatusSummary {
            project_name: self
                .engine
                .config_store
                .read()
                .await
                .map_err(service_error)?
                .project
                .name,
            tasks: self
                .engine
                .task_service
                .list(&TaskFilter::default())
                .await
                .map_err(service_error)?,
            agents: self
                .engine
                .agent_service
                .list()
                .await
                .map_err(service_error)?,
            state: self.engine.state.read().await.map_err(service_error)?,
        })
    }
    pub(super) async fn state_get(
        &self,
        _: &OpContext,
        _: StateGetArgs,
    ) -> OpResult<OrchestratorState> {
        self.engine.state.read().await.map_err(service_error)
    }
    pub(super) async fn ui_bootstrap(
        &self,
        _: &OpContext,
        _: UiBootstrapArgs,
    ) -> OpResult<LaunchContext> {
        let config = self
            .engine
            .config_store
            .read()
            .await
            .map_err(service_error)?;
        let global = self
            .engine
            .global_config_store
            .read()
            .await
            .map_err(service_error)?;
        let agents = self
            .engine
            .agent_service
            .list()
            .await
            .map_err(service_error)?;
        let startup_warnings = orch_engine::prompt_mode::startup_diagnostics(&config, &agents)
            .into_iter()
            .map(|diagnostic| {
                format!(
                    "{} ({}): {}",
                    diagnostic.agent_name, diagnostic.agent_id, diagnostic.message
                )
            })
            .collect();
        Ok(LaunchContext {
            project_name: config.project.name,
            version: orch_proto::BINARY_VERSION.into(),
            default_adapter: config.defaults.agent.adapter.to_string(),
            palette: global.tui.palette,
            activity_filter: global.tui.activity_filter,
            notifications: global.tui.notifications,
            max_concurrent: config
                .scheduling
                .max_concurrent_agents
                .try_into()
                .map_err(|_| invalid("configured concurrency exceeds protocol limit"))?,
            dispatch_on: self.dispatch_state().dispatch_on,
            prompt_mode: config.defaults.agent.prompt_mode,
            startup_warnings,
        })
    }
    pub(super) async fn task_create(&self, _: &OpContext, a: TaskCreateArgs) -> OpResult<Task> {
        let attachments = attachment_paths(a.attachments)?;
        let sources = attachments.clone().unwrap_or_default();
        let task = self
            .engine
            .task_service
            .create(CreateTaskInput {
                title: a.title.into_inner(),
                description: a.description.map(|s| s.into_inner()),
                priority: a.priority,
                assignee: a.assignee.map(|s| s.into_inner()),
                labels: a.labels.map(strings),
                depends_on: a.depends_on.map(strings),
                max_attempts: a.max_attempts,
                workspace_mode: a.workspace_mode,
                review_criteria: a.review_criteria.map(|v| v.into_inner()),
                scope: a.scope.map(strings),
                goal_id: a.goal_id.map(|s| s.into_inner()),
                attachments: attachments
                    .map(|paths| {
                        paths
                            .into_iter()
                            .map(|path| {
                                path.into_os_string().into_string().map_err(|_| {
                                    invalid("attachment source path is not valid UTF-8")
                                })
                            })
                            .collect::<OpResult<Vec<_>>>()
                    })
                    .transpose()?,
            })
            .await
            .map_err(service_error)?;
        self.finish_staged_paths(&sources);
        Ok(task)
    }
    pub(super) async fn task_list(&self, _: &OpContext, a: TaskListArgs) -> OpResult<Vec<Task>> {
        self.engine
            .task_service
            .list(&TaskFilter {
                status: a.status,
                goal_id: a.goal_id.map(|s| s.into_inner()),
            })
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_get(&self, _: &OpContext, a: TaskGetArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .get(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_assign(&self, _: &OpContext, a: TaskAssignArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .assign(&a.id, &a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_cancel(&self, _: &OpContext, a: TaskCancelArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .cancel(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_approve(&self, _: &OpContext, a: TaskApproveArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .approve(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_review_complete(
        &self,
        _: &OpContext,
        a: TaskReviewCompleteArgs,
    ) -> OpResult<Task> {
        self.engine
            .task_service
            .update_status(&a.id, TaskStatus::Done)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_reject(&self, _: &OpContext, a: TaskRejectArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .reject(&a.id, a.feedback.map(|s| s.into_inner()))
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_retry(&self, _: &OpContext, a: TaskRetryArgs) -> OpResult<Task> {
        self.engine
            .task_service
            .retry(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_delete(&self, _: &OpContext, a: TaskDeleteArgs) -> OpResult<()> {
        self.engine
            .task_service
            .delete(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn task_update_fields(
        &self,
        _: &OpContext,
        a: TaskUpdateFieldsArgs,
    ) -> OpResult<Task> {
        let attachments = attachment_paths(a.attachments)?;
        let sources = attachments.clone().unwrap_or_default();
        let task = self
            .engine
            .task_service
            .update(
                &a.id,
                UpdateTaskFields {
                    title: a.title.map(|s| s.into_inner()),
                    description: a.description.map(|s| s.into_inner()),
                    priority: a.priority,
                    labels: a.labels.map(strings),
                    attachments,
                },
            )
            .await
            .map_err(service_error)?;
        self.finish_staged_paths(&sources);
        Ok(task)
    }
    pub(super) async fn agent_list(&self, _: &OpContext, _: AgentListArgs) -> OpResult<Vec<Agent>> {
        let agents = self
            .engine
            .agent_service
            .list()
            .await
            .map_err(service_error)?;
        let remote =
            orch_infra::storage::remote::RemoteAgentDirectory::new(self.engine.paths.clone());
        remote.list(agents).await.map_err(service_error)
    }
    pub(super) async fn agent_get(&self, _: &OpContext, a: AgentGetArgs) -> OpResult<AgentView> {
        let agent = if a.id.contains('@') {
            orch_infra::storage::remote::RemoteAgentDirectory::new(self.engine.paths.clone())
                .resolve(&a.id, Vec::new())
                .await
                .map_err(service_error)?
                .ok_or_else(|| {
                    service_error(orch_core::error::Error::Domain(DomainError::AgentNotFound(
                        a.id.to_string(),
                    )))
                })?
        } else {
            self.engine
                .agent_service
                .get(&a.id)
                .await
                .map_err(service_error)?
        };
        let effective_config = if a.effective {
            let config = self
                .engine
                .config_store
                .read()
                .await
                .map_err(service_error)?;
            Some(EffectiveConfig {
                prompt_mode: agent
                    .config
                    .prompt_mode
                    .unwrap_or(config.defaults.agent.prompt_mode),
                prompt_mode_source: if agent.config.prompt_mode.is_some() {
                    PromptModeSource::Agent
                } else {
                    PromptModeSource::Project
                },
            })
        } else {
            None
        };
        Ok(AgentView {
            agent,
            effective_config,
        })
    }
    pub(super) async fn agent_remove(&self, _: &OpContext, a: AgentRemoveArgs) -> OpResult<()> {
        self.engine
            .agent_service
            .remove(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn agent_enable(&self, _: &OpContext, a: AgentEnableArgs) -> OpResult<Agent> {
        self.engine
            .agent_service
            .enable(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn agent_disable(
        &self,
        _: &OpContext,
        a: AgentDisableArgs,
    ) -> OpResult<Agent> {
        self.engine
            .agent_service
            .disable(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn agent_autonomous(
        &self,
        _: &OpContext,
        a: AgentAutonomousArgs,
    ) -> OpResult<Agent> {
        let enabled = match a.enabled {
            Some(value) => value,
            None => !self
                .engine
                .agent_service
                .get(&a.id)
                .await
                .map_err(service_error)?
                .autonomous
                .unwrap_or(false),
        };
        self.engine
            .agent_service
            .set_autonomous(&a.id, enabled)
            .await
            .map_err(service_error)
    }
    pub(super) async fn agent_stop(&self, _: &OpContext, a: AgentStopArgs) -> OpResult<()> {
        self.engine
            .orchestrator
            .force_stop_agent(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_create(&self, _: &OpContext, a: GoalCreateArgs) -> OpResult<Goal> {
        self.engine
            .goal_service
            .create(CreateGoalInput {
                title: a.title.into_inner(),
                description: a.description.map(|s| s.into_inner()),
                assignee: a.assignee.map(|s| s.into_inner()),
            })
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_list(&self, _: &OpContext, a: GoalListArgs) -> OpResult<Vec<Goal>> {
        self.engine
            .goal_service
            .list(&GoalFilter { status: a.status })
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_get(&self, _: &OpContext, a: GoalGetArgs) -> OpResult<GoalView> {
        Ok(GoalView {
            goal: self
                .engine
                .goal_service
                .get(&a.id)
                .await
                .map_err(service_error)?,
            tasks: self
                .engine
                .goal_service
                .list_tasks_for_goal(&a.id)
                .await
                .map_err(service_error)?,
            progress: self
                .engine
                .goal_service
                .get_progress_report(&a.id)
                .await
                .map_err(service_error)?,
        })
    }
    pub(super) async fn goal_update(&self, _: &OpContext, a: GoalUpdateArgs) -> OpResult<Goal> {
        self.engine
            .goal_service
            .update(
                &a.id,
                &UpdateGoalFields {
                    title: a.title.map(|s| s.into_inner()),
                    description: a.description.map(|s| s.into_inner()),
                    assignee: a.assignee.map(|s| s.into_inner()),
                },
            )
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_status(&self, _: &OpContext, a: GoalStatusArgs) -> OpResult<Goal> {
        self.engine
            .goal_service
            .update_status(&a.id, a.status, a.force)
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_delete(&self, _: &OpContext, a: GoalDeleteArgs) -> OpResult<()> {
        self.engine
            .goal_service
            .delete(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_tasks(&self, _: &OpContext, a: GoalTasksArgs) -> OpResult<Vec<Task>> {
        self.engine
            .goal_service
            .list_tasks_for_goal(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn goal_progress(
        &self,
        _: &OpContext,
        a: GoalProgressArgs,
    ) -> OpResult<Option<String>> {
        self.engine
            .goal_service
            .get_progress_report(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn message_send(
        &self,
        _: &OpContext,
        a: MessageSendArgs,
    ) -> OpResult<Vec<Message>> {
        self.engine
            .message_service
            .send(CreateMessageInput {
                from_agent_id: a.from_agent_id.into_inner(),
                to_agent_id: Some(a.to_agent_id.into_inner()),
                channel: MessageChannel::Direct,
                subject: a.subject.into_inner(),
                body: a.body.into_inner(),
                ttl_ms: a.ttl_ms,
                team_id: None,
                reply_to: a.reply_to.map(|s| s.into_inner()),
            })
            .await
            .map_err(service_error)
    }
    pub(super) async fn message_broadcast(
        &self,
        _: &OpContext,
        a: MessageBroadcastArgs,
    ) -> OpResult<Vec<Message>> {
        self.engine
            .message_service
            .send(CreateMessageInput {
                from_agent_id: a.from_agent_id.into_inner(),
                to_agent_id: None,
                channel: MessageChannel::Broadcast,
                subject: a.subject.into_inner(),
                body: a.body.into_inner(),
                ttl_ms: a.ttl_ms,
                team_id: a.team_id.map(|s| s.into_inner()),
                reply_to: None,
            })
            .await
            .map_err(service_error)
    }
    pub(super) async fn message_inbox(
        &self,
        _: &OpContext,
        a: MessageInboxArgs,
    ) -> OpResult<Vec<Message>> {
        self.engine
            .message_service
            .list_pending_for_agent(&a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn message_list(
        &self,
        _: &OpContext,
        a: MessageListArgs,
    ) -> OpResult<Vec<Message>> {
        match a.agent_id {
            Some(id) => self
                .engine
                .message_service
                .list_for_agent(&id)
                .await
                .map_err(service_error),
            None => self
                .engine
                .message_service
                .list_all()
                .await
                .map_err(service_error),
        }
    }
    pub(super) async fn message_mark_read(
        &self,
        _: &OpContext,
        a: MessageMarkReadArgs,
    ) -> OpResult<Vec<Message>> {
        let mut messages = Vec::new();
        for id in a.ids.into_inner() {
            let message = self
                .engine
                .messages
                .get(&id)
                .await
                .map_err(service_error)?
                .ok_or_else(|| invalid("message does not exist"))?;
            if message.to_agent_id.as_deref() != Some(a.agent_id.as_ref()) {
                return Err(invalid("message belongs to another recipient"));
            }
            self.engine
                .messages
                .mark_delivered(&id)
                .await
                .map_err(service_error)?;
            if let Some(message) = self.engine.messages.get(&id).await.map_err(service_error)? {
                messages.push(message);
            }
        }
        Ok(messages)
    }
    pub(super) async fn context_set(
        &self,
        ctx: &OpContext,
        a: ContextSetArgs,
    ) -> OpResult<Option<ContextEntry>> {
        if a.ttl_ms.is_some_and(|ttl| ttl == 0 || ttl > 2_592_000_000) {
            return Err(invalid("TTL must be between 1ms and 2592000000ms"));
        }
        self.engine
            .context
            .set(&a.key, &a.value, a.ttl_ms)
            .await
            .map_err(service_error)?;
        self.context_get(ctx, ContextGetArgs { key: a.key }).await
    }
    pub(super) async fn context_get(
        &self,
        _: &OpContext,
        a: ContextGetArgs,
    ) -> OpResult<Option<ContextEntry>> {
        self.engine
            .context
            .get(&a.key)
            .await
            .map_err(service_error)?
            .map(context_entry)
            .transpose()
    }
    pub(super) async fn context_list(
        &self,
        _: &OpContext,
        _: ContextListArgs,
    ) -> OpResult<Vec<ContextEntry>> {
        self.engine
            .context
            .list()
            .await
            .map_err(service_error)?
            .into_iter()
            .map(context_entry)
            .collect()
    }
    pub(super) async fn context_delete(&self, _: &OpContext, a: ContextDeleteArgs) -> OpResult<()> {
        self.engine
            .context
            .delete(&a.key)
            .await
            .map_err(service_error)
    }
    pub(super) async fn maintenance_expire_context(
        &self,
        _: &OpContext,
        _: MaintenanceExpireContextArgs,
    ) -> OpResult<ExpiryReport> {
        Ok(ExpiryReport {
            removed: self
                .engine
                .context_service
                .expire_now()
                .await
                .map_err(service_error)?
                .len() as u64,
        })
    }
    pub(super) async fn team_create(&self, _: &OpContext, a: TeamCreateArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .create(CreateTeamInput {
                name: a.name.into_inner(),
                description: a.description.map(|s| s.into_inner()),
                lead_agent_id: a.lead_agent_id.into_inner(),
                member_agent_ids: a.member_agent_ids.map(strings),
                config: Some(PartialTeamConfig {
                    max_concurrent_tasks: a.max_concurrent_tasks,
                    auto_claim: a.auto_claim,
                    message_ttl_ms: a.message_ttl_ms,
                }),
            })
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_list(&self, _: &OpContext, _: TeamListArgs) -> OpResult<Vec<Team>> {
        self.engine.team_service.list().await.map_err(service_error)
    }
    pub(super) async fn team_get(&self, _: &OpContext, a: TeamGetArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .get(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_join(&self, _: &OpContext, a: TeamJoinArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .join(&a.team_id, &a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_leave(&self, _: &OpContext, a: TeamLeaveArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .leave(&a.team_id, &a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_add_task(&self, _: &OpContext, a: TeamAddTaskArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .add_task(&a.team_id, &a.task_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_remove_task(
        &self,
        _: &OpContext,
        a: TeamRemoveTaskArgs,
    ) -> OpResult<Team> {
        self.engine
            .team_service
            .remove_task(&a.team_id, &a.task_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_set_lead(&self, _: &OpContext, a: TeamSetLeadArgs) -> OpResult<Team> {
        self.engine
            .team_service
            .set_lead(&a.team_id, &a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_disband(&self, _: &OpContext, a: TeamDisbandArgs) -> OpResult<()> {
        self.engine
            .team_service
            .disband(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn team_for_agent(
        &self,
        _: &OpContext,
        a: TeamForAgentArgs,
    ) -> OpResult<Option<Team>> {
        self.engine
            .team_service
            .find_team_for_agent(&a.agent_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn run_list(&self, _: &OpContext, a: RunListArgs) -> OpResult<Vec<Run>> {
        let mut runs = self
            .engine
            .run_service
            .list_all()
            .await
            .map_err(service_error)?;
        runs.retain(|run| {
            a.task_id
                .as_ref()
                .is_none_or(|id| run.task_id == id.as_ref())
                && a.agent_id
                    .as_ref()
                    .is_none_or(|id| run.agent_id == id.as_ref())
        });
        if let Some(since) = a.since_ms {
            let cutoff = self
                .engine
                .clock
                .now_ms()
                .saturating_sub(since.min(i64::MAX as u64) as i64);
            runs.retain(|r| {
                r.finished_at
                    .as_ref()
                    .and_then(|s| orch_core::clock::epoch_ms_from_iso(s))
                    .is_none_or(|time| time >= cutoff)
            });
        }
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if let Some(limit) = a.limit {
            runs.truncate(limit as usize);
        }
        Ok(runs)
    }
    pub(super) async fn run_get(&self, _: &OpContext, a: RunGetArgs) -> OpResult<Option<Run>> {
        self.engine
            .run_service
            .get(&a.id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn run_events(
        &self,
        _: &OpContext,
        a: RunEventsArgs,
    ) -> OpResult<Vec<RunEvent>> {
        self.engine
            .run_service
            .read_events(&a.run_id)
            .await
            .map_err(service_error)
    }
    pub(super) async fn run_events_tail(
        &self,
        _: &OpContext,
        a: RunEventsTailArgs,
    ) -> OpResult<Vec<RunEvent>> {
        self.engine
            .run_service
            .read_events_tail(&a.run_id, a.count as usize)
            .await
            .map_err(service_error)
    }
}

fn strings<const N: usize, const M: usize>(
    values: orch_proto::bounded::BoundedVec<orch_proto::bounded::BoundedString<N>, M>,
) -> Vec<String> {
    values
        .into_inner()
        .into_iter()
        .map(|s| s.into_inner())
        .collect()
}

fn context_entry(entry: orch_core::storage::ContextEntry) -> OpResult<ContextEntry> {
    serde_json::from_value(
        serde_json::to_value(entry).map_err(|_| invalid("context result encoding failed"))?,
    )
    .map_err(|_| invalid("context result encoding failed"))
}

fn attachment_paths(
    paths: Option<orch_proto::bounded::BoundedVec<orch_proto::path::WirePath, 64>>,
) -> OpResult<Option<Vec<std::path::PathBuf>>> {
    paths
        .map(|paths| {
            paths
                .into_inner()
                .into_iter()
                .map(|path| {
                    let path = path.into_path_buf();
                    if !path.is_absolute() {
                        return Err(invalid("attachment source paths must be absolute"));
                    }
                    Ok(path)
                })
                .collect()
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::super::Handler;
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn fixture() -> (Root, ServiceHandler, OpContext) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "orch-core-op-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::create_dir(path.join(".orchestry")).unwrap();
        let root = Root(path);
        let engine = Arc::new(
            orch_engine::build_full_with(
                &root.0,
                orch_engine::EngineSeams {
                    global_config_path: Some(root.0.join("global.yml")),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
        let coordinator = Arc::new(crate::coordinator::ProjectCoordinator::new(
            engine.events.operation_boundary(),
        ));
        let handler = ServiceHandler::new(engine, coordinator);
        let ctx = OpContext::new(orch_proto::handshake::Session {
            caller: orch_proto::handshake::Caller {
                kind: orch_proto::handshake::CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            mode: orch_proto::handshake::SessionMode::Normal,
        });
        (root, handler, ctx)
    }

    fn args<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn task_handlers_reject_relative_attachments_before_creating_then_preserve_fields() {
        let (root, handler, ctx) = fixture().await;
        let relative = TaskCreateArgs {
            attachments: Some(
                orch_proto::bounded::BoundedVec::new(vec![
                    orch_proto::path::WirePath::new("relative.txt").unwrap(),
                ])
                .unwrap(),
            ),
            ..args(serde_json::json!({"title":"rejected"}))
        };
        assert!(
            Handler::task_create(&handler, &ctx, relative)
                .await
                .is_err()
        );
        assert!(
            Handler::task_list(&handler, &ctx, args(serde_json::json!({})))
                .await
                .unwrap()
                .is_empty()
        );
        let attachment = root.0.join("source.txt");
        std::fs::write(&attachment, "attached bytes").unwrap();
        let create = TaskCreateArgs {
            attachments: Some(
                orch_proto::bounded::BoundedVec::new(vec![
                    orch_proto::path::WirePath::new(attachment).unwrap(),
                ])
                .unwrap(),
            ),
            ..args(serde_json::json!({"title":"original","priority":2}))
        };
        let task = Handler::task_create(&handler, &ctx, create).await.unwrap();
        assert_eq!(task.attachments.as_ref().unwrap().len(), 1);
        let updated = Handler::task_update_fields(
            &handler,
            &ctx,
            args(serde_json::json!({"id":task.id,"title":"new"})),
        )
        .await
        .unwrap();
        assert_eq!(updated.title, "new");
        assert_eq!(updated.priority, 2);
        assert_eq!(updated.attachments, task.attachments);
        let relative_update = TaskUpdateFieldsArgs {
            id: task.id.try_into().unwrap(),
            attachments: Some(
                orch_proto::bounded::BoundedVec::new(vec![
                    orch_proto::path::WirePath::new("relative").unwrap(),
                ])
                .unwrap(),
            ),
            ..args(serde_json::json!({"id":"unused"}))
        };
        assert!(
            Handler::task_update_fields(&handler, &ctx, relative_update)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn goal_edit_preserves_pause_and_context_reads_do_not_expire_values() {
        let (_root, handler, ctx) = fixture().await;
        let goal = Handler::goal_create(
            &handler,
            &ctx,
            args(serde_json::json!({"title":"original"})),
        )
        .await
        .unwrap();
        Handler::goal_status(
            &handler,
            &ctx,
            args(serde_json::json!({"id":goal.id,"status":"paused"})),
        )
        .await
        .unwrap();
        let changed = Handler::goal_update(
            &handler,
            &ctx,
            args(serde_json::json!({"id":goal.id,"title":"new"})),
        )
        .await
        .unwrap();
        assert_eq!(changed.status, GoalStatus::Paused);
        assert_eq!(changed.title, "new");
        let set = Handler::context_set(
            &handler,
            &ctx,
            args(serde_json::json!({"key":"key","value":"value"})),
        )
        .await
        .unwrap()
        .unwrap();
        let path = handler.engine.paths.context_path("key").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value = Handler::context_get(&handler, &ctx, args(serde_json::json!({"key":"key"})))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value, set);
        assert_eq!(
            Handler::context_list(&handler, &ctx, ContextListArgs {})
                .await
                .unwrap(),
            vec![set]
        );
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        Handler::context_delete(&handler, &ctx, args(serde_json::json!({"key":"key"})))
            .await
            .unwrap();
        assert!(
            Handler::context_list(&handler, &ctx, ContextListArgs {})
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn message_read_marking_is_separate_from_inbox_and_team_methods_round_trip() {
        let (_root, handler, ctx) = fixture().await;
        let agent = Handler::agent_create(
            &handler,
            &ctx,
            args(serde_json::json!({"name":"worker","adapter":"shell","command":"printf hello"})),
        )
        .await
        .unwrap()
        .agent;
        let team = Handler::team_create(
            &handler,
            &ctx,
            args(serde_json::json!({"name":"team","lead_agent_id":agent.id})),
        )
        .await
        .unwrap();
        assert_eq!(
            Handler::team_for_agent(
                &handler,
                &ctx,
                args(serde_json::json!({"agent_id":agent.id}))
            )
            .await
            .unwrap()
            .unwrap()
            .id,
            team.id
        );
        let messages = Handler::message_send(
            &handler,
            &ctx,
            args(serde_json::json!({"from_agent_id":"cli","to_agent_id":agent.id,"body":"hello"})),
        )
        .await
        .unwrap();
        let inbox = Handler::message_inbox(
            &handler,
            &ctx,
            args(serde_json::json!({"agent_id":agent.id})),
        )
        .await
        .unwrap();
        assert_eq!(messages, inbox);
        assert_eq!(inbox[0].status, MessageStatus::Pending);
        let marked = Handler::message_mark_read(
            &handler,
            &ctx,
            args(serde_json::json!({"agent_id":agent.id,"ids":[messages[0].id]})),
        )
        .await
        .unwrap();
        assert_eq!(marked[0].status, MessageStatus::Delivered);
        assert!(
            Handler::message_inbox(
                &handler,
                &ctx,
                args(serde_json::json!({"agent_id":agent.id}))
            )
            .await
            .unwrap()
            .is_empty()
        );
        Handler::team_disband(&handler, &ctx, args(serde_json::json!({"id":team.id})))
            .await
            .unwrap();
        assert_eq!(
            Handler::team_get(&handler, &ctx, args(serde_json::json!({"id":team.id})))
                .await
                .unwrap()
                .status,
            TeamStatus::Disbanded
        );
    }
}
