//! Goal tools preserve CLI projections and gate pause/force before Engine creation.
use crate::mcp::{
    projections,
    registry::{CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolMeta},
};
use orch_core::{Error, Result};
use orch_domain::CreateGoalInput;
use orch_engine::goal_service::UpdateGoalFields;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GoalStatus {
    Active,
    Paused,
    Achieved,
    Abandoned,
}
impl From<GoalStatus> for orch_domain::GoalStatus {
    fn from(value: GoalStatus) -> Self {
        match value {
            GoalStatus::Active => Self::Active,
            GoalStatus::Paused => Self::Paused,
            GoalStatus::Achieved => Self::Achieved,
            GoalStatus::Abandoned => Self::Abandoned,
        }
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalAddArgs {
    title: String,
    description: Option<String>,
    assignee: Option<String>,
}
impl ToolArgs for GoalAddArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalListArgs {
    status: Option<String>,
}
impl ToolArgs for GoalListArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalShowArgs {
    id: String,
}
impl ToolArgs for GoalShowArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalStatusArgs {
    id: String,
    status: GoalStatus,
    #[serde(default)]
    force: bool,
}
impl ToolArgs for GoalStatusArgs {
    fn validate(&self, _: &Identity, mode: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        if mode != Mode::AllowDestructive {
            let restricted = match (matches!(self.status, GoalStatus::Paused), self.force) {
                (true, true) => Some("status=paused and force=true"),
                (true, false) => Some("status=paused"),
                (false, true) => Some("force=true"),
                (false, false) => None,
            };
            if let Some(argument) = restricted {
                return Err(rmcp::ErrorData::invalid_params(
                    format!("{argument} requires --allow-destructive"),
                    None,
                ));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalUpdateArgs {
    id: String,
    title: Option<String>,
    description: Option<String>,
    assignee: Option<String>,
}
impl ToolArgs for GoalUpdateArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GoalDeleteArgs {
    id: String,
}
impl ToolArgs for GoalDeleteArgs {}

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
    registry.add(
        meta(
            "goal_add",
            "Create a goal (orch goal add).",
            Class::Mutate,
            false,
            false,
            true,
        ),
        add,
    )?;
    registry.add(meta("goal_list", "List goals with optional CLI-compatible filter (orch goal list). Empty means all; unknown status matches none.", Class::Read, false, true, false), list)?;
    registry.add(
        meta(
            "goal_show",
            "Read goal with all linked tasks and optional progress (orch goal show).",
            Class::Read,
            false,
            true,
            false,
        ),
        show,
    )?;
    registry.add(meta("goal_status", "Change goal lifecycle. Pausing can cancel all todo/retrying autonomous tasks for the goal's assignee, including other goals; pause and force require --allow-destructive.", Class::Mutate, true, false, true), status)?;
    registry.add(meta("goal_update", "Update goal text or assignee (orch goal update). An empty assignee clears it; omission preserves it.", Class::Mutate, true, false, true), update)?;
    registry.add(
        meta(
            "goal_delete",
            "Delete goal (orch goal delete).",
            Class::Destructive,
            true,
            false,
            false,
        ),
        delete,
    )?;
    Ok(())
}

async fn add(cx: Arc<CallContext>, args: GoalAddArgs) -> Result<Value> {
    let goal = cx
        .engine
        .goal_service
        .create(CreateGoalInput {
            title: args.title,
            description: args.description,
            assignee: args.assignee,
        })
        .await?;
    serde_json::to_value(goal).map_err(|_| Error::other("serialize goal"))
}
async fn list(cx: Arc<CallContext>, args: GoalListArgs) -> Result<Value> {
    projections::goals(&cx.engine, args.status.as_deref()).await
}
async fn show(cx: Arc<CallContext>, args: GoalShowArgs) -> Result<Value> {
    projections::goal(&cx.engine, &args.id).await
}
async fn status(cx: Arc<CallContext>, args: GoalStatusArgs) -> Result<Value> {
    let goal = cx
        .engine
        .goal_service
        .update_status(&args.id, args.status.into(), args.force)
        .await?;
    serde_json::to_value(goal).map_err(|_| Error::other("serialize goal"))
}
async fn update(cx: Arc<CallContext>, args: GoalUpdateArgs) -> Result<Value> {
    let goal = cx
        .engine
        .goal_service
        .update(
            &args.id,
            &UpdateGoalFields {
                title: args.title,
                description: args.description,
                assignee: args.assignee,
            },
        )
        .await?;
    serde_json::to_value(goal).map_err(|_| Error::other("serialize goal"))
}
async fn delete(cx: Arc<CallContext>, args: GoalDeleteArgs) -> Result<Value> {
    cx.engine.goal_service.delete(&args.id).await?;
    Ok(serde_json::json!({"deleted": args.id}))
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
    async fn all_six_goal_tools_validate_and_project_cli_shapes() {
        let (_dir, cx) = fixture().await;
        let goal = add(cx.clone(), args(json!({"title":"Ship","description":"g"})))
            .await
            .unwrap();
        let id = goal["id"].as_str().unwrap();
        assert_eq!(goal["status"], "active");
        assert!(goal.get("assignee").is_none());
        assert!(goal.get("tasks").is_none());
        assert_eq!(
            list(cx.clone(), args(json!({"status":""}))).await.unwrap(),
            json!([goal])
        );
        assert_eq!(
            list(cx.clone(), args(json!({"status":"unknown"})))
                .await
                .unwrap(),
            json!([])
        );
        let task = cx
            .engine
            .task_service
            .create(orch_domain::CreateTaskInput {
                title: "child".into(),
                goal_id: Some(id.to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        let shown = show(cx.clone(), args(json!({"id":id}))).await.unwrap();
        assert_eq!(shown["tasks"][0]["id"], task.id);
        assert!(shown.get("progress").is_none());
        cx.engine
            .context
            .set(&format!("{id}-progress"), "progress", None)
            .await
            .unwrap();
        let shown = show(cx.clone(), args(json!({"id":id}))).await.unwrap();
        assert_eq!(shown["progress"], "progress");
        let changed = update(
            cx.clone(),
            args(json!({"id":id,"title":"Ship now","assignee":""})),
        )
        .await
        .unwrap();
        assert_eq!(changed["title"], "Ship now");
        assert!(changed.get("assignee").is_none());
        let paused = status(cx.clone(), args(json!({"id":id,"status":"paused"})))
            .await
            .unwrap();
        assert_eq!(paused["status"], "paused");
        assert!(paused.get("tasks").is_none());
        assert_eq!(
            status(cx.clone(), args(json!({"id":id,"status":"active"})))
                .await
                .unwrap()["status"],
            "active"
        );
        assert_eq!(
            delete(cx.clone(), args(json!({"id":id}))).await.unwrap(),
            json!({"deleted":id})
        );
        assert!(show(cx, args(json!({"id":id}))).await.is_err());
    }
    #[test]
    fn status_pause_or_force_gate_is_static_and_names_each_restriction() {
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            for status in ["active", "paused", "achieved", "abandoned"] {
                for force in [false, true] {
                    let args: GoalStatusArgs = serde_json::from_value(
                        json!({"id":"unassigned-or-missing","status":status,"force":force}),
                    )
                    .unwrap();
                    let result = args.validate(&Identity::of(None), mode);
                    if mode != Mode::AllowDestructive && (status == "paused" || force) {
                        let err = result.unwrap_err();
                        assert_eq!(err.code.0, -32602);
                        assert!(err.message.contains("--allow-destructive"));
                        if status == "paused" {
                            assert!(err.message.contains("status=paused"));
                        }
                        if force {
                            assert!(err.message.contains("force=true"));
                        }
                    } else {
                        result.unwrap();
                    }
                }
            }
        }
        assert!(
            serde_json::from_value::<GoalStatusArgs>(json!({"id":"g","status":"unknown"})).is_err()
        );
        assert!(
            serde_json::from_value::<GoalStatusArgs>(json!({"id":"g","status":"active","extra":1}))
                .is_err()
        );
    }

    #[test]
    fn catalogue_modes_annotations_and_schemas() {
        let expected = [
            ("goal_add", Class::Mutate, false, false, true),
            ("goal_delete", Class::Destructive, true, false, false),
            ("goal_list", Class::Read, false, true, false),
            ("goal_show", Class::Read, false, true, false),
            ("goal_status", Class::Mutate, true, false, true),
            ("goal_update", Class::Mutate, true, false, true),
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
