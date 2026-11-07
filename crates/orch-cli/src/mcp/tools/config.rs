//! Project config tools retain the CLI's validation and definition lock paths.
use crate::document_input::{self, MutationKind};
use crate::mcp::registry::{
    CallContext, Class, Execution, Identity, Mode, Registry, ToolArgs, ToolMeta,
};
use orch_core::{Error, Result};
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
    registry.add(meta("config_set", "Set project config by dotted key (orch config set). value is a string parsed as JSON first. Existing CLI validation applies; invalid config may prevent subsequent Engine construction.", Class::Mutate), set)?;
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
async fn get(cx: Arc<CallContext>, args: GetArgs) -> Result<Value> {
    let mut object = serde_json::Map::new();
    object.insert("key".into(), Value::String(args.key.clone()));
    if let Some(stored) = cx.engine.config_store.get(&args.key).await? {
        object.insert("value".into(), stored);
    }
    Ok(Value::Object(object))
}

async fn set(cx: Arc<CallContext>, args: SetArgs) -> Result<Value> {
    crate::commands::config::set_value(&cx.engine, &args.key, &args.value).await?;
    let value = cx.engine.config_store.get(&args.key).await.map_err(|err| {
        Error::other(format!(
            "Config mutation may already have committed; failed to re-read {}: {err}",
            args.key
        ))
    })?;
    Ok(json!({"key": args.key, "value": value}))
}
async fn mutate(cx: Arc<CallContext>, args: FileArgs, kind: MutationKind) -> Result<Value> {
    let path = cx.project_root.join(&args.file);
    let input = document_input::read_document(&path, args.input_format.map(Into::into)).await?;
    crate::commands::config::apply_definition(&cx.engine, &input, kind, &|err| {
        document_input::at_source(&path, err)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::mcp::registry::{Identity, Mode, ToolArgs};
    fn args<A: serde::de::DeserializeOwned + ToolArgs>(value: Value) -> A {
        let args: A = serde_json::from_value(value).unwrap();
        args.validate(&Identity::of(None), Mode::Default).unwrap();
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
            identity: Identity::of(None),
            mode: Mode::Default,
        });
        (dir, cx)
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
        let (_dir, cx) = fixture().await;
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
            serde_json::to_value(cx.engine.config_store.read().await.unwrap()).unwrap();
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
    async fn config_set_retains_raw_branch_and_poison_breaks_the_next_engine() {
        let (_dir, cx) = fixture().await;
        // project is not prompt-affecting, so the historical raw set accepts it.
        let result = set(cx.clone(), args(json!({"key":"project","value":"false"})))
            .await
            .unwrap();
        assert_eq!(result, json!({"key":"project","value":false}));
        let rebuilt = orch_engine::build_light_with(
            &cx.project_root,
            orch_engine::EngineSeams {
                global_config_path: Some(cx.project_root.join(".orchestry/global.yml")),
                ..Default::default()
            },
        )
        .await;
        assert!(rebuilt.is_err());
        // The process and the already-built context remain alive. Production
        // dispatch builds per call and encodes the next build failure as isError.
        assert_eq!(
            get(cx.clone(), args(json!({"key":"project"})))
                .await
                .unwrap(),
            result
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
