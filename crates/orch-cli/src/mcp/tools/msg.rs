//! Message tools resolve attribution here, before the request is built;
//! inbox reads never consume.
use crate::mcp::{
    projections,
    registry::{
        CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolError, ToolMeta,
        ToolResult, json, wire,
    },
};
use orch_core::Result;
use orch_proto::ops::args as rpc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MsgSendArgs {
    to_agent_id: String,
    body: String,
    subject: Option<String>,
    from: Option<String>,
    ttl: Option<u64>,
    reply_to: Option<String>,
}
impl ToolArgs for MsgSendArgs {
    fn validate(&self, identity: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        identity.sender("from", self.from.as_deref()).map(|_| ())
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MsgBroadcastArgs {
    body: String,
    subject: Option<String>,
    from: Option<String>,
    team: Option<String>,
    ttl: Option<u64>,
}
impl ToolArgs for MsgBroadcastArgs {
    fn validate(&self, identity: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        identity.sender("from", self.from.as_deref()).map(|_| ())
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MsgInboxArgs {
    agent_id: Option<String>,
}
impl ToolArgs for MsgInboxArgs {
    fn validate(&self, identity: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        identity
            .inbox_agent("agent_id", self.agent_id.as_deref())
            .map(|_| ())
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MsgListArgs {
    agent: Option<String>,
}
impl ToolArgs for MsgListArgs {}

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
    registry.add(meta("msg_send", "Send a direct message (orch msg send). Explicit from overrides server identity; without either, invalid params. ttl is milliseconds.", Class::Mutate, false, false, true), send)?;
    registry.add(meta("msg_broadcast", "Broadcast to eligible agents or a team (orch msg broadcast). Explicit from overrides server identity; without either, invalid params. ttl is milliseconds.", Class::Mutate, false, false, true), broadcast)?;
    registry.add(meta("msg_inbox", "Read pending messages without consuming them (orch msg inbox). Explicit agent_id overrides server identity; without either, invalid params.", Class::Read, false, true, false), inbox)?;
    registry.add(meta("msg_list", "List messages sent/received by an agent or all (orch msg list). agent is an optional filter, never defaulted to identity.", Class::Read, false, true, false), list)?;
    Ok(())
}

fn identity_error(err: rmcp::ErrorData) -> ToolError {
    ToolError::Text(err.message.to_string())
}
async fn send(cx: Arc<CallContext>, args: MsgSendArgs) -> ToolResult {
    let from_agent_id = cx
        .identity
        .sender("from", args.from.as_deref())
        .map_err(identity_error)?;
    let messages = cx
        .request(wire::<rpc::MessageSendArgs>(json!({
            "from_agent_id": from_agent_id,
            "to_agent_id": args.to_agent_id,
            "subject": args.subject.unwrap_or_default(),
            "body": args.body,
            "ttl_ms": args.ttl,
            "reply_to": args.reply_to,
        }))?)
        .await?;
    json(&messages, "messages")
}
async fn broadcast(cx: Arc<CallContext>, args: MsgBroadcastArgs) -> ToolResult {
    let from_agent_id = cx
        .identity
        .sender("from", args.from.as_deref())
        .map_err(identity_error)?;
    let messages = cx
        .request(wire::<rpc::MessageBroadcastArgs>(json!({
            "from_agent_id": from_agent_id,
            "subject": args.subject.unwrap_or_default(),
            "body": args.body,
            "ttl_ms": args.ttl,
            "team_id": args.team,
        }))?)
        .await?;
    json(&messages, "messages")
}
async fn inbox(cx: Arc<CallContext>, args: MsgInboxArgs) -> ToolResult {
    let agent = cx
        .identity
        .inbox_agent("agent_id", args.agent_id.as_deref())
        .map_err(identity_error)?;
    projections::inbox(&cx, &agent).await
}
async fn list(cx: Arc<CallContext>, args: MsgListArgs) -> ToolResult {
    let messages = cx
        .request(wire::<rpc::MessageListArgs>(
            json!({"agent_id": args.agent}),
        )?)
        .await?;
    json(&messages, "messages")
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

    async fn agent(fx: &crate::mcp::testing::Fixture, name: &str) -> String {
        fx.engine()
            .agent_service
            .create(orch_domain::CreateAgentInput {
                name: name.into(),
                adapter: "shell".into(),
                command: Some("true".into()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
    }
    #[tokio::test]
    async fn all_four_message_tools_validate_and_preserve_arrays_and_pending_state() {
        let (fx, cx) = fixture().await;
        let sender = agent(&fx, "sender").await;
        let receiver = agent(&fx, "receiver").await;
        let disabled = agent(&fx, "disabled").await;
        fx.engine().agent_service.disable(&disabled).await.unwrap();
        let direct = send(
            cx.clone(),
            args(json!({"to_agent_id":receiver,"body":"hello","from":sender,"ttl":60000})),
        )
        .await
        .unwrap();
        assert_eq!(direct.as_array().unwrap().len(), 1);
        assert_eq!(direct[0]["from_agent_id"], sender);
        assert_eq!(direct[0]["to_agent_id"], receiver);
        assert_eq!(direct[0]["status"], "pending");
        let reply = send(cx.clone(), args(json!({"to_agent_id":sender,"body":"reply","from":receiver,"reply_to":direct[0]["id"]}))).await.unwrap();
        assert_eq!(reply[0]["reply_to"], direct[0]["id"]);
        let pending = inbox(cx.clone(), args(json!({"agent_id":receiver})))
            .await
            .unwrap();
        assert_eq!(pending, direct);
        assert_eq!(
            inbox(cx.clone(), args(json!({"agent_id":receiver})))
                .await
                .unwrap(),
            pending
        );
        let broadcasted = broadcast(cx.clone(), args(json!({"from":sender,"body":"broadcast"})))
            .await
            .unwrap();
        assert_eq!(broadcasted.as_array().unwrap().len(), 1);
        assert_eq!(broadcasted[0]["to_agent_id"], receiver);
        assert_eq!(
            list(cx.clone(), args(json!({})))
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            list(cx.clone(), args(json!({"agent":disabled})))
                .await
                .unwrap(),
            json!([])
        );
        // No identity filter is applied to msg_list; no registered agents is successful empty fan-out.
        let (_fx_empty, empty) = fixture().await;
        assert_eq!(
            broadcast(empty.clone(), args(json!({"body":"empty"})))
                .await
                .unwrap(),
            json!([])
        );
        assert_eq!(inbox(empty, args(json!({}))).await.unwrap(), json!([]));
    }
    #[test]
    fn identity_validation_precedes_services_and_explicit_blanks_never_fall_back() {
        for default in [None, Some("server")] {
            let identity = Identity::of(default);
            for explicit in [None, Some(""), Some("  "), Some("override"), Some("cli")] {
                let valid = explicit.is_some_and(|v| !v.trim().is_empty())
                    || (explicit.is_none() && default.is_some());
                let mut send_value = json!({"body":"x","to_agent_id":"a"});
                let mut broadcast_value = json!({"body":"x"});
                let mut inbox_value = json!({});
                if let Some(value) = explicit {
                    send_value["from"] = json!(value);
                    broadcast_value["from"] = json!(value);
                    inbox_value["agent_id"] = json!(value);
                }
                let send_args: MsgSendArgs = serde_json::from_value(send_value).unwrap();
                let broadcast_args: MsgBroadcastArgs =
                    serde_json::from_value(broadcast_value).unwrap();
                let inbox_args: MsgInboxArgs = serde_json::from_value(inbox_value).unwrap();
                assert_eq!(send_args.validate(&identity, Mode::Default).is_ok(), valid);
                assert_eq!(
                    broadcast_args.validate(&identity, Mode::Default).is_ok(),
                    valid
                );
                assert_eq!(inbox_args.validate(&identity, Mode::Default).is_ok(), valid);
                if valid {
                    assert_eq!(
                        identity.sender("from", explicit).unwrap(),
                        explicit.or(default).unwrap()
                    );
                } else {
                    let err = send_args.validate(&identity, Mode::Default).unwrap_err();
                    assert_eq!(err.code.0, -32602);
                    assert!(err.message.contains("from"));
                }
            }
        }
        let _: MsgListArgs = args(json!({}));
        assert!(
            serde_json::from_value::<MsgSendArgs>(
                json!({"to_agent_id":"a","body":"x","from":"cli","ttl":-1})
            )
            .is_err()
        );
        assert!(serde_json::from_value::<MsgInboxArgs>(json!({"agent_id":"a","extra":1})).is_err());
    }

    #[test]
    fn catalogue_modes_annotations_and_schemas() {
        let expected = [
            ("msg_broadcast", Class::Mutate, false, false, true),
            ("msg_inbox", Class::Read, false, true, false),
            ("msg_list", Class::Read, false, true, false),
            ("msg_send", Class::Mutate, false, false, true),
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
