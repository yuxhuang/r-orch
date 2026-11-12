//! [`DaemonBackend`] over a scripted in-memory daemon (`orch_client::testing`):
//! the observation session is opened before the first read, carries both
//! invalidations and the bus, and its gap / resync / new-instance / EOF paths
//! end the way brief D7 says. No socket, no process, no real daemon.

use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use orch_client::testing::{FakeServer, fake_transport};
use orch_client::{Client, Project};
use orch_domain::{OrchestratorEvent, PromptMode, Task, TaskStatus, TuiPaletteName};
use orch_proto::bounded::Id;
use orch_proto::frame::{
    ChangeKind, ErrFrame, Event, EventBody, EventFrame, OkFrame, ResyncReason, Store,
};
use orch_proto::handshake::{Caller, CallerKind, SessionMode};
use orch_proto::ops::Request;
use orch_proto::ops::args::SubscribeArgs;
use orch_proto::ops::results::SubscribeAck;
use orch_tui::backend::{Observation, ObservedStore, TuiBackend};
use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc;

use super::backend::DaemonBackend;
use super::connect::Connector;

/// What a scripted subscription session does next.
enum Script {
    Event {
        seq: u64,
        body: EventBody,
    },
    /// Drop the connection: EOF on the observation lane.
    Close,
}

/// The fake daemon behind every session a test's [`Connector`] opens.
struct FakeDaemon {
    instance: Mutex<String>,
    commit_seq: AtomicU64,
    /// Every request name, in arrival order, across all sessions.
    log: Mutex<Vec<String>>,
    subscribes: Mutex<Vec<SubscribeArgs>>,
    /// One script sender per subscription, in subscription order.
    scripts: Mutex<Vec<mpsc::UnboundedSender<Script>>>,
    connects: AtomicUsize,
    tasks: Mutex<Vec<Task>>,
    /// When set, the `ui.bootstrap` reply waits for this many pushes to be
    /// scripted first (a change committed between subscribe and the read).
    push_before_bootstrap: Mutex<Option<EventBody>>,
    dispatch_on: AtomicBool,
    /// While set, the connector finds no daemon (connection refused).
    down: AtomicBool,
}

impl FakeDaemon {
    fn new() -> Arc<FakeDaemon> {
        Arc::new(FakeDaemon {
            instance: Mutex::new("inst-a".into()),
            commit_seq: AtomicU64::new(100),
            log: Mutex::new(Vec::new()),
            subscribes: Mutex::new(Vec::new()),
            scripts: Mutex::new(Vec::new()),
            connects: AtomicUsize::new(0),
            tasks: Mutex::new(vec![task("tsk_1")]),
            push_before_bootstrap: Mutex::new(None),
            dispatch_on: AtomicBool::new(false),
            down: AtomicBool::new(false),
        })
    }

    fn log(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn instance(&self) -> String {
        self.instance
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Script the `n`th subscription (0-based).
    fn script(&self, n: usize, script: Script) {
        let scripts = self.scripts.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(scripts[n].send(script).is_ok(), "subscription alive");
    }

    fn subscriptions(&self) -> usize {
        self.scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

fn task(id: &str) -> Task {
    let mut task = orch_tui::backend::make_task(id, "Task", TaskStatus::Todo);
    task.id = id.into();
    task
}

fn change(store: Store, id: Option<&str>, commit_seq: u64) -> EventBody {
    EventBody::Change {
        store,
        id: id.map(|id| Id::new(id).expect("scripted entity ID is valid")),
        kind: if id.is_some() {
            ChangeKind::Created
        } else {
            ChangeKind::Updated
        },
        commit_seq,
    }
}

async fn ok<T: Serialize>(peer: &mut FakeServer, id: orch_proto::frame::RequestId, seq: u64, v: T) {
    let ok = serde_json::value::to_raw_value(&v).expect("scripted response serializes");
    let _ = peer
        .send(&OkFrame {
            id,
            ok,
            commit_seq: seq,
        })
        .await;
}

async fn serve(mut peer: FakeServer, daemon: Arc<FakeDaemon>) {
    let instance = daemon.instance();
    if peer
        .accept(Id::new(instance.clone()).expect("scripted instance ID is valid"))
        .await
        .is_err()
    {
        return;
    }
    while let Ok(Some(frame)) = peer.recv().await {
        daemon
            .log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(frame.req.name().to_owned());
        let seq = daemon.commit_seq.load(Ordering::SeqCst);
        match frame.req {
            Request::Subscribe(args) => {
                daemon
                    .subscribes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(args);
                let (tx, mut rx) = mpsc::unbounded_channel();
                let subscription = {
                    let mut scripts = daemon
                        .scripts
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    scripts.push(tx);
                    scripts.len() as u64
                };
                ok(
                    &mut peer,
                    frame.id,
                    seq,
                    SubscribeAck {
                        subscription,
                        instance: instance.clone(),
                        seq: 0,
                    },
                )
                .await;
                if subscription == 1
                    && let Some(body) = daemon
                        .push_before_bootstrap
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take()
                {
                    let tx = daemon
                        .scripts
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)[0]
                        .clone();
                    let _ = tx.send(Script::Event { seq: 1, body });
                }
                while let Some(script) = rx.recv().await {
                    match script {
                        Script::Event { seq, body } => {
                            let ev = Event {
                                subscription,
                                instance: Id::new(instance.clone())
                                    .expect("scripted instance ID is valid"),
                                seq,
                                body,
                            };
                            if peer.send(&EventFrame { ev }).await.is_err() {
                                return;
                            }
                        }
                        Script::Close => return,
                    }
                }
                return;
            }
            Request::UiBootstrap(_) => {
                // A push scripted "before the bootstrap" has to be on the
                // wire before this reply: give the subscription a moment.
                tokio::time::sleep(Duration::from_millis(20)).await;
                ok(
                    &mut peer,
                    frame.id,
                    seq,
                    json!({
                        "project_name": "fake", "version": "0", "default_adapter": "claude",
                        "palette": TuiPaletteName::Amber, "activity_filter": "all",
                        "notifications": {"toast": true, "bell": false},
                        "max_concurrent": 2, "dispatch_on": false,
                        "prompt_mode": PromptMode::Normal, "startup_warnings": [],
                    }),
                )
                .await;
            }
            Request::TaskList(_) => {
                let tasks = daemon
                    .tasks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                ok(&mut peer, frame.id, seq, tasks).await;
            }
            Request::TaskCreate(_) => {
                let seq = daemon.commit_seq.fetch_add(1, Ordering::SeqCst) + 1;
                ok(&mut peer, frame.id, seq, task("tsk_new")).await;
            }
            Request::AgentList(_) | Request::GoalList(_) | Request::TeamList(_) => {
                ok(&mut peer, frame.id, seq, Vec::<()>::new()).await;
            }
            Request::StateGet(_) => {
                ok(&mut peer, frame.id, seq, orch_domain::default_state()).await;
            }
            Request::RunTimings(_) => {
                let snapshot = orch_proto::ops::results::TimingSnapshot::default();
                ok(&mut peer, frame.id, seq, snapshot).await;
            }
            Request::RemoteStatus(_) => {
                let report = json!({
                    "listen": null, "listen_configured": null, "controller": null,
                    "last_session_error": null, "inbound": null, "remotes": [],
                });
                ok(&mut peer, frame.id, seq, report).await;
            }
            Request::ConfigPromptModeGet(_) => {
                ok(&mut peer, frame.id, seq, PromptMode::Normal).await;
            }
            Request::ModelsCatalog(_) => {
                ok(&mut peer, frame.id, seq, Vec::<()>::new()).await;
            }
            Request::RunHistory(_) => {
                ok(&mut peer, frame.id, seq, json!({"batches": []})).await;
            }
            Request::DaemonStatus(_) => {
                use orch_proto::ops::results::{DaemonStatus, DispatchState, OwnershipStatus};
                use orch_proto::path::WirePath;
                let path = |p: &str| {
                    WirePath::new(PathBuf::from(p)).expect("scripted absolute path is valid")
                };
                let status = DaemonStatus {
                    pid: 1,
                    project_root: path("/fake"),
                    socket: path("/fake/s"),
                    protocol: orch_proto::LOCAL_PROTOCOL,
                    binary: "0".into(),
                    instance: instance.clone(),
                    executable: path("/fake/orch"),
                    started_at: "2026-11-09T00:00:00Z".into(),
                    dispatch: DispatchState {
                        dispatch_on: daemon.dispatch_on.load(Ordering::SeqCst),
                        batch_active: false,
                    },
                    running_tasks: Vec::new(),
                    sessions: 1,
                    session_cap: 64,
                    busy_rejections: 0,
                    ownership: OwnershipStatus::Held,
                    removed_env: Vec::new(),
                };
                ok(&mut peer, frame.id, seq, status).await;
            }
            _ => {
                let err = orch_proto::error::ErrorBody::new(
                    orch_proto::error::ErrorCode::General,
                    "not scripted",
                    orch_proto::error::Execution::NotExecuted,
                );
                let _ = peer.send(&ErrFrame { id: frame.id, err }).await;
            }
        }
    }
}

/// A connector whose every session is served by `daemon`, or refused while
/// `daemon.down` is set. It only ever dials the in-memory transport.
fn connector(daemon: &Arc<FakeDaemon>, root: &std::path::Path) -> Connector {
    let opener = Arc::clone(daemon);
    let project_root = root.to_path_buf();
    Connector::new(root.to_path_buf(), false, move || {
        let daemon = Arc::clone(&opener);
        let root = project_root.clone();
        Box::pin(async move {
            daemon.connects.fetch_add(1, Ordering::SeqCst);
            if daemon.down.load(Ordering::SeqCst) {
                return Err(orch_client::ClientError::Connection(
                    orch_client::NoDaemon::no_daemon(
                        &root.to_string_lossy(),
                        orch_proto::error::NoDaemonDetail::ConnectionRefused,
                    ),
                ));
            }
            let (stream, peer) = fake_transport();
            tokio::spawn(serve(peer, daemon));
            let project = Project::discover(&root, None)?;
            Client::from_stream(
                project,
                stream,
                Caller {
                    kind: CallerKind::Tui,
                    agent_id: None,
                    pid: None,
                },
                SessionMode::Normal,
            )
            .await
        })
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    rt: tokio::runtime::Runtime,
    daemon: Arc<FakeDaemon>,
    backend: Option<DaemonBackend>,
    stop: Arc<AtomicBool>,
}

impl Fixture {
    fn launch() -> Fixture {
        Fixture::launch_with(FakeDaemon::new())
    }

    fn launch_with(daemon: Arc<FakeDaemon>) -> Fixture {
        let dir = tempfile::tempdir().expect("create isolated fake project");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir.path().join(".orchestry"))
            .expect("create fake project marker");
        let root = dir.path().to_path_buf();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("create fake daemon runtime");
        let connector = connector(&daemon, &root);
        let stop = Arc::new(AtomicBool::new(false));
        let backend = rt
            .block_on(DaemonBackend::launch(
                connector,
                "test",
                PathBuf::from(&root),
                Arc::clone(&stop),
            ))
            .expect("launch");
        Fixture {
            _dir: dir,
            rt,
            daemon,
            backend: Some(backend),
            stop,
        }
    }

    fn backend(&mut self) -> &mut DaemonBackend {
        self.backend
            .as_mut()
            .expect("fixture backend lives until drop")
    }

    /// Poll `f` against the backend until it yields, or fail after 5 s.
    fn wait_for<T>(&mut self, what: &str, mut f: impl FnMut(&mut DaemonBackend) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(value) = f(self.backend()) {
                return value;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until the daemon has seen `n` subscriptions.
    fn wait_subscriptions(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.daemon.subscriptions() < n {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for subscription {n}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The backend's tasks run on this runtime: drop it first.
        drop(self.backend.take());
        let _ = &self.rt;
    }
}

const EVERY_STORE: [Store; 10] = [
    Store::Tasks,
    Store::Goals,
    Store::Agents,
    Store::Messages,
    Store::Context,
    Store::Teams,
    Store::Runs,
    Store::Config,
    Store::State,
    Store::Remote,
];

#[test]
fn tui_subscribes_to_every_store_and_the_bus_before_its_first_read() {
    let fixture = Fixture::launch();
    let log = fixture.daemon.log();
    let subscribe = log.iter().position(|op| op == "subscribe");
    let bootstrap = log.iter().position(|op| op == "ui.bootstrap");
    assert!(
        matches!((subscribe, bootstrap), (Some(s), Some(b)) if s < b),
        "subscribe must precede the bootstrap: {log:?}"
    );
    assert_eq!(log[0], "subscribe", "nothing is read before it: {log:?}");
    let args = fixture
        .daemon
        .subscribes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)[0]
        .clone();
    let mut stores: Vec<_> = args.stores.iter().copied().collect();
    stores.sort_by_key(|s| format!("{s:?}"));
    let mut every = EVERY_STORE.to_vec();
    every.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(stores, every);
    assert!(args.bus, "the bus rides on the same observation");
    assert!(args.filter.is_none(), "the dashboard shows every entity");
}

#[test]
fn tui_one_observation_feed_carries_changes_and_bus_and_no_logs_follow_opens() {
    let mut fixture = Fixture::launch();
    fixture.wait_subscriptions(1);
    fixture.daemon.script(
        0,
        Script::Event {
            seq: 1,
            body: change(Store::Tasks, Some("tsk_2"), 101),
        },
    );
    fixture.daemon.script(
        0,
        Script::Event {
            seq: 2,
            body: EventBody::Bus(Box::new(OrchestratorEvent::TaskAssigned {
                task_id: "tsk_2".into(),
                agent_id: "agt_1".into(),
            })),
        },
    );
    let observed = fixture.wait_for("the change", |b| {
        let got = b.poll_observations();
        (!got.is_empty()).then_some(got)
    });
    assert_eq!(
        observed,
        vec![Observation::Changed {
            store: ObservedStore::Tasks,
            id: Some("tsk_2".into()),
            commit_seq: 101,
        }]
    );
    let events = fixture.wait_for("the bus event", |b| {
        let got = b.poll_events();
        (!got.is_empty()).then_some(got)
    });
    assert_eq!(events.len(), 1);
    assert!(
        !fixture.daemon.log().iter().any(|op| op == "logs.follow"),
        "the bus replaces the logs.follow session: {:?}",
        fixture.daemon.log()
    );
}

#[test]
fn tui_a_change_committed_between_subscribe_and_the_bootstrap_is_not_lost() {
    let daemon = FakeDaemon::new();
    *daemon
        .push_before_bootstrap
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(change(Store::Tasks, Some("tsk_9"), 101));
    let mut fixture = Fixture::launch_with(daemon);
    let observed = fixture.wait_for("the early change", |b| {
        let got = b.poll_observations();
        (!got.is_empty()).then_some(got)
    });
    assert!(
        observed.iter().any(|o| matches!(
            o,
            Observation::Changed {
                store: ObservedStore::Tasks,
                commit_seq: 101,
                ..
            }
        )),
        "{observed:?}"
    );
}

#[test]
fn tui_reads_report_the_commit_ordinal_they_observed() {
    let mut fixture = Fixture::launch();
    fixture.daemon.commit_seq.store(250, Ordering::SeqCst);
    fixture.backend().refresh_tasks().unwrap();
    assert_eq!(fixture.backend().last_read_seq(), Some(250));
}

#[test]
fn tui_reads_the_dispatch_switch_from_daemon_status() {
    let mut fixture = Fixture::launch();
    fixture.daemon.dispatch_on.store(true, Ordering::SeqCst);
    assert_eq!(fixture.backend().refresh_dispatch(), Ok(true));
}

#[test]
fn tui_a_gap_resubscribes_then_reports_resynced_without_replaying_a_mutation() {
    let mut fixture = Fixture::launch();
    fixture.wait_subscriptions(1);
    fixture
        .backend()
        .create_task("Once", Default::default())
        .unwrap();
    fixture.daemon.script(
        0,
        Script::Event {
            seq: 1,
            body: change(Store::Tasks, None, 102),
        },
    );
    // seq 2 is missing: a gap.
    fixture.daemon.script(
        0,
        Script::Event {
            seq: 3,
            body: change(Store::Goals, None, 103),
        },
    );
    let mut seen = Vec::new();
    fixture.wait_for("the resync", |b| {
        seen.extend(b.poll_observations());
        seen.contains(&Observation::Resynced).then_some(())
    });
    // The new subscription exists before the dashboard is told to refetch.
    assert_eq!(fixture.daemon.subscriptions(), 2);
    let log = fixture.daemon.log();
    assert_eq!(
        log.iter().filter(|op| *op == "subscribe").count(),
        2,
        "{log:?}"
    );
    assert_eq!(
        log.iter().filter(|op| *op == "task.create").count(),
        1,
        "{log:?}"
    );
    assert!(fixture.backend().session_loss().failure().is_none());
    // The new subscription delivers like the first.
    fixture.daemon.script(
        1,
        Script::Event {
            seq: 1,
            body: change(Store::Teams, None, 104),
        },
    );
    fixture.wait_for("a change on the new subscription", |b| {
        b.poll_observations()
            .into_iter()
            .any(|o| {
                matches!(
                    o,
                    Observation::Changed {
                        store: ObservedStore::Teams,
                        ..
                    }
                )
            })
            .then_some(())
    });
}

#[test]
fn tui_a_server_resync_resubscribes_then_reports_resynced() {
    for reason in [ResyncReason::Lagged, ResyncReason::StoreSetUncertain] {
        let mut fixture = Fixture::launch();
        fixture.wait_subscriptions(1);
        fixture.daemon.script(
            0,
            Script::Event {
                seq: 1,
                body: EventBody::Resync { reason },
            },
        );
        let mut seen = Vec::new();
        fixture.wait_for("the resync", |b| {
            seen.extend(b.poll_observations());
            seen.contains(&Observation::Resynced).then_some(())
        });
        assert_eq!(fixture.daemon.subscriptions(), 2, "{reason:?}");
    }
}

#[test]
fn tui_a_new_daemon_instance_on_resubscribe_is_a_lost_session() {
    let mut fixture = Fixture::launch();
    fixture.wait_subscriptions(1);
    // The daemon was restarted: the next session welcomes another instance.
    *fixture
        .daemon
        .instance
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = "inst-b".into();
    fixture.daemon.script(
        0,
        Script::Event {
            seq: 1,
            body: EventBody::Resync {
                reason: ResyncReason::Lagged,
            },
        },
    );
    let loss = fixture.backend().session_loss();
    fixture.wait_for("the lost session", |_| loss.failure());
    assert!(
        fixture.stop.load(Ordering::SeqCst),
        "the draw loop is told to stop"
    );
    let mut seen = Vec::new();
    seen.extend(fixture.backend().poll_observations());
    assert!(
        !seen.contains(&Observation::Resynced),
        "an old-instance cache is never refreshed in place: {seen:?}"
    );
}

#[test]
fn tui_eof_on_the_observation_is_a_lost_session_and_nothing_reconnects() {
    let mut fixture = Fixture::launch();
    fixture.wait_subscriptions(1);
    let loss = fixture.backend().session_loss();
    // Let the background history load finish so the connect count settles.
    std::thread::sleep(Duration::from_millis(100));
    let connects = fixture.daemon.connects.load(Ordering::SeqCst);
    fixture.daemon.script(0, Script::Close);
    let failure = fixture.wait_for("the lost session", |_| loss.failure());
    assert!(failure.contains("No orch daemon"), "{failure}");
    assert!(fixture.stop.load(Ordering::SeqCst));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        fixture.daemon.connects.load(Ordering::SeqCst),
        connects,
        "a lost observation is not retried behind the user's back"
    );
}

/// A manual driver, not an assertion: with `ORCH_TUI_FAKE_DRIVE=<dir>` set it
/// runs the real `orch tui` loop (dashboard, no-daemon screen, `r`, `q`) on
/// this terminal against the fake daemon, so it can be driven in tmux. Files
/// created in `<dir>` script the daemon: `update` pushes a new task, `outage`
/// takes the daemon down and ends the observation, `up` brings it back.
/// Without the variable it returns at once.
#[test]
fn drive_the_real_dashboard_loop_against_the_fake_daemon() {
    let Some(control) = std::env::var_os("ORCH_TUI_FAKE_DRIVE") else {
        return;
    };
    let control = PathBuf::from(control);
    let dir = tempfile::tempdir().expect("create isolated fake project");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(dir.path().join(".orchestry"))
        .expect("create fake project marker");
    let daemon = FakeDaemon::new();
    let connector = connector(&daemon, dir.path());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let script = Arc::clone(&daemon);
    rt.spawn(async move {
        let mut done = (false, false, false);
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if !done.0 && control.join("update").exists() {
                done.0 = true;
                let seq = script.commit_seq.fetch_add(1, Ordering::SeqCst) + 1;
                let mut pushed = task("tsk_push");
                pushed.title = "Pushed by another client — no poll".into();
                script
                    .tasks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(pushed);
                let subscription = script.subscriptions() - 1;
                script.script(
                    subscription,
                    Script::Event {
                        seq: 1,
                        body: change(Store::Tasks, Some("tsk_push"), seq),
                    },
                );
            }
            if !done.1 && control.join("outage").exists() {
                done.1 = true;
                script.down.store(true, Ordering::SeqCst);
                let subscription = script.subscriptions() - 1;
                script.script(subscription, Script::Close);
            }
            if !done.2 && control.join("up").exists() {
                done.2 = true;
                script.down.store(false, Ordering::SeqCst);
            }
        }
    });
    let exit = rt
        .block_on(super::command::run_dashboard(
            connector,
            dir.path().to_path_buf(),
            "fake",
        ))
        .unwrap();
    eprintln!("orch tui (fake daemon) exited: {exit:?}");
}
