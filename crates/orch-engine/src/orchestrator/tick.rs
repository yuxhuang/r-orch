//! The tick loop: reconcile, autonomous seeding, and the reactive dispatch
//! debounce.
//!
//! Phase order is contract (PARITY.md §7.3) and the whole tick runs under the
//! state lock.

use std::sync::atomic::Ordering;

use orch_core::error::Result;
use orch_core::storage::{AgentStore, GoalStore, TaskStore};
use orch_domain::{
    AgentStatus, OrchestratorEvent, OrchestratorState, RunningEntry, TaskStatus, is_dispatchable,
    is_terminal,
};

use super::{AUTO_SEED_COOLDOWN_MS, DEBOUNCE, MAX_IMMEDIATE_DISPATCH_RETRIES, Orchestrator};

/// `new Date(value).getTime()`. Now lives beside the formatter it inverts,
/// in `orch-core`, so both share one node-verified test vector.
pub(super) use orch_core::clock::epoch_ms_from_iso;

impl Orchestrator {
    /// One tick, under the state lock, in contract order: invalidate the
    /// tick-scoped caches, load state, reconcile, seed autonomous work,
    /// dispatch, emit `orchestrator:tick`. Then, **outside** the lock, touch
    /// the lock file so a slow tick is not mistaken for a dead holder.
    pub(super) async fn tick_once(&self) -> Result<()> {
        if self.is_shutting_down() {
            return Ok(());
        }

        self.inner.tick_in_progress.store(true, Ordering::SeqCst);
        let result = self.tick_body().await;
        self.inner.tick_in_progress.store(false, Ordering::SeqCst);

        // Touched even when the tick failed: the holder is alive either way,
        // and a failing tick must not look like a crashed process.
        self.deps().lock.touch().await;
        result
    }

    async fn tick_body(&self) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        if self.is_shutting_down() {
            return Ok(());
        }

        self.invalidate_caches().await;
        self.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");

        self.reconcile(state).await?;
        if !self.inner.skip_autonomous_seeding.load(Ordering::SeqCst) {
            self.seed_autonomous_tasks().await;
        }
        self.dispatch_all(state).await?;

        let tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        self.emit(OrchestratorEvent::OrchestratorTick {
            running: state.running.len(),
            queued: tasks
                .iter()
                .filter(|task| is_dispatchable(task.status))
                .count(),
        });
        Ok(())
    }

    /// Drop every tick-scoped memoized read.
    pub(super) async fn invalidate_caches(&self) {
        self.inner.cached_tasks.invalidate_async().await;
        self.inner.cached_agents.invalidate_async().await;
        if let Some(goals) = self.inner.cached_goals.as_ref() {
            goals.invalidate_async().await;
        }
    }

    /// Check PID liveness, detect stalls, fix stale agents and orphaned tasks,
    /// and dispatch anything whose retry has come due (PARITY.md §7.4).
    pub(super) async fn reconcile(&self, state: &mut OrchestratorState) -> Result<()> {
        let now = self.deps().clock.now_ms();
        let entries: Vec<(String, RunningEntry)> = state
            .running
            .iter()
            .map(|(task_id, entry)| (task_id.clone(), entry.clone()))
            .collect();

        // Pre-fetch in parallel, as the TS `Promise.all` pair does: one round
        // trip per store rather than one per entry in sequence.
        let (tasks, agents) = futures::future::join(
            futures::future::join_all(
                entries
                    .iter()
                    .map(|(task_id, _)| self.deps().task_store.get(task_id)),
            ),
            futures::future::join_all(
                entries
                    .iter()
                    .map(|(_, entry)| self.deps().agent_store.get(&entry.agent_id)),
            ),
        )
        .await;

        for (index, (task_id, entry)) in entries.iter().enumerate() {
            let task = tasks
                .get(index)
                .and_then(|result| result.as_ref().ok())
                .and_then(Option::as_ref);
            let live = task.is_some_and(|task| !is_terminal(task.status));

            // 1. The task is gone or already terminal: the running entry is
            // stale bookkeeping, nothing more.
            if !live {
                if let Some(remote) = &entry.remote {
                    self.cancel_remote(remote).await;
                }
                self.take_cancel(task_id).await;
                state.running.shift_remove(task_id);
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
                            "reconcile setStatus idle for stale agent {} (task {task_id})",
                            entry.agent_id
                        ),
                    );
                }
                continue;
            }

            // A peer owns both process liveness and stall detection. A lost
            // link leaves the outcome pending, regardless of the local clock.
            if let Some(remote) = &entry.remote {
                let registry = orch_infra::storage::remote::FileRemoteStore::new(
                    orch_infra::storage::Paths::new(self.deps().project_root.clone()),
                );
                if !registry
                    .list()
                    .await?
                    .iter()
                    .any(|item| item.alias == remote.alias)
                {
                    self.cancel_remote(remote).await;
                    self.fail_remote_unknown_locked(
                        state,
                        task_id,
                        &format!("remote {} removed; outcome unknown", remote.alias),
                    )
                    .await?;
                }
                continue;
            }

            // 2. A live collector means the child may have exited cleanly with
            // the success handler still queued for the state lock. Checking
            // the PID or the stall clock here would misread that as a crash.
            if self.inner.active_collectors.lock().await.contains(task_id) {
                continue;
            }

            // A worker mirror can outlive its engine's result collector. PID
            // disappearance then proves only that it stopped, not whether its
            // effects succeeded. Never export this as a retryable failure.
            // Keep the agent reserved while it lives, and do not stall-kill a
            // stored PID: telemetry was lost and the PID may have been reused.
            if task.is_some_and(|task| {
                task.labels
                    .iter()
                    .any(|label| label.starts_with("remote-ctl:"))
            }) {
                if self.deps().processes.is_alive(entry.pid) {
                    continue;
                }
                self.deps()
                    .run_service
                    .finish(
                        &entry.run_id,
                        orch_domain::RunStatus::Cancelled,
                        None,
                        Some("Worker execution collector lost; outcome unknown".into()),
                    )
                    .await?;
                self.deps()
                    .task_service
                    .update_status(task_id, TaskStatus::Cancelled)
                    .await?;
                self.deps()
                    .agent_service
                    .set_status(&entry.agent_id, AgentStatus::Idle)
                    .await?;
                state.running.shift_remove(task_id);
                state.retry_queue.retain(|retry| retry.task_id != *task_id);
                continue;
            }

            // 3. PID liveness.
            if !self.deps().processes.is_alive(entry.pid) {
                // A failure *inside* the failure handler — an invalid
                // transition, say — must still free the slot, or the entry
                // leaks and the agent stays busy forever. The handler has
                // already reported whatever went wrong.
                if self
                    .handle_run_failure_locked(
                        state,
                        task_id,
                        entry,
                        "Process crashed unexpectedly",
                        None,
                    )
                    .await
                    .is_err()
                {
                    state.running.shift_remove(task_id);
                    if let Err(err) = self
                        .deps()
                        .agent_service
                        .set_status(&entry.agent_id, AgentStatus::Idle)
                        .await
                    {
                        self.emit_error(
                            err,
                            format!(
                                "reconcile crash fallback setStatus idle for agent {} (task {task_id})",
                                entry.agent_id
                            ),
                        );
                    }
                }
                continue;
            }

            // 4. Stall detection, per-agent timeout first.
            //
            // The clock is per emitted `AgentEvent`, not per stdout line:
            // `collect_events` refreshes `last_event_at` inside its event
            // loop, so a line the parser maps to nothing does not refresh it
            // (PARITY.md §7.4, V34).
            let stall_timeout = agents
                .get(index)
                .and_then(|r| r.as_ref().ok())
                .and_then(|agent| agent.as_ref())
                .and_then(|agent| agent.config.stall_timeout_ms)
                .unwrap_or(self.config().defaults.agent.stall_timeout_ms);
            let Some(last_event_at) = epoch_ms_from_iso(&entry.last_event_at) else {
                continue;
            };
            if now.saturating_sub(last_event_at) <= stall_timeout as i64 {
                continue;
            }

            self.emit(OrchestratorEvent::OrchestratorStallDetected {
                run_id: entry.run_id.clone(),
            });
            self.take_cancel(task_id).await;
            self.deps()
                .processes
                .kill_with_grace(entry.pid, super::STALL_KILL_GRACE)
                .await;
            // Same fallback as the crash path above.
            if self
                .handle_run_failure_locked(state, task_id, entry, "Agent stalled (no events)", None)
                .await
                .is_err()
            {
                state.running.shift_remove(task_id);
                if let Err(err) = self
                    .deps()
                    .agent_service
                    .set_status(&entry.agent_id, AgentStatus::Idle)
                    .await
                {
                    self.emit_error(
                        err,
                        format!(
                            "reconcile stall fallback setStatus idle for agent {} (task {task_id})",
                            entry.agent_id
                        ),
                    );
                }
            }
        }

        self.reconcile_stale_agents(state).await?;
        self.reconcile_orphaned_tasks(state).await?;
        self.process_retry_queue(state).await?;
        self.save_state(state).await
    }

    /// Agents stuck in `running` with no running entry — the mirror image of
    /// an orphaned task.
    async fn reconcile_stale_agents(&self, state: &OrchestratorState) -> Result<()> {
        let running_agents: std::collections::HashSet<&str> = state
            .running
            .values()
            .map(|entry| entry.agent_id.as_str())
            .collect();
        let agents = self.inner.cached_agents.list().await?;
        let stale: Vec<&orch_domain::Agent> = agents
            .iter()
            .filter(|agent| {
                agent.status == AgentStatus::Running && !running_agents.contains(agent.id.as_str())
            })
            .collect();
        if stale.is_empty() {
            return Ok(());
        }
        // One failure fails the whole reconcile, where the orphaned-task sweep
        // below swallows per item. That asymmetry looks like an oversight and
        // is not: the reference wraps these in a bare
        // `await Promise.all(staleAgents.map(...))`
        // (`orchestrator.ts:618-622`), so a rejection propagates out of
        // reconcile and fails the tick, while the orphaned-task map has its
        // own `try`/`catch` (`:630-637`). Reproduced rather than tidied, and
        // `a_stale_agent_write_failure_fails_the_tick_but_an_orphaned_task_one_does_not`
        // pins both halves.
        futures::future::join_all(stale.iter().map(|agent| {
            self.deps()
                .agent_service
                .set_status(&agent.id, AgentStatus::Idle)
        }))
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        Ok(())
    }

    /// Tasks stuck in `in_progress` with no running entry. Force-written when
    /// the transition is rejected — this is one of the recovery paths that
    /// deliberately bypasses the state machine (PARITY.md §7.1).
    async fn reconcile_orphaned_tasks(&self, state: &OrchestratorState) -> Result<()> {
        let tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        let orphaned: Vec<orch_domain::Task> = tasks
            .into_iter()
            .filter(|task| {
                task.status == TaskStatus::InProgress && !state.running.contains_key(&task.id)
            })
            .collect();
        if orphaned.is_empty() {
            return Ok(());
        }

        futures::future::join_all(orphaned.into_iter().map(|mut task| async move {
            if self
                .deps()
                .task_service
                .update_status(&task.id, TaskStatus::Failed)
                .await
                .is_err()
            {
                task.status = TaskStatus::Failed;
                task.updated_at = self.deps().clock.now_iso();
                if let Err(err) = self.deps().task_store.save(&task).await {
                    self.emit_error(err, format!("force-write orphaned task {}", task.id));
                }
            }
            self.emit(OrchestratorEvent::TaskOrphaned {
                task_id: task.id.clone(),
            });
        }))
        .await;
        Ok(())
    }

    /// Dispatch everything whose `due_at` has passed, re-reading each task
    /// first: it may have succeeded through another path while it waited.
    async fn process_retry_queue(&self, state: &mut OrchestratorState) -> Result<()> {
        let now = self.deps().clock.now_ms();
        let mut due = Vec::new();
        state.retry_queue.retain(|retry| {
            match epoch_ms_from_iso(&retry.due_at) {
                Some(due_at) if now >= due_at => {
                    due.push(retry.task_id.clone());
                    false
                }
                // An unparseable `due_at` never comes due, exactly as
                // `now >= NaN` is never true. It stays queued rather than
                // being silently dropped.
                _ => true,
            }
        });

        for task_id in due {
            let task = self.deps().task_store.get(&task_id).await?;
            let Some(task) = task.filter(|task| is_dispatchable(task.status)) else {
                continue;
            };
            self.dispatch_task(state, &task_id, Some(task)).await?;
        }
        Ok(())
    }

    /// Create work for idle autonomous agents (PARITY.md §7.12).
    ///
    /// Failures are per-agent and non-fatal: one agent whose task cannot be
    /// created must not stop the others from being seeded.
    pub(super) async fn seed_autonomous_tasks(&self) {
        let agents = match self.inner.cached_agents.list().await {
            Ok(agents) => agents,
            Err(err) => {
                self.emit_error(err, "autonomous seeding: list agents");
                return;
            }
        };
        let autonomous: Vec<orch_domain::Agent> = agents
            .into_iter()
            .filter(|agent| agent.autonomous == Some(true) && agent.status == AgentStatus::Idle)
            .collect();
        if autonomous.is_empty() {
            return;
        }

        let all_tasks = match self.inner.cached_tasks.list(&Default::default()).await {
            Ok(tasks) => tasks,
            Err(err) => {
                self.emit_error(err, "autonomous seeding: list tasks");
                return;
            }
        };
        let active_goals = match self.inner.cached_goals.as_ref() {
            Some(goals) => goals
                .list(&orch_core::storage::GoalFilter::with_status(
                    orch_domain::GoalStatus::Active,
                ))
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };

        let now = self.deps().clock.now_ms();
        let mut claimed_goals: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut any_created = false;

        for agent in autonomous {
            // An agent with work in flight does not need more.
            if all_tasks.iter().any(|task| {
                task.assignee.as_deref() == Some(&agent.id) && !is_terminal(task.status)
            }) {
                continue;
            }
            // Cooldown, so a failing agent is not handed a new task every tick.
            {
                let seeded = self.inner.last_auto_seed_at.lock().await;
                let last = seeded.get(&agent.id).copied().unwrap_or(0);
                if now - last < AUTO_SEED_COOLDOWN_MS {
                    continue;
                }
            }

            // Prefer a goal assigned to this agent, then any unassigned one.
            // Claiming within the pass stops two agents taking the same goal.
            let goal = active_goals
                .iter()
                .find(|goal| {
                    goal.assignee.as_deref() == Some(&agent.id) && !claimed_goals.contains(&goal.id)
                })
                .or_else(|| {
                    active_goals
                        .iter()
                        .find(|goal| goal.assignee.is_none() && !claimed_goals.contains(&goal.id))
                });
            if let Some(goal) = goal {
                claimed_goals.insert(goal.id.clone());
            }

            let role = agent.role.as_deref().unwrap_or("general assistant");
            let (title, description) = match goal {
                Some(goal) => (
                    format!("[auto] {}: {}", agent.name, slice_chars(&goal.title, 60)),
                    format!(
                        "## GOAL (highest priority)\n\n{}\n\n---\nAgent role: {role}",
                        if goal.description.is_empty() {
                            &goal.title
                        } else {
                            &goal.description
                        }
                    ),
                ),
                None => (
                    format!("[auto] {}: {}", agent.name, slice_chars(role, 60)),
                    format!("Autonomous work cycle. Agent role: {role}"),
                ),
            };

            let input = orch_domain::CreateTaskInput {
                title,
                description: Some(description),
                assignee: Some(agent.id.clone()),
                labels: Some(vec![orch_domain::AUTONOMOUS_LABEL.to_owned()]),
                priority: Some(3),
                goal_id: goal.map(|goal| goal.id.clone()),
                ..Default::default()
            };
            match self.deps().task_service.create(input).await {
                Ok(_) => {
                    self.inner
                        .last_auto_seed_at
                        .lock()
                        .await
                        .insert(agent.id.clone(), now);
                    any_created = true;
                }
                Err(err) => {
                    self.emit_error(err, format!("autonomous task for agent {}", agent.id));
                }
            }
        }

        if any_created {
            self.inner.cached_tasks.invalidate_async().await;
        }
    }

    // ── reactive dispatch ────────────────────────────────────────────────

    /// Schedule an immediate dispatch, debounced by 500 ms. If a tick is in
    /// flight when the timer fires it reschedules, up to 10 times (≈5 s), then
    /// gives up and lets the next poll handle it.
    ///
    /// The debounce is observable — `orchestrator.test.ts` waits past it — so
    /// it stays even though the `select!` arms already reduce the contention
    /// the retry loop was written for.
    pub(super) fn schedule_immediate_dispatch(&self, retries: u32) {
        if self.is_shutting_down() {
            return;
        }
        let orchestrator = self.clone();
        tokio::spawn(async move {
            {
                let mut slot = orchestrator.inner.immediate_dispatch.lock().await;
                if slot.is_some() {
                    return; // already scheduled
                }
                let inner = orchestrator.clone();
                *slot = Some(tokio::spawn(async move {
                    tokio::time::sleep(DEBOUNCE).await;
                    *inner.inner.immediate_dispatch.lock().await = None;
                    if inner.is_shutting_down() {
                        return;
                    }
                    if inner.inner.tick_in_progress.load(Ordering::SeqCst) {
                        if retries < MAX_IMMEDIATE_DISPATCH_RETRIES {
                            inner.schedule_immediate_dispatch(retries + 1);
                        }
                        return;
                    }
                    if let Err(err) = inner.immediate_dispatch().await {
                        inner.emit_error(err, "immediate dispatch on task:created");
                    }
                }));
            }
        });
    }

    /// A mini-tick: invalidate, load, dispatch, save. **No reconcile.**
    /// Skipped entirely while a `run <id>` single-task run is in flight, so a
    /// one-shot command cannot opportunistically start other work.
    async fn immediate_dispatch(&self) -> Result<()> {
        if self.is_shutting_down() {
            return Ok(());
        }
        if !self.inner.single_task_runs.lock().await.is_empty() {
            return Ok(());
        }
        self.fresh_dispatch_all().await
    }

    /// Whether an immediate dispatch is currently scheduled. The TS suite
    /// asserts on `immediateDispatchTimer` being cleared by `stop`.
    #[cfg(test)]
    pub(super) async fn has_pending_immediate_dispatch(&self) -> bool {
        self.inner.immediate_dispatch.lock().await.is_some()
    }
}

/// `String.prototype.slice(0, n)` over `char`s. The titles this is applied to
/// are user-supplied, so a byte slice could split a code point.
fn slice_chars(value: &str, max: usize) -> &str {
    match value.char_indices().nth(max) {
        Some((byte_index, _)) => &value[..byte_index],
        None => value,
    }
}
