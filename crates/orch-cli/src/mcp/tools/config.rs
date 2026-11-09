//! Project config tools. Validation, the definition lock and the refresh of
//! the daemon's own configuration all happen daemon-side.
use crate::document_input;
use crate::mcp::registry::{
    CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolError, ToolMeta,
    ToolResult, json, wire,
};
use crate::mcp::tools::task::{at_source, definition_input};
use orch_core::Result;
use orch_proto::ops::args::{self as rpc, MutationKind};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    key: String,
}
impl ToolArgs for GetArgs {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    key: String,
    /// Parsed as JSON first, otherwise stored as a string.
    value: String,
}
impl ToolArgs for SetArgs {}
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
struct FileArgs {
    file: String,
    input_format: Option<Format>,
}
impl ToolArgs for FileArgs {
    fn validate(&self, _: &Identity, _: Mode) -> std::result::Result<(), rmcp::ErrorData> {
        if self.file.is_empty() || self.file == "-" {
            Err(rmcp::ErrorData::invalid_params(
                "file must be a nonempty regular file path; '-' would consume protocol stdin",
                None,
            ))
        } else {
            Ok(())
        }
    }
}
fn meta(name: &'static str, description: &'static str, class: Class) -> ToolMeta {
    let read = class == Class::Read;
    ToolMeta {
        name,
        description,
        class,
        execution: Execution::Store,
        destructive_hint: !read,
        idempotent_hint: read,
        open_world_hint: !read,
    }
}
pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(
        meta(
            "config_get",
            "Read project config by dotted key (orch config get). Missing keys omit value.",
            Class::Read,
        ),
        get,
    )?;
    registry.add(meta("config_set", "Set project config by dotted key (orch config set). value is a string parsed as JSON first. Existing CLI validation applies; the daemon applies the new config to later dispatch.", Class::Mutate), set)?;
    registry.add(
        meta(
            "config_update",
            "Replace project config from a local definition file (orch config update).",
            Class::Mutate,
        ),
        |cx, args: FileArgs| mutate(cx, args, MutationKind::Update),
    )?;
    registry.add(
        meta(
            "config_patch",
            "Merge-patch project config from a local definition file (orch config patch).",
            Class::Mutate,
        ),
        |cx, args: FileArgs| mutate(cx, args, MutationKind::Patch),
    )?;
    Ok(())
}
async fn get(cx: Arc<CallContext>, args: GetArgs) -> ToolResult {
    // Raw: a value stored as `null` is `"value": null`, a missing key has no
    // `value`, and the typed result cannot tell the two apart.
    cx.request_json(wire::<rpc::ConfigGetArgs>(json!({"key": args.key}))?)
        .await
}

async fn set(cx: Arc<CallContext>, args: SetArgs) -> ToolResult {
    let value = cx
        .request(wire::<rpc::ConfigSetArgs>(
            json!({"key": args.key, "value": args.value}),
        )?)
        .await?;
    // `value` is always present after a set, null included.
    Ok(json!({"key": value.key, "value": value.value.unwrap_or(Value::Null)}))
}
async fn mutate(cx: Arc<CallContext>, args: FileArgs, kind: MutationKind) -> ToolResult {
    let (path, definition) =
        definition_input(&cx, &args.file, args.input_format.map(Into::into), kind).await?;
    let applied = cx
        .request(rpc::ConfigDefinitionApplyArgs { definition })
        .await
        .map_err(|err| at_source(&path, err))?;
    serde_json::from_str(&applied.document)
        .map_err(|_| ToolError::Text("the daemon returned an unreadable config".to_owned()))
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

    #[test]
    fn config_args_are_strict_and_files_cannot_consume_stdin() {
        for file in ["", "-"] {
            let args: FileArgs = serde_json::from_value(json!({"file":file})).unwrap();
            assert_eq!(
                args.validate(&Identity::of(None), Mode::Default)
                    .unwrap_err()
                    .code,
                rmcp::model::ErrorCode::INVALID_PARAMS
            );
        }
        assert!(
            serde_json::from_value::<FileArgs>(json!({"file":"x","input_format":"toml"})).is_err()
        );
        assert!(serde_json::from_value::<SetArgs>(json!({"key":"x","value":1})).is_err());
        assert!(serde_json::from_value::<GetArgs>(json!({"key":"x","extra":true})).is_err());
        let _: FileArgs = args(json!({"file":"x.yaml","input_format":"yaml"}));
    }
    #[tokio::test]
    async fn all_four_config_tools_preserve_raw_values_and_missing_keys() {
        let (fx, cx) = fixture().await;
        assert_eq!(
            get(cx.clone(), args(json!({"key":"extension.missing"})))
                .await
                .unwrap(),
            json!({"key":"extension.missing"})
        );
        for (raw, expected) in [
            ("true", json!(true)),
            ("hello", json!("hello")),
            ("null", Value::Null),
            ("[1,2]", json!([1, 2])),
        ] {
            assert_eq!(
                set(
                    cx.clone(),
                    args(json!({"key":"extension.test","value":raw}))
                )
                .await
                .unwrap(),
                json!({"key":"extension.test","value":expected})
            );
            assert_eq!(
                get(cx.clone(), args(json!({"key":"extension.test"})))
                    .await
                    .unwrap(),
                json!({"key":"extension.test","value":expected})
            );
        }
        let path = cx.project_root.join("config.json");
        tokio::fs::write(&path, br#"{"project":{"name":"changed"}}"#)
            .await
            .unwrap();
        let patched = mutate(
            cx.clone(),
            args(json!({"file":"config.json"})),
            MutationKind::Patch,
        )
        .await
        .unwrap();
        assert_eq!(patched["project"]["name"], "changed");
        // A complete replacement uses the existing full validation contract.
        let replacement =
            serde_json::to_value(fx.engine().config_store.read().await.unwrap()).unwrap();
        tokio::fs::write(&path, serde_json::to_vec(&replacement).unwrap())
            .await
            .unwrap();
        assert_eq!(
            mutate(
                cx.clone(),
                args(json!({"file":"config.json","input_format":"json"})),
                MutationKind::Update
            )
            .await
            .unwrap(),
            replacement
        );
        assert!(
            set(
                cx.clone(),
                args(json!({"key":"defaults.agent.prompt_mode","value":"invalid"}))
            )
            .await
            .is_err()
        );
        assert!(
            set(cx.clone(), args(json!({"key":"defaults","value":"false"})))
                .await
                .is_err()
        );
        for bytes in [
            br#"{"defaults":{"agent":{"prompt_mode":"bogus"}}}"#.to_vec(),
            vec![b'x'; document_input::MAX_DOCUMENT_BYTES + 1],
        ] {
            tokio::fs::write(&path, bytes).await.unwrap();
            let err = mutate(
                cx.clone(),
                args(json!({"file":"config.json"})),
                MutationKind::Patch,
            )
            .await
            .unwrap_err();
            assert!(err.to_string().contains("config.json"), "{err}");
        }
    }
    #[tokio::test]
    async fn a_set_that_would_poison_the_config_is_refused_and_leaves_it_unchanged() {
        let (_fx, cx) = fixture().await;
        let before = get(cx.clone(), args(json!({"key":"project"})))
            .await
            .unwrap();
        // project is not prompt-affecting: the raw set branch applies, and the
        // daemon refuses a value it could not load afterwards.
        let err = set(cx.clone(), args(json!({"key":"project","value":"false"})))
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().starts_with("config: "), "{err}");
        assert_eq!(
            get(cx.clone(), args(json!({"key":"project"})))
                .await
                .unwrap(),
            before
        );
    }
    #[test]
    fn catalogue_enforces_modes_annotations_and_strict_schemas() {
        let expected: [(&str, Class, bool, &[&str]); 4] = [
            ("config_get", Class::Read, false, &["key"][..]),
            ("config_patch", Class::Mutate, true, &["file"][..]),
            ("config_set", Class::Mutate, true, &["key", "value"][..]),
            ("config_update", Class::Mutate, true, &["file"][..]),
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
