//! gate-resilience F1 / lead ruling 19 — a signal to a standalone `orch run`
//! must not orphan the OpenCode effort probe's process group.
//!
//! The probe runs in its own process group, its deadline and kill guard live
//! in orch's runtime, and its pid is deliberately not in the model-pid slot
//! startup recovery reads. Before the fix `orch run <task>` and `run --all`
//! had no termination handler: SIGINT killed orch at once and the probe and
//! its descendant kept running, also after a restart.
//!
//! Each scenario drives the real `orch` binary in a throwaway project, home
//! and `PATH` whose `opencode` is a fake that records its pid, forks a
//! sleeping descendant and waits (so the probe never answers). SIGINT is sent
//! once that descendant exists. The contract:
//!
//! * standalone `run <task>` and `run --all` exit `130` within the probe
//!   deadline plus the kill grace, with **zero** live probe or descendant
//!   processes, and leave the task recoverable (not `in_progress`), and a
//!   following `serve --once` leaves nothing alive either;
//! * a signal during the model run of a standalone `run <task>` stops that
//!   run and settles its records;
//! * `run --watch` and `serve` keep their behaviour: exit 0, nothing alive.

mod token_eco_support;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use token_eco_support::{Sandbox, new_reader};

const MODEL: &str = "openrouter/anthropic/claude-opus-5.5";
/// The probe deadline (2 s) plus `DEFAULT_KILL_GRACE` (10 s).
const EXIT_BOUND: Duration = Duration::from_secs(12);

/// A project with one OpenCode agent at a new effort spelling (so its
/// dispatch always runs the wire probe) and one task for it. `opencode` is a
/// fake that never answers the probe.
struct Rig {
    sb: Sandbox,
    task: String,
    marks: tempfile::TempDir,
}

impl Rig {
    fn opencode() -> Rig {
        let new = new_reader();
        let sb = Sandbox::new(&new);
        let marks = tempfile::tempdir().unwrap();
        let agent = sb.seed_agent(&new, "Oc", "opencode", Some(MODEL), Some("xhigh"));
        let rig = Rig::with_task(sb, marks, &agent);
        // Every invocation (the `--version` probe first) records its pid,
        // forks a descendant that holds the pipes, and waits.
        write_script(
            &rig.sb.bin.path().join("opencode"),
            &hanging_script(rig.marks.path(), "probe"),
        );
        rig
    }

    /// A shell agent whose model run is the hanging script.
    fn shell() -> Rig {
        let new = new_reader();
        let sb = Sandbox::new(&new);
        let marks = tempfile::tempdir().unwrap();
        let script = marks.path().join("model.sh");
        write_script(&script, &hanging_script(marks.path(), "model"));
        let out = sb.run(
            &new,
            &[
                "agent",
                "add",
                "Sh",
                "--adapter",
                "shell",
                "--command",
                &format!("exec '{}'", script.display()),
                "--workspace-mode",
                "shared",
                "--quiet",
            ],
            None,
        );
        assert!(out.ok(), "agent add: {}", out.all());
        let agent = out.stdout.trim().to_owned();
        Rig::with_task(sb, marks, &agent)
    }

    fn with_task(sb: Sandbox, marks: tempfile::TempDir, agent: &str) -> Rig {
        let out = sb.run(
            &new_reader(),
            &["task", "add", "signal", "--assignee", agent, "--quiet"],
            None,
        );
        assert!(out.ok(), "task add: {}", out.all());
        Rig {
            sb,
            task: out.stdout.trim().to_owned(),
            marks,
        }
    }

    fn spawn(&self, args: &[&str]) -> Running {
        let mut cmd = self.sb.command(&new_reader());
        let child = cmd
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn orch");
        Running::new(child)
    }

    /// Wait for the hanging script's descendant to exist: `<what>.desc.pid`.
    fn wait_for(&self, what: &str, running: &mut Running) -> (i32, i32) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let (Some(leader), Some(desc)) = (
                read_pid(&self.marks.path().join(format!("{what}.pid"))),
                read_pid(&self.marks.path().join(format!("{what}.desc.pid"))),
            ) {
                return (leader, desc);
            }
            if let Some(status) = running.child.try_wait().unwrap() {
                panic!(
                    "orch exited ({status}) before the {what} started: {}",
                    running.output()
                );
            }
            assert!(Instant::now() < deadline, "the {what} never started");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.sb.run(&new_reader(), args, None);
        assert!(out.ok(), "{args:?}: {}", out.all());
        serde_json::from_str(&out.stdout).expect("json")
    }

    fn task_status(&self) -> String {
        self.json(&["--json", "task", "show", &self.task])["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn run_statuses(&self) -> Vec<String> {
        self.json(&["--json", "logs", "--task", &self.task])
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["status"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Every pid the fakes ever recorded, for the final sweep.
    fn recorded_pids(&self) -> Vec<i32> {
        std::fs::read_dir(self.marks.path())
            .unwrap()
            .flat_map(|e| {
                let path = e.unwrap().path();
                if path.extension().is_some_and(|x| x == "pid") {
                    read_pids(&path)
                } else {
                    Vec::new()
                }
            })
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Never leave a fake behind, whatever the test concluded: each
        // recorded pid, and the process group each leader heads.
        for pid in self.recorded_pids() {
            let pid = nix::unistd::Pid::from_raw(pid);
            let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

/// The script every hanging fake runs. `<what>.pid` is the leader (the probe
/// or model child itself), `<what>.desc.pid` a descendant in its group; a
/// second invocation (a restart re-dispatching the task) appends to `.all`.
fn hanging_script(marks: &Path, what: &str) -> String {
    let m = marks.display();
    format!(
        "#!/bin/sh\n\
         if [ -e '{m}/{what}.pid' ]; then echo $$ >> '{m}/{what}.again.pid'; else echo $$ > '{m}/{what}.pid.tmp'; fi\n\
         sleep 60 &\n\
         if [ -e '{m}/{what}.pid' ]; then echo $! >> '{m}/{what}.again.pid'; else echo $! > '{m}/{what}.desc.pid'; mv '{m}/{what}.pid.tmp' '{m}/{what}.pid'; fi\n\
         wait\n"
    )
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The first pid in a marker file (a file may hold several lines).
fn read_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn read_pids(path: &Path) -> Vec<i32> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

fn alive(pid: i32) -> bool {
    // A zombie still answers signal 0; `ps` tells the two apart.
    std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| {
            let stat = String::from_utf8_lossy(&o.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        })
        .unwrap_or(false)
}

/// Every pid is gone (or a zombie awaiting its new parent) within `within`.
fn assert_all_dead(pids: &[i32], within: Duration, what: &str) {
    let deadline = Instant::now() + within;
    loop {
        let live: Vec<i32> = pids.iter().copied().filter(|p| alive(*p)).collect();
        if live.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: still alive: {live:?} of {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn sigint(child: &Child) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("send SIGINT");
}

/// A running `orch` whose pipes are drained on threads, so it can never block
/// on a full pipe.
struct Running {
    child: Child,
    readers: Vec<std::thread::JoinHandle<String>>,
}

impl Running {
    fn new(mut child: Child) -> Running {
        let mut readers = Vec::new();
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            readers.push(std::thread::spawn(move || {
                let mut pipe = pipe;
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                text
            }));
        }
        Running { child, readers }
    }

    /// Wait for exit within `bound`; returns `(exit code, elapsed)`. A
    /// signal death has no code and reports `-signal`.
    fn wait(&mut self, bound: Duration) -> (i32, Duration) {
        use std::os::unix::process::ExitStatusExt;
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let code = status
                    .code()
                    .unwrap_or_else(|| -status.signal().unwrap_or(0));
                return (code, started.elapsed());
            }
            if started.elapsed() > bound {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("orch did not exit within {bound:?}: {}", self.output());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn output(&mut self) -> String {
        if self.child.try_wait().unwrap().is_none() {
            return "(still running)".into();
        }
        self.readers
            .drain(..)
            .map(|r| r.join().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// SIGINT during the probe of a standalone command; returns the probe pids.
fn interrupt_during_probe(rig: &Rig, args: &[&str]) -> (Vec<i32>, String) {
    let mut orch = rig.spawn(args);
    let (probe, desc) = rig.wait_for("probe", &mut orch);
    sigint(&orch.child);
    let (code, took) = orch.wait(EXIT_BOUND);
    let output = orch.output();
    assert_eq!(code, 130, "{args:?} must exit 128+SIGINT: {output}");
    assert!(took < EXIT_BOUND, "{args:?} took {took:?}");
    (vec![probe, desc], output)
}

fn assert_recoverable(rig: &Rig) {
    let status = rig.task_status();
    assert!(
        ["retrying", "failed", "todo"].contains(&status.as_str()),
        "the task must be recoverable, not {status}"
    );
    let runs = rig.run_statuses();
    assert!(
        runs.iter().all(|s| s != "running" && s != "preparing"),
        "no run may stay in flight: {runs:?}"
    );
}

#[test]
fn sigint_during_the_probe_of_a_standalone_run_leaves_no_probe_process_alive_even_after_restart() {
    let rig = Rig::opencode();
    let (pids, output) = interrupt_during_probe(&rig, &["run", &rig.task]);
    // Already dead when orch has exited: nothing else is left to kill them.
    assert_all_dead(
        &pids,
        Duration::from_secs(2),
        &format!("run <task>: {output}"),
    );
    assert_recoverable(&rig);
    assert!(
        output.contains("Interrupted by SIGINT"),
        "the interrupt must be reported: {output}"
    );

    // A restart finds nothing alive, and its own (bounded) probe of the
    // re-dispatched task is cleaned up too.
    let restart = rig.sb.run(&new_reader(), &["serve", "--once"], None);
    let again = read_pids(&rig.marks.path().join("probe.again.pid"));
    let mut all = pids.clone();
    all.extend(&again);
    assert_all_dead(
        &all,
        Duration::from_secs(2),
        &format!("after serve --once: {}", restart.all()),
    );
}

#[test]
fn sigint_during_the_probe_of_run_all_leaves_no_probe_process_alive() {
    let rig = Rig::opencode();
    let (pids, output) = interrupt_during_probe(&rig, &["run", "--all"]);
    assert_all_dead(
        &pids,
        Duration::from_secs(2),
        &format!("run --all: {output}"),
    );
    assert_recoverable(&rig);
}

#[test]
fn sigint_during_the_model_run_of_a_standalone_run_stops_it_and_settles_the_records() {
    let rig = Rig::shell();
    let mut orch = rig.spawn(&["run", &rig.task]);
    let (model, desc) = rig.wait_for("model", &mut orch);
    sigint(&orch.child);
    let (code, took) = orch.wait(EXIT_BOUND);
    let output = orch.output();
    assert_eq!(code, 130, "{output}");
    assert!(took < EXIT_BOUND, "took {took:?}");
    assert_all_dead(&[model, desc], Duration::from_secs(2), &output);
    assert_recoverable(&rig);
    assert_eq!(rig.run_statuses(), ["cancelled"], "{output}");
}

/// The watch-mode paths are untouched: they keep their own signal arms.
fn watch_mode_regression(args: &[&str]) {
    let rig = Rig::opencode();
    let mut orch = rig.spawn(args);
    let (probe, desc) = rig.wait_for("probe", &mut orch);
    sigint(&orch.child);
    let (code, _) = orch.wait(EXIT_BOUND + Duration::from_secs(10));
    let output = orch.output();
    assert_eq!(code, 0, "{args:?}: {output}");
    assert_all_dead(&[probe, desc], Duration::from_secs(2), &output);
    let status = rig.task_status();
    assert_ne!(status, "in_progress", "{args:?}: {output}");
}

#[test]
fn run_watch_and_serve_still_stop_cleanly_on_sigint_during_the_probe() {
    let logs = tempfile::tempdir().unwrap();
    let log: PathBuf = logs.path().join("serve.log");
    let serve = ["serve", "--log-file", log.to_str().unwrap()];
    std::thread::scope(|scope| {
        let watch = scope.spawn(|| watch_mode_regression(&["run", "--watch"]));
        let serve = scope.spawn(|| watch_mode_regression(&serve));
        watch.join().expect("run --watch regression");
        serve.join().expect("serve regression");
    });
}
