//! Read projections shared by the tools and the resources (design D4, D15).
//!
//! Each returns the JSON value the matching CLI command prints with `--json`
//! (parsed-value equal: same keys, same optional-key absence, same order),
//! from one daemon read. None of them takes the bridge's admission permit —
//! the caller already holds it, which is what keeps a resource read from
//! waiting on itself.

use orch_domain::{GoalStatus, TaskStatus};
use orch_proto::bounded::Id;
use orch_proto::ops::args;
use serde_json::Value;

use super::registry::{CallContext, ToolResult, json, wire};

/// An entity id as the wire type; an over-long one is refused locally.
pub(crate) fn id(value: &str) -> ToolResult<Id> {
    wire(Value::String(value.to_owned()))
}

/// `orch status --json`: `{tasks, agents, state}`. The agent list is local
/// only, as the CLI's is.
pub(crate) async fn status(cx: &CallContext) -> ToolResult {
    let status = cx.request(args::StatusGetArgs {}).await?;
    let mut object = serde_json::Map::new();
    object.insert("tasks".to_owned(), json(&status.tasks, "tasks")?);
    object.insert("agents".to_owned(), json(&status.agents, "agents")?);
    object.insert("state".to_owned(), json(&status.state, "state")?);
    Ok(Value::Object(object))
}

/// `orch task list [--status]`.
pub(crate) async fn tasks(cx: &CallContext, status: Option<TaskStatus>) -> ToolResult {
    let tasks = cx
        .request(args::TaskListArgs {
            status,
            goal_id: None,
        })
        .await?;
    json(&tasks, "tasks")
}

/// `orch task show ID`.
pub(crate) async fn task(cx: &CallContext, task_id: &str) -> ToolResult {
    json(&cx.request(args::TaskGetArgs { id: id(task_id)? }).await?, "task")
}

/// `orch goal list [--status]`, with the CLI's filter semantics: absent or
/// empty is every goal, and an unknown status matches nothing rather than
/// failing.
pub(crate) async fn goals(cx: &CallContext, status: Option<&str>) -> ToolResult {
    let status = match status {
        None | Some("") => None,
        Some(raw) => match raw.parse::<GoalStatus>() {
            Ok(status) => Some(status),
            Err(()) => return Ok(Value::Array(Vec::new())),
        },
    };
    json(&cx.request(args::GoalListArgs { status }).await?, "goals")
}

/// `orch goal show ID`: the goal's fields plus `tasks`, and `progress` only
/// when there is a report (absent, never null).
pub(crate) async fn goal(cx: &CallContext, goal_id: &str) -> ToolResult {
    let view = cx.request(args::GoalGetArgs { id: id(goal_id)? }).await?;
    let mut merged = json(&view.goal, "goal")?;
    let Some(object) = merged.as_object_mut() else {
        return json(&view.goal, "goal");
    };
    object.insert("tasks".to_owned(), json(&view.tasks, "tasks")?);
    if let Some(progress) = view.progress {
        object.insert("progress".to_owned(), Value::String(progress));
    }
    Ok(merged)
}

/// `orch agent list`: local agents plus remote snapshots.
pub(crate) async fn agents(cx: &CallContext) -> ToolResult {
    json(&cx.request(args::AgentListArgs {}).await?, "agents")
}

/// `orch agent show ID [--effective]`: the agent, or with `effective` the
/// agent under `agent` plus its resolved prompt mode under
/// `effective_config`. A missing agent is the daemon's not-found error.
pub(crate) async fn agent(cx: &CallContext, agent_id: &str, effective: bool) -> ToolResult {
    let view = cx
        .request(args::AgentGetArgs {
            id: id(agent_id)?,
            effective,
        })
        .await?;
    if effective {
        json(&view, "agent")
    } else {
        json(&view.agent, "agent")
    }
}

/// `orch context list`: live entries. Expired entries are hidden, never
/// deleted by the read.
pub(crate) async fn context(cx: &CallContext) -> ToolResult {
    json(&cx.request(args::ContextListArgs {}).await?, "context")
}

/// `orch msg inbox AGENT`: pending messages, without marking them delivered.
pub(crate) async fn inbox(cx: &CallContext, agent_id: &str) -> ToolResult {
    json(
        &cx.request(args::MessageInboxArgs {
            agent_id: id(agent_id)?,
        })
        .await?,
        "messages",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mode::Mode;
    use crate::mcp::testing::Fixture;
    use orch_domain::CreateTaskInput;

    #[tokio::test]
    async fn projections_match_the_cli_shapes() {
        let fx = Fixture::start().await;
        let cx = fx.context(None, Mode::ReadOnly);
        let task = fx
            .engine()
            .task_service
            .create(CreateTaskInput {
                title: "t".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let status = status(&cx).await.unwrap();
        let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["tasks", "agents", "state"]);
        assert_eq!(status["tasks"][0]["id"], Value::String(task.id.clone()));

        assert_eq!(tasks(&cx, None).await.unwrap().as_array().unwrap().len(), 1);
        assert_eq!(
            tasks(&cx, Some(TaskStatus::Done)).await.unwrap(),
            Value::Array(vec![])
        );
        assert_eq!(super::task(&cx, &task.id).await.unwrap()["title"], "t");
        assert!(super::task(&cx, "tsk_missing").await.is_err());

        // goal list keeps the CLI's lenient filter.
        assert_eq!(
            goals(&cx, Some("nonsense")).await.unwrap(),
            Value::Array(vec![])
        );
        assert_eq!(goals(&cx, Some("")).await.unwrap(), Value::Array(vec![]));

        let err = agent(&cx, "agt_missing", false).await.expect_err("missing");
        assert_eq!(err.to_string(), "Agent not found: agt_missing");
        assert_eq!(agents(&cx).await.unwrap(), Value::Array(vec![]));
        assert_eq!(context(&cx).await.unwrap(), Value::Array(vec![]));
        assert_eq!(inbox(&cx, "agt_x").await.unwrap(), Value::Array(vec![]));
    }

    #[tokio::test]
    async fn goal_show_omits_progress_when_absent_and_lists_linked_tasks() {
        let fx = Fixture::start().await;
        let cx = fx.context(None, Mode::ReadOnly);
        let goal_entity = fx
            .engine()
            .goal_service
            .create(orch_domain::CreateGoalInput {
                title: "g".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let value = goal(&cx, &goal_entity.id).await.unwrap();
        assert_eq!(value["title"], "g");
        assert_eq!(value["tasks"], Value::Array(vec![]));
        assert!(value.get("progress").is_none(), "{value}");
    }

    #[tokio::test]
    async fn agent_show_effective_nests_the_agent_beside_its_resolution() {
        let fx = Fixture::start().await;
        let cx = fx.context(None, Mode::ReadOnly);
        let created = fx
            .engine()
            .agent_service
            .create(orch_domain::CreateAgentInput {
                name: "worker".into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let plain = agent(&cx, &created.id, false).await.unwrap();
        assert_eq!(plain["id"], created.id.as_str());
        let effective = agent(&cx, &created.id, true).await.unwrap();
        let keys: Vec<&String> = effective.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["agent", "effective_config"]);
        assert_eq!(effective["agent"], plain);
        assert_eq!(
            effective["effective_config"],
            serde_json::json!({"prompt_mode": "normal", "prompt_mode_source": "project"})
        );
    }
}
