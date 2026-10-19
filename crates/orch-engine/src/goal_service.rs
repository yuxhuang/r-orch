//! Goal service.
//!
//! Goals are persistent objectives that drive autonomous agent work. The state
//! machine lives in [`orch_domain::GoalStatus`]; what is here
//! is the achieved guard and the autonomous side effects, both of which are
//! normative and both of which are deliberately **silent** on failure.
//!
//! # Optional dependencies
//!
//! `agent_service`, `task_service` and `context_store` are optional,
//! and the CLI really does construct a `GoalService` without them (`orch goal
//! list` does not need a task service). Every path that uses one degrades to a no-op rather than
//! erroring, so `None` is a supported configuration and not a half-built
//! service.
//!
//! # Why the side effects swallow everything
//!
//! A goal outlives its assignee: the agent can be deleted while the goal
//! stays. Toggling autonomous mode is therefore best-effort — an "agent not
//! found" must not fail `goal update`, so every such toggle swallows the error
//! (including "agent not found") and carries on. The
//! `*_swallows_a_set_autonomous_failure` tests pin this.

use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{ContextStore, GoalFilter, GoalStore, TaskFilter};
use orch_domain::{
    CreateGoalInput, DomainError, Goal, GoalStatus, OrchestratorEvent, Task, TaskStatus,
    is_goal_terminal, is_terminal,
};

use crate::agent_service::AgentService;
use crate::events::EventBus;
use crate::task_service::TaskService;

/// The fields `goal update` can change.
///
/// `None` means "not supplied", so only the fields the user named are touched.
/// For `assignee`, `Some("")` **clears** the assignee.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateGoalFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub assignee: Option<String>,
}

/// Goal lifecycle, the achieved guard, and autonomous-mode side effects.
pub struct GoalService {
    goals: Arc<dyn GoalStore>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    agent_service: Option<Arc<AgentService>>,
    task_service: Option<Arc<TaskService>>,
    context_store: Option<Arc<dyn ContextStore>>,
}

impl GoalService {
    /// The three `Option` dependencies are optional — see the module docs.
    pub fn new(
        goals: Arc<dyn GoalStore>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        agent_service: Option<Arc<AgentService>>,
        task_service: Option<Arc<TaskService>>,
        context_store: Option<Arc<dyn ContextStore>>,
    ) -> Self {
        GoalService {
            goals,
            events,
            clock,
            ids,
            agent_service,
            task_service,
            context_store,
        }
    }

    /// Create an `active` goal. An `assignee` switches that agent into
    /// autonomous mode.
    pub async fn create(&self, input: CreateGoalInput) -> Result<Goal> {
        if input.title.trim().is_empty() {
            return Err(Error::invalid_arguments("Goal title is required"));
        }

        let now = self.clock.now_iso();
        let goal = Goal {
            id: self.ids.generate("goal"),
            title: input.title.trim().to_owned(),
            description: input.description.as_deref().unwrap_or("").trim().to_owned(),
            status: GoalStatus::Active,
            // Not trimmed and not validated against the agent store: the id
            // is taken as given, and an unknown one simply means the
            // autonomous toggle below finds nothing.
            assignee: input.assignee,
            created_at: now.clone(),
            updated_at: Some(now),
        };

        self.goals.save(&goal).await?;
        self.events.emit(OrchestratorEvent::GoalCreated {
            goal_id: goal.id.clone(),
            title: goal.title.clone(),
        });

        if let Some(assignee) = assignee_of(&goal) {
            self.enable_autonomous(assignee).await;
        }

        Ok(goal)
    }

    pub async fn list(&self, filter: &GoalFilter) -> Result<Vec<Goal>> {
        self.goals.list(filter).await
    }

    pub async fn get(&self, id: &str) -> Result<Goal> {
        self.goals
            .get(id)
            .await?
            .ok_or_else(|| Error::Domain(DomainError::GoalNotFound(id.to_owned())))
    }

    /// Move a goal through the state machine, running the achieved guard and
    /// the autonomous side effects.
    ///
    /// `force` only affects the `achieved` guard; it is the `--force` flag of
    /// `orch goal achieve`.
    pub async fn update_status(
        &self,
        id: &str,
        new_status: GoalStatus,
        force: bool,
    ) -> Result<Goal> {
        let mut goal = self.get(id).await?;
        let old_status = goal.status;

        if !old_status.can_transition_to(new_status) {
            return Err(Error::invalid_arguments(format!(
                "Cannot transition goal from '{old_status}' to '{new_status}'"
            )));
        }

        if new_status == GoalStatus::Achieved {
            self.enforce_achieved_guard(id, force).await?;
        }

        goal.status = new_status;
        goal.updated_at = Some(self.clock.now_iso());
        self.goals.save(&goal).await?;

        self.events.emit(OrchestratorEvent::GoalStatusChanged {
            goal_id: id.to_owned(),
            from: old_status,
            to: new_status,
        });

        if let Some(assignee) = assignee_of(&goal) {
            if new_status == GoalStatus::Paused {
                // Order matters: autonomous is disabled first so the
                // orchestrator cannot dispatch a replacement task in between.
                self.maybe_disable_autonomous(assignee).await;
                self.cancel_pending_autonomous_tasks(assignee).await;
            } else if new_status == GoalStatus::Active && old_status == GoalStatus::Paused {
                self.enable_autonomous(assignee).await;
            } else if is_goal_terminal(new_status) {
                self.maybe_disable_autonomous(assignee).await;
            }
        }

        Ok(goal)
    }

    /// Edit a goal's fields. Reassigning enables autonomous mode on the new
    /// agent and maybe-disables it on the old one.
    pub async fn update(&self, id: &str, fields: &UpdateGoalFields) -> Result<Goal> {
        let mut goal = self.get(id).await?;
        let old_assignee = goal.assignee.clone();

        if let Some(title) = &fields.title {
            if title.trim().is_empty() {
                return Err(Error::invalid_arguments("Goal title cannot be empty"));
            }
            goal.title = title.trim().to_owned();
        }
        if let Some(description) = &fields.description {
            goal.description = description.trim().to_owned();
        }
        if let Some(assignee) = &fields.assignee {
            // `--assignee ""` clears it, which is the only way to unassign.
            goal.assignee = Some(assignee.clone()).filter(|a| !a.is_empty());
        }

        goal.updated_at = Some(self.clock.now_iso());
        self.goals.save(&goal).await?;
        self.events.emit(OrchestratorEvent::GoalUpdated {
            goal_id: id.to_owned(),
        });

        // The two toggles concern different agents, so running them in
        // sequence changes nothing observable.
        if goal.assignee != old_assignee {
            if let Some(new_assignee) = assignee_of(&goal) {
                self.enable_autonomous(new_assignee).await;
            }
            if let Some(old) = old_assignee.as_deref().filter(|a| !a.is_empty()) {
                self.maybe_disable_autonomous(old).await;
            }
        }

        Ok(goal)
    }

    /// Delete a goal. Child tasks keep their now-dangling `goalId`; deletion is not
    /// cascading.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let goal = self.get(id).await?;
        let assignee = goal.assignee.clone();

        self.goals.delete(id).await?;
        self.events.emit(OrchestratorEvent::GoalDeleted {
            goal_id: id.to_owned(),
        });

        if let Some(assignee) = assignee.as_deref().filter(|a| !a.is_empty()) {
            self.maybe_disable_autonomous(assignee).await;
        }

        Ok(())
    }

    /// Tasks linked to this goal, or an empty list when no task service is
    /// wired.
    pub async fn list_tasks_for_goal(&self, goal_id: &str) -> Result<Vec<Task>> {
        match &self.task_service {
            Some(tasks) => tasks.list(&TaskFilter::with_goal(goal_id)).await,
            None => Ok(Vec::new()),
        }
    }

    /// The agent-written progress note for a goal, read from the shared
    /// context under the `<goalId>-progress` key.
    pub async fn get_progress_report(&self, goal_id: &str) -> Result<Option<String>> {
        let Some(context) = &self.context_store else {
            return Ok(None);
        };
        Ok(context
            .get(&format!("{goal_id}-progress"))
            .await?
            .map(|entry| entry.value))
    }

    /// The achieved guard.
    ///
    /// Tasks labelled `autonomous` are excluded: they are the *mechanism* for
    /// achieving the goal, and the side effects below cancel them afterwards,
    /// so counting them would deadlock every autonomous goal.
    async fn enforce_achieved_guard(&self, id: &str, force: bool) -> Result<()> {
        let Some(tasks) = &self.task_service else {
            return Ok(());
        };

        let linked = tasks.list(&TaskFilter::with_goal(id)).await?;
        let pending: Vec<Task> = linked
            .into_iter()
            .filter(|t| !is_terminal(t.status) && !t.is_autonomous())
            .collect();
        if pending.is_empty() {
            return Ok(());
        }

        if !force {
            return Err(pending_tasks_error(id, &pending));
        }

        // `in_progress` tasks own a live OS process that only the orchestrator
        // can kill, so force cancels everything else and then still refuses.
        for task in pending
            .iter()
            .filter(|t| t.status != TaskStatus::InProgress)
        {
            // A task that refuses to cancel must not abort the whole force, so
            // per-task failures are dropped.
            let _ = tasks.cancel(&task.id).await;
        }

        let running: Vec<Task> = pending
            .into_iter()
            .filter(|t| t.status == TaskStatus::InProgress)
            .collect();
        if running.is_empty() {
            return Ok(());
        }
        Err(pending_tasks_error(id, &running))
    }

    /// Switch an agent into autonomous mode, ignoring failures.
    async fn enable_autonomous(&self, agent_id: &str) {
        let Some(agents) = &self.agent_service else {
            return;
        };
        let _ = agents.set_autonomous(agent_id, true).await;
    }

    /// Switch an agent out of autonomous mode, but only when this was its last
    /// `active` goal. Ignores failures, the goal-store read included: a failed
    /// read means "leave autonomous alone", never an error to the caller.
    async fn maybe_disable_autonomous(&self, agent_id: &str) {
        let Some(agents) = &self.agent_service else {
            return;
        };
        if let Ok(false) = self.has_active_goal_for_agent(agent_id).await {
            let _ = agents.set_autonomous(agent_id, false).await;
        }
    }

    async fn has_active_goal_for_agent(&self, agent_id: &str) -> Result<bool> {
        Ok(self
            .goals
            .list(&GoalFilter::with_status(GoalStatus::Active))
            .await?
            .iter()
            .any(|g| g.assignee.as_deref() == Some(agent_id)))
    }

    /// Cancel the agent's dispatchable (`todo`/`retrying`) autonomous tasks, so
    /// pausing a goal does not leave work queued that would start the moment
    /// autonomous mode came back.
    ///
    /// Best-effort: a failed
    /// listing cancels nothing at all rather than cancelling part of the set.
    async fn cancel_pending_autonomous_tasks(&self, agent_id: &str) {
        let Some(tasks) = &self.task_service else {
            return;
        };

        let mut pending = Vec::new();
        for status in [TaskStatus::Todo, TaskStatus::Retrying] {
            match tasks.list(&TaskFilter::with_status(status)).await {
                Ok(listed) => pending.extend(
                    listed
                        .into_iter()
                        .filter(|t| t.assignee.as_deref() == Some(agent_id) && t.is_autonomous()),
                ),
                Err(_) => return,
            }
        }

        for task in pending {
            let _ = tasks.cancel(&task.id).await;
        }
    }
}

/// The assignee, treating `Some("")` as unassigned.
///
/// Every side effect is guarded on the assignee being non-empty;
/// a stored `assignee: ""` must therefore toggle
/// nothing.
fn assignee_of(goal: &Goal) -> Option<&str> {
    goal.assignee.as_deref().filter(|a| !a.is_empty())
}

/// `GoalHasPendingTasksError` with a `<id> (<status>), …` summary of every
/// pending task; `the_pending_task_error_names_every_task_and_status` pins the
/// text.
fn pending_tasks_error(goal_id: &str, pending: &[Task]) -> Error {
    let summary = pending
        .iter()
        .map(|t| format!("{} ({})", t.id, t.status))
        .collect::<Vec<_>>()
        .join(", ");
    Error::Domain(DomainError::GoalHasPendingTasks {
        id: goal_id.to_owned(),
        count: pending.len(),
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::storage::{AgentStore, TaskStore};
    use orch_core::test_support::{
        AgentStoreCall, FakeAgentStore, FakeContextStore, FakeGoalStore, FakeTaskStore, FixedClock,
        SeqIdGenerator, make_agent, make_task,
    };
    use orch_domain::{AUTONOMOUS_LABEL, Agent, OrchestratorEventType, default_config};

    const SEEDED_AT: &str = "2025-01-01T00:00:00Z";

    struct Harness {
        service: GoalService,
        goals: Arc<FakeGoalStore>,
        agents: Arc<FakeAgentStore>,
        tasks: Arc<FakeTaskStore>,
        context: Arc<FakeContextStore>,
        bus: Arc<EventBus>,
    }

    impl Harness {
        /// Every optional dependency wired.
        fn full() -> Harness {
            Harness::new(true, true, true)
        }

        /// `new GoalService(goalStore, eventBus, agentService)`.
        fn with_agent_service() -> Harness {
            Harness::new(true, false, false)
        }

        /// `new GoalService(goalStore, eventBus, undefined, taskService)`.
        fn with_task_service() -> Harness {
            Harness::new(false, true, false)
        }

        /// `new GoalService(goalStore, eventBus)` — the graceful-no-op config.
        fn bare() -> Harness {
            Harness::new(false, false, false)
        }

        fn new(agent_service: bool, task_service: bool, context_store: bool) -> Harness {
            let goals = Arc::new(FakeGoalStore::new());
            let agents = Arc::new(FakeAgentStore::new());
            let tasks = Arc::new(FakeTaskStore::new());
            let context = Arc::new(FakeContextStore::new());
            let bus = Arc::new(EventBus::new());
            let clock: Arc<dyn Clock> = Arc::new(FixedClock::new());
            let ids: Arc<dyn IdGenerator> = Arc::new(SeqIdGenerator::new());

            let service = GoalService::new(
                Arc::clone(&goals) as Arc<dyn GoalStore>,
                Arc::clone(&bus),
                Arc::clone(&clock),
                Arc::clone(&ids),
                agent_service.then(|| {
                    Arc::new(AgentService::new(
                        Arc::clone(&agents) as Arc<dyn AgentStore>,
                        Arc::new(orch_core::test_support::FakeStateStore::new())
                            as Arc<dyn orch_core::StateStore>,
                        Arc::clone(&bus),
                        default_config(),
                        Arc::clone(&ids),
                    ))
                }),
                task_service.then(|| {
                    Arc::new(TaskService::new(
                        Arc::clone(&tasks) as Arc<dyn TaskStore>,
                        Arc::clone(&agents) as Arc<dyn AgentStore>,
                        Arc::clone(&bus),
                        default_config(),
                        Arc::clone(&clock),
                        Arc::clone(&ids),
                        None,
                    ))
                }),
                context_store.then(|| Arc::clone(&context) as Arc<dyn ContextStore>),
            );

            Harness {
                service,
                goals,
                agents,
                tasks,
                context,
                bus,
            }
        }

        /// The stand-in for `expect(setAutonomous).toHaveBeenCalledTimes(n)`:
        /// `set_autonomous` persists the flag and nothing else in these tests
        /// writes an agent, so the agent-store write count *is* the call count.
        /// Counting writes rather than events also keeps these assertions
        /// independent of how `AgentService` announces the toggle.
        fn autonomous_calls(&self) -> usize {
            self.agents
                .calls
                .count_matching(|c| matches!(c, AgentStoreCall::Save(_)))
        }

        /// The persisted autonomous flag — `None` when it was never written.
        fn autonomous_of(&self, agent_id: &str) -> Option<bool> {
            self.agents.peek(agent_id).and_then(|a| a.autonomous)
        }

        /// Seed an agent whose autonomous flag starts at `autonomous`, so every
        /// toggle below is a real change and therefore observable.
        fn seed_agent(&self, id: &str, autonomous: bool) {
            self.agents.seed(Agent {
                id: id.to_owned(),
                name: id.to_owned(),
                autonomous: Some(autonomous),
                ..make_agent()
            });
        }
    }

    /// A goal fixture.
    fn goal(id: &str) -> Goal {
        Goal {
            id: id.to_owned(),
            title: "Test goal".into(),
            description: String::new(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: SEEDED_AT.into(),
            updated_at: Some(SEEDED_AT.into()),
        }
    }

    fn assigned_goal(id: &str, assignee: &str) -> Goal {
        Goal {
            assignee: Some(assignee.to_owned()),
            ..goal(id)
        }
    }

    /// A task fixture for this suite: `description: ''`, no labels.
    fn child_task(id: &str, status: TaskStatus, goal_id: &str) -> Task {
        Task {
            id: id.to_owned(),
            description: String::new(),
            status,
            goal_id: Some(goal_id.to_owned()),
            ..make_task()
        }
    }

    fn status_of(h: &Harness, task_id: &str) -> Option<TaskStatus> {
        h.tasks.peek(task_id).map(|t| t.status)
    }

    // ── create ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_with_an_assignee_enables_autonomous() {
        let h = Harness::with_agent_service();
        h.seed_agent("agt_001", false);

        h.service
            .create(CreateGoalInput {
                title: "Goal A".into(),
                assignee: Some("agt_001".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_001"), Some(true));
    }

    #[tokio::test]
    async fn create_without_an_assignee_leaves_autonomous_alone() {
        let h = Harness::with_agent_service();
        h.seed_agent("agt_001", false);

        h.service
            .create(CreateGoalInput {
                title: "Goal A".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_001"), Some(false));
    }

    /// The created record's shape and its event.
    #[tokio::test]
    async fn create_trims_and_emits_goal_created() {
        let h = Harness::bare();
        let mut sub = h.bus.subscribe();

        let created = h
            .service
            .create(CreateGoalInput {
                title: "  Ship v2  ".into(),
                description: Some("  Get it out  ".into()),
                assignee: None,
            })
            .await
            .unwrap();

        assert!(created.id.starts_with("goal_"), "{}", created.id);
        assert_eq!(created.title, "Ship v2");
        assert_eq!(created.description, "Get it out");
        assert_eq!(created.status, GoalStatus::Active);
        assert_eq!(created.updated_at.as_deref(), Some(&*created.created_at));
        assert_eq!(h.goals.peek(&created.id), Some(created.clone()));

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::GoalCreated {
                goal_id: created.id,
                title: "Ship v2".into(),
            }
        );
    }

    /// The message ("Goal title is required") and exit code 2 are asserted below.
    #[tokio::test]
    async fn create_rejects_an_empty_title() {
        let h = Harness::bare();
        for title in ["", "   ", "\t\n"] {
            let err = h
                .service
                .create(CreateGoalInput {
                    title: title.into(),
                    ..Default::default()
                })
                .await
                .expect_err("rejected");
            assert_eq!(err.to_string(), "Goal title is required");
            assert_eq!(err.exit_code(), 2);
        }
        assert!(h.goals.snapshot().is_empty());
    }

    // ── delete ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_disables_autonomous_for_the_former_assignee() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_del1", "agt_002"));
        h.seed_agent("agt_002", true);

        h.service.delete("goal_del1").await.unwrap();

        // The goal is gone before the check runs, so the agent has no active
        // goal left and is switched off.
        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_002"), Some(false));
    }

    #[tokio::test]
    async fn delete_without_an_assignee_leaves_autonomous_alone() {
        let h = Harness::with_agent_service();
        h.goals.seed(goal("goal_del2"));
        h.seed_agent("agt_002", true);

        h.service.delete("goal_del2").await.unwrap();

        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_002"), Some(true));
    }

    /// The deletion itself and its event.
    #[tokio::test]
    async fn delete_removes_the_goal_and_emits_goal_deleted() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_d"));
        let mut sub = h.bus.subscribe();

        h.service.delete("goal_d").await.unwrap();

        assert!(h.goals.peek("goal_d").is_none());
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::GoalDeleted {
                goal_id: "goal_d".into(),
            }
        );
    }

    /// `GoalNotFoundError` is exit 1, not exit 2.
    #[tokio::test]
    async fn get_reports_goal_not_found_with_exit_1() {
        let h = Harness::bare();
        let err = h.service.get("goal_nope").await.expect_err("missing");
        assert_eq!(err.to_string(), "Goal not found: goal_nope");
        assert_eq!(err.exit_code(), 1);
    }

    // ── update_status ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn achieved_disables_autonomous() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_s1", "agt_003"));
        h.seed_agent("agt_003", true);

        h.service
            .update_status("goal_s1", GoalStatus::Achieved, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_003"), Some(false));
    }

    #[tokio::test]
    async fn abandoned_disables_autonomous() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_s2", "agt_004"));
        h.seed_agent("agt_004", true);

        h.service
            .update_status("goal_s2", GoalStatus::Abandoned, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_004"), Some(false));
    }

    #[tokio::test]
    async fn paused_disables_autonomous() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_s3", "agt_005"));
        h.seed_agent("agt_005", true);

        h.service
            .update_status("goal_s3", GoalStatus::Paused, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_of("agt_005"), Some(false));
    }

    #[tokio::test]
    async fn terminal_without_an_assignee_leaves_autonomous_alone() {
        let h = Harness::with_agent_service();
        h.goals.seed(goal("goal_s4"));
        h.seed_agent("agt_005", true);

        h.service
            .update_status("goal_s4", GoalStatus::Achieved, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_005"), Some(true));
    }

    /// The state machine rejects the transition with its message and exit code 2,
    /// and nothing is written.
    #[tokio::test]
    async fn an_invalid_transition_is_rejected_with_exit_2() {
        let h = Harness::bare();
        h.goals.seed(Goal {
            status: GoalStatus::Achieved,
            ..goal("goal_term")
        });

        let err = h
            .service
            .update_status("goal_term", GoalStatus::Active, false)
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Cannot transition goal from 'achieved' to 'active'"
        );
        assert_eq!(err.exit_code(), 2);
        // Nothing was written.
        assert_eq!(h.goals.saved().len(), 0);
    }

    /// A status change is saved and announced as exactly one
    /// `goal:status_changed` event carrying the goal id and both statuses.
    #[tokio::test]
    async fn a_status_change_is_persisted_and_announced() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_e"));
        let mut sub = h.bus.subscribe();

        let updated = h
            .service
            .update_status("goal_e", GoalStatus::Paused, false)
            .await
            .unwrap();

        assert_eq!(updated.status, GoalStatus::Paused);
        assert_ne!(updated.updated_at.as_deref(), Some(SEEDED_AT));
        assert_eq!(
            h.goals.peek("goal_e").map(|g| g.status),
            Some(GoalStatus::Paused)
        );
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::GoalStatusChanged {
                goal_id: "goal_e".into(),
                from: GoalStatus::Active,
                to: GoalStatus::Paused,
            }
        );
    }

    /// `paused → active` re-enables autonomous mode, which
    /// is the other half of the autonomous side effects.
    #[tokio::test]
    async fn resuming_a_paused_goal_re_enables_autonomous() {
        let h = Harness::with_agent_service();
        h.goals.seed(Goal {
            status: GoalStatus::Paused,
            ..assigned_goal("goal_resume", "agt_r")
        });
        h.seed_agent("agt_r", false);

        h.service
            .update_status("goal_resume", GoalStatus::Active, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_r"), Some(true));
    }

    /// Pausing also cancels the agent's queued autonomous tasks.
    #[tokio::test]
    async fn pausing_cancels_the_agents_dispatchable_autonomous_tasks() {
        let h = Harness::full();
        h.goals.seed(assigned_goal("goal_p", "agt_p"));
        h.seed_agent("agt_p", true);
        for (id, status) in [
            ("tsk_todo", TaskStatus::Todo),
            ("tsk_retrying", TaskStatus::Retrying),
            ("tsk_running", TaskStatus::InProgress),
        ] {
            h.tasks.seed(Task {
                assignee: Some("agt_p".into()),
                labels: vec![AUTONOMOUS_LABEL.to_owned()],
                ..child_task(id, status, "goal_p")
            });
        }
        // Another agent's queued autonomous task, and this agent's manual one.
        h.tasks.seed(Task {
            assignee: Some("agt_other".into()),
            labels: vec![AUTONOMOUS_LABEL.to_owned()],
            ..child_task("tsk_other", TaskStatus::Todo, "goal_p")
        });
        h.tasks.seed(Task {
            assignee: Some("agt_p".into()),
            ..child_task("tsk_manual", TaskStatus::Todo, "goal_p")
        });

        h.service
            .update_status("goal_p", GoalStatus::Paused, false)
            .await
            .unwrap();

        assert_eq!(status_of(&h, "tsk_todo"), Some(TaskStatus::Cancelled));
        assert_eq!(status_of(&h, "tsk_retrying"), Some(TaskStatus::Cancelled));
        // `in_progress` is not dispatchable, so it is left running.
        assert_eq!(status_of(&h, "tsk_running"), Some(TaskStatus::InProgress));
        assert_eq!(status_of(&h, "tsk_other"), Some(TaskStatus::Todo));
        assert_eq!(status_of(&h, "tsk_manual"), Some(TaskStatus::Todo));
    }

    // ── update (assignee change) ───────────────────────────────────────────

    #[tokio::test]
    async fn changing_the_assignee_enables_the_new_and_disables_the_old() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_u1", "agt_old"));
        h.seed_agent("agt_old", true);
        h.seed_agent("agt_new", false);

        h.service
            .update(
                "goal_u1",
                &UpdateGoalFields {
                    assignee: Some("agt_new".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(h.autonomous_of("agt_new"), Some(true));
        assert_eq!(h.autonomous_of("agt_old"), Some(false));
        assert_eq!(h.autonomous_calls(), 2);
    }

    #[tokio::test]
    async fn setting_an_assignee_where_there_was_none_only_enables() {
        let h = Harness::with_agent_service();
        h.goals.seed(goal("goal_u2"));
        h.seed_agent("agt_new2", false);

        h.service
            .update(
                "goal_u2",
                &UpdateGoalFields {
                    assignee: Some("agt_new2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_new2"), Some(true));
    }

    #[tokio::test]
    async fn clearing_the_assignee_only_disables_the_old() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_u3", "agt_will_lose"));
        h.seed_agent("agt_will_lose", true);

        let updated = h
            .service
            .update(
                "goal_u3",
                &UpdateGoalFields {
                    assignee: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.assignee, None);
        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_will_lose"), Some(false));
    }

    #[tokio::test]
    async fn updating_other_fields_leaves_autonomous_alone() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_u4", "agt_same"));
        h.seed_agent("agt_same", true);

        let updated = h
            .service
            .update(
                "goal_u4",
                &UpdateGoalFields {
                    title: Some("New title".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.title, "New title");
        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_same"), Some(true));
    }

    /// The field-level behaviour of `update` — trimming,
    /// the empty-title guard, and the `goal:updated` event.
    #[tokio::test]
    async fn update_trims_supplied_fields_and_leaves_the_rest() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_f"));
        let mut sub = h.bus.subscribe();

        let updated = h
            .service
            .update(
                "goal_f",
                &UpdateGoalFields {
                    description: Some("  new body  ".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(updated.description, "new body");
        assert_eq!(updated.title, "Test goal");
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            *events[0],
            OrchestratorEvent::GoalUpdated {
                goal_id: "goal_f".into(),
            }
        );
    }

    #[tokio::test]
    async fn update_rejects_an_empty_title() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_f2"));

        for title in ["", "   "] {
            let err = h
                .service
                .update(
                    "goal_f2",
                    &UpdateGoalFields {
                        title: Some(title.into()),
                        ..Default::default()
                    },
                )
                .await
                .expect_err("rejected");
            assert_eq!(err.to_string(), "Goal title cannot be empty");
            assert_eq!(err.exit_code(), 2);
        }
        assert_eq!(h.goals.saved().len(), 0);
    }

    // ── no agent service (an absent dependency) ───────────────────

    #[tokio::test]
    async fn create_without_an_agent_service_is_a_no_op() {
        let h = Harness::bare();
        let mut sub = h.bus.subscribe();
        let goal = h
            .service
            .create(CreateGoalInput {
                title: "Goal X".into(),
                assignee: Some("agt_x".into()),
                ..Default::default()
            })
            .await
            .expect("create");

        // "No-op" is about the *agent* half only: the goal itself is still
        // created, stored and announced with its assignee intact. `is_ok()`
        // alone would also pass if this silently created nothing.
        assert_eq!(goal.assignee.as_deref(), Some("agt_x"));
        assert_eq!(h.goals.peek(&goal.id).expect("stored").title, "Goal X");
        assert_eq!(h.goals.saved().len(), 1);
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), OrchestratorEventType::GoalCreated);
        // And nothing was written to the agent store, there being no service.
        assert_eq!(h.agents.saved().len(), 0);
    }

    #[tokio::test]
    async fn delete_without_an_agent_service_is_a_no_op() {
        let h = Harness::bare();
        h.goals.seed(assigned_goal("goal_noop1", "agt_y"));

        h.service.delete("goal_noop1").await.expect("delete");

        assert!(
            h.goals.peek("goal_noop1").is_none(),
            "the goal survived its own deletion"
        );
        assert_eq!(h.agents.saved().len(), 0);
    }

    #[tokio::test]
    async fn update_status_without_an_agent_service_is_a_no_op() {
        let h = Harness::bare();
        h.goals.seed(assigned_goal("goal_noop2", "agt_z"));

        let goal = h
            .service
            .update_status("goal_noop2", GoalStatus::Achieved, false)
            .await
            .expect("update status");

        assert_eq!(goal.status, GoalStatus::Achieved);
        assert_eq!(
            h.goals.peek("goal_noop2").expect("stored").status,
            GoalStatus::Achieved,
            "the new status never reached the store"
        );
        assert_eq!(h.agents.saved().len(), 0);
    }

    // ── set_autonomous failures are swallowed ──────────────────────────────

    #[tokio::test]
    async fn create_swallows_a_set_autonomous_failure() {
        let h = Harness::with_agent_service();
        // No `agt_missing` in the store, so `set_autonomous` raises
        // `AgentNotFoundError`.
        let goal = h
            .service
            .create(CreateGoalInput {
                title: "Goal err".into(),
                assignee: Some("agt_missing".into()),
                ..Default::default()
            })
            .await
            .expect("create");

        // Swallowed means the goal lands anyway, with the assignee it asked
        // for — not that the call returns `Ok` having abandoned the write.
        assert_eq!(goal.assignee.as_deref(), Some("agt_missing"));
        assert_eq!(h.goals.peek(&goal.id).expect("stored").title, "Goal err");
    }

    #[tokio::test]
    async fn delete_swallows_a_set_autonomous_failure() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_err1", "agt_missing"));

        h.service.delete("goal_err1").await.expect("delete");

        assert!(
            h.goals.peek("goal_err1").is_none(),
            "the agent failure took the deletion with it"
        );
    }

    #[tokio::test]
    async fn update_status_swallows_a_set_autonomous_failure() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_err2", "agt_missing"));

        h.service
            .update_status("goal_err2", GoalStatus::Achieved, false)
            .await
            .expect("update status");

        assert_eq!(
            h.goals.peek("goal_err2").expect("stored").status,
            GoalStatus::Achieved,
            "the agent failure took the status change with it"
        );
    }

    // ── the achieved guard ─────────────────────────────────────────────────

    #[tokio::test]
    async fn achieved_blocks_on_a_todo_child_task() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_pend1"));
        h.tasks
            .seed(child_task("tsk_child1", TaskStatus::Todo, "goal_pend1"));

        let err = h
            .service
            .update_status("goal_pend1", GoalStatus::Achieved, false)
            .await
            .expect_err("blocked");
        assert_eq!(
            err.to_string(),
            "Cannot mark goal goal_pend1 as achieved: 1 task(s) still pending \u{2014} tsk_child1 (todo)"
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(
            err.hint().as_deref(),
            Some("Use --force to cancel pending tasks and mark achieved")
        );
        assert_eq!(
            h.goals.peek("goal_pend1").map(|g| g.status),
            Some(GoalStatus::Active)
        );
    }

    #[tokio::test]
    async fn achieved_blocks_on_an_in_progress_child_task() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_pend2"));
        h.tasks.seed(child_task(
            "tsk_child2",
            TaskStatus::InProgress,
            "goal_pend2",
        ));

        let err = h
            .service
            .update_status("goal_pend2", GoalStatus::Achieved, false)
            .await
            .expect_err("blocked");
        assert!(err.to_string().contains("tsk_child2 (in_progress)"));
    }

    #[tokio::test]
    async fn achieved_blocks_on_a_review_child_task() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_pend3"));
        h.tasks
            .seed(child_task("tsk_child3", TaskStatus::Review, "goal_pend3"));

        let err = h
            .service
            .update_status("goal_pend3", GoalStatus::Achieved, false)
            .await
            .expect_err("blocked");
        assert!(err.to_string().contains("tsk_child3 (review)"));
    }

    #[tokio::test]
    async fn achieved_allows_terminal_child_tasks() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_ok1"));
        for (id, status) in [
            ("tsk_done", TaskStatus::Done),
            ("tsk_fail", TaskStatus::Failed),
            ("tsk_canc", TaskStatus::Cancelled),
        ] {
            h.tasks.seed(child_task(id, status, "goal_ok1"));
        }

        let result = h
            .service
            .update_status("goal_ok1", GoalStatus::Achieved, false)
            .await
            .unwrap();
        assert_eq!(result.status, GoalStatus::Achieved);
    }

    #[tokio::test]
    async fn achieved_allows_a_goal_with_no_child_tasks() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_empty"));

        let result = h
            .service
            .update_status("goal_empty", GoalStatus::Achieved, false)
            .await
            .unwrap();
        assert_eq!(result.status, GoalStatus::Achieved);
    }

    #[tokio::test]
    async fn force_cancels_every_cancellable_child_task() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_force1"));
        for (id, status) in [
            ("tsk_f1", TaskStatus::Todo),
            ("tsk_f2", TaskStatus::Retrying),
            ("tsk_f3", TaskStatus::Done),
            ("tsk_f4", TaskStatus::Review),
        ] {
            h.tasks.seed(child_task(id, status, "goal_force1"));
        }

        let result = h
            .service
            .update_status("goal_force1", GoalStatus::Achieved, true)
            .await
            .unwrap();

        assert_eq!(result.status, GoalStatus::Achieved);
        assert_eq!(status_of(&h, "tsk_f1"), Some(TaskStatus::Cancelled));
        assert_eq!(status_of(&h, "tsk_f2"), Some(TaskStatus::Cancelled));
        assert_eq!(status_of(&h, "tsk_f4"), Some(TaskStatus::Cancelled));
        // The terminal one was never pending, so it is untouched.
        assert_eq!(status_of(&h, "tsk_f3"), Some(TaskStatus::Done));
    }

    #[tokio::test]
    async fn force_still_blocks_on_in_progress_child_tasks() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_force2"));
        h.tasks
            .seed(child_task("tsk_ip1", TaskStatus::InProgress, "goal_force2"));
        h.tasks
            .seed(child_task("tsk_td1", TaskStatus::Todo, "goal_force2"));

        let err = h
            .service
            .update_status("goal_force2", GoalStatus::Achieved, true)
            .await
            .expect_err("blocked");

        // The count and summary cover only what is left running.
        assert_eq!(
            err.to_string(),
            "Cannot mark goal goal_force2 as achieved: 1 task(s) still pending \u{2014} tsk_ip1 (in_progress)"
        );
        // The cancellable one was still cancelled on the way through.
        assert_eq!(status_of(&h, "tsk_td1"), Some(TaskStatus::Cancelled));
        assert_eq!(status_of(&h, "tsk_ip1"), Some(TaskStatus::InProgress));
        assert_eq!(
            h.goals.peek("goal_force2").map(|g| g.status),
            Some(GoalStatus::Active)
        );
    }

    #[tokio::test]
    async fn paused_does_not_run_the_achieved_guard() {
        let h = Harness::full();
        h.goals.seed(assigned_goal("goal_pause", "agt_x"));
        h.tasks
            .seed(child_task("tsk_p1", TaskStatus::Todo, "goal_pause"));

        let result = h
            .service
            .update_status("goal_pause", GoalStatus::Paused, false)
            .await
            .unwrap();

        assert_eq!(result.status, GoalStatus::Paused);
        // The goal-scoped listing is the guard's signature; pausing only lists
        // by status, for `cancel_pending_autonomous_tasks`.
        assert!(
            !h.tasks
                .calls
                .contains(&orch_core::test_support::TaskStoreCall::List(
                    TaskFilter::with_goal("goal_pause")
                ))
        );
        // The non-autonomous child task survives the pause.
        assert_eq!(status_of(&h, "tsk_p1"), Some(TaskStatus::Todo));
    }

    #[tokio::test]
    async fn abandoned_does_not_run_the_achieved_guard() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_aband"));
        h.tasks
            .seed(child_task("tsk_ab1", TaskStatus::Todo, "goal_aband"));

        let result = h
            .service
            .update_status("goal_aband", GoalStatus::Abandoned, false)
            .await
            .unwrap();

        assert_eq!(result.status, GoalStatus::Abandoned);
        assert_eq!(status_of(&h, "tsk_ab1"), Some(TaskStatus::Todo));
    }

    #[tokio::test]
    async fn the_pending_task_error_names_every_task_and_status() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_msg"));
        h.tasks
            .seed(child_task("tsk_m1", TaskStatus::Todo, "goal_msg"));
        h.tasks
            .seed(child_task("tsk_m2", TaskStatus::Retrying, "goal_msg"));

        let err = h
            .service
            .update_status("goal_msg", GoalStatus::Achieved, false)
            .await
            .expect_err("blocked");
        let message = err.to_string();
        assert!(message.contains("tsk_m1 (todo)"), "{message}");
        assert!(message.contains("tsk_m2 (retrying)"), "{message}");
        assert!(message.contains("2 task(s) still pending"), "{message}");
        // The summary joins with ", " in listing order.
        assert!(
            message.ends_with("tsk_m1 (todo), tsk_m2 (retrying)"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn the_achieved_guard_ignores_autonomous_tasks() {
        let h = Harness::full();
        h.goals.seed(assigned_goal("goal_auto1", "agt_a"));
        h.tasks.seed(Task {
            labels: vec![AUTONOMOUS_LABEL.to_owned()],
            ..child_task("tsk_auto", TaskStatus::InProgress, "goal_auto1")
        });
        h.tasks
            .seed(child_task("tsk_done", TaskStatus::Done, "goal_auto1"));

        // The `[auto]` task is in_progress but excluded, so no deadlock.
        let result = h
            .service
            .update_status("goal_auto1", GoalStatus::Achieved, false)
            .await
            .unwrap();
        assert_eq!(result.status, GoalStatus::Achieved);
    }

    #[tokio::test]
    async fn the_achieved_guard_still_blocks_on_a_user_task() {
        let h = Harness::with_task_service();
        h.goals.seed(goal("goal_block"));
        h.tasks.seed(Task {
            labels: vec![AUTONOMOUS_LABEL.to_owned()],
            ..child_task("tsk_auto", TaskStatus::InProgress, "goal_block")
        });
        h.tasks
            .seed(child_task("tsk_user", TaskStatus::InProgress, "goal_block"));

        let err = h
            .service
            .update_status("goal_block", GoalStatus::Achieved, false)
            .await
            .expect_err("blocked");
        // Only the user task is counted.
        assert_eq!(
            err.to_string(),
            "Cannot mark goal goal_block as achieved: 1 task(s) still pending \u{2014} tsk_user (in_progress)"
        );
    }

    #[tokio::test]
    async fn paused_to_achieved_is_allowed_and_disables_autonomous() {
        let h = Harness::full();
        h.goals.seed(Goal {
            status: GoalStatus::Paused,
            ..assigned_goal("goal_pa1", "agt_pa")
        });
        h.seed_agent("agt_pa", true);

        let result = h
            .service
            .update_status("goal_pa1", GoalStatus::Achieved, false)
            .await
            .unwrap();

        assert_eq!(result.status, GoalStatus::Achieved);
        assert_eq!(h.autonomous_of("agt_pa"), Some(false));
    }

    #[tokio::test]
    async fn paused_to_achieved_with_force_cancels_pending_tasks() {
        let h = Harness::with_task_service();
        h.goals.seed(Goal {
            status: GoalStatus::Paused,
            ..goal("goal_pf1")
        });
        h.tasks
            .seed(child_task("tsk_pf1", TaskStatus::Todo, "goal_pf1"));
        h.tasks
            .seed(child_task("tsk_pf2", TaskStatus::Review, "goal_pf1"));

        let result = h
            .service
            .update_status("goal_pf1", GoalStatus::Achieved, true)
            .await
            .unwrap();

        assert_eq!(result.status, GoalStatus::Achieved);
        assert_eq!(status_of(&h, "tsk_pf1"), Some(TaskStatus::Cancelled));
        assert_eq!(status_of(&h, "tsk_pf2"), Some(TaskStatus::Cancelled));
    }

    #[tokio::test]
    async fn the_achieved_guard_is_skipped_without_a_task_service() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_notsk"));
        // A child task would block, but there is no task service to find it.
        h.tasks
            .seed(child_task("tsk_ghost", TaskStatus::Todo, "goal_notsk"));

        let result = h
            .service
            .update_status("goal_notsk", GoalStatus::Achieved, false)
            .await
            .unwrap();
        assert_eq!(result.status, GoalStatus::Achieved);
    }

    // ── several active goals per agent ─────────────────────────────────────

    #[tokio::test]
    async fn delete_keeps_autonomous_while_another_active_goal_remains() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_m1", "agt_shared"));
        h.goals.seed(assigned_goal("goal_m2", "agt_shared"));
        h.seed_agent("agt_shared", true);

        h.service.delete("goal_m1").await.unwrap();

        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_shared"), Some(true));
    }

    #[tokio::test]
    async fn delete_of_the_last_active_goal_disables_autonomous() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_last", "agt_solo"));
        h.seed_agent("agt_solo", true);

        h.service.delete("goal_last").await.unwrap();

        assert_eq!(h.autonomous_calls(), 1);
        assert_eq!(h.autonomous_of("agt_solo"), Some(false));
    }

    #[tokio::test]
    async fn terminal_keeps_autonomous_while_another_active_goal_remains() {
        let h = Harness::with_agent_service();
        h.goals.seed(assigned_goal("goal_t1", "agt_multi"));
        h.goals.seed(assigned_goal("goal_t2", "agt_multi"));
        h.seed_agent("agt_multi", true);

        h.service
            .update_status("goal_t1", GoalStatus::Achieved, false)
            .await
            .unwrap();

        assert_eq!(h.autonomous_calls(), 0);
        assert_eq!(h.autonomous_of("agt_multi"), Some(true));
    }

    // ── list / list_tasks_for_goal / get_progress_report ───────────────────

    /// The filter reaches the store.
    #[tokio::test]
    async fn list_passes_the_filter_through_to_the_store() {
        let h = Harness::bare();
        h.goals.seed(goal("goal_a"));
        h.goals.seed(Goal {
            status: GoalStatus::Paused,
            ..goal("goal_b")
        });

        assert_eq!(
            h.service.list(&GoalFilter::default()).await.unwrap().len(),
            2
        );
        let active = h
            .service
            .list(&GoalFilter::with_status(GoalStatus::Active))
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "goal_a");
    }

    /// `listTasksForGoal` degrades to an empty list.
    #[tokio::test]
    async fn list_tasks_for_goal_is_empty_without_a_task_service() {
        let h = Harness::bare();
        h.tasks
            .seed(child_task("tsk_1", TaskStatus::Todo, "goal_1"));
        assert!(
            h.service
                .list_tasks_for_goal("goal_1")
                .await
                .unwrap()
                .is_empty()
        );

        let wired = Harness::with_task_service();
        wired
            .tasks
            .seed(child_task("tsk_1", TaskStatus::Todo, "goal_1"));
        wired
            .tasks
            .seed(child_task("tsk_2", TaskStatus::Todo, "goal_2"));
        let listed = wired.service.list_tasks_for_goal("goal_1").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "tsk_1");
    }

    /// The progress report is a context entry keyed
    /// `<goalId>-progress`, and is `None` with no context store.
    #[tokio::test]
    async fn get_progress_report_reads_the_goal_progress_context_key() {
        let h = Harness::full();
        h.context.seed("goal_1-progress", "half way");

        assert_eq!(
            h.service.get_progress_report("goal_1").await.unwrap(),
            Some("half way".to_owned())
        );
        assert_eq!(h.service.get_progress_report("goal_2").await.unwrap(), None);

        let bare = Harness::bare();
        bare.context.seed("goal_1-progress", "half way");
        assert_eq!(
            bare.service.get_progress_report("goal_1").await.unwrap(),
            None
        );
    }

    /// A `Some("")` assignee is JS-falsy, so it toggles
    /// nothing on any path.
    #[tokio::test]
    async fn an_empty_assignee_string_toggles_nothing() {
        let h = Harness::with_agent_service();
        h.goals.seed(Goal {
            assignee: Some(String::new()),
            ..goal("goal_blank")
        });

        h.service
            .update_status("goal_blank", GoalStatus::Achieved, false)
            .await
            .unwrap();
        h.service.delete("goal_blank").await.unwrap();

        assert_eq!(h.autonomous_calls(), 0);
    }

    /// `create` with an assignee emits `goal:created`
    /// before the autonomous toggle, so a subscriber sees the goal first.
    #[tokio::test]
    async fn goal_created_is_emitted_before_the_autonomous_toggle() {
        let h = Harness::with_agent_service();
        h.seed_agent("agt_order", false);
        let mut sub = h.bus.subscribe();

        h.service
            .create(CreateGoalInput {
                title: "Ordered".into(),
                assignee: Some("agt_order".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        let types: Vec<_> = sub.try_drain().iter().map(|e| e.event_type()).collect();
        assert_eq!(types.first(), Some(&OrchestratorEventType::GoalCreated));
    }
}
