//! R119 against real processes: a recorded pid whose process is alive, and one whose
//! leader is gone while a child of its group still runs (F17). The check asks only whether
//! they exist; the canaries are this test's own children and it ends them itself.
//! Every source is a private temporary directory of invented content.
use orch_infra::import::preflight::{Options, SystemProbe, preflight};
use serde_json::json;
use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

struct Canary(Child);
impl Drop for Canary {
    fn drop(&mut self) {
        // Cleanup of the test's own children; the importer never signals.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = self.0.wait();
    }
}

struct Group(i32);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

fn state_with_run(pid: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let orch = dir.path().join(".orchestry");
    fs::create_dir(&orch).unwrap();
    let entry = json!({"run_id": "r1", "agent_id": "agt_a", "task_id": "tsk_r1", "pid": pid,
        "started_at": "2025-01-01T00:00:00.000Z", "last_event_at": "2025-01-01T00:00:00.000Z"});
    fs::write(
        orch.join("state.json"),
        json!({"version": 1, "running": {"tsk_r1": entry}}).to_string(),
    )
    .unwrap();
    dir
}

async fn check(dir: &tempfile::TempDir) -> orch_infra::import::preflight::Report {
    preflight(
        dir.path().to_owned(),
        Options::default(),
        Arc::new(SystemProbe),
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_while_a_recorded_process_exists() {
    let mut live = Canary(
        Command::new("sleep")
            .arg("60")
            .process_group(0)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let dir = state_with_run(live.0.id());
    let report = check(&dir).await;
    let found: Vec<_> = report
        .refusals
        .listed
        .iter()
        .map(|f| (f.path.as_str(), f.rule))
        .collect();
    assert_eq!(found, [("state.json", "in_flight_live")]);
    let detail = &report.refusals.listed[0].detail;
    assert!(detail.contains("run r1; task tsk_r1"), "{detail}");
    assert!(detail.contains(&format!("pid {}", live.0.id())), "{detail}");
    assert!(
        detail.contains("the process and its process group were found"),
        "{detail}"
    );
    assert_eq!(report.counts.running, 1);
    // Nothing was sent to it: still running after the check.
    assert!(live.0.try_wait().unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_while_only_the_recorded_process_group_has_a_member() {
    // The leader exits and is reaped; its child stays in the leader's group.
    let mut leader = Command::new("sh")
        .args(["-c", "sleep 60 & exit 0"])
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let pid = leader.id();
    leader.wait().unwrap();
    let _group = Group(pid as i32);
    let dir = state_with_run(pid);
    let report = check(&dir).await;
    assert_eq!(report.refusals.total(), 1, "{:?}", report.refusals);
    let finding = &report.refusals.listed[0];
    assert_eq!(finding.rule, "in_flight_live");
    assert!(
        finding.detail.contains("only the process group was found"),
        "{}",
        finding.detail
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_pid_that_names_nothing_passes() {
    // A pid that is gone and whose group is empty: nothing to refuse, one run to close.
    let mut gone = Command::new("true").process_group(0).spawn().unwrap();
    let pid = gone.id();
    gone.wait().unwrap();
    let dir = state_with_run(pid);
    let report = check(&dir).await;
    assert!(!report.refused(), "{:?}", report.refusals);
    assert_eq!(report.counts.in_flight_to_close, 1);
}
