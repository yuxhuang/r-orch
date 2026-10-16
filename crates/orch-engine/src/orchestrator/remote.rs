//! Controller lifecycle without process handles or local workspace effects.

use futures::{StreamExt, stream::BoxStream};
use orch_core::remote::RemoteRunEvent;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use orch_core::error::Result;
use orch_core::remote::{RemoteBridge, RemoteOutcome, RunEventTap};
use orch_domain::{
    OrchestratorEvent, OrchestratorState, RemoteRunRef, RunStatus, TaskProof, TaskStatus,
};

use super::Orchestrator;

impl Orchestrator {
    /// Install before starting the owner. Light commands never create a link.
    pub fn set_remote_bridge(&self, bridge: Arc<dyn RemoteBridge>) {
        *self
            .inner
            .remote_bridge
            .write()
            .unwrap_or_else(|e| e.into_inner()) = bridge;
    }

    pub fn set_run_event_tap(&self, tap: Arc<dyn RunEventTap>) {
        *self
            .inner
            .event_tap
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(tap);
    }

    /// Publish the controller this instance's peer listener is serving, or
    /// `None` once its session ends. Set by the peer worker on admission.
    pub fn set_inbound_controller(&self, instance_id: Option<String>) {
        *self
            .inner
            .inbound_controller
            .write()
            .unwrap_or_else(|e| e.into_inner()) = instance_id;
    }

    pub(super) fn inbound_controller(&self) -> Option<String> {
        self.inner
            .inbound_controller
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Release the owner's in-process tap when its peer runtime stops.
    pub fn clear_run_event_tap(&self) {
        *self
            .inner
            .event_tap
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(super) fn remote_bridge(&self) -> Arc<dyn RemoteBridge> {
        Arc::clone(
            &self
                .inner
                .remote_bridge
                .read()
                .unwrap_or_else(|e| e.into_inner()),
        )
    }

    pub(super) async fn cancel_remote(&self, remote: &RemoteRunRef) {
        if let Err(err) = self
            .remote_bridge()
            .cancel(&remote.alias, &remote.exec_key)
            .await
        {
            self.emit_error(
                err,
                format!("cancel remote {}:{}", remote.alias, remote.exec_key),
            );
        }
    }

    pub(super) async fn fail_remote_unknown_locked(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        error: &str,
    ) -> Result<()> {
        self.take_cancel(task_id).await;
        self.persist_remote_unknown_locked(state, task_id, error)
            .await
    }

    async fn persist_remote_unknown_locked(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        error: &str,
    ) -> Result<()> {
        let Some(entry) = state.running.get(task_id).cloned() else {
            return self.save_remote_state(state).await;
        };
        self.deps()
            .run_service
            .finish(
                &entry.run_id,
                RunStatus::Failed,
                None,
                Some(error.to_owned()),
            )
            .await?;
        if let Some(mut task) = self.deps().task_store.get(task_id).await?
            && !orch_domain::is_terminal(task.status)
        {
            let from = task.status;
            task.status = TaskStatus::Failed;
            task.updated_at = self.deps().clock.now_iso();
            let mut proof = task.proof.unwrap_or_default();
            proof.agent_summary = Some(error.to_owned());
            task.proof = Some(proof);
            self.deps().task_store.save(&task).await?;
            self.emit(OrchestratorEvent::TaskStatusChanged {
                task_id: task_id.to_owned(),
                from,
                to: TaskStatus::Failed,
            });
        }
        state.running.shift_remove(task_id);
        state.claimed.shift_remove(task_id);
        state.retry_queue.retain(|retry| retry.task_id != task_id);
        state.stats.total_tasks_failed += 1;
        state.stats.total_runs += 1;
        self.save_remote_state(state).await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn collect_remote_events(
        &self,
        mut events: BoxStream<'static, RemoteRunEvent>,
        run_id: &str,
        task_id: &str,
        agent_id: &str,
        auto_approve: bool,
        cancel: CancellationToken,
    ) -> Result<()> {
        let mut collected = super::collect::Collected::default();
        loop {
            let item = tokio::select! {
                _ = cancel.cancelled() => break,
                item = events.next() => item,
            };
            match item {
                Some(RemoteRunEvent::Event(event)) => {
                    if let Err(err) = self
                        .record_event(&mut collected, event, run_id, task_id, agent_id)
                        .await
                    {
                        self.emit_error(err, format!("record remote progress {run_id}"));
                    }
                }
                Some(RemoteRunEvent::Finished(outcome)) => {
                    loop {
                        match self
                            .handle_remote_outcome(task_id, run_id, outcome.clone(), auto_approve)
                            .await
                        {
                            Ok(()) => break,
                            Err(err) => {
                                self.emit_error(err, format!("persist remote outcome {run_id}"))
                            }
                        }
                        tokio::select! {
                            _ = cancel.cancelled() => return Ok(()),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
                        }
                    }
                    if let Some((_, alias)) = agent_id.rsplit_once('@')
                        && let Err(err) = self.remote_bridge().ack_finished(alias, run_id).await
                    {
                        self.emit_error(err, format!("ack remote outcome {run_id}"));
                    }
                    break;
                }
                Some(RemoteRunEvent::Unknown) => {
                    // Sync is authoritative, but applying it still requires
                    // durable writes, exactly like a finished envelope.
                    loop {
                        let result = {
                            let mut slot = self.inner.state.lock().await;
                            if let Some(state) = slot.as_mut() {
                                self.persist_remote_unknown_locked(
                                    state,
                                    task_id,
                                    "remote lost execution record",
                                )
                                .await
                            } else {
                                Ok(())
                            }
                        };
                        match result {
                            Ok(()) => break,
                            Err(error) => self.emit_error(
                                error,
                                format!("persist unknown remote outcome {run_id}"),
                            ),
                        }
                        tokio::select! {
                            _ = cancel.cancelled() => return Ok(()),
                            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {},
                        }
                    }
                    self.drop_cancel(task_id).await;
                    break;
                }
                // A dropped stream is not proof the execution stopped. Leave
                // it outcome-pending, as for a submit whose ack was lost.
                None => break,
            }
        }
        self.deps().run_store.close_run_events(run_id);
        Ok(())
    }

    /// Apply a terminal envelope under the same mutex as cancellation and
    /// reconcile. The entry check makes duplicate envelopes harmless.
    pub(super) async fn handle_remote_outcome(
        &self,
        task_id: &str,
        run_id: &str,
        outcome: RemoteOutcome,
        auto_approve: bool,
    ) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        let Some(state) = slot.as_mut() else {
            return Ok(());
        };
        let Some(entry) = state
            .running
            .get(task_id)
            .filter(|entry| entry.run_id == run_id)
            .cloned()
        else {
            // A prior attempt may have removed the entry in memory and then
            // failed its state write. Ack only after that removal is durable.
            return self.save_remote_state(state).await;
        };
        let Some(remote) = entry.remote.as_ref() else {
            return Ok(());
        };
        let Some(mut task) = self.deps().task_store.get(task_id).await? else {
            self.cancel_remote(remote).await;
            self.deps()
                .run_service
                .finish(
                    run_id,
                    RunStatus::Cancelled,
                    None,
                    Some("Task no longer exists".into()),
                )
                .await?;
            state.running.shift_remove(task_id);
            return self.save_remote_state(state).await;
        };
        // External cancellation may have reached the store before reconcile.
        if orch_domain::is_terminal(task.status)
            && !(task.status == TaskStatus::Failed
                && matches!(outcome, RemoteOutcome::Failed { .. }))
        {
            self.cancel_remote(remote).await;
            self.deps()
                .run_service
                .finish(
                    run_id,
                    RunStatus::Cancelled,
                    None,
                    Some("Task already terminal".into()),
                )
                .await?;
            state.running.shift_remove(task_id);
            return self.save_remote_state(state).await;
        }
        match outcome {
            RemoteOutcome::Failed { error, error_kind } => {
                return self
                    .handle_run_failure_locked(state, task_id, &entry, &error, error_kind)
                    .await;
            }
            RemoteOutcome::Cancelled { reason } => {
                self.deps()
                    .run_service
                    .finish(run_id, RunStatus::Cancelled, None, Some(reason))
                    .await?;
                let previous_status = task.status;
                task.status = TaskStatus::Cancelled;
                task.updated_at = self.deps().clock.now_iso();
                self.deps().task_store.save(&task).await?;
                self.emit(OrchestratorEvent::TaskStatusChanged {
                    task_id: task_id.to_owned(),
                    from: previous_status,
                    to: TaskStatus::Cancelled,
                });
            }
            RemoteOutcome::Succeeded {
                summary,
                files_changed,
                branch,
                review_results,
                tokens,
            } => {
                task.proof = Some(TaskProof {
                    agent_summary: Some(
                        summary.chars().take(super::MAX_AGENT_SUMMARY_LEN).collect(),
                    ),
                    files_changed,
                    branch: branch.map(|branch| format!("{}:{branch}", remote.alias)),
                    remote: Some(remote.alias.clone()),
                    ..task.proof.unwrap_or_default()
                });
                task.review_results = review_results;
                task.feedback = None;
                let criteria_passed = task
                    .review_criteria
                    .as_ref()
                    .is_some_and(|criteria| !criteria.is_empty())
                    && task.review_results.as_ref().is_some_and(|results| {
                        task.review_criteria
                            .as_ref()
                            .unwrap()
                            .iter()
                            .all(|criterion| {
                                results
                                    .iter()
                                    .any(|r| r.criterion == *criterion && r.passed)
                            })
                            && results.iter().all(|r| r.passed)
                    });
                let previous_status = task.status;
                task.status = TaskStatus::Review;
                task.updated_at = self.deps().clock.now_iso();
                self.deps()
                    .run_service
                    .finish(run_id, RunStatus::Succeeded, tokens, None)
                    .await?;
                self.deps().task_store.save(&task).await?;
                self.emit(OrchestratorEvent::TaskStatusChanged {
                    task_id: task_id.to_owned(),
                    from: previous_status,
                    to: TaskStatus::Review,
                });
                if auto_approve || criteria_passed {
                    self.deps()
                        .task_service
                        .update_status(task_id, TaskStatus::Done)
                        .await?;
                }
                state.stats.total_tasks_completed += 1;
                state.stats.total_runs += 1;
                state.stats.total_runtime_ms += self.runtime_ms(&entry).unwrap_or(0);
                if let Some(tokens) = tokens {
                    let total = &mut state.stats.total_tokens;
                    total.input += tokens.input;
                    total.output += tokens.output;
                    total.reasoning += tokens.reasoning;
                    total.cache_read += tokens.cache_read;
                    total.cache_write += tokens.cache_write;
                    total.total = total.input + total.output + total.reasoning;
                }
            }
            RemoteOutcome::ReviewFailed {
                summary,
                review_results,
            } => {
                task.review_results = Some(review_results);
                task.proof = Some(TaskProof {
                    remote: Some(remote.alias.clone()),
                    agent_summary: Some(summary),
                    ..task.proof.unwrap_or_default()
                });
                self.deps()
                    .run_service
                    .finish(run_id, RunStatus::Succeeded, None, None)
                    .await?;
                self.force_task_to_review(
                    state,
                    task,
                    &entry.agent_id,
                    &format!("REMOTE REVIEW FAILED on {}", remote.alias),
                )
                .await?;
            }
            RemoteOutcome::IntegrationFailed { summary, info } => {
                task.proof = Some(TaskProof {
                    remote: Some(remote.alias.clone()),
                    agent_summary: Some(summary),
                    ..task.proof.unwrap_or_default()
                });
                self.deps()
                    .run_service
                    .finish(run_id, RunStatus::Succeeded, None, None)
                    .await?;
                self.force_task_to_review(
                    state,
                    task,
                    &entry.agent_id,
                    &format!("REMOTE MERGE CONFLICT on {}: {info}", remote.alias),
                )
                .await?;
            }
        }
        self.drop_cancel(task_id).await;
        state.running.shift_remove(task_id);
        state.retry_queue.retain(|retry| retry.task_id != task_id);
        self.save_remote_state(state).await?;
        self.schedule_dispatch_after_run(task_id).await;
        Ok(())
    }
}
