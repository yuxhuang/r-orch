//! Stored remote snapshots only: no connection, listener or owner runtime.
use crate::mcp::registry::{CallContext, Class, EmptyArgs, Execution, Registry, ToolMeta};
use orch_core::{Error, Result};
use orch_domain::RemoteStatus;
use orch_infra::storage::remote::FileRemoteStore;
use serde_json::{Value, json};
use std::sync::Arc;

fn meta(name: &'static str, description: &'static str) -> ToolMeta {
    ToolMeta {
        name,
        description,
        class: Class::Read,
        execution: Execution::Store,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    }
}
pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add(
        meta(
            "remote_list",
            "List persisted remote definitions (orch remote list).",
        ),
        list,
    )?;
    registry.add(meta("remote_status", "Read stored remote and live-owner listener snapshots (orch remote status); does not connect or listen."), status)?;
    Ok(())
}
async fn list(cx: Arc<CallContext>, _: EmptyArgs) -> Result<Value> {
    serde_json::to_value(FileRemoteStore::new(cx.engine.paths.clone()).list().await?)
        .map_err(|_| Error::other("serialize remotes"))
}
async fn status(cx: Arc<CallContext>, _: EmptyArgs) -> Result<Value> {
    let store = FileRemoteStore::new(cx.engine.paths.clone());
    let config = cx.engine.config_store.read().await?;
    let inbound = store.read_inbound().await?;
    let listener = crate::commands::remote::listener_status(&cx.engine.paths, false).await?;
    let mut remotes = Vec::new();
    for entry in store.list().await? {
        remotes.push(
            store
                .read_status(&entry.alias)
                .await?
                .unwrap_or(RemoteStatus {
                    alias: entry.alias,
                    addr: entry.addr,
                    online: false,
                    last_seen: None,
                    last_error: None,
                    peer_name: None,
                    instance_id: None,
                    os: None,
                    agents: Vec::new(),
                    peer_version: None,
                    link_state: orch_domain::LinkState::Connecting,
                    next_retry_at: None,
                    attempts: 0,
                }),
        );
    }
    Ok(
        json!({"listen": listener.addr, "listen_configured": config.peer.listen,
        "controller": listener.controller, "last_session_error": listener.last_session_error,
        "inbound": inbound, "remotes": remotes}),
    )
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

    #[tokio::test]
    async fn both_remote_tools_are_snapshot_projections_with_offline_fallbacks() {
        let (_dir, cx) = fixture().await;
        assert!(serde_json::from_value::<EmptyArgs>(json!({"alias":"x"})).is_err());
        assert_eq!(list(cx.clone(), args(json!({}))).await.unwrap(), json!([]));
        let store = FileRemoteStore::new(cx.engine.paths.clone());
        store
            .add(orch_domain::RemoteConfig {
                alias: "peer".into(),
                addr: "127.0.0.1:9".into(),
                added_at: cx.engine.clock.now_iso(),
            })
            .await
            .unwrap();
        let listed = list(cx.clone(), args(json!({}))).await.unwrap();
        assert_eq!(listed[0]["alias"], "peer");
        assert!(listed[0].get("online").is_none());
        cx.engine
            .config_store
            .set("peer.listen", json!("127.0.0.1:0"))
            .await
            .unwrap();
        let snapshot = status(cx.clone(), args(json!({}))).await.unwrap();
        assert_eq!(snapshot["listen"], Value::Null);
        assert_eq!(snapshot["listen_configured"], "127.0.0.1:0");
        assert_eq!(snapshot["controller"], Value::Null);
        assert_eq!(snapshot["last_session_error"], Value::Null);
        assert_eq!(snapshot["remotes"][0]["online"], false);
        assert_eq!(snapshot["remotes"][0]["link_state"], "connecting");
        assert_eq!(snapshot.as_object().unwrap().len(), 6);
        assert_eq!(list(cx, args(json!({}))).await.unwrap(), listed);
    }
    #[test]
    fn catalogue_enforces_modes_annotations_and_strict_schemas() {
        let expected: [(&str, Class, bool, &[&str]); 2] = [
            ("remote_list", Class::Read, false, &[][..]),
            ("remote_status", Class::Read, false, &[][..]),
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
