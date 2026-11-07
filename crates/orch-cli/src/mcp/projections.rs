//! Read projections shared by the tools and the resources (design D4, D15).
//!
//! Each returns the JSON value the matching CLI command prints with `--json`
//! (parsed-value equal: same keys, same optional-key absence, same order).
//! None of them prints, dispatches an MCP call, or takes the server's
//! admission gate — the caller already holds it, which is what keeps a
//! resource read from deadlocking on itself.

use orch_core::error::{Error, Result};
use orch_core::storage::{GoalFilter, TaskFilter};
use orch_domain::{Agent, GoalStatus, TaskStatus};
use orch_engine::Engine;
use orch_infra::storage::remote::RemoteAgentDirectory;
use serde::Serialize;
use serde_json::Value;

fn to_value<T: Serialize + ?Sized>(value: &T, what: &str) -> Result<Value> {
    serde_json::to_value(value).map_err(|_| Error::other(format!("serialize {what}")))
}

/// `orch status --json`: `{tasks, agents, state}`. The agent list is local
/// only, as the CLI's is.
pub(crate) async fn status(engine: &Engine) -> Result<Value> {
    let tasks = engine.task_service.list(&TaskFilter::default()).await?;
    let agents = engine.agent_service.list().await?;
    let state = engine.state.read().await?;
    let mut object = serde_json::Map::new();
    object.insert("tasks".to_owned(), to_value(&tasks, "tasks")?);
    object.insert("agents".to_owned(), to_value(&agents, "agents")?);
    object.insert("state".to_owned(), to_value(&state, "state")?);
    Ok(Value::Object(object))
}

/// `orch task list [--status]`.
pub(crate) async fn tasks(engine: &Engine, status: Option<TaskStatus>) -> Result<Value> {
    let filter = TaskFilter {
        status,
        goal_id: None,
    };
    to_value(&engine.task_service.list(&filter).await?, "tasks")
}

/// `orch task show ID`.
pub(crate) async fn task(engine: &Engine, id: &str) -> Result<Value> {
    to_value(&engine.task_service.get(id).await?, "task")
}

/// `orch goal list [--status]`, with the CLI's filter semantics: absent or
/// empty is every goal, and an unknown status matches nothing rather than
/// failing.
pub(crate) async fn goals(engine: &Engine, status: Option<&str>) -> Result<Value> {
    let goals = match status {
        None | Some("") => engine.goal_service.list(&GoalFilter::default()).await?,
        Some(raw) => match raw.parse::<GoalStatus>() {
            Ok(status) => {
                engine
                    .goal_service
                    .list(&GoalFilter::with_status(status))
                    .await?
            }
            Err(()) => Vec::new(),
        },
    };
    to_value(&goals, "goals")
}

/// `orch goal show ID`: the goal's fields plus `tasks`, and `progress` only
/// when there is a report (absent, never null).
pub(crate) async fn goal(engine: &Engine, id: &str) -> Result<Value> {
    let goal = engine.goal_service.get(id).await?;
    let tasks = engine.goal_service.list_tasks_for_goal(id).await?;
    let progress = engine.goal_service.get_progress_report(id).await?;
    let mut merged = to_value(&goal, "goal")?;
    let object = merged
        .as_object_mut()
        .ok_or_else(|| Error::other("serialize goal"))?;
    object.insert("tasks".to_owned(), to_value(&tasks, "tasks")?);
    if let Some(progress) = progress {
        object.insert("progress".to_owned(), Value::String(progress));
    }
    Ok(merged)
}

/// `orch agent list`: local agents plus remote snapshots.
pub(crate) async fn agents(engine: &Engine) -> Result<Value> {
    let agents = RemoteAgentDirectory::new(engine.paths.clone())
        .list(engine.agent_service.list().await?)
        .await?;
    to_value(&agents, "agents")
}

/// `orch agent show ID [--effective]`. A missing agent is the CLI's own
/// error.
#[allow(dead_code, reason = "seam consumed by the B2 tool groups")]
pub(crate) async fn agent(engine: &Engine, id: &str, effective: bool) -> Result<Value> {
    find_agent(engine, id, effective)
        .await?
        .ok_or_else(|| Error::invalid_arguments(format!("Agent not found: {id}")))
}

/// [`agent`], with a missing agent as `None` so the resource can report it
/// as not found.
pub(crate) async fn find_agent(
    engine: &Engine,
    id: &str,
    effective: bool,
) -> Result<Option<Value>> {
    let Some(agent) = resolve_agent(engine, id).await? else {
        return Ok(None);
    };
    let value = if effective {
        to_value(
            &crate::commands::agent::EffectiveAgent::of(engine, &agent),
            "agent",
        )?
    } else {
        to_value(&agent, "agent")?
    };
    Ok(Some(value))
}

async fn resolve_agent(engine: &Engine, id: &str) -> Result<Option<Agent>> {
    RemoteAgentDirectory::new(engine.paths.clone())
        .resolve(id, engine.agent_service.list().await?)
        .await
}

/// `orch context list`: live entries (expired ones are purged by the read).
pub(crate) async fn context(engine: &Engine) -> Result<Value> {
    to_value(&engine.context.list().await?, "context")
}

/// `orch msg inbox AGENT`: pending messages, without marking them delivered.
pub(crate) async fn inbox(engine: &Engine, agent_id: &str) -> Result<Value> {
    to_value(
        &engine
            .message_service
            .list_pending_for_agent(agent_id)
            .await?,
        "messages",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::CreateTaskInput;

    async fn engine(dir: &std::path::Path) -> Engine {
        std::fs::create_dir_all(dir.join(".orchestry")).unwrap();
        orch_engine::build_light_with(
            dir,
            orch_engine::EngineSeams {
                global_config_path: Some(dir.join(".orchestry").join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn projections_match_the_cli_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path()).await;
        let task = engine
            .task_service
            .create(CreateTaskInput {
                title: "t".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let status = status(&engine).await.unwrap();
        let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["tasks", "agents", "state"]);
        assert_eq!(status["tasks"][0]["id"], Value::String(task.id.clone()));

        assert_eq!(
            tasks(&engine, None)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            tasks(&engine, Some(TaskStatus::Done)).await.unwrap(),
            Value::Array(vec![])
        );
        assert_eq!(super::task(&engine, &task.id).await.unwrap()["title"], "t");
        assert!(super::task(&engine, "tsk_missing").await.is_err());

        // goal list keeps the CLI's lenient filter.
        assert_eq!(
            goals(&engine, Some("nonsense")).await.unwrap(),
            Value::Array(vec![])
        );
        assert_eq!(
            goals(&engine, Some("")).await.unwrap(),
            Value::Array(vec![])
        );

        let err = agent(&engine, "agt_missing", false)
            .await
            .expect_err("missing");
        assert_eq!(err.to_string(), "Agent not found: agt_missing");
        assert_eq!(
            find_agent(&engine, "agt_missing", false).await.unwrap(),
            None
        );
        assert_eq!(agents(&engine).await.unwrap(), Value::Array(vec![]));
        assert_eq!(context(&engine).await.unwrap(), Value::Array(vec![]));
        assert_eq!(inbox(&engine, "agt_x").await.unwrap(), Value::Array(vec![]));
    }

    #[tokio::test]
    async fn goal_show_omits_progress_when_absent_and_lists_linked_tasks() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine(temp.path()).await;
        let goal_entity = engine
            .goal_service
            .create(orch_domain::CreateGoalInput {
                title: "g".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let value = goal(&engine, &goal_entity.id).await.unwrap();
        assert_eq!(value["title"], "g");
        assert_eq!(value["tasks"], Value::Array(vec![]));
        assert!(value.get("progress").is_none(), "{value}");
    }
}
