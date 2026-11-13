//! The dashboard is driven by push, not by polling (T3; spec R16/R18, brief
//! D3/D7/§5).
//!
//! Two layers, both against a real daemon and observed from the daemon's side
//! of the wire by a counting proxy (`push_support::CountingProxy`):
//!
//! * in-process: the real `orch-tui` `App` over the real `DaemonBackend`, with
//!   the UI clock under the test's control, so "several former refresh
//!   intervals" pass in microseconds;
//! * binary: the real `orch tui` in tmux, for which time is real.
//!
//! Neither layer greps the source for polling; they count what the frontend
//! actually asks the daemon while nothing changes.

#![cfg(unix)]

mod push_support;
#[path = "tui_support/mod.rs"]
mod tui_support;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use orch_cli::tui::{Connector, DaemonBackend};
use orch_client::ClientConfig;
use orch_core::storage::ConfigStore;
use orch_domain::{CreateTaskInput, WorkspaceMode, default_config};
use orch_infra::storage::{FileConfigStore, Paths};
use orch_proto::handshake::{Caller, CallerKind, SessionMode};
use orch_tui::clock::{Clock as _, TestClock};
use orch_tui::state::ViewId;
use orch_tui::{App, views};
use push_support::{CountingProxy, Pane, Project, STATE_READS, Seen, state_reads};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tui_support::TestDaemon;

/// The refresh interval the dashboard used to poll at (`WATCH_POLL_MS`).
const FORMER_REFRESH_MS: i64 = 5_000;
/// How far the UI clock may move, after an external change, before the change
/// is on screen: well under one former refresh interval.
const AT_ONCE_MS: i64 = 1_000;
const NEW_TITLE: &str = "Pushed-by-another-client";

async fn project(name: &str) -> (tempfile::TempDir, Arc<TestDaemon>) {
    let temp = tempfile::tempdir().expect("tempdir");
    orch_infra::storage::fs_utils::clear_ensured_dirs();
    let paths = Paths::new(temp.path());
    tui_support::create_state_dir(temp.path());
    let mut config = default_config();
    config.project.name = name.to_owned();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    config.scheduling.poll_interval_ms = 100_000;
    ConfigStore::write(&FileConfigStore::new(paths.clone()), &config)
        .await
        .expect("write config.yml");
    let daemon = Arc::new(TestDaemon::start_default(temp.path()).await);
    (temp, daemon)
}

/// `daemon`'s client config as the dashboard dials it, but through `proxy`.
fn via(proxy: &CountingProxy, daemon: &TestDaemon) -> ClientConfig {
    let mut config = daemon.client_config();
    config.socket = Some(proxy.socket().to_path_buf());
    config.test_socket_root = Some(proxy.dir().to_path_buf());
    config.caller = Caller {
        kind: CallerKind::Tui,
        agent_id: None,
        pid: None,
    };
    config.mode = SessionMode::Normal;
    config
}

fn frame(app: &App<DaemonBackend, TestClock>, terminal: &mut Terminal<TestBackend>) -> String {
    terminal.draw(|f| _ = views::draw(f, app)).expect("draw");
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

/// A dashboard on its own thread (not driving the runtime, as
/// `orch_tui::run`'s is), reporting back through channels.
struct Dashboard {
    /// Everything the dashboard sent while its clock crossed six former
    /// refresh intervals with nothing changing.
    quiet: Vec<Seen>,
    /// Tell the dashboard the external change has been made.
    changed: Sender<()>,
    /// `(was the change shown, UI-clock ms that passed meanwhile)`.
    shown: Receiver<(bool, i64)>,
}

async fn start_dashboard(backend: DaemonBackend, proxy: Arc<CountingProxy>) -> Dashboard {
    let (quiet_tx, quiet_rx) = channel::<Vec<Seen>>();
    let (changed_tx, changed_rx) = channel::<()>();
    let (shown_tx, shown_rx) = channel::<(bool, i64)>();
    std::thread::spawn(move || {
        let clock = TestClock::new(1_000_000);
        let mut app = App::new(backend, clock.clone());
        app.state.width = 100;
        app.state.height = 30;
        app.state.view = ViewId::Tasks;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        frame(&app, &mut terminal);
        // Start-up: the first ticks (catalog load, subscription) happen here.
        for _ in 0..3 {
            app.tick();
        }
        let mark = proxy.mark();
        for _ in 0..6 {
            clock.advance_ms(FORMER_REFRESH_MS);
            app.tick();
        }
        quiet_tx.send(proxy.ops_since(mark)).ok();

        if changed_rx.recv().is_err() {
            return;
        }
        let started = clock.now_ms();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut shown = false;
        while Instant::now() < deadline {
            app.tick();
            if frame(&app, &mut terminal).contains(NEW_TITLE) {
                shown = true;
                break;
            }
            // Only a debounce the dashboard itself armed may move its clock;
            // real time passes while the push travels.
            if let Some(at) = app.state.refresh_at {
                clock.set_ms(at);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        shown_tx.send((shown, clock.now_ms() - started)).ok();
        // Keep the app, and so its connection, alive until the test is done.
        let _ = changed_rx.recv();
    });
    let quiet = tokio::task::spawn_blocking(move || quiet_rx.recv().expect("the quiet period"))
        .await
        .expect("join");
    Dashboard {
        quiet,
        changed: changed_tx,
        shown: shown_rx,
    }
}

/// Rename the seeded task through a second client: a change that raises no
/// bus event, so only a pushed invalidation (or a poll) can reveal it.
async fn rename_elsewhere(daemon: &TestDaemon) {
    let mut other = orch_client::Client::connect(&daemon.client_config())
        .await
        .expect("another client");
    let tasks = other
        .request(
            serde_json::from_value::<orch_proto::ops::args::TaskListArgs>(serde_json::json!({}))
                .expect("list args"),
        )
        .await
        .expect("list tasks");
    other
        .request(
            serde_json::from_value::<orch_proto::ops::args::TaskUpdateFieldsArgs>(
                serde_json::json!({"id": tasks[0].id, "title": NEW_TITLE}),
            )
            .expect("update args"),
        )
        .await
        .expect("rename the task");
}

async fn launched(daemon: &Arc<TestDaemon>, proxy: &CountingProxy) -> DaemonBackend {
    DaemonBackend::launch(
        Connector::from_config(via(proxy, daemon), CallerKind::Tui),
        "9.9.9",
        daemon.root().to_path_buf(),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .expect("launch the dashboard backend")
}

/// The spec target (R16). With the daemon quiescent the UI clock crosses six
/// former refresh intervals and the dashboard asks the daemon for nothing; it
/// holds a subscription instead; and a change made by another client reaches
/// the screen without the clock getting anywhere near a refresh interval.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn push_updates_without_disk_observer() {
    let (_temp, daemon) = project("push").await;
    daemon
        .task_service
        .create(CreateTaskInput {
            title: "00000000".into(),
            ..Default::default()
        })
        .await
        .expect("seed a task");
    let proxy = Arc::new(CountingProxy::start(
        daemon.harness().socket().expect("socket"),
    ));
    let backend = launched(&daemon, &proxy).await;
    let dashboard = start_dashboard(backend, Arc::clone(&proxy)).await;

    let mut problems = Vec::new();
    let reads = state_reads(&dashboard.quiet);
    if !reads.is_empty() {
        problems.push(format!(
            "{} unsolicited state reads while the daemon was quiescent across six \
             {FORMER_REFRESH_MS} ms intervals of UI time: {reads:?}",
            reads.len()
        ));
    }
    if !proxy.all_ops().iter().any(|seen| seen.op == "subscribe") {
        problems.push("the dashboard never subscribed to change notifications".to_owned());
    }

    rename_elsewhere(&daemon).await;
    dashboard.changed.send(()).expect("dashboard alive");
    let shown = dashboard.shown;
    let (appeared, advanced) = tokio::task::spawn_blocking(move || shown.recv())
        .await
        .expect("join")
        .expect("the dashboard reports");
    if !appeared {
        problems.push(format!(
            "an external change did not reach the screen within 10 s of real time \
             (UI clock moved {advanced} ms)"
        ));
    } else if advanced > AT_ONCE_MS {
        problems.push(format!(
            "an external change needed {advanced} ms of UI time to show; it must not \
             wait for a refresh interval ({FORMER_REFRESH_MS} ms)"
        ));
    }
    drop(dashboard.changed);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The subscription is established before the first state read (register
/// before reading, brief D3): no change can fall between a view and the
/// subscription that keeps it current.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dashboard_subscribes_before_its_first_state_read() {
    let (_temp, daemon) = project("order").await;
    let proxy = Arc::new(CountingProxy::start(
        daemon.harness().socket().expect("socket"),
    ));
    let backend = launched(&daemon, &proxy).await;
    let dashboard = start_dashboard(backend, Arc::clone(&proxy)).await;
    drop(dashboard.changed);

    let ops = proxy.all_ops();
    let first_subscribe = ops.iter().position(|seen| seen.op == "subscribe");
    let first_read = ops
        .iter()
        .position(|seen| STATE_READS.contains(&seen.op.as_str()));
    let subscribe = first_subscribe.unwrap_or_else(|| panic!("never subscribed; saw {ops:?}"));
    if let Some(read) = first_read {
        assert!(
            subscribe < read,
            "the first state read ({}) came before the subscription: {ops:?}",
            ops[read].op
        );
    }
}

/// The binary on a terminal: `orch tui` is quiet for longer than the old
/// refresh interval of real time, then shows a change made by another client
/// before that interval would have elapsed.
#[test]
fn the_real_dashboard_is_quiet_past_the_old_interval_then_shows_an_external_change() {
    let project = Project::start();
    project.add_task("seed-task-alpha");
    let pane = Pane::launch(&project, "tui", "quiet");
    pane.wait_for("the dashboard", |s| s.contains("ORCH"));
    pane.keys(&["t"]);
    pane.wait_for("the Tasks view with the seeded task", |s| {
        s.contains("seed-task-alpha")
    });

    let mark = project.proxy.mark();
    // 1.5 former refresh intervals of real time, watching the screen only.
    pane.watch_for(Duration::from_millis(7_500));
    let quiet = project.proxy.ops_since(mark);
    let reads = state_reads(&quiet);
    assert!(
        reads.is_empty(),
        "the dashboard polled the daemon {} times in 7.5 s of silence: {reads:?}",
        reads.len()
    );
    assert!(
        project
            .proxy
            .all_ops()
            .iter()
            .any(|seen| seen.op == "subscribe"),
        "the dashboard never subscribed"
    );

    project.add_task("pushed-beta-9921");
    pane.wait_for_within(Duration::from_millis(3_500), "the pushed task", |s| {
        s.contains("pushed-beta-9921")
    });
}

#[test]
fn the_state_read_list_names_real_operations() {
    // A typo in the list would make "zero state reads" vacuously true.
    for op in STATE_READS {
        let frame = serde_json::json!({"id": 1, "req": {"op": op, "args": {}}});
        if let Err(error) = serde_json::from_value::<orch_proto::frame::RequestFrame>(frame) {
            assert!(
                !error.to_string().contains("unknown variant"),
                "{op} is not an operation: {error}"
            );
        }
    }
}
