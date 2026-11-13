#![cfg(feature = "testing")]
//! The engine guarantees, observed through the socket (T1; design D12
//! `daemon_engine::*`; spec R9–R12, R30): the same scenarios the engine-level
//! tests pin below the socket, now across the real session and coordinator.

mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use support::{Raw, Reaper, TestClock, err_code, ok, running_pid, shell_agent, task, task_status};

// -- cancel ---------------------------------------------------------------

/// `task.cancel` from any client kind that may cancel stops the live child
/// before the success reply, and the daemon forgets it.
async fn cancel_stops_the_child(kind: &str, mode: &str) {
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, kind, mode).await;
    // Agent and task creation are Mutate: use an owner session for setup.
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut owner, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut owner, "long job", json!({"assignee": agent})).await;

    let tick = owner.request(1, "dispatch.tick", json!({})).await;
    assert_eq!(ok(&tick)["launched"], json!([id]), "{tick}");
    let pid = running_pid(&daemon, &id)
        .await
        .expect("the task is running");
    reaper.watch(pid);
    assert!(reaper.alive(pid));

    let reply = raw.request(1, "task.cancel", json!({"id": id})).await;
    assert_eq!(ok(&reply)["status"], "cancelled", "{reply}");
    // The success reply is the proof the local child has stopped.
    assert!(
        !reaper.alive(pid),
        "task.cancel returned ok while the child ran"
    );
    assert!(running_pid(&daemon, &id).await.is_none());
    let status = owner.request(2, "daemon.status", json!({})).await;
    assert_eq!(ok(&status)["running_tasks"], json!([]));
    // A late tick cannot resurrect it.
    let tick = owner.request(3, "dispatch.tick", json!({})).await;
    assert_eq!(ok(&tick)["launched"], json!([]), "{tick}");
    assert_eq!(task_status(&mut owner, &id).await, "cancelled");
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_live_process_from_a_cli_session() {
    cancel_stops_the_child("cli", "normal").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_live_process_from_an_mcp_allow_destructive_session() {
    cancel_stops_the_child("mcp", "allow_destructive").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_live_process_from_a_tui_session() {
    cancel_stops_the_child("tui", "normal").await;
}

// -- pause ----------------------------------------------------------------

/// After the pause reply, no tick launches the paused goal's work; an
/// unrelated goal's task still launches (pause is per goal).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_prevents_post_decision_spawn() {
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    // A busy agent runs one task at a time, so the paused goal's work and the
    // unrelated goal's work use different agents.
    let other_agent = shell_agent(&mut raw, &daemon, "sleeper two", "exec /bin/sleep 120").await;
    let goal = async |title: &str, raw: &mut Raw| -> String {
        let reply = raw
            .request(2_000_000, "goal.create", json!({"title": title}))
            .await;
        ok(&reply)["id"].as_str().unwrap().to_owned()
    };
    let paused_goal = goal("to be paused", &mut raw).await;
    let other_goal = goal("unrelated", &mut raw).await;
    let mut paused_tasks = Vec::new();
    for i in 0..3 {
        paused_tasks.push(
            task(
                &mut raw,
                &format!("p{i}"),
                json!({"assignee": agent, "goal_id": paused_goal}),
            )
            .await,
        );
    }
    let free = task(
        &mut raw,
        "free",
        json!({"assignee": other_agent, "goal_id": other_goal}),
    )
    .await;

    let reply = raw
        .request(
            1,
            "goal.status",
            json!({"id": paused_goal, "status": "paused"}),
        )
        .await;
    assert_eq!(ok(&reply)["status"], "paused", "{reply}");

    // Several ticks after the decision: nothing of the paused goal starts.
    let mut launched = Vec::new();
    for n in 0..5 {
        let tick = raw.request(10 + n, "dispatch.tick", json!({})).await;
        launched.extend(
            ok(&tick)["launched"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned()),
        );
    }
    for id in &paused_tasks {
        assert!(
            !launched.contains(id),
            "paused task {id} launched after the pause reply"
        );
        assert!(running_pid(&daemon, id).await.is_none(), "{id}");
        // Not launched, and not lost: the task waits for the goal to resume.
        assert_eq!(task_status(&mut raw, id).await, "todo", "{id}");
    }
    // Unrelated work is untouched by the pause.
    assert!(
        launched.contains(&free),
        "unrelated goal's task must still run: {launched:?}"
    );
    if let Some(pid) = running_pid(&daemon, &free).await {
        reaper.watch(pid);
    }

    // Resuming makes the same tasks dispatchable again.
    let reply = raw
        .request(
            30,
            "goal.status",
            json!({"id": paused_goal, "status": "active"}),
        )
        .await;
    assert_eq!(ok(&reply)["status"], "active", "{reply}");
    let tick = raw.request(31, "dispatch.tick", json!({})).await;
    let resumed = ok(&tick)["launched"].as_array().unwrap().len();
    assert!(
        resumed >= 1,
        "a resumed goal's tasks must be dispatchable: {tick}"
    );
    for id in &paused_tasks {
        if let Some(pid) = running_pid(&daemon, id).await {
            reaper.watch(pid);
        }
    }
    daemon.shutdown().await.unwrap();
}

/// A pause racing continuous ticks from other sessions: every tick whose
/// request was *sent* after the pause reply was *received* launches none of
/// the paused goal's tasks. (Ticks already in flight may legitimately have
/// launched earlier work; the invariant is the one-way barrier.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ticks_started_after_the_pause_reply_never_launch_the_paused_goal() {
    let daemon = Arc::new(DaemonHarness::start().await.unwrap());
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let goal = ok(&raw.request(1, "goal.create", json!({"title": "g"})).await)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut ids = Vec::new();
    for i in 0..12 {
        ids.push(
            task(
                &mut raw,
                &format!("t{i}"),
                json!({"assignee": agent, "goal_id": goal}),
            )
            .await,
        );
    }

    let paused = Arc::new(AtomicBool::new(false));
    let violations = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let mut tickers = Vec::new();
    for t in 0..3u64 {
        let (daemon, paused, violations, ids) = (
            daemon.clone(),
            paused.clone(),
            violations.clone(),
            ids.clone(),
        );
        tickers.push(tokio::spawn(async move {
            let mut raw = Raw::session(&daemon, "cli", "normal").await;
            let mut n = 0;
            loop {
                let after_pause = paused.load(Ordering::SeqCst);
                n += 1;
                let tick = raw
                    .request(100 + n + t * 10_000, "dispatch.tick", json!({}))
                    .await;
                if let Some(launched) = tick.get("ok").and_then(|ok| ok["launched"].as_array()) {
                    for id in launched.iter().filter_map(|v| v.as_str()) {
                        if after_pause && ids.iter().any(|i| i == id) {
                            violations.lock().unwrap().push(id.to_owned());
                        }
                    }
                }
                if after_pause && n > 6 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        }));
    }
    // Let ticks start launching, then decide the pause.
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    let reply = raw
        .request(2, "goal.status", json!({"id": goal, "status": "paused"}))
        .await;
    assert_eq!(ok(&reply)["status"], "paused", "{reply}");
    paused.store(true, Ordering::SeqCst);
    for t in tickers {
        t.await.unwrap();
    }
    assert_eq!(&*violations.lock().unwrap(), &Vec::<String>::new());
    for id in &ids {
        if let Some(pid) = running_pid(&daemon, id).await {
            reaper.watch(pid);
        }
    }
    Arc::into_inner(daemon).unwrap().shutdown().await.unwrap();
}

// -- context --------------------------------------------------------------

/// Snapshot every file under the project's `.orchestry/context` directory:
/// name, length, mtime. Reads must leave it identical.
fn context_fingerprint(root: &std::path::Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let dir = root.join(".orchestry/context");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let meta = entry.metadata().unwrap();
            out.push((
                entry.file_name().to_string_lossy().into_owned(),
                meta.len(),
                meta.modified().unwrap(),
            ));
        }
    }
    out.sort();
    out
}

/// Advancing the runtime past maintenance ticks must leave expiry to the
/// explicit step when the harness disables the timer.
#[tokio::test(start_paused = true)]
async fn maintenance_timer_off_leaves_expiry_to_the_explicit_step() {
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    daemon
        .engine()
        .context
        .set("lapsing", "v", Some(1000))
        .await
        .unwrap();
    clock.advance_ms(2_000);
    // Virtual Tokio time: cross multiple maintenance ticks without a real wait.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(daemon.step_expiry().await.unwrap(), 1);
    assert_eq!(daemon.step_expiry().await.unwrap(), 0);
    daemon.shutdown().await.unwrap();
}

/// Reads filter but never delete; an explicit expiry sees the renewal; and
/// expiry runs with dispatch OFF (R10, R11).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_do_not_expire_and_renewal_survives() {
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let status = raw.request(1, "daemon.status", json!({})).await;
    assert_eq!(
        ok(&status)["dispatch"]["dispatch_on"],
        false,
        "expiry must not need dispatch"
    );

    ok(&raw
        .request(
            2,
            "context.set",
            json!({"key": "lapsing", "value": "v1", "ttl_ms": 1000}),
        )
        .await);
    ok(&raw
        .request(3, "context.set", json!({"key": "forever", "value": "keep"}))
        .await);

    // Time passes: the short key is now expired but still on disk.
    clock.advance_ms(2_000);
    let before = context_fingerprint(daemon.project_root());
    assert!(before.iter().any(|(name, _, _)| name == "lapsing.json"));
    for round in 0..3 {
        let listed = raw.request(10 + round, "context.list", json!({})).await;
        let keys: Vec<&str> = ok(&listed)
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["key"].as_str().unwrap())
            .collect();
        assert!(!keys.contains(&"lapsing"), "expired value served: {keys:?}");
        let got = raw
            .request(20 + round, "context.get", json!({"key": "lapsing"}))
            .await;
        assert!(ok(&got).is_null(), "{got}");
        let got = raw
            .request(30 + round, "context.get", json!({"key": "forever"}))
            .await;
        assert_eq!(ok(&got)["value"], "keep");
    }
    assert_eq!(context_fingerprint(daemon.project_root()), before);

    // Renew an expired value before explicit expiry: only `lapsing` is removed.
    ok(&raw
        .request(
            50,
            "context.set",
            json!({"key": "renewed", "value": "renewed-v1", "ttl_ms": 1000}),
        )
        .await);
    clock.advance_ms(2_000);
    ok(&raw
        .request(
            51,
            "context.set",
            json!({"key": "renewed", "value": "renewed-v2", "ttl_ms": 60_000}),
        )
        .await);
    let report = raw
        .request(52, "maintenance.expire_context", json!({}))
        .await;
    assert_eq!(ok(&report)["removed"], 1, "{report}");
    let listed = raw.request(53, "context.list", json!({})).await;
    let mut kept: Vec<(String, String)> = ok(&listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["key"].as_str().unwrap().to_owned(),
                e["value"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        [
            ("forever".into(), "keep".into()),
            ("renewed".into(), "renewed-v2".into())
        ],
        "the lapsed keys must be gone and the renewed one intact"
    );

    // Past the renewed ttl an explicit step removes it, from the harness
    // step as well (the same coordinated handler the timer uses).
    clock.advance_ms(61_000);
    let first = daemon.step_expiry().await.unwrap();
    assert_eq!(first, 1);
    assert_eq!(daemon.step_expiry().await.unwrap(), 0, "idempotent");
    let listed = raw.request(60, "context.list", json!({})).await;
    let keys: Vec<&str> = ok(&listed)
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["forever"]);
    daemon.shutdown().await.unwrap();
}

// -- goal edit race (R12 / F2) -------------------------------------------

/// A title edit racing a pause may be ordered either way, but never loses the
/// pause: the final goal is `paused` with the new title, in every interleaving.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goal_edit_cannot_resurrect_active() {
    let daemon = Arc::new(DaemonHarness::start().await.unwrap());
    let mut setup = Raw::session(&daemon, "cli", "normal").await;
    let mut edit = Raw::session(&daemon, "cli", "normal").await;
    let mut pause = Raw::session(&daemon, "cli", "normal").await;
    for round in 0..40u64 {
        let goal = ok(&setup
            .request(
                1 + round * 10,
                "goal.create",
                json!({"title": format!("orig {round}")}),
            )
            .await)["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let title = format!("edited {round}");
        let (edit_reply, pause_reply) = tokio::join!(
            edit.request(
                1 + round * 10,
                "goal.update",
                json!({"id": goal, "title": title})
            ),
            pause.request(
                1 + round * 10,
                "goal.status",
                json!({"id": goal, "status": "paused"})
            ),
        );
        ok(&edit_reply);
        ok(&pause_reply);
        let after = setup
            .request(5 + round * 10, "goal.get", json!({"id": goal}))
            .await;
        let got = &ok(&after)["goal"];
        assert_eq!(got["title"], title, "round {round}: edit lost: {after}");
        assert_eq!(
            got["status"], "paused",
            "round {round}: pause resurrected as active: {after}"
        );
    }
    Arc::into_inner(daemon).unwrap().shutdown().await.unwrap();
}

// -- mode enforcement ------------------------------------------------------

/// The daemon decides what a session may do, whatever the bridge in front of
/// it did: a raw client that skips every tool filter is still refused, and the
/// refusal executes nothing and moves no commit ordinal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_enforced_daemon_side_with_raw_client() {
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut owner, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut owner, "victim", json!({"assignee": agent})).await;
    ok(&owner.request(1, "dispatch.tick", json!({})).await);
    let pid = running_pid(&daemon, &id).await.unwrap();
    reaper.watch(pid);

    // A read reports the commit ordinal it observed.
    let seq_of = async |raw: &mut Raw| -> u64 {
        raw.request(900, "task.list", json!({})).await["commit_seq"]
            .as_u64()
            .unwrap()
    };

    let mut read_only = Raw::session(&daemon, "mcp", "read_only").await;
    let seq = seq_of(&mut read_only).await;
    // Destructive, Mutate and Control ops are all refused for read_only.
    let refused = [
        ("task.cancel", json!({"id": id})),
        ("task.create", json!({"title": "nope"})),
        ("goal.create", json!({"title": "nope"})),
        ("context.set", json!({"key": "k", "value": "v"})),
        ("context.delete", json!({"key": "k"})),
        ("maintenance.expire_context", json!({})),
        ("dispatch.set", json!({"dispatch": "on"})),
        ("dispatch.tick", json!({})),
        ("daemon.stop", json!({})),
        ("agent.stop", json!({"id": agent})),
    ];
    for (n, (op, args)) in refused.iter().enumerate() {
        let reply = read_only.request(1000 + n as u64, op, args.clone()).await;
        assert_eq!(err_code(&reply), "not_permitted", "{op}: {reply}");
        assert_eq!(reply["err"]["execution"], "not_executed", "{op}");
    }
    assert!(reaper.alive(pid), "a refused cancel still killed the agent");
    assert_eq!(running_pid(&daemon, &id).await, Some(pid));
    assert_eq!(
        seq_of(&mut read_only).await,
        seq,
        "a refusal advanced the commit ordinal"
    );
    // Reads still work.
    ok(&read_only.request(2000, "task.get", json!({"id": id})).await);
    ok(&read_only.request(2001, "task.list", json!({})).await);

    // Normal MCP: may mutate, still may not do destructive or control work.
    let mut normal = Raw::session(&daemon, "mcp", "normal").await;
    ok(&normal
        .request(1, "task.create", json!({"title": "allowed"}))
        .await);
    for (n, (op, args)) in [
        ("task.cancel", json!({"id": id})),
        ("goal.delete", json!({"id": "gl_none"})),
        ("dispatch.tick", json!({})),
        ("daemon.stop", json!({})),
    ]
    .iter()
    .enumerate()
    {
        let reply = normal.request(10 + n as u64, op, args.clone()).await;
        assert_eq!(err_code(&reply), "not_permitted", "{op}: {reply}");
    }
    // `goal.status force=true` is argument-dependent: refused without
    // allow_destructive even though the op is Mutate.
    let goal = ok(&owner
        .request(30, "goal.create", json!({"title": "g"}))
        .await)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let forced = normal
        .request(
            31,
            "goal.status",
            json!({"id": goal, "status": "achieved", "force": true}),
        )
        .await;
    assert_eq!(err_code(&forced), "not_permitted", "{forced}");
    assert!(reaper.alive(pid));

    // allow_destructive may cancel, but control ops stay owner-only.
    let mut destructive = Raw::session(&daemon, "mcp", "allow_destructive").await;
    let reply = destructive
        .request(1, "dispatch.set", json!({"dispatch": "on"}))
        .await;
    assert_eq!(err_code(&reply), "not_permitted", "{reply}");
    let reply = destructive
        .request(2, "task.cancel", json!({"id": id}))
        .await;
    assert_eq!(ok(&reply)["status"], "cancelled", "{reply}");
    assert!(!reaper.alive(pid));
    daemon.shutdown().await.unwrap();
}

/// Recovery adopts live local PIDs without creating collectors. Their later
/// death is owner maintenance, even while all scheduling stays switched off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_reconciles_adopted_death_without_dispatch_or_idle_publication() {
    #[derive(Default)]
    struct AdoptedProcesses {
        dead: std::sync::Mutex<Vec<u32>>,
        starts: std::sync::atomic::AtomicUsize,
    }
    impl AdoptedProcesses {
        fn set_dead(&self, pid: u32) {
            self.dead.lock().unwrap().push(pid);
        }
        fn spawned(&self) -> Vec<()> {
            vec![(); self.starts.load(Ordering::SeqCst)]
        }
    }
    #[async_trait::async_trait]
    impl orch_core::ProcessManager for AdoptedProcesses {
        fn is_alive(&self, pid: u32) -> bool {
            !self.dead.lock().unwrap().contains(&pid)
        }
        fn kill(&self, pid: u32, _: orch_core::Signal) {
            self.set_dead(pid);
        }
        async fn kill_with_grace(&self, pid: u32, _: std::time::Duration) {
            self.set_dead(pid);
        }
        fn spawn(
            &self,
            _: &str,
            _: &[String],
            _: orch_core::process::SpawnOptions,
        ) -> orch_core::Result<orch_core::process::SpawnedChild> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Err(orch_core::Error::other("maintenance must not spawn"))
        }
    }
    fn make_agent() -> orch_domain::Agent {
        serde_json::from_value(
            json!({"id":"agt_fixture", "name":"fixture", "adapter":"shell",
            "config":{}, "status":"idle", "stats":{"tasks_completed":0, "tasks_failed":0,
            "total_runs":0, "total_runtime_ms":0}}),
        )
        .unwrap()
    }
    fn make_task() -> orch_domain::Task {
        serde_json::from_value(
            json!({"id":"tsk_fixture", "title":"fixture", "description":"",
            "status":"todo", "priority":3, "labels":[], "depends_on":[],
            "created_at":"2025-01-01T00:00:00Z", "updated_at":"2025-01-01T00:00:00Z",
            "attempts":0, "max_attempts":3}),
        )
        .unwrap()
    }
    fn make_run() -> orch_domain::Run {
        serde_json::from_value(json!({"id":"run_fixture", "task_id":"tsk_fixture",
            "agent_id":"agt_fixture", "attempt":1, "status":"running",
            "started_at":"2025-01-01T00:00:00Z", "workspace_path":"", "prompt":""}))
        .unwrap()
    }
    use orch_domain::{
        AgentStatus, OrchestratorState, RetryEntry, RunStatus, RunningEntry, TaskStatus,
    };
    use orch_engine::container::{EngineSeams, build_full_with_processes};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};

    let project = tempfile::tempdir().unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(project.path().join(".orchestry"))
        .unwrap();
    let processes = Arc::new(AdoptedProcesses::default());
    let seed = build_full_with_processes(
        project.path(),
        EngineSeams {
            global_config_path: Some(project.path().join("global.yml")),
            ..Default::default()
        },
        Default::default(),
        processes.clone(),
    )
    .await
    .unwrap();
    let mut state = OrchestratorState::default();
    for (name, pid) in [("dead", 701), ("live", 702)] {
        let agent = format!("agt_{name}");
        let task = format!("tsk_{name}");
        let run = format!("run_{name}");
        seed.agents
            .save(&orch_domain::Agent {
                id: agent.clone(),
                status: AgentStatus::Running,
                current_task: Some(task.clone()),
                ..make_agent()
            })
            .await
            .unwrap();
        seed.tasks
            .save(&orch_domain::Task {
                id: task.clone(),
                assignee: Some(agent.clone()),
                status: TaskStatus::InProgress,
                attempts: 1,
                max_attempts: 1,
                ..make_task()
            })
            .await
            .unwrap();
        seed.runs
            .save(&orch_domain::Run {
                id: run.clone(),
                task_id: task.clone(),
                agent_id: agent.clone(),
                status: RunStatus::Running,
                pid: Some(pid),
                ..make_run()
            })
            .await
            .unwrap();
        state.running.insert(
            task.clone(),
            RunningEntry {
                task_id: task,
                agent_id: agent,
                run_id: run,
                pid,
                started_at: "2025-01-01T00:00:00Z".into(),
                last_event_at: "2025-01-01T00:00:00Z".into(),
                remote: None,
            },
        );
    }
    seed.agents
        .save(&orch_domain::Agent {
            id: "agt_queue".into(),
            ..make_agent()
        })
        .await
        .unwrap();
    seed.agents
        .save(&orch_domain::Agent {
            id: "agt_auto".into(),
            autonomous: Some(true),
            ..make_agent()
        })
        .await
        .unwrap();
    for (id, status) in [
        ("tsk_queue", TaskStatus::Todo),
        ("tsk_retry", TaskStatus::Retrying),
    ] {
        seed.tasks
            .save(&orch_domain::Task {
                id: id.into(),
                assignee: Some("agt_queue".into()),
                status,
                ..make_task()
            })
            .await
            .unwrap();
    }
    state.retry_queue.push(RetryEntry {
        task_id: "tsk_retry".into(),
        attempt: 2,
        due_at: "2025-01-01T00:00:00Z".into(),
        error: "earlier".into(),
    });
    seed.state.write(&state).await.unwrap();
    drop(seed);

    let daemon = DaemonHarness::builder()
        .processes(processes.clone())
        .start_in(project.path())
        .await
        .unwrap();
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    assert_eq!(
        ok(&owner.request(1, "daemon.status", json!({})).await)["dispatch"]["dispatch_on"],
        false
    );
    // Startup retained both PIDs; neither is being watched by a collector.
    assert_eq!(daemon.engine().state.read().await.unwrap().running.len(), 2);
    let mut observer = Raw::session(&daemon, "tui", "normal").await;
    ok(&observer
        .request(2, "subscribe", json!({"stores": ["tasks", "state"]}))
        .await);
    processes.set_dead(701);
    let mut task_pushed = false;
    let mut state_pushed = false;
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while !task_pushed || !state_pushed {
            let frame = observer.recv().await.unwrap();
            let ev = &frame["ev"];
            task_pushed |= ev["store"] == "tasks";
            state_pushed |= ev["store"] == "state";
        }
    })
    .await
    .expect("adopted death must push its task and state outcome with dispatch off");
    assert_eq!(task_status(&mut owner, "tsk_dead").await, "failed");
    assert_eq!(
        daemon
            .engine()
            .runs
            .get("run_dead")
            .await
            .unwrap()
            .unwrap()
            .status,
        RunStatus::Failed
    );
    let after = daemon.engine().state.read().await.unwrap();
    assert!(!after.running.contains_key("tsk_dead"));
    assert!(
        after.running.contains_key("tsk_live"),
        "maintenance must not apply stall policy"
    );
    assert_eq!(after.retry_queue, state.retry_queue);
    assert_eq!(task_status(&mut owner, "tsk_queue").await, "todo");
    assert_eq!(task_status(&mut owner, "tsk_retry").await, "retrying");
    assert_eq!(
        daemon
            .engine()
            .tasks
            .list(&Default::default())
            .await
            .unwrap()
            .len(),
        4,
        "no autonomous task seeded"
    );
    assert!(
        processes.spawned().is_empty(),
        "maintenance started an agent"
    );

    let path = project.path().join(".orchestry/state.json");
    let before = std::fs::metadata(&path).unwrap();
    let seq = owner.request(3, "daemon.status", json!({})).await["commit_seq"].clone();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    let status = owner.request(4, "daemon.status", json!({})).await;
    assert_eq!(
        status["commit_seq"], seq,
        "unchanged maintenance announces nothing"
    );
    assert_eq!(ok(&status)["dispatch"]["dispatch_on"], false);
    let after = std::fs::metadata(path).unwrap();
    assert_eq!(
        (after.ino(), after.modified().unwrap()),
        (before.ino(), before.modified().unwrap())
    );
    assert!(processes.spawned().is_empty());
    daemon.shutdown().await.unwrap();
}

/// Expiry follows local-run reconciliation in each maintenance iteration, so
/// removing an expired marker proves a complete owner pass has happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn maintenance_errors_are_deduplicated_until_a_successful_pass() {
    async fn pass(daemon: &DaemonHarness, clock: &TestClock) {
        daemon
            .engine()
            .context
            .set("pass-marker", "v", Some(1))
            .await
            .unwrap();
        let marker = daemon.engine().paths.context_path("pass-marker").unwrap();
        assert!(marker.exists());
        clock.advance_ms(2);
        tokio::time::timeout(std::time::Duration::from_secs(4), async {
            loop {
                if !tokio::fs::try_exists(&marker).await.unwrap() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("maintenance did not reach expiry after local reconciliation");
    }
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .start()
        .await
        .unwrap();
    let path = daemon.project_root().join(".orchestry/state.json");
    let stored = std::fs::read(&path).unwrap();
    let mut events = daemon.engine().events.subscribe();
    std::fs::write(&path, b"invalid state json").unwrap();
    for _ in 0..3 {
        pass(&daemon, &clock).await;
    }
    let failures = |events: Vec<Arc<orch_domain::OrchestratorEvent>>| {
        events
            .into_iter()
            .filter(|event| {
                matches!(event.as_ref(),
            orch_domain::OrchestratorEvent::OrchestratorError {context, fatal: false, ..}
            if context == "local run maintenance")
            })
            .collect::<Vec<_>>()
    };
    let first = failures(events.try_drain());
    std::fs::write(&path, &stored).unwrap();
    pass(&daemon, &clock).await;
    let recovery = failures(events.try_drain());
    std::fs::write(&path, b"invalid state json").unwrap();
    pass(&daemon, &clock).await;
    let second = failures(events.try_drain());
    std::fs::write(&path, &stored).unwrap();
    daemon.shutdown().await.unwrap();
    assert_eq!(
        first.len(),
        1,
        "one error for repeated failing passes: {first:?}"
    );
    assert!(recovery.is_empty(), "a successful pass is not an error");
    assert_eq!(second.len(), 1, "recovery must reset error suppression");
    assert_eq!(
        first[0], second[0],
        "same failure must be reported again after recovery"
    );
}
