//! Daemon-owned phased dispatch: no project permit crosses external preparation.

mod completion;
mod shutdown;
mod startup;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orch_core::adapter::ExecuteParams;
use orch_core::error::{Error, Result};
use orch_domain::{
    Agent, AgentStatus, GoalStatus, OrchestratorEvent, OrchestratorState, RunStatus, RunningEntry,
    Task, TaskStatus, is_blocked, is_dispatchable, scopes_overlap,
};

use super::dispatch::{
    MailboxMode, PREPARATION_OWNS_EFFORT_DIAGNOSTICS, PreparedPrompt, check_preparation,
};
use super::{CANCEL_KILL_GRACE, Orchestrator};

/// Only preparation failures may be cancelled by an overlapping stop. Final
/// execute errors always retain their existing failure accounting.
enum PreparationFailureOutcome {
    Failed,
    Cancelled,
}

const SCOPE_OVERLAP_REPEAT_MS: i64 = 300_000;

fn scope_overlap_repeat_due(now_ms: i64, last_emitted_ms: i64) -> bool {
    now_ms.saturating_sub(last_emitted_ms) >= SCOPE_OVERLAP_REPEAT_MS
}

impl Orchestrator {
    fn daemon_preparation_stopping(&self) -> bool {
        self.is_shutting_down() || self.spawn_admission().is_closed()
    }

    async fn daemon_launch_cancelled(
        &self,
        token: &tokio_util::sync::CancellationToken,
        guard: &AtomicBool,
    ) {
        tokio::select! {
            () = token.cancelled() => (),
            () = async {
                while !guard.load(Ordering::SeqCst) && !self.is_shutting_down() {
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            } => (),
        }
    }

    pub fn enable_daemon_mode(&self) {
        self.inner.daemon_mode.store(true, Ordering::SeqCst);
    }

    pub fn set_daemon_batch_active(&self, active: bool) {
        self.inner
            .daemon_batch_active
            .store(active, Ordering::SeqCst);
    }

    pub fn is_daemon_mode(&self) -> bool {
        self.inner.daemon_mode.load(Ordering::SeqCst)
    }

    pub async fn complete_onboarding(&self) -> Result<()> {
        self.deps()
            .events
            .operation_boundary()
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                state.onboarding_completed = Some(true);
                self.save_state(state).await
            })
            .await
            .value
    }

    /// Owner maintenance independent of scheduling. The operation boundary
    /// serializes this with cancellation, startup and collector completion.
    pub async fn daemon_reconcile_dead_local_runs(&self) -> Result<()> {
        self.deps()
            .events
            .operation_boundary()
            .run(false, async {
                if self.is_shutting_down() {
                    return Ok(());
                }
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                self.reconcile_dead_local_runs(slot.as_mut().expect("state loaded"))
                    .await
            })
            .await
            .value
    }

    /// Reconcile and select under the boundary, then prepare each candidate
    /// outside it. The caller owns persistent dispatch/batch policy.
    pub async fn daemon_tick(&self, skip_autonomous_seeding: bool) -> Result<()> {
        self.daemon_tick_with_guard(skip_autonomous_seeding, Arc::new(AtomicBool::new(false)))
            .await
    }

    pub async fn daemon_tick_with_guard(
        &self,
        skip_autonomous_seeding: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<()> {
        self.deps()
            .events
            .operation_boundary()
            .with_source(
                crate::container::commit::CommitSource::Tick,
                Box::pin(self.daemon_tick_with_guard_inner(skip_autonomous_seeding, cancelled)),
            )
            .await
    }

    async fn daemon_tick_with_guard_inner(
        &self,
        skip_autonomous_seeding: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<()> {
        let candidates =
            self.deps()
                .events
                .operation_boundary()
                .run(false, async {
                    if cancelled.load(Ordering::SeqCst) || self.is_shutting_down() {
                        return Ok(Vec::new());
                    }
                    let mut slot = self.inner.state.lock().await;
                    self.load_state(&mut slot).await?;
                    let state = slot.as_mut().expect("state loaded");
                    self.invalidate_caches().await;
                    self.reconcile(state).await?;
                    self.inner.terminal_intents.lock().await.retain(|run_id| {
                        state.running.values().any(|entry| &entry.run_id == run_id)
                    });
                    if let Err(error) = self.expire_context().await {
                        self.emit_error(error, "context expiry");
                    }
                    if !skip_autonomous_seeding
                        && !self.inner.daemon_batch_active.load(Ordering::SeqCst)
                    {
                        self.seed_autonomous_tasks().await;
                    }
                    let mut tasks = self.deps().task_store.list(&Default::default()).await?;
                    self.daemon_prune_scope_overlap_reports(&tasks, state);
                    tasks.sort_by(|a, b| {
                        a.priority
                            .cmp(&b.priority)
                            .then_with(|| a.goal_id.is_none().cmp(&b.goal_id.is_none()))
                            .then_with(|| b.updated_at.cmp(&a.updated_at))
                    });
                    Ok::<_, Error>(
                        tasks
                            .into_iter()
                            .filter(|task| is_dispatchable(task.status))
                            .map(|task| task.id)
                            .collect::<Vec<_>>(),
                    )
                })
                .await
                .value?;
        let kills = std::mem::take(&mut *self.inner.deferred_kills.lock().await);
        for (task_id, entry, error) in kills {
            self.deps()
                .processes
                .kill_with_grace(entry.pid, super::STALL_KILL_GRACE)
                .await;
            self.handle_run_failure(&task_id, &entry, &error, None)
                .await?;
        }
        self.daemon_drain_cleanups().await;
        for task_id in candidates {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            if let Err(error) = self
                .daemon_launch_task_with_guard(&task_id, Arc::clone(&cancelled), false)
                .await
            {
                self.emit_error(error, format!("dispatch task {task_id}"));
            }
        }
        self.deps()
            .events
            .operation_boundary()
            .run(false, async {
                let slot = self.inner.state.lock().await;
                let running = slot.as_ref().map_or(0, |state| state.running.len());
                let queued = self
                    .deps()
                    .task_store
                    .list(&Default::default())
                    .await?
                    .iter()
                    .filter(|task| is_dispatchable(task.status))
                    .count();
                self.emit(OrchestratorEvent::OrchestratorTick { running, queued });
                Ok::<_, Error>(())
            })
            .await
            .value
    }

    pub(super) async fn daemon_cancel_task(&self, task_id: &str, strict: bool) -> Result<()> {
        let boundary = self.deps().events.operation_boundary();
        let (entry, preparing_runs) = boundary.run(false, async {
            let mut slot = self.inner.state.lock().await;
            self.load_state(&mut slot).await?;
            let state = slot.as_mut().expect("state loaded");
            let current = self.deps().task_service.get(task_id).await?;
            if strict && orch_domain::is_terminal(current.status) {
                return Err(Error::Domain(orch_domain::DomainError::InvalidTransition {
                    task_id: task_id.to_owned(), from: current.status, to: TaskStatus::Cancelled,
                }));
            }
            let entry = state.running.get(task_id).cloned();
            if let Some(entry) = &entry && self.worker_signal_unverified(entry).await {
                return Err(Error::other("Worker execution ownership cannot verify; not signalled; reservation retained"));
            }
            let cancelled = self.deps().task_service.cancel_record(task_id).await;
            if strict { cancelled?; }
            self.take_cancel(task_id).await;
            if !self.inner.preparing_agents.lock().await.contains_key(task_id) {
                state.claimed.shift_remove(task_id);
            }
            state.retry_queue.retain(|retry| retry.task_id != task_id);
            let preparing_runs = self.deps().run_store.list_for_task(task_id).await?.into_iter()
                .filter(|run| run.status == RunStatus::Preparing).map(|run| run.id).collect::<Vec<_>>();
            self.save_state(state).await?;
            Ok::<_, Error>((entry, preparing_runs))
        }).await.value?;
        if let Some(entry) = &entry {
            if let Some(remote) = &entry.remote {
                self.remote_bridge()
                    .cancel(&remote.alias, &remote.exec_key)
                    .await
                    .map_err(|error| {
                        Error::other(format!("remote cancellation outcome unknown: {error}"))
                    })?;
            } else {
                self.deps()
                    .processes
                    .kill_with_grace(entry.pid, CANCEL_KILL_GRACE)
                    .await;
                if self.deps().processes.is_alive(entry.pid) {
                    return Err(Error::other(
                        "Task cancellation timed out; local child is still alive",
                    ));
                }
            }
        }
        boundary
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                for run_id in preparing_runs {
                    self.deps()
                        .run_service
                        .finish(&run_id, RunStatus::Cancelled, None, None)
                        .await?;
                }
                if let Some(entry) = &entry {
                    self.deps()
                        .run_service
                        .finish(&entry.run_id, RunStatus::Cancelled, None, None)
                        .await?;
                    if state
                        .running
                        .get(task_id)
                        .is_some_and(|current| current.run_id == entry.run_id)
                    {
                        state.running.shift_remove(task_id);
                        if entry.remote.is_none() {
                            self.deps()
                                .agent_service
                                .set_status(&entry.agent_id, AgentStatus::Idle)
                                .await?;
                            if let Some(mut agent) =
                                self.deps().agent_store.get(&entry.agent_id).await?
                            {
                                agent.current_task = None;
                                self.deps().agent_store.save(&agent).await?;
                            }
                        }
                    }
                }
                self.save_state(state).await
            })
            .await
            .value
    }

    /// A deliberate dispatch request, independent of the persistent switch.
    pub async fn daemon_launch_task(&self, task_id: &str) -> Result<()> {
        self.daemon_launch_task_with_guard(task_id, Arc::new(AtomicBool::new(false)), true)
            .await
    }

    /// `override_retry_wait` is the explicit `orch run <id>`: like v0.13 it
    /// starts a `retrying` task before its back-off is due (the launch
    /// consumes the queued retry). Batch launches leave the wait to the tick.
    pub async fn daemon_launch_task_with_guard(
        &self,
        task_id: &str,
        cancelled: Arc<AtomicBool>,
        override_retry_wait: bool,
    ) -> Result<()> {
        let boundary = self.deps().events.operation_boundary();
        // Selection can reject every candidate without touching a store.
        // Tracked writers declare changes before I/O, including failed writes.
        let selected = boundary
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                if cancelled.load(Ordering::SeqCst) || self.is_shutting_down() {
                    return Ok(None);
                }
                if state.claimed.contains(task_id) || state.running.contains_key(task_id) {
                    self.daemon_forget_scope_overlap(task_id);
                    return Ok(None);
                }
                if self.inner.active_collectors.lock().await.contains(task_id) {
                    return Ok(None);
                }
                let task = self.deps().task_service.get(task_id).await?;
                if !self
                    .daemon_task_eligible(&task, state, override_retry_wait)
                    .await?
                {
                    return Ok(None);
                }
                // Its last attempt already failed before spawn; only the
                // record is missing. Finish that instead of probing again.
                if super::dispatch::final_failure_unrecorded(&task) {
                    if self.finish_unrecorded_final_failure(state, task_id).await {
                        self.inner.remote_state_dirty.store(true, Ordering::SeqCst);
                        self.save_state(state).await?;
                    }
                    return Ok(None);
                }
                let agents = self.dispatch_agent_directory().await?;
                let preparing = self.inner.preparing_agents.lock().await;
                let at_capacity = state
                    .running
                    .values()
                    .filter(|entry| entry.remote.is_none())
                    .count()
                    + preparing
                        .values()
                        .filter(|(_, id)| !id.contains('@'))
                        .count()
                    >= self.config().scheduling.max_concurrent_agents;
                let eligible = agents
                    .iter()
                    .filter(|agent| {
                        (!at_capacity || agent.remote.is_some())
                            && !state
                                .running
                                .values()
                                .any(|entry| entry.agent_id == agent.id)
                            && !preparing.values().any(|(_, id)| id == &agent.id)
                    })
                    .cloned()
                    .collect();
                let Some(agent) = crate::agent_service::AgentService::find_best_in(&task, eligible)
                else {
                    return Ok(None);
                };
                // The directory comes from the agent index; the pre-spawn
                // re-validation reads the agent file. Launch from the file, so
                // an index entry older than its file (hand edit, or a crash
                // between the two writes) cannot invalidate every preparation.
                let agent = if agent.remote.is_none() {
                    match self.deps().agent_store.get(&agent.id).await? {
                        Some(current) if current.status == AgentStatus::Idle => current,
                        _ => return Ok(None),
                    }
                } else {
                    agent
                };
                drop(preparing);
                self.daemon_forget_scope_overlap(task_id);
                state.claimed.insert(task_id.to_owned());
                // Only the final launch phase consumes the due retry. A stop
                // during preparation must leave its back-off intact.
                let generation = self.inner.preparation_seq.fetch_add(1, Ordering::SeqCst) + 1;
                self.inner
                    .preparing_agents
                    .lock()
                    .await
                    .insert(task_id.to_owned(), (generation, agent.id.clone()));
                let launch_config = self.config();
                let cancel = self.register_cancel(task_id).await;
                if let Err(error) = self.save_state(state).await {
                    state.claimed.shift_remove(task_id);
                    // No launch began. Roll back before a collector's debounced
                    // save can persist the claim, then release this reservation.
                    let mut preparing = self.inner.preparing_agents.lock().await;
                    if preparing
                        .get(task_id)
                        .is_some_and(|(current, _)| *current == generation)
                    {
                        preparing.remove(task_id);
                        drop(preparing);
                        self.drop_cancel(task_id).await;
                    }
                    return Err(error);
                }
                Ok::<_, Error>(Some((
                    task,
                    agent,
                    agents,
                    cancel,
                    generation,
                    launch_config,
                )))
            })
            .await
            .value?;
        let Some((task, agent, agents, cancel, generation, launch_config)) = selected else {
            return Ok(());
        };

        let result = async {
            let prepared = if let Some(remote) = &agent.remote {
                Ok(orch_core::workspace::PrepareResult {
                    path: format!("remote:{}", remote.alias),
                    branch: None,
                })
            } else {
                tokio::select! {
                    biased;
                    () = self.daemon_launch_cancelled(&cancel, &cancelled) => {
                        self.daemon_release_preparation(task_id, generation).await?;
                        return Ok(());
                    }
                    prepared = self.deps().workspace.prepare(&task, &agent, &launch_config) => prepared,
                }
            };
            let workspace = match prepared {
                Ok(workspace) => workspace,
                Err(error) => {
                    // As in v0.13: a workspace that cannot be prepared costs an
                    // attempt and backs off. Released with no record, the task
                    // stays dispatchable and every tick repeats the failure.
                    // That holds for every preparation error, not only the
                    // workspace domain errors: a workspaces directory that
                    // cannot be created is an I/O error and a failed isolated
                    // copy a plain one. None of them means the preparation was
                    // invalidated; the recorder checks that itself (generation,
                    // and a terminal task under the dispatch gate).
                    if let PreparationFailureOutcome::Cancelled = self.daemon_record_workspace_failure(task_id, generation, &error)
                        .await? {
                        self.daemon_release_preparation(task_id, generation).await?;
                        return Ok(());
                    }
                    return Err(error);
                }
            };

            let prompt = boundary
                .run(false, async {
                    if cancelled.load(Ordering::SeqCst)
                        || self.config() != launch_config
                        || cancel.is_cancelled()
                        || !self
                            .daemon_preparation_valid(&task, &agent, generation)
                            .await?
                    {
                        return Ok(None);
                    }
                    self.prepare_prompt(
                        &task,
                        &agent,
                        &agents,
                        &workspace.path,
                        MailboxMode::Snapshot,
                    )
                    .await
                    .map(Some)
                })
                .await
                .value;
            let PreparedPrompt {
                prompt,
                system_prompt,
                captured_mode,
                message_ids,
            } = match prompt {
                Ok(Some(prompt)) => prompt,
                Ok(None) => {
                    self.daemon_release_preparation(task_id, generation).await?;
                    return Ok(());
                }
                Err(error) => {
                    self.daemon_release_preparation(task_id, generation).await?;
                    return Err(error);
                }
            };
            let run = boundary
                .run(false, async {
                    if cancelled.load(Ordering::SeqCst)
                        || self.config() != launch_config
                        || cancel.is_cancelled()
                        || !self
                            .daemon_preparation_valid(&task, &agent, generation)
                            .await?
                    {
                        return Ok(None);
                    }
                    self.deps()
                        .run_service
                        .create(crate::run_service::CreateRunInput {
                            task_id: task.id.clone(),
                            agent_id: agent.id.clone(),
                            attempt: task.attempts + 1,
                            prompt: prompt.clone(),
                            workspace_path: workspace.path.clone(),
                        })
                        .await
                        .map(Some)
                })
                .await
                .value?;
            let Some(run) = run else {
                self.daemon_release_preparation(task_id, generation).await?;
                return Ok(());
            };

            let adapter = if agent.remote.is_none() {
                Some(self.deps().adapters.require(&agent.adapter)?.clone())
            } else {
                None
            };
            let mut env: BTreeMap<String, String> = agent
                .config
                .env
                .clone()
                .unwrap_or_default()
                .into_iter()
                .collect();
            env.insert("ORCH_AGENT_ID".into(), agent.id.clone());
            env.insert("ORCH_AGENT_NAME".into(), agent.name.clone());
            env.insert("ORCH_TASK_ID".into(), task.id.clone());
            let mut invocation_config = agent.config.clone();
            if let Some(mode) = captured_mode {
                invocation_config.prompt_mode = Some(mode);
            }
            let mut params = ExecuteParams {
                prompt: prompt.clone(),
                system_prompt,
                workspace: workspace.path.clone().into(),
                env,
                inherit_env: if task
                    .labels
                    .iter()
                    .any(|label| label.starts_with("remote-ctl:"))
                {
                    orch_core::process::EnvInheritance::WithoutSecrets
                } else {
                    orch_core::process::EnvInheritance::Full
                },
                config: invocation_config,
                cancel: cancel.clone(),
            };
            // Network/probe preparation cannot block task edits, goal pause, cancel,
            // expiry, or agent-run client requests.
            let preparation = if agent.remote.is_none() {
                let prepare = async { adapter
                        .as_ref()
                        .expect("local adapter resolved")
                        .prepare_effort(&params)
                        .await };
                let prepared = tokio::select! {
                    biased;
                    () = self.daemon_launch_cancelled(&cancel, &cancelled) => {
                        self.daemon_release_preparation(task_id, generation).await?;
                        return Ok(());
                    }
                    prepared = prepare => prepared,
                };
                Some(prepared.and_then(|prepared| check_preparation(&params.config, prepared)))
            } else { None };
            if let (Some(preparation), Some(adapter)) = (preparation, adapter.as_ref()) {
                let preparation = match preparation {
                    Ok(preparation) => preparation,
                    Err(mut error) => {
                        error.message = format!("effort preparation failed: {error}");
                        if let PreparationFailureOutcome::Cancelled = self.daemon_fail_preparation(
                            &task, &agent, generation, &run.id, &error, &workspace,
                        )
                        .await? {
                            self.daemon_release_preparation(task_id, generation).await?;
                            return Ok(());
                        }
                        return Err(error.into());
                    }
                };
                params.config = preparation.config;
                let mut diagnostics = preparation.diagnostics;
                if !PREPARATION_OWNS_EFFORT_DIAGNOSTICS.contains(&adapter.kind()) {
                    match self.dispatch_effort_diagnostics(adapter.kind(), &params.config) {
                        Ok(planned) => diagnostics.extend(planned),
                        Err(error) => {
                            if let PreparationFailureOutcome::Cancelled = self.daemon_fail_preparation(
                                &task, &agent, generation, &run.id, &error, &workspace,
                            )
                            .await? {
                                self.daemon_release_preparation(task_id, generation).await?;
                                return Ok(());
                            }
                            return Err(error.into());
                        }
                    }
                }
                self.report_effort_diagnostics(&run.id, &agent.id, &diagnostics);
            }

            // Admission precedes the project boundary and state mutex. A queued
            // launch owns no blocking worker and cannot outlive cancellation.
            let admission = self.spawn_admission();
            let mut reservation = if agent.remote.is_none() {
                tokio::select! {
                    biased;
                    () = self.daemon_launch_cancelled(&cancel, &cancelled) => {
                        self.daemon_release_preparation(task_id, generation).await?;
                        return Ok(());
                    }
                    permit = admission.reserve() => Some(permit),
                }
            } else { None };

            let launched = boundary
                .run(false, async {
                    if self.is_shutting_down() {
                        return Ok(false);
                    }
                    let mut slot = self.inner.state.lock().await;
                    self.load_state(&mut slot).await?;
                    let state = slot.as_mut().expect("state loaded");
                    let admitted = reservation.as_mut().is_none_or(|permit| permit.try_admit());
                    if !admitted
                        || cancelled.load(Ordering::SeqCst)
                        || self.config() != launch_config
                        || cancel.is_cancelled()
                        || !self
                            .daemon_preparation_valid(&task, &agent, generation)
                            .await?
                        || self.is_shutting_down()
                    {
                        self.deps()
                            .run_service
                            .finish(&run.id, RunStatus::Cancelled, None, None)
                            .await?;
                        return Ok(false);
                    }
                    self.deps()
                        .task_service
                        .update_status(task_id, TaskStatus::InProgress)
                        .await?;
                    self.deps().task_service.assign(task_id, &agent.id).await?;
                    self.deps().task_service.increment_attempts(task_id).await?;
                    if let Some(mut fresh) = self.deps().task_store.get(task_id).await? {
                        let mut proof = fresh.proof.take().unwrap_or_default();
                        if agent.remote.is_none() {
                            proof.remote = None;
                        }
                        if let Some(branch) = &workspace.branch {
                            proof.branch = Some(branch.clone());
                        }
                        fresh.proof = Some(proof);
                        fresh.workspace = Some(workspace.path.clone());
                        self.deps().task_store.save(&fresh).await?;
                    }
                    state.retry_queue.retain(|retry| retry.task_id != task_id);
                    if let Some(remote) = &agent.remote {
                        let reference = orch_domain::RemoteRunRef::new(&remote.alias, &run.id);
                        let now = self.deps().clock.now_iso();
                        state.running.insert(
                            task_id.to_owned(),
                            RunningEntry {
                                task_id: task_id.to_owned(),
                                agent_id: agent.id.clone(),
                                run_id: run.id.clone(),
                                pid: 0,
                                started_at: now.clone(),
                                last_event_at: now,
                                remote: Some(reference),
                            },
                        );
                        state.claimed.shift_remove(task_id);
                        if let Err(error) = self.save_state(state).await {
                            use orch_core::storage::TaskStore;

                            // Nothing was submitted. Restore the slot while we
                            // still hold its lock, before any debounce can write
                            // it. The preparing owner stays for error-path release.
                            state.running.shift_remove(task_id);
                            state.claimed.insert(task_id.to_owned());
                            // The InProgress save already spent this attempt.
                            // Use the same retry policy as fail_before_spawn,
                            // without treating a remote agent as a local one or
                            // inserting an unsubmitted run for its failure handler.
                            let recorded = async {
                                self.deps().run_service.finish(
                                    &run.id, RunStatus::Failed, None, Some(error.to_string()),
                                ).await?;
                                let fresh = self.deps().task_service.get(task_id).await?;
                                let status = orch_domain::resolve_failure_status(&fresh);
                                self.deps().task_service.update_status(task_id, status).await?;
                                if status == TaskStatus::Retrying {
                                    let delay = orch_domain::calculate_retry_delay(
                                        fresh.attempts.saturating_sub(1),
                                        self.config().scheduling.retry_base_delay_ms,
                                        self.config().scheduling.retry_max_delay_ms,
                                    );
                                    self.enqueue_retry(state, task_id, fresh.attempts + 1, delay, &error.to_string());
                                    self.emit(OrchestratorEvent::RunRetry {
                                        run_id: run.id.clone(), attempt: fresh.attempts + 1, delay_ms: delay,
                                    });
                                } else {
                                    self.inner.cached_tasks.invalidate_async().await;
                                    let tasks = self.inner.cached_tasks.list(&Default::default()).await?;
                                    self.cascade_fail_dependents(task_id, &tasks,
                                        &format!("dependency {task_id} failed: {error}")).await;
                                    state.stats.total_tasks_failed += 1;
                                }
                                state.stats.total_runs += 1;
                                self.emit(OrchestratorEvent::AgentError {
                                    run_id: run.id.clone(), agent_id: agent.id.clone(),
                                    error: error.to_string(), error_kind: None,
                                });
                                self.save_remote_state(state).await
                            }.await;
                            if let Err(record_error) = recorded {
                                self.emit_error(record_error, format!("record remote pre-submit failure for {task_id}"));
                            }
                            return Err(error);
                        }
                        self.inner.preparing_agents.lock().await.remove(task_id);
                        let request = orch_core::remote::RemoteSubmit {
                            alias: remote.alias.clone(),
                            exec_key: run.id.clone(),
                            agent_id: agent
                                .id
                                .rsplit_once('@')
                                .map(|(id, _)| id)
                                .unwrap_or(&agent.id)
                                .to_owned(),
                            from: task
                                .labels
                                .iter()
                                .find_map(|label| label.strip_prefix("remote-from:"))
                                .unwrap_or("cli")
                                .to_owned(),
                            prompt,
                            title: task.title.clone(),
                            priority: task.priority,
                            review_criteria: task.review_criteria.clone().unwrap_or_default(),
                            goal_id: task.goal_id.clone(),
                        };
                        let handle = self.remote_bridge().submit(request);
                        let auto_approve = task.is_autonomous()
                            || agent.config.approval_policy
                                == Some(orch_domain::ApprovalPolicy::Auto);
                        self.spawn_remote_collector(
                            handle,
                            &run.id,
                            task_id,
                            &agent.id,
                            auto_approve,
                            cancel,
                        )
                        .await;
                        return Ok(true);
                    }
                    let execution = {
                        let _scope = reservation.expect("local launch reservation").enter();
                        self.execute_prepared(
                            adapter.as_ref().expect("local adapter resolved").as_ref(), params,
                        )
                    };
                    let handle = match execution {
                        Ok(handle) => handle,
                        Err(error) => {
                            self.fail_before_spawn(
                                state,
                                task_id,
                                &run.id,
                                &agent.id,
                                &error.to_string(),
                                error.kind,
                            )
                            .await;
                            return Err(error.into());
                        }
                    };
                    let child_pid = handle.pid;
                    let now = self.deps().clock.now_iso();
                    state.claimed.shift_remove(task_id);
                    self.inner.preparing_agents.lock().await.remove(task_id);
                    self.inner.remote_state_dirty.store(true, Ordering::SeqCst);
                    state.running.insert(
                        task_id.to_owned(),
                        RunningEntry {
                            task_id: task_id.to_owned(),
                            agent_id: agent.id.clone(),
                            run_id: run.id.clone(),
                            pid: handle.pid,
                            started_at: now.clone(),
                            last_event_at: now,
                            remote: None,
                        },
                    );
                    self.inner
                        .active_collectors
                        .lock()
                        .await
                        .insert(task_id.to_owned());
                    let orchestrator = self.clone();
                    let collector_task_id = task_id.to_owned();
                    let agent_id = agent.id.clone();
                    let run_id = run.id.clone();
                    tokio::spawn(async move {
                        if let Err(error) = orchestrator
                            .collect_events(handle.events, &run_id, &collector_task_id, &agent_id)
                            .await
                        {
                            let message = error.to_string();
                            orchestrator.emit_error(
                                &error,
                                format!("collect events for {collector_task_id}"),
                            );
                            let entry = orchestrator
                                .deps()
                                .events
                                .operation_boundary()
                                .run(false, async {
                                    let slot = orchestrator.inner.state.lock().await;
                                    slot.as_ref()
                                        .and_then(|state| state.running.get(&collector_task_id))
                                        .filter(|entry| entry.run_id == run_id)
                                        .cloned()
                                })
                                .await
                                .value;
                            if let Some(entry) = entry
                                && let Err(error) = orchestrator
                                    .handle_run_failure(&collector_task_id, &entry, &message, None)
                                    .await
                            {
                                orchestrator.emit_error(
                                    error,
                                    format!("finalize failed collector for {collector_task_id}"),
                                );
                            }
                        }
                        orchestrator
                            .inner
                            .active_collectors
                            .lock()
                            .await
                            .remove(&collector_task_id);
                        orchestrator
                            .inner
                            .finishing_collectors
                            .lock()
                            .await
                            .remove(&collector_task_id);
                    });
                    self.deps()
                        .agent_service
                        .set_status(&agent.id, AgentStatus::Running)
                        .await?;
                    let mut current = self.deps().agent_service.get(&agent.id).await?;
                    current.current_task = Some(task_id.to_owned());
                    current.last_error = None;
                    self.deps().agent_store.save(&current).await?;
                    self.deps().run_service.start(&run.id, child_pid).await?;
                    self.save_state(state).await?;
                    if let Some(messages) = &self.deps().message_service {
                        messages
                            .mark_snapshot_delivered(&agent.id, task_id, &message_ids)
                            .await?;
                    }
                    Ok::<_, Error>(true)
                })
                .await
                .value?;
            if !launched {
                self.daemon_release_preparation(task_id, generation).await?;
            }
            Ok(())
        }
        .await;
        if result.is_err()
            && let Err(error) = self.daemon_release_preparation(task_id, generation).await
        {
            self.emit_error(error, format!("release failed preparation {task_id}"));
        }
        result
    }

    async fn daemon_task_eligible(
        &self,
        task: &Task,
        state: &OrchestratorState,
        override_retry_wait: bool,
    ) -> Result<bool> {
        if !is_dispatchable(task.status) {
            self.daemon_forget_scope_overlap(&task.id);
            return Ok(false);
        }
        if task.awaits_approval() {
            return Ok(false);
        }
        if let Some(controller) = super::dispatch::mirror_controller(task)
            && self.inbound_controller().as_deref() != Some(controller)
        {
            return Ok(false);
        }

        if task.status == TaskStatus::Retrying
            && !override_retry_wait
            && super::dispatch::retry_pending(state, &task.id, self.deps().clock.now_ms())
        {
            return Ok(false);
        }
        if let (Some(id), Some(goals)) = (&task.goal_id, &self.deps().goal_store)
            && goals
                .get(id)
                .await?
                .is_some_and(|goal| goal.status == GoalStatus::Paused)
        {
            return Ok(false);
        }
        // Only dependencies and scope reservations need the other tasks. A
        // backlog of independent tasks must not parse the whole index per
        // candidate while holding the boundary.
        let reserved = state
            .running
            .keys()
            .chain(state.claimed.iter())
            .any(|id| id != &task.id);
        let scoped = task.scope.as_deref().is_some_and(|scope| !scope.is_empty());
        if task.depends_on.is_empty() && !(scoped && reserved) {
            self.daemon_forget_scope_overlap(&task.id);
            return Ok(true);
        }
        let tasks = self.deps().task_store.list(&Default::default()).await?;
        self.daemon_prune_scope_overlap_reports(&tasks, state);
        if is_blocked(task, tasks.as_slice()) {
            return Ok(false);
        }
        // This observation has a repeat policy; it is not a scheduling
        // authority. Events are buffered here and published only after this
        // operation releases its permit, outside the project permit.
        let mut reports = self
            .inner
            .scope_overlap_reports
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let overlaps = |running: &&Task| {
            running.id != task.id
                && (state.running.contains_key(&running.id) || state.claimed.contains(&running.id))
                && scopes_overlap(task.scope.as_deref(), running.scope.as_deref())
        };
        let remembered = reports.get(&task.id).and_then(|(id, _)| {
            tasks
                .iter()
                .find(|running| &running.id == id && overlaps(running))
        });
        let blocker =
            remembered.or_else(|| tasks.iter().filter(overlaps).min_by(|a, b| a.id.cmp(&b.id)));
        if let Some(blocker) = blocker {
            let now_ms = self.deps().clock.now_ms();
            if reports.get(&task.id).is_some_and(|(id, last)| {
                id == &blocker.id && !scope_overlap_repeat_due(now_ms, *last)
            }) {
                return Ok(false);
            }
            reports.insert(task.id.clone(), (blocker.id.clone(), now_ms));
            drop(reports);
            self.emit(OrchestratorEvent::TaskScopeOverlap {
                task_id: task.id.clone(),
                overlapping_task_id: blocker.id.clone(),
                patterns: task.scope.clone().unwrap_or_default(),
            });
            return Ok(false);
        }
        reports.remove(&task.id);
        Ok(true)
    }

    fn daemon_forget_scope_overlap(&self, task_id: &str) {
        self.inner
            .scope_overlap_reports
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
    }

    fn daemon_prune_scope_overlap_reports(&self, tasks: &[Task], state: &OrchestratorState) {
        let mut reports = self
            .inner
            .scope_overlap_reports
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if reports.is_empty() {
            return;
        }
        let by_id: HashMap<&str, &Task> =
            tasks.iter().map(|task| (task.id.as_str(), task)).collect();
        let reserved = |id: &str| state.running.contains_key(id) || state.claimed.contains(id);
        reports.retain(|id, (blocker_id, _)| {
            let (Some(task), Some(blocker)) =
                (by_id.get(id.as_str()), by_id.get(blocker_id.as_str()))
            else {
                return false;
            };
            is_dispatchable(task.status)
                && !reserved(id)
                && id != blocker_id
                && reserved(blocker_id)
                && scopes_overlap(task.scope.as_deref(), blocker.scope.as_deref())
        });
    }

    async fn daemon_preparation_valid(
        &self,
        task: &Task,
        agent: &Agent,
        generation: u64,
    ) -> Result<bool> {
        if !self
            .inner
            .preparing_agents
            .lock()
            .await
            .get(&task.id)
            .is_some_and(|(current, _)| *current == generation)
        {
            return Ok(false);
        }
        let Some(current) = self.deps().task_store.get(&task.id).await? else {
            return Ok(false);
        };
        if current != *task || !is_dispatchable(current.status) || current.awaits_approval() {
            return Ok(false);
        }
        if let (Some(id), Some(goals)) = (&current.goal_id, &self.deps().goal_store)
            && goals
                .get(id)
                .await?
                .is_some_and(|goal| goal.status == GoalStatus::Paused)
        {
            return Ok(false);
        }
        if agent.remote.is_none() {
            let Some(current_agent) = self.deps().agent_store.get(&agent.id).await? else {
                return Ok(false);
            };
            if current_agent.name != agent.name
                || current_agent.adapter != agent.adapter
                || current_agent.role != agent.role
                || current_agent.config != agent.config
                || current_agent.autonomous != agent.autonomous
                || current_agent.capabilities != agent.capabilities
                || current_agent.status != AgentStatus::Idle
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Whether this generation still owns the task's preparation. A
    /// generation is never reissued, so once false it stays false.
    pub(super) async fn owns_preparation(&self, task_id: &str, generation: u64) -> bool {
        self.inner
            .preparing_agents
            .lock()
            .await
            .get(task_id)
            .is_some_and(|(current, _)| *current == generation)
    }

    async fn daemon_release_preparation(&self, task_id: &str, generation: u64) -> Result<()> {
        // Already released (a workspace failure releases as it records): do
        // not queue for the boundary only to commit nothing.
        if !self.owns_preparation(task_id, generation).await {
            return Ok(());
        }
        self.deps()
            .events
            .operation_boundary()
            .run(false, async {
                if self.is_shutting_down() || !self.owns_preparation(task_id, generation).await {
                    return Ok(());
                }
                self.inner
                    .pending_preparation_cleanup
                    .lock()
                    .await
                    .insert(task_id.to_owned(), generation);
                let mut slot = self.inner.state.lock().await;
                // The previous release reached its final state write. Leave
                // that failed write for the next operation, as the recorder's
                // dirty-state contract requires, instead of flushing it from
                // the launch's immediate error-path release.
                if self.inner.remote_state_dirty.load(Ordering::SeqCst)
                    && slot
                        .as_ref()
                        .is_some_and(|state| !state.claimed.contains(task_id))
                {
                    return Err(Error::other("Preparation cleanup awaits state persistence"));
                }
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                self.release_preparation_locked(state, task_id, generation)
                    .await
            })
            .await
            .value
    }

    /// Retain the reservation until run finalization and claim removal are
    /// durable. Every caller holds the project boundary and the state lock.
    pub(super) async fn release_preparation_locked(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        generation: u64,
    ) -> Result<()> {
        self.inner
            .pending_preparation_cleanup
            .lock()
            .await
            .insert(task_id.to_owned(), generation);
        for run in self.deps().run_store.list_for_task(task_id).await? {
            if run.status == RunStatus::Preparing {
                self.deps()
                    .run_service
                    .finish(&run.id, RunStatus::Cancelled, None, None)
                    .await?;
            }
        }
        state.claimed.shift_remove(task_id);
        // A failed write must not reload the old claim or lose a recorder's
        // queued retry. The preparation still reserves the task and agent.
        self.save_remote_state(state).await?;
        self.inner.preparing_agents.lock().await.remove(task_id);
        self.drop_cancel(task_id).await;
        self.inner
            .pending_preparation_cleanup
            .lock()
            .await
            .remove(task_id);
        Ok(())
    }

    /// Account a workspace failure for the preparation this call still owns
    /// and release it in the same operation: one state write per failed
    /// candidate, not one for the retry and another for the release.
    async fn daemon_record_workspace_failure(
        &self,
        task_id: &str,
        generation: u64,
        error: &Error,
    ) -> Result<PreparationFailureOutcome> {
        self.deps()
            .events
            .operation_boundary()
            .run(false, async {
                if self.daemon_preparation_stopping() {
                    return Ok(PreparationFailureOutcome::Cancelled);
                }
                if !self.owns_preparation(task_id, generation).await {
                    return Ok(PreparationFailureOutcome::Failed);
                }
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                if self
                    .record_workspace_dispatch_failure(state, task_id, None, error)
                    .await
                {
                    // The task is saved `retrying` (on a final attempt: with
                    // its record pending), but its back-off exists only here
                    // until the save below. If the release or that save
                    // fails, the next `load_state` must write this state
                    // rather than replace it, or the retry runs with no wait.
                    self.inner.remote_state_dirty.store(true, Ordering::SeqCst);
                }
                self.release_preparation_locked(state, task_id, generation)
                    .await?;
                Ok(PreparationFailureOutcome::Failed)
            })
            .await
            .value
    }

    async fn daemon_fail_preparation(
        &self,
        task: &Task,
        agent: &Agent,
        generation: u64,
        run_id: &str,
        error: &orch_core::adapter::AdapterError,
        workspace: &orch_core::workspace::PrepareResult,
    ) -> Result<PreparationFailureOutcome> {
        let mut owns_preparation = false;
        let mut stopped = false;
        let result = self
            .deps()
            .events
            .operation_boundary()
            .run(false, async {
                if self.daemon_preparation_stopping() {
                    stopped = true;
                    return Ok(());
                }

                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                let current = self.deps().task_service.get(&task.id).await?;
                owns_preparation = self
                    .inner
                    .preparing_agents
                    .lock()
                    .await
                    .get(&task.id)
                    .is_some_and(|(current, _)| *current == generation);
                if !owns_preparation || orch_domain::is_terminal(current.status) {
                    self.deps()
                        .run_service
                        .finish(run_id, RunStatus::Cancelled, None, None)
                        .await?;
                } else {
                    state.retry_queue.retain(|retry| retry.task_id != task.id);
                    self.deps()
                        .task_service
                        .update_status(&task.id, TaskStatus::InProgress)
                        .await?;
                    self.deps()
                        .task_service
                        .increment_attempts(&task.id)
                        .await?;
                    let now = self.deps().clock.now_iso();
                    let entry = RunningEntry {
                        task_id: task.id.clone(),
                        agent_id: agent.id.clone(),
                        run_id: run_id.to_owned(),
                        pid: 0,
                        started_at: now.clone(),
                        last_event_at: now,
                        remote: None,
                    };
                    state.running.insert(task.id.clone(), entry.clone());
                    self.handle_run_failure_locked(
                        state,
                        &task.id,
                        &entry,
                        &error.to_string(),
                        Some(error.kind),
                    )
                    .await?;
                }
                self.save_state(state).await
            })
            .await
            .value;
        if owns_preparation && let Some(branch) = &workspace.branch {
            let cleanup = (task.id.clone(), branch.clone());
            let mut cleanups = self.inner.deferred_cleanups.lock().await;
            if !cleanups.contains(&cleanup) {
                cleanups.push(cleanup);
            }
        }
        self.daemon_drain_cleanups().await;
        if owns_preparation {
            self.daemon_release_preparation(&task.id, generation)
                .await?;
        }
        result.map(|()| {
            if stopped {
                PreparationFailureOutcome::Cancelled
            } else {
                PreparationFailureOutcome::Failed
            }
        })
    }
}

#[cfg(test)]
mod scope_overlap_tests {
    use super::*;
    use crate::orchestrator::tests::harness::Harness;
    use orch_core::storage::TaskStore;
    use orch_core::test_support::make_task;

    #[tokio::test]
    async fn t7_deleted_held_tasks_do_not_accumulate_observations() {
        let blocker = Task {
            id: "A".into(),
            scope: Some(vec!["src/**".into()]),
            ..make_task()
        };
        let mut state = OrchestratorState::default();
        state.claimed.insert("A".into());
        let h = Harness::builder().tasks([blocker]).state(state).build();
        h.orchestrator.enable_daemon_mode();
        for n in 0..20 {
            let held = Task {
                id: format!("held-{n}"),
                scope: Some(vec!["src/auth/**".into()]),
                ..make_task()
            };
            h.tasks.seed(held.clone());
            h.orchestrator.daemon_launch_task(&held.id).await.unwrap();
            assert_eq!(
                h.events()
                    .iter()
                    .filter(|e| matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }))
                    .count(),
                n + 1
            );
            assert_eq!(
                h.orchestrator
                    .inner
                    .scope_overlap_reports
                    .lock()
                    .unwrap()
                    .len(),
                1
            );
            h.tasks.delete(&held.id).await.unwrap();
            h.orchestrator.daemon_tick(true).await.unwrap();
            assert!(
                h.orchestrator
                    .inner
                    .scope_overlap_reports
                    .lock()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn t17_signed_extremes_do_not_wrap_the_repeat_interval() {
        assert!(scope_overlap_repeat_due(i64::MAX, i64::MIN));
        assert!(!scope_overlap_repeat_due(i64::MIN, i64::MAX));
        assert!(!scope_overlap_repeat_due(i64::MIN, i64::MIN));
        assert!(!scope_overlap_repeat_due(i64::MAX, i64::MAX));
        assert!(!scope_overlap_repeat_due(i64::MAX, i64::MAX - 299_999));
        assert!(scope_overlap_repeat_due(i64::MIN + 300_000, i64::MIN));
    }
}
