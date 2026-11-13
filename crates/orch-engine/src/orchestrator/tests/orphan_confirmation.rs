//! Post-kill liveness is controlled independently of the kill's return.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orch_core::error::Result;
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_core::test_support::{make_agent, make_run, make_task};
use orch_domain::{OrchestratorState, Run, RunStatus, TaskStatus};
use tokio::time::Instant;

use super::harness::{Harness, LaunchProcesses, NOW_ISO, running_entry};

#[derive(Default)]
struct ControlledProcesses {
    // None survives forever; Some(delay) dies that long after kill returns.
    exits: BTreeMap<u32, Option<Duration>>,
    killed: Mutex<Vec<(u32, Instant)>>,
    checks: Mutex<Vec<(u32, Instant)>>,
}

#[async_trait::async_trait]
impl ProcessManager for ControlledProcesses {
    fn is_alive(&self, pid: u32) -> bool {
        let now = Instant::now();
        self.checks.lock().unwrap().push((pid, now));
        let killed = self.killed.lock().unwrap();
        match (self.exits.get(&pid), killed.iter().find(|(p, _)| *p == pid)) {
            (Some(Some(delay)), Some((_, at))) => now < *at + *delay,
            _ => true,
        }
    }

    fn kill(&self, _: u32, _: Signal) {
        panic!("confirmation must not send another signal");
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        assert_eq!(grace, orch_core::process::DEFAULT_KILL_GRACE);
        self.killed.lock().unwrap().push((pid, Instant::now()));
    }

    fn spawn(&self, _: &str, _: &[String], _: SpawnOptions) -> Result<SpawnedChild> {
        panic!("recovery must not spawn");
    }
}

fn run(id: &str, pid: u32) -> Run {
    Run {
        id: id.into(),
        task_id: format!("tsk_{id}"),
        status: RunStatus::Running,
        pid: Some(pid),
        ..make_run()
    }
}

fn fixture(processes: Arc<dyn ProcessManager>, runs: &[Run], adopted: bool) -> Harness {
    let mut state = OrchestratorState::default();
    let tasks = runs
        .iter()
        .map(|run| orch_domain::Task {
            id: run.task_id.clone(),
            status: TaskStatus::InProgress,
            assignee: Some(run.agent_id.clone()),
            ..make_task()
        })
        .collect::<Vec<_>>();
    if adopted {
        for run in runs {
            state.running.insert(
                run.task_id.clone(),
                running_entry(
                    &run.task_id,
                    &run.agent_id,
                    &run.id,
                    run.pid.unwrap(),
                    NOW_ISO,
                ),
            );
        }
    }
    let h = Harness::builder()
        .tasks(tasks)
        .agents([make_agent()])
        .state(state)
        .processes(processes)
        .build();
    for run in runs {
        h.runs.seed(run.clone());
    }
    h
}

#[tokio::test]
async fn startup_waits_for_death_including_the_final_deadline_check() {
    // Death at exactly the confirmation deadline requires the final probe.
    let processes = Arc::new(ControlledProcesses {
        exits: [(41, Some(Duration::from_secs(1)))].into(),
        ..Default::default()
    });
    let run = run("run_deadline", 41);
    let h = fixture(processes.clone(), std::slice::from_ref(&run), false);
    h.orchestrator.daemon_initialize().await.unwrap();
    let killed = processes.killed.lock().unwrap();
    assert_eq!(killed.len(), 1);
    let checks = processes.checks.lock().unwrap();
    let after = checks
        .iter()
        .filter(|(_, at)| *at >= killed[0].1)
        .collect::<Vec<_>>();
    assert!(
        after.len() > 2,
        "confirmation must poll, not just sleep once"
    );
    assert!(after.last().unwrap().1 >= killed[0].1 + Duration::from_secs(1));
    assert_eq!(h.runs.peek(&run.id).unwrap().status, RunStatus::Cancelled);
}

#[tokio::test]
async fn startup_attempts_every_orphan_before_reporting_all_survivors() {
    let processes = Arc::new(ControlledProcesses {
        exits: [
            (41, None),
            (42, Some(Duration::from_millis(20))),
            (43, None),
        ]
        .into(),
        ..Default::default()
    });
    let runs = [run("run_a", 41), run("run_b", 42), run("run_c", 43)];
    let h = fixture(processes.clone(), &runs, false);
    let began = Instant::now();
    let error = h
        .orchestrator
        .daemon_initialize()
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        processes
            .killed
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>(),
        vec![41, 42, 43]
    );
    assert!(began.elapsed() >= Duration::from_secs(2));
    assert_eq!(
        error,
        "Daemon recovery could not stop an orphaned local child: run run_a, PID 41; run run_c, PID 43"
    );
    for run in &runs {
        assert_eq!(h.runs.peek(&run.id).unwrap().status, RunStatus::Running);
    }
    assert!(
        h.persisted_state().pid.is_none(),
        "failed recovery must not become ready"
    );
}

#[tokio::test]
async fn shutdown_confirms_death_and_retains_only_surviving_reservations() {
    let processes = Arc::new(ControlledProcesses {
        exits: [
            (41, None),
            (42, Some(Duration::from_millis(20))),
            (43, None),
        ]
        .into(),
        ..Default::default()
    });
    let runs = [run("run_a", 41), run("run_b", 42), run("run_c", 43)];
    let h = fixture(processes.clone(), &runs, true);
    h.orchestrator.daemon_initialize().await.unwrap();
    let error = h.orchestrator.daemon_stop().await.unwrap_err().to_string();
    assert_eq!(
        processes
            .killed
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>(),
        vec![41, 42, 43]
    );
    assert_eq!(
        error,
        "Daemon shutdown incomplete; local children remain alive: run run_a, PID 41; run run_c, PID 43"
    );
    let state = h.persisted_state();
    for run in [&runs[0], &runs[2]] {
        assert!(state.running.contains_key(&run.task_id));
        assert_eq!(h.runs.peek(&run.id).unwrap().status, RunStatus::Running);
    }
    assert!(!state.running.contains_key(&runs[1].task_id));
    assert_eq!(
        h.runs.peek(&runs[1].id).unwrap().status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn recovery_identifiers_are_bounded_and_cannot_add_error_lines() {
    let processes = Arc::new(ControlledProcesses::default());
    let run = run(&format!("run_\n{}", "x".repeat(256)), 41);
    let h = fixture(processes, &[run], false);
    let error = h
        .orchestrator
        .daemon_initialize()
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        format!(
            "Daemon recovery could not stop an orphaned local child: run run__{}, PID 41",
            "x".repeat(59)
        )
    );
}

#[tokio::test]
async fn worker_mirror_and_unverified_reservations_are_never_signalled() {
    let processes = Arc::new(ControlledProcesses::default());
    let mut mirror = run("run_mirror", 41);
    mirror.agent_id = "agt_worker@peer".into();
    let h = fixture(processes.clone(), &[mirror], false);
    h.orchestrator.daemon_initialize().await.unwrap();
    assert!(processes.killed.lock().unwrap().is_empty());

    let owned = run("run_worker", 42);
    let h = fixture(processes.clone(), std::slice::from_ref(&owned), true);
    let mut task = h.task(&owned.task_id);
    task.labels.push("remote-ctl:controller".into());
    h.tasks.seed(task);
    h.orchestrator.daemon_initialize().await.unwrap();
    h.orchestrator.daemon_stop().await.unwrap_err();
    assert!(processes.killed.lock().unwrap().is_empty());
    assert!(h.persisted_state().running.contains_key(&owned.task_id));
}

// The fixture's existing real-process assembly uses a private environment
// and process group. This guard cleans only the child created below.
struct ChildCleanup {
    processes: Arc<dyn ProcessManager>,
    pid: Option<u32>,
}
impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            self.processes.kill(pid, Signal::Kill);
        }
    }
}

async fn forced_child(shutdown: bool) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let root = tempfile::tempdir().unwrap();
    let work = LaunchProcesses::default();
    let (_, processes) = work.shell_adapter_and_processes(root.path());
    let reservation = super::harness::reserve_process_launch().await;
    let child = {
        let _scope = reservation.enter();
        processes
            .spawn(
                "/bin/sh",
                &[
                    "-c".into(),
                    "trap '' TERM; printf 'ready\\n'; while :; do :; done".into(),
                ],
                SpawnOptions {
                    cwd: root.path().into(),
                    ..Default::default()
                },
            )
            .unwrap()
    };
    let mut cleanup = ChildCleanup {
        processes: processes.clone(),
        pid: Some(child.pid),
    };
    let mut output = BufReader::new(child.stdout.unwrap()).lines();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("ready")
    );
    let run = run("run_forced", child.pid);
    let h = fixture(processes.clone(), std::slice::from_ref(&run), shutdown);
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        h.orchestrator.daemon_initialize().await?;
        if shutdown {
            h.orchestrator.daemon_stop().await?;
        }
        Ok::<_, orch_core::error::Error>(())
    })
    .await
    .unwrap();
    // Reap before any assertion can unwind and drop this runtime.
    let outcome = tokio::time::timeout(Duration::from_secs(3), child.exit.wait())
        .await
        .unwrap();
    cleanup.pid = None;
    assert_eq!(
        outcome.signal,
        Some(9),
        "the child must ignore TERM and require KILL"
    );
    assert!(!processes.is_alive(child.pid));
    // Already reaped: never send a signal to a potentially recycled PID.
    drop(cleanup);
    result.unwrap();
    assert_eq!(h.runs.peek(&run.id).unwrap().status, RunStatus::Cancelled);
    assert!(h.persisted_state().running.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_accepts_a_term_ignoring_child_after_sigkill() {
    forced_child(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_releases_a_term_ignoring_child_after_sigkill() {
    forced_child(true).await;
}
