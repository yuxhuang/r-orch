#![cfg(feature = "testing")]
//! The commit-triggered dispatch wake (T3; spec R16, ruling 10.9, brief D6,
//! ruling 14.4), observed from outside the dispatch loop.
//!
//! The daemon's periodic poll is configured to 60 s, so anything that happens
//! within a second or two of a client's write was caused by the wake, not by
//! the poll. A *tick* is observed as the engine's `orchestrator:tick` bus event
//! (every completed automatic tick emits exactly one); launches are observed
//! at a fake process manager (`ORCH_TASK_ID` of each spawn). The deterministic
//! vectors run on Tokio's paused clock: while the daemon is idle the runtime
//! jumps straight to the next timer, so "the tick happened 500 ms after the
//! commit" is exact and nothing sleeps. A wake that does not exist shows as
//! the 5-second bound of the wait expiring first.
//!
//! Vectors (D6): launch after the 500 ms wake, not the 60 s poll; N = 100
//! commits in one window = exactly one wake tick; writes during that tick = at
//! most one follow-up; continuous traffic cannot postpone the first wake and
//! ticks over T are bounded by `1 + ceil(T / 500 ms)`; a tick that finds
//! nothing to do does not wake itself; commits with dispatch OFF do not tick
//! and never switch it on; a pause decided before the wake keeps its tasks
//! unlaunched; a pre-launch failure keeps its back-off however many wakes;
//! the periodic tick still runs; and a collector/completion *spawned by a
//! tick* wakes the scheduler (ruling 14.4).

use orch_core::process::{
    ExitNotifier, ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild,
};
use orch_daemon::DispatchMode;
use orch_daemon::testing::DaemonHarness;
use orch_domain::OrchestratorEvent;
use orch_engine::events::EventSubscription;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tokio::time::Instant;

#[macro_use]
mod support;
use support::{Raw, TestClock, ok};

/// The only way a wake can be told from the poll: the poll is far away.
const POLL_MS: u64 = 60_000;
/// How long a wake may take in these vectors: 500 ms plus scheduling slack.
const WAKE_BOUND: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// A fake process manager that records launches and holds exits
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Scripted {
    /// Children exit only when [`Scripted::release`] says so.
    hold: bool,
    /// Refuse spawns that carry no task id (the `git` probes of workspace
    /// preparation), as a machine without git would.
    refuse_probes: bool,
    /// Block inside `spawn` until [`Scripted::open_gate`] (a tick in flight).
    gated: bool,
    /// Gate the external effort probe instead of the permit-held task spawn.
    probe_gate: bool,
    gate: (Mutex<bool>, Condvar),
    entered: tokio::sync::Notify,
    spawns: Mutex<Vec<String>>,
    held: Mutex<Vec<(String, u32, ExitNotifier)>>,
    alive: Mutex<HashSet<u32>>,
    pids: AtomicU32,
}

impl Scripted {
    fn launches_of(&self, task: &str) -> usize {
        self.spawns
            .lock()
            .unwrap()
            .iter()
            .filter(|spawned| spawned.as_str() == task)
            .count()
    }

    /// Task launches (spawns carrying a task id; adapter probes carry none).
    fn launches(&self) -> usize {
        self.spawns
            .lock()
            .unwrap()
            .iter()
            .filter(|task| !task.is_empty())
            .count()
    }

    /// A kill ends the child: its exit signal fires, as a real reaper's would.
    fn end(&self, pid: u32) {
        self.alive.lock().unwrap().remove(&pid);
        for (_, held_pid, notifier) in self.held.lock().unwrap().iter() {
            if *held_pid == pid {
                notifier.notify(ExitOutcome {
                    code: None,
                    signal: Some(15),
                });
            }
        }
    }

    /// Let the held child of `task` exit successfully.
    fn release(&self, task: &str) {
        for (id, pid, notifier) in self.held.lock().unwrap().iter() {
            if id == task {
                self.alive.lock().unwrap().remove(pid);
                notifier.notify(ExitOutcome {
                    code: Some(0),
                    signal: None,
                });
            }
        }
    }

    fn open_gate(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

/// A failed assertion must not leave the runtime worker blocked in `spawn`.
struct OpenOnDrop(Arc<Scripted>);
impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open_gate();
    }
}

#[async_trait::async_trait]
impl ProcessManager for Scripted {
    fn is_alive(&self, pid: u32) -> bool {
        self.alive.lock().unwrap().contains(&pid)
    }
    fn kill(&self, pid: u32, _: Signal) {
        self.end(pid);
    }
    async fn kill_with_grace(&self, pid: u32, _: Duration) {
        self.end(pid);
    }
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let task = opts.env.get("ORCH_TASK_ID").cloned().unwrap_or_default();
        let probe = command == "opencode" && (args == ["--version"] || args == ["run", "--help"]);
        if self.probe_gate && probe {
            if args == ["--version"] {
                self.entered.notify_one();
                tokio::task::block_in_place(|| {
                    let mut open = self.gate.0.lock().unwrap();
                    while !*open {
                        open = self.gate.1.wait(open).unwrap();
                    }
                });
            }
            return Ok(SpawnedChild {
                pid: 50_000 + self.pids.fetch_add(1, Ordering::SeqCst),
                stdout: Some(Box::new(std::io::Cursor::new(if args == ["--version"] {
                    b"opencode v2.1.2".to_vec()
                } else {
                    b"--model provider/model#variant".to_vec()
                }))),
                stderr: None,
                stdin: None,
                exit: ExitSignal::resolved(ExitOutcome {
                    code: Some(0),
                    signal: None,
                }),
            });
        }
        self.spawns.lock().unwrap().push(task.clone());
        if self.refuse_probes && task.is_empty() {
            return Err(orch_core::Error::other("no git in this test"));
        }
        if self.gated && !self.probe_gate {
            self.entered.notify_one();
            tokio::task::block_in_place(|| {
                let mut open = self.gate.0.lock().unwrap();
                while !*open {
                    open = self.gate.1.wait(open).unwrap();
                }
            });
        }
        let pid = 50_000 + self.pids.fetch_add(1, Ordering::SeqCst);
        let exit = if self.hold {
            let (notifier, exit) = ExitSignal::channel();
            self.alive.lock().unwrap().insert(pid);
            self.held.lock().unwrap().push((task, pid, notifier));
            exit
        } else {
            ExitSignal::resolved(ExitOutcome {
                code: Some(0),
                signal: None,
            })
        };
        Ok(SpawnedChild {
            pid,
            stdout: Some(Box::new(std::io::Cursor::new(Vec::new()))),
            stderr: None,
            stdin: None,
            exit,
        })
    }
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

struct Fx {
    daemon: DaemonHarness,
    raw: Raw,
    bus: EventSubscription,
    pm: Arc<Scripted>,
    clock: Arc<TestClock>,
    /// Automatic ticks seen so far (see [`Fx::ticks`]).
    seen: usize,
    _project: tempfile::TempDir,
}

async fn fixture(dispatch: DispatchMode, pm: Scripted) -> Fx {
    use orch_core::storage::ConfigStore;
    use std::os::unix::fs::DirBuilderExt;
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root.join(".orchestry"))
        .unwrap();
    let mut config = orch_domain::default_config();
    config.scheduling.poll_interval_ms = POLL_MS;
    orch_infra::storage::FileConfigStore::new(orch_infra::storage::Paths::new(&root))
        .write(&config)
        .await
        .unwrap();
    let pm = Arc::new(pm);
    let clock = TestClock::new();
    let daemon = DaemonHarness::builder()
        .clock(clock.clone())
        .maintenance_timer(false)
        .dispatch(dispatch)
        .processes(pm.clone() as Arc<dyn ProcessManager>)
        .start_in(&root)
        .await
        .unwrap();
    let bus = daemon.engine().events.subscribe();
    let raw = Raw::session(&daemon, "cli", "normal").await;
    Fx {
        daemon,
        raw,
        bus,
        pm,
        clock,
        seen: 0,
        _project: project,
    }
}

fn is_tick(event: &OrchestratorEvent) -> bool {
    matches!(event, OrchestratorEvent::OrchestratorTick { .. })
}

impl Fx {
    /// Total automatic ticks completed so far.
    fn ticks(&mut self) -> usize {
        self.seen += self
            .bus
            .try_drain()
            .iter()
            .filter(|event| is_tick(event))
            .count();
        self.seen
    }

    /// Wait (virtual time) for one more tick; `None` if none within `within`.
    async fn next_tick(&mut self, within: Duration) -> Option<Instant> {
        let before = self.ticks();
        let wait = async {
            loop {
                match self.bus.recv().await {
                    Some(event) if is_tick(&event) => return,
                    Some(_) => {}
                    None => std::future::pending::<()>().await,
                }
            }
        };
        match tokio::time::timeout(within, wait).await {
            Ok(()) => {
                self.seen = before + 1;
                Some(Instant::now())
            }
            Err(_) => None,
        }
    }

    /// Let the wake that the fixture's own setup commits armed run, so a
    /// vector starts from an idle scheduler. Best effort: with no wake it
    /// simply times out and the vector's own assertion reports the absence.
    async fn settle(&mut self) {
        let _ = self.next_tick(WAKE_BOUND).await;
        // Anything else that completed meanwhile is part of the baseline.
        let _ = self.ticks();
    }

    async fn agent(&mut self, name: &str, workspace_mode: &str) -> String {
        let reply = self
            .raw
            .request(
                1_000_300,
                "agent.create",
                json!({
                    "name": name,
                    "adapter": "shell",
                    "command": "exit 0",
                    "approval_policy": "auto",
                    "workspace_mode": workspace_mode,
                    "env": [
                        {"name": "HOME", "value": self.daemon.project_root().to_str().unwrap()},
                        {"name": "PATH", "value": "/usr/bin:/bin"},
                    ],
                }),
            )
            .await;
        ok(&reply)["agent"]["id"].as_str().unwrap().to_owned()
    }

    async fn task(&mut self, title: &str, extra: Value) -> String {
        support::task(&mut self.raw, title, extra).await
    }

    async fn status_of(&mut self, id: &str) -> String {
        support::task_status(&mut self.raw, id).await
    }

    async fn call(&mut self, op: &str, args: Value) -> Value {
        let reply = self.raw.request(1_000_301, op, args).await;
        ok(&reply).clone()
    }

    async fn finish(self) {
        self.pm.open_gate();
        self.daemon.shutdown().await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Vectors on the paused clock
// ---------------------------------------------------------------------------

/// An eligible task queued through a client launches after the 500 ms wake,
/// not after the 60 s poll, and not before the debounce window.
#[tokio::test(start_paused = true)]
async fn a_queued_task_launches_after_the_500ms_wake_not_the_60s_poll() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    let agent = fx.agent("wakeful", "shared").await;
    fx.settle().await;

    let queued_at = Instant::now();
    let id = fx.task("wake me", json!({"assignee": agent})).await;
    let tick_at = fx.next_tick(WAKE_BOUND).await.unwrap_or_else(|| {
        panic!(
            "no wake tick within {WAKE_BOUND:?} of a commit while the poll interval is \
             {POLL_MS} ms: the commit did not wake the scheduler"
        )
    });
    let waited = tick_at - queued_at;
    assert!(
        waited >= Duration::from_millis(400),
        "the wake ran after {waited:?}: the 500 ms debounce window was skipped"
    );
    assert!(
        waited <= Duration::from_millis(1_500),
        "the wake ran after {waited:?}, not about 500 ms"
    );
    let pm = fx.pm.clone();
    eventually!(
        "the woken tick to launch the task",
        pm.launches_of(&id) == 1
    );
    assert_eq!(fx.pm.launches_of(&id), 1, "exactly one launch");
    fx.finish().await;
}

/// N = 100 commits inside one window cost exactly one wake tick, however many
/// commits there were (a dirty generation, not a queued tick per commit).
#[tokio::test(start_paused = true)]
async fn a_burst_of_100_commits_in_one_window_causes_exactly_one_wake_tick() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    fx.settle().await;
    let base = fx.ticks();
    for n in 0..100 {
        fx.task(&format!("burst {n}"), json!({})).await;
    }
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "100 commits woke nothing within {WAKE_BOUND:?} (poll is {POLL_MS} ms)"
    );
    // Keep the clock moving well past any further debounce window, but far
    // short of the poll: nothing else may tick.
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(
        fx.ticks() - base,
        1,
        "100 commits in one window must be one wake tick"
    );
    fx.finish().await;
}

/// Continuous traffic cannot postpone the first wake (the deadline is fixed by
/// the first commit, later commits coalesce without moving it), and over a
/// duration T the wake ticks stay within `1 + ceil(T / 500 ms)`.
#[tokio::test(start_paused = true)]
async fn continuous_traffic_cannot_postpone_the_first_wake_and_ticks_stay_bounded() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    fx.settle().await;
    let base = fx.ticks();
    let started = Instant::now();
    let mut first_tick_after = None;
    for n in 0..30 {
        fx.task(&format!("traffic {n}"), json!({})).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        if first_tick_after.is_none() && fx.ticks() > base {
            first_tick_after = Some(Instant::now() - started);
        }
    }
    let traffic = Instant::now() - started;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let total = Instant::now() - started;
    let first = first_tick_after.unwrap_or_else(|| {
        panic!("no tick during {traffic:?} of continuous writes: the wake was postponed forever")
    });
    assert!(
        first <= Duration::from_millis(900),
        "the first wake tick came after {first:?} of continuous traffic (500 ms debounce)"
    );
    let ticks = fx.ticks() - base;
    let bound = 1 + total.as_millis().div_ceil(500) as usize;
    assert!(
        ticks <= bound,
        "{ticks} wake ticks in {total:?}; the bound is 1 + ceil(T/500ms) = {bound}"
    );
    fx.finish().await;
}

/// A tick that finds nothing to do commits nothing that matters and so does
/// not wake itself: after one wake tick the scheduler is quiet until the poll.
#[tokio::test(start_paused = true)]
async fn a_tick_that_finds_nothing_to_do_does_not_wake_itself() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    fx.settle().await;
    let base = fx.ticks();
    fx.task("one commit", json!({})).await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "the commit did not wake the scheduler within {WAKE_BOUND:?}"
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(
        fx.ticks() - base,
        1,
        "a no-op tick re-armed the wake (a self-wake feedback loop)"
    );
    fx.finish().await;
}

/// With dispatch OFF commits tick nothing, and the wake never switches
/// dispatch on.
#[tokio::test(start_paused = true)]
async fn commits_with_dispatch_off_do_not_tick_and_never_switch_dispatch_on() {
    let mut fx = fixture(DispatchMode::Off, Scripted::default()).await;
    let agent = fx.agent("idle", "shared").await;
    let id = fx.task("not yet", json!({"assignee": agent})).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(fx.ticks(), 0, "an OFF daemon ticked on a commit");
    assert_eq!(fx.pm.launches(), 0, "an OFF daemon launched work");
    assert_eq!(fx.status_of(&id).await, "todo");
    let status = fx.call("daemon.status", json!({})).await;
    assert_eq!(status["dispatch"]["dispatch_on"], false, "{status}");
    fx.finish().await;
}

/// The periodic tick is not replaced by the wake: with no commits at all the
/// scheduler still ticks every poll interval (time-dependent retries and
/// maintenance depend on it).
#[tokio::test(start_paused = true)]
async fn the_periodic_tick_still_runs_when_nothing_commits() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    fx.settle().await;
    let base = fx.ticks();
    let tick = fx.next_tick(Duration::from_millis(POLL_MS + 5_000)).await;
    assert!(
        tick.is_some(),
        "no periodic tick within {} ms of silence (base {base})",
        POLL_MS + 5_000
    );
    fx.finish().await;
}

/// Pause decided before the wake fires: the wake tick still runs but launches
/// nothing of the paused goal; resuming is a commit and wakes the scheduler,
/// which then launches the waiting task.
#[tokio::test(start_paused = true)]
async fn a_pause_decided_before_the_wake_keeps_the_task_unlaunched_and_resume_wakes_it() {
    let mut fx = fixture(DispatchMode::On, Scripted::default()).await;
    let agent = fx.agent("pausable", "shared").await;
    let goal = fx.call("goal.create", json!({"title": "g"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.settle().await;

    let id = fx
        .task(
            "queued under a goal",
            json!({"assignee": agent, "goal_id": goal}),
        )
        .await;
    // The queueing armed the wake; the pause lands inside its window.
    fx.call("goal.status", json!({"id": goal, "status": "paused"}))
        .await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "commits did not wake the scheduler within {WAKE_BOUND:?}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        fx.pm.launches_of(&id),
        0,
        "the wake tick launched a task of a paused goal"
    );
    assert_eq!(fx.status_of(&id).await, "todo");

    fx.call("goal.status", json!({"id": goal, "status": "active"}))
        .await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "resuming did not wake the scheduler within {WAKE_BOUND:?}"
    );
    let pm = fx.pm.clone();
    eventually!("the resumed task to launch", pm.launches_of(&id) == 1);
    fx.finish().await;
}

/// A workspace failure before spawn is retried on its back-off, not on every
/// wake: however many commits wake the scheduler, the injected clock decides.
#[tokio::test(start_paused = true)]
async fn wakes_do_not_evade_the_pre_launch_retry_backoff() {
    let mut fx = fixture(
        DispatchMode::On,
        Scripted {
            refuse_probes: true,
            ..Default::default()
        },
    )
    .await;
    // A worktree agent in a project that is not a git repository fails before
    // any run or process exists (see daemon_prelaunch_failure).
    let agent = fx.agent("needs-git", "worktree").await;
    fx.settle().await;
    let id = fx
        .task("needs a worktree", json!({"assignee": agent}))
        .await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "the commit did not wake the scheduler within {WAKE_BOUND:?}"
    );
    let record = fx.call("task.get", json!({"id": id})).await;
    assert_eq!(
        record["attempts"], 1,
        "the failed launch must count: {record}"
    );
    assert_eq!(
        fx.pm.launches_of(&id),
        0,
        "nothing may spawn without a workspace"
    );

    // Five more wakes with the back-off (10 s on the injected clock) not due.
    for n in 0..5 {
        fx.task(&format!("unrelated {n}"), json!({})).await;
        assert!(
            fx.next_tick(WAKE_BOUND).await.is_some(),
            "wake {n} did not tick"
        );
        let record = fx.call("task.get", json!({"id": id})).await;
        assert_eq!(
            record["attempts"], 1,
            "wake {n} re-ran the launch inside the back-off: {record}"
        );
    }
    // Once the injected clock passes the back-off, the next wake retries.
    fx.clock.advance_ms(10_001);
    fx.task("after the back-off", json!({})).await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "the commit after the back-off did not wake the scheduler"
    );
    let record = fx.call("task.get", json!({"id": id})).await;
    assert_eq!(record["attempts"], 2, "the due retry did not run: {record}");
    fx.finish().await;
}

/// Ruling 14.4: the commits of a collector or completion that a tick *spawned*
/// are not tick-origin commits. They wake the scheduler, so a dependent task
/// starts within the wake window of its prerequisite finishing, not after the
/// 60 s poll.
#[tokio::test(start_paused = true)]
async fn a_collector_completion_spawned_by_a_tick_wakes_the_scheduler() {
    let mut fx = fixture(
        DispatchMode::On,
        Scripted {
            hold: true,
            ..Default::default()
        },
    )
    .await;
    let agent = fx.agent("chain", "shared").await;
    fx.settle().await;
    let first = fx.task("first", json!({"assignee": agent})).await;
    let second = fx
        .task("second", json!({"assignee": agent, "depends_on": [first]}))
        .await;
    assert!(
        fx.next_tick(WAKE_BOUND).await.is_some(),
        "queueing did not wake the scheduler within {WAKE_BOUND:?}"
    );
    let pm = fx.pm.clone();
    eventually!("the first task to launch", pm.launches_of(&first) == 1);
    assert_eq!(fx.pm.launches_of(&second), 0, "the dependent started early");

    // The agent finishes on its own, with no client write: its collector (a
    // task spawned by the tick that launched it) completes the run.
    let released_at = Instant::now();
    fx.pm.release(&first);
    let woke = fx.next_tick(WAKE_BOUND).await.unwrap_or_else(|| {
        panic!(
            "no tick within {WAKE_BOUND:?} of the collector completing {first} while the \
             poll is {POLL_MS} ms: commits of tasks spawned by a tick were treated as the \
             tick's own and suppressed"
        )
    });
    assert!(
        woke - released_at <= Duration::from_millis(2_000),
        "the completion woke the scheduler after {:?}",
        woke - released_at
    );
    let pm = fx.pm.clone();
    eventually!("the dependent task to launch", pm.launches_of(&second) == 1);
    fx.finish().await;
}

// ---------------------------------------------------------------------------
// Writes during an in-flight tick (real time: the tick must really be busy)
// ---------------------------------------------------------------------------

/// The external preparation probe holds one tick in flight without holding
/// the operation permit. All writers acknowledge before it is released. The
/// no-output child stays alive until the write follow-up and its quiet window
/// have completed, separating the collector's independently eligible commit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_during_the_wake_tick_cause_at_most_one_follow_up_tick() {
    // Unlike Fx::next_tick, do not drain already queued tick events: opening
    // the probe may complete the in-flight tick before this future is polled.
    async fn completed_tick(fx: &mut Fx, why: &str) -> Instant {
        tokio::time::timeout(WAKE_BOUND, async {
            loop {
                if let Some(event) = fx.bus.recv().await
                    && is_tick(&event)
                {
                    fx.seen += 1;
                    return Instant::now();
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{why}: no tick within {WAKE_BOUND:?}"))
    }

    async fn quiet(fx: &mut Fx, why: &str) {
        let before = fx.seen;
        let extra = tokio::time::timeout(Duration::from_millis(1_500), async {
            loop {
                if let Some(event) = fx.bus.recv().await
                    && is_tick(&event)
                {
                    fx.seen += 1;
                    return;
                }
            }
        })
        .await;
        assert!(
            extra.is_err(),
            "{why}: at most one follow-up tick; saw an extra tick"
        );
        assert_eq!(fx.seen, before, "{why}: scheduler must remain quiet");
    }

    let mut fx = fixture(
        DispatchMode::On,
        Scripted {
            gated: true,
            probe_gate: true,
            hold: true,
            ..Default::default()
        },
    )
    .await;
    let agent = fx
        .call(
            "agent.create",
            json!({
                "name": "slow-preparation", "adapter": "opencode", "command": "fake-agent",
                "model": "openrouter/anthropic/claude-opus-5.5", "effort": "high",
                "approval_policy": "auto", "workspace_mode": "shared",
                "env": [
                    {"name": "HOME", "value": fx.daemon.project_root().to_str().unwrap()},
                    {"name": "PATH", "value": "/usr/bin:/bin"}
                ]
            }),
        )
        .await["agent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let _open = OpenOnDrop(fx.pm.clone());
    fx.settle().await;
    let base = fx.ticks();
    let id = fx.task("slow to start", json!({"assignee": agent})).await;
    tokio::time::timeout(Duration::from_secs(10), fx.pm.entered.notified())
        .await
        .expect("the wake tick never reached external preparation within 10 s");

    let mut writers = Vec::new();
    for n in 0..20 {
        let mut other = Raw::session(&fx.daemon, "cli", "normal").await;
        writers.push(tokio::spawn(async move {
            support::task(&mut other, &format!("during the tick {n}"), json!({})).await;
        }));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        for writer in writers {
            writer.await.unwrap();
        }
    })
    .await
    .expect("writers cannot commit while the tick is held at the external probe");
    assert_eq!(
        fx.ticks(),
        base,
        "the first tick must remain in flight until all writers acknowledge"
    );
    fx.pm.open_gate();
    let completed = completed_tick(&mut fx, "in-flight tick completion").await;
    assert_eq!(fx.pm.launches_of(&id), 1, "exactly one launch");
    let follow_up = completed_tick(&mut fx, "one follow-up for in-flight commits").await;
    println!("follow-up latency: {:?}", follow_up - completed);
    assert_eq!(fx.seen - base, 2, "the wake tick and exactly one follow-up");
    quiet(&mut fx, "in-flight commits").await;

    fx.pm.release(&id);
    completed_tick(&mut fx, "collector completion wake").await;
    assert_eq!(
        fx.seen - base,
        3,
        "one independent collector completion wake"
    );
    quiet(&mut fx, "collector completion").await;
    fx.finish().await;
}
