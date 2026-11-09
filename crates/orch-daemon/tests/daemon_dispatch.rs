#![cfg(feature = "testing")]
//! The dispatch command mapping (design D8) observed through the socket (T1;
//! `daemon_dispatch::mapping_and_disconnect`, `serve_once_two_pane_sequence`;
//! spec R5, R16–R18). Only the rows reachable in PR 1 exist here: the commands
//! themselves are remapped in PR 2.

#[macro_use]
mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use support::{
    Raw, Reaper, dispatch_state, err_code, ok, quick_agent, running_pid, shell_agent, task,
    task_status,
};

// -- dispatch.set ---------------------------------------------------------

/// A fresh daemon starts OFF; `dispatch.set` flips only the switch and the
/// state it reports is the state the status shows (TUI start/stop watch).
#[tokio::test]
async fn dispatch_set_toggles_the_switch_and_status_agrees() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut raw = Raw::session(&daemon, "tui", "normal").await;
    assert_eq!(dispatch_state(&mut raw).await, (false, false), "starts OFF");
    for (want, word) in [
        (true, "on"),
        (false, "off"),
        (false, "off"),
        (true, "on"),
        (true, "on"),
    ] {
        let reply = raw
            .request(1, "dispatch.set", json!({"dispatch": word}))
            .await;
        assert_eq!(ok(&reply)["dispatch_on"], want, "{reply}");
        assert_eq!(dispatch_state(&mut raw).await, (want, false));
    }
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_daemon_started_with_dispatch_on_says_so() {
    let daemon = DaemonHarness::builder()
        .dispatch(orch_daemon::DispatchMode::On)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    assert_eq!(dispatch_state(&mut raw).await, (true, false));
    daemon.shutdown().await.unwrap();
}

// -- tick (`run --once`) --------------------------------------------------

/// A tick is a deliberate execution request: it runs whether the switch is
/// ON or OFF, never changes the switch, and launches each task once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_runs_one_scheduling_pass_without_touching_the_switch() {
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut raw, "queued", json!({"assignee": agent})).await;
    assert_eq!(dispatch_state(&mut raw).await, (false, false));

    let first = raw.request(1, "dispatch.tick", json!({})).await;
    assert_eq!(ok(&first)["launched"], json!([id]), "{first}");
    reaper.watch(running_pid(&daemon, &id).await.unwrap());
    // Same state, same tick: the running task is not launched twice.
    let second = raw.request(2, "dispatch.tick", json!({})).await;
    assert_eq!(ok(&second)["launched"], json!([]), "{second}");
    assert_eq!(
        dispatch_state(&mut raw).await,
        (false, false),
        "a tick changed the switch"
    );
    daemon.shutdown().await.unwrap();
}

// -- run <id> / run --all -------------------------------------------------

/// `run <id>`: launch one task and observe it to a terminal state; the result
/// carries the exit status the CLI reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_task_observes_one_task_to_its_terminal_state() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let good = quick_agent(&mut raw, &daemon, "good", 0).await;
    let bad = quick_agent(&mut raw, &daemon, "bad", 3).await;
    let ok_task = task(&mut raw, "passes", json!({"assignee": good})).await;
    let bad_task = task(
        &mut raw,
        "fails",
        json!({"assignee": bad, "max_attempts": 1}),
    )
    .await;

    let reply = raw
        .request(1, "dispatch.run_task", json!({"task_id": ok_task}))
        .await;
    assert_eq!(ok(&reply)["exit_code"], 0, "{reply}");
    // `manual` approval parks a finished task in review, not done.
    assert_eq!(ok(&reply)["tasks"][0]["status"], "review", "{reply}");

    let reply = raw
        .request(2, "dispatch.run_task", json!({"task_id": bad_task}))
        .await;
    assert_eq!(ok(&reply)["exit_code"], 1, "{reply}");
    assert_eq!(ok(&reply)["tasks"][0]["status"], "failed", "{reply}");

    let missing = raw
        .request(3, "dispatch.run_task", json!({"task_id": "tsk_missing"}))
        .await;
    assert_eq!(err_code(&missing), "not_found", "{missing}");
    assert_eq!(missing["err"]["exit_code"], 1, "{missing}");
    daemon.shutdown().await.unwrap();
}

/// `run --all`: a finite batch over the currently eligible work, through the
/// same engine (no second orchestrator).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_all_runs_every_currently_eligible_task_and_reports_once() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let a = quick_agent(&mut raw, &daemon, "a", 0).await;
    let b = quick_agent(&mut raw, &daemon, "b", 0).await;
    let t1 = task(&mut raw, "one", json!({"assignee": a})).await;
    let t2 = task(&mut raw, "two", json!({"assignee": b})).await;

    let reply = raw.request(1, "dispatch.run_all", json!({})).await;
    assert_eq!(ok(&reply)["exit_code"], 0, "{reply}");
    let mut ids: Vec<&str> = ok(&reply)["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    ids.sort();
    let mut want = [t1.as_str(), t2.as_str()];
    want.sort();
    assert_eq!(ids, want, "{reply}");
    assert_eq!(
        dispatch_state(&mut raw).await,
        (false, false),
        "a finite run left a batch or switch behind"
    );
    daemon.shutdown().await.unwrap();
}

/// Losing the observer never kills the run: closing the connection that
/// asked for `run <id>` leaves the child running and the task `in_progress`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_disconnect_leaves_the_run_running() {
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut owner, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut owner, "long", json!({"assignee": agent})).await;

    {
        let mut observer = Raw::session(&daemon, "cli", "normal").await;
        observer
            .send(&json!({"id": 1, "req": {"op": "dispatch.run_task", "args": {"task_id": id}}}))
            .await;
        eventually!(
            "the task to start",
            running_pid(&daemon, &id).await.is_some()
        );
        // Observer vanishes with its request still outstanding.
    }
    let pid = running_pid(&daemon, &id).await.expect("still registered");
    reaper.watch(pid);
    // Wait for the daemon to notice the disconnect (session count back to 1),
    // then require the run to have survived it.
    eventually!("the observer session to be reaped", {
        let status = owner.request(9, "daemon.status", json!({})).await;
        ok(&status)["sessions"] == 1
    });
    assert!(reaper.alive(pid), "observer disconnect killed the agent");
    assert_eq!(task_status(&mut owner, &id).await, "in_progress");

    // And the run is still cancellable by someone else.
    let reply = owner.request(10, "task.cancel", json!({"id": id})).await;
    assert_eq!(ok(&reply)["status"], "cancelled", "{reply}");
    assert!(!reaper.alive(pid));
    daemon.shutdown().await.unwrap();
}

/// OFF inhibits automatic scheduling only: cancel, expiry and status stay
/// fully alive (R5), including for a child launched while ON.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn off_keeps_cancel_expiry_and_status_alive() {
    let clock = support::TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .start()
        .await
        .unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut raw, "long", json!({"assignee": agent})).await;
    ok(&raw
        .request(1, "dispatch.set", json!({"dispatch": "on"}))
        .await);
    ok(&raw.request(2, "dispatch.tick", json!({})).await);
    let pid = running_pid(&daemon, &id).await.unwrap();
    reaper.watch(pid);
    ok(&raw
        .request(3, "dispatch.set", json!({"dispatch": "off"}))
        .await);
    assert!(
        reaper.alive(pid),
        "turning dispatch off must not kill a running agent"
    );
    assert_eq!(dispatch_state(&mut raw).await, (false, false));

    ok(&raw
        .request(
            4,
            "context.set",
            json!({"key": "k", "value": "v", "ttl_ms": 10}),
        )
        .await);
    clock.advance_ms(1_000);
    // The daemon's own 1 s maintenance timer may have removed the key already;
    // either way, with dispatch OFF the key must end up expired and gone.
    let expired = raw
        .request(5, "maintenance.expire_context", json!({}))
        .await;
    assert!(ok(&expired)["removed"].as_u64().unwrap() <= 1, "{expired}");
    let listed = raw.request(7, "context.list", json!({})).await;
    assert_eq!(ok(&listed), &json!([]), "expiry did not run while OFF");

    let cancelled = raw.request(6, "task.cancel", json!({"id": id})).await;
    assert_eq!(ok(&cancelled)["status"], "cancelled", "{cancelled}");
    assert!(!reaper.alive(pid));
    daemon.shutdown().await.unwrap();
}

// -- serve --once (batch) -------------------------------------------------

/// Two panes, one daemon: ON rejects a batch; OFF admits it and a second
/// batch conflicts; a later ON by the other pane ends the batch as
/// `interrupted` without the finishing batch ever restoring OFF; the observer's
/// own disconnect afterwards changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_once_two_pane_sequence() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut pane_a = Raw::session(&daemon, "cli", "normal").await;
    let mut pane_b = Raw::session(&daemon, "cli", "normal").await;

    // Initial ON: refused as an invalid argument with the documented text.
    ok(&pane_a
        .request(1, "dispatch.set", json!({"dispatch": "on"}))
        .await);
    let refused = pane_a
        .request(2, "dispatch.until_terminal", json!({}))
        .await;
    assert_eq!(err_code(&refused), "invalid_arguments", "{refused}");
    assert_eq!(refused["err"]["exit_code"], 2);
    assert!(
        refused["err"]["message"]
            .as_str()
            .unwrap()
            .contains("dispatch is ON; serve --once needs dispatch OFF"),
        "{refused}"
    );
    assert_eq!(
        dispatch_state(&mut pane_b).await,
        (true, false),
        "a refused batch moved the state"
    );

    // OFF with unassignable work: the batch stays pending (all-task wait).
    ok(&pane_a
        .request(3, "dispatch.set", json!({"dispatch": "off"}))
        .await);
    task(&mut pane_b, "blocked: nobody to run it", json!({})).await;
    pane_a
        .send(&json!({"id": 4, "req": {"op": "dispatch.until_terminal", "args": {}}}))
        .await;
    eventually!(
        "the batch to become active",
        dispatch_state(&mut pane_b).await == (false, true)
    );

    // One batch at a time: a second observer gets a conflict (exit 4).
    let second = pane_b
        .request(5, "dispatch.until_terminal", json!({}))
        .await;
    assert_eq!(err_code(&second), "conflict", "{second}");
    assert_eq!(second["err"]["exit_code"], 4, "{second}");

    // The other pane turns dispatch ON: the batch ends, interrupted, and the
    // switch is ON — the batch must not restore OFF over it.
    let on = pane_b
        .request(6, "dispatch.set", json!({"dispatch": "on"}))
        .await;
    assert_eq!(ok(&on)["dispatch_on"], true);
    let finished = pane_a.terminal(4).await;
    assert_eq!(ok(&finished), "interrupted", "{finished}");
    assert_eq!(dispatch_state(&mut pane_b).await, (true, false));

    // The observer going away afterwards changes nothing either.
    drop(pane_a);
    eventually!("the observer to be reaped", {
        let status = pane_b.request(7, "daemon.status", json!({})).await;
        ok(&status)["sessions"] == 1
    });
    assert_eq!(dispatch_state(&mut pane_b).await, (true, false));
    daemon.shutdown().await.unwrap();
}

/// A batch whose observer disconnects ends its scheduling, leaves the switch
/// OFF, and leaves queued work queued.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_observer_disconnect_ends_the_batch_and_keeps_dispatch_off() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut watcher = Raw::session(&daemon, "cli", "normal").await;
    let queued = task(&mut watcher, "still queued afterwards", json!({})).await;
    {
        let mut pane = Raw::session(&daemon, "cli", "normal").await;
        pane.send(&json!({"id": 1, "req": {"op": "dispatch.until_terminal", "args": {}}}))
            .await;
        eventually!(
            "the batch to become active",
            dispatch_state(&mut watcher).await == (false, true)
        );
    }
    eventually!(
        "the batch to end with its observer",
        dispatch_state(&mut watcher).await == (false, false)
    );
    assert_eq!(task_status(&mut watcher, &queued).await, "todo");
    daemon.shutdown().await.unwrap();
}

/// Everything already terminal: the batch has nothing to wait for and reports
/// `all_done` at once, leaving the daemon up and OFF.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_batch_with_no_pending_work_finishes_all_done_and_leaves_the_daemon_running() {
    let daemon = DaemonHarness::start().await.unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let reply = raw.request(1, "dispatch.until_terminal", json!({})).await;
    assert_eq!(ok(&reply), "all_done", "{reply}");
    assert_eq!(dispatch_state(&mut raw).await, (false, false));
    ok(&raw.request(2, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}
