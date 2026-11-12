//! Non-forking wake fixture. Only make_fake_harness changes on the repair branch.
use crate::support::{self, Raw, TestClock, ok};
use orch_core::process::{
    ExitNotifier, ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild,
};
use orch_daemon::{DispatchMode, testing::DaemonHarness};
use orch_domain::OrchestratorEvent;
use orch_engine::events::EventSubscription;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
pub(super) const POLL_MS: u64 = 60_000;
pub(super) const WAKE_BOUND: Duration = Duration::from_secs(5);

async fn make_fake_harness(
    root: &std::path::Path,
    pm: Arc<Scripted>,
    clock: Arc<TestClock>,
) -> DaemonHarness {
    DaemonHarness::builder()
        .clock(clock)
        .maintenance_timer(false)
        .dispatch(DispatchMode::On)
        .processes(pm as Arc<dyn ProcessManager>)
        .isolated_fake_spawns()
        .start_in(root)
        .await
        .unwrap()
}
#[derive(Default)]
pub(super) struct Scripted {
    /// Children exit only when [`Scripted::release`] says so.
    pub(super) hold: bool,
    /// Refuse spawns that carry no task id (the `git` probes of workspace
    /// preparation), as a machine without git would.
    pub(super) refuse_probes: bool,
    spawns: Mutex<Vec<String>>,
    held: Mutex<Vec<(String, u32, ExitNotifier)>>,
    alive: Mutex<HashSet<u32>>,
    pids: AtomicU32,
}

impl Scripted {
    pub(super) fn held(refuse_probes: bool) -> Self {
        Self {
            hold: true,
            refuse_probes,
            ..Default::default()
        }
    }

    pub(super) fn launches_of(&self, task: &str) -> usize {
        self.spawns
            .lock()
            .unwrap()
            .iter()
            .filter(|spawned| spawned.as_str() == task)
            .count()
    }

    /// Task launches (spawns carrying a task id; adapter probes carry none).
    pub(super) fn launches(&self) -> usize {
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
    pub(super) fn release(&self, task: &str) {
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
        _command: &str,
        _args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let task = opts.env.get("ORCH_TASK_ID").cloned().unwrap_or_default();
        self.spawns.lock().unwrap().push(task.clone());
        if self.refuse_probes && task.is_empty() {
            return Err(orch_core::Error::other("no git in this test"));
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

pub(super) struct Fx {
    pub(super) daemon: DaemonHarness,
    raw: Raw,
    bus: EventSubscription,
    pub(super) pm: Arc<Scripted>,
    /// Automatic ticks seen so far (see [`Fx::ticks`]).
    seen: usize,
    _project: tempfile::TempDir,
}

pub(super) async fn fixture(pm: Scripted) -> Fx {
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
    let daemon = make_fake_harness(&root, pm.clone(), clock.clone()).await;
    let bus = daemon.engine().events.subscribe();
    let raw = Raw::session(&daemon, "cli", "normal").await;
    Fx {
        daemon,
        raw,
        bus,
        pm,
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
    pub(super) async fn next_tick(&mut self, within: Duration) -> Option<Instant> {
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
    pub(super) async fn settle(&mut self) {
        let _ = self.next_tick(WAKE_BOUND).await;
        // Anything else that completed meanwhile is part of the baseline.
        let _ = self.ticks();
    }

    pub(super) async fn agent(&mut self, name: &str, workspace_mode: &str) -> String {
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

    pub(super) async fn task(&mut self, title: &str, extra: Value) -> String {
        support::task(&mut self.raw, title, extra).await
    }

    pub(super) async fn call(&mut self, op: &str, args: Value) -> Value {
        let reply = self.raw.request(1_000_301, op, args).await;
        ok(&reply).clone()
    }

    pub(super) async fn finish(self) {
        self.daemon.shutdown().await.unwrap();
    }
}

impl Scripted {
    pub(super) fn log(&self) -> Vec<String> {
        self.spawns.lock().unwrap().clone()
    }
}
