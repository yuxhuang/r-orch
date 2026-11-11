use std::sync::Arc;

use orch_core::error::{Error, Result};
use orch_domain::{AgentStatus, OrchestratorEvent, RunStatus, TaskProof, TaskStatus, TokenUsage};

use super::super::MAX_AGENT_SUMMARY_LEN;
use super::super::Orchestrator;
use crate::agent_service::AgentStatsUpdate;
use crate::review_runner::{ReviewRunner, ReviewRunnerOptions, all_passed, format_report};

impl Orchestrator {
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the collector completion seam"
    )]
    pub(crate) async fn daemon_handle_success(
        &self,
        task_id: &str,
        run_id: &str,
        agent_id: &str,
        tokens: Option<TokenUsage>,
        result_text: Option<String>,
        mut files_changed: Vec<String>,
    ) -> Result<()> {
        let boundary = self.deps().events.operation_boundary();
        let snapshot = boundary
            .run_from(
                crate::container::commit::CommitSource::Collector,
                false,
                async {
                    let slot = self.inner.state.lock().await;
                    if self.inner.terminal_intents.lock().await.contains(run_id)
                        || self.is_shutting_down()
                        || !slot.as_ref().is_some_and(|state| {
                            state
                                .running
                                .get(task_id)
                                .is_some_and(|entry| entry.run_id == run_id)
                        })
                    {
                        return Ok(None);
                    }
                    self.deps()
                        .task_store
                        .get(task_id)
                        .await
                        .map(|task| task.filter(|task| !orch_domain::is_terminal(task.status)))
                },
            )
            .await
            .value?;
        let Some(snapshot) = snapshot else {
            return Ok(());
        };

        let branch = snapshot
            .proof
            .as_ref()
            .and_then(|proof| proof.branch.as_deref());
        if files_changed.is_empty()
            && let Some(branch) = branch
        {
            files_changed = self.deps().workspace.get_changed_files(branch).await?;
        }
        let merge_error = if let Some(branch) = branch {
            match self.deps().workspace.merge_back(branch).await {
                Ok(result) if result.success() => {
                    self.emit(OrchestratorEvent::WorkspaceMergeSucceeded {
                        task_id: task_id.to_owned(),
                        branch: branch.to_owned(),
                    });
                    if let Err(error) = self.deps().workspace.cleanup(task_id, Some(branch)).await {
                        self.emit_error(error, format!("workspace cleanup for {task_id}"));
                    }
                    None
                }
                Ok(result) => {
                    let conflict = result.conflict_info().unwrap_or_default().to_owned();
                    self.emit(OrchestratorEvent::WorkspaceMergeConflict {
                        task_id: task_id.to_owned(),
                        branch: branch.to_owned(),
                        conflict_info: conflict.clone(),
                    });
                    Some(format!("MERGE CONFLICT: {conflict}"))
                }
                Err(error) => Some(format!("MERGE ERROR: {error}")),
            }
        } else {
            None
        };
        let criteria = snapshot.review_criteria.clone().unwrap_or_default();
        let reviews = if merge_error.is_none() && !criteria.is_empty() {
            let cwd = snapshot
                .workspace
                .clone()
                .unwrap_or_else(|| self.deps().project_root.display().to_string());
            Some(
                ReviewRunner::with_runner(
                    ReviewRunnerOptions::new(cwd),
                    Arc::clone(&self.deps().command_runner),
                )
                .run_all(&criteria)
                .await,
            )
        } else {
            None
        };

        boundary
            .run_from(
                crate::container::commit::CommitSource::Collector,
                false,
                async {
                    let mut slot = self.inner.state.lock().await;
                    let Some(state) = slot.as_mut() else {
                        return Ok(());
                    };
                    if self.inner.terminal_intents.lock().await.contains(run_id)
                        || self.is_shutting_down()
                        || !state
                            .running
                            .get(task_id)
                            .is_some_and(|entry| entry.run_id == run_id)
                    {
                        return Ok(());
                    }
                    // Cancellation and field edits may have committed while git/review
                    // ran. Re-read now; only this run's runtime fields are changed.
                    let Some(mut task) = self.deps().task_store.get(task_id).await? else {
                        return Ok(());
                    };
                    if orch_domain::is_terminal(task.status) {
                        return Ok(());
                    }
                    self.flush_state_lazy(state).await;
                    self.drop_cancel(task_id).await;
                    let existing = task.proof.take().unwrap_or_default();
                    let summary = result_text
                        .as_ref()
                        .map(|text| text.chars().take(MAX_AGENT_SUMMARY_LEN).collect::<String>())
                        .or(existing.agent_summary.clone());
                    task.proof = Some(TaskProof {
                        agent_summary: merge_error
                            .as_ref()
                            .map(|prefix| {
                                format!("{prefix}\n\n{}", summary.as_deref().unwrap_or_default())
                                    .chars()
                                    .take(MAX_AGENT_SUMMARY_LEN)
                                    .collect()
                            })
                            .or(summary),
                        files_changed: if files_changed.is_empty() {
                            existing.files_changed.clone()
                        } else {
                            files_changed
                        },
                        ..existing
                    });
                    task.feedback = None;
                    let agent = self.deps().agent_store.get(agent_id).await?;
                    let auto_approve = task.is_autonomous()
                        || task
                            .labels
                            .iter()
                            .any(|label| label.starts_with("remote-ctl:"))
                        || agent
                            .as_ref()
                            .and_then(|agent| agent.config.approval_policy)
                            == Some(orch_domain::ApprovalPolicy::Auto);
                    let same_criteria =
                        task.review_criteria.as_deref().unwrap_or_default() == criteria;
                    let mut approved = auto_approve && merge_error.is_none() && same_criteria;
                    if let Some(reviews) = reviews.filter(|_| same_criteria) {
                        let passed = all_passed(&reviews);
                        let proof = task.proof.as_mut().expect("proof initialized");
                        proof.test_results = Some(format_report(&reviews));
                        task.review_results = Some(reviews.clone());
                        self.emit(OrchestratorEvent::TaskAutoReviewed {
                            task_id: task_id.to_owned(),
                            passed,
                            results: reviews,
                        });
                        approved = merge_error.is_none()
                            && (passed || auto_approve)
                            && (passed
                                || !task
                                    .labels
                                    .iter()
                                    .any(|label| label.starts_with("remote-ctl:")));
                    }
                    self.deps().task_store.save(&task).await?;
                    self.deps()
                        .run_service
                        .finish(run_id, RunStatus::Succeeded, tokens, None)
                        .await?;
                    let runtime_ms = state
                        .running
                        .get(task_id)
                        .and_then(|entry| self.runtime_ms(entry))
                        .unwrap_or(0);
                    state.running.shift_remove(task_id);
                    state.stats.total_runtime_ms += runtime_ms;
                    state.stats.total_tasks_completed += 1;
                    state.stats.total_runs += 1;
                    if let Some(tokens) = tokens {
                        let total = &mut state.stats.total_tokens;
                        total.input += tokens.input;
                        total.output += tokens.output;
                        total.reasoning += tokens.reasoning;
                        total.cache_read += tokens.cache_read;
                        total.cache_write += tokens.cache_write;
                        total.total = total.input + total.output + total.reasoning;
                    }
                    let previous = agent.as_ref().map(|agent| &agent.stats);
                    self.deps()
                        .agent_service
                        .update_stats(
                            agent_id,
                            AgentStatsUpdate {
                                tasks_completed: Some(
                                    previous.map_or(0, |stats| stats.tasks_completed) + 1,
                                ),
                                total_runs: Some(previous.map_or(0, |stats| stats.total_runs) + 1),
                                total_runtime_ms: Some(
                                    previous.map_or(0, |stats| stats.total_runtime_ms) + runtime_ms,
                                ),
                                tokens_used: tokens.map(|tokens| {
                                    previous.and_then(|stats| stats.tokens_used).unwrap_or(0)
                                        + tokens.total
                                }),
                                ..Default::default()
                            },
                        )
                        .await?;
                    self.deps()
                        .task_service
                        .update_status(task_id, TaskStatus::Review)
                        .await?;
                    if approved {
                        self.deps()
                            .task_service
                            .update_status(task_id, TaskStatus::Done)
                            .await?;
                    }
                    self.deps()
                        .agent_service
                        .set_status(agent_id, AgentStatus::Idle)
                        .await?;
                    if let Some(mut agent) = self.deps().agent_store.get(agent_id).await? {
                        agent.current_task = None;
                        self.deps().agent_store.save(&agent).await?;
                    }
                    self.save_state(state).await
                },
            )
            .await
            .value
    }

    pub(crate) async fn daemon_drain_cleanups(&self) {
        let _draining = self.inner.cleanup_draining.lock().await;
        let cleanups = std::mem::take(&mut *self.inner.deferred_cleanups.lock().await);
        for (task_id, branch) in cleanups {
            if let Err(error) = self.deps().workspace.cleanup(&task_id, Some(&branch)).await {
                self.emit_error(error, format!("workspace cleanup for {task_id}"));
            }
        }
    }

    pub(crate) async fn daemon_force_stop_agent(&self, agent_id: &str) -> Result<()> {
        let tasks = self
            .deps()
            .events
            .operation_boundary()
            .run_from(
                crate::container::commit::CommitSource::Collector,
                false,
                async {
                    let mut slot = self.inner.state.lock().await;
                    self.load_state(&mut slot).await?;
                    let state = slot.as_ref().expect("state loaded");
                    Ok::<_, Error>(
                        state
                            .running
                            .iter()
                            .filter(|(_, entry)| entry.agent_id == agent_id)
                            .map(|(id, _)| id.clone())
                            .chain(
                                self.inner
                                    .preparing_agents
                                    .lock()
                                    .await
                                    .iter()
                                    .filter(|(_, (_, id))| id == agent_id)
                                    .map(|(id, _)| id.clone()),
                            )
                            .collect::<Vec<_>>(),
                    )
                },
            )
            .await
            .value?;
        for task_id in tasks {
            self.daemon_cancel_task(&task_id, false).await?;
            self.deps()
                .events
                .operation_boundary()
                .run_from(
                    crate::container::commit::CommitSource::Collector,
                    false,
                    async {
                        if let Some(mut task) = self.deps().task_store.get(&task_id).await? {
                            task.status = TaskStatus::Failed;
                            task.updated_at = self.deps().clock.now_iso();
                            self.deps().task_store.save(&task).await?;
                        }
                        Ok::<_, Error>(())
                    },
                )
                .await
                .value?;
        }
        Ok(())
    }
}
