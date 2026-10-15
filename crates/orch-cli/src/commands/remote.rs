//! Persistent peer registry and listening preferences. Transport belongs to the owner.
use orch_core::error::{Error, Result};
use orch_domain::{RemoteConfig, RemoteStatus, RunStatus, TaskStatus};
use orch_engine::Engine;
use orch_infra::storage::{lock, remote::FileRemoteStore};

use crate::{
    context::CliContext,
    output::{Styles, print_json, print_success, print_table},
    tree::RemoteSub,
};

pub async fn run(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    command: &RemoteSub,
) -> Result<()> {
    let store = FileRemoteStore::new(engine.paths.clone());
    match command {
        RemoteSub::Add { alias, addr } => {
            let addr = normalize_addr(addr)?;
            store
                .add(RemoteConfig {
                    alias: alias.clone(),
                    addr,
                    added_at: engine.clock.now_iso(),
                })
                .await?;
            print_success(styles, &format!("Added remote {alias}"));
        }
        RemoteSub::Remove { alias } => {
            // Take the same lock as the engine before touching its state. If an
            // owner is live, registry reconciliation performs the failure instead.
            let acquired = lock::acquire_lock(&engine.paths.lock_path()).await?;
            let result = async {
                store.remove(alias).await?;
                if acquired.acquired {
                    fail_removed_runs(engine, alias).await?;
                }
                Result::<()>::Ok(())
            }
            .await;
            if acquired.acquired {
                lock::release_lock(&engine.paths.lock_path()).await;
            }
            result?;
            print_success(styles, &format!("Removed remote {alias}"));
        }
        RemoteSub::List => {
            let entries = store.list().await?;
            if ctx.json {
                return print_json(&entries);
            }
            let mut rows = Vec::new();
            for entry in entries {
                let online = store
                    .read_status(&entry.alias)
                    .await?
                    .is_some_and(|status| status.online);
                rows.push(vec![
                    entry.alias,
                    entry.addr,
                    if online { "online" } else { "offline" }.into(),
                ]);
            }
            print_table(styles, &["ALIAS", "ADDRESS", "STATUS"], &rows, 2);
        }
        RemoteSub::Status => {
            let config = engine.config_store.read().await?;
            let inbound = store.read_inbound().await?;
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
                        }),
                );
            }
            if ctx.json {
                return print_json(
                    &serde_json::json!({"listen": config.peer.listen, "inbound": inbound, "remotes": remotes}),
                );
            }
            println!(
                "  Listen: {}",
                config.peer.listen.as_deref().unwrap_or("off")
            );
            println!(
                "  Controller: {}",
                inbound
                    .as_ref()
                    .map(|i| i.controller_name.as_str())
                    .unwrap_or("none")
            );
            for status in remotes {
                println!(
                    "  {}: {}{}",
                    status.alias,
                    if status.online { "online" } else { "offline" },
                    status
                        .last_error
                        .as_ref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                );
            }
        }
        RemoteSub::Listen { state, addr } => {
            if state == "off" && addr.is_some() {
                return Err(Error::invalid_arguments(
                    "remote listen off does not accept an address",
                ));
            }
            let value = if state == "on" {
                serde_json::Value::String(normalize_listen(
                    addr.as_deref().unwrap_or("0.0.0.0:7690"),
                )?)
            } else {
                serde_json::Value::Null
            };
            engine.config_store.set("peer.listen", value).await?;
            print_success(
                styles,
                &format!("Peer listening {state}; takes effect when the engine starts"),
            );
        }
    }
    Ok(())
}

pub fn normalize_listen(addr: &str) -> Result<String> {
    addr.parse::<std::net::SocketAddr>()
        .map(|a| a.to_string())
        .map_err(|_| Error::invalid_arguments("Listen address must be an IP address and port"))
}

fn normalize_addr(addr: &str) -> Result<String> {
    if addr.is_empty() || addr.chars().any(char::is_whitespace) || addr.contains('/') {
        return Err(Error::invalid_arguments(
            "Remote address must be host[:port]",
        ));
    }
    if addr.starts_with('[') {
        if addr.parse::<std::net::SocketAddr>().is_ok() {
            return Ok(addr.into());
        }
        return Err(Error::invalid_arguments(
            "IPv6 remote addresses require [address]:port",
        ));
    }
    if let Some((host, port)) = addr.split_once(':') {
        if host.is_empty() || port.parse::<u16>().is_err() {
            return Err(Error::invalid_arguments(
                "Remote address must be host[:port]",
            ));
        }
        Ok(addr.into())
    } else {
        Ok(format!("{addr}:7690"))
    }
}

async fn fail_removed_runs(engine: &Engine, alias: &str) -> Result<()> {
    let mut state = engine.state.read().await?;
    let removed: Vec<_> = state
        .running
        .values()
        .filter(|e| e.remote.as_ref().is_some_and(|r| r.alias == alias))
        .cloned()
        .collect();
    for entry in removed {
        let reason = format!("remote {alias} removed; outcome unknown");
        if let Some(mut task) = engine.tasks.get(&entry.task_id).await?
            && task.status == TaskStatus::InProgress
        {
            task.status = TaskStatus::Failed;
            task.feedback = Some(reason.clone());
            task.updated_at = engine.clock.now_iso();
            engine.tasks.save(&task).await?;
        }
        if let Some(mut run) = engine.runs.get(&entry.run_id).await? {
            run.status = RunStatus::Failed;
            run.error = Some(reason);
            run.finished_at = Some(engine.clock.now_iso());
            engine.runs.save(&run).await?;
        }
        state.running.shift_remove(&entry.task_id);
        state.claimed.shift_remove(&entry.task_id);
        state
            .retry_queue
            .retain(|retry| retry.task_id != entry.task_id);
    }
    engine.state.write(&state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (tempfile::TempDir, Engine, CliContext) {
        let temp = tempfile::tempdir().unwrap();
        let paths = orch_infra::storage::Paths::new(temp.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        let engine = orch_engine::build_light_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(paths.root().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ctx = CliContext::new(
            temp.path().to_path_buf(),
            false,
            false,
            true,
            false,
            crate::context::EnvFlags::default(),
        );
        (temp, engine, ctx)
    }

    #[tokio::test]
    async fn registry_and_listening_preferences_persist() {
        let (_temp, engine, ctx) = fixture().await;
        run(
            &engine,
            &ctx,
            &Styles::plain(),
            &RemoteSub::Add {
                alias: "w1".into(),
                addr: "worker".into(),
            },
        )
        .await
        .unwrap();
        let store = FileRemoteStore::new(engine.paths.clone());
        assert_eq!(store.list().await.unwrap()[0].addr, "worker:7690");
        run(
            &engine,
            &ctx,
            &Styles::plain(),
            &RemoteSub::Listen {
                state: "on".into(),
                addr: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            engine
                .config_store
                .read()
                .await
                .unwrap()
                .peer
                .listen
                .as_deref(),
            Some("0.0.0.0:7690")
        );
        run(
            &engine,
            &ctx,
            &Styles::plain(),
            &RemoteSub::Listen {
                state: "off".into(),
                addr: None,
            },
        )
        .await
        .unwrap();
        assert!(
            engine
                .config_store
                .read()
                .await
                .unwrap()
                .peer
                .listen
                .is_none()
        );
        run(
            &engine,
            &ctx,
            &Styles::plain(),
            &RemoteSub::Remove { alias: "w1".into() },
        )
        .await
        .unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn removal_fails_inflight_run_without_retry_when_no_owner() {
        let (_temp, engine, ctx) = fixture().await;
        let store = FileRemoteStore::new(engine.paths.clone());
        store
            .add(RemoteConfig {
                alias: "w1".into(),
                addr: "worker:7690".into(),
                added_at: engine.clock.now_iso(),
            })
            .await
            .unwrap();
        let mut task = orch_core::test_support::make_task();
        task.status = TaskStatus::InProgress;
        engine.tasks.save(&task).await.unwrap();
        let run = engine
            .run_service
            .create(orch_engine::run_service::CreateRunInput {
                task_id: task.id.clone(),
                agent_id: "agt_worker@w1".into(),
                attempt: 1,
                prompt: "test".into(),
                workspace_path: String::new(),
            })
            .await
            .unwrap();
        let mut state = engine.state.read().await.unwrap();
        state.running.insert(
            task.id.clone(),
            orch_domain::RunningEntry {
                remote: Some(orch_domain::RemoteRunRef {
                    alias: "w1".into(),
                    exec_key: run.id.clone(),
                }),
                run_id: run.id.clone(),
                agent_id: run.agent_id.clone(),
                task_id: task.id.clone(),
                pid: 0,
                started_at: engine.clock.now_iso(),
                last_event_at: engine.clock.now_iso(),
            },
        );
        engine.state.write(&state).await.unwrap();
        run_command_remove(&engine, &ctx).await;
        assert_eq!(
            engine.tasks.get(&task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        let run = engine.runs.get(&run.id).await.unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(
            run.error.as_deref(),
            Some("remote w1 removed; outcome unknown")
        );
        let state = engine.state.read().await.unwrap();
        assert!(state.running.is_empty());
        assert!(state.retry_queue.is_empty());
    }

    async fn run_command_remove(engine: &Engine, ctx: &CliContext) {
        run(
            engine,
            ctx,
            &Styles::plain(),
            &RemoteSub::Remove { alias: "w1".into() },
        )
        .await
        .unwrap();
    }

    #[test]
    fn address_defaults_and_validation() {
        assert_eq!(normalize_addr("worker").unwrap(), "worker:7690");
        assert_eq!(normalize_addr("worker:1234").unwrap(), "worker:1234");
        assert!(normalize_addr("worker:not-a-port").is_err());
        assert!(normalize_addr("../worker").is_err());
        assert!(normalize_listen("127.0.0.1:0").is_ok());
        assert!(normalize_listen("worker:1234").is_err());
    }
}
