//! Deterministic wake tests: an explicit clock driver prevents Tokio's idle
//! auto-advance from hiding a wait for the 60 s periodic fallback.
use super::*;
use crate::lifecycle::internal_context;
use crate::ops::Handler;
use orch_domain::OrchestratorEvent;
use orch_engine::container::commit::{CommitSource, CommitStore};
use orch_proto::ops::args::{DispatchSetArgs, DispatchSwitch};

struct Rig {
    _temp: tempfile::TempDir,
    handler: ServiceHandler,
    quit: watch::Sender<bool>,
    loop_task: tokio::task::JoinHandle<()>,
    driver: tokio::task::JoinHandle<()>,
    events: orch_engine::EventSubscription,
    processes: Arc<Processes>,
    clock: Arc<tests::TestClock>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.quit.send_replace(true);
        self.loop_task.abort();
        self.driver.abort();
    }
}
impl Rig {
    async fn new(on: bool) -> Self {
        let driver = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let (temp, root) = tests::project().await;
        let mut config = orch_domain::default_config();
        config.scheduling.poll_interval_ms = 60_000;
        use orch_core::storage::ConfigStore;
        orch_infra::storage::FileConfigStore::new(orch_infra::storage::Paths::new(&root))
            .write(&config)
            .await
            .unwrap();
        let processes = Arc::new(Processes::default());
        let clock = Arc::new(tests::TestClock(std::sync::atomic::AtomicI64::new(
            1_760_000_000_000,
        )));
        let engine = Arc::new(
            orch_engine::build_full_with_processes(
                &root,
                orch_engine::EngineSeams {
                    clock: clock.clone(),
                    global_config_path: Some(root.join("global.yml")),
                    ..Default::default()
                },
                config.peer,
                processes.clone(),
            )
            .await
            .unwrap(),
        );
        let coordinator = Arc::new(crate::coordinator::ProjectCoordinator::new(
            engine.operation_boundary(),
        ));
        let handler = ServiceHandler::new(engine, coordinator);
        Handler::dispatch_set(
            &handler,
            &internal_context(),
            DispatchSetArgs {
                dispatch: if on {
                    DispatchSwitch::On
                } else {
                    DispatchSwitch::Off
                },
            },
        )
        .await
        .unwrap();
        let events = handler.engine.events.subscribe();
        let (quit, rx) = watch::channel(false);
        let loop_task = tokio::spawn(run_loop(handler.clone(), rx));
        settle().await;
        let mut rig = Self {
            _temp: temp,
            handler,
            quit,
            loop_task,
            driver,
            events,
            processes,
            clock,
        };
        if on {
            advance(500).await;
            rig.wait_ticks(1).await;
        }
        rig
    }
    async fn commit(&self, source: CommitSource) {
        let boundary = self.handler.engine.operation_boundary();
        boundary
            .run_from(source, false, async {
                boundary.note_store(CommitStore::Remote);
            })
            .await;
        settle().await;
    }
    fn ticks(&mut self) -> usize {
        let mut count = 0;
        for event in self.events.try_drain() {
            count += usize::from(matches!(*event, OrchestratorEvent::OrchestratorTick { .. }));
        }
        count
    }
    async fn wait_ticks(&mut self, n: usize) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        let mut count = 0;
        while count < n && std::time::Instant::now() < end {
            count += self.ticks();
            tokio::task::yield_now().await;
        }
        assert_eq!(count, n, "wake ticks before the periodic deadline");
    }
}
async fn settle() {
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
}
async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn a_peer_origin_commit_wakes_the_scheduler() {
    let mut rig = Rig::new(true).await;
    rig.commit(CommitSource::Peer).await;
    advance(499).await;
    assert_eq!(rig.ticks(), 0);
    advance(1).await;
    rig.wait_ticks(1).await;
}

#[tokio::test(start_paused = true)]
async fn hundred_commits_coalesce_and_noop_tick_does_not_self_wake() {
    let mut rig = Rig::new(true).await;
    for _ in 0..100 {
        rig.commit(CommitSource::Rpc).await;
    }
    advance(500).await;
    rig.wait_ticks(1).await;
    assert!(
        rig.handler.wake.deadline().is_none(),
        "tick-origin commits must not re-arm the wake"
    );
    advance(2000).await;
    assert_eq!(rig.ticks(), 0, "no-op tick must not schedule itself");
}

#[derive(Default)]
struct Processes {
    children: std::sync::Mutex<Vec<Child>>,
    probe: std::sync::Mutex<Option<Child>>,
    fail: std::sync::atomic::AtomicBool,
    calls: std::sync::atomic::AtomicUsize,
}
struct Child {
    pid: u32,
    _stdout: tokio::io::DuplexStream,
    _stderr: tokio::io::DuplexStream,
    exit: orch_core::process::ExitNotifier,
}
impl Processes {
    fn len(&self) -> usize {
        self.children.lock().unwrap().len()
    }
    fn complete(&self) {
        for child in self.children.lock().unwrap().drain(..) {
            child.exit.notify(orch_core::process::ExitOutcome {
                code: Some(0),
                signal: None,
            });
        }
    }
}
#[async_trait::async_trait]
impl orch_core::ProcessManager for Processes {
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        _: orch_core::process::SpawnOptions,
    ) -> orch_core::Result<orch_core::process::SpawnedChild> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(orch_core::Error::other("injected spawn failure"));
        }
        if command == "opencode" && args == ["run", "--help"] {
            return Ok(orch_core::process::SpawnedChild {
                pid: 80001,
                stdout: Some(Box::new(std::io::Cursor::new(
                    b"--model provider/model#variant",
                ))),
                stderr: None,
                stdin: None,
                exit: orch_core::process::ExitSignal::resolved(orch_core::process::ExitOutcome {
                    code: Some(0),
                    signal: None,
                }),
            });
        }
        let (stdout, read_out) = tokio::io::duplex(4096);
        let (stderr, read_err) = tokio::io::duplex(4096);
        let (notifier, exit) = orch_core::process::ExitSignal::channel();
        let mut children = self.children.lock().unwrap();
        let pid = 70000 + children.len() as u32;
        let child = Child {
            pid,
            _stdout: stdout,
            _stderr: stderr,
            exit: notifier,
        };
        if command == "opencode" && args == ["--version"] {
            *self.probe.lock().unwrap() = Some(child);
        } else {
            children.push(child);
        }
        Ok(orch_core::process::SpawnedChild {
            pid,
            stdout: Some(Box::new(read_out)),
            stderr: Some(Box::new(read_err)),
            stdin: None,
            exit,
        })
    }
    fn is_alive(&self, pid: u32) -> bool {
        self.children.lock().unwrap().iter().any(|c| c.pid == pid)
    }
    fn kill(&self, _: u32, _: orch_core::Signal) {
        self.complete();
    }
    async fn kill_with_grace(&self, _: u32, _: Duration) {
        self.complete();
    }
}

impl Rig {
    async fn task(&self, depends_on: Vec<String>) -> orch_domain::Task {
        self.task_with(depends_on, false, None).await
    }
    async fn task_with(
        &self,
        depends_on: Vec<String>,
        probe: bool,
        goal: Option<String>,
    ) -> orch_domain::Task {
        let ctx = internal_context();
        let agent = Handler::agent_create(&self.handler, &ctx, serde_json::from_value(serde_json::json!({
            "name": format!("agent-{}", self.handler.coordinator.current_commit_seq()),
            "adapter": if probe { "opencode" } else { "shell" }, "command": "fake-agent",
            "model": if probe { Some("openrouter/anthropic/claude-opus-5.5") } else { None },
            "effort": if probe { Some("high") } else { None }, "workspace_mode": "shared", "approval_policy": "auto",
            "env": [{"name":"HOME","value":self._temp.path().to_str().unwrap()}, {"name":"PATH","value":"/usr/bin:/bin"}]
        })).unwrap()).await.unwrap().agent;
        self.client_task(serde_json::json!({
            "title": "wake launch", "assignee": agent.id, "depends_on": depends_on, "goal_id": goal
        }))
        .await
    }
}
async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while !done().await {
        assert!(std::time::Instant::now() < end, "timed out: {what}");
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn continuous_traffic_cannot_postpone_first_wake_and_rate_is_bounded() {
    let mut rig = Rig::new(true).await;
    let mut ticks = 0;
    for n in 1..=50 {
        rig.commit(CommitSource::Rpc).await;
        advance(100).await;
        if n == 5 {
            rig.wait_ticks(1).await;
            ticks += 1;
        } else {
            ticks += rig.ticks();
        }
        assert!(ticks <= 1 + n / 5, "500 ms bound at {n}: {ticks}");
    }
    assert!(
        ticks >= 5,
        "continuous traffic must keep dispatch responsive"
    );
}

#[tokio::test(start_paused = true)]
async fn off_commits_do_not_tick_and_on_rearms() {
    let mut rig = Rig::new(false).await;
    rig.commit(CommitSource::Rpc).await;
    advance(2000).await;
    assert_eq!(rig.ticks(), 0);
    Handler::dispatch_set(
        &rig.handler,
        &internal_context(),
        DispatchSetArgs {
            dispatch: DispatchSwitch::On,
        },
    )
    .await
    .unwrap();
    advance(500).await;
    rig.wait_ticks(1).await;
}

#[tokio::test(start_paused = true)]
async fn empty_context_maintenance_does_not_wake() {
    let mut rig = Rig::new(true).await;
    let before = rig.handler.coordinator.current_commit_seq();
    assert_eq!(rig.handler.expire_context().await.unwrap(), 0);
    assert_eq!(
        rig.handler.coordinator.current_commit_seq(),
        before,
        "empty maintenance must not publish a commit"
    );
    advance(500).await;
    assert_eq!(rig.ticks(), 0);
    assert!(
        rig.handler.wake.deadline().is_none(),
        "empty expiry must not arm a wake"
    );
}

#[tokio::test(start_paused = true)]
async fn eligible_task_launches_at_500ms_not_the_60s_periodic_deadline() {
    let rig = Rig::new(true).await;
    let task = rig.task(vec![]).await;
    advance(499).await;
    assert_eq!(rig.processes.len(), 0);
    advance(1).await;
    until("500 ms fake launch", async || rig.processes.len() == 1).await;
    assert_eq!(
        rig.handler
            .engine
            .tasks
            .get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .attempts,
        1
    );
    rig.processes.complete();
}

#[tokio::test(start_paused = true)]
async fn a_collector_completion_spawned_by_a_tick_wakes_the_scheduler() {
    let mut rig = Rig::new(true).await;
    let first = rig.task(vec![]).await;
    let dependent = rig.task(vec![first.id.clone()]).await;
    advance(500).await;
    rig.wait_ticks(1).await;
    assert_eq!(rig.processes.len(), 1, "dependent remains blocked");
    // Consume every commit from launch, then make the tick-spawned collector
    // finish after the tick has returned. No independent client write follows.
    assert!(rig.handler.wake.deadline().is_none());
    rig.processes.complete();
    until("collector commit", async || {
        rig.handler
            .engine
            .tasks
            .get(&first.id)
            .await
            .unwrap()
            .unwrap()
            .status
            == orch_domain::TaskStatus::Done
            && rig.handler.wake.deadline().is_some()
    })
    .await;
    advance(500).await;
    rig.wait_ticks(1).await;
    assert_eq!(
        rig.handler
            .engine
            .tasks
            .get(&dependent.id)
            .await
            .unwrap()
            .unwrap()
            .attempts,
        1
    );
    rig.processes.complete();
}

#[tokio::test(start_paused = true)]
async fn prelaunch_failure_preserves_backoff_under_commit_traffic() {
    let mut rig = Rig::new(true).await;
    rig.processes
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let task = rig.task(vec![]).await;
    advance(500).await;
    rig.wait_ticks(1).await;
    let calls = rig
        .processes
        .calls
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(calls > 0);
    for _ in 0..4 {
        rig.commit(CommitSource::Rpc).await;
        advance(500).await;
        rig.wait_ticks(1).await;
    }
    assert_eq!(
        rig.processes
            .calls
            .load(std::sync::atomic::Ordering::SeqCst),
        calls,
        "wake must not bypass pre-launch retry back-off"
    );
    assert_eq!(
        rig.handler
            .engine
            .tasks
            .get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .attempts,
        1
    );
    // Move the engine clock past the first retry deadline, preserving the
    // periodic fallback as the source of time-dependent retry progress.
    rig.clock
        .0
        .fetch_add(60_001, std::sync::atomic::Ordering::SeqCst);
    advance(60_000).await;
    until("periodic retry", async || {
        rig.processes
            .calls
            .load(std::sync::atomic::Ordering::SeqCst)
            > calls
    })
    .await;
}

impl Processes {
    async fn release_probe(&self) {
        use tokio::io::AsyncWriteExt;
        let child = self.probe.lock().unwrap().take();
        if let Some(mut child) = child {
            child._stdout.write_all(b"opencode v2.1.2").await.unwrap();
            child.exit.notify(orch_core::process::ExitOutcome {
                code: Some(0),
                signal: None,
            });
        }
    }
}

#[tokio::test(start_paused = true)]
async fn commits_during_a_tick_schedule_exactly_one_followup() {
    let mut rig = Rig::new(true).await;
    rig.task_with(vec![], true, None).await;
    advance(500).await;
    until("tick in external probe", async || {
        rig.processes.probe.lock().unwrap().is_some()
    })
    .await;
    for n in 0..100 {
        Handler::context_set(
            &rig.handler,
            &internal_context(),
            serde_json::from_value(
                serde_json::json!({"key":format!("during-{n}"), "value":"changed"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    }
    assert_eq!(rig.ticks(), 0, "first tick remains in flight");
    rig.processes.release_probe().await;
    rig.wait_ticks(1).await;
    advance(499).await;
    assert_eq!(rig.ticks(), 0);
    advance(1).await;
    rig.wait_ticks(1).await;
    advance(500).await;
    assert_eq!(rig.ticks(), 0, "no third tick");
    rig.processes.complete();
}

#[tokio::test(start_paused = true)]
async fn concurrent_goal_pause_prevents_launch_from_pending_wake() {
    use orch_proto::ops::args::{GoalCreateArgs, GoalStatusArgs};
    let mut rig = Rig::new(true).await;
    let ctx = internal_context();
    let goal = Handler::goal_create(
        &rig.handler,
        &ctx,
        serde_json::from_value::<GoalCreateArgs>(
            serde_json::json!({"title":"paused while preparing"}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let task = rig.task_with(vec![], true, Some(goal.id.clone())).await;
    advance(500).await;
    until("tick in probe before pause", async || {
        rig.processes.probe.lock().unwrap().is_some()
    })
    .await;
    Handler::goal_status(
        &rig.handler,
        &ctx,
        serde_json::from_value::<GoalStatusArgs>(
            serde_json::json!({"id":goal.id, "status":"paused"}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    rig.processes.release_probe().await;
    rig.wait_ticks(1).await;
    advance(500).await;
    rig.wait_ticks(1).await;
    assert_eq!(
        rig.processes.len(),
        0,
        "pause cancels the pre-launch decision"
    );
    assert_eq!(
        rig.handler
            .engine
            .tasks
            .get(&task.id)
            .await
            .unwrap()
            .unwrap()
            .attempts,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn stopping_and_lost_ownership_are_rechecked_at_wake_execution() {
    for lost in [false, true] {
        let mut rig = Rig::new(true).await;
        rig.task(vec![]).await;
        if lost {
            rig.handler.set_ownership_lost().await.unwrap();
        } else {
            rig.handler.coordinator.stop_admission();
        }
        advance(500).await;
        assert_eq!(rig.ticks(), 0);
        assert_eq!(rig.processes.len(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn nonempty_context_expiry_still_wakes() {
    use orch_proto::ops::args::ContextSetArgs;
    let mut rig = Rig::new(true).await;
    Handler::context_set(
        &rig.handler,
        &internal_context(),
        serde_json::from_value::<ContextSetArgs>(
            serde_json::json!({"key":"lease", "value":"value", "ttl_ms":1}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    advance(500).await;
    rig.wait_ticks(1).await;
    rig.clock
        .0
        .fetch_add(2, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(rig.handler.expire_context().await.unwrap(), 1);
    advance(500).await;
    rig.wait_ticks(1).await;
}

impl Rig {
    /// A raw CLI session over the production framing/handler path. No store
    /// seeding substitutes for the client mutation whose latency we assert.
    async fn client_task(&self, args: serde_json::Value) -> orch_domain::Task {
        use crate::session::{Peer, Server, ServerConfig};
        use orch_proto::handshake::{Caller, CallerKind, Hello, ProjectId, SessionMode};
        use std::os::unix::fs::MetadataExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let root = self.handler.engine.paths.project_root();
        let meta = std::fs::metadata(root.join(".orchestry")).unwrap();
        let project_id = ProjectId {
            dev: meta.dev(),
            ino: meta.ino(),
        };
        let uid = nix::unistd::geteuid().as_raw();
        let server = Server::new(
            ServerConfig {
                project_root: root.to_owned(),
                project_id,
                instance: "daemon_wake".into(),
                pid: std::process::id(),
                protocol: orch_proto::LOCAL_PROTOCOL,
                binary: orch_proto::BINARY_VERSION.into(),
                euid: uid,
            },
            self.handler.clone(),
        );
        let (client, stream) = tokio::io::duplex(1024 * 1024);
        server.admit(
            stream,
            Peer {
                uid: Some(uid),
                pid: Some(std::process::id()),
            },
        );
        let mut client = BufReader::new(client);
        let hello = Hello::session(
            orch_proto::path::WirePath::new(root.to_owned()).unwrap(),
            project_id,
            &Caller {
                kind: CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            SessionMode::Normal,
        );
        let mut bytes = serde_json::to_vec(&serde_json::json!({"hello":hello})).unwrap();
        bytes.push(b'\n');
        client.get_mut().write_all(&bytes).await.unwrap();
        let mut line = String::new();
        client.read_line(&mut line).await.unwrap();
        let welcome: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(welcome["welcome"]["compatible"], true);
        let mut bytes =
            serde_json::to_vec(&serde_json::json!({"id":1,"req":{"op":"task.create","args":args}}))
                .unwrap();
        bytes.push(b'\n');
        client.get_mut().write_all(&bytes).await.unwrap();
        line.clear();
        client.read_line(&mut line).await.unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(reply.get("ok").is_some(), "task.create: {reply}");
        serde_json::from_value(reply["ok"].clone()).unwrap()
    }
}

#[tokio::test(start_paused = true)]
async fn all_external_sources_wake_but_manual_tick_does_not() {
    let mut rig = Rig::new(true).await;
    for source in [
        CommitSource::Rpc,
        CommitSource::Internal,
        CommitSource::Peer,
        CommitSource::Expiry,
        CommitSource::Collector,
    ] {
        rig.commit(source).await;
        advance(500).await;
        rig.wait_ticks(1).await;
    }
    Handler::dispatch_tick(
        &rig.handler,
        &internal_context(),
        orch_proto::ops::args::DispatchTickArgs {},
    )
    .await
    .unwrap();
    assert_eq!(rig.ticks(), 1, "manual tick itself");
    assert!(rig.handler.wake.deadline().is_none());
    advance(500).await;
    assert_eq!(rig.ticks(), 0, "manual tick must not self-wake");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_idle_peer_wake_ticks_over_ten_seconds() {
    async fn handler_at(root: &std::path::Path, listen: bool) -> ServiceHandler {
        use orch_core::storage::ConfigStore;
        std::fs::create_dir(root.join(".orchestry")).unwrap();
        let mut config = orch_domain::default_config();
        config.scheduling.poll_interval_ms = 60_000;
        config.peer.name = Some(if listen { "worker" } else { "controller" }.into());
        config.peer.listen = listen.then(|| "127.0.0.1:0".into());
        orch_infra::storage::FileConfigStore::new(orch_infra::storage::Paths::new(root))
            .write(&config)
            .await
            .unwrap();
        let engine = Arc::new(
            orch_engine::build_full_with_processes(
                root,
                orch_engine::EngineSeams {
                    global_config_path: Some(root.join("global.yml")),
                    ..Default::default()
                },
                config.peer,
                Arc::new(Processes::default()),
            )
            .await
            .unwrap(),
        );
        let coordinator = Arc::new(crate::coordinator::ProjectCoordinator::new(
            engine.operation_boundary(),
        ));
        ServiceHandler::new(engine, coordinator)
    }
    let worker_root = tempfile::tempdir().unwrap();
    let controller_root = tempfile::tempdir().unwrap();
    let worker = handler_at(worker_root.path(), true).await;
    let controller = handler_at(controller_root.path(), false).await;
    let worker_runtime = orch_peer::PeerRuntime::start(worker.engine.clone())
        .await
        .unwrap();
    let addr = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) =
                tokio::fs::read(worker.engine.paths.remotes_dir().join("_listener.json")).await
            {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if let Some(addr) = value["listen_addr"].as_str() {
                    break addr.to_owned();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    orch_infra::storage::remote::FileRemoteStore::new(controller.engine.paths.clone())
        .add(orch_domain::RemoteConfig {
            alias: "worker".into(),
            addr,
            added_at: controller.engine.clock.now_iso(),
        })
        .await
        .unwrap();
    let controller_runtime = orch_peer::PeerRuntime::start(controller.engine.clone())
        .await
        .unwrap();
    let (quit, rx) = watch::channel(false);
    for handler in [&worker, &controller] {
        Handler::dispatch_set(
            handler,
            &internal_context(),
            DispatchSetArgs {
                dispatch: DispatchSwitch::On,
            },
        )
        .await
        .unwrap();
    }
    let worker_loop = tokio::spawn(run_loop(worker.clone(), rx.clone()));
    let controller_loop = tokio::spawn(run_loop(controller.clone(), rx));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) =
                tokio::fs::read(controller.engine.paths.remotes_dir().join("worker.json")).await
            {
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if value["online"] == true {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("linked controller");
    // Let startup/ON commits and a full heartbeat window finish first.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let mut worker_events = worker.engine.events.subscribe();
    let mut controller_events = controller.engine.events.subscribe();
    let worker_records = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let (handler, captured) = (&worker, worker_records.clone());
        let wake = handler.wake.clone();
        let hub = handler.hub.clone();
        handler
            .engine
            .operation_boundary()
            .set_publisher(Some(Arc::new(move |publication| {
                if let orch_engine::container::commit::Publication::Commit(record) = publication {
                    captured.lock().unwrap().push(record.clone());
                }
                wake.publish(publication);
                hub.publish(publication);
            })));
    }
    let worker_seq = worker.coordinator.current_commit_seq();
    let started = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let elapsed = started.elapsed();
    let ticks = |events: &mut orch_engine::EventSubscription| {
        events
            .try_drain()
            .iter()
            .filter(|ev| matches!(***ev, OrchestratorEvent::OrchestratorTick { .. }))
            .count()
    };
    let worker_ticks = ticks(&mut worker_events);
    let controller_ticks = ticks(&mut controller_events);
    println!(
        "idle_peer_wake: elapsed_ms={} controller_ticks={controller_ticks} worker_ticks={worker_ticks}",
        elapsed.as_millis()
    );
    println!("idle_worker_commits: {:?}", worker_records.lock().unwrap());
    assert_eq!(worker_ticks, 0, "idle worker outbox/sweep must not wake");
    assert_eq!(
        worker.coordinator.current_commit_seq(),
        worker_seq,
        "idle worker must not commit"
    );
    // This value is a measurement for CP7, not a release latency/count gate.
    assert!(
        controller_ticks > 0,
        "real controller status commits remain eligible"
    );
    quit.send_replace(true);
    worker_loop.await.unwrap();
    controller_loop.await.unwrap();
    controller_runtime.shutdown().await;
    worker_runtime.shutdown().await;
}
