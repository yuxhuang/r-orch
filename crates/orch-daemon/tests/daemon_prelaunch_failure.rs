#![cfg(feature = "testing")]
//! Board-2 perf F1: a launch that cannot reach spawn must not leave the task
//! dispatchable with no record, or every tick repeats the whole preparation
//! (state.json rewrites, a `git` probe per candidate) and client reads queue
//! behind it on the operation boundary.

mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::{Value, json};
use std::time::Duration;
use support::{DEADLINE, Raw, Reaper, ok, running_pid, task};

/// A shell agent whose launch needs a git worktree. The harness project is
/// not a git repository, so workspace preparation fails before any run, prompt
/// or process exists, exactly like a project whose `.git` went missing.
async fn worktree_agent(raw: &mut Raw, daemon: &DaemonHarness) -> String {
    let reply = raw
        .request(
            1_000_100,
            "agent.create",
            json!({
                "name": "needs-git",
                "adapter": "shell",
                "command": "exit 0",
                "workspace_mode": "worktree",
                "env": [
                    {"name": "HOME", "value": daemon.project_root().to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"},
                ],
            }),
        )
        .await;
    ok(&reply)["agent"]["id"].as_str().unwrap().to_owned()
}

async fn task_record(raw: &mut Raw, id: &str) -> Value {
    let reply = raw.request(1_000_101, "task.get", json!({"id": id})).await;
    ok(&reply).clone()
}

fn retry_queue(daemon: &DaemonHarness) -> Vec<Value> {
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(daemon.project_root().join(".orchestry/state.json")).unwrap(),
    )
    .unwrap();
    state["retry_queue"].as_array().cloned().unwrap_or_default()
}

/// state.json's content and the task file's `(inode, mtime)`. A tick
/// rewrites state.json once even when idle, so its bytes are compared; the
/// task file is only ever replaced by a rename, so its identity is enough.
fn written(daemon: &DaemonHarness, task: &str) -> (String, u64, std::time::SystemTime) {
    use std::os::unix::fs::MetadataExt;
    let orchestry = daemon.project_root().join(".orchestry");
    let meta = std::fs::metadata(orchestry.join("tasks").join(format!("{task}.yml"))).unwrap();
    (
        std::fs::read_to_string(orchestry.join("state.json")).unwrap(),
        meta.ino(),
        meta.modified().unwrap(),
    )
}

/// v0.13 parity (`record_workspace_dispatch_failure`): the failed preparation
/// counts an attempt and queues the retry with its back-off; a tick inside
/// the back-off commits nothing, and a read issued while a tick runs is not
/// starved by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_failure_before_spawn_counts_an_attempt_and_backs_off() {
    let clock = support::TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = worktree_agent(&mut raw, &daemon).await;
    let id = task(&mut raw, "needs a worktree", json!({"assignee": agent})).await;

    let first = daemon.step_dispatch().await.unwrap();
    // `dispatch.tick` reports tasks whose attempt count rose, as it does for
    // an adapter preparation failure: the attempt was made and recorded.
    assert_eq!(first.launched, vec![id.clone()]);
    assert!(
        running_pid(&daemon, &id).await.is_none(),
        "nothing may run after a failed workspace"
    );
    let record = task_record(&mut raw, &id).await;
    assert_eq!(
        record["attempts"], 1,
        "the failed launch was not counted: {record}"
    );
    assert_eq!(record["status"], "retrying", "{record}");
    let queue = retry_queue(&daemon);
    assert_eq!(queue.len(), 1, "no back-off was queued: {queue:?}");
    assert_eq!(queue[0]["task_id"], json!(id));
    assert_eq!(
        queue[0]["due_at"],
        json!(orch_core::clock::iso_from_epoch_ms(
            support::TestClock::START_MS + 10_000
        )),
        "the first retry waits retry_base_delay_ms"
    );
    assert!(
        queue[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("git repository")),
        "{queue:?}"
    );

    // Inside the back-off: the tick neither prepares nor writes. Reads issued
    // meanwhile complete promptly (the bound only fires on a starved read).
    let before = written(&daemon, &id);
    let mut reader = Raw::session(&daemon, "cli", "normal").await;
    let (tick, read) = tokio::join!(daemon.step_dispatch(), async {
        let started = std::time::Instant::now();
        let reply = reader
            .request(1_000_102, "task.get", json!({"id": id}))
            .await;
        (started.elapsed(), reply)
    });
    assert!(tick.unwrap().launched.is_empty());
    assert_eq!(ok(&read.1)["attempts"], 1, "{}", read.1);
    assert!(
        read.0 < Duration::from_secs(2),
        "task.get took {:?} behind a back-off tick",
        read.0
    );
    assert_eq!(
        written(&daemon, &id),
        before,
        "a tick inside the back-off changed state.json or rewrote the task"
    );
    assert_eq!(task_record(&mut raw, &id).await["attempts"], 1);

    // Once due, the retry runs again and counts again.
    clock.advance_ms(10_001);
    daemon.step_dispatch().await.unwrap();
    let record = task_record(&mut raw, &id).await;
    assert_eq!(record["attempts"], 2, "{record}");
    assert_eq!(record["status"], "retrying", "{record}");
    let queue = retry_queue(&daemon);
    assert_eq!(queue.len(), 1, "{queue:?}");
    assert_eq!(
        queue[0]["due_at"],
        json!(orch_core::clock::iso_from_epoch_ms(
            support::TestClock::START_MS + 10_001 + 20_000
        )),
        "the back-off grows with the attempt"
    );
    daemon.shutdown().await.unwrap();
}

/// The fixture-t6 trigger: an agent file changed without its index entry
/// (hand edit, or a crash between the two writes). Selection reads the index,
/// the pre-spawn re-validation reads the file; comparing the two invalidated
/// every preparation, so nothing ever launched and every tick rewrote
/// state.json twice per candidate. The launch must use the agent file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_file_newer_than_its_index_entry_still_launches() {
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = support::shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut raw, "stale index", json!({"assignee": agent})).await;

    let file = daemon
        .project_root()
        .join(".orchestry/agents")
        .join(format!("{agent}.yml"));
    let yaml = std::fs::read_to_string(&file).unwrap();
    assert!(!yaml.contains("\nrole:"), "{yaml}");
    let edited = yaml.replacen(
        "\nadapter: shell\n",
        "\nadapter: shell\nrole: Edited by hand\n",
        1,
    );
    assert_ne!(edited, yaml, "{yaml}");
    std::fs::write(&file, edited).unwrap();

    let tick = tokio::time::timeout(DEADLINE, daemon.step_dispatch())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tick.launched,
        vec![id.clone()],
        "the stale index blocked the launch"
    );
    reaper.watch(running_pid(&daemon, &id).await.unwrap());
    daemon.shutdown().await.unwrap();
}

/// Board-2b perf F1b: every boundary operation a failed candidate takes is
/// one more turn client reads queue behind. After its selection, a workspace
/// failure records the attempt and releases the preparation in a single
/// commit, and the error path does not take the boundary again for a release
/// that already happened. The difference between a one- and a two-candidate
/// tick is the per-candidate cost, independent of the tick's own operations.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_failure_commits_once_after_its_selection() {
    let daemon = DaemonHarness::builder()
        .clock(support::TestClock::new())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = worktree_agent(&mut raw, &daemon).await;
    let boundary = daemon.engine().operation_boundary();

    let first = task(&mut raw, "one candidate", json!({"assignee": agent})).await;
    let before = boundary.current_commit_seq();
    assert_eq!(
        daemon.step_dispatch().await.unwrap().launched,
        vec![first.clone()]
    );
    let one = boundary.current_commit_seq() - before;

    // Out of the next tick, so it holds exactly two fresh candidates.
    let cancelled = raw
        .request(1_000_103, "task.cancel", json!({"id": first}))
        .await;
    ok(&cancelled);
    let second = task(&mut raw, "second candidate", json!({"assignee": agent})).await;
    let third = task(&mut raw, "third candidate", json!({"assignee": agent})).await;
    let before = boundary.current_commit_seq();
    let mut launched = daemon.step_dispatch().await.unwrap().launched;
    launched.sort();
    let mut expected = vec![second.clone(), third.clone()];
    expected.sort();
    assert_eq!(launched, expected);
    let two = boundary.current_commit_seq() - before;

    assert_eq!(
        two - one,
        2,
        "a failed candidate must cost its selection plus one recording commit \
         (one tick: {one}, two: {two})"
    );
    for id in [&second, &third] {
        let record = task_record(&mut raw, id).await;
        assert_eq!(record["attempts"], 1, "{record}");
        assert_eq!(record["status"], "retrying", "{record}");
    }
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(daemon.project_root().join(".orchestry/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        state["claimed"],
        json!([]),
        "a released claim survived: {state}"
    );
    assert_eq!(retry_queue(&daemon).len(), 2, "{state}");
    daemon.shutdown().await.unwrap();
}

/// The recording commit lists the other tasks only for a final attempt; that
/// list must still feed the cascade, or a dependent stays blocked on a task
/// that can never finish.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_final_workspace_failure_fails_its_dependents() {
    let daemon = DaemonHarness::builder()
        .clock(support::TestClock::new())
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = worktree_agent(&mut raw, &daemon).await;
    let last = task(
        &mut raw,
        "last attempt",
        json!({"assignee": agent, "max_attempts": 1}),
    )
    .await;
    let dependent = task(&mut raw, "dependent", json!({"depends_on": [last]})).await;

    assert_eq!(
        daemon.step_dispatch().await.unwrap().launched,
        vec![last.clone()]
    );
    let record = task_record(&mut raw, &last).await;
    assert_eq!(record["status"], "failed", "{record}");
    assert_eq!(record["attempts"], 1, "{record}");
    assert_eq!(
        support::task_status(&mut raw, &dependent).await,
        "failed",
        "the final workspace failure did not cascade"
    );
    assert!(retry_queue(&daemon).is_empty());
    daemon.shutdown().await.unwrap();
}

/// Selection skips the task list when neither a dependency nor a scope
/// reservation can decide; a running task's scope must still hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_scope_still_blocks_an_overlapping_candidate() {
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .start()
        .await
        .unwrap();
    let reaper = Reaper::new(&daemon);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let holder = support::shell_agent(&mut raw, &daemon, "holder", "exec /bin/sleep 120").await;
    let other = support::shell_agent(&mut raw, &daemon, "other", "exec /bin/sleep 120").await;
    let running = task(
        &mut raw,
        "holds src",
        json!({"assignee": holder, "scope": ["src/**"]}),
    )
    .await;
    let tick = tokio::time::timeout(DEADLINE, daemon.step_dispatch())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tick.launched, vec![running.clone()]);
    reaper.watch(running_pid(&daemon, &running).await.unwrap());

    let overlapping = task(
        &mut raw,
        "inside src",
        json!({"assignee": other, "scope": ["src/engine/**"]}),
    )
    .await;
    let tick = tokio::time::timeout(DEADLINE, daemon.step_dispatch())
        .await
        .unwrap()
        .unwrap();
    assert!(tick.launched.is_empty(), "{:?}", tick.launched);
    assert_eq!(support::task_status(&mut raw, &overlapping).await, "todo");

    let disjoint = task(
        &mut raw,
        "outside src",
        json!({"assignee": other, "scope": ["docs/**"]}),
    )
    .await;
    let tick = tokio::time::timeout(DEADLINE, daemon.step_dispatch())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tick.launched, vec![disjoint.clone()]);
    reaper.watch(running_pid(&daemon, &disjoint).await.unwrap());
    assert_eq!(support::task_status(&mut raw, &overlapping).await, "todo");
    daemon.shutdown().await.unwrap();
}
