//! Shared context tools keep missing-key null results. Reads hide expired
//! entries and never delete them; expiry is the daemon's own step.
use crate::mcp::registry::EmptyArgs;
use crate::mcp::{
    projections,
    registry::{
        CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolMeta, ToolResult,
        json, wire,
    },
};
use orch_core::Result;
use orch_proto::ops::args as rpc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// The longest TTL a context entry may have: 30 days, in milliseconds.
const MAX_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextSetArgs {
    key: String,
    value: String,
    ttl: Option<u64>,
}
impl ToolArgs for ContextSetArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        if self.ttl.is_some_and(|ttl| ttl == 0 || ttl > MAX_TTL_MS) {
            return Err(rmcp::ErrorData::invalid_params(
                format!("TTL must be a positive number up to {MAX_TTL_MS}ms (30 days)"),
                None,
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextGetArgs {
    key: String,
}
impl ToolArgs for ContextGetArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextDeleteArgs {
    key: String,
}
impl ToolArgs for ContextDeleteArgs {}

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
            "context_set",
            "Set shared context with optional expiry (orch context set). ttl is milliseconds.",
            Class::Mutate,
            true,
            false,
            false,
        ),
        set,
    )?;
    registry.add(
        meta(
            "context_get",
            "Read a context entry or null (orch context get). Expired entries read as null.",
            Class::Read,
            false,
            true,
            false,
        ),
        get,
    )?;
    registry.add(
        meta(
            "context_list",
            "List live context entries (orch context list). Expired entries are omitted.",
            Class::Read,
            false,
            true,
            false,
        ),
        list,
    )?;
    registry.add(
        meta(
            "context_delete",
            "Delete a context entry (orch context delete).",
            Class::Destructive,
            true,
            true,
            false,
        ),
        delete,
    )?;
    Ok(())
}

async fn set(cx: Arc<CallContext>, args: ContextSetArgs) -> ToolResult {
    let entry = cx
        .request(wire::<rpc::ContextSetArgs>(json!({
            "key": args.key,
            "value": args.value,
            "ttl_ms": args.ttl,
        }))?)
        .await?;
    json(&entry, "context entry")
}
async fn get(cx: Arc<CallContext>, args: ContextGetArgs) -> ToolResult {
    let entry = cx
        .request(wire::<rpc::ContextGetArgs>(json!({"key": args.key}))?)
        .await?;
    json(&entry, "context entry")
}
async fn list(cx: Arc<CallContext>, _: EmptyArgs) -> ToolResult {
    projections::context(&cx).await
}
async fn delete(cx: Arc<CallContext>, args: ContextDeleteArgs) -> ToolResult {
    cx.request(wire::<rpc::ContextDeleteArgs>(json!({"key": args.key}))?)
        .await?;
    Ok(Value::Null)
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

    #[tokio::test]
    async fn all_four_context_tools_validate_and_project() {
        let (fx, cx) = fixture().await;
        assert_eq!(
            get(cx.clone(), args(json!({"key":"missing"})))
                .await
                .unwrap(),
            Value::Null
        );
        let entry = set(
            cx.clone(),
            args(json!({"key":"k","value":"hello","ttl":60000})),
        )
        .await
        .unwrap();
        assert_eq!(entry["key"], "k");
        assert_eq!(entry["value"], "hello");
        assert_eq!(entry["ttl_ms"], 60000);
        assert!(entry["expires_at"].is_string());
        assert!(entry["updated_at"].is_string());
        assert_eq!(
            get(cx.clone(), args(json!({"key":"k"}))).await.unwrap(),
            entry
        );
        assert_eq!(
            list(cx.clone(), args(json!({}))).await.unwrap(),
            json!([entry])
        );
        assert_eq!(
            delete(cx.clone(), args(json!({"key":"k"}))).await.unwrap(),
            Value::Null
        );
        assert_eq!(
            get(cx, args(json!({"key":"k"}))).await.unwrap(),
            Value::Null
        );
    }
    #[test]
    fn ttl_is_checked_before_the_daemon_and_outer_arguments_are_strict() {
        for ttl in [0, MAX_TTL_MS + 1, u64::MAX] {
            let args: ContextSetArgs =
                serde_json::from_value(json!({"key":"k","value":"v","ttl":ttl})).unwrap();
            let err = args
                .validate(&Identity::of(None), Mode::Default)
                .unwrap_err();
            assert_eq!(err.code.0, -32602);
            assert_eq!(
                err.message,
                "TTL must be a positive number up to 2592000000ms (30 days)"
            );
        }
        let _: ContextSetArgs = args(json!({"key":"k","value":"v","ttl":MAX_TTL_MS}));
        let _: ContextSetArgs = args(json!({"key":"k","value":"v"}));
        assert!(
            serde_json::from_value::<ContextSetArgs>(json!({"key":"k","value":"v","ttl":-1}))
                .is_err()
        );
        assert!(serde_json::from_value::<ContextGetArgs>(json!({"key":"k","extra":1})).is_err());
        assert!(serde_json::from_value::<ContextDeleteArgs>(json!({})).is_err());
        assert!(serde_json::from_value::<EmptyArgs>(json!({"key":"k"})).is_err());
    }

    #[test]
    fn catalogue_modes_annotations_and_schemas() {
        let expected = [
            ("context_delete", Class::Destructive, true, true, false),
            ("context_get", Class::Read, false, true, false),
            ("context_list", Class::Read, false, true, false),
            ("context_set", Class::Mutate, true, false, false),
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
