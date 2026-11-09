//! Team tools: one daemon request each; membership writes return the team
//! as the daemon committed it.
use crate::mcp::projections::id;
use crate::mcp::registry::{
    CallContext, Class, EmptyArgs, Execution, Registry, ToolArgs, ToolMeta, ToolResult, json, wire,
};
use orch_core::Result;
use orch_proto::ops::args as rpc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    name: String,
    lead: String,
    members: Option<String>,
    description: Option<String>,
    #[serde(default)]
    no_auto_claim: bool,
}
impl ToolArgs for CreateArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}
impl ToolArgs for IdArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MemberArgs {
    team_id: String,
    agent_id: String,
}
impl ToolArgs for MemberArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskArgs {
    team_id: String,
    task_id: String,
}
impl ToolArgs for TaskArgs {}

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
    registry.add(meta("team_create", "Create a team (orch team create). members is comma-separated; no_auto_claim disables automatic claiming.", Class::Mutate, false, false, true), create)?;
    registry.add(
        meta(
            "team_list",
            "List teams (orch team list).",
            Class::Read,
            false,
            true,
            false,
        ),
        list,
    )?;
    registry.add(
        meta(
            "team_show",
            "Read a team (orch team show).",
            Class::Read,
            false,
            true,
            false,
        ),
        show,
    )?;
    registry.add(
        meta(
            "team_join",
            "Add an agent to a team (orch team join).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        join,
    )?;
    registry.add(
        meta(
            "team_leave",
            "Remove an agent from a team (orch team leave).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        leave,
    )?;
    registry.add(
        meta(
            "team_add_task",
            "Add a task to a team's pool (orch team add-task).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        add_task,
    )?;
    registry.add(
        meta(
            "team_set_lead",
            "Transfer team leadership (orch team set-lead).",
            Class::Mutate,
            true,
            false,
            true,
        ),
        set_lead,
    )?;
    registry.add(
        meta(
            "team_disband",
            "Disband a team (orch team disband).",
            Class::Destructive,
            true,
            false,
            true,
        ),
        disband,
    )?;
    Ok(())
}
async fn create(cx: Arc<CallContext>, args: CreateArgs) -> ToolResult {
    let members = args.members.map(|raw| {
        raw.split(',')
            .map(|part| part.trim().to_owned())
            .collect::<Vec<_>>()
    });
    let team = cx
        .request(wire::<rpc::TeamCreateArgs>(json!({
            "name": args.name,
            "lead_agent_id": args.lead,
            "description": args.description,
            "member_agent_ids": members,
            "auto_claim": !args.no_auto_claim,
        }))?)
        .await?;
    json(&team, "team")
}
async fn list(cx: Arc<CallContext>, _: EmptyArgs) -> ToolResult {
    json(&cx.request(rpc::TeamListArgs {}).await?, "teams")
}
async fn show(cx: Arc<CallContext>, args: IdArgs) -> ToolResult {
    json(
        &cx.request(rpc::TeamGetArgs { id: id(&args.id)? }).await?,
        "team",
    )
}
async fn join(cx: Arc<CallContext>, args: MemberArgs) -> ToolResult {
    let team = cx
        .request(rpc::TeamJoinArgs {
            team_id: id(&args.team_id)?,
            agent_id: id(&args.agent_id)?,
        })
        .await?;
    json(&team, "team")
}
async fn leave(cx: Arc<CallContext>, args: MemberArgs) -> ToolResult {
    let team = cx
        .request(rpc::TeamLeaveArgs {
            team_id: id(&args.team_id)?,
            agent_id: id(&args.agent_id)?,
        })
        .await?;
    json(&team, "team")
}
async fn add_task(cx: Arc<CallContext>, args: TaskArgs) -> ToolResult {
    let team = cx
        .request(rpc::TeamAddTaskArgs {
            team_id: id(&args.team_id)?,
            task_id: id(&args.task_id)?,
        })
        .await?;
    json(&team, "team")
}
async fn set_lead(cx: Arc<CallContext>, args: MemberArgs) -> ToolResult {
    let team = cx
        .request(rpc::TeamSetLeadArgs {
            team_id: id(&args.team_id)?,
            agent_id: id(&args.agent_id)?,
        })
        .await?;
    json(&team, "team")
}
async fn disband(cx: Arc<CallContext>, args: IdArgs) -> ToolResult {
    cx.request(rpc::TeamDisbandArgs { id: id(&args.id)? })
        .await?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    use crate::mcp::registry::{Identity, Mode, ToolArgs};
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(None), Mode::Default).unwrap();
        args
    }
    async fn fixture() -> (crate::mcp::testing::Fixture, Arc<CallContext>) {
        let fx = crate::mcp::testing::Fixture::start().await;
        let cx = fx.context(Some("cli"), Mode::AllowDestructive);
        (fx, cx)
    }

    #[tokio::test]
    async fn all_eight_team_tools_project_committed_entities() {
        let (fx, cx) = fixture().await;
        let lead = fx
            .engine()
            .agent_service
            .create(orch_domain::CreateAgentInput {
                name: "lead".into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let member = fx
            .engine()
            .agent_service
            .create(orch_domain::CreateAgentInput {
                name: "member".into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let _: EmptyArgs = args(json!({}));
        assert_eq!(list(cx.clone(), args(json!({}))).await.unwrap(), json!([]));
        let team = create(
            cx.clone(),
            args(json!({"name":"team","lead":lead.id,"no_auto_claim":true})),
        )
        .await
        .unwrap();
        let id = team["id"].as_str().unwrap();
        assert_eq!(team["config"]["auto_claim"], false);
        assert_eq!(
            show(cx.clone(), args(json!({"id":id}))).await.unwrap(),
            team
        );
        let joined = join(cx.clone(), args(json!({"team_id":id,"agent_id":member.id})))
            .await
            .unwrap();
        assert_eq!(joined["members"].as_array().unwrap().len(), 2);
        assert_eq!(
            joined,
            show(cx.clone(), args(json!({"id":id}))).await.unwrap()
        );
        let task = fx
            .engine()
            .task_service
            .create(orch_domain::CreateTaskInput {
                title: "pooled".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let pooled = add_task(cx.clone(), args(json!({"team_id":id,"task_id":task.id})))
            .await
            .unwrap();
        assert_eq!(pooled["task_pool"], json!([task.id]));
        let transferred = set_lead(cx.clone(), args(json!({"team_id":id,"agent_id":member.id})))
            .await
            .unwrap();
        assert_eq!(transferred["lead_agent_id"], member.id);
        let left = leave(cx.clone(), args(json!({"team_id":id,"agent_id":lead.id})))
            .await
            .unwrap();
        assert_eq!(left["members"].as_array().unwrap().len(), 1);
        let listed = list(cx.clone(), args(json!({}))).await.unwrap();
        assert_eq!(listed, json!([left]));
        assert_eq!(
            disband(cx.clone(), args(json!({"id":id}))).await.unwrap(),
            Value::Null
        );
        assert_eq!(
            show(cx, args(json!({"id":id}))).await.unwrap()["status"],
            "disbanded"
        );
    }
    #[test]
    fn argument_structs_are_strict_and_domain_validation_stays_in_services() {
        assert!(
            serde_json::from_value::<CreateArgs>(
                json!({"name":"x","lead":"x","no_auto_claim":"yes"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<MemberArgs>(json!({"team_id":"t","agent_id":"a","extra":1}))
                .is_err()
        );
        assert!(serde_json::from_value::<TaskArgs>(json!({"team_id":"t"})).is_err());
        let _: CreateArgs = args(json!({"name":"","lead":""}));
        let _: IdArgs = args(json!({"id":""}));
    }
    #[test]
    fn catalogue_enforces_modes_annotations_and_strict_schemas() {
        let expected: [(&str, Class, bool, &[&str]); 8] = [
            (
                "team_add_task",
                Class::Mutate,
                true,
                &["task_id", "team_id"][..],
            ),
            ("team_create", Class::Mutate, false, &["lead", "name"][..]),
            ("team_disband", Class::Destructive, true, &["id"][..]),
            (
                "team_join",
                Class::Mutate,
                true,
                &["agent_id", "team_id"][..],
            ),
            (
                "team_leave",
                Class::Mutate,
                true,
                &["agent_id", "team_id"][..],
            ),
            ("team_list", Class::Read, false, &[][..]),
            (
                "team_set_lead",
                Class::Mutate,
                true,
                &["agent_id", "team_id"][..],
            ),
            ("team_show", Class::Read, false, &["id"][..]),
        ];
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let mut registry = Registry::new(mode);
            register(&mut registry).unwrap();
            let tools = registry.into_router().list_all();
            let wanted: Vec<_> = expected
                .iter()
                .filter(|(_, class, _, _)| mode.allows(*class))
                .collect();
            assert_eq!(tools.len(), wanted.len());
            for (tool, (name, class, destructive, required)) in tools.iter().zip(wanted) {
                assert_eq!(tool.name.as_ref(), *name);
                let value = serde_json::to_value(tool).unwrap();
                assert_eq!(
                    value["annotations"],
                    serde_json::json!({
                        "readOnlyHint": *class == Class::Read,
                        "destructiveHint": destructive,
                        "idempotentHint": *class == Class::Read,
                        "openWorldHint": *class != Class::Read,
                    })
                );
                assert_eq!(value["inputSchema"]["additionalProperties"], false);
                let mut actual: Vec<_> = value["inputSchema"]["required"]
                    .as_array()
                    .map(|items| items.iter().map(|v| v.as_str().unwrap()).collect())
                    .unwrap_or_default();
                actual.sort();
                assert_eq!(&actual, required, "{name}");
            }
        }
    }
}
