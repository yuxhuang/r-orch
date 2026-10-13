//! Task service — ported from `src/application/task-service.ts`.
//!
//! WP-0b implements the walking-skeleton subset: `create`, `list` and `get`.
//! The lifecycle transitions (`update_status`, `assign`, `cancel`, `retry`,
//! `reject`, `update`, `delete`, `increment_attempts`) and attachment copying
//! are WP-4's; the validation and event shapes here are the ones they extend.

use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, TaskFilter, TaskStore};
use orch_domain::{
    CreateTaskInput, DomainError, OrchestratorConfig, OrchestratorEvent, Task, TaskStatus,
};

use crate::events::EventBus;

pub struct TaskService {
    tasks: Arc<dyn TaskStore>,
    agents: Arc<dyn AgentStore>,
    events: Arc<EventBus>,
    config: OrchestratorConfig,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl TaskService {
    pub fn new(
        tasks: Arc<dyn TaskStore>,
        agents: Arc<dyn AgentStore>,
        events: Arc<EventBus>,
        config: OrchestratorConfig,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        TaskService {
            tasks,
            agents,
            events,
            config,
            clock,
            ids,
        }
    }

    pub async fn create(&self, input: CreateTaskInput) -> Result<Task> {
        if input.title.trim().is_empty() {
            return Err(Error::invalid_arguments("Task title is required"));
        }

        let priority = input.priority.unwrap_or(self.config.defaults.task.priority);
        if !(1..=4).contains(&priority) {
            return Err(Error::invalid_arguments(
                "Priority must be an integer between 1 and 4",
            ));
        }

        // Validate dependencies before anything is written.
        let depends_on = input.depends_on.unwrap_or_default();
        if !depends_on.is_empty() {
            let mut missing = Vec::new();
            for dep_id in &depends_on {
                if self.tasks.get(dep_id).await?.is_none() {
                    missing.push(dep_id.clone());
                }
            }
            if !missing.is_empty() {
                return Err(Error::invalid_arguments(format!(
                    "Unknown depends_on task ID(s): {}",
                    missing.join(", ")
                )));
            }
        }

        let assignee = self.resolve_assignee(input.assignee.as_deref()).await?;
        let now = self.clock.now_iso();

        let task = Task {
            id: self.ids.generate("tsk"),
            title: input.title.trim().to_owned(),
            description: input.description.as_deref().unwrap_or("").trim().to_owned(),
            status: TaskStatus::Todo,
            priority,
            assignee,
            labels: input.labels.unwrap_or_default(),
            depends_on,
            created_at: now.clone(),
            updated_at: now,
            attempts: 0,
            max_attempts: input
                .max_attempts
                .unwrap_or(self.config.defaults.task.max_attempts),
            workspace_mode: input.workspace_mode,
            workspace: None,
            proof: None,
            review_criteria: input.review_criteria,
            review_results: None,
            scope: input.scope,
            feedback: None,
            goal_id: input.goal_id,
            // Attachment copying is WP-4's; `task add --attach` is outside the
            // walking skeleton and the CLI does not offer it yet.
            attachments: None,
        };

        self.tasks.save(&task).await?;
        self.events.emit(OrchestratorEvent::TaskCreated {
            task: Box::new(task.clone()),
        });

        Ok(task)
    }

    pub async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.tasks.list(filter).await
    }

    pub async fn get(&self, id: &str) -> Result<Task> {
        self.tasks
            .get(id)
            .await?
            .ok_or_else(|| Error::Domain(DomainError::TaskNotFound(id.to_owned())))
    }

    /// Assign a task to an agent.
    ///
    /// Added by WP-5a because `task assign` is the first command that
    /// **re-saves** an existing entity, which is what makes the `_index.json`
    /// last-write order observable to the parity harness (PARITY.md §14.4 —
    /// the gap `fixtures/scenarios/README.md` lists). The `task:assigned`
    /// event carries the **requested** `agent_id`, not the resolved one, as
    /// the reference does.
    pub async fn assign(&self, task_id: &str, agent_id: &str) -> Result<Task> {
        let mut task = self.get(task_id).await?;
        task.assignee = self.resolve_assignee(Some(agent_id)).await?;
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;

        self.events.emit(OrchestratorEvent::TaskAssigned {
            task_id: task_id.to_owned(),
            agent_id: agent_id.to_owned(),
        });

        Ok(task)
    }

    /// Resolve an assignee to an agent **id**, accepting either an id or an
    /// exact name. The two error messages are contract (PARITY.md §3.1).
    async fn resolve_assignee(&self, assignee: Option<&str>) -> Result<Option<String>> {
        let Some(assignee) = assignee.filter(|a| !a.is_empty()) else {
            return Ok(None);
        };

        if assignee.starts_with("agt_") {
            return match self.agents.get(assignee).await? {
                Some(agent) => Ok(Some(agent.id)),
                None => Err(Error::invalid_arguments(format!(
                    "Unknown agent ID: \"{assignee}\". No agent with this ID exists."
                ))),
            };
        }

        match self.agents.get_by_name(assignee).await? {
            Some(agent) => Ok(Some(agent.id)),
            None => Err(Error::invalid_arguments(format!(
                "Unknown agent: \"{assignee}\". Use an agent ID (agt_xxx) or an exact agent name."
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{
        FakeAgentStore, FakeTaskStore, FixedClock, SeqIdGenerator, make_agent,
    };
    use orch_domain::{Agent, OrchestratorEventType, default_config};

    struct Harness {
        service: TaskService,
        tasks: Arc<FakeTaskStore>,
        bus: Arc<EventBus>,
    }

    fn harness(agents: Vec<Agent>) -> Harness {
        let tasks = Arc::new(FakeTaskStore::new());
        let agent_store = Arc::new(FakeAgentStore::with_agents(agents));
        let bus = Arc::new(EventBus::new());
        let service = TaskService::new(
            Arc::clone(&tasks) as Arc<dyn TaskStore>,
            agent_store as Arc<dyn AgentStore>,
            Arc::clone(&bus),
            default_config(),
            Arc::new(FixedClock::new()),
            Arc::new(SeqIdGenerator::new()),
        );
        Harness {
            service,
            tasks,
            bus,
        }
    }

    fn input(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            title: title.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn create_applies_the_documented_defaults() {
        let h = harness(vec![]);
        let task = h.service.create(input("Second task")).await.unwrap();

        assert_eq!(task.id, "tsk_0000001");
        assert_eq!(task.title, "Second task");
        assert_eq!(task.description, "");
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.priority, 3);
        assert_eq!(task.max_attempts, 3);
        assert_eq!(task.attempts, 0);
        assert!(task.labels.is_empty());
        assert!(task.depends_on.is_empty());
        assert_eq!(task.created_at, task.updated_at);
        assert_eq!(task.assignee, None);

        // Every optional is absent, so the record is the minimal §4.2 shape.
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 11);
    }

    #[tokio::test]
    async fn create_persists_and_emits_task_created() {
        let h = harness(vec![]);
        let task = h.service.create(input("Do it")).await.unwrap();
        let mut sub = h.bus.subscribe();

        assert_eq!(h.tasks.peek(&task.id).unwrap(), task);
        // Subscribing after the fact sees nothing, so re-emit to inspect shape.
        h.service.create(input("Again")).await.unwrap();
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), OrchestratorEventType::TaskCreated);
    }

    #[tokio::test]
    async fn create_trims_the_title_and_description() {
        let h = harness(vec![]);
        let task = h
            .service
            .create(CreateTaskInput {
                title: "  spaced  ".into(),
                description: Some("  body  ".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(task.title, "spaced");
        assert_eq!(task.description, "body");
    }

    #[tokio::test]
    async fn an_empty_title_is_invalid_arguments() {
        let h = harness(vec![]);
        for title in ["", "   ", "\t\n"] {
            let err = h.service.create(input(title)).await.expect_err("rejected");
            assert_eq!(err.to_string(), "Task title is required");
            assert_eq!(err.exit_code(), 2);
        }
        assert!(h.tasks.is_empty());
    }

    #[tokio::test]
    async fn priority_must_be_between_1_and_4() {
        let h = harness(vec![]);
        for priority in [0, 5, -1, 100] {
            let err = h
                .service
                .create(CreateTaskInput {
                    priority: Some(priority),
                    ..input("t")
                })
                .await
                .expect_err("rejected");
            assert_eq!(
                err.to_string(),
                "Priority must be an integer between 1 and 4"
            );
            assert_eq!(err.exit_code(), 2);
        }
        for priority in [1, 2, 3, 4] {
            assert!(
                h.service
                    .create(CreateTaskInput {
                        priority: Some(priority),
                        ..input("t")
                    })
                    .await
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn unknown_dependencies_are_listed_in_the_error() {
        let h = harness(vec![]);
        let existing = h.service.create(input("dep")).await.unwrap();

        let err = h
            .service
            .create(CreateTaskInput {
                depends_on: Some(vec![
                    existing.id.clone(),
                    "tsk_nope".into(),
                    "tsk_also".into(),
                ]),
                ..input("t")
            })
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Unknown depends_on task ID(s): tsk_nope, tsk_also"
        );
        assert_eq!(err.exit_code(), 2);

        // A valid dependency is accepted.
        let task = h
            .service
            .create(CreateTaskInput {
                depends_on: Some(vec![existing.id.clone()]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(task.depends_on, vec![existing.id]);
    }

    #[tokio::test]
    async fn an_assignee_resolves_by_id_or_by_name() {
        let h = harness(vec![make_agent()]);

        let by_id = h
            .service
            .create(CreateTaskInput {
                assignee: Some("agt_test1".into()),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(by_id.assignee.as_deref(), Some("agt_test1"));

        let by_name = h
            .service
            .create(CreateTaskInput {
                assignee: Some("TestAgent".into()),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(by_name.assignee.as_deref(), Some("agt_test1"));
    }

    #[tokio::test]
    async fn an_unresolvable_assignee_reports_the_right_message_for_its_shape() {
        let h = harness(vec![make_agent()]);

        let err = h
            .service
            .create(CreateTaskInput {
                assignee: Some("agt_nope".into()),
                ..input("t")
            })
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Unknown agent ID: \"agt_nope\". No agent with this ID exists."
        );
        assert_eq!(err.exit_code(), 2);

        let err = h
            .service
            .create(CreateTaskInput {
                assignee: Some("Nobody".into()),
                ..input("t")
            })
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Unknown agent: \"Nobody\". Use an agent ID (agt_xxx) or an exact agent name."
        );
    }

    #[tokio::test]
    async fn get_reports_task_not_found_with_exit_1() {
        let h = harness(vec![]);
        let err = h.service.get("tsk_nope").await.expect_err("missing");
        assert_eq!(err.to_string(), "Task not found: tsk_nope");
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn list_passes_the_filter_through_to_the_store() {
        let h = harness(vec![]);
        h.service.create(input("a")).await.unwrap();
        assert_eq!(
            h.service.list(&TaskFilter::default()).await.unwrap().len(),
            1
        );
        assert_eq!(
            h.service
                .list(&TaskFilter::with_status(TaskStatus::Done))
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn optional_inputs_land_on_the_record() {
        let h = harness(vec![]);
        let task = h
            .service
            .create(CreateTaskInput {
                labels: Some(vec!["urgent".into()]),
                max_attempts: Some(7),
                workspace_mode: Some(orch_domain::WorkspaceMode::Shared),
                review_criteria: Some(vec![orch_domain::ReviewCriterion::TestPass]),
                scope: Some(vec!["src/**".into()]),
                goal_id: Some("goal_1".into()),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(task.labels, vec!["urgent"]);
        assert_eq!(task.max_attempts, 7);
        assert_eq!(
            task.workspace_mode,
            Some(orch_domain::WorkspaceMode::Shared)
        );
        assert_eq!(
            task.scope.as_deref(),
            Some(["src/**".to_owned()].as_slice())
        );
        assert_eq!(task.goal_id.as_deref(), Some("goal_1"));
        // camelCase on the wire.
        assert_eq!(serde_json::to_value(&task).unwrap()["goalId"], "goal_1");
    }
}
