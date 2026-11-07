//! Task tools share authored-definition validation and use only light services.
use crate::document_input::{self, MutationKind};
use crate::mcp::{
    projections,
    registry::{CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolMeta},
};
use orch_core::{Error, Result};
use orch_domain::{CreateTaskInput, ReviewCriterion};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Todo,
    InProgress,
    Retrying,
    Review,
    Done,
    Failed,
    Cancelled,
}
impl From<TaskStatus> for orch_domain::TaskStatus {
    fn from(value: TaskStatus) -> Self {
        match value {
            TaskStatus::Todo => Self::Todo,
            TaskStatus::InProgress => Self::InProgress,
            TaskStatus::Retrying => Self::Retrying,
            TaskStatus::Review => Self::Review,
            TaskStatus::Done => Self::Done,
            TaskStatus::Failed => Self::Failed,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }
}
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Workspace {
    Shared,
    Worktree,
    Isolated,
}
impl From<Workspace> for orch_domain::WorkspaceMode {
    fn from(value: Workspace) -> Self {
        match value {
            Workspace::Shared => Self::Shared,
            Workspace::Worktree => Self::Worktree,
            Workspace::Isolated => Self::Isolated,
        }
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskAddArgs {
    title: String,
    description: Option<String>,
    #[serde(default = "default_priority")]
    priority: i64,
    /// Comma-separated labels; empty elements are retained.
    labels: Option<String>,
    depends_on: Option<String>,
    max_attempts: Option<u32>,
    workspace_mode: Option<Workspace>,
    assignee: Option<String>,
    /// Comma-separated test_pass, typecheck, lint.
    review_criteria: Option<String>,
    scope: Option<String>,
    goal_id: Option<String>,
    /// Comma-separated attachment paths; empty elements are dropped.
    attach: Option<String>,
}
impl ToolArgs for TaskAddArgs {}
fn default_priority() -> i64 {
    3
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskListArgs {
    status: Option<TaskStatus>,
}
impl ToolArgs for TaskListArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskShowArgs {
    id: String,
}
impl ToolArgs for TaskShowArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskAssignArgs {
    task_id: String,
    agent_id: String,
}
impl ToolArgs for TaskAssignArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskCancelArgs {
    id: String,
}
impl ToolArgs for TaskCancelArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskApproveArgs {
    id: String,
}
impl ToolArgs for TaskApproveArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskRejectArgs {
    id: String,
    reason: Option<String>,
}
impl ToolArgs for TaskRejectArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskRetryArgs {
    id: String,
}
impl ToolArgs for TaskRetryArgs {}
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Format {
    Json,
    Yaml,
}
impl From<Format> for document_input::InputFormat {
    fn from(value: Format) -> Self {
        match value {
            Format::Json => Self::Json,
            Format::Yaml => Self::Yaml,
        }
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskUpdateArgs {
    id: String,
    file: String,
    input_format: Option<Format>,
}
impl ToolArgs for TaskUpdateArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        validate_file(&self.file)
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskPatchArgs {
    id: String,
    file: String,
    input_format: Option<Format>,
}
impl ToolArgs for TaskPatchArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        validate_file(&self.file)
    }
}

fn validate_file(file: &str) -> std::result::Result<(), rmcp::ErrorData> {
    if file.is_empty() || file == "-" {
        Err(rmcp::ErrorData::invalid_params(
            "file must be a nonempty regular file path; '-' would consume protocol stdin",
            None,
        ))
    } else {
        Ok(())
    }
}

fn meta(
    name: &'static str,
    description: &'static str,
    class: Class,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
) -> ToolMeta {
    ToolMeta {
        name,
        description,
        class,
        execution: Execution::Store,
        destructive_hint,
        idempotent_hint,
        open_world_hint,
    }
}
pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(meta("task_add", "Create a task (orch task add). List fields are comma-separated; attach drops empty paths.", Class::Mutate, false, false, true), add)?;
    registry.add(
        meta(
            "task_list",
            "List tasks with an optional status (orch task list).",
            Class::Read,
            false,
            true,
            false,
        ),
        list,
    )?;
    registry.add(
        meta(
            "task_show",
            "Read one complete task (orch task show).",
            Class::Read,
            false,
            true,
            false,
        ),
        show,
    )?;
    registry.add(
        meta(
            "task_assign",
            "Assign task to an agent (orch task assign).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        assign,
    )?;
    registry.add(meta("task_cancel", "Mark a task cancelled in storage (orch task cancel). A live owner stops the run on reconciliation; without an owner this does not kill an orphan process.", Class::Destructive, true, false, true), cancel)?;
    registry.add(
        meta(
            "task_approve",
            "Approve review or release a held task (orch task approve).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        approve,
    )?;
    registry.add(
        meta(
            "task_reject",
            "Return task for rework (orch task reject).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        reject,
    )?;
    registry.add(
        meta(
            "task_retry",
            "Reset a failed or cancelled task for retry (orch task retry).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        retry,
    )?;
    registry.add(meta("task_update", "Replace authored task definition from a local file (orch task update). Protocol stdin is unavailable.", Class::Mutate, true, false, true), update)?;
    registry.add(meta("task_patch", "Merge-patch authored task definition from a local file (orch task patch). Protocol stdin is unavailable.", Class::Mutate, true, false, true), patch)?;
    Ok(())
}

fn csv(raw: &str) -> Vec<String> {
    raw.split(',').map(|part| part.trim().to_owned()).collect()
}
async fn add(cx: Arc<CallContext>, args: TaskAddArgs) -> Result<Value> {
    let review_criteria = args.review_criteria.as_deref().map(|raw| csv(raw).into_iter().map(|value| value.parse::<ReviewCriterion>().map_err(|()| Error::invalid_arguments(format!("Unknown review criterion: \"{value}\". Use test_pass, typecheck or lint.")))).collect::<Result<Vec<_>>>()).transpose()?;
    let task = cx
        .engine
        .task_service
        .create(CreateTaskInput {
            title: args.title,
            description: args.description,
            priority: Some(args.priority),
            assignee: args.assignee,
            labels: args.labels.as_deref().map(csv),
            depends_on: args.depends_on.as_deref().map(csv),
            max_attempts: args.max_attempts,
            workspace_mode: args.workspace_mode.map(Into::into),
            review_criteria,
            scope: args.scope.as_deref().map(csv),
            goal_id: args.goal_id,
            attachments: args.attach.as_deref().map(|raw| {
                csv(raw)
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect()
            }),
        })
        .await?;
    serde_json::to_value(task).map_err(|_| Error::other("serialize task"))
}
async fn list(cx: Arc<CallContext>, args: TaskListArgs) -> Result<Value> {
    projections::tasks(&cx.engine, args.status.map(Into::into)).await
}
async fn show(cx: Arc<CallContext>, args: TaskShowArgs) -> Result<Value> {
    projections::task(&cx.engine, &args.id).await
}
async fn assign(cx: Arc<CallContext>, args: TaskAssignArgs) -> Result<Value> {
    let task = cx
        .engine
        .task_service
        .assign(&args.task_id, &args.agent_id)
        .await?;
    serde_json::to_value(task).map_err(|_| Error::other("serialize task"))
}
async fn reread(cx: &CallContext, id: &str) -> Result<Value> {
    projections::task(&cx.engine, id).await.map_err(|err| {
        Error::other(format!(
            "Task mutation may already have committed; failed to re-read {id}: {err}"
        ))
    })
}
async fn cancel(cx: Arc<CallContext>, args: TaskCancelArgs) -> Result<Value> {
    cx.engine.task_service.cancel(&args.id).await?;
    reread(&cx, &args.id).await
}
async fn approve(cx: Arc<CallContext>, args: TaskApproveArgs) -> Result<Value> {
    cx.engine.task_service.approve(&args.id).await?;
    reread(&cx, &args.id).await
}
async fn reject(cx: Arc<CallContext>, args: TaskRejectArgs) -> Result<Value> {
    cx.engine.task_service.reject(&args.id, args.reason).await?;
    reread(&cx, &args.id).await
}
async fn retry(cx: Arc<CallContext>, args: TaskRetryArgs) -> Result<Value> {
    cx.engine.task_service.retry(&args.id).await?;
    reread(&cx, &args.id).await
}

async fn definition(
    cx: Arc<CallContext>,
    id: String,
    file: String,
    input_format: Option<Format>,
    kind: MutationKind,
) -> Result<Value> {
    let file = cx.project_root.join(file);
    let input = document_input::read_document(&file, input_format.map(Into::into)).await?;
    let task = crate::commands::task::apply_definition(&cx.engine, &id, &input, kind, &|err| {
        document_input::at_source(&file, err)
    })
    .await?;
    serde_json::to_value(task).map_err(|_| Error::other("serialize task"))
}
async fn update(cx: Arc<CallContext>, args: TaskUpdateArgs) -> Result<Value> {
    definition(
        cx,
        args.id,
        args.file,
        args.input_format,
        MutationKind::Update,
    )
    .await
}
async fn patch(cx: Arc<CallContext>, args: TaskPatchArgs) -> Result<Value> {
    definition(
        cx,
        args.id,
        args.file,
        args.input_format,
        MutationKind::Patch,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(Some("cli")), Mode::AllowDestructive)
            .unwrap();
        args
    }
    async fn fixture() -> (tempfile::TempDir, Arc<CallContext>) {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join(".orchestry"))
            .await
            .unwrap();
        let engine = orch_engine::build_light_with(
            dir.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(dir.path().join(".orchestry/global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let cx = Arc::new(CallContext {
            engine: Arc::new(engine),
            project_root: dir.path().to_owned(),
            identity: Identity::of(Some("cli")),
            mode: Mode::AllowDestructive,
        });
        (dir, cx)
    }

    #[tokio::test]
    async fn task_add_list_show_assign_preserve_cli_fields_and_defaults() {
        let (_dir, cx) = fixture().await;
        let task=add(cx.clone(),args(json!({"title":"t","labels":"a,, b","scope":"src/**, docs/**","workspace_mode":"shared","review_criteria":"test_pass,lint","attach":", ,"}))).await.unwrap();
        let id = task["id"].as_str().unwrap();
        assert_eq!(task["priority"], 3);
        assert_eq!(task["labels"], json!(["a", "", "b"]));
        assert_eq!(task["scope"], json!(["src/**", "docs/**"]));
        assert_eq!(task["workspace_mode"], "shared");
        assert_eq!(task["review_criteria"], json!(["test_pass", "lint"]));
        assert!(
            task.get("attachments")
                .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
        );
        assert_eq!(
            show(cx.clone(), args(json!({"id":id}))).await.unwrap(),
            task
        );
        assert_eq!(
            list(cx.clone(), args(json!({}))).await.unwrap(),
            json!([task])
        );
        assert_eq!(
            list(cx.clone(), args(json!({"status":"done"})))
                .await
                .unwrap(),
            json!([])
        );
        let agent = cx
            .engine
            .agent_service
            .create(orch_domain::CreateAgentInput {
                name: "worker".into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            assign(
                cx.clone(),
                args(json!({"task_id":id,"agent_id":agent.name}))
            )
            .await
            .unwrap()["assignee"],
            agent.id
        );
        assert!(
            add(cx.clone(), args(json!({"title":"bad","priority":0})))
                .await
                .is_err()
        );
        assert!(
            add(cx, args(json!({"title":"bad","review_criteria":"other"})))
                .await
                .unwrap_err()
                .to_string()
                .contains("Unknown review criterion")
        );
    }
    #[tokio::test]
    async fn lifecycle_tools_return_reread_tasks_and_store_only_cancel_handles_running() {
        let (_dir, cx) = fixture().await;
        let task = add(cx.clone(), args(json!({"title":"running"})))
            .await
            .unwrap();
        let id = task["id"].as_str().unwrap();
        cx.engine
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        let cancelled = cancel(cx.clone(), args(json!({"id":id}))).await.unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled, projections::task(&cx.engine, id).await.unwrap());
        assert_eq!(
            retry(cx.clone(), args(json!({"id":id}))).await.unwrap()["status"],
            "todo"
        );
        cx.engine
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        cx.engine
            .task_service
            .update_status(id, orch_domain::TaskStatus::Review)
            .await
            .unwrap();
        let rejected = reject(cx.clone(), args(json!({"id":id,"reason":"fix tests"})))
            .await
            .unwrap();
        assert_eq!(rejected["feedback"], "fix tests");
        assert_eq!(rejected["status"], "todo");
        cx.engine
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        cx.engine
            .task_service
            .update_status(id, orch_domain::TaskStatus::Review)
            .await
            .unwrap();
        assert_eq!(
            approve(cx.clone(), args(json!({"id":id}))).await.unwrap()["status"],
            "done"
        );
        assert!(cancel(cx.clone(), args(json!({"id":id}))).await.is_err());
        cx.engine.tasks.delete(id).await.unwrap();
        let err = reread(&cx, id).await.unwrap_err().to_string();
        assert!(err.contains("may already have committed"));
        assert!(err.contains(id));
        // Light Engine has no owner/process lifecycle: cancelling never creates a PID lock.
        assert!(!cx.engine.paths.lock_path().exists());
    }
    #[tokio::test]
    async fn update_and_patch_use_fixed_project_relative_files_and_shared_validation() {
        let (_dir, cx) = fixture().await;
        let task = add(
            cx.clone(),
            args(json!({"title":"old","description":"old","labels":"old"})),
        )
        .await
        .unwrap();
        let id = task["id"].as_str().unwrap();
        tokio::fs::write(
            cx.project_root.join("replace.json"),
            r#"{"title":"new","description":"new d","priority":2}"#,
        )
        .await
        .unwrap();
        let updated = update(
            cx.clone(),
            args(json!({"id":id,"file":"replace.json","input_format":"json"})),
        )
        .await
        .unwrap();
        assert_eq!(updated["title"], "new");
        assert_eq!(updated["labels"], json!([]));
        tokio::fs::write(cx.project_root.join("patch.yaml"), "description: patched\n")
            .await
            .unwrap();
        let patched = patch(cx.clone(), args(json!({"id":id,"file":"patch.yaml"})))
            .await
            .unwrap();
        assert_eq!(patched["title"], "new");
        assert_eq!(patched["description"], "patched");
        tokio::fs::write(cx.project_root.join("bad.json"), r#"{"status":"done"}"#)
            .await
            .unwrap();
        let err = patch(cx, args(json!({"id":id,"file":"bad.json"})))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("bad.json"));
        assert!(err.contains("read-only field"));
    }
    #[test]
    fn definition_and_typed_argument_validation_runs_before_any_engine() {
        for file in ["-", ""] {
            let update: TaskUpdateArgs =
                serde_json::from_value(json!({"id":"t","file":file})).unwrap();
            let patch: TaskPatchArgs =
                serde_json::from_value(json!({"id":"t","file":file})).unwrap();
            for result in [
                update.validate(&Identity::of(None), Mode::Default),
                patch.validate(&Identity::of(None), Mode::Default),
            ] {
                let err = result.unwrap_err();
                assert_eq!(err.code.0, -32602);
                assert!(err.message.contains("file"));
            }
        }
        assert!(serde_json::from_value::<TaskListArgs>(json!({"status":"other"})).is_err());
        assert!(
            serde_json::from_value::<TaskAddArgs>(json!({"title":"t","priority":"3"})).is_err()
        );
        assert!(
            serde_json::from_value::<TaskAddArgs>(json!({"title":"t","workspace_mode":"other"}))
                .is_err()
        );
        assert!(serde_json::from_value::<TaskShowArgs>(json!({"id":"t","extra":1})).is_err());
        let _: TaskRejectArgs = args(json!({"id":"t"}));
    }

    #[test]
    fn catalogue_modes_annotations_and_schemas() {
        let expected = [
            ("task_add", Class::Mutate, false, false, true),
            ("task_approve", Class::Mutate, true, false, true),
            ("task_assign", Class::Mutate, true, false, true),
            ("task_cancel", Class::Destructive, true, false, true),
            ("task_list", Class::Read, false, true, false),
            ("task_patch", Class::Mutate, true, false, true),
            ("task_reject", Class::Mutate, true, false, true),
            ("task_retry", Class::Mutate, true, false, true),
            ("task_show", Class::Read, false, true, false),
            ("task_update", Class::Mutate, true, false, true),
        ];
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let mut registry = Registry::new(mode);
            register(&mut registry).unwrap();
            let tools = registry.into_router().list_all();
            let wanted: Vec<_> = expected
                .iter()
                .filter(|(_, class, _, _, _)| mode.allows(*class))
                .collect();
            assert_eq!(tools.len(), wanted.len());
            for (tool, (name, class, destructive, idempotent, open)) in tools.iter().zip(wanted) {
                assert_eq!(tool.name.as_ref(), *name);
                let value = serde_json::to_value(tool).unwrap();
                assert_eq!(
                    value["annotations"],
                    json!({"readOnlyHint": *class == Class::Read, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": open})
                );
                assert_eq!(value["inputSchema"]["additionalProperties"], false);
            }
        }
    }
    // Emulate another writer changing/removing the entity between save and read,
    // without timing or a real second process. Returning the service snapshot
    // instead of re-reading must fail these tests.
    struct AfterSave {
        inner: Arc<dyn orch_core::storage::TaskStore>,
        remove: bool,
    }
    #[async_trait::async_trait]
    impl orch_core::storage::TaskStore for AfterSave {
        async fn list(
            &self,
            filter: &orch_core::storage::TaskFilter,
        ) -> Result<Vec<orch_domain::Task>> {
            self.inner.list(filter).await
        }
        async fn get(&self, id: &str) -> Result<Option<orch_domain::Task>> {
            self.inner.get(id).await
        }
        async fn save(&self, task: &orch_domain::Task) -> Result<()> {
            let mut changed = task.clone();
            changed.description = "changed after service snapshot".into();
            self.inner.save(&changed).await?;
            if self.remove {
                self.inner.delete(&task.id).await?;
            }
            Ok(())
        }
        async fn delete(&self, id: &str) -> Result<()> {
            self.inner.delete(id).await
        }
    }
    #[tokio::test]
    async fn each_lifecycle_tool_rereads_and_discloses_postcommit_read_failure() {
        for name in ["cancel", "approve", "reject", "retry"] {
            for remove in [false, true] {
                let (_dir, mut cx) = fixture().await;
                let mut task = cx
                    .engine
                    .task_service
                    .create(CreateTaskInput {
                        title: "task".into(),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                task.status = match name {
                    "approve" | "reject" => orch_domain::TaskStatus::Review,
                    "retry" => orch_domain::TaskStatus::Cancelled,
                    _ => orch_domain::TaskStatus::InProgress,
                };
                cx.engine.tasks.save(&task).await.unwrap();
                let engine = Arc::get_mut(&mut Arc::get_mut(&mut cx).unwrap().engine).unwrap();
                let store = Arc::new(AfterSave {
                    inner: engine.tasks.clone(),
                    remove,
                });
                engine.tasks = store.clone();
                engine.task_service = Arc::new(orch_engine::task_service::TaskService::new(
                    store,
                    engine.agents.clone(),
                    engine.events.clone(),
                    engine.config.clone(),
                    engine.clock.clone(),
                    engine.ids.clone(),
                    Some(engine.paths.clone()),
                ));
                let value = json!({"id":task.id});
                let result = match name {
                    "cancel" => cancel(cx.clone(), args(value)).await,
                    "approve" => approve(cx.clone(), args(value)).await,
                    "reject" => reject(cx.clone(), args(value)).await,
                    _ => retry(cx.clone(), args(value)).await,
                };
                if remove {
                    let err = result.unwrap_err().to_string();
                    assert!(err.contains("may already have committed"), "{name}: {err}");
                    assert!(cx.engine.tasks.get(&task.id).await.unwrap().is_none());
                } else {
                    assert_eq!(
                        result.unwrap()["description"],
                        "changed after service snapshot",
                        "{name}"
                    );
                }
            }
        }
    }
}
