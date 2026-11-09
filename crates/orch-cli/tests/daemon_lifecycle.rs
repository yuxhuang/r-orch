//! Binary-level daemon lifecycle (T1b; spec R5, R15, R31, R35, R37; design
//! D3/D12/D13). Every test starts a real `orch daemon start` child on an
//! isolated short socket and judges it by exit status, files and the process
//! table; protocol detail belongs to the in-process harness tests.
//!
//! PR 1 is developmental: `orch daemon *` needs `ORCH_DAEMON_DEVELOPMENTAL=1`,
//! which `Sandbox::orch` puts in the child env only.

mod daemon_support;

use daemon_support::*;
use nix::sys::signal::Signal;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration;

fn has_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
        .output()
        .is_ok_and(|out| out.status.success())
}

// ── two explicit starts race ───────────────────────────────────────────────

/// Release two `orch daemon start` children at the same instant. Returns
/// `(winner, loser)`; the loser has exited with the lock-conflict code.
fn race_two_starts(sandbox: &Sandbox) -> (DaemonChild, DaemonChild) {
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|tag| {
            let cmd = sandbox.start_cmd(&[]);
            let err = sandbox.short_dir().join(format!("race-{tag}.err"));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                DaemonChild::spawn(cmd, err)
            })
        })
        .collect();
    let mut winner = None;
    let mut loser = None;
    for handle in handles {
        let mut child = handle.join().unwrap();
        match child.outcome(READY) {
            Outcome::Ready(_) => {
                assert!(winner.is_none(), "two daemons reached READY");
                winner = Some(child);
            }
            Outcome::Exited(status) => {
                assert_eq!(status.code(), Some(4), "loser: {}", child.stderr());
                assert!(loser.is_none(), "both starts failed");
                loser = Some(child);
            }
        }
    }
    (
        winner.expect("no start won the race"),
        loser.expect("both starts reported READY"),
    )
}

#[test]
fn two_explicit_starts_one_owner() {
    for round in 0..3 {
        let sandbox = Sandbox::new();
        sandbox.init_project();
        let (winner, loser) = race_two_starts(&sandbox);
        assert!(
            loser
                .stderr()
                .contains(&format!("orch daemon already running (PID {})", winner.pid)),
            "round {round}: {}",
            loser.stderr()
        );
        // One listener, owned by the winner; the guard and discovery agree.
        let status = sandbox.status().unwrap();
        assert_eq!(status.pid, winner.pid, "round {round}");
        assert_eq!(sandbox.daemon_json().unwrap()["pid"], winner.pid);
        assert_eq!(
            fs::read_to_string(sandbox.state_dir().join(".daemon-owner.lock"))
                .unwrap()
                .trim(),
            winner.pid.to_string()
        );
        assert!(!is_alive(loser.pid), "the loser must have exited");
    }
}

#[test]
fn two_explicit_starts_over_a_stale_socket_still_one_owner() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    // A crashed predecessor left a socket inode behind: bound, then dropped.
    drop(UnixListener::bind(sandbox.socket()).unwrap());
    fs::set_permissions(
        sandbox.socket(),
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let (winner, loser) = race_two_starts(&sandbox);
    assert!(
        loser
            .stderr()
            .contains(&format!("already running (PID {})", winner.pid)),
        "{}",
        loser.stderr()
    );
    assert_eq!(sandbox.status().unwrap().pid, winner.pid);
}

#[test]
fn second_start_while_running_names_the_owner_and_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let owner = start_ready(&sandbox, &[], "owner");
    let before = (
        fs::metadata(sandbox.socket()).unwrap().ino(),
        fs::read(sandbox.state_dir().join("daemon.json")).unwrap(),
    );
    let out = sandbox.run(&["daemon", "start"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&format!("orch daemon already running (PID {})", owner.pid))
    );
    let after = (
        fs::metadata(sandbox.socket()).unwrap().ino(),
        fs::read(sandbox.state_dir().join("daemon.json")).unwrap(),
    );
    assert_eq!(before, after, "the loser touched the owner's endpoint");
    assert_eq!(sandbox.status().unwrap().pid, owner.pid);
}

// ── ownership is not time-based ───────────────────────────────────────────

fn touch_ancient(path: &std::path::Path) {
    let out = Command::new("/usr/bin/touch")
        .args(["-t", "200001010000"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(fs::metadata(path).unwrap().mtime() < 1_000_000_000);
}

#[test]
fn live_owner_old_mtime_not_evicted() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let owner = start_ready(&sandbox, &[], "owner");
    let lock = sandbox.state_dir().join("orchestry.lock");
    let guard = sandbox.state_dir().join(".daemon-owner.lock");
    touch_ancient(&lock);
    touch_ancient(&guard);
    let inode = fs::metadata(&lock).unwrap().ino();

    let out = sandbox.run(&["daemon", "start"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains(&format!("already running (PID {})", owner.pid)));
    assert_eq!(fs::metadata(&lock).unwrap().ino(), inode, "lock replaced");
    assert_eq!(
        fs::read_to_string(&lock).unwrap().trim(),
        owner.pid.to_string()
    );
    assert_eq!(sandbox.status().unwrap().pid, owner.pid);
}

#[test]
fn live_legacy_pid_with_ancient_mtime_blocks_start_and_is_not_removed() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    // A live non-daemon process (a v0.13 `run --watch` stand-in) holds the
    // legacy PID lock, long stale by mtime.
    let mut holder = Command::new("/bin/sleep").arg("120").spawn().unwrap();
    let _reap = KillOnDrop(holder.id());
    let lock = sandbox.state_dir().join("orchestry.lock");
    fs::write(&lock, holder.id().to_string()).unwrap();
    touch_ancient(&lock);

    let out = sandbox.run(&["daemon", "start"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains(&format!("held by live PID {}", holder.id())),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&lock).unwrap(), holder.id().to_string());
    assert!(holder.try_wait().unwrap().is_none(), "holder was killed");
    assert!(!sandbox.socket().exists());
    assert!(!sandbox.state_dir().join("daemon.json").exists());
    let _ = holder.kill();
    let _ = holder.wait();
}

#[test]
fn dead_legacy_pid_does_not_block_start_and_is_replaced() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    let lock = sandbox.state_dir().join("orchestry.lock");
    fs::write(&lock, dead.to_string()).unwrap();
    let daemon = start_ready(&sandbox, &[], "owner");
    assert_eq!(
        fs::read_to_string(&lock).unwrap().trim(),
        daemon.pid.to_string()
    );
}

// ── signals ────────────────────────────────────────────────────────────────

/// SIGTERM, SIGINT and SIGHUP all stop a foreground daemon cleanly: the
/// running agent is killed, its run is cancelled, the retry path owns the
/// task, only the daemon's own endpoint files go and the project can be
/// started again at once (guard and PID lock released).
fn signals_reconcile_and_release(sig: Signal) {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (_agent, task) = sandbox.seed_sleeper();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "owner");
    let agent_pid = wait_for(READY, "the dispatched agent", || sandbox.running_pid(&task));
    let _reap = KillOnDrop(agent_pid);
    let run_id = sandbox.running_run_id(&task).unwrap();
    assert!(is_alive(agent_pid));

    daemon.signal(sig);
    let status = daemon.wait_exit(READY);
    assert_eq!(status.code(), Some(0), "{sig:?}: {}", daemon.stderr());

    wait_dead(agent_pid, "the agent stand-in to be killed");
    assert_eq!(sandbox.run_status(&run_id).as_deref(), Some("cancelled"));
    assert!(
        sandbox.state_json().unwrap()["running"]
            .as_object()
            .unwrap()
            .is_empty(),
        "the run is still recorded as running"
    );
    assert!(!sandbox.socket().exists(), "own socket left behind");
    assert!(!sandbox.state_dir().join("daemon.json").exists());
    assert!(!sandbox.state_dir().join("orchestry.lock").exists());
    // The guard inode stays but is no longer locked: a successor starts.
    assert!(sandbox.state_dir().join(".daemon-owner.lock").exists());
    let successor = start_ready(&sandbox, &[], "successor");
    assert_ne!(successor.pid, daemon.pid);
}

#[test]
fn signals_reconcile_and_release_sigterm() {
    signals_reconcile_and_release(Signal::SIGTERM);
}
#[test]
fn signals_reconcile_and_release_sigint() {
    signals_reconcile_and_release(Signal::SIGINT);
}
#[test]
fn foreground_sighup_stops() {
    signals_reconcile_and_release(Signal::SIGHUP);
}

#[test]
fn daemon_stop_command_ends_the_process_and_releases_the_project() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (_agent, task) = sandbox.seed_sleeper();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "owner");
    let agent_pid = wait_for(READY, "the dispatched agent", || sandbox.running_pid(&task));
    let _reap = KillOnDrop(agent_pid);

    let out = sandbox.run(&["daemon", "stop"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        daemon.wait_exit(READY).code(),
        Some(0),
        "{}",
        daemon.stderr()
    );
    wait_dead(agent_pid, "the agent stand-in");
    assert!(!sandbox.socket().exists());
    assert!(!sandbox.state_dir().join("daemon.json").exists());
}

// ── crash → NoDaemon → explicit restart ───────────────────────────────────

#[test]
fn crash_requires_explicit_start() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (_agent, task) = sandbox.seed_sleeper();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "first");
    let agent_pid = wait_for(READY, "the dispatched agent", || sandbox.running_pid(&task));
    let _reap = KillOnDrop(agent_pid);
    let first_pid = daemon.pid;

    daemon.crash();

    // SIGKILL runs no cleanup: the evidence of the crash is all still there.
    let socket_ino = fs::metadata(sandbox.socket()).unwrap().ino();
    let discovery = fs::read(sandbox.state_dir().join("daemon.json")).unwrap();
    let state = fs::read(sandbox.state_dir().join("state.json")).unwrap();

    // Every control verb reports "no daemon" (exit 7) and repairs nothing.
    for args in [
        vec!["daemon", "status"],
        vec!["daemon", "stop"],
        vec!["daemon", "dispatch", "on"],
    ] {
        let mut cmd = sandbox.orch();
        cmd.args(&args);
        let out = output_with_deadline(cmd, CALL);
        assert_eq!(
            out.status.code(),
            Some(7),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains("No orch daemon is running for"),
            "{}",
            text(&out.stderr)
        );
        assert!(text(&out.stderr).contains("Start it with: orch daemon start"));
    }
    assert_eq!(fs::metadata(sandbox.socket()).unwrap().ino(), socket_ino);
    assert_eq!(
        fs::read(sandbox.state_dir().join("daemon.json")).unwrap(),
        discovery,
        "a client rewrote daemon.json"
    );
    assert_eq!(
        fs::read(sandbox.state_dir().join("state.json")).unwrap(),
        state,
        "a client changed daemon-owned state"
    );
    // Nothing restarted it: the recorded owner is still the dead one.
    assert_eq!(sandbox.daemon_json().unwrap()["pid"], first_pid);
    assert!(!is_alive(first_pid));

    // Explicit restart: replaces the stale endpoint and recovers per D13. The
    // crashed daemon's agent is still alive, so its recorded entry is kept
    // (live local entries retain their recovery reservation).
    let mut second = start_ready(&sandbox, &[], "second");
    assert_ne!(second.pid, first_pid);
    assert_eq!(sandbox.daemon_json().unwrap()["pid"], second.pid);
    assert!(is_alive(agent_pid), "startup killed a live recorded agent");
    assert_eq!(sandbox.running_pid(&task), Some(agent_pid));
    let runs = state_run_ids(&sandbox);
    assert_eq!(runs.len(), 1);
    assert_eq!(sandbox.run_status(&runs[0]).as_deref(), Some("running"));
    // The new owner now owns that agent: an explicit stop reaps it.
    let out = sandbox.run(&["daemon", "stop"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(second.wait_exit(READY).code(), Some(0));
    wait_dead(agent_pid, "the adopted agent stand-in after stop");
    assert_eq!(sandbox.run_status(&runs[0]).as_deref(), Some("cancelled"));
}

#[test]
fn restart_after_crash_cancels_runs_whose_agent_died_with_the_daemon() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (_agent, task) = sandbox.seed_sleeper();
    let mut daemon = start_ready(&sandbox, &["--dispatch", "on"], "first");
    let agent_pid = wait_for(READY, "the dispatched agent", || sandbox.running_pid(&task));
    let run_id = sandbox.running_run_id(&task).unwrap();
    daemon.crash();
    force_kill(agent_pid);
    wait_dead(agent_pid, "the agent stand-in");

    // Default OFF after a restart: recovery runs, nothing is re-dispatched.
    let second = start_ready(&sandbox, &[], "second");
    assert!(!sandbox.status().unwrap().dispatch.dispatch_on);
    assert_eq!(sandbox.run_status(&run_id).as_deref(), Some("cancelled"));
    assert!(
        sandbox.state_json().unwrap()["running"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(is_alive(second.pid));
}

fn state_run_ids(sandbox: &Sandbox) -> Vec<String> {
    fs::read_dir(sandbox.state_dir().join("runs"))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            Some(name.strip_suffix(".json")?.to_owned())
        })
        .collect()
}

// ── stale / hostile socket paths ──────────────────────────────────────────

#[test]
fn stale_socket_is_replaced_on_start() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    drop(UnixListener::bind(sandbox.socket()).unwrap());
    assert!(sandbox.socket().exists());
    let daemon = start_ready(&sandbox, &[], "owner");
    assert_eq!(sandbox.status().unwrap().pid, daemon.pid);
    let mode = fs::metadata(sandbox.socket()).unwrap().mode() & 0o777;
    assert_eq!(mode, 0o600, "the replacement socket must be owner-only");
}

#[test]
fn start_refuses_a_regular_file_at_the_socket_path_and_leaves_it() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    fs::write(sandbox.socket(), "precious").unwrap();
    let out = sandbox.run(&["daemon", "start"]);
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(fs::read_to_string(sandbox.socket()).unwrap(), "precious");
    assert!(!sandbox.state_dir().join("daemon.json").exists());
}

#[test]
fn start_refuses_a_symlink_at_the_socket_path_and_leaves_it() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let target = sandbox.short_dir().join("target");
    fs::write(&target, "target").unwrap();
    std::os::unix::fs::symlink(&target, sandbox.socket()).unwrap();
    let out = sandbox.run(&["daemon", "start"]);
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("listening"));
    assert!(
        fs::symlink_metadata(sandbox.socket())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "target");
}

#[test]
fn start_refuses_a_live_foreign_listener_at_the_socket_path_and_leaves_it() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let listener = UnixListener::bind(sandbox.socket()).unwrap();
    let ino = fs::metadata(sandbox.socket()).unwrap().ino();
    let out = sandbox.run(&["daemon", "start"]);
    assert_ne!(out.status.code(), Some(0), "{}", text(&out.stdout));
    assert!(!text(&out.stdout).contains("listening"));
    assert_eq!(fs::metadata(sandbox.socket()).unwrap().ino(), ino);
    drop(listener);
}

// ── agent context ──────────────────────────────────────────────────────────

#[test]
fn agent_context_cannot_start_daemon() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    // Every start spelling PR 1 has: bare, detached, dispatch-on.
    for identity in ["ORCH_TASK_ID", "ORCH_AGENT_ID"] {
        for extra in [vec![], vec!["--detach"], vec!["--dispatch", "on"]] {
            let mut cmd = sandbox.start_cmd(&extra);
            cmd.env(identity, "t-1");
            let out = output_with_deadline(cmd, CALL);
            assert_eq!(out.status.code(), Some(2), "{identity} {extra:?}");
            assert!(
                text(&out.stderr).contains(orch_proto::error::START_REFUSED_IN_AGENT_CONTEXT),
                "{}",
                text(&out.stderr)
            );
        }
    }
    assert!(!sandbox.socket().exists());
    assert!(!sandbox.state_dir().join("daemon.json").exists());
    assert!(!sandbox.state_dir().join(".daemon-owner.lock").exists());
    assert!(!sandbox.state_dir().join("orchestry.lock").exists());
}

#[test]
fn agent_context_with_from_agent_override_starts() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut cmd = sandbox.start_cmd(&["--from-agent"]);
    cmd.env("ORCH_TASK_ID", "t-1");
    let mut daemon = DaemonChild::spawn(cmd, sandbox.short_dir().join("agent.err"));
    daemon.expect_ready(&sandbox.socket());
}

// ── detach ─────────────────────────────────────────────────────────────────

/// The detached daemon's pid, from the discovery file; kills it on drop.
struct DetachedOwner(Sandbox, u32);
impl Drop for DetachedOwner {
    fn drop(&mut self) {
        force_kill(self.1);
    }
}

#[test]
fn detach_reports_ready_then_runs_in_its_own_session() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let out = sandbox.run(&["daemon", "start", "--detach"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains(&format!("listening {}", sandbox.socket().display())),
        "{}",
        text(&out.stdout)
    );
    let pid = u32::try_from(sandbox.daemon_json().unwrap()["pid"].as_u64().unwrap()).unwrap();
    let owner = DetachedOwner(sandbox, pid);
    let sandbox = &owner.0;
    assert_eq!(sandbox.status().unwrap().pid, pid);
    let table = process_table();
    let me = table.iter().find(|p| p.pid == pid).unwrap();
    assert_eq!(me.pgid, pid, "the detached daemon leads its own group");
    assert_ne!(me.ppid, std::process::id(), "reparented away from the test");
    let log = sandbox.state_dir().join("daemon.log");
    assert_eq!(fs::metadata(&log).unwrap().mode() & 0o777, 0o600);

    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    wait_dead(pid, "the detached daemon");
}

#[test]
fn detach_failure_is_the_parents_failure() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let owner = start_ready(&sandbox, &[], "owner");
    let out = sandbox.run(&["daemon", "start", "--detach"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    // The child's own refusal is in the private log; the parent may also
    // echo its tail.
    let log = fs::read_to_string(sandbox.state_dir().join("daemon.log")).unwrap_or_default();
    let needle = format!("already running (PID {})", owner.pid);
    assert!(
        log.contains(&needle) || text(&out.stderr).contains(&needle),
        "stderr: {} log: {log}",
        text(&out.stderr)
    );
    assert_eq!(sandbox.status().unwrap().pid, owner.pid);
}

// ── terminal hang-up (tmux) ───────────────────────────────────────────────

struct Tmux {
    sandbox: Sandbox,
    name: String,
}
impl Tmux {
    fn new(sandbox: Sandbox) -> Tmux {
        let name = format!("odl{}", std::process::id());
        Tmux { sandbox, name }
    }
    fn cmd(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin")
            .env("TMUX_TMPDIR", self.sandbox.short_dir())
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .args(["-L", &self.name, "-f", "/dev/null"]);
        cmd
    }
    fn run(&self, args: &[&str]) {
        let out = output_with_deadline(
            {
                let mut cmd = self.cmd();
                cmd.args(args);
                cmd
            },
            CALL,
        );
        assert!(out.status.success(), "tmux {args:?}: {}", text(&out.stderr));
    }
    /// A pane running `script` with the sandbox's child environment.
    fn spawn_pane(&self, script: &str) {
        let wrapper = self.sandbox.short_dir().join("pane.sh");
        let exe = env!("CARGO_BIN_EXE_orch");
        let root = self.sandbox.root();
        let script = script.replace("orch", &format!("'{exe}'"));
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\ncd '{root}'\nexec env -i HOME='{root}/home' PATH=/usr/bin:/bin NO_COLOR=1 \
                 ORCHESTRY_GLOBAL_CONFIG_PATH='{root}/global.yml' ORCH_PROJECT_ROOT='{root}' \
                 ORCH_SOCKET='{sock}' ORCH_DAEMON_DEVELOPMENTAL=1 /bin/sh -c \"{script}\"\n",
                root = root.display(),
                sock = self.sandbox.socket().display(),
            ),
        )
        .unwrap();
        fs::set_permissions(
            &wrapper,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        self.run(&[
            "new-session",
            "-d",
            "-s",
            "t",
            "-x",
            "100",
            "-y",
            "30",
            wrapper.to_str().unwrap(),
        ]);
    }
}
impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.cmd().arg("kill-server").output();
    }
}

#[test]
fn foreground_daemon_in_a_pane_stops_cleanly_when_the_pane_is_killed() {
    if !has_tmux() {
        eprintln!("SKIP: tmux is not installed on this runner");
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (_agent, task) = sandbox.seed_sleeper();
    let tmux = Tmux::new(sandbox);
    tmux.spawn_pane("exec orch daemon start --dispatch on");
    let sandbox = &tmux.sandbox;
    let daemon_pid = wait_for(READY, "the pane's daemon", || {
        sandbox.status().ok().map(|s| s.pid)
    });
    let agent_pid = wait_for(READY, "the dispatched agent", || sandbox.running_pid(&task));
    let _reap = KillOnDrop(agent_pid);
    let _kill_daemon = KillOnDrop(daemon_pid);

    tmux.run(&["kill-pane", "-t", "t"]);

    wait_dead(daemon_pid, "the foreground daemon to stop on hang-up");
    wait_dead(agent_pid, "the agent stand-in");
    assert!(!sandbox.socket().exists(), "hang-up left a socket behind");
    assert!(!sandbox.state_dir().join("daemon.json").exists());
    assert!(!sandbox.state_dir().join("orchestry.lock").exists());
}

#[test]
fn detached_survives_tmux_kill_pane() {
    if !has_tmux() {
        eprintln!("SKIP: tmux is not installed on this runner");
        return;
    }
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let tmux = Tmux::new(sandbox);
    // The parent exits once the child is READY; `sleep` keeps the pane open
    // so kill-pane really hangs up a live terminal.
    tmux.spawn_pane("orch daemon start --detach; exec sleep 120");
    let sandbox = &tmux.sandbox;
    let pid = wait_for(READY, "the detached daemon", || {
        sandbox.status().ok().map(|s| s.pid)
    });
    let _kill = KillOnDrop(pid);

    tmux.run(&["kill-pane", "-t", "t"]);

    // Hang-up of the terminal did not reach the daemon's own session.
    assert!(is_alive(pid));
    assert_eq!(sandbox.status().unwrap().pid, pid);
    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    wait_dead(pid, "the detached daemon after stop");
}

// ── things this layer cannot do ───────────────────────────────────────────

#[test]
#[ignore = "needs two orch executables of different build versions; PR 1 builds one. \
            Covered with a skew-advertising harness daemon in daemon_cmd_control.rs \
            (protocol_skew_names_both_versions_and_frozen_stop_still_works)"]
fn two_executable_stale_dev_instance() {}

#[test]
#[ignore = "SIGSTOP of the owner for >60 s is too slow for the default suite; the same \
            invariant (old mtime never evicts a live owner) is live_owner_old_mtime_not_evicted"]
fn live_owner_suspended_over_sixty_seconds_not_evicted() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let owner = start_ready(&sandbox, &[], "owner");
    signal(owner.pid, Signal::SIGSTOP);
    std::thread::sleep(Duration::from_secs(70));
    let out = sandbox.run(&["daemon", "start"]);
    signal(owner.pid, Signal::SIGCONT);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
}
