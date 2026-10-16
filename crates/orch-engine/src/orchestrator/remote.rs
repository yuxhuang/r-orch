//! Controller lifecycle without process handles or local workspace effects.

use futures::{StreamExt, stream::BoxStream};
use orch_core::remote::RemoteRunEvent;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use orch_core::error::Result;
use orch_core::remote::{RemoteBridge, RemoteOutcome, RemotePhaseUpdate, RunEventTap};
use orch_domain::{
    OrchestratorEvent, OrchestratorState, RemotePhase, RemoteRunRef, RunStatus, TaskProof,
    TaskStatus,
};

use super::{Orchestrator, REMOTE_QUEUED_WARN_MS, REMOTE_SUBMIT_DEADLINE_MS};

/// How long one remote entry has been in its current phase, for the two
/// deadlines. `dispatched` counts only the time its peer was online; nothing
/// else needs a wall clock that stops.
#[derive(Debug, Default)]
pub(super) struct RemoteClock {
    phase: RemotePhase,
    /// Milliseconds spent in `phase`. Online-only while `dispatched`.
    elapsed_ms: i64,
    /// When this entry was last sampled, so a tick adds its own interval.
    sampled_at_ms: i64,
    /// When the queued warning was last emitted, to keep it hourly.
    warned_at_ms: Option<i64>,
}

impl RemoteClock {
    /// Fold one reconcile sample in and return the time spent in `phase`.
    fn sample(&mut self, phase: RemotePhase, now_ms: i64, counts: bool) -> i64 {
        if phase != self.phase {
            *self = RemoteClock {
                phase,
                sampled_at_ms: now_ms,
                ..RemoteClock::default()
            };
            return 0;
        }
        let delta = now_ms.saturating_sub(self.sampled_at_ms).max(0);
        self.sampled_at_ms = now_ms;
        if counts {
            self.elapsed_ms = self.elapsed_ms.saturating_add(delta);
        }
        self.elapsed_ms
    }

    fn warn_due(&mut self, now_ms: i64) -> bool {
        let due = self
            .warned_at_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= REMOTE_QUEUED_WARN_MS);
        if due {
            self.warned_at_ms = Some(now_ms);
        }
        due
    }
}

impl Orchestrator {
    /// The two phase deadlines, from inside reconcile's remote branch.
    ///
    /// A submit the worker never acknowledges is a real failure: nothing ran,
    /// so the task can fail and be retried. A mirror that stays queued is
    /// not — it is usually waiting for its worker owner's approval, which the
    /// controller cannot grant and must not cancel work over. So that one
    /// only ever warns, once an hour, naming the command that would clear it.
    ///
    /// Returns true when the entry was failed and is gone from `state`.
    pub(super) async fn enforce_remote_deadlines(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        online: bool,
    ) -> Result<bool> {
        let Some(remote) = state
            .running
            .get(task_id)
            .and_then(|entry| entry.remote.clone())
        else {
            return Ok(false);
        };
        let now = self.deps().clock.now_ms();
        let (elapsed, warn_due) = {
            let mut clocks = self.inner.remote_clocks.lock().await;
            let clock = clocks.entry(task_id.to_owned()).or_default();
            // Only `dispatched` stops counting while the peer is away: an
            // offline worker has not failed to answer.
            let counts = online || remote.phase != RemotePhase::Dispatched;
            let elapsed = clock.sample(remote.phase, now, counts);
            let warn_due = remote.phase == RemotePhase::Queued
                && elapsed >= REMOTE_QUEUED_WARN_MS
                && clock.warn_due(now);
            (elapsed, warn_due)
        };
        if remote.phase == RemotePhase::Dispatched && elapsed >= REMOTE_SUBMIT_DEADLINE_MS {
            let error = format!("remote {} never acknowledged submit", remote.alias);
            tracing::warn!(%task_id, alias = %remote.alias, online_ms = elapsed, "{error}");
            // Cancel first: the worker may hold a mirror whose `Submitted`
            // never reached us, and it must not run after this task fails.
            self.cancel_remote(&remote).await;
            self.fail_remote_unknown_locked(state, task_id, &error)
                .await?;
            return Ok(true);
        }
        if warn_due {
            let hours = elapsed / 3_600_000;
            self.emit(OrchestratorEvent::OrchestratorError {
                error: format!("{task_id} has been queued on {} for {hours}h", remote.alias),
                context: remote.hint().unwrap_or_else(|| {
                    format!(
                        "The mirror is waiting on {}; run `orch task list` there to see why",
                        remote.alias
                    )
                }),
                fatal: false,
            });
        }
        Ok(false)
    }

    /// Forget the phase clocks of entries reconcile no longer holds, so a
    /// long-lived owner does not accumulate one per completed remote run.
    pub(super) async fn prune_remote_clocks(&self, state: &OrchestratorState) {
        self.inner
            .remote_clocks
            .lock()
            .await
            .retain(|task_id, _| state.running.contains_key(task_id));
    }
}

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

    /// Record what the worker says about an execution that has not finished,
    /// and start its run record the moment the worker says it is running.
    ///
    /// Only the phase moves. The task stays `in_progress` and the run stays
    /// `preparing` until then, so no view claims a peer is running work it
    /// has merely been sent. Returns the phase the entry ended up in.
    pub(super) async fn apply_remote_phase(
        &self,
        task_id: &str,
        run_id: &str,
        update: &RemotePhaseUpdate,
    ) -> Result<RemotePhase> {
        let mut slot = self.inner.state.lock().await;
        let Some(state) = slot.as_mut() else {
            return Ok(update.phase);
        };
        let Some(remote) = state
            .running
            .get_mut(task_id)
            .filter(|entry| entry.run_id == run_id)
            .and_then(|entry| entry.remote.as_mut())
        else {
            // The entry has been retired; a phase never resurrects one.
            return Ok(RemotePhase::Terminal);
        };
        let before = remote.phase;
        if update.worker_task_id.is_some() {
            remote.worker_task_id = update.worker_task_id.clone();
        }
        if before == update.phase || before.advances_to(update.phase) {
            remote.phase = update.phase;
            remote.reason = update.reason.clone();
        }
        let phase = remote.phase;
        if phase == RemotePhase::Running && before != RemotePhase::Running {
            // The run may already be `running` when a restart rehydrated this
            // entry; starting it twice would emit a second `agent:started`.
            if self
                .deps()
                .run_store
                .get(run_id)
                .await?
                .is_some_and(|run| run.status == RunStatus::Preparing)
            {
                self.deps().run_service.start(run_id, 0).await?;
            }
        }
        if phase != before {
            tracing::debug!(%task_id, %run_id, from = before.as_str(), to = phase.as_str(), "Remote execution phase");
        }
        self.save_remote_state(state).await?;
        Ok(phase)
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
        // Phase bookkeeping, so a long run does not take the state lock once
        // per progress event just to re-assert what it already said.
        let mut running = false;
        loop {
            let item = tokio::select! {
                _ = cancel.cancelled() => break,
                item = events.next() => item,
            };
            match item {
                Some(RemoteRunEvent::Phase(update)) => {
                    match self.apply_remote_phase(task_id, run_id, &update).await {
                        Ok(phase) => {
                            running = matches!(phase, RemotePhase::Running | RemotePhase::Terminal)
                        }
                        Err(err) => self.emit_error(err, format!("record remote phase {run_id}")),
                    }
                }
                Some(RemoteRunEvent::Event(event)) => {
                    // An agent emitting progress is running, whatever frame
                    // the worker's one-second tick has got round to sending.
                    if !running {
                        match self
                            .apply_remote_phase(
                                task_id,
                                run_id,
                                &RemotePhaseUpdate::new(RemotePhase::Running),
                            )
                            .await
                        {
                            Ok(phase) => {
                                running =
                                    matches!(phase, RemotePhase::Running | RemotePhase::Terminal)
                            }
                            Err(err) => {
                                self.emit_error(err, format!("record remote phase {run_id}"))
                            }
                        }
                    }
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
                    // The worker has no execution record for this key, but it
                    // may well still hold the mirror — an unreadable one is
                    // reported unknown and then becomes readable again. This
                    // task is about to fail here, so the mirror must go, and
                    // only an explicit cancel clears one inside a session.
                    if let Some(remote) = {
                        let slot = self.inner.state.lock().await;
                        slot.as_ref()
                            .and_then(|state| state.running.get(task_id))
                            .filter(|entry| entry.run_id == run_id)
                            .and_then(|entry| entry.remote.clone())
                    } {
                        self.cancel_remote(&remote).await;
                    }
                    // Sync is authoritative, but applying it still requires
                    // durable writes, exactly like a finished envelope.
                    loop {
                        let result = {
                            let mut slot = self.inner.state.lock().await;
                            if let Some(state) = slot.as_mut() {
                                let error = remote_unknown_error(
                                    state
                                        .running
                                        .get(task_id)
                                        .and_then(|entry| entry.remote.as_ref()),
                                );
                                self.persist_remote_unknown_locked(state, task_id, &error)
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

/// Why a run with no outcome failed, including the worker's own explanation
/// when it gave one. Without it a corrupt mirror on the worker reaches the
/// operator as the bare "remote lost execution record", which says nothing
/// about where to look or what to fix.
fn remote_unknown_error(remote: Option<&RemoteRunRef>) -> String {
    let base = "remote lost execution record";
    match remote.and_then(|remote| remote.reason.as_deref().map(|r| (&remote.alias, r))) {
        Some((alias, reason)) => format!("{base} ({alias}: {reason})"),
        None => base.to_owned(),
    }
}
