//! Persistent peer registry and listening preferences. Transport belongs to the owner.
use orch_core::error::{Error, Result};
use orch_domain::{RemoteConfig, RemoteStatus, RunStatus, TaskStatus};
use orch_engine::Engine;
use orch_infra::storage::{lock, remote::FileRemoteStore};

use crate::{
    context::CliContext,
    output::{Color, Icon, Styles, print_json, print_success, print_table},
    tree::RemoteSub,
};

/// Where `remote listen on` and a bare `--listen` bind: this host only. The
/// peer protocol has no authentication, so any address reachable from other
/// hosts must be typed explicitly (security review F1).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7690";

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
            let configured = config.peer.listen;
            let listener = listener_status(&engine.paths, false).await?;
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
                        }),
                );
            }
            if ctx.json {
                return print_json(&serde_json::json!({
                    "listen": listener.addr,
                    "listen_configured": configured,
                    "controller": listener.controller,
                    "last_session_error": listener.last_session_error,
                    "inbound": inbound,
                    "remotes": remotes,
                }));
            }
            println!(
                "  Listen: {}{}",
                describe_listen(listener.addr.as_deref(), configured.as_deref()),
                listener
                    .last_session_error
                    .as_ref()
                    .map(|error| format!(" (last session ended: {error})"))
                    .unwrap_or_default()
            );
            println!(
                "  Controller: {}{}",
                listener.controller.as_deref().unwrap_or("none"),
                inbound
                    .as_ref()
                    .filter(|_| listener.controller.is_some())
                    .and_then(|inbound| inbound.controller_version.as_deref())
                    .map(|version| format!(" (v{version})"))
                    .unwrap_or_default()
            );
            for status in remotes {
                println!(
                    "  {}: {}{}{}",
                    status.alias,
                    if status.online { "online" } else { "offline" },
                    status
                        .peer_version
                        .as_ref()
                        .map(|version| format!(" v{version}"))
                        .unwrap_or_default(),
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
            let listen = if state == "on" {
                Some(normalize_listen(addr.as_deref().unwrap_or(DEFAULT_LISTEN))?)
            } else {
                None
            };
            let value = listen
                .clone()
                .map_or(serde_json::Value::Null, serde_json::Value::String);
            engine.config_store.set("peer.listen", value).await?;
            print_success(
                styles,
                &match &listen {
                    Some(listen) => {
                        format!("Peer listening on {listen}; takes effect when the engine starts")
                    }
                    None => "Peer listening off; takes effect when the engine starts".into(),
                },
            );
            if let Some(listen) = &listen {
                warn_if_exposed(styles, listen);
            }
        }
    }
    Ok(())
}

/// What a live engine owner's peer listener has published.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ListenerStatus {
    pub addr: Option<String>,
    pub controller: Option<String>,
    /// Why the last inbound controller session ended. It outlives the session
    /// on purpose: a controller that keeps reconnecting is only explicable
    /// from the error the worker recorded as each session died.
    pub last_session_error: Option<String>,
}

/// The address of a socket a live engine owner has bound, and its connected
/// controller. A configured preference (which may be port 0) is never an
/// active listener: an empty [`ListenerStatus`] unless the lock holder is
/// alive, fresh, and published the record itself. `own_runtime` says whether this process
/// currently runs a peer runtime; without one, a record stamped with our own
/// PID is left over from a stopped listener (TUI `/pause`).
pub(crate) async fn listener_status(
    paths: &orch_infra::storage::Paths,
    own_runtime: bool,
) -> Result<ListenerStatus> {
    #[derive(serde::Deserialize)]
    struct Listener {
        listen_addr: String,
        controller_name: Option<String>,
        pid: Option<u32>,
        #[serde(default)]
        last_session_error: Option<String>,
    }

    let recent = tokio::fs::metadata(paths.lock_path())
        .await
        .ok()
        .and_then(|meta| meta.modified().ok())
        .is_some_and(|modified| {
            std::time::SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age <= lock::LOCK_STALE)
        });
    let holder = lock::check_lock(&paths.lock_path()).await;
    if recent && holder.acquired {
        let listener: Option<Listener> =
            orch_infra::storage::fs_utils::read_json(&paths.remotes_dir().join("_listener.json"))
                .await?;
        if let Some(listener) = listener
            && listener.pid == holder.pid
            && (own_runtime || listener.pid != Some(std::process::id()))
        {
            return Ok(ListenerStatus {
                addr: Some(listener.listen_addr),
                controller: listener.controller_name,
                last_session_error: listener.last_session_error,
            });
        }
    }
    Ok(ListenerStatus::default())
}

/// One wording for the CLI and the TUI header: only a bound socket is shown
/// as an address; a configured preference is labelled as such.
pub(crate) fn describe_listen(active: Option<&str>, configured: Option<&str>) -> String {
    match (active, configured) {
        (Some(addr), _) if exposure_warning(addr).is_some() => {
            format!("{addr} (EXPOSED, no auth)")
        }
        (Some(addr), _) => addr.to_owned(),
        (None, Some(configured)) => format!("off (configured {configured}, not active)"),
        (None, None) => "off".to_owned(),
    }
}

/// The warning for a listen address other hosts may reach, or `None` for a
/// loopback IP. Anything that does not parse as one (a hand-edited
/// `peer.listen` such as `localhost:7690`) warns rather than guess.
pub fn exposure_warning(addr: &str) -> Option<String> {
    let parsed = addr.parse::<std::net::SocketAddr>().ok();
    if parsed.is_some_and(|parsed| parsed.ip().is_loopback()) {
        return None;
    }
    let reach = if parsed.is_some_and(|parsed| parsed.ip().is_unspecified()) {
        "on ALL network interfaces"
    } else {
        "beyond this host"
    };
    Some(format!(
        "Peer listener {addr} accepts connections {reach} with no authentication \
         or encryption: any host that can reach it can run tasks on this instance's \
         agents, including shell commands. Use it only on a trusted network; bind \
         127.0.0.1 to keep it local."
    ))
}

/// Prints [`exposure_warning`] on stderr, so it survives a redirected or
/// machine-read stdout (`serve --log-format json`).
pub(crate) fn warn_if_exposed(styles: &Styles, addr: &str) {
    if let Some(warning) = exposure_warning(addr) {
        let glyph = styles.paint(Color::Yellow, styles.icon(Icon::Warning));
        eprintln!("  {glyph} {warning}");
    }
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
    async fn listener_status_reports_only_a_live_owners_bound_socket() {
        let (_temp, engine, _ctx) = fixture().await;
        let record = engine.paths.remotes_dir().join("_listener.json");
        let publish = |pid: u32| {
            let record = record.clone();
            async move {
                orch_infra::storage::fs_utils::write_json(
                    &record,
                    &serde_json::json!({
                        "listen_addr": "127.0.0.1:41234",
                        "controller_name": "controller-a",
                        "pid": pid,
                    }),
                )
                .await
                .unwrap();
            }
        };
        let live = || ListenerStatus {
            addr: Some("127.0.0.1:41234".into()),
            controller: Some("controller-a".into()),
            last_session_error: None,
        };
        let inactive = ListenerStatus::default;
        publish(std::process::id()).await;
        // No owner: a leftover record is not a listener.
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );

        let acquired = lock::acquire_lock(&engine.paths.lock_path()).await.unwrap();
        assert!(acquired.acquired);
        assert_eq!(listener_status(&engine.paths, true).await.unwrap(), live());
        // This process holds the lock but its peer runtime stopped (TUI
        // `/pause`): the record it left behind is stale.
        assert_eq!(
            listener_status(&engine.paths, false).await.unwrap(),
            inactive()
        );
        // Published by a different process than the lock holder.
        publish(std::process::id() + 1).await;
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );
        // Records from builds without an owner stamp are never trusted.
        orch_infra::storage::fs_utils::write_json(
            &record,
            &serde_json::json!({"listen_addr": "127.0.0.1:41234", "controller_name": null}),
        )
        .await
        .unwrap();
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );
        tokio::fs::remove_file(&record).await.unwrap();
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );

        publish(std::process::id()).await;
        std::fs::OpenOptions::new()
            .write(true)
            .open(engine.paths.lock_path())
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::now() - lock::LOCK_STALE - std::time::Duration::from_secs(1),
            ))
            .unwrap();
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );
        lock::release_lock(&engine.paths.lock_path()).await;
        assert_eq!(
            listener_status(&engine.paths, true).await.unwrap(),
            inactive()
        );
    }

    #[test]
    fn describe_listen_never_presents_a_preference_as_a_listener() {
        assert_eq!(
            describe_listen(Some("127.0.0.1:50162"), Some("127.0.0.1:0")),
            "127.0.0.1:50162"
        );
        assert_eq!(
            describe_listen(None, Some("127.0.0.1:0")),
            "off (configured 127.0.0.1:0, not active)"
        );
        assert_eq!(describe_listen(None, None), "off");
    }

    #[test]
    fn describe_listen_marks_an_active_network_listener_as_exposed() {
        for addr in ["0.0.0.0:7690", "[::]:7690", "192.168.1.5:7690"] {
            assert_eq!(
                describe_listen(Some(addr), None),
                format!("{addr} (EXPOSED, no auth)")
            );
        }
        assert_eq!(describe_listen(Some("[::1]:7690"), None), "[::1]:7690");
    }

    #[test]
    fn loopback_listen_addresses_need_no_exposure_warning() {
        for addr in [
            DEFAULT_LISTEN,
            "127.0.0.1:0",
            "127.8.9.10:7690",
            "[::1]:7690",
        ] {
            assert_eq!(exposure_warning(addr), None, "{addr}");
        }
    }

    #[test]
    fn wildcard_listen_addresses_warn_about_all_interfaces() {
        for addr in ["0.0.0.0:7690", "[::]:7690"] {
            let warning = exposure_warning(addr).expect(addr);
            for needle in [
                addr,
                "ALL network interfaces",
                "no authentication",
                "127.0.0.1",
            ] {
                assert!(warning.contains(needle), "{needle} missing: {warning}");
            }
        }
    }

    #[test]
    fn specific_network_and_unparsed_listen_addresses_warn() {
        let lan = exposure_warning("192.168.1.5:7690").expect("LAN address");
        assert!(lan.contains("192.168.1.5:7690") && lan.contains("no authentication"));
        assert!(!lan.contains("ALL network interfaces"), "{lan}");
        // `peer.listen` set by hand is bound as given; never stay silent about
        // an address this check cannot prove is loopback.
        assert!(exposure_warning("localhost:7690").is_some());
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
            Some("127.0.0.1:7690")
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
                remote: Some(orch_domain::RemoteRunRef::new("w1", &run.id)),
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
