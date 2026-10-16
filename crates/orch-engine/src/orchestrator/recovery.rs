//! Startup recovery and graceful shutdown (PARITY.md §7.6, §7.14).

use orch_core::error::Result;
use orch_core::storage::TaskStore;
use orch_domain::{
    AgentStatus, OrchestratorState, RunStatus, TaskStatus, is_terminal, resolve_failure_status,
};

use super::Orchestrator;

// Leave room inside the TUI's five-second dead-terminal wind-down budget.
const REMOTE_CANCEL_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

impl Orchestrator {
    /// A running entry may only be a reservation for a saved, unverified PID.
    /// Missing records cannot turn lost ownership into permission to signal.
    pub(super) async fn worker_signal_unverified(&self, entry: &orch_domain::RunningEntry) -> bool {
        if entry.remote.is_some() {
            return false;
        }
        let task = self.deps().task_store.get(&entry.task_id).await;
        let run = self.deps().run_store.get(&entry.run_id).await;
        let unknown = |error: Option<&str>| {
            error.is_some_and(|error| {
                error.starts_with("Worker execution collector lost; outcome unknown")
            })
        };
        if run
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|run| unknown(run.error.as_deref()))
            || task
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|task| unknown(task.feedback.as_deref()))
        {
            return true;
        }
        if self
            .inner
            .active_collectors
            .lock()
            .await
            .contains(&entry.task_id)
        {
            return false;
        }
        match (task, run) {
            (Ok(Some(task)), Ok(_)) => task
                .labels
                .iter()
                .any(|label| label.starts_with("remote-ctl:")),
            _ => true,
        }
    }

    /// A saved PID proves liveness, not identity. Quarantine worker mirrors
    /// before any generic local recovery can signal it or schedule a retry.
    pub(super) async fn quarantine_worker_mirror(
        &self,
        state: &mut OrchestratorState,
        task: &orch_domain::Task,
        run: Option<&orch_domain::Run>,
    ) -> Result<()> {
        let entry = state.running.get(&task.id).cloned();
        let pid = entry
            .as_ref()
            .map(|entry| entry.pid)
            .or_else(|| run.and_then(|run| run.pid));
        let existing_diagnostic = run
            .and_then(|run| run.error.as_deref())
            .filter(|error| error.starts_with("Worker execution collector lost; outcome unknown"))
            .or_else(|| {
                task.feedback.as_deref().filter(|error| {
                    error.starts_with("Worker execution collector lost; outcome unknown")
                })
            });
        let diagnostic = existing_diagnostic.map(str::to_owned).unwrap_or_else(|| match pid {
            Some(pid) => format!(
                "Worker execution collector lost; outcome unknown; PID {pid} not signalled, may still be running; agent reservation held while this PID is alive (PID reuse may delay release)"
            ),
            None => "Worker execution collector lost; outcome unknown; no saved PID: child not signalled, may still be running; a child may have started before its PID was persisted; reservation released".into(),
        });
        let alive = pid.is_some_and(|pid| self.deps().processes.is_alive(pid));
        // Persist a reconstructed reservation before publishing a terminal
        // outcome. A crash between those writes must not free a live workspace.
        if alive
            && entry.is_none()
            && let Some(run) = run
        {
            state.running.insert(
                task.id.clone(),
                orch_domain::RunningEntry {
                    task_id: task.id.clone(),
                    run_id: run.id.clone(),
                    agent_id: run.agent_id.clone(),
                    pid: pid.expect("live PID"),
                    started_at: run.started_at.clone(),
                    last_event_at: self.deps().clock.now_iso(),
                    remote: None,
                },
            );
            self.save_state(state).await?;
        }
        if let Some(run_id) = entry
            .as_ref()
            .map(|entry| entry.run_id.as_str())
            .or_else(|| run.map(|run| run.id.as_str()))
        {
            // Do not rewrite terminal records on every tick/restart.
            if task.status != TaskStatus::Cancelled
                || task.feedback.as_deref() != Some(diagnostic.as_str())
                || run.is_some_and(|run| run.status != RunStatus::Cancelled)
            {
                self.deps()
                    .run_service
                    .finish(run_id, RunStatus::Cancelled, None, Some(diagnostic.clone()))
                    .await?;
            }
        }
        if task.status != TaskStatus::Cancelled
            || task.feedback.as_deref() != Some(diagnostic.as_str())
        {
            let mut cancelled = task.clone();
            cancelled.status = TaskStatus::Cancelled;
            cancelled.feedback = Some(diagnostic.clone());
            cancelled.updated_at = self.deps().clock.now_iso();
            self.deps().task_store.save(&cancelled).await?;
            self.emit_error(&diagnostic, format!("worker recovery: task {}", task.id));
        }
        self.update_worker_reservation(
            state,
            &task.id,
            entry
                .as_ref()
                .map(|entry| entry.run_id.as_str())
                .or_else(|| run.map(|run| run.id.as_str())),
            entry
                .as_ref()
                .map(|entry| entry.agent_id.as_str())
                .or_else(|| run.map(|run| run.agent_id.as_str()))
                .or(task.assignee.as_deref()),
            alive,
        )
        .await
    }

    /// Reservation liveness and release use run/entry metadata, even when the
    /// individual task record is missing or unreadable. This never signals.
    pub(super) async fn update_worker_reservation(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        run_id: Option<&str>,
        agent_id: Option<&str>,
        alive: bool,
    ) -> Result<()> {
        if !alive {
            // Clear the saved PID before removing the reservation. A failed
            // run read/write must retain it for the next reconcile, and a
            // later recycled PID must not resurrect a released reservation.
            if let Some(run_id) = run_id
                && let Some(mut finished) = self.deps().run_store.get(run_id).await?
                && finished.pid.take().is_some()
            {
                self.deps().run_store.save(&finished).await?;
            }
            state.running.shift_remove(task_id);
        }
        state.claimed.shift_remove(task_id);
        state.retry_queue.retain(|retry| retry.task_id != task_id);
        if let Some(agent_id) = agent_id
            && let Err(err) = self
                .deps()
                .agent_service
                .set_status(
                    agent_id,
                    if alive {
                        AgentStatus::Running
                    } else {
                        AgentStatus::Idle
                    },
                )
                .await
        {
            self.emit_error(err, format!("worker recovery: agent {agent_id}"));
        }
        self.save_state(state).await
    }

    async fn quarantine_worker_mirrors(&self, state: &mut OrchestratorState) -> Result<()> {
        let tasks = self.deps().task_store.list(&Default::default()).await?;
        let runs = self.deps().run_store.list_all().await?;
        let mirrors: Vec<_> = tasks
            .iter()
            .filter(|task| {
                task.labels
                    .iter()
                    .any(|label| label.starts_with("remote-ctl:"))
                    || runs.iter().any(|run| {
                        run.task_id == task.id
                            && run.error.as_deref().is_some_and(|error| {
                                error
                                    .starts_with("Worker execution collector lost; outcome unknown")
                            })
                    })
            })
            .collect();
        for task in mirrors {
            let run = runs
                .iter()
                .filter(|run| run.task_id == task.id)
                .max_by_key(|run| run.attempt);
            let needs_quarantine = state.running.contains_key(&task.id)
                || task.status == TaskStatus::InProgress
                || (task.status == TaskStatus::Cancelled
                    && task.feedback.as_deref().is_some_and(|error| {
                        error.starts_with("Worker execution collector lost; outcome unknown")
                    })
                    && run.is_some_and(|run| run.pid.is_some()))
                || run.is_some_and(|run| {
                    matches!(run.status, RunStatus::Running | RunStatus::Preparing)
                        || (run.pid.is_some()
                            && run.error.as_deref().is_some_and(|error| {
                                error
                                    .starts_with("Worker execution collector lost; outcome unknown")
                            }))
                });
            if needs_quarantine
                && let Err(err) = self.quarantine_worker_mirror(state, task, run).await
            {
                self.emit_error(err, format!("worker recovery: task {}", task.id));
            }
        }
        Ok(())
    }

    /// Clean up after a crashed or restarted process.
    ///
    /// Orphaned tasks are **cancelled, not failed**: a failed task retries,
    /// and retrying work an agent already committed is worse than stopping.
    /// A user can reactivate a specific task by hand.
    pub(super) async fn cleanup_stale_running_entries(
        &self,
        state: &mut OrchestratorState,
    ) -> Result<()> {
        if let Err(err) = self.quarantine_worker_mirrors(state).await {
            self.emit_error(err, "worker recovery: list mirrors");
        }
        let dead: Vec<(String, orch_domain::RunningEntry)> = state
            .running
            .iter()
            .filter(|(_, entry)| {
                entry.remote.is_some() || !self.deps().processes.is_alive(entry.pid)
            })
            .map(|(task_id, entry)| (task_id.clone(), entry.clone()))
            .collect();

        for (task_id, _) in &dead {
            state.running.shift_remove(task_id);
        }

        futures::future::join_all(dead.iter().map(|(task_id, entry)| async move {
            if entry.remote.is_none()
                && let Err(err) = self
                    .deps()
                    .agent_service
                    .set_status(&entry.agent_id, AgentStatus::Idle)
                    .await
            {
                self.emit_error(
                    err,
                    format!(
                        "startup cleanup: setStatus idle for agent {}",
                        entry.agent_id
                    ),
                );
            }
            self.force_task_cancelled(task_id).await;
            if let Err(err) = self
                .deps()
                .run_service
                .finish(
                    &entry.run_id,
                    RunStatus::Cancelled,
                    None,
                    Some("Orchestrator restarted".to_owned()),
                )
                .await
            {
                self.emit_error(err, format!("startup cleanup: finish run {}", entry.run_id));
            }
        }))
        .await;

        // **Always** cleared: any claim that survived a restart is stale by
        // definition, because the process that set it is gone. This is what
        // unsticks tasks left "claimed" by a crash.
        state.claimed.clear();

        // Only when a restart was actually detected. Without dead PIDs, an
        // orphan is just a task reconcile has not reached yet, and cancelling
        // it would be wrong.
        if !dead.is_empty() {
            let all_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
            let orphaned: Vec<String> = all_tasks
                .into_iter()
                .filter(|task| {
                    task.status == TaskStatus::InProgress && !state.running.contains_key(&task.id)
                })
                .map(|task| task.id)
                .collect();
            futures::future::join_all(
                orphaned
                    .iter()
                    .map(|task_id| self.force_task_cancelled(task_id)),
            )
            .await;

            let cancelled: std::collections::HashSet<&str> = dead
                .iter()
                .map(|(task_id, _)| task_id.as_str())
                .chain(orphaned.iter().map(String::as_str))
                .collect();
            state
                .retry_queue
                .retain(|retry| !cancelled.contains(retry.task_id.as_str()));
            self.save_state(state).await?;
        }

        self.cleanup_orphaned_preparing_runs(state).await;
        Ok(())
    }

    /// Finalize runs created but never started — a crash between
    /// `run_service.create` and `run_service.start`. They have no running
    /// entry, so reconcile cannot see them and they would stay `preparing`
    /// forever.
    async fn cleanup_orphaned_preparing_runs(&self, state: &OrchestratorState) {
        let all_runs = match self.deps().run_store.list_all().await {
            Ok(runs) => runs,
            Err(err) => {
                self.emit_error(err, "startup cleanup: cleanupOrphanedPreparingRuns");
                return;
            }
        };

        // A run belonging to a *live* entry may legitimately be `preparing`
        // for a moment during this process's own dispatch.
        let active: std::collections::HashSet<&str> = state
            .running
            .values()
            .map(|entry| entry.run_id.as_str())
            .collect();
        let orphaned: Vec<&orch_domain::Run> = all_runs
            .iter()
            .filter(|run| run.status == RunStatus::Preparing && !active.contains(run.id.as_str()))
            .collect();
        if orphaned.is_empty() {
            return;
        }

        futures::future::join_all(orphaned.iter().map(|run| async move {
            if let Err(err) = self
                .deps()
                .run_service
                .finish(
                    &run.id,
                    RunStatus::Cancelled,
                    None,
                    Some("Orphaned preparing run (orchestrator restarted)".to_owned()),
                )
                .await
            {
                self.emit_error(
                    err,
                    format!("startup cleanup: finish orphaned preparing run {}", run.id),
                );
            }
        }))
        .await;
    }

    /// Finalize runs that were started but whose running entry never reached
    /// disk — a crash between `adapter.execute` and `save_state`.
    ///
    /// The window is four statements wide in `dispatch_claimed`, and nothing
    /// else closes it: `dead` is computed from `state.running`, so it is
    /// empty; the orphaned-task sweep is gated on `dead` being non-empty;
    /// `reconcile` iterates `state.running`; and the `preparing` sweep filters
    /// that status only. So the *task* self-heals through
    /// `reconcile_orphaned_tasks`, but the run record stays `running` forever
    /// and the agent's child keeps working in a workspace whose task has been
    /// failed — writing, committing, burning tokens, parented to nothing.
    ///
    /// Worker mirrors have already been quarantined by startup recovery.
    /// For local runs the pid is the only handle (PARITY.md §8.4), and it
    /// is on the run record, so the existing local path kills then finalizes.
    pub(super) async fn cleanup_orphaned_running_runs(
        &self,
        state: &OrchestratorState,
    ) -> Result<()> {
        let all_runs = match self.deps().run_store.list_all().await {
            Ok(runs) => runs,
            Err(err) => {
                self.emit_error(err, "startup cleanup: cleanupOrphanedRunningRuns");
                return Ok(());
            }
        };

        let active: std::collections::HashSet<&str> = state
            .running
            .values()
            .map(|entry| entry.run_id.as_str())
            .collect();
        let orphaned: Vec<&orch_domain::Run> = all_runs
            .iter()
            .filter(|run| run.status == RunStatus::Running && !active.contains(run.id.as_str()))
            .collect();

        for run in orphaned {
            if run.agent_id.contains('@') {
                self.force_task_cancelled(&run.task_id).await;
            }
            // Sequential, not `join_all`: each of these may spend the full
            // kill grace, and a crashed daemon's leftovers are a handful.
            if !run.agent_id.contains('@')
                && let Some(pid) = run.pid
                && self.deps().processes.is_alive(pid)
            {
                self.deps()
                    .processes
                    .kill_with_grace(pid, orch_core::process::DEFAULT_KILL_GRACE)
                    .await;
            }
            if let Err(err) = self
                .deps()
                .run_service
                .finish(
                    &run.id,
                    RunStatus::Cancelled,
                    None,
                    Some("Orchestrator restarted".to_owned()),
                )
                .await
            {
                self.emit_error(
                    err,
                    format!("startup cleanup: finish orphaned running run {}", run.id),
                );
            }
        }
        Ok(())
    }

    /// Cancel a task, falling back to a direct store write when the
    /// transition is rejected. One of the recovery paths that deliberately
    /// bypasses the state machine (PARITY.md §7.1).
    pub(super) async fn force_task_cancelled(&self, task_id: &str) {
        if self
            .deps()
            .task_service
            .update_status(task_id, TaskStatus::Cancelled)
            .await
            .is_ok()
        {
            return;
        }
        let mut task = match self.deps().task_store.get(task_id).await {
            Ok(Some(task)) => task,
            // Gone: there is nothing left to cancel.
            Ok(None) => return,
            // Unreadable is not the same as absent, and since WP-0's YAML
            // hardening it is not even the same as corrupt: a read can now
            // fail on an alias-expansion cap or a parse deadline, where the
            // file is intact and the next attempt would succeed. Returning
            // silently would leave the task non-terminal with nothing said.
            Err(err) => {
                self.emit_error(err, format!("startup cleanup: read task {task_id}"));
                return;
            }
        };
        if is_terminal(task.status) {
            return;
        }
        task.status = TaskStatus::Cancelled;
        task.updated_at = self.deps().clock.now_iso();
        if let Err(err) = self.deps().task_store.save(&task).await {
            self.emit_error(err, format!("startup cleanup: force-cancel task {task_id}"));
        }
    }

    /// Called once after shutdown has enqueued all cancels and released the
    /// state mutex, while the owner lock and peer transport are still alive.
    ///
    /// Shutdown defers finishing its remote runs to here, so the drain's
    /// verdict lands on the durable run record: a run whose cancel is still
    /// unacknowledged at the deadline is finished with an outcome-unknown
    /// error (`orch logs <exec_key>`), independent of whichever terminal or
    /// event subscriber outlives the process. This moves those existing
    /// `finish` writes out of the state mutex; it adds no await.
    pub(super) async fn drain_remote_cancellations(&self, deferred: Vec<DeferredRemoteFinish>) {
        let pending = match self
            .remote_bridge()
            .drain_cancellations(REMOTE_CANCEL_DRAIN_DEADLINE)
            .await
        {
            Ok(pending) => {
                // Shutdown's own runs are reported below, with the run record.
                for cancel in pending.iter().filter(|cancel| {
                    !deferred.iter().any(|d| {
                        d.remote.alias == cancel.alias && d.remote.exec_key == cancel.exec_key
                    })
                }) {
                    self.emit_error(
                        orch_core::error::Error::other("Remote cancel outcome unknown; cancellation will be re-sent on the next Sync"),
                        format!("stop: cancel remote {}:{}", cancel.alias, cancel.exec_key),
                    );
                }
                Some(pending)
            }
            Err(error) => {
                self.emit_error(error, "stop: drain remote cancellations");
                None
            }
        };

        for deferred in deferred {
            let remote = &deferred.remote;
            // A failed drain proves nothing was acknowledged.
            let unknown = pending.as_ref().is_none_or(|pending| {
                pending
                    .iter()
                    .any(|p| p.alias == remote.alias && p.exec_key == remote.exec_key)
            });
            if unknown {
                self.emit_error(
                    orch_core::error::Error::other(remote_cancel_unknown(remote)),
                    format!("stop: cancel remote {}:{}", remote.alias, remote.exec_key),
                );
            }
            if let Err(err) = self
                .deps()
                .run_service
                .finish(
                    &deferred.run_id,
                    RunStatus::Cancelled,
                    None,
                    unknown.then(|| remote_cancel_unknown(remote)),
                )
                .await
            {
                self.emit_error(err, format!("stop: finish run {}", deferred.run_id));
            }
        }
    }

    /// Wind down every running agent during `stop`.
    ///
    /// Each task goes to `resolve_failure_status`, so one with attempts left
    /// comes back as `retrying` on the next start rather than being lost.
    ///
    /// **Divergence worth naming:** the TS loop `await`s without catching, so
    /// one failing store call aborts `stop()` before it releases the lock —
    /// which leaves the project wedged for every later invocation. Here each
    /// entry's failures are reported as `orchestrator:error` and the wind-down
    /// continues, so the lock is always released. No ported test distinguishes
    /// the two (nothing throws with the in-memory fakes).
    ///
    /// Remote runs are not finished here: they are returned so
    /// [`Orchestrator::drain_remote_cancellations`] can record the drain's
    /// verdict on them outside the state mutex.
    pub(super) async fn shutdown_running_entries(
        &self,
        state: &OrchestratorState,
    ) -> Vec<DeferredRemoteFinish> {
        let mut deferred = Vec::new();
        let entries: Vec<(String, orch_domain::RunningEntry)> = state
            .running
            .iter()
            .map(|(task_id, entry)| (task_id.clone(), entry.clone()))
            .collect();

        for (task_id, entry) in entries {
            if self.worker_signal_unverified(&entry).await {
                self.emit_error(
                    "Worker execution ownership cannot verify; not signalled; reservation retained",
                    format!("stop: task {task_id}"),
                );
                continue;
            }
            self.take_cancel(&task_id).await;
            if let Some(remote) = &entry.remote {
                self.cancel_remote(remote).await;
            } else {
                self.deps()
                    .processes
                    .kill_with_grace(entry.pid, orch_core::process::DEFAULT_KILL_GRACE)
                    .await;
            }

            if let Some(remote) = &entry.remote {
                deferred.push(DeferredRemoteFinish {
                    run_id: entry.run_id.clone(),
                    remote: remote.clone(),
                });
            } else if let Err(err) = self
                .deps()
                .run_service
                .finish(&entry.run_id, RunStatus::Cancelled, None, None)
                .await
            {
                self.emit_error(err, format!("stop: finish run {}", entry.run_id));
            }

            match self.deps().task_store.get(&task_id).await {
                Ok(Some(task)) => {
                    let next = if entry.remote.is_some() {
                        TaskStatus::Cancelled
                    } else {
                        resolve_failure_status(&task)
                    };
                    if let Err(err) = self.deps().task_service.update_status(&task_id, next).await {
                        self.emit_error(err, format!("stop: set task {task_id} to {next}"));
                    }
                }
                Ok(None) => {}
                Err(err) => self.emit_error(err, format!("stop: read task {task_id}")),
            }

            if entry.remote.is_none()
                && let Err(err) = self
                    .deps()
                    .agent_service
                    .set_status(&entry.agent_id, AgentStatus::Idle)
                    .await
            {
                self.emit_error(
                    err,
                    format!("stop: setStatus idle for agent {}", entry.agent_id),
                );
            }
        }
        deferred
    }
}

/// A remote run cancelled by shutdown, finished once the drain has decided
/// whether its worker acknowledged the cancel.
pub(super) struct DeferredRemoteFinish {
    run_id: String,
    remote: orch_domain::remote::RemoteRunRef,
}

/// The durable diagnostic for a shutdown cancel the worker never confirmed.
pub(crate) fn remote_cancel_unknown(remote: &orch_domain::remote::RemoteRunRef) -> String {
    format!(
        "Remote cancel outcome unknown: peer {} did not acknowledge cancellation of {} before shutdown, so it may still be running there; cancellation will be re-sent on the next Sync",
        remote.alias, remote.exec_key
    )
}
