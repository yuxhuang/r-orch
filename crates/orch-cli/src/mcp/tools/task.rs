//! Task tools: one daemon request each. Mutations return the task as the
//! daemon committed it, inside the same operation.
use crate::document_input;
use crate::mcp::{
    projections::{self, id},
    registry::{
        CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolError, ToolMeta,
        ToolResult, json, wire,
    },
};
use orch_core::Result;
use orch_domain::ReviewCriterion;
use orch_proto::ops::args::{self as rpc, MutationKind};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
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
    registry.add(meta("task_cancel", "Cancel a task (orch task cancel). The daemon stops a running local agent before the task is cancelled.", Class::Destructive, true, false, true), cancel)?;
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
async fn add(cx: Arc<CallContext>, args: TaskAddArgs) -> ToolResult {
    let review_criteria = args
        .review_criteria
        .as_deref()
        .map(|raw| {
            csv(raw)
                .into_iter()
                .map(|value| {
                    value.parse::<ReviewCriterion>().map_err(|()| {
                        ToolError::Text(format!(
                            "Unknown review criterion: \"{value}\". Use test_pass, typecheck or lint."
                        ))
                    })
                })
                .collect::<ToolResult<Vec<_>>>()
        })
        .transpose()?;
    // Relative paths name files under the project root, never the bridge's
    // working directory (D12); absolute paths pass through. The daemon reads
    // them; resolve here, before creating anything, and reject paths that
    // cannot be represented intact.
    let attachments = args
        .attach
        .as_deref()
        .map(|raw| {
            csv(raw)
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(|part| {
                    cx.project_root
                        .join(part)
                        .into_os_string()
                        .into_string()
                        .map_err(|_| {
                            ToolError::Text(
                                "project root is not valid UTF-8; attachments cannot be resolved"
                                    .to_owned(),
                            )
                        })
                })
                .collect::<ToolResult<Vec<_>>>()
        })
        .transpose()?;
    let task = cx
        .request(wire::<rpc::TaskCreateArgs>(json!({
            "title": args.title,
            "description": args.description,
            "priority": args.priority,
            "assignee": args.assignee,
            "labels": args.labels.as_deref().map(csv),
            "depends_on": args.depends_on.as_deref().map(csv),
            "max_attempts": args.max_attempts,
            "workspace_mode": args.workspace_mode.map(orch_domain::WorkspaceMode::from),
            "review_criteria": review_criteria,
            "scope": args.scope.as_deref().map(csv),
            "goal_id": args.goal_id,
            "attachments": attachments,
        }))?)
        .await?;
    json(&task, "task")
}
async fn list(cx: Arc<CallContext>, args: TaskListArgs) -> ToolResult {
    projections::tasks(&cx, args.status.map(Into::into)).await
}
async fn show(cx: Arc<CallContext>, args: TaskShowArgs) -> ToolResult {
    projections::task(&cx, &args.id).await
}
async fn assign(cx: Arc<CallContext>, args: TaskAssignArgs) -> ToolResult {
    let task = cx
        .request(rpc::TaskAssignArgs {
            id: id(&args.task_id)?,
            agent_id: id(&args.agent_id)?,
        })
        .await?;
    json(&task, "task")
}
async fn cancel(cx: Arc<CallContext>, args: TaskCancelArgs) -> ToolResult {
    json(
        &cx.request(rpc::TaskCancelArgs { id: id(&args.id)? }).await?,
        "task",
    )
}
async fn approve(cx: Arc<CallContext>, args: TaskApproveArgs) -> ToolResult {
    json(
        &cx.request(rpc::TaskApproveArgs { id: id(&args.id)? }).await?,
        "task",
    )
}
async fn reject(cx: Arc<CallContext>, args: TaskRejectArgs) -> ToolResult {
    let task = cx
        .request(wire::<rpc::TaskRejectArgs>(
            json!({"id": args.id, "feedback": args.reason}),
        )?)
        .await?;
    json(&task, "task")
}
async fn retry(cx: Arc<CallContext>, args: TaskRetryArgs) -> ToolResult {
    json(
        &cx.request(rpc::TaskRetryArgs { id: id(&args.id)? }).await?,
        "task",
    )
}

/// A definition file's document, read here and sent as JSON text. The daemon
/// validates and applies it; its argument errors name the file, as the CLI's
/// do.
pub(crate) async fn definition_input(
    cx: &CallContext,
    file: &str,
    format: Option<document_input::InputFormat>,
    kind: MutationKind,
) -> ToolResult<(std::path::PathBuf, rpc::DefinitionInput)> {
    let path = cx.project_root.join(file);
    let document = document_input::read_document(&path, format).await?;
    let text = serde_json::to_string(&document)
        .map_err(|_| ToolError::Text("serialize the definition".to_owned()))?;
    // A document that would crowd the 1 MiB request frame travels as a
    // transfer on this call's session instead (design D6).
    let input = if text.len() > INLINE_DOCUMENT_BYTES {
        let token = cx
            .upload(rpc::TransferPurpose::Document, text.as_bytes())
            .await
            .map_err(|err| at_source(&path, err))?;
        json!({"input": {"input_transfer": token, "format": "json"}, "kind": kind})
    } else {
        json!({"input": {"document": text, "format": "json"}, "kind": kind})
    };
    let input = wire(input).map_err(|err| at_source(&path, err))?;
    Ok((path, input))
}

/// Documents above this many bytes of JSON are uploaded, not inlined.
const INLINE_DOCUMENT_BYTES: usize = 256 * 1024;

/// `<file>: <reason>` for an argument error, as `document_input::at_source`.
pub(crate) fn at_source(path: &std::path::Path, err: ToolError) -> ToolError {
    use orch_client::ClientError;
    use orch_proto::error::ErrorCode;
    match err {
        ToolError::Client(ClientError::Remote(body)) if body.code == ErrorCode::InvalidArguments => {
            ToolError::Text(format!("{}: {}", path.display(), body.message.as_str()))
        }
        ToolError::Text(text) => ToolError::Text(format!("{}: {text}", path.display())),
        other => other,
    }
}

async fn definition(
    cx: Arc<CallContext>,
    task_id: String,
    file: String,
    input_format: Option<Format>,
    kind: MutationKind,
) -> ToolResult {
    let (path, definition) =
        definition_input(&cx, &file, input_format.map(Into::into), kind).await?;
    let task = cx
        .request(rpc::TaskDefinitionApplyArgs {
            id: id(&task_id)?,
            definition,
        })
        .await
        .map_err(|err| at_source(&path, err))?;
    json(&task, "task")
}
async fn update(cx: Arc<CallContext>, args: TaskUpdateArgs) -> ToolResult {
    definition(
        cx,
        args.id,
        args.file,
        args.input_format,
        MutationKind::Update,
    )
    .await
}
async fn patch(cx: Arc<CallContext>, args: TaskPatchArgs) -> ToolResult {
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
    use serde_json::{Value, json};
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(Some("cli")), Mode::AllowDestructive)
            .unwrap();
        args
    }
    async fn fixture() -> (crate::mcp::testing::Fixture, Arc<CallContext>) {
        let fx = crate::mcp::testing::Fixture::start().await;
        let cx = fx.context(Some("cli"), Mode::AllowDestructive);
        (fx, cx)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn task_add_rejects_non_utf8_attachment_root_without_creating_a_task() {
        use std::os::unix::ffi::OsStrExt;

        let (fx, _) = fixture().await;
        let cx = Arc::new(CallContext::new(
            fx.connector(Mode::AllowDestructive, None),
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff-project")),
            Identity::of(Some("cli")),
            true,
        ));
        let result = add(
            cx.clone(),
            args(json!({"title":"t", "attach":"relative.txt"})),
        )
        .await;
        let err = result.unwrap_err();
        assert!(matches!(err, ToolError::Text(_)), "{err:?}");
        let message = err.to_string();
        assert_eq!(
            crate::mcp::registry::encode(Err(err), false).is_error,
            Some(true)
        );
        assert!(
            message.contains("project root is not valid UTF-8"),
            "{message}"
        );
        assert!(
            message.contains("attachments cannot be resolved"),
            "{message}"
        );
        assert!(
            fx.engine()
                .tasks
                .list(&Default::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn task_add_list_show_assign_preserve_cli_fields_and_defaults() {
        let (fx, cx) = fixture().await;
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
        let agent = fx.engine()
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
    async fn lifecycle_tools_return_the_committed_task() {
        let (fx, cx) = fixture().await;
        let task = add(cx.clone(), args(json!({"title":"running"})))
            .await
            .unwrap();
        let id = task["id"].as_str().unwrap();
        fx.engine()
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        let cancelled = cancel(cx.clone(), args(json!({"id":id}))).await.unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled, projections::task(&cx, id).await.unwrap());
        assert_eq!(
            retry(cx.clone(), args(json!({"id":id}))).await.unwrap()["status"],
            "todo"
        );
        fx.engine()
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        fx.engine()
            .task_service
            .update_status(id, orch_domain::TaskStatus::Review)
            .await
            .unwrap();
        let rejected = reject(cx.clone(), args(json!({"id":id,"reason":"fix tests"})))
            .await
            .unwrap();
        assert_eq!(rejected["feedback"], "fix tests");
        assert_eq!(rejected["status"], "todo");
        fx.engine()
            .task_service
            .update_status(id, orch_domain::TaskStatus::InProgress)
            .await
            .unwrap();
        fx.engine()
            .task_service
            .update_status(id, orch_domain::TaskStatus::Review)
            .await
            .unwrap();
        assert_eq!(
            approve(cx.clone(), args(json!({"id":id}))).await.unwrap()["status"],
            "done"
        );
        let done = cancel(cx.clone(), args(json!({"id":id}))).await.unwrap_err();
        assert!(matches!(done, ToolError::Client(_)), "{done:?}");
    }
    #[tokio::test]
    async fn update_and_patch_use_fixed_project_relative_files_and_shared_validation() {
        let (fx, cx) = fixture().await;
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
    fn definition_and_typed_argument_validation_runs_before_the_daemon_is_contacted() {
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
}
