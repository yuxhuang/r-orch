//! Dispatch: candidate selection, scope gating, and the claim → assign →
//! execute sequence (PARITY.md §7.5).

use std::collections::{BTreeMap, HashMap, HashSet};

use orch_core::adapter::ExecuteParams;
use orch_core::error::{Error, Result};
use orch_core::storage::{AgentStore, GoalStore, TaskStore};
use orch_core::template::{GoalContext, RetryContext};
use orch_domain::{
    AgentStatus, DomainError, OrchestratorEvent, OrchestratorState, RetryEntry, RunningEntry,
    ScopeIndex, Task, TaskProof, TaskStatus, calculate_retry_delay, is_blocked, is_dispatchable,
    is_terminal, resolve_failure_status, scopes_overlap,
};
use orch_infra::template::{
    BuildPromptOptions, DEFAULT_SYSTEM_TEMPLATE, DEFAULT_USER_TEMPLATE, build_prompt_context,
};

use super::{MAX_RETRY_QUEUE_SIZE, Orchestrator, RETRY_CONTEXT_EVENTS};

impl Orchestrator {
    /// Invalidate → load → dispatch → save, under the state lock. Shared by
    /// `run_task`, `run_all` and the reactive dispatch.
    pub(super) async fn fresh_dispatch_all(&self) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        self.invalidate_caches().await;
        self.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");
        if !self.is_shutting_down() {
            self.dispatch_all(state).await?;
        }
        self.save_state(state).await
    }

    pub(super) async fn fresh_dispatch_only(&self, task_id: &str) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        self.invalidate_caches().await;
        self.load_state(&mut slot).await?;
        let state = slot.as_mut().expect("state loaded");
        let result = self.dispatch_only_task(state, task_id).await;
        self.save_state(state).await?;
        result
    }

    /// Dispatch up to the free slots, in scheduling order, skipping anything
    /// whose scope overlaps work already in flight.
    pub(super) async fn dispatch_all(&self, state: &mut OrchestratorState) -> Result<()> {
        let available_slots = self
            .config()
            .scheduling
            .max_concurrent_agents
            .saturating_sub(state.running.len());
        if available_slots == 0 {
            return Ok(());
        }

        let all_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        let task_map: HashMap<String, Task> = all_tasks
            .iter()
            .map(|task| (task.id.clone(), task.clone()))
            .collect();

        let mut candidates: Vec<Task> = all_tasks
            .iter()
            .filter(|task| {
                is_dispatchable(task.status)
                    && !is_blocked(task, &task_map)
                    && !state.running.contains_key(&task.id)
                    && !state.claimed.contains(&task.id)
            })
            .cloned()
            .collect();

        // The scheduling contract (PARITY.md §7.5 step 3): priority ascending,
        // then goal-linked first, then `updated_at` descending. Stable, so an
        // exact three-way tie keeps store order.
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.goal_id.is_none().cmp(&b.goal_id.is_none()))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        candidates.truncate(available_slots);

        // Scope gating. The index starts from everything in flight, and an
        // approved candidate is added to it so later candidates in the same
        // pass are checked against it too — a blocked candidate is not, which
        // is why a third task can still run after the second is rejected.
        let in_progress_scoped: Vec<&Task> = all_tasks
            .iter()
            .filter(|task| {
                task.status == TaskStatus::InProgress
                    && task.scope.as_ref().is_some_and(|scope| !scope.is_empty())
            })
            .collect();
        let mut scope_index =
            ScopeIndex::new(in_progress_scoped.iter().map(|task| task.scope.as_deref()));

        let mut blocked: HashSet<String> = HashSet::new();
        for candidate in &candidates {
            let Some(scope) = candidate.scope.as_deref().filter(|scope| !scope.is_empty()) else {
                continue;
            };
            if scope_index.overlaps_any(Some(scope)) {
                let overlapper = in_progress_scoped
                    .iter()
                    .find(|task| scopes_overlap(Some(scope), task.scope.as_deref()));
                self.emit(OrchestratorEvent::TaskScopeOverlap {
                    task_id: candidate.id.clone(),
                    // The index can report an overlap against an *approved
                    // peer* rather than an in-progress task, in which case TS
                    // falls back to naming the candidate itself. Replicated.
                    overlapping_task_id: overlapper
                        .map(|task| task.id.clone())
                        .unwrap_or_else(|| candidate.id.clone()),
                    patterns: scope.to_vec(),
                });
                blocked.insert(candidate.id.clone());
            } else {
                scope_index.add(Some(scope));
            }
        }

        for task in &candidates {
            if blocked.contains(&task.id) {
                continue;
            }
            if let Err(err) = self.dispatch_task(state, &task.id, None).await {
                // A workspace failure is the one dispatch error that counts
                // against the attempt budget: it means the agent never got to
                // run, and retrying forever would spin.
                if matches!(err, Error::Domain(DomainError::Workspace { .. })) {
                    self.record_workspace_dispatch_failure(state, &task.id, &all_tasks, &err)
                        .await;
                }
                self.emit_error(&err, format!("dispatch task {}", task.id));
            }
        }
        Ok(())
    }

    /// Increment the attempt, apply `resolve_failure_status`, and either
    /// enqueue a retry or cascade-fail dependents. Every failure here is
    /// swallowed: being unable to *record* a dispatch error must not stop the
    /// remaining dispatches.
    async fn record_workspace_dispatch_failure(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        all_tasks: &[Task],
        err: &Error,
    ) {
        let Ok(Some(task)) = self.deps().task_store.get(task_id).await else {
            return;
        };
        if is_terminal(task.status) {
            return;
        }

        let updated = Task {
            attempts: task.attempts + 1,
            updated_at: self.deps().clock.now_iso(),
            ..task
        };
        let next_status = resolve_failure_status(&updated);
        let updated = Task {
            status: next_status,
            ..updated
        };
        if self.deps().task_store.save(&updated).await.is_err() {
            return;
        }

        if next_status == TaskStatus::Failed {
            // Patch the in-memory list rather than re-reading from disk: the
            // cascade walk only needs the new status.
            let patched: Vec<Task> = all_tasks
                .iter()
                .map(|task| {
                    if task.id == updated.id {
                        updated.clone()
                    } else {
                        task.clone()
                    }
                })
                .collect();
            self.inner.cached_tasks.invalidate_async().await;
            self.cascade_fail_dependents(
                &updated.id,
                &patched,
                &format!("dependency {} failed: {err}", updated.id),
            )
            .await;
        } else {
            let delay = calculate_retry_delay(
                updated.attempts.saturating_sub(1),
                self.config().scheduling.retry_base_delay_ms,
                self.config().scheduling.retry_max_delay_ms,
            );
            self.enqueue_retry(
                state,
                &updated.id,
                updated.attempts,
                delay,
                &err.to_string(),
            );
            let _ = self.save_state(state).await;
        }
    }

    /// `orch run <id>`: dispatch exactly one task.
    ///
    /// Every *other* dispatchable task is temporarily claimed so the shared
    /// dispatch path cannot opportunistically start it, then the original
    /// claimed set is restored. Without this a one-shot command would behave
    /// like `run --all` whenever more work happened to be ready.
    async fn dispatch_only_task(&self, state: &mut OrchestratorState, task_id: &str) -> Result<()> {
        let original_claimed = state.claimed.clone();
        let all_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        self.inner
            .single_task_runs
            .lock()
            .await
            .insert(task_id.to_owned());

        for task in &all_tasks {
            if task.id != task_id && is_dispatchable(task.status) {
                state.claimed.insert(task.id.clone());
            }
        }

        let result = self.dispatch_task(state, task_id, None).await;

        state.claimed = original_claimed;
        if !state.running.contains_key(task_id) {
            self.inner.single_task_runs.lock().await.remove(task_id);
        }
        self.save_state(state).await?;
        result
    }

    /// Claim → find an agent → prepare a workspace → build the prompt →
    /// create the run → transition → execute → start collecting.
    ///
    /// The claim is **persisted before the spawn** so a crash between the two
    /// leaves a recoverable claim rather than an invisible run.
    pub(super) async fn dispatch_task(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        prefetched: Option<Task>,
    ) -> Result<()> {
        if let Some(entry) = state.running.get(task_id) {
            return Err(Error::Domain(DomainError::TaskAlreadyRunning {
                task_id: task_id.to_owned(),
                run_id: entry.run_id.clone(),
                agent: entry.agent_id.clone(),
            }));
        }

        let task = match prefetched {
            Some(task) => task,
            None => self.deps().task_service.get(task_id).await?,
        };
        // It may have finished through another path while it sat in a queue.
        if !is_dispatchable(task.status) {
            return Ok(());
        }

        state.claimed.insert(task_id.to_owned());
        self.save_state(state).await?;

        match self.dispatch_claimed(state, task).await {
            Ok(()) => Ok(()),
            Err(err) => {
                // The process never launched, so the claim and the token are
                // both rollback-able.
                self.drop_cancel(task_id).await;
                state.claimed.shift_remove(task_id);
                self.save_state(state).await?;
                Err(err)
            }
        }
    }

    async fn dispatch_claimed(&self, state: &mut OrchestratorState, task: Task) -> Result<()> {
        let task_id = task.id.clone();

        let all_agents = self.inner.cached_agents.list().await?;
        let Some(agent) = self.deps().agent_service.find_best_agent(&task).await? else {
            if all_agents.is_empty() {
                return Err(Error::Domain(DomainError::NoAgents));
            }
            // No *idle* agent: the task keeps its place in the queue.
            state.claimed.shift_remove(&task_id);
            self.save_state(state).await?;
            return Ok(());
        };

        let prepared = self
            .deps()
            .workspace
            .prepare(&task, &agent, self.config())
            .await?;
        let workspace_path = prepared.path.clone();
        let worktree_branch = prepared.branch.clone();

        let attempt = task.attempts + 1;
        let retry_context = if attempt > 1 {
            self.deps()
                .run_service
                .get_last_failed_run_context(&task_id)
                .await?
                .map(|context| RetryContext {
                    previous_error: context.error,
                    previous_output: context.output,
                })
        } else {
            None
        };
        let _ = RETRY_CONTEXT_EVENTS;

        // Shared context, the mailbox drain and the goal are independent reads.
        let (shared_context, pending_messages, goal) = futures::future::join3(
            async {
                match self.deps().context_store.as_ref() {
                    Some(store) => store.get_all().await.ok(),
                    None => None,
                }
            },
            async {
                match self.deps().message_service.as_ref() {
                    Some(messages) => messages
                        .drain_mailbox(&agent.id, &task_id)
                        .await
                        .unwrap_or_default(),
                    None => Vec::new(),
                }
            },
            async {
                match (task.goal_id.as_deref(), self.inner.cached_goals.as_ref()) {
                    (Some(goal_id), Some(goals)) => goals.get(goal_id).await.ok().flatten(),
                    _ => None,
                }
            },
        )
        .await;

        let goal_context = match goal {
            Some(goal) => {
                let goal_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
                let progress = match self.deps().context_store.as_ref() {
                    Some(store) => store
                        .get(&format!("{}-progress", goal.id))
                        .await
                        .ok()
                        .flatten()
                        .map(|entry| entry.value),
                    None => None,
                };
                Some(GoalContext {
                    task_names: goal_tasks
                        .iter()
                        .filter(|t| t.goal_id.as_deref() == Some(goal.id.as_str()))
                        .map(|t| format!("[{}] {}", t.status, t.title))
                        .collect(),
                    id: goal.id,
                    title: goal.title,
                    description: goal.description,
                    status: goal.status,
                    progress,
                })
            }
            None => None,
        };

        let context = build_prompt_context(
            &task,
            &agent,
            attempt,
            &workspace_path,
            self.config(),
            BuildPromptOptions {
                all_agents: &all_agents,
                retry_context,
                shared_context: shared_context.as_ref(),
                feedback: task.feedback.clone(),
                messages: &pending_messages,
                goal: goal_context,
            },
        );

        // Split mode keeps the system half cacheable between runs; a
        // user-supplied `prompt.template` opts out of the split entirely.
        let prompt_config = self.config().prompt.clone().unwrap_or_default();
        let (mut prompt, mut system_prompt) = match prompt_config.template.as_deref() {
            Some(legacy) => (
                self.deps().templates.render(legacy, &context).await?,
                None::<String>,
            ),
            None => {
                let system = self
                    .deps()
                    .templates
                    .render(
                        prompt_config
                            .system_template
                            .as_deref()
                            .unwrap_or(DEFAULT_SYSTEM_TEMPLATE),
                        &context,
                    )
                    .await?;
                let user = self
                    .deps()
                    .templates
                    .render(
                        prompt_config
                            .user_template
                            .as_deref()
                            .unwrap_or(DEFAULT_USER_TEMPLATE),
                        &context,
                    )
                    .await?;
                (user, Some(system))
            }
        };

        // Library skills go on the system prompt, which is where they stay
        // cacheable; the legacy single-template path has nowhere else to put
        // them, so they are appended to the combined prompt.
        if let Some(loader) = self.deps().skill_loader.as_ref()
            && let Some(skills) = agent.config.skills.as_deref().filter(|s| !s.is_empty())
        {
            let block = loader.load_skills(skills).await?;
            if !block.is_empty() {
                match system_prompt.as_mut() {
                    Some(system) => {
                        system.push_str("\n\n");
                        system.push_str(&block);
                    }
                    None => {
                        prompt.push_str("\n\n");
                        prompt.push_str(&block);
                    }
                }
            }
        }

        let run = self
            .deps()
            .run_service
            .create(crate::run_service::CreateRunInput {
                task_id: task_id.clone(),
                agent_id: agent.id.clone(),
                attempt,
                prompt: prompt.clone(),
                workspace_path: workspace_path.clone(),
            })
            .await?;

        // A re-run of a terminal task needs its budget reset before the
        // `todo → in_progress` transition is legal.
        if task.status == TaskStatus::Failed || task.status == TaskStatus::Cancelled {
            self.deps().task_service.retry(&task_id).await?;
        }
        self.deps()
            .task_service
            .update_status(&task_id, TaskStatus::InProgress)
            .await?;
        self.deps().task_service.assign(&task_id, &agent.id).await?;
        self.deps()
            .task_service
            .increment_attempts(&task_id)
            .await?;

        // Persist the branch immediately, re-reading first so the
        // `in_progress` status set above is not overwritten. Without this a
        // later failure loses the branch and its work with it.
        if let Some(branch) = worktree_branch.as_deref()
            && let Some(mut fresh) = self.deps().task_store.get(&task_id).await?
        {
            let mut proof = fresh.proof.take().unwrap_or_else(TaskProof::default);
            proof.branch = Some(branch.to_owned());
            fresh.proof = Some(proof);
            fresh.workspace = Some(workspace_path.clone());
            self.deps().task_store.save(&fresh).await?;
        }

        self.deps()
            .agent_service
            .set_status(&agent.id, AgentStatus::Running)
            .await?;
        let mut agent_data = self.deps().agent_service.get(&agent.id).await?;
        agent_data.current_task = Some(task_id.clone());
        // Cleared on a successful dispatch so the TUI stops showing a stale
        // failure for an agent that is working again.
        agent_data.last_error = None;
        self.deps().agent_store.save(&agent_data).await?;

        let adapter = self.deps().adapters.require(&agent.adapter)?.clone();
        let cancel = self.register_cancel(&task_id).await;

        let mut env: BTreeMap<String, String> = agent
            .config
            .env
            .as_ref()
            .map(|env| {
                env.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        env.insert("ORCH_AGENT_ID".into(), agent.id.clone());
        env.insert("ORCH_AGENT_NAME".into(), agent.name.clone());
        env.insert("ORCH_TASK_ID".into(), task_id.clone());

        let handle = adapter.execute(ExecuteParams {
            prompt,
            system_prompt,
            workspace: std::path::PathBuf::from(&workspace_path),
            env,
            config: agent_data.config.clone(),
            cancel,
        })?;

        let pid = handle.pid;
        let now = self.deps().clock.now_iso();
        self.deps().run_service.start(&run.id, pid).await?;

        state.claimed.shift_remove(&task_id);
        state.running.insert(
            task_id.clone(),
            RunningEntry {
                run_id: run.id.clone(),
                agent_id: agent.id.clone(),
                task_id: task_id.clone(),
                pid,
                started_at: now.clone(),
                last_event_at: now,
            },
        );
        self.save_state(state).await?;

        // Registered *before* the task is spawned, so reconcile can never see
        // a running entry whose collector is not yet tracked.
        self.inner
            .active_collectors
            .lock()
            .await
            .insert(task_id.clone());
        let orchestrator = self.clone();
        let run_id = run.id.clone();
        let agent_id = agent.id.clone();
        tokio::spawn(async move {
            let collector_task_id = task_id.clone();
            if let Err(err) = orchestrator
                .collect_events(handle.events, &run_id, &task_id, &agent_id)
                .await
            {
                orchestrator.emit_error(err, format!("adapter execution for {task_id}"));
            }
            orchestrator
                .inner
                .active_collectors
                .lock()
                .await
                .remove(&collector_task_id);
        });

        Ok(())
    }

    /// Deduplicated by task id and bounded; on overflow the oldest goes.
    pub(super) fn enqueue_retry(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        attempt: u32,
        delay_ms: u64,
        error: &str,
    ) {
        if state
            .retry_queue
            .iter()
            .any(|retry| retry.task_id == task_id)
        {
            return;
        }
        if state.retry_queue.len() >= MAX_RETRY_QUEUE_SIZE {
            state.retry_queue.remove(0);
        }
        let due_at = orch_core::clock::iso_from_epoch_ms(
            self.deps().clock.now_ms().saturating_add(delay_ms as i64),
        );
        state.retry_queue.push(RetryEntry {
            task_id: task_id.to_owned(),
            attempt,
            due_at,
            error: error.to_owned(),
        });
    }

    /// Breadth-first walk over the reverse-dependency graph, failing every
    /// task that depended — directly or transitively — on a permanently failed
    /// one. Each cascaded task emits **both** `task:status_changed` and
    /// `task:cascade_failed` (PARITY.md §7.10).
    pub(super) async fn cascade_fail_dependents(
        &self,
        failed_task_id: &str,
        all_tasks: &[Task],
        reason: &str,
    ) {
        let mut reverse: HashMap<&str, Vec<&Task>> = HashMap::new();
        for task in all_tasks {
            for dependency in &task.depends_on {
                reverse.entry(dependency.as_str()).or_default().push(task);
            }
        }

        let mut queue: Vec<String> = vec![failed_task_id.to_owned()];
        let mut head = 0;
        let mut visited: HashSet<String> = HashSet::new();
        let mut cascaded_any = false;

        while head < queue.len() {
            let parent = queue[head].clone();
            head += 1;
            if !visited.insert(parent.clone()) {
                continue;
            }
            let Some(dependents) = reverse.get(parent.as_str()) else {
                continue;
            };

            let mut to_fail: Vec<(Task, TaskStatus)> = Vec::new();
            for task in dependents {
                if is_terminal(task.status) || visited.contains(&task.id) {
                    continue;
                }
                to_fail.push(((*task).clone(), task.status));
                queue.push(task.id.clone());
            }
            if to_fail.is_empty() {
                continue;
            }

            let now = self.deps().clock.now_iso();
            // Written through the store, not the service: a dependent in
            // `todo` has no legal transition to `failed`.
            futures::future::join_all(to_fail.iter().map(|(task, _)| {
                let failed = Task {
                    status: TaskStatus::Failed,
                    updated_at: now.clone(),
                    ..task.clone()
                };
                async move { self.deps().task_store.save(&failed).await }
            }))
            .await;

            for (task, previous_status) in &to_fail {
                self.emit(OrchestratorEvent::TaskStatusChanged {
                    task_id: task.id.clone(),
                    from: *previous_status,
                    to: TaskStatus::Failed,
                });
                self.emit(OrchestratorEvent::TaskCascadeFailed {
                    task_id: task.id.clone(),
                    failed_dependency_id: failed_task_id.to_owned(),
                    reason: reason.to_owned(),
                });
            }
            cascaded_any = true;
        }

        if cascaded_any {
            self.inner.cached_tasks.invalidate_async().await;
        }
    }
}
