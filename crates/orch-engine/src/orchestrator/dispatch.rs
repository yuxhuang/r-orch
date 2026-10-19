//! Dispatch: candidate selection, scope gating, and the claim → assign →
//! execute sequence.

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
        let mirror_peer = task
            .labels
            .iter()
            .find_map(|label| label.strip_prefix("remote:"))
            .filter(|_| mirror_controller(&task).is_some());
        let (mut prompt, mut system_prompt) = if let Some(controller) = mirror_peer {
            let from = task
                .labels
                .iter()
                .find_map(|label| label.strip_prefix("remote-from:"))
                .unwrap_or("cli");
            (
                task.description.clone(),
                Some(format!(
                    "You are {}{}\n\n## Remote execution\nYou are executing a task delegated by `{controller}`.\nPaths refer to this machine; controller files are not available. If a required input is missing, fail and say so.\nDo not use `orch task add`, `orch goal`, or `orch context` here: they are not visible to the requester.\nTo ask the requester, run `orch msg send {from}@{controller} \"…\"`.\nPut your result in your final message.",
                    agent.name,
                    agent
                        .role
                        .as_ref()
                        .map(|role| format!(" ({role})"))
                        .unwrap_or_default()
                )),
            )
        } else {
            match prompt_config.template.as_deref() {
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

        let handle = adapter.execute(ExecuteParams {
            prompt,
            system_prompt,
            workspace: std::path::PathBuf::from(&workspace_path),
            env,
            inherit_env,
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

/// Running entries that hold a local slot: everything except remote runs.
/// The controller instance a mirror task belongs to, from its `remote-ctl:`
/// label. `None` for ordinary local work.
fn mirror_controller(task: &Task) -> Option<&str> {
    task.labels
        .iter()
        .find_map(|label| label.strip_prefix("remote-ctl:"))
}

fn local_running(state: &OrchestratorState) -> usize {
    state
        .running
        .values()
        .filter(|entry| entry.remote.is_none())
        .count()
}
