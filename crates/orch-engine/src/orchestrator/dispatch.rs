//! Dispatch: candidate selection, scope gating, and the claim → assign →
//! execute sequence.

use std::collections::{BTreeMap, HashMap, HashSet};

use orch_core::adapter::ExecuteParams;
use orch_core::error::{Error, Result};
use orch_core::storage::{AgentStore, GoalStore, TaskStore};
use orch_core::template::{GoalContext, GoalTaskRow, RetryContext};
use orch_domain::{
    AgentConfig, AgentStatus, DomainError, OrchestratorEvent, OrchestratorState, PromptMode,
    RetryEntry, RunningEntry, ScopeIndex, Task, TaskProof, TaskStatus, calculate_retry_delay,
    is_blocked, is_dispatchable, is_terminal, resolve_failure_status, scopes_overlap,
};
use orch_infra::template::{
    BuildPromptOptions, RenderedPrompt, build_eco_prompt_context, build_prompt_context,
    render_prompt,
};

use super::tick::epoch_ms_from_iso;
use super::{MAX_RETRY_QUEUE_SIZE, Orchestrator};

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
    ///
    /// `max_concurrent_agents` caps **local** runs only. A remote run executes
    /// on its worker, which enforces its own cap, and each remote agent runs
    /// one task at a time, so remote dispatch is already bounded by the online
    /// remote agents. Once the local slots are full, the pass keeps going only
    /// while an online remote agent is idle, and offers candidates to remote
    /// agents alone.
    pub(super) async fn dispatch_all(&self, state: &mut OrchestratorState) -> Result<()> {
        let max_local = self.config().scheduling.max_concurrent_agents;
        // Loaded at most once per pass, and only when the local slots are full.
        let mut remote_agents: Option<Vec<String>> = None;
        if local_running(state) >= max_local
            && !self.has_idle_remote_agent(state, &mut remote_agents).await
        {
            return Ok(());
        }

        let all_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        let task_map: HashMap<String, Task> = all_tasks
            .iter()
            .map(|task| (task.id.clone(), task.clone()))
            .collect();

        // A `retrying` task still waiting out its retry delay is left to
        // `process_retry_queue`, which starts it once the entry comes due.
        let now_ms = self.deps().clock.now_ms();
        let mut candidates: Vec<Task> = all_tasks
            .iter()
            .filter(|task| {
                is_dispatchable(task.status)
                    && !is_blocked(task, &task_map)
                    && !state.running.contains_key(&task.id)
                    && !state.claimed.contains(&task.id)
                    && !(task.status == TaskStatus::Retrying
                        && retry_pending(state, &task.id, now_ms))
            })
            .cloned()
            .collect();

        // The scheduling contract: priority ascending,
        // then goal-linked first, then `updated_at` descending. Stable, so an
        // exact three-way tie keeps store order.
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.goal_id.is_none().cmp(&b.goal_id.is_none()))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });

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

        for task in &candidates {
            // A standalone signal stops the pass at the next candidate; the
            // dispatch already made settles normally.
            if self.is_interrupted() {
                break;
            }
            let remote_only = local_running(state) >= max_local;
            if remote_only && !self.has_idle_remote_agent(state, &mut remote_agents).await {
                break;
            }
            if let Some(scope) = task.scope.as_deref().filter(|scope| !scope.is_empty())
                && scope_index.overlaps_any(Some(scope))
            {
                let overlapper = in_progress_scoped
                    .iter()
                    .find(|other| scopes_overlap(Some(scope), other.scope.as_deref()));
                self.emit(OrchestratorEvent::TaskScopeOverlap {
                    task_id: task.id.clone(),
                    overlapping_task_id: overlapper
                        .map(|other| other.id.clone())
                        .unwrap_or_else(|| task.id.clone()),
                    patterns: scope.to_vec(),
                });
                continue;
            }
            if let Err(err) = self
                .dispatch_task_to(state, &task.id, None, remote_only)
                .await
            {
                if matches!(err, Error::Domain(DomainError::Workspace { .. })) {
                    self.record_workspace_dispatch_failure(state, &task.id, &all_tasks, &err)
                        .await;
                }
                self.emit_error(&err, format!("dispatch task {}", task.id));
            }
            // A task waiting for an eligible agent consumes neither a slot
            // nor a scope reservation. Keep looking down the ready queue.
            if state.running.contains_key(&task.id) {
                scope_index.add(task.scope.as_deref());
            }
        }
        Ok(())
    }

    /// Whether an online remote agent has no running entry. The directory is
    /// read once per pass into `cache`; a read error is reported and treated
    /// as "none", which is the old full-slots behaviour: stop dispatching.
    async fn has_idle_remote_agent(
        &self,
        state: &OrchestratorState,
        cache: &mut Option<Vec<String>>,
    ) -> bool {
        if cache.is_none() {
            let directory = orch_infra::storage::remote::RemoteAgentDirectory::new(
                orch_infra::storage::Paths::new(self.deps().project_root.clone()),
            );
            let online = match directory.list(Vec::new()).await {
                Ok(agents) => agents
                    .into_iter()
                    .filter(|agent| agent.remote.as_ref().is_some_and(|remote| remote.online))
                    .map(|agent| agent.id)
                    .collect(),
                Err(err) => {
                    self.emit_error(err, "list remote agents for dispatch".to_owned());
                    Vec::new()
                }
            };
            *cache = Some(online);
        }
        cache
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|id| !state.running.values().any(|entry| &entry.agent_id == id))
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
        let task = match self.deps().task_store.get(task_id).await {
            Ok(Some(task)) => task,
            // Gone: no attempt to account for.
            Ok(None) => return,
            // An unreadable task is not an absent one — see the same
            // distinction in `force_task_cancelled`. Silently skipping the
            // failure accounting would lose an attempt and leave the task
            // dispatchable again with no record of why.
            Err(err) => {
                self.emit_error(
                    err,
                    format!("read task {task_id} after a workspace failure"),
                );
                return;
            }
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
        self.dispatch_task_to(state, task_id, prefetched, false)
            .await
    }

    /// [`Self::dispatch_task`], optionally limited to remote agents: the
    /// scheduler's pass uses that once the local slots are full.
    async fn dispatch_task_to(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        prefetched: Option<Task>,
        remote_only: bool,
    ) -> Result<()> {
        // After a standalone signal nothing new is claimed (ruling 19).
        if self.is_interrupted() {
            return Ok(());
        }
        if let Some(entry) = state.running.get(task_id) {
            return Err(Error::Domain(DomainError::TaskAlreadyRunning {
                task_id: task_id.to_owned(),
                run_id: entry.run_id.clone(),
                agent: entry.agent_id.clone(),
            }));
        }
        // A run reconcile failed (a stall) may still be draining its stream.
        // The collector state and the cancel token are keyed by task id, so
        // a second run now would have the old collector's late completion
        // finish it. The task keeps its place until the stream ends.
        if self.inner.active_collectors.lock().await.contains(task_id) {
            return Ok(());
        }

        let task = match prefetched {
            Some(task) => task,
            None => self.deps().task_service.get(task_id).await?,
        };
        // It may have finished through another path while it sat in a queue.
        // A task held for the owner's approval waits like a blocked one.
        if !is_dispatchable(task.status) || task.awaits_approval() {
            return Ok(());
        }
        // A mirror belongs to the controller session that submitted it: only
        // that session can be told the run started, and only it will collect
        // the outcome. Running one for an absent controller burns an agent on
        // work nobody is waiting for, so the mirror waits instead (a mirror
        // whose controller never returns is cleaned up separately).
        if let Some(controller) = mirror_controller(&task)
            && self.inbound_controller().as_deref() != Some(controller)
        {
            tracing::debug!(
                task_id = %task.id,
                %controller,
                "Skipping remote mirror: its controller is not the current session"
            );
            return Ok(());
        }

        state.claimed.insert(task_id.to_owned());
        // Starting the task consumes its retry, whichever path started it
        // (the entry came due, or `run <id>` overrode the wait). A leftover
        // entry would swallow the next failure's retry, because
        // `enqueue_retry` deduplicates by task id.
        state.retry_queue.retain(|retry| retry.task_id != task_id);
        self.save_state(state).await?;

        match self.dispatch_claimed(state, task, remote_only).await {
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

    async fn dispatch_claimed(
        &self,
        state: &mut OrchestratorState,
        task: Task,
        remote_only: bool,
    ) -> Result<()> {
        let task_id = task.id.clone();

        let local_agents = self.inner.cached_agents.list().await?;
        let directory = orch_infra::storage::remote::RemoteAgentDirectory::new(
            orch_infra::storage::Paths::new(self.deps().project_root.clone()),
        );
        let all_agents = directory.list(local_agents).await?;
        let claimed_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
        let eligible_agents = all_agents
            .iter()
            .filter(|agent| {
                (!remote_only || agent.remote.is_some())
                    && !state
                        .running
                        .values()
                        .any(|entry| entry.agent_id == agent.id)
                    && !claimed_tasks.iter().any(|claimed| {
                        claimed.id != task.id
                            && state.claimed.contains(&claimed.id)
                            && claimed.assignee.as_deref() == Some(agent.id.as_str())
                    })
            })
            .cloned()
            .collect();
        let Some(agent) = crate::agent_service::AgentService::find_best_in(&task, eligible_agents)
        else {
            if all_agents.is_empty() {
                return Err(Error::Domain(DomainError::NoAgents));
            }
            // No *idle* agent: the task keeps its place in the queue.
            state.claimed.shift_remove(&task_id);
            self.save_state(state).await?;
            return Ok(());
        };

        let (workspace_path, worktree_branch) = if let Some(remote) = &agent.remote {
            (format!("remote:{}", remote.alias), None)
        } else {
            let prepared = self
                .deps()
                .workspace
                .prepare(&task, &agent, self.config())
                .await?;
            (prepared.path, prepared.branch)
        };

        let attempt = task.attempts + 1;
        let retry_context = if attempt > 1 {
            match self
                .deps()
                .run_service
                .get_last_failed_run_context(&task_id)
                .await?
            {
                Some(context) => {
                    // The source window is only vouched for when nothing can
                    // still append to it: the failed run is terminal, and no
                    // collector for this task is still draining its stream (a
                    // run failed by reconcile while its child was still
                    // writing). `dispatch_task_to` already waits for that
                    // collector; this keeps eco safe should another path
                    // dispatch. Otherwise eco keeps the full output.
                    let window_stable =
                        !self.inner.active_collectors.lock().await.contains(&task_id);
                    Some(RetryContext {
                        previous_error: context.error,
                        previous_output: context.output,
                        source_run_id: window_stable.then_some(context.run_id),
                    })
                }
                None => None,
            }
        } else {
            None
        };

        // Shared context, the mailbox drain and the goal are independent reads.
        let (shared_context, pending_messages, goal) = futures::future::join3(
            async {
                match self.deps().context_store.as_ref() {
                    Some(store) => store.get_all().await.ok(),
                    None => None,
                }
            },
            async {
                if agent.remote.is_some() {
                    return Vec::new();
                }
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
                let linked: Vec<&Task> = goal_tasks
                    .iter()
                    .filter(|t| t.goal_id.as_deref() == Some(goal.id.as_str()))
                    .collect();
                Some(GoalContext {
                    task_names: linked
                        .iter()
                        .map(|t| format!("[{}] {}", t.status, t.title))
                        .collect(),
                    task_rows: linked
                        .iter()
                        .map(|t| GoalTaskRow {
                            id: t.id.clone(),
                            status: t.status,
                            title: t.title.clone(),
                        })
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

        // Split mode keeps the system half cacheable between runs; a
        // user-supplied `prompt.template` opts out of the split entirely.
        let mirror_peer = task
            .labels
            .iter()
            .find_map(|label| label.strip_prefix("remote:"))
            .filter(|_| mirror_controller(&task).is_some());

        // One prompt mode per dispatch, captured here and used for both the
        // render and the invocation config. Remote execution and mirrors
        // bypass eco entirely and read nothing.
        let captured_mode = if agent.remote.is_none() && mirror_peer.is_none() {
            let project_mode = self.current_project_prompt_mode().await?;
            let mode = agent.config.prompt_mode.unwrap_or(project_mode);
            if mode == PromptMode::Eco {
                // Hand-edited files bypass the mutation-time check.
                let mut effective = self.config().clone();
                effective.defaults.agent.prompt_mode = project_mode;
                crate::prompt_mode::check_agent_candidate(
                    &effective,
                    &agent.id,
                    &agent.name,
                    &agent.config,
                )?;
            }
            Some(mode)
        } else {
            None
        };

        let options = BuildPromptOptions {
            all_agents: &all_agents,
            retry_context,
            shared_context: shared_context.as_ref(),
            feedback: task.feedback.clone(),
            messages: &pending_messages,
            goal: goal_context,
        };
        let context = if captured_mode == Some(PromptMode::Eco) {
            build_eco_prompt_context(
                &task,
                &agent,
                attempt,
                &workspace_path,
                self.config(),
                options,
            )
        } else {
            build_prompt_context(
                &task,
                &agent,
                attempt,
                &workspace_path,
                self.config(),
                options,
            )
        };

        let skills = match (
            self.deps().skill_loader.as_ref(),
            agent.config.skills.as_deref().filter(|s| !s.is_empty()),
        ) {
            (Some(loader), Some(skills)) => Some(loader.load_skills(skills).await?),
            _ => None,
        };

        let RenderedPrompt {
            prompt,
            system_prompt,
        } = if let Some(controller) = mirror_peer {
            let from = task
                .labels
                .iter()
                .find_map(|label| label.strip_prefix("remote-from:"))
                .unwrap_or("cli");
            let mut system = format!(
                "You are {}{}\n\n## Remote execution\nYou are executing a task delegated by `{controller}`.\nPaths refer to this machine; controller files are not available. If a required input is missing, fail and say so.\nDo not use `orch task add`, `orch goal`, or `orch context` here: they are not visible to the requester.\nTo ask the requester, run `orch msg send {from}@{controller} \"…\"`.\nPut your result in your final message.",
                agent.name,
                agent
                    .role
                    .as_ref()
                    .map(|role| format!(" ({role})"))
                    .unwrap_or_default()
            );
            if let Some(block) = skills.as_deref().filter(|block| !block.is_empty()) {
                system.push_str("\n\n");
                system.push_str(block);
            }
            RenderedPrompt {
                prompt: task.description.clone(),
                system_prompt: Some(system),
            }
        } else {
            render_prompt(
                self.deps().templates.as_ref(),
                self.config().prompt.as_ref(),
                &context,
                skills.as_deref(),
            )
            .await?
        };

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

        // A task explicitly reassigned back to this machine cannot carry a
        // worker branch into local merge-back (including shared workspaces).
        if agent.remote.is_none()
            && let Some(mut fresh) = self.deps().task_store.get(&task_id).await?
            && fresh
                .proof
                .as_ref()
                .is_some_and(|proof| proof.remote.is_some())
        {
            fresh.proof = None;
            fresh.workspace = None;
            self.deps().task_store.save(&fresh).await?;
        }

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

        if let Some(remote) = &agent.remote {
            let now = self.deps().clock.now_iso();
            // The run stays `preparing` until the worker says it is running.
            // Starting it here claimed a peer had begun work that had not
            // even been written to the socket.
            let reference = orch_domain::RemoteRunRef::new(&remote.alias, &run.id);
            state.claimed.shift_remove(&task_id);
            state.running.insert(
                task_id.clone(),
                RunningEntry {
                    run_id: run.id.clone(),
                    agent_id: agent.id.clone(),
                    task_id: task_id.clone(),
                    pid: 0,
                    started_at: now.clone(),
                    last_event_at: now,
                    remote: Some(reference),
                },
            );
            // Reservation and execution key must survive a controller crash
            // before the worker is ever allowed to execute the request.
            self.save_state(state).await?;
            let cancel = self.register_cancel(&task_id).await;
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
            let auto_approve = task.is_autonomous()
                || agent.config.approval_policy == Some(orch_domain::ApprovalPolicy::Auto);
            // Enqueue before releasing the state lock, so a concurrent cancel
            // cannot overtake initial submission. No network I/O occurs here.
            let handle = self.remote_bridge().submit(request);
            self.spawn_remote_collector(handle, &run.id, &task_id, &agent.id, auto_approve, cancel)
                .await;
            return Ok(());
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
        // Work submitted by a remote peer must not run with this machine's
        // credentials (security review F2). The agent's own `config.env` is
        // the owner's explicit choice and is still applied on top.
        let inherit_env = if mirror_controller(&task).is_some() {
            orch_core::process::EnvInheritance::WithoutSecrets
        } else {
            orch_core::process::EnvInheritance::Full
        };

        // The authored definition stays as read; only the invocation copy
        // carries the captured mode and the prepared effort.
        let mut invocation_config = agent_data.config.clone();
        if let Some(mode) = captured_mode {
            invocation_config.prompt_mode = Some(mode);
        }
        let mut params = ExecuteParams {
            prompt,
            system_prompt,
            workspace: std::path::PathBuf::from(&workspace_path),
            env,
            inherit_env,
            config: invocation_config,
            cancel,
        };

        // Bounded effort preparation (OpenCode's wire check; identity for
        // every other adapter) is awaited before the model child exists. A
        // failure here, or in the spawn itself, goes through the run-failure
        // path: run failed, agent idle with `last_error`, task retried or
        // failed by the normal policy — never an agent stuck `running`.
        //
        // A standalone signal (ruling 19) cancels the preparation rather than
        // dropping it: the probe kills and reaps its process group and returns,
        // and the dispatch then fails here, before any model child, so the
        // critical section still ends with every record settled.
        let prepared = {
            let cancel = params.cancel.clone();
            let interrupt = self.inner.interrupt.clone();
            let preparation = adapter.prepare_effort(&params);
            tokio::pin!(preparation);
            tokio::select! {
                biased;
                prepared = &mut preparation => prepared,
                () = interrupt.cancelled() => {
                    cancel.cancel();
                    preparation.await
                }
            }
        };
        if self.is_interrupted() {
            let message = "dispatch interrupted by a signal before the model started";
            let kind = orch_domain::classify_adapter_error(message, None);
            self.fail_before_spawn(state, &task_id, &run.id, &agent.id, message, kind)
                .await;
            return Err(Error::other(message));
        }
        let preparation =
            match prepared.and_then(|prepared| check_preparation(&params.config, prepared)) {
                Ok(preparation) => preparation,
                Err(err) => {
                    let message = format!("effort preparation failed: {err}");
                    self.fail_before_spawn(state, &task_id, &run.id, &agent.id, &message, err.kind)
                        .await;
                    return Err(Error::other(message));
                }
            };
        params.config = preparation.config;
        // Effort diagnostics come from one source per adapter (lead ruling 8):
        // OpenCode's preparation, whose wire check can change the outcome,
        // or, for every other adapter, the pure dispatch plan of the prepared
        // config. Their identity preparation carries no diagnostics, so the
        // two never repeat a note. A plan rejection is the same error
        // `execute` would return, raised here before any model child.
        let mut diagnostics = preparation.diagnostics;
        if !PREPARATION_OWNS_EFFORT_DIAGNOSTICS.contains(&adapter.kind()) {
            match orch_infra::adapters::effort_diagnostics(adapter.kind(), &params.config) {
                Ok(planned) => diagnostics.extend(planned),
                Err(err) => {
                    self.fail_before_spawn(
                        state,
                        &task_id,
                        &run.id,
                        &agent.id,
                        &err.to_string(),
                        err.kind,
                    )
                    .await;
                    return Err(err.into());
                }
            }
        }
        self.report_effort_diagnostics(&run.id, &agent.id, &diagnostics);

        let handle = match adapter.execute(params) {
            Ok(handle) => handle,
            Err(err) => {
                self.fail_before_spawn(
                    state,
                    &task_id,
                    &run.id,
                    &agent.id,
                    &err.to_string(),
                    err.kind,
                )
                .await;
                return Err(err.into());
            }
        };

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
                remote: None,
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
            // `finishing` first, so it stays a subset of `active`.
            orchestrator
                .inner
                .finishing_collectors
                .lock()
                .await
                .remove(&collector_task_id);
            orchestrator
                .inner
                .active_collectors
                .lock()
                .await
                .remove(&collector_task_id);
        });

        Ok(())
    }

    /// The project prompt mode for the dispatch being assembled.
    ///
    /// Re-read from `config.yml` immediately before prompt assembly, so a
    /// mode switch by another process (the TUI's F3, `orch config set`)
    /// applies to the next dispatch without a restart. Only this key is taken
    /// from the fresh read; scheduling and every other setting keep the
    /// startup snapshot. A read or parse failure is a configuration error
    /// before any run exists — never a silent fall back to normal. Without a
    /// config store (unit tests), the startup config is used.
    async fn current_project_prompt_mode(&self) -> Result<PromptMode> {
        let Some(store) = self.config_store() else {
            return Ok(self.config().defaults.agent.prompt_mode);
        };
        match store.read().await {
            Ok(config) => Ok(config.defaults.agent.prompt_mode),
            Err(err) => Err(Error::invalid_arguments(format!(
                "configuration preflight: cannot read defaults.agent.prompt_mode: {err}"
            ))),
        }
    }

    /// Finalize a dispatch that failed after its run was created and its
    /// agent marked running, but before a model child exists (effort
    /// preparation or spawn failure). It goes through the same failure
    /// handler as a run whose stream fails immediately: the run is recorded
    /// failed, the agent returns to idle with `last_error`, the task is
    /// retried or failed by the existing policy, and the slot is released.
    async fn fail_before_spawn(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        run_id: &str,
        agent_id: &str,
        error: &str,
        error_kind: orch_domain::AdapterErrorKind,
    ) {
        let now = self.deps().clock.now_iso();
        let entry = RunningEntry {
            run_id: run_id.to_owned(),
            agent_id: agent_id.to_owned(),
            task_id: task_id.to_owned(),
            pid: 0,
            started_at: now.clone(),
            last_event_at: now,
            remote: None,
        };
        state.running.insert(task_id.to_owned(), entry.clone());
        if let Err(err) = self
            .handle_run_failure_locked(state, task_id, &entry, error, Some(error_kind))
            .await
        {
            self.emit_error(err, format!("record pre-spawn failure for {task_id}"));
            // The handler reports its own failures; the slot and the agent
            // must still be released.
            state.running.shift_remove(task_id);
            if let Err(err) = self
                .deps()
                .agent_service
                .set_status(agent_id, AgentStatus::Idle)
                .await
            {
                self.emit_error(
                    err,
                    format!("reset agent {agent_id} after pre-spawn failure"),
                );
            }
            let _ = self.save_state(state).await;
        }
    }

    /// Effort warnings (`legacy-ignored`, clamps, capability uncertainty,
    /// the Codex managed-model coupling) are operator information, not
    /// provider output (design D5.2, lead ruling 8). Only warnings arrive
    /// here: the informational `request-only` note is filtered at its source
    /// (lead ruling 20) and stays on `agent add`/`show`. Each warning goes to
    /// exactly one surface before the model child exists: a non-fatal
    /// `orchestrator:error` on the bus, with
    /// `context` starting [`EFFORT_DIAGNOSTIC_CONTEXT`] and `error` the note
    /// verbatim. `serve` logs it as a warn-level record (stderr or
    /// `--log-file`); `run --watch` prints it as a ⚠ line.
    /// It is never a run event, so retry `previous_output`, the collector's
    /// `last_agent_message` and the task's `final_result` cannot see it.
    fn report_effort_diagnostics(&self, run_id: &str, agent_id: &str, diagnostics: &[String]) {
        for diagnostic in diagnostics {
            self.emit_error(
                diagnostic,
                format!("{EFFORT_DIAGNOSTIC_CONTEXT} for run {run_id} (agent {agent_id})"),
            );
        }
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
    /// `task:cascade_failed`.
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
            //
            // **Sequential, not concurrent.** Each save appends to
            // `_index.json`, whose entry order is observable and is the order
            // the saves *complete* in. Concurrent saves would race on that
            // order — `join_all` on a multi-threaded runtime genuinely
            // interleaves — so awaiting in order keeps it deterministic. The
            // cost is nothing: a cascade is a handful of tasks.
            for (task, _) in &to_fail {
                let failed = Task {
                    status: TaskStatus::Failed,
                    updated_at: now.clone(),
                    ..task.clone()
                };
                let _ = self.deps().task_store.save(&failed).await;
            }

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

/// The `orchestrator:error` context prefix of an effort diagnostic; the
/// rest names the run and agent.
pub(crate) const EFFORT_DIAGNOSTIC_CONTEXT: &str = "effort preparation";

/// Adapters whose `prepare_effort` returns the complete diagnostics for a
/// dispatch (its wire check can change the plan), so the pure plan must not
/// be consulted again for them.
const PREPARATION_OWNS_EFFORT_DIAGNOSTICS: &[&str] = &["opencode"];

/// Enforce the preparation contract at its only production caller: the
/// prepared copy may differ from the invocation config only by a cleared
/// generic effort (the legacy-ignore outcome). Anything else is a contract
/// violation reported before the spawn, never silently executed.
fn check_preparation(
    invocation: &AgentConfig,
    prepared: orch_core::EffortPreparation,
) -> std::result::Result<orch_core::EffortPreparation, orch_core::AdapterError> {
    let effort_ok = prepared.config.effort.is_none() || prepared.config.effort == invocation.effort;
    let mut rest = prepared.config.clone();
    rest.effort = invocation.effort;
    if effort_ok && &rest == invocation {
        Ok(prepared)
    } else {
        Err(orch_core::AdapterError::new(
            "adapter effort preparation changed more than the generic effort",
            None,
        ))
    }
}

/// Running entries that hold a local slot: everything except remote runs.
/// The controller instance a mirror task belongs to, from its `remote-ctl:`
/// label. `None` for ordinary local work.
fn mirror_controller(task: &Task) -> Option<&str> {
    task.labels
        .iter()
        .find_map(|label| label.strip_prefix("remote-ctl:"))
}

/// Whether `task_id` has a retry queued that is not yet due. An entry whose
/// `due_at` does not parse never comes due in `process_retry_queue`, so it
/// does not hold the task back here either: that would strand it in
/// `retrying` for good.
fn retry_pending(state: &OrchestratorState, task_id: &str, now_ms: i64) -> bool {
    state.retry_queue.iter().any(|retry| {
        retry.task_id == task_id
            && epoch_ms_from_iso(&retry.due_at).is_some_and(|due_at| now_ms < due_at)
    })
}

fn local_running(state: &OrchestratorState) -> usize {
    state
        .running
        .values()
        .filter(|entry| entry.remote.is_none())
        .count()
}
