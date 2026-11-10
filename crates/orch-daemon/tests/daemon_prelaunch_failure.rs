#![cfg(feature = "testing")]
//! Board-2 perf F1: a launch that cannot reach spawn must not leave the task
//! dispatchable with no record, or every tick repeats the whole preparation
//! (state.json rewrites, a `git` probe per candidate) and client reads queue
//! behind it on the operation boundary.

mod support;

use async_trait::async_trait;
use orch_core::process::{ExitNotifier, ExitOutcome, ExitSignal, SpawnOptions, SpawnedChild};
use orch_core::{ProcessManager, Signal};
use orch_daemon::testing::DaemonHarness;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use support::{DEADLINE, Raw, Reaper, ok, running_pid, task};
use tokio::sync::{Notify, oneshot};

/// A shell agent whose launch needs a git worktree. The harness project is
/// not a git repository, so workspace preparation fails before any run, prompt
/// or process exists, exactly like a project whose `.git` went missing.
async fn worktree_agent(raw: &mut Raw, daemon: &DaemonHarness) -> String {
    workspace_agent(raw, daemon, "worktree").await
}

async fn workspace_agent(raw: &mut Raw, daemon: &DaemonHarness, mode: &str) -> String {
    let reply = raw
        .request(
            1_000_100,
            "agent.create",
            json!({
                "name": "needs-git",
                "adapter": "shell",
                "command": "exit 0",
                "workspace_mode": mode,
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

/// The harness project is not a git repository, so every workspace probe
/// (`git rev-parse --is-inside-work-tree`) answers 128, as git does. The test
/// can park one probe: the batch then stops between two of its boundary
/// operations, inside the external preparation step that runs without the
/// boundary, so its progress is counted in operations, not in time.
#[derive(Default)]
struct Probes {
    state: Mutex<ProbeState>,
    entered: Notify,
}

#[derive(Default)]
struct ProbeState {
    /// Probes answered or parked so far, one per candidate.
    count: usize,
    /// The 1-based probe to park when it arrives.
    park: Option<usize>,
    parked: Option<ExitNotifier>,
    unexpected: Vec<String>,
}

const NOT_A_REPOSITORY: ExitOutcome = ExitOutcome {
    code: Some(128),
    signal: None,
};

impl Probes {
    fn park(&self, probe: usize) {
        self.state.lock().unwrap().park = Some(probe);
    }

    /// Wait until probe `probe` is parked.
    async fn parked(&self, probe: usize) {
        tokio::time::timeout(DEADLINE, async {
            loop {
                let notified = self.entered.notified();
                {
                    let state = self.state.lock().unwrap();
                    if state.parked.is_some() && state.count == probe {
                        return;
                    }
                }
                notified.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the batch never reached workspace probe {probe}"));
    }

    /// Answer the parked probe, and park probe `next` instead, if any.
    fn release(&self, next: Option<usize>) {
        let parked = {
            let mut state = self.state.lock().unwrap();
            state.park = next;
            state.parked.take()
        };
        if let Some(parked) = parked {
            parked.notify(NOT_A_REPOSITORY);
        }
    }
}

/// A failed assertion must not leave the batch parked under the daemon's stop.
struct Unpark(Arc<Probes>);

impl Drop for Unpark {
    fn drop(&mut self) {
        self.0.release(None);
    }
}

#[async_trait]
impl ProcessManager for Probes {
    fn is_alive(&self, _: u32) -> bool {
        false
    }
    fn kill(&self, _: u32, _: Signal) {}
    async fn kill_with_grace(&self, _: u32, _: Duration) {}
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        _: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let mut state = self.state.lock().unwrap();
        if command != "git" || args != ["rev-parse", "--is-inside-work-tree"] {
            state
                .unexpected
                .push(format!("{command} {}", args.join(" ")));
            return Err(orch_core::Error::other("only the workspace probe may run"));
        }
        state.count += 1;
        let exit = if state.park == Some(state.count) {
            let (notifier, exit) = ExitSignal::channel();
            state.parked = Some(notifier);
            drop(state);
            self.entered.notify_waiters();
            exit
        } else {
            ExitSignal::resolved(NOT_A_REPOSITORY)
        };
        Ok(SpawnedChild {
            pid: 987_654,
            stdout: None,
            stderr: None,
            stdin: None,
            exit,
        })
    }
}

/// Board-2b perf F1b, amended acceptance A5: a client read queued while a
/// batch of failing workspace preparations runs is served before the batch
/// ends, after a bounded number of the batch's boundary operations, however
/// many candidates the batch has left.
///
/// The bound: a failed candidate takes the boundary twice (selection, then
/// the recording commit) and prepares its workspace between the two without
/// it. The boundary is a FIFO lock, and the batch is one sequential task, so
/// at most one batch operation is ever queued ahead of a read: a read that
/// queues behind the batch is served after at most **one** batch commit. The
/// socket read below cannot be timed to the moment it queues, so for it the
/// test asserts the structural bound of **two** (the operations between two
/// workspace probes; the next probe is parked); the FIFO bound of one is
/// asserted on a waiter whose queueing the test controls. A batch that held
/// the boundary across candidates would serve neither until it ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_queued_during_a_failure_batch_is_served_before_the_batch_ends() {
    const CANDIDATES: usize = 8;
    const PARK: usize = 3;
    const READ: u64 = 1_000_104;
    let probes = Arc::new(Probes::default());
    let _unpark = Unpark(Arc::clone(&probes));
    let daemon = DaemonHarness::builder()
        .clock(support::TestClock::new())
        .maintenance_timer(false)
        .processes(probes.clone())
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = worktree_agent(&mut raw, &daemon).await;
    let mut ids = Vec::new();
    for n in 0..CANDIDATES {
        ids.push(
            task(
                &mut raw,
                &format!("candidate {n}"),
                json!({"assignee": agent}),
            )
            .await,
        );
    }
    let mut reader = Raw::session(&daemon, "cli", "normal").await;
    let boundary = daemon.engine().operation_boundary();

    probes.park(PARK);
    let start = boundary.current_commit_seq();
    let (tick, (at_park, witness, read, at_next_park)) =
        tokio::join!(daemon.step_dispatch(), async {
            probes.parked(PARK).await;
            let at_park = boundary.current_commit_seq();

            // Hold the boundary, so what the batch does next queues on it.
            let (held_tx, held) = oneshot::channel();
            let (release, release_rx) = oneshot::channel::<()>();
            let holder = tokio::spawn({
                let boundary = Arc::clone(&boundary);
                async move {
                    boundary
                        .run(false, async move {
                            let _ = held_tx.send(());
                            let _ = release_rx.await;
                        })
                        .await;
                }
            });
            tokio::time::timeout(DEADLINE, held)
                .await
                .expect(
                    "the boundary stayed taken while the batch prepared a workspace: the batch \
                 holds it across candidates, so no read is served until the batch ends",
                )
                .unwrap();

            reader
                .send(&json!({"id": READ, "req": {"op": "task.list", "args": {}}}))
                .await;
            // The batch records the parked candidate (queued behind the holder),
            // selects the next one and parks again at its probe.
            probes.release(Some(PARK + 1));
            let mut witness = std::pin::pin!(boundary.run(false, async {}));
            std::future::poll_fn(|cx| {
                assert!(
                    witness.as_mut().poll(cx).is_pending(),
                    "the boundary is held"
                );
                Poll::Ready(())
            })
            .await;
            release.send(()).unwrap();
            holder.await.unwrap();
            let witness = tokio::time::timeout(DEADLINE, witness)
                .await
                .expect("a queued boundary waiter starved behind the batch")
                .commit_seq;
            let read = reader.terminal(READ).await;
            probes.parked(PARK + 1).await;
            let at_next_park = boundary.current_commit_seq();
            probes.release(None);
            (at_park, witness, read, at_next_park)
        });

    let mut launched = tick.unwrap().launched;
    launched.sort();
    ids.sort();
    assert_eq!(launched, ids, "every candidate was tried once");
    let end = boundary.current_commit_seq();
    let listed = ok(&read).as_array().unwrap();
    assert_eq!(listed.len(), CANDIDATES, "{read}");
    // What the read saw is the batch mid-way: the parked candidate recorded or
    // not, every later one untried.
    let tried = listed.iter().filter(|task| task["attempts"] == 1).count();
    assert!(
        (PARK - 1..=PARK).contains(&tried),
        "the read saw {tried} of {CANDIDATES} candidates tried: {read}"
    );
    let served = read["commit_seq"].as_u64().unwrap();
    let commits = format!(
        "commits: start {start}, park {at_park}, witness {witness}, read {served}, \
         next park {at_next_park}, end {end}"
    );
    // The tick's selection commit, then two per failed candidate.
    assert_eq!(at_park - start, 2 * PARK as u64, "{commits}");
    assert_eq!(end - start, 1 + 2 * CANDIDATES as u64, "{commits}");
    assert_eq!(
        at_next_park - at_park,
        2,
        "between two workspace probes a failed candidate commits its record and \
         the next selection: {commits}"
    );
    assert!(
        witness <= at_park + 1,
        "a waiter queued behind the batch was overtaken by more than one batch \
         commit, the boundary is not FIFO: {commits}"
    );
    assert!(
        (at_park..=at_park + 2).contains(&served),
        "the read was not served within the two batch commits after it was sent: \
         {commits}"
    );
    assert!(probes.state.lock().unwrap().unexpected.is_empty());
    daemon.shutdown().await.unwrap();
}

/// A git repository as the workspace manager sees it, with no real command
/// run: the repository probe succeeds, an isolated `git clone` fails and so
/// does the `rsync` it falls back to, as on a full or read-only filesystem.
/// Every spawn is logged as `command first-argument`, and an `rsync` can be
/// parked until the test releases it.
#[derive(Default)]
struct Tools {
    state: Mutex<ToolsState>,
    entered: Notify,
}

#[derive(Default)]
struct ToolsState {
    log: Vec<String>,
    park_rsync: bool,
    parked: Option<ExitNotifier>,
}

const RSYNC_FAILED: ExitOutcome = ExitOutcome {
    code: Some(23),
    signal: None,
};

impl Tools {
    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    async fn rsync_parked(&self) {
        tokio::time::timeout(DEADLINE, async {
            loop {
                let notified = self.entered.notified();
                if self.state.lock().unwrap().parked.is_some() {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("the preparation never reached rsync");
    }

    fn release(&self) {
        let parked = self.state.lock().unwrap().parked.take();
        if let Some(parked) = parked {
            parked.notify(RSYNC_FAILED);
        }
    }
}

/// A failed assertion must not leave a preparation parked under the stop.
struct Unblock(Arc<Tools>);

impl Drop for Unblock {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[async_trait]
impl ProcessManager for Tools {
    fn is_alive(&self, _: u32) -> bool {
        false
    }
    fn kill(&self, _: u32, _: Signal) {}
    async fn kill_with_grace(&self, _: u32, _: Duration) {}
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        _: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let mut state = self.state.lock().unwrap();
        let first = args.first().map(String::as_str).unwrap_or_default();
        state.log.push(format!("{command} {first}"));
        let code = match (command, first) {
            ("git", "rev-parse") => 0,
            ("git", "clone") => 128,
            ("rsync", _) if state.park_rsync => {
                let (notifier, exit) = ExitSignal::channel();
                state.parked = Some(notifier);
                drop(state);
                self.entered.notify_waiters();
                return Ok(child(exit));
            }
            ("rsync", _) => 23,
            _ => return Err(orch_core::Error::other("no agent process may start")),
        };
        Ok(child(ExitSignal::resolved(ExitOutcome {
            code: Some(code),
            signal: None,
        })))
    }
}

fn child(exit: ExitSignal) -> SpawnedChild {
    SpawnedChild {
        pid: 987_655,
        stdout: None,
        stderr: None,
        stdin: None,
        exit,
    }
}

fn due_at(ms_after_start: i64) -> Value {
    json!(orch_core::clock::iso_from_epoch_ms(
        support::TestClock::START_MS + ms_after_start
    ))
}

/// The state after `attempts` failed preparations: that many attempts,
/// `retrying`, one queued back-off due at `due` whose error names the
/// cause, and nothing running.
async fn assert_backed_off(
    raw: &mut Raw,
    daemon: &DaemonHarness,
    id: &str,
    attempts: u32,
    due: Value,
    cause: &str,
) {
    assert!(running_pid(daemon, id).await.is_none(), "a run started");
    let record = task_record(raw, id).await;
    assert_eq!(
        record["attempts"], attempts,
        "the failed preparation was not counted: {record}"
    );
    assert_eq!(record["status"], "retrying", "{record}");
    let queue = retry_queue(daemon);
    assert_eq!(queue.len(), 1, "no back-off was queued: {queue:?}");
    assert_eq!(queue[0]["task_id"], json!(id));
    assert_eq!(queue[0]["due_at"], due, "{queue:?}");
    assert!(
        queue[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains(cause)),
        "{queue:?}"
    );
}

/// Board-2b code F3: a workspaces directory that cannot be created (here a
/// regular file in its place; equally a read-only `.orchestry`) is an I/O
/// error, not a workspace domain error, and was released with no record:
/// the task stayed `todo` with no attempt and every tick prepared it again.
/// It must cost an attempt and back off like any failed preparation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blocked_workspaces_directory_counts_an_attempt_and_backs_off() {
    let clock = support::TestClock::new();
    let tools = Arc::new(Tools::default());
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .processes(tools.clone())
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = worktree_agent(&mut raw, &daemon).await;
    let id = task(&mut raw, "blocked workspaces", json!({"assignee": agent})).await;
    let workspaces = daemon.project_root().join(".orchestry/workspaces");
    if workspaces.is_dir() {
        std::fs::remove_dir(&workspaces).unwrap();
    }
    std::fs::write(&workspaces, "not a directory").unwrap();

    assert_eq!(
        daemon.step_dispatch().await.unwrap().launched,
        vec![id.clone()]
    );
    assert_backed_off(
        &mut raw,
        &daemon,
        &id,
        1,
        due_at(10_000),
        "creating the workspaces directory",
    )
    .await;
    assert_eq!(tools.log(), ["git rev-parse"]);

    // Inside the back-off: no preparation and no write.
    let before = written(&daemon, &id);
    assert!(daemon.step_dispatch().await.unwrap().launched.is_empty());
    assert_eq!(
        written(&daemon, &id),
        before,
        "a tick inside the back-off changed state.json or rewrote the task"
    );

    clock.advance_ms(10_001);
    daemon.step_dispatch().await.unwrap();
    assert_backed_off(
        &mut raw,
        &daemon,
        &id,
        2,
        due_at(10_001 + 20_000),
        "creating the workspaces directory",
    )
    .await;
    // The positive repository probe is cached; nothing else may have run.
    assert_eq!(tools.log(), ["git rev-parse"]);
    daemon.shutdown().await.unwrap();
}

/// Board-2b code F3: an isolated workspace whose clone and rsync fallback
/// both fail is a plain error, released with no record before the fix. It
/// must cost an attempt and back off; the agent itself never starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_isolated_rsync_counts_an_attempt_and_backs_off() {
    let clock = support::TestClock::new();
    let tools = Arc::new(Tools::default());
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .processes(tools.clone())
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = workspace_agent(&mut raw, &daemon, "isolated").await;
    let id = task(&mut raw, "isolated copy", json!({"assignee": agent})).await;

    assert_eq!(
        daemon.step_dispatch().await.unwrap().launched,
        vec![id.clone()]
    );
    assert_backed_off(
        &mut raw,
        &daemon,
        &id,
        1,
        due_at(10_000),
        "rsync failed with code 23",
    )
    .await;
    assert_eq!(tools.log(), ["git rev-parse", "git clone", "rsync -a"]);

    let before = written(&daemon, &id);
    assert!(daemon.step_dispatch().await.unwrap().launched.is_empty());
    assert_eq!(
        written(&daemon, &id),
        before,
        "a tick inside the back-off changed state.json or rewrote the task"
    );
    assert_eq!(
        tools.log(),
        ["git rev-parse", "git clone", "rsync -a"],
        "a tick inside the back-off prepared the workspace again"
    );

    clock.advance_ms(10_001);
    daemon.step_dispatch().await.unwrap();
    assert_backed_off(
        &mut raw,
        &daemon,
        &id,
        2,
        due_at(10_001 + 20_000),
        "rsync failed with code 23",
    )
    .await;
    assert_eq!(
        tools.log(),
        [
            "git rev-parse",
            "git clone",
            "rsync -a",
            "git clone",
            "rsync -a"
        ]
    );
    daemon.shutdown().await.unwrap();
}

/// A cancel decided while the workspace is being prepared stands: the
/// preparation that then fails is not charged to the cancelled task and
/// queues no retry, and the task is never prepared again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_a_failing_rsync_keeps_the_task_cancelled() {
    let clock = support::TestClock::new();
    let tools = Arc::new(Tools::default());
    tools.state.lock().unwrap().park_rsync = true;
    let _unblock = Unblock(Arc::clone(&tools));
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .processes(tools.clone())
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = workspace_agent(&mut raw, &daemon, "isolated").await;
    let id = task(&mut raw, "cancelled copy", json!({"assignee": agent})).await;

    let (tick, ()) = tokio::time::timeout(DEADLINE, async {
        tokio::join!(daemon.step_dispatch(), async {
            tools.rsync_parked().await;
            let cancelled = raw
                .request(1_000_105, "task.cancel", json!({"id": id}))
                .await;
            ok(&cancelled);
            tools.release();
        })
    })
    .await
    .expect("the tick or the cancel hung");
    tick.unwrap();

    let record = task_record(&mut raw, &id).await;
    assert_eq!(record["status"], "cancelled", "{record}");
    assert_eq!(record["attempts"], 0, "{record}");
    assert!(
        retry_queue(&daemon).is_empty(),
        "{:?}",
        retry_queue(&daemon)
    );
    assert!(running_pid(&daemon, &id).await.is_none());

    clock.advance_ms(60_000);
    daemon.step_dispatch().await.unwrap();
    assert_eq!(task_record(&mut raw, &id).await["status"], "cancelled");
    assert_eq!(tools.log(), ["git rev-parse", "git clone", "rsync -a"]);
    daemon.shutdown().await.unwrap();
}
