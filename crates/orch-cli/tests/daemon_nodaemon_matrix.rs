//! The no-daemon matrix (T2a; spec R3, R4, R5, R6, R16, R28; design D5/D12
//! `daemon_no_daemon::all_frontends_matrix`; goal row 3).
//!
//! Every frontend kind x every way a daemon can fail to be there:
//!
//! | kind                | channel that carries the uniform message                   |
//! |---------------------|------------------------------------------------------------|
//! | CLI read / CLI write| stderr, exit 7, empty stdout                                |
//! | TUI                 | the full screen; `r` retries; `q` exits 7                  |
//! | web                 | stderr + exit 7 at start (preflight)                        |
//! | MCP initialize      | JSON-RPC error / tool error with `exit_code` 7; bridge alive|
//! | MCP tool call       | same, and the bridge exits 0 on stdin EOF                   |
//!
//! x { no socket, stale socket inode, a live daemon one protocol ahead }.
//!
//! For each cell the proof is the same four things:
//! 1. the uniform text (agent variant when an agent identity is set),
//! 2. the channel's exit/error signal,
//! 3. no process was created: only the client itself may be new while it
//!    runs, nothing new remains after it exits (process table, exe path and
//!    project/socket markers),
//! 4. nothing was repaired or written: the project tree, the socket inode
//!    and the socket directory are identical afterwards (a direct-store
//!    fallback or an auto-start would show here).
//!
//! The cells run one at a time (a process-wide lock) so a before/after
//! process-table comparison is not confused by a sibling case's clients.

mod daemon_support;

use daemon_support::frontends::*;
use daemon_support::*;
use serde_json::json;
use std::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poison| poison.into_inner())
}

// ── one check per kind ─────────────────────────────────────────────────────

fn check_cli(cond: Cond, args: &[&str], env: &[(&str, &str)], agent: bool) {
    let _serial = serial();
    let scene = Scene::new(cond);
    let before = scene.snapshot();
    let watch = Watch::begin();

    let run = run_cli(&scene, args, env);

    let who = format!("cli {args:?}");
    assert_eq!(run.code, Some(7), "{who}: {}", run.stderr);
    assert!(run.stdout.is_empty(), "{who} wrote stdout: {}", run.stdout);
    scene.assert_uniform(&who, &run.stderr, agent);
    watch.assert_only_clients(&who, &[]);
    assert!(
        process_group_empty(run.pid),
        "{who} left a process in its group"
    );
    watch.assert_nothing_left(&who, &scene);
    scene.assert_untouched(&who, &before);
}

fn check_web(cond: Cond) {
    let _serial = serial();
    let scene = Scene::new(cond);
    let before = scene.snapshot();
    let watch = Watch::begin();

    let run = run_web_preflight(&scene);

    assert_eq!(run.code, Some(7), "web: {}", run.stderr);
    scene.assert_uniform("web", &run.stderr, false);
    assert!(
        !run.stdout.contains("http://"),
        "web announced a URL before the preflight: {}",
        run.stdout
    );
    watch.assert_only_clients("web", &[]);
    assert!(process_group_empty(run.pid));
    watch.assert_nothing_left("web", &scene);
    scene.assert_untouched("web", &before);
}

fn check_mcp_initialize(cond: Cond, env: &[(&str, &str)], agent: bool) {
    let _serial = serial();
    let scene = Scene::new(cond);
    let before = scene.snapshot();
    let watch = Watch::begin();
    let mut cmd = scene.orch();
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut mcp = Mcp::spawn(cmd, &[]);
    let pid = mcp.pid;

    let response = mcp.initialize();

    let failure = mcp_failure("mcp initialize", &response);
    scene.assert_uniform("mcp initialize", &failure, agent);
    assert_mcp_exit_code_7("mcp initialize", &response);
    assert!(
        mcp.is_running(),
        "the bridge died instead of surfacing the error: {}",
        mcp.stderr()
    );
    watch.assert_only_clients("mcp initialize", &[pid]);
    assert_eq!(mcp.finish(), Some(0), "stderr: {}", mcp.stderr());
    assert!(process_group_empty(pid));
    watch.assert_nothing_left("mcp initialize", &scene);
    scene.assert_untouched("mcp initialize", &before);
}

fn check_mcp_tool_call(cond: Cond, env: &[(&str, &str)], agent: bool) {
    let _serial = serial();
    let scene = Scene::new(cond);
    let before = scene.snapshot();
    let watch = Watch::begin();
    let mut cmd = scene.orch();
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut mcp = Mcp::spawn(cmd, &[]);
    let pid = mcp.pid;

    // A client that ignores a failed initialize and calls on anyway must get
    // the same message, not a hang, a crash or a direct-store answer.
    let _ = mcp.initialize();
    mcp.notify("notifications/initialized", json!({}));
    let response = mcp.call_tool("status", json!({}));

    let failure = mcp_failure("mcp tool call", &response);
    scene.assert_uniform("mcp tool call", &failure, agent);
    assert_mcp_exit_code_7("mcp tool call", &response);
    // A write tool is refused the same way and changes nothing.
    let response = mcp.call_tool("task_add", json!({"title": "must not exist"}));
    let failure = mcp_failure("mcp write tool", &response);
    scene.assert_uniform("mcp write tool", &failure, agent);
    watch.assert_only_clients("mcp tool call", &[pid]);
    assert_eq!(mcp.finish(), Some(0), "stderr: {}", mcp.stderr());
    assert!(process_group_empty(pid));
    watch.assert_nothing_left("mcp tool call", &scene);
    scene.assert_untouched("mcp tool call", &before);
}

fn check_tui(cond: Cond) {
    let _serial = serial();
    let scene = Scene::new(cond);
    let before = scene.snapshot();
    let watch = Watch::begin();

    let pane = TuiPane::launch(&scene, "tui", &[]);
    let screen = pane.wait_screen("the no-daemon screen", |s| {
        s.contains("No orch daemon is running for")
    });
    scene.assert_uniform("tui", &screen, false);
    // While it shows the message the only orch process is the TUI itself.
    let clients = pane.client_pids();
    assert_eq!(clients.len(), 1, "expected exactly the TUI: {clients:?}");
    watch.assert_only_clients("tui", &clients);
    assert!(
        !pane.has_exited(),
        "the TUI exited instead of offering retry"
    );

    // `r` re-tries the connection; with no daemon the message stays and the
    // TUI is still the only orch process.
    pane.keys(&["r"]);
    let screen = pane.wait_screen("the message after a retry", |s| {
        s.contains("No orch daemon is running for")
    });
    scene.assert_uniform("tui after retry", &screen, false);
    assert!(!pane.has_exited(), "retry made the TUI exit");
    watch.assert_only_clients("tui after retry", &clients);

    // `q` quits with the dedicated exit code.
    pane.keys(&["q"]);
    assert_eq!(pane.exit_code(), 7, "screen:\n{}", pane.screen());
    watch.assert_nothing_left("tui", &scene);
    scene.assert_untouched("tui", &before);
}

// ── the cells ──────────────────────────────────────────────────────────────

/// One `#[test]` per cell. Every cell fails at the I1b base (the CLI, TUI,
/// web and MCP still read and write the store directly there) and passes once
/// the cutover slices land; the single `ignore` below is the only line to
/// delete when they have (`rg 'CUTOVER:' crates/orch-cli/tests`).
macro_rules! cells {
    ($( $name:ident => $body:expr; )*) => {
        $(
            #[test]
            #[ignore = "CUTOVER: needs S2a-cli, S2b (MCP) and S2c (TUI/web); fails at the I1b base"]
            fn $name() {
                $body
            }
        )*
    };
}

const READ: &[&str] = &["task", "list"];
const WRITE: &[&str] = &["task", "add", "must not exist"];

cells! {
    cli_read_without_a_socket_exits_7_with_the_uniform_message_and_spawns_nothing
        => check_cli(Cond::NoSocket, READ, &[], false);
    cli_read_over_a_stale_socket_exits_7_with_the_uniform_message_and_repairs_nothing
        => check_cli(Cond::StaleSocket, READ, &[], false);
    cli_read_against_a_daemon_of_another_protocol_exits_7_naming_both_versions
        => check_cli(Cond::Skew, READ, &[], false);

    cli_write_without_a_socket_exits_7_and_writes_no_entity_file
        => check_cli(Cond::NoSocket, WRITE, &[], false);
    cli_write_over_a_stale_socket_exits_7_and_writes_no_entity_file
        => check_cli(Cond::StaleSocket, WRITE, &[], false);
    cli_write_against_a_daemon_of_another_protocol_exits_7_and_writes_no_entity_file
        => check_cli(Cond::Skew, WRITE, &[], false);

    cli_read_in_an_agent_context_gets_the_agent_variant_without_a_start_hint
        => check_cli(Cond::NoSocket, READ, &[("ORCH_TASK_ID", "tsk_agent")], true);
    cli_write_with_an_agent_identity_gets_the_agent_variant_without_a_start_hint
        => check_cli(Cond::StaleSocket, WRITE, &[("ORCH_AGENT_ID", "agt_agent")], true);

    web_preflight_without_a_socket_exits_7_with_the_uniform_message_and_no_listener
        => check_web(Cond::NoSocket);
    web_preflight_over_a_stale_socket_exits_7_with_the_uniform_message
        => check_web(Cond::StaleSocket);
    web_preflight_against_a_daemon_of_another_protocol_exits_7_naming_both_versions
        => check_web(Cond::Skew);

    mcp_initialize_without_a_socket_returns_the_uniform_error_and_the_bridge_stays_up
        => check_mcp_initialize(Cond::NoSocket, &[], false);
    mcp_initialize_over_a_stale_socket_returns_the_uniform_error_and_the_bridge_stays_up
        => check_mcp_initialize(Cond::StaleSocket, &[], false);
    mcp_initialize_against_a_daemon_of_another_protocol_returns_the_skew_error
        => check_mcp_initialize(Cond::Skew, &[], false);
    mcp_initialize_with_an_agent_identity_returns_the_agent_variant
        => check_mcp_initialize(Cond::NoSocket, &[("ORCH_AGENT_ID", "agt_agent")], true);

    mcp_tool_calls_without_a_socket_return_the_uniform_error_and_eof_exits_0
        => check_mcp_tool_call(Cond::NoSocket, &[], false);
    mcp_tool_calls_over_a_stale_socket_return_the_uniform_error_and_eof_exits_0
        => check_mcp_tool_call(Cond::StaleSocket, &[], false);
    mcp_tool_calls_against_a_daemon_of_another_protocol_return_the_skew_error
        => check_mcp_tool_call(Cond::Skew, &[], false);

    tui_without_a_socket_shows_the_message_retries_and_q_exits_7
        => check_tui(Cond::NoSocket);
    tui_over_a_stale_socket_shows_the_message_retries_and_q_exits_7
        => check_tui(Cond::StaleSocket);
    tui_against_a_daemon_of_another_protocol_shows_the_skew_message_and_q_exits_7
        => check_tui(Cond::Skew);
}

// ── beyond the matrix ──────────────────────────────────────────────────────

/// Self-check of the CLI cell's checklist against a client that is already
/// converted (`orch daemon status`): if the harness were blind (wrong exit
/// code, wrong text, a process filter that sees nothing) these would fail
/// today instead of waiting for the cutover.
#[test]
fn cli_checklist_passes_for_the_converted_daemon_status_client_in_every_condition() {
    for cond in [Cond::NoSocket, Cond::StaleSocket, Cond::Skew] {
        check_cli(cond, &["daemon", "status"], &[], false);
    }
    check_cli(
        Cond::NoSocket,
        &["daemon", "status"],
        &[("ORCH_TASK_ID", "tsk_agent")],
        true,
    );
}

/// ...and it rejects a client that succeeds (`init --help` is offline and
/// exits 0), so a vacuous pass is impossible.
#[test]
fn cli_checklist_rejects_a_client_that_answers_without_a_daemon() {
    let verdict = std::panic::catch_unwind(|| {
        check_cli(Cond::NoSocket, &["init", "--help"], &[], false);
    });
    assert!(
        verdict.is_err(),
        "the checklist accepted a client that exited 0"
    );
}

/// `r` in the disconnected TUI is the retry: once the owner starts the daemon
/// the next `r` reaches it and the dashboard replaces the message.
#[test]
#[ignore = "CUTOVER: needs S2c (TUI/web); fails at the I1b base"]
fn tui_retry_reaches_the_daemon_after_the_owner_starts_it() {
    let _serial = serial();
    let scene = Scene::new(Cond::NoSocket);
    let pane = TuiPane::launch(&scene, "tui", &[]);
    pane.wait_screen("the no-daemon screen", |s| {
        s.contains("No orch daemon is running for")
    });

    // The owner starts the daemon (an explicit act, by the test).
    let daemon = start_ready(scene.sandbox(), &[], "owner");
    pane.keys(&["r"]);

    let screen = pane.wait_screen("the dashboard after retry", |s| {
        !s.contains("No orch daemon is running for") && s.contains("ORCH")
    });
    assert!(!pane.has_exited(), "screen:\n{screen}");
    // Only the owner's daemon and the TUI exist: the TUI started nothing.
    let exe = env!("CARGO_BIN_EXE_orch");
    let daemons: Vec<_> = process_table()
        .into_iter()
        .filter(|p| p.command.starts_with(&format!("{exe} daemon start")))
        .filter(|p| p.command.contains("daemon start") && is_alive(p.pid))
        .collect();
    assert!(
        daemons.iter().all(|p| p.pid == daemon.pid),
        "a daemon other than the owner's exists: {daemons:?}"
    );
}

/// Web has no way to retry a closed tab's page, so an outage while serving is
/// an HTTP 503 with the generic text, not a silent hang and not a restart.
#[test]
#[ignore = "CUTOVER: needs S2c (TUI/web); fails at the I1b base"]
fn web_answers_503_when_the_daemon_dies_while_serving_and_starts_nothing() {
    let _serial = serial();
    let scene = Scene::new(Cond::NoSocket);
    let mut daemon = start_ready(scene.sandbox(), &[], "owner");
    let port = free_loopback_port();
    let addr = format!("127.0.0.1:{port}");
    let mut cmd = scene.orch();
    cmd.args(["web", "--no-auth", "--bind", &addr]);
    let web = DaemonChild::spawn(cmd, scene.test_root.join("web.err"));
    let _kill_web = KillOnDrop(web.pid);
    wait_for(READY, "orch web to listen", || {
        http_get(&addr, "/").ok().filter(|(status, _)| *status != 0)
    });
    let watch = Watch::begin();

    daemon.crash();
    let (status, body) = wait_for(READY, "a 503 after the daemon died", || {
        http_get(&addr, "/")
            .ok()
            .filter(|(status, _)| *status == 503)
    });

    assert_eq!(status, 503);
    assert!(
        normalize(&body).contains("orch daemon unavailable")
            || normalize(&body).contains("No orch daemon is running for"),
        "503 body: {body}"
    );
    assert!(
        !normalize(&body).contains(&*scene.socket.to_string_lossy()),
        "the 503 leaked the socket path: {body}"
    );
    // The page's server did not restart anything.
    assert!(watch.new_orch().is_empty(), "{:?}", watch.new_orch());
    assert_eq!(scene.sandbox().daemon_json().unwrap()["pid"], daemon.pid);
    assert!(!is_alive(daemon.pid));
}

/// Positive control: the process detector used above flags a daemon that a
/// client did start, so a green matrix is not an artefact of a blind filter.
#[test]
fn process_watch_flags_a_daemon_started_during_the_window() {
    let _serial = serial();
    let scene = Scene::new(Cond::NoSocket);
    let watch = Watch::begin();
    let daemon = start_ready(scene.sandbox(), &[], "control");
    let new = watch.new_orch();
    assert!(new.iter().any(|p| p.pid == daemon.pid), "{new:?}");
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watch.assert_only_clients("control", &[]);
    }));
    assert!(caught.is_err(), "the watch missed a running daemon");
}
