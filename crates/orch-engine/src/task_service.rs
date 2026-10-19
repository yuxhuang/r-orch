//! Task service.
//!
//! Every state change a user can ask for goes through here, which is where the
//! transition table is enforced. The orchestrator deliberately
//! bypasses it on recovery paths by writing through the store instead — see
//! `crate::orchestrator`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, TaskFilter, TaskStore};
use orch_domain::{
    AWAITING_APPROVAL_LABEL, CreateTaskInput, DomainError, OrchestratorConfig, OrchestratorEvent,
    Task, TaskStatus, can_transition, is_terminal,
};
use orch_infra::storage::{Paths, fs_utils};

use crate::events::EventBus;
use orch_infra::storage::remote::RemoteAgentDirectory;

/// Fields `task edit` may change. `None` leaves a field alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateTaskFields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<i64>,
    pub labels: Option<Vec<String>>,
    /// Source paths to copy into the task's attachment directory. **Appended**
    /// to whatever is already there, unlike every other field here.
    pub attachments: Option<Vec<PathBuf>>,
}

pub struct TaskService {
    remote_directory: Option<RemoteAgentDirectory>,
    tasks: Arc<dyn TaskStore>,
    agents: Arc<dyn AgentStore>,
    events: Arc<EventBus>,
    config: OrchestratorConfig,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    /// `None` disables attachment handling, exactly as the optional `paths`
    /// constructor argument does. Tests leave it out, which is why so many
    /// never touch the filesystem.
    paths: Option<Paths>,
}

impl TaskService {
    pub fn new(
        tasks: Arc<dyn TaskStore>,
        agents: Arc<dyn AgentStore>,
        events: Arc<EventBus>,
        config: OrchestratorConfig,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        paths: Option<Paths>,
    ) -> Self {
        TaskService {
            remote_directory: None,
            tasks,
            agents,
            events,
            config,
            clock,
            ids,
            paths,
        }
    }

    pub fn with_remote_directory(mut self, directory: RemoteAgentDirectory) -> Self {
        self.remote_directory = Some(directory);
        self
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
        let mirror = input
            .labels
            .as_ref()
            .is_some_and(|labels| labels.iter().any(|label| label.starts_with("remote-ctl:")));
        let description = input.description.as_deref().unwrap_or("");
        let description = if mirror {
            description
        } else {
            description.trim()
        }
        .to_owned();

        let mut task = Task {
            id: self.ids.generate("tsk"),
            title: input.title.trim().to_owned(),
            description,
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
            attachments: None,
        };

        // An empty list leaves `attachments` absent rather than `[]`, which
        // keeps the as-created record at its minimal shape.
        if let Some(sources) = input.attachments.as_deref().filter(|s| !s.is_empty()) {
            let sources: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
            if let Some(names) = self.copy_attachments(&task.id, &sources, None).await? {
                task.attachments = Some(names);
            }
        }

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

    /// Move a task through the state machine. A transition
    /// outside the table is `InvalidTransitionError` (exit 1) and nothing is
    /// written.
    pub async fn update_status(&self, id: &str, new_status: TaskStatus) -> Result<Task> {
        let mut task = self.get(id).await?;
        let old_status = task.status;

        if !can_transition(old_status, new_status) {
            return Err(Error::Domain(DomainError::InvalidTransition {
                task_id: id.to_owned(),
                from: old_status,
                to: new_status,
            }));
        }

        task.status = new_status;
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;

        self.events.emit(OrchestratorEvent::TaskStatusChanged {
            task_id: id.to_owned(),
            from: old_status,
            to: new_status,
        });

        Ok(task)
    }

    /// `orch task approve`. A `todo` task held by [`AWAITING_APPROVAL_LABEL`]
    /// is released for dispatch (the hold is the only change); anything else
    /// takes the ordinary `review → done` transition.
    pub async fn approve(&self, id: &str) -> Result<Task> {
        let mut task = self.get(id).await?;
        if task.status == TaskStatus::Todo && task.awaits_approval() {
            task.labels.retain(|label| label != AWAITING_APPROVAL_LABEL);
            task.updated_at = self.clock.now_iso();
            self.tasks.save(&task).await?;
            return Ok(task);
        }
        self.update_status(id, TaskStatus::Done).await
    }

    /// Assign a task. `agent_id` may be an id or an exact agent name; the
    /// **event carries the raw input**, not the resolved id, which is what
    /// subscribers see.
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

    /// Cancel a non-terminal task. Killing any live process is the
    /// orchestrator's job, not this one's.
    pub async fn cancel(&self, id: &str) -> Result<Task> {
        let task = self.get(id).await?;

        if is_terminal(task.status) {
            return Err(Error::Domain(DomainError::InvalidTransition {
                task_id: id.to_owned(),
                from: task.status,
                to: TaskStatus::Cancelled,
            }));
        }

        self.update_status(id, TaskStatus::Cancelled).await
    }

    /// Reset a `failed`/`cancelled` task to `todo` with a fresh attempt
    /// budget. Written through the store rather than `update_status` because
    /// it also zeroes `attempts`.
    pub async fn retry(&self, id: &str) -> Result<Task> {
        let mut task = self.get(id).await?;

        if task.status != TaskStatus::Failed && task.status != TaskStatus::Cancelled {
            return Err(Error::Domain(DomainError::InvalidTransition {
                task_id: id.to_owned(),
                from: task.status,
                to: TaskStatus::Todo,
            }));
        }

        let old_status = task.status;
        task.status = TaskStatus::Todo;
        task.attempts = 0;
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;

        self.events.emit(OrchestratorEvent::TaskStatusChanged {
            task_id: id.to_owned(),
            from: old_status,
            to: TaskStatus::Todo,
        });

        Ok(task)
    }

    /// Send a task in `review` back to `todo` with optional feedback, which
    /// the next dispatch injects into the prompt.
    pub async fn reject(&self, id: &str, feedback: Option<String>) -> Result<Task> {
        let mut task = self.get(id).await?;

        if task.status != TaskStatus::Review {
            return Err(Error::Domain(DomainError::InvalidTransition {
                task_id: id.to_owned(),
                from: task.status,
                to: TaskStatus::Todo,
            }));
        }

        let old_status = task.status;
        task.status = TaskStatus::Todo;
        task.attempts = 0;
        task.feedback = feedback;
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;

        self.events.emit(OrchestratorEvent::TaskStatusChanged {
            task_id: id.to_owned(),
            from: old_status,
            to: TaskStatus::Todo,
        });

        Ok(task)
    }

    pub async fn update(&self, id: &str, fields: UpdateTaskFields) -> Result<Task> {
        self.update_fields(id, fields, false).await
    }

    /// File mutations use the same validation/store path, with authority guards
    /// and no-op detection at its final live read. Attachments are not writable.
    pub async fn update_definition(&self, id: &str, fields: UpdateTaskFields) -> Result<Task> {
        if fields.attachments.is_some() {
            return Err(Error::invalid_arguments("attachments: read-only field"));
        }
        self.update_fields(id, fields, true).await
    }

    async fn update_fields(
        &self,
        id: &str,
        fields: UpdateTaskFields,
        definition: bool,
    ) -> Result<Task> {
        let mut task = self.get(id).await?;
        if definition
            && (task.status == TaskStatus::InProgress
                || task.labels.iter().any(|l| l.starts_with("remote-ctl:")))
        {
            return Err(Error::invalid_arguments(
                "cannot mutate a running or remote-controlled task definition",
            ));
        }
        if definition
            && fields
                .labels
                .as_ref()
                .unwrap_or(&task.labels)
                .iter()
                .any(|label| label.starts_with("remote-ctl:") || label.starts_with("exec:"))
        {
            return Err(Error::invalid_arguments(
                "labels: remote-ctl: and exec: prefixes are reserved for worker mirrors",
            ));
        }
        if definition
            && fields
                .title
                .as_ref()
                .is_none_or(|value| value == &task.title)
            && fields
                .description
                .as_ref()
                .is_none_or(|value| value == &task.description)
            && fields.priority.is_none_or(|value| value == task.priority)
            && fields
                .labels
                .as_ref()
                .is_none_or(|value| value == &task.labels)
        {
            return Ok(task);
        }
        let before = definition.then(|| task.clone());

        if let Some(title) = fields.title.as_deref() {
            if title.trim().is_empty() {
                return Err(Error::invalid_arguments("Task title cannot be empty"));
            }
            task.title = title.trim().to_owned();
        }
        if let Some(description) = fields.description.as_deref() {
            task.description = if task
                .labels
                .iter()
                .any(|label| label.starts_with("remote-ctl:"))
            {
                description
            } else {
                description.trim()
            }
            .to_owned();
        }
        if let Some(priority) = fields.priority {
            if !(1..=4).contains(&priority) {
                return Err(Error::invalid_arguments(
                    "Priority must be an integer between 1 and 4",
                ));
            }
            task.priority = priority;
        }
        if let Some(labels) = fields.labels {
            task.labels = labels;
        }
        if let Some(sources) = fields.attachments.as_deref().filter(|s| !s.is_empty())
            && let Some(names) = self
                .copy_attachments(
                    id,
                    sources,
                    Some(task.attachments.as_deref().unwrap_or_default()),
                )
                .await?
        {
            let mut existing = task.attachments.take().unwrap_or_default();
            existing.extend(names);
            task.attachments = Some(existing);
        }

        if before.as_ref() == Some(&task) {
            return Ok(task);
        }
        if definition {
            fs_utils::check_definition_write_size(&task)?;
        }
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;
        Ok(task)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let task = self.get(id).await?;
        if task.status == TaskStatus::InProgress {
            return Err(Error::invalid_arguments(
                "Cannot delete a running task. Cancel it first.",
            ));
        }
        self.tasks.delete(id).await?;

        if let Some(paths) = &self.paths {
            let dir = paths.task_attachments_dir(id)?;
            // A missing directory is not an error.
            if let Err(err) = tokio::fs::remove_dir_all(&dir).await
                && err.kind() != std::io::ErrorKind::NotFound
            {
                return Err(Error::io("remove task attachments", &dir, err));
            }
        }

        Ok(())
    }

    pub fn attachment_path(&self, task_id: &str, filename: &str) -> Result<PathBuf> {
        let paths = self
            .paths
            .as_ref()
            .ok_or_else(|| Error::invalid_arguments("Paths not configured"))?;
        Ok(paths.task_attachments_dir(task_id)?.join(filename))
    }

    pub async fn increment_attempts(&self, id: &str) -> Result<Task> {
        let mut task = self.get(id).await?;
        task.attempts += 1;
        task.updated_at = self.clock.now_iso();
        self.tasks.save(&task).await?;
        Ok(task)
    }

    /// Copy source files into `.orchestry/attachments/<task id>/` and return
    /// their basenames.
    ///
    /// Validate all sources and names before creating the directory or copying.
    /// `existing` enables append collision checks, including unrecorded files.
    /// `None` is returned when attachment handling is disabled (no `paths`).
    async fn copy_attachments(
        &self,
        task_id: &str,
        sources: &[PathBuf],
        existing: Option<&[String]>,
    ) -> Result<Option<Vec<String>>> {
        let Some(paths) = &self.paths else {
            return Ok(None);
        };

        let dir = paths.task_attachments_dir(task_id)?;
        let mut names = Vec::with_capacity(sources.len());
        for source in sources {
            let missing = || {
                Error::invalid_arguments(format!("Attachment file not found: {}", source.display()))
            };
            if !fs_utils::path_exists(source).await {
                return Err(missing());
            }
            names.push(
                source
                    .file_name()
                    .ok_or_else(missing)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        for source in sources {
            // metadata follows symlinks: links to regular files remain valid.
            let metadata = tokio::fs::metadata(source).await.map_err(|err| {
                Error::invalid_arguments(format!(
                    "Cannot read attachment metadata: {}: {err}",
                    source.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(Error::invalid_arguments(format!(
                    "Attachment is not a regular file: {}",
                    source.display()
                )));
            }
        }
        // Compare lowercase keys for portable collision checks; keep source names.
        let mut seen = HashMap::with_capacity(names.len());
        for (name, source) in names.iter().zip(sources) {
            if let Some(first) = seen.insert(name.to_lowercase(), source) {
                return Err(Error::invalid_arguments(format!(
                    "Duplicate attachment name: {name} ({}, {})",
                    first.display(),
                    source.display()
                )));
            }
        }
        if let Some(existing) = existing {
            let existing: HashSet<_> = existing.iter().map(|name| name.to_lowercase()).collect();
            for name in &names {
                if existing.contains(&name.to_lowercase())
                    || fs_utils::path_exists(&dir.join(name)).await
                {
                    return Err(Error::invalid_arguments(format!(
                        "Attachment already exists: {name}"
                    )));
                }
            }
        }
        // Only after validation, so rejected input leaves no directory or copies.
        fs_utils::ensure_dir(&dir).await?;
        for (source, basename) in sources.iter().zip(&names) {
            let destination = dir.join(basename);
            tokio::fs::copy(source, &destination).await.map_err(|err| {
                Error::io(
                    format!(
                        "copy task attachment {} -> {}: {err}",
                        source.display(),
                        destination.display()
                    ),
                    &destination,
                    err,
                )
            })?;
        }

        Ok(Some(names))
    }

    /// Resolve an assignee to an agent **id**, accepting either an id or an
    /// exact name. The two error messages are contract.
    async fn resolve_assignee(&self, assignee: Option<&str>) -> Result<Option<String>> {
        let Some(assignee) = assignee.filter(|a| !a.is_empty()) else {
            return Ok(None);
        };

        if assignee.contains('@') {
            if let Some(directory) = &self.remote_directory
                && let Some(agent) = directory
                    .resolve(assignee, self.agents.list().await?)
                    .await?
            {
                return Ok(Some(agent.id));
            }
            return Err(Error::invalid_arguments(format!(
                "Unknown remote agent: {assignee}"
            )));
        }

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

/// Whether `path` is inside `dir`. Used only by the tests, which assert the
/// containment relation `taskAttachmentsDir` is required to keep.
#[cfg(test)]
fn is_inside(path: &std::path::Path, dir: &std::path::Path) -> bool {
    path.starts_with(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use orch_core::test_support::{
        FakeAgentStore, FakeTaskStore, FixedClock, SeqIdGenerator, make_agent, make_task,
    };
    use orch_domain::{Agent, OrchestratorEventType, TaskProof, default_config};

    struct Harness {
        service: TaskService,
        tasks: Arc<FakeTaskStore>,
        bus: Arc<EventBus>,
        /// Kept alive so the temp directory outlives the service.
        _temp: Option<tempfile::TempDir>,
    }

    fn build(agents: Vec<Agent>, tasks: Vec<Task>, temp: Option<tempfile::TempDir>) -> Harness {
        let task_store = Arc::new(FakeTaskStore::with_tasks(tasks));
        let agent_store = Arc::new(FakeAgentStore::with_agents(agents));
        let bus = Arc::new(EventBus::new());
        let paths = temp.as_ref().map(|t| Paths::new(t.path()));
        let service = TaskService::new(
            Arc::clone(&task_store) as Arc<dyn TaskStore>,
            agent_store as Arc<dyn AgentStore>,
            Arc::clone(&bus),
            default_config(),
            Arc::new(FixedClock::new()),
            Arc::new(SeqIdGenerator::new()),
            paths,
        );
        Harness {
            service,
            tasks: task_store,
            bus,
            _temp: temp,
        }
    }

    fn harness(agents: Vec<Agent>) -> Harness {
        build(agents, vec![], None)
    }

    fn seeded(tasks: Vec<Task>) -> Harness {
        build(vec![], tasks, None)
    }

    /// A harness with a real `.orchestry/` tree, for the attachment tests.
    fn with_paths() -> Harness {
        let temp = tempfile::tempdir().expect("tempdir");
        fs_utils::clear_ensured_dirs();
        build(vec![], vec![], Some(temp))
    }

    #[tokio::test]
    async fn definition_update_refuses_running_and_remote_tasks_and_preserves_noop_timestamp() {
        let mut before = make_task();
        before.description = "  imported body  ".into();
        let h = seeded(vec![before.clone()]);
        let after = h
            .service
            .update_definition(
                &before.id,
                UpdateTaskFields {
                    title: Some(before.title.clone()),
                    description: Some(before.description.clone()),
                    priority: Some(before.priority),
                    labels: Some(before.labels.clone()),
                    attachments: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(after, before);
        assert!(h.tasks.saved().is_empty(), "no-op must not save");
        for remote in [false, true] {
            let mut before = make_task();
            if remote {
                before.labels.push("remote-ctl:controller".into());
            } else {
                before.status = TaskStatus::InProgress;
            }
            let h = seeded(vec![before.clone()]);
            assert!(
                h.service
                    .update_definition(
                        &before.id,
                        UpdateTaskFields {
                            title: Some("changed".into()),
                            ..Default::default()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(h.service.get(&before.id).await.unwrap(), before);
            assert!(h.tasks.saved().is_empty());
        }
    }

    fn path_str(path: &Path) -> String {
        path.display().to_string()
    }

    fn input(title: &str) -> CreateTaskInput {
        CreateTaskInput {
            title: title.into(),
            ..Default::default()
        }
    }

    fn task_with(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            status,
            ..make_task()
        }
    }

    // ── create ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn mirror_task_description_is_preserved_verbatim() {
        let h = harness(Vec::new());
        let prompt = "  Controller prompt\n\nKeep exact spacing.\n";
        let task = h
            .service
            .create(CreateTaskInput {
                title: "mirror".into(),
                description: Some(prompt.into()),
                labels: Some(vec!["remote-ctl:controller".into()]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(task.description, prompt);
    }

    #[tokio::test]
    async fn remote_assignee_resolves_from_offline_directory() {
        let mut h = harness(Vec::new());
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let store = orch_infra::storage::remote::FileRemoteStore::new(paths.clone());
        store
            .add(orch_domain::RemoteConfig {
                alias: "w1".into(),
                addr: "worker:7690".into(),
                added_at: "now".into(),
            })
            .await
            .unwrap();
        store
            .write_status(&orch_domain::RemoteStatus {
                alias: "w1".into(),
                addr: "worker:7690".into(),
                online: false,
                last_seen: None,
                last_error: None,
                peer_name: None,
                instance_id: None,
                os: Some("linux".into()),
                agents: vec![make_agent()],
                peer_version: None,
                link_state: orch_domain::LinkState::Connecting,
                next_retry_at: None,
                attempts: 0,
            })
            .await
            .unwrap();
        h.service = h
            .service
            .with_remote_directory(RemoteAgentDirectory::new(paths));
        assert_eq!(
            h.service
                .resolve_assignee(Some("agt_test1@w1"))
                .await
                .unwrap(),
            Some("agt_test1@w1".into())
        );
        assert_eq!(
            h.service
                .resolve_assignee(Some("TestAgent@w1"))
                .await
                .unwrap(),
            Some("agt_test1@w1".into())
        );
        assert!(
            h.service
                .resolve_assignee(Some("agt_test1@missing"))
                .await
                .is_err()
        );
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

        // Every optional is absent, so the record is the minimal shape.
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 11);
    }

    #[tokio::test]
    async fn create_persists_and_emits_task_created() {
        let h = harness(vec![]);
        let mut sub = h.bus.subscribe();
        let task = h.service.create(input("Do it")).await.unwrap();

        assert_eq!(h.tasks.peek(&task.id).unwrap(), task);
        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), OrchestratorEventType::TaskCreated);
        match events[0].as_ref() {
            OrchestratorEvent::TaskCreated { task: emitted } => assert_eq!(**emitted, task),
            other => panic!("unexpected event: {other:?}"),
        }
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

        let err = h
            .service
            .create(CreateTaskInput {
                depends_on: Some(vec!["tsk_only".into()]),
                ..input("t")
            })
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Unknown depends_on task ID(s): tsk_only");

        // A valid dependency is accepted, as are an empty and an absent list.
        let task = h
            .service
            .create(CreateTaskInput {
                depends_on: Some(vec![existing.id.clone()]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(task.depends_on, vec![existing.id.clone()]);
        assert!(
            h.service
                .create(CreateTaskInput {
                    depends_on: Some(vec![]),
                    ..input("t")
                })
                .await
                .is_ok()
        );
        assert!(h.service.create(input("t")).await.is_ok());

        let multi = h
            .service
            .create(CreateTaskInput {
                depends_on: Some(vec![existing.id.clone(), task.id.clone()]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(multi.depends_on, vec![existing.id, task.id]);
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
    async fn an_absent_assignee_stays_absent() {
        let h = harness(vec![make_agent()]);
        assert_eq!(h.service.create(input("t")).await.unwrap().assignee, None);
    }

    #[tokio::test]
    async fn get_reports_task_not_found_with_exit_1() {
        let h = harness(vec![]);
        let err = h.service.get("tsk_nope").await.expect_err("missing");
        assert_eq!(err.to_string(), "Task not found: tsk_nope");
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn get_returns_the_task_when_present() {
        let h = seeded(vec![make_task()]);
        assert_eq!(h.service.get("tsk_test1").await.unwrap().id, "tsk_test1");
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

    // ── update_status ────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_status_transitions_and_emits() {
        let h = seeded(vec![make_task()]);
        let mut sub = h.bus.subscribe();

        let task = h
            .service
            .update_status("tsk_test1", TaskStatus::InProgress)
            .await
            .unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(
            h.tasks.peek("tsk_test1").unwrap().status,
            TaskStatus::InProgress
        );

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        match events[0].as_ref() {
            OrchestratorEvent::TaskStatusChanged { task_id, from, to } => {
                assert_eq!(task_id, "tsk_test1");
                assert_eq!(*from, TaskStatus::Todo);
                assert_eq!(*to, TaskStatus::InProgress);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_status_refuses_a_transition_outside_the_table() {
        let h = seeded(vec![make_task()]);
        let err = h
            .service
            .update_status("tsk_test1", TaskStatus::Done)
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Invalid transition for tsk_test1: todo → done"
        );
        assert_eq!(err.exit_code(), 1);
        // Nothing was written.
        assert_eq!(h.tasks.peek("tsk_test1").unwrap().status, TaskStatus::Todo);
    }

    // ── approve ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn approve_releases_a_held_todo_task_without_changing_its_status() {
        let h = seeded(vec![Task {
            labels: vec!["exec:key".into(), AWAITING_APPROVAL_LABEL.into()],
            ..make_task()
        }]);
        let mut sub = h.bus.subscribe();
        let task = h.service.approve("tsk_test1").await.unwrap();
        let stored = h.tasks.peek("tsk_test1").unwrap();
        assert_eq!(
            (
                task.status,
                stored.status,
                stored.labels,
                sub.try_drain().len()
            ),
            (
                TaskStatus::Todo,
                TaskStatus::Todo,
                vec!["exec:key".to_owned()],
                0
            )
        );
    }

    #[tokio::test]
    async fn approve_moves_a_reviewed_task_to_done() {
        let h = seeded(vec![task_with("tsk_1", TaskStatus::Review)]);
        h.service.approve("tsk_1").await.unwrap();
        assert_eq!(h.tasks.peek("tsk_1").unwrap().status, TaskStatus::Done);
    }

    #[tokio::test]
    async fn approve_still_refuses_an_unheld_todo_task() {
        let h = seeded(vec![make_task()]);
        let err = h.service.approve("tsk_test1").await.expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Invalid transition for tsk_test1: todo → done"
        );
    }

    #[tokio::test]
    async fn approve_does_not_release_a_cancelled_held_task() {
        let h = seeded(vec![Task {
            status: TaskStatus::Cancelled,
            labels: vec![AWAITING_APPROVAL_LABEL.into()],
            ..make_task()
        }]);
        assert!(h.service.approve("tsk_test1").await.is_err());
        assert!(h.tasks.peek("tsk_test1").unwrap().awaits_approval());
    }

    // ── assign ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn assign_sets_the_assignee_and_emits() {
        let h = build(vec![make_agent()], vec![make_task()], None);
        let mut sub = h.bus.subscribe();

        let task = h.service.assign("tsk_test1", "agt_test1").await.unwrap();
        assert_eq!(task.assignee.as_deref(), Some("agt_test1"));

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        match events[0].as_ref() {
            OrchestratorEvent::TaskAssigned { task_id, agent_id } => {
                assert_eq!(task_id, "tsk_test1");
                assert_eq!(agent_id, "agt_test1");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assign_resolves_a_name_to_an_id_but_emits_the_raw_input() {
        let h = build(vec![make_agent()], vec![make_task()], None);
        let mut sub = h.bus.subscribe();

        let task = h.service.assign("tsk_test1", "TestAgent").await.unwrap();
        assert_eq!(task.assignee.as_deref(), Some("agt_test1"));

        // The event carries what the caller passed.
        match sub.try_drain()[0].as_ref() {
            OrchestratorEvent::TaskAssigned { agent_id, .. } => assert_eq!(agent_id, "TestAgent"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn assign_rejects_an_unknown_agent() {
        let h = build(vec![make_agent()], vec![make_task()], None);
        let err = h
            .service
            .assign("tsk_test1", "Nobody")
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Unknown agent: \"Nobody\". Use an agent ID (agt_xxx) or an exact agent name."
        );
    }

    // ── cancel ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn cancel_moves_a_todo_task_to_cancelled() {
        let h = seeded(vec![make_task()]);
        assert_eq!(
            h.service.cancel("tsk_test1").await.unwrap().status,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn cancel_also_works_on_an_in_progress_task() {
        // Process cleanup is the orchestrator's job; the service only moves
        // the record.
        let h = seeded(vec![task_with("tsk_1", TaskStatus::InProgress)]);
        assert_eq!(
            h.service.cancel("tsk_1").await.unwrap().status,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn cancel_refuses_a_terminal_task() {
        for status in [TaskStatus::Done, TaskStatus::Failed, TaskStatus::Cancelled] {
            let h = seeded(vec![task_with("tsk_1", status)]);
            let err = h.service.cancel("tsk_1").await.expect_err("rejected");
            assert_eq!(
                err.to_string(),
                format!("Invalid transition for tsk_1: {status} → cancelled")
            );
        }
    }

    // ── retry ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn retry_resets_a_failed_task_to_todo_with_no_attempts() {
        let h = seeded(vec![Task {
            id: "tsk_1".into(),
            status: TaskStatus::Failed,
            attempts: 3,
            ..make_task()
        }]);
        let mut sub = h.bus.subscribe();

        let task = h.service.retry("tsk_1").await.unwrap();
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.attempts, 0);

        match sub.try_drain()[0].as_ref() {
            OrchestratorEvent::TaskStatusChanged { from, to, .. } => {
                assert_eq!(*from, TaskStatus::Failed);
                assert_eq!(*to, TaskStatus::Todo);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn retry_also_resets_a_cancelled_task() {
        let h = seeded(vec![task_with("tsk_1", TaskStatus::Cancelled)]);
        assert_eq!(
            h.service.retry("tsk_1").await.unwrap().status,
            TaskStatus::Todo
        );
    }

    #[tokio::test]
    async fn retry_refuses_anything_but_failed_or_cancelled() {
        for status in [
            TaskStatus::Todo,
            TaskStatus::InProgress,
            TaskStatus::Retrying,
            TaskStatus::Review,
            TaskStatus::Done,
        ] {
            let h = seeded(vec![task_with("tsk_1", status)]);
            let err = h.service.retry("tsk_1").await.expect_err("rejected");
            assert_eq!(
                err.to_string(),
                format!("Invalid transition for tsk_1: {status} → todo")
            );
        }
    }

    // ── reject ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reject_sends_a_reviewed_task_back_to_todo_with_feedback() {
        let h = seeded(vec![Task {
            id: "tsk_1".into(),
            status: TaskStatus::Review,
            attempts: 2,
            ..make_task()
        }]);
        let task = h
            .service
            .reject("tsk_1", Some("needs tests".into()))
            .await
            .unwrap();
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.attempts, 0);
        assert_eq!(task.feedback.as_deref(), Some("needs tests"));
    }

    #[tokio::test]
    async fn reject_refuses_a_task_that_is_not_in_review() {
        let h = seeded(vec![make_task()]);
        let err = h
            .service
            .reject("tsk_test1", None)
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Invalid transition for tsk_test1: todo → todo"
        );
    }

    // ── update ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_changes_the_given_fields_only() {
        let h = seeded(vec![make_task()]);
        let task = h
            .service
            .update(
                "tsk_test1",
                UpdateTaskFields {
                    title: Some("  new title  ".into()),
                    description: Some("  new body  ".into()),
                    priority: Some(1),
                    labels: Some(vec!["a".into()]),
                    attachments: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(task.title, "new title");
        assert_eq!(task.description, "new body");
        assert_eq!(task.priority, 1);
        assert_eq!(task.labels, vec!["a"]);
        assert_eq!(task.status, TaskStatus::Todo);
    }

    #[tokio::test]
    async fn update_rejects_an_empty_title_and_an_out_of_range_priority() {
        let h = seeded(vec![make_task()]);

        let err = h
            .service
            .update(
                "tsk_test1",
                UpdateTaskFields {
                    title: Some("   ".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Task title cannot be empty");
        assert_eq!(err.exit_code(), 2);

        for priority in [0, 5] {
            let err = h
                .service
                .update(
                    "tsk_test1",
                    UpdateTaskFields {
                        priority: Some(priority),
                        ..Default::default()
                    },
                )
                .await
                .expect_err("rejected");
            assert_eq!(
                err.to_string(),
                "Priority must be an integer between 1 and 4"
            );
        }
        for priority in [1, 4] {
            assert!(
                h.service
                    .update(
                        "tsk_test1",
                        UpdateTaskFields {
                            priority: Some(priority),
                            ..Default::default()
                        },
                    )
                    .await
                    .is_ok()
            );
        }
    }

    /// Priorities that are `NaN`, `Infinity` or fractional must be rejected. A fractional or non-finite `i64` is unrepresentable in
    /// Rust, so the range check is the whole of the surviving contract.
    #[tokio::test]
    async fn a_non_integral_priority_is_unrepresentable() {
        let h = harness(vec![]);
        assert!(
            h.service
                .create(CreateTaskInput {
                    priority: Some(i64::MAX),
                    ..input("t")
                })
                .await
                .is_err()
        );
        assert!(
            h.service
                .create(CreateTaskInput {
                    priority: Some(i64::MIN),
                    ..input("t")
                })
                .await
                .is_err()
        );
    }

    // ── delete ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_removes_a_non_running_task() {
        let h = seeded(vec![make_task()]);
        h.service.delete("tsk_test1").await.unwrap();
        assert!(h.tasks.peek("tsk_test1").is_none());
    }

    #[tokio::test]
    async fn delete_refuses_a_running_task() {
        let h = seeded(vec![task_with("tsk_1", TaskStatus::InProgress)]);
        let err = h.service.delete("tsk_1").await.expect_err("rejected");
        assert_eq!(
            err.to_string(),
            "Cannot delete a running task. Cancel it first."
        );
        assert_eq!(err.exit_code(), 2);
        assert!(h.tasks.peek("tsk_1").is_some());
    }

    // ── increment_attempts ───────────────────────────────────────────────

    #[tokio::test]
    async fn increment_attempts_adds_one() {
        let h = seeded(vec![make_task()]);
        assert_eq!(
            h.service
                .increment_attempts("tsk_test1")
                .await
                .unwrap()
                .attempts,
            1
        );
        assert_eq!(
            h.service
                .increment_attempts("tsk_test1")
                .await
                .unwrap()
                .attempts,
            2
        );
        assert_eq!(h.tasks.peek("tsk_test1").unwrap().attempts, 2);
    }

    // ── attachments ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn without_paths_attachments_are_ignored() {
        let h = harness(vec![]);
        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec!["/nonexistent/file.txt".into()]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(task.attachments, None);
    }

    #[tokio::test]
    async fn an_absent_or_empty_attachment_list_leaves_the_field_unset() {
        let h = with_paths();
        assert_eq!(
            h.service.create(input("t")).await.unwrap().attachments,
            None
        );
        assert_eq!(
            h.service
                .create(CreateTaskInput {
                    attachments: Some(vec![]),
                    ..input("t")
                })
                .await
                .unwrap()
                .attachments,
            None
        );
    }

    #[tokio::test]
    async fn create_copies_attachments_and_stores_their_basenames() {
        let h = with_paths();
        let source_dir = tempfile::tempdir().unwrap();
        let first = source_dir.path().join("spec.md");
        let second = source_dir.path().join("diagram.png");
        tokio::fs::write(&first, b"spec").await.unwrap();
        tokio::fs::write(&second, b"png").await.unwrap();

        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&first), path_str(&second)]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(
            task.attachments.as_deref(),
            Some(["spec.md".to_owned(), "diagram.png".to_owned()].as_slice())
        );

        let dir = h.service.attachment_path(&task.id, "").unwrap();
        assert!(dir.ends_with(format!("attachments/{}/", task.id)) || dir.is_dir());
        let copied = h.service.attachment_path(&task.id, "spec.md").unwrap();
        assert_eq!(tokio::fs::read(&copied).await.unwrap(), b"spec");
    }

    #[tokio::test]
    async fn a_missing_attachment_is_reported_and_nothing_is_copied() {
        let h = with_paths();
        let source_dir = tempfile::tempdir().unwrap();
        let present = source_dir.path().join("present.txt");
        tokio::fs::write(&present, b"ok").await.unwrap();
        let missing = source_dir.path().join("missing.txt");

        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&present), path_str(&missing)]),
                ..input("t")
            })
            .await
            .expect_err("rejected");
        assert_eq!(
            err.to_string(),
            format!("Attachment file not found: {}", missing.display())
        );
        assert_eq!(err.exit_code(), 2);
        // Validation precedes every copy, so the first file was not copied and
        // no task record exists.
        assert!(h.tasks.is_empty());
        let copied = h
            .service
            .attachment_path("tsk_0000001", "present.txt")
            .unwrap();
        assert!(!copied.exists());
        assert!(
            !copied.parent().unwrap().exists(),
            "no empty task directory"
        );
    }

    #[tokio::test]
    async fn a_directory_attachment_is_rejected_before_any_copy_or_directory_creation() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let present = sources.path().join("present.txt");
        tokio::fs::write(&present, b"keep").await.unwrap();

        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&present), path_str(sources.path())]),
                ..input("t")
            })
            .await
            .expect_err("directories are not attachment files");

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "Attachment is not a regular file: {}",
                sources.path().display()
            )
        );
        assert!(h.tasks.is_empty());
        assert!(
            !h.service
                .attachment_path("tsk_0000001", "")
                .unwrap()
                .exists(),
            "validation must precede attachment directory creation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_socket_attachment_is_rejected_as_invalid_arguments_before_copying() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let socket = sources.path().join("source.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&socket)]),
                ..input("t")
            })
            .await
            .expect_err("sockets are not attachment files");

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(
            err.to_string(),
            format!("Attachment is not a regular file: {}", socket.display())
        );
        assert!(h.tasks.is_empty());
        assert!(
            !h.service
                .attachment_path("tsk_0000001", "")
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn duplicate_attachment_basenames_are_rejected_before_any_copy() {
        let h = with_paths();
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = first_dir.path().join("same.txt");
        let second = second_dir.path().join("same.txt");
        tokio::fs::write(&first, b"first").await.unwrap();
        tokio::fs::write(&second, b"second").await.unwrap();

        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&first), path_str(&second)]),
                ..input("t")
            })
            .await
            .expect_err("duplicate basenames must not silently overwrite files");

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(
            err.to_string(),
            format!(
                "Duplicate attachment name: same.txt ({}, {})",
                first.display(),
                second.display()
            )
        );
        assert!(h.tasks.is_empty());
        assert!(
            !h.service
                .attachment_path("tsk_0000001", "")
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn case_variant_attachment_basenames_are_rejected_before_any_copy() {
        for (first_name, second_name) in [("Report.txt", "report.txt"), ("Ä.txt", "ä.txt")] {
            let h = with_paths();
            let first_dir = tempfile::tempdir().unwrap();
            let second_dir = tempfile::tempdir().unwrap();
            let first = first_dir.path().join(first_name);
            let second = second_dir.path().join(second_name);
            tokio::fs::write(&first, b"first").await.unwrap();
            tokio::fs::write(&second, b"second").await.unwrap();

            let err = h
                .service
                .create(CreateTaskInput {
                    attachments: Some(vec![path_str(&first), path_str(&second)]),
                    ..input("t")
                })
                .await
                .expect_err("case variants must not silently overwrite attachment bytes");

            assert_eq!(err.exit_code(), 2, "{err}");
            assert_eq!(
                err.to_string(),
                format!(
                    "Duplicate attachment name: {second_name} ({}, {})",
                    first.display(),
                    second.display()
                )
            );
            assert!(h.tasks.is_empty());
            assert!(
                !h.service
                    .attachment_path("tsk_0000001", "")
                    .unwrap()
                    .exists()
            );
            assert_eq!(tokio::fs::read(&first).await.unwrap(), b"first");
            assert_eq!(tokio::fs::read(&second).await.unwrap(), b"second");
        }
    }

    #[tokio::test]
    async fn appending_case_variant_attachment_basenames_preserves_the_task_and_stored_state() {
        // Missing stored copies exercise recorded-name validation even on a
        // case-insensitive filesystem, where path_exists would catch the clash.
        for keep_stored in [false, true] {
            let h = with_paths();
            let first_dir = tempfile::tempdir().unwrap();
            let second_dir = tempfile::tempdir().unwrap();
            let original = first_dir.path().join("Report.txt");
            let replacement = second_dir.path().join("report.txt");
            let fresh = second_dir.path().join("fresh.txt");
            tokio::fs::write(&original, b"original").await.unwrap();
            tokio::fs::write(&replacement, b"replacement")
                .await
                .unwrap();
            tokio::fs::write(&fresh, b"fresh").await.unwrap();
            let task = h
                .service
                .create(CreateTaskInput {
                    attachments: Some(vec![path_str(&original)]),
                    ..input("t")
                })
                .await
                .unwrap();
            assert_eq!(task.attachments.as_ref().unwrap(), &["Report.txt"]);
            let stored = h.service.attachment_path(&task.id, "Report.txt").unwrap();
            if !keep_stored {
                tokio::fs::remove_file(&stored).await.unwrap();
            }

            let err = h
                .service
                .update(
                    &task.id,
                    UpdateTaskFields {
                        title: Some("must not be saved".into()),
                        attachments: Some(vec![fresh, replacement]),
                        ..Default::default()
                    },
                )
                .await
                .expect_err("case variants must not append duplicate attachment references");

            assert_eq!(err.exit_code(), 2, "{err}");
            assert_eq!(err.to_string(), "Attachment already exists: report.txt");
            assert_eq!(h.service.get(&task.id).await.unwrap(), task);
            if keep_stored {
                assert_eq!(tokio::fs::read(&stored).await.unwrap(), b"original");
            } else {
                assert!(!stored.exists());
                assert!(
                    !h.service
                        .attachment_path(&task.id, "report.txt")
                        .unwrap()
                        .exists()
                );
            }
            assert!(
                !h.service
                    .attachment_path(&task.id, "fresh.txt")
                    .unwrap()
                    .exists()
            );
        }
    }

    #[tokio::test]
    async fn appending_an_existing_attachment_basename_preserves_the_task_and_bytes() {
        let h = with_paths();
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let original = first_dir.path().join("same.txt");
        let replacement = second_dir.path().join("same.txt");
        let fresh = second_dir.path().join("fresh.txt");
        tokio::fs::write(&original, b"original").await.unwrap();
        tokio::fs::write(&replacement, b"replacement")
            .await
            .unwrap();
        tokio::fs::write(&fresh, b"fresh").await.unwrap();
        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&original)]),
                ..input("t")
            })
            .await
            .unwrap();

        let err = h
            .service
            .update(
                &task.id,
                UpdateTaskFields {
                    title: Some("must not be saved".into()),
                    attachments: Some(vec![fresh, replacement]),
                    ..Default::default()
                },
            )
            .await
            .expect_err("an append must not replace existing attachment bytes");

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(err.to_string(), "Attachment already exists: same.txt");
        assert_eq!(h.service.get(&task.id).await.unwrap(), task);
        assert_eq!(
            tokio::fs::read(h.service.attachment_path(&task.id, "same.txt").unwrap())
                .await
                .unwrap(),
            b"original"
        );
        assert!(
            !h.service
                .attachment_path(&task.id, "fresh.txt")
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn appending_a_recorded_basename_is_rejected_even_if_the_stored_copy_is_missing() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("same.txt");
        tokio::fs::write(&source, b"original").await.unwrap();
        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&source)]),
                ..input("t")
            })
            .await
            .unwrap();
        let stored = h.service.attachment_path(&task.id, "same.txt").unwrap();
        tokio::fs::remove_file(&stored).await.unwrap();

        let err = h
            .service
            .update(
                &task.id,
                UpdateTaskFields {
                    attachments: Some(vec![source]),
                    ..Default::default()
                },
            )
            .await
            .expect_err("a missing copy must not allow duplicate attachment references");

        assert_eq!(err.exit_code(), 2, "{err}");
        assert_eq!(err.to_string(), "Attachment already exists: same.txt");
        assert_eq!(h.service.get(&task.id).await.unwrap(), task);
        assert!(!stored.exists());
    }

    #[tokio::test]
    async fn repeating_the_same_attachment_path_is_rejected_before_copying() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("same.txt");
        tokio::fs::write(&source, b"original").await.unwrap();
        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&source), path_str(&source)]),
                ..input("t")
            })
            .await
            .expect_err("repeated paths are duplicate names");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(
            err.to_string(),
            format!(
                "Duplicate attachment name: same.txt ({0}, {0})",
                source.display()
            )
        );
        assert!(h.tasks.is_empty());
        assert!(
            !h.service
                .attachment_path("tsk_0000001", "")
                .unwrap()
                .exists()
        );
    }

    #[tokio::test]
    async fn appending_an_unrecorded_stored_basename_preserves_the_task_and_directory() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("same.txt");
        let fresh = sources.path().join("fresh.txt");
        tokio::fs::write(&source, b"replacement").await.unwrap();
        tokio::fs::write(&fresh, b"fresh").await.unwrap();
        let task = h.service.create(input("t")).await.unwrap();
        let stored = h.service.attachment_path(&task.id, "same.txt").unwrap();
        tokio::fs::create_dir_all(stored.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&stored, b"unrecorded").await.unwrap();
        let err = h
            .service
            .update(
                &task.id,
                UpdateTaskFields {
                    attachments: Some(vec![fresh, source]),
                    ..Default::default()
                },
            )
            .await
            .expect_err("unrecorded files must not be overwritten");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), "Attachment already exists: same.txt");
        assert_eq!(h.service.get(&task.id).await.unwrap(), task);
        assert_eq!(tokio::fs::read(stored).await.unwrap(), b"unrecorded");
        assert!(
            !h.service
                .attachment_path(&task.id, "fresh.txt")
                .unwrap()
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_to_a_regular_attachment_is_copied_by_content() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("original.txt");
        let link = sources.path().join("link.txt");
        tokio::fs::write(&source, b"linked content").await.unwrap();
        std::os::unix::fs::symlink(&source, &link).unwrap();
        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&link)]),
                ..input("t")
            })
            .await
            .unwrap();
        assert_eq!(
            task.attachments.as_deref(),
            Some(["link.txt".into()].as_slice())
        );
        let stored = h.service.attachment_path(&task.id, "link.txt").unwrap();
        assert_eq!(tokio::fs::read(&stored).await.unwrap(), b"linked content");
        assert!(
            !tokio::fs::symlink_metadata(&stored)
                .await
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn attachment_copy_errors_name_the_source_destination_and_io_cause() {
        let h = with_paths();
        let sources = tempfile::tempdir().unwrap();
        let source = sources.path().join("file.txt");
        tokio::fs::write(&source, b"content").await.unwrap();
        let destination = h
            .service
            .attachment_path("tsk_0000001", "file.txt")
            .unwrap();
        tokio::fs::create_dir_all(&destination).await.unwrap();
        let err = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&source)]),
                ..input("t")
            })
            .await
            .expect_err("copying onto a directory fails");
        let Error::Io { source: cause, .. } = &err else {
            panic!("expected IO error: {err}")
        };
        assert_eq!(err.exit_code(), 1);
        assert_eq!(
            err.to_string(),
            format!(
                "copy task attachment {} -> {}: {cause}",
                source.display(),
                destination.display()
            )
        );
        assert!(h.tasks.is_empty());
    }

    #[tokio::test]
    async fn update_appends_attachments_rather_than_replacing_them() {
        let h = with_paths();
        let source_dir = tempfile::tempdir().unwrap();
        let first = source_dir.path().join("one.txt");
        let second = source_dir.path().join("two.txt");
        tokio::fs::write(&first, b"1").await.unwrap();
        tokio::fs::write(&second, b"2").await.unwrap();

        let task = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&first)]),
                ..input("t")
            })
            .await
            .unwrap();
        let updated = h
            .service
            .update(
                &task.id,
                UpdateTaskFields {
                    attachments: Some(vec![second]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            updated.attachments.as_deref(),
            Some(["one.txt".to_owned(), "two.txt".to_owned()].as_slice())
        );
    }

    #[tokio::test]
    async fn delete_removes_the_attachments_directory_and_tolerates_its_absence() {
        let h = with_paths();
        let source_dir = tempfile::tempdir().unwrap();
        let file = source_dir.path().join("a.txt");
        tokio::fs::write(&file, b"a").await.unwrap();

        let with_attachment = h
            .service
            .create(CreateTaskInput {
                attachments: Some(vec![path_str(&file)]),
                ..input("t")
            })
            .await
            .unwrap();
        let dir = h.service.attachment_path(&with_attachment.id, "").unwrap();
        assert!(dir.parent().is_some_and(Path::exists));

        h.service.delete(&with_attachment.id).await.unwrap();
        assert!(
            !h.service
                .attachment_path(&with_attachment.id, "a.txt")
                .unwrap()
                .exists()
        );

        // A task that never had attachments deletes cleanly too.
        let plain = h.service.create(input("t")).await.unwrap();
        h.service.delete(&plain.id).await.unwrap();
    }

    #[tokio::test]
    async fn attachment_path_is_under_the_attachments_directory_and_rejects_traversal() {
        let h = with_paths();
        let paths = Paths::new(h._temp.as_ref().unwrap().path());
        let task_dir = paths.task_attachments_dir("tsk_1").unwrap();
        assert!(is_inside(&task_dir, &paths.attachments_dir()));
        assert_eq!(
            h.service.attachment_path("tsk_1", "spec.md").unwrap(),
            task_dir.join("spec.md")
        );

        // `sanitize_id` is the path-traversal defence.
        assert!(paths.task_attachments_dir("../etc").is_err());
        assert!(h.service.attachment_path("../etc", "passwd").is_err());
    }

    #[tokio::test]
    async fn attachment_path_requires_paths_to_be_configured() {
        let h = harness(vec![]);
        let err = h
            .service
            .attachment_path("tsk_1", "a.txt")
            .expect_err("no paths");
        assert_eq!(err.to_string(), "Paths not configured");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn a_proof_bearing_task_round_trips_through_update() {
        // `update` must not drop fields it does not touch — the orchestrator
        // relies on `proof` surviving a user edit.
        let h = seeded(vec![Task {
            proof: Some(TaskProof {
                branch: Some("orch/tsk_test1".into()),
                files_changed: vec!["src/a.rs".into()],
                ..TaskProof::default()
            }),
            ..make_task()
        }]);
        let updated = h
            .service
            .update(
                "tsk_test1",
                UpdateTaskFields {
                    title: Some("edited".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            updated.proof.unwrap().branch.as_deref(),
            Some("orch/tsk_test1")
        );
    }
}
