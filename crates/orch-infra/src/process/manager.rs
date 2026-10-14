//! [`SystemProcessManager`] — ported from `ProcessManager`
//! (`src/infrastructure/process/process-manager.ts:22-81`).
//!
//! **Why this is hand-rolled rather than built on `process-wrap`**
//! (ARCHITECTURE §4.8 exemption (b)): the parity contract needs a *pid-keyed*
//! API — "there is no registry of spawned processes, the PID in
//! `state.running` is the only handle" (PARITY.md §8.4) — so `kill`,
//! `is_alive` and `kill_with_grace` cannot go through a child handle at all,
//! and must keep the TS group-first/fallback/swallow-errors shape.
//! `process-wrap` only covers the remaining piece, the process group, and its
//! `ProcessGroupChild::wait()` additionally waits for the *whole group* to be
//! reaped, which would delay our `ExitSignal` past node's `close` event
//! whenever an agent CLI leaves a background child alive. The group itself
//! comes from `Command::process_group(0)`, which is a safe std API, so
//! nothing here needs `unsafe` (the crate forbids it).

use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nix::errno::Errno;
use nix::unistd::Pid;
use orch_core::error::{Error, Result};
use orch_core::process::{
    ExitOutcome, ExitSignal, KILL_POLL_INTERVAL, ProcessManager, Signal, SpawnOptions,
    SpawnedChild, StdioMode,
};

/// The real process manager. Stateless and cheap to clone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcessManager;

impl SystemProcessManager {
    pub fn new() -> Self {
        SystemProcessManager
    }
}

/// The pid as something safe to hand to `kill(2)`, or `None` if it is not a
/// single real process.
///
/// Two values must never reach `kill`, and `state.running` is populated from
/// JSON on disk, so both are reachable from a stale or hand-edited file:
///
/// - **0** means "every process in the caller's own group" — the orchestrator
///   would SIGTERM itself and every agent it started;
/// - anything above [`i32::MAX`] wraps to a negative `i32`, and a negative
///   target is a *process group*; `u32::MAX` in particular wraps to `-1`,
///   which signals **every process the user can signal**.
///
/// `kill` negates this value itself to signal the group first, so the result
/// is always strictly positive.
fn signal_target(pid: u32) -> Option<i32> {
    match i32::try_from(pid) {
        Ok(pid) if pid > 0 => Some(pid),
        _ => {
            tracing::warn!(pid, "refusing to signal: not a single process id");
            None
        }
    }
}

fn to_nix(signal: Signal) -> nix::sys::signal::Signal {
    use nix::sys::signal::Signal as S;
    match signal {
        Signal::Term => S::SIGTERM,
        Signal::Kill => S::SIGKILL,
        Signal::Int => S::SIGINT,
        Signal::Hup => S::SIGHUP,
    }
}

#[async_trait]
impl ProcessManager for SystemProcessManager {
    /// `kill(pid, 0)` succeeds, **or** fails with `EPERM` — the process exists
    /// but we may not signal it.
    fn is_alive(&self, pid: u32) -> bool {
        let Some(pid) = signal_target(pid) else {
            // Not a process, so not alive. Reporting `true` here — which
            // `kill(0, 0)` does, because the caller's own group exists — would
            // have reconcile keep waiting on a running entry forever.
            return false;
        };
        match nix::sys::signal::kill(Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(Errno::EPERM) => true,
            Err(_) => false,
        }
    }

    /// Signals the whole process **group** first, so a child's own children
    /// (vitest, playwright, …) are reaped too, and falls back to the bare pid.
    /// Errors are swallowed either way: the process is simply already gone.
    fn kill(&self, pid: u32, signal: Signal) {
        let signal = to_nix(signal);
        let Some(pid) = signal_target(pid) else {
            return;
        };
        if nix::sys::signal::kill(Pid::from_raw(-pid), signal).is_ok() {
            return;
        }
        if let Err(errno) = nix::sys::signal::kill(Pid::from_raw(pid), signal) {
            tracing::debug!(pid, %signal, %errno, "kill failed; process already gone");
        }
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        if !self.is_alive(pid) {
            return;
        }

        self.kill(pid, Signal::Term);

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !self.is_alive(pid) {
                return;
            }
            tokio::time::sleep(KILL_POLL_INTERVAL).await;
        }

        // Force kill if still alive. Like the TS, death is not confirmed
        // afterwards (PARITY.md §14.14).
        tracing::debug!(pid, "grace period expired; sending SIGKILL");
        self.kill(pid, Signal::Kill);
    }

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdin(match opts.stdin {
                StdioMode::Ignore => Stdio::null(),
                StdioMode::Pipe => Stdio::piped(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if !opts.cwd.as_os_str().is_empty() {
            cmd.current_dir(&opts.cwd);
        }
        for (key, value) in &opts.env {
            cmd.env(key, value);
        }

        // node's `detached: true`: a new process group whose pgid is the
        // child's pid, which is what makes `kill(-pid)` reach the whole tree.
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|source| spawn_error(command, source))?;
        // The TS throws `Failed to spawn process: <command>` when node hands
        // back a child with no pid; in Rust that case is the `Err` above, and
        // a successfully spawned child always has one until it is reaped.
        let pid = child
            .id()
            .ok_or_else(|| Error::other(format!("Failed to spawn process: {command}")))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdin = child.stdin.take();

        // Reap in the background. This is both the `ExitSignal` source and
        // what keeps `is_alive` honest: an unreaped child stays a zombie and
        // `kill(pid, 0)` would keep answering "alive". No `kill_on_drop`, so
        // the child outlives us exactly as node's `proc.unref()` allows.
        let (notifier, exit) = ExitSignal::channel();
        tokio::spawn(async move {
            let outcome = match child.wait().await {
                Ok(status) => ExitOutcome {
                    code: status.code(),
                    signal: status.signal(),
                },
                Err(err) => {
                    tracing::warn!(pid, error = %err, "failed to reap child");
                    ExitOutcome {
                        code: None,
                        signal: None,
                    }
                }
            };
            notifier.notify(outcome);
        });

        Ok(SpawnedChild::from_tokio(pid, stdout, stderr, stdin, exit))
    }
}

fn spawn_error(command: &str, source: std::io::Error) -> Error {
    // `Display` is exactly the TS message; the io error stays in the source
    // chain for `ORCHESTRY_DEBUG`.
    tracing::warn!(command, error = %source, "spawn failed");
    Error::Io {
        context: format!("Failed to spawn process: {command}"),
        path: Some(PathBuf::from(command)),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::process::DEFAULT_KILL_GRACE;
    use orch_core::test_support::{describe, wait_for_no_live_members};
    use std::collections::BTreeMap;
    use std::path::Path;
    use tokio::io::AsyncReadExt;

    fn spawn_options_in(cwd: impl AsRef<Path>) -> SpawnOptions {
        SpawnOptions {
            cwd: cwd.as_ref().to_path_buf(),
            ..Default::default()
        }
    }

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn sh(script: &str) -> Vec<String> {
        args(&["-c", script])
    }

    /// Reaping is asynchronous, and these tests run alongside a dozen other
    /// test binaries, so the bound is generous: it returns the moment the
    /// condition holds, and a short one would only buy a flake.
    async fn wait_until_dead(pm: &SystemProcessManager, pid: u32) -> bool {
        for _ in 0..1_000 {
            if !pm.is_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[tokio::test]
    async fn spawn_pipes_stdout_and_reports_the_exit_code() {
        let pm = SystemProcessManager::new();
        let mut child = pm
            .spawn(
                "sh",
                &sh("printf 'hello\\n'; exit 3"),
                SpawnOptions::default(),
            )
            .expect("spawn");

        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("stdout is piped")
            .read_to_string(&mut out)
            .await
            .expect("read");
        assert_eq!(out, "hello\n");

        let outcome = child.exit.wait().await;
        assert_eq!(outcome.code, Some(3));
        assert_eq!(outcome.signal, None);
    }

    #[tokio::test]
    async fn stdin_is_ignored_by_default_and_piped_on_request() {
        let pm = SystemProcessManager::new();
        let child = pm
            .spawn("sh", &sh("exit 0"), SpawnOptions::default())
            .expect("spawn");
        assert!(child.stdin.is_none());

        let child = pm
            .spawn(
                "sh",
                &sh("exit 0"),
                SpawnOptions {
                    stdin: StdioMode::Pipe,
                    ..Default::default()
                },
            )
            .expect("spawn");
        assert!(child.stdin.is_some());
    }

    #[tokio::test]
    async fn the_environment_is_inherited_and_overlaid() {
        let pm = SystemProcessManager::new();
        let mut env = BTreeMap::new();
        env.insert("ORCH_TEST_OVERLAY".to_owned(), "overlaid".to_owned());

        let mut child = pm
            .spawn(
                "sh",
                &sh("printf '%s %s' \"$ORCH_TEST_OVERLAY\" \"${PATH:+has-path}\""),
                SpawnOptions {
                    env,
                    ..Default::default()
                },
            )
            .expect("spawn");

        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("stdout")
            .read_to_string(&mut out)
            .await
            .expect("read");
        assert_eq!(out, "overlaid has-path");
    }

    #[tokio::test]
    async fn the_cwd_is_applied() {
        let dir = tempfile::tempdir().expect("tempdir");
        let canonical = dir.path().canonicalize().expect("canonicalize");
        let pm = SystemProcessManager::new();
        let mut child = pm
            .spawn("sh", &sh("pwd -P"), spawn_options_in(&canonical))
            .expect("spawn");

        let mut out = String::new();
        child
            .stdout
            .take()
            .expect("stdout")
            .read_to_string(&mut out)
            .await
            .expect("read");
        assert_eq!(out.trim_end(), canonical.to_string_lossy());
    }

    #[tokio::test]
    async fn a_missing_binary_fails_with_the_ts_message() {
        let pm = SystemProcessManager::new();
        let err = pm
            .spawn("orch-definitely-not-a-binary", &[], SpawnOptions::default())
            .expect_err("spawn should fail");
        assert_eq!(
            err.to_string(),
            "Failed to spawn process: orch-definitely-not-a-binary"
        );
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn spawn_creates_a_new_process_group_led_by_the_child() {
        let pm = SystemProcessManager::new();
        let mut child = pm
            .spawn(
                "sh",
                // Just enough output to know the child is up; the group is
                // then read with `getpgid`, not parsed out of `ps`. The two
                // flakes that cost us a morning — `ps -o pgid=` dropping its
                // column padding at a 7-digit pid, and the empty `pgid` that
                // left behind — are both artifacts of the parsing rather than
                // of process groups.
                &sh("printf 'up\\n'; sleep 30"),
                SpawnOptions::default(),
            )
            .expect("spawn");

        let mut up = [0u8; 3];
        child
            .stdout
            .as_mut()
            .expect("stdout")
            .read_exact(&mut up)
            .await
            .expect("read");

        // node's `detached: true`: the child leads a group of its own, which
        // is what makes `kill(-pid)` reach the whole tree.
        let pgid = nix::unistd::getpgid(Some(Pid::from_raw(child.pid as i32))).expect("pgid");
        assert_eq!(pgid.as_raw() as u32, child.pid);
        assert_ne!(
            pgid.as_raw() as u32,
            std::process::id(),
            "the child must not share our group"
        );

        pm.kill(child.pid, Signal::Kill);
        assert!(wait_until_dead(&pm, child.pid).await);
    }

    #[tokio::test]
    async fn is_alive_tracks_a_real_process() {
        let pm = SystemProcessManager::new();
        let child = pm
            .spawn("sleep", &args(&["30"]), SpawnOptions::default())
            .expect("spawn");

        assert!(pm.is_alive(child.pid));
        pm.kill(child.pid, Signal::Kill);
        assert!(wait_until_dead(&pm, child.pid).await);
    }

    #[test]
    fn is_alive_is_true_for_our_own_pid_and_false_for_a_free_one() {
        let pm = SystemProcessManager::new();
        assert!(pm.is_alive(std::process::id()));
        assert!(!pm.is_alive(999_000_001));
    }

    #[test]
    fn only_a_single_positive_process_id_is_signalable() {
        assert_eq!(signal_target(1), Some(1));
        assert_eq!(signal_target(i32::MAX as u32), Some(i32::MAX));
        // `kill(0, …)` means the caller's own process group.
        assert_eq!(signal_target(0), None);
        // These wrap to a negative `i32`, which `kill(2)` reads as a process
        // group — `u32::MAX` becomes `-1`, "every process we may signal".
        assert_eq!(signal_target(u32::MAX), None);
        assert_eq!(signal_target(i32::MAX as u32 + 1), None);
    }

    #[test]
    fn a_pid_that_is_not_a_process_is_never_alive() {
        let pm = SystemProcessManager::new();
        // `kill(0, 0)` *succeeds* — our own group exists — so without the
        // guard this reports `true` and reconcile waits on the running entry
        // forever. Both values are reachable: `state.running` is JSON on disk.
        assert!(!pm.is_alive(0));
        assert!(!pm.is_alive(u32::MAX));
    }

    #[tokio::test]
    async fn signalling_a_non_process_pid_does_not_signal_us() {
        let pm = SystemProcessManager::new();
        // Without the guard, each of these sends the signal to a process
        // *group*: pid 0 is our own, and `u32::MAX` wraps to -1. This test
        // then does not fail — it terminates the test runner, which is the
        // only way a mistake of this shape can announce itself.
        pm.kill(0, Signal::Term);
        pm.kill(u32::MAX, Signal::Term);
        pm.kill_with_grace(0, Duration::from_millis(50)).await;

        assert!(
            pm.is_alive(std::process::id()),
            "we signalled our own process group"
        );
    }

    #[test]
    fn killing_a_dead_pid_does_not_panic() {
        let pm = SystemProcessManager::new();
        pm.kill(999_000_002, Signal::Term);
        pm.kill(999_000_002, Signal::Kill);
    }

    #[tokio::test]
    async fn kill_with_grace_returns_immediately_for_a_dead_pid() {
        let pm = SystemProcessManager::new();
        let started = Instant::now();
        pm.kill_with_grace(999_000_003, DEFAULT_KILL_GRACE).await;
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn kill_with_grace_terminates_a_cooperative_process() {
        let pm = SystemProcessManager::new();
        let child = pm
            .spawn("sleep", &args(&["60"]), SpawnOptions::default())
            .expect("spawn");

        pm.kill_with_grace(child.pid, Duration::from_secs(2)).await;
        assert!(wait_until_dead(&pm, child.pid).await);
        assert_eq!(child.exit.wait().await.signal, Some(15));
    }

    #[tokio::test]
    async fn kill_with_grace_sigkills_a_process_that_ignores_sigterm() {
        let pm = SystemProcessManager::new();
        let mut child = pm
            .spawn(
                "sh",
                // The loop keeps the shell itself in control, so the signal
                // disposition is the shell's rather than `sleep`'s.
                &sh("trap '' TERM; printf 'up\\n'; while :; do sleep 0.05; done"),
                SpawnOptions::default(),
            )
            .expect("spawn");

        // Wait for the trap to be installed; a SIGTERM that lands before it
        // would simply kill the shell.
        let mut up = [0u8; 3];
        child
            .stdout
            .as_mut()
            .expect("stdout")
            .read_exact(&mut up)
            .await
            .expect("read");

        pm.kill_with_grace(child.pid, Duration::from_millis(300))
            .await;
        assert!(wait_until_dead(&pm, child.pid).await);
        assert_eq!(child.exit.wait().await.signal, Some(9));
    }

    #[tokio::test]
    async fn the_group_kill_reaps_the_children_of_the_child() {
        let pm = SystemProcessManager::new();
        let child = pm
            .spawn(
                "sh",
                // Every child is forked **before** `up`, and the shell then
                // `wait`s — a builtin, so it forks nothing more. That ordering
                // is the whole point.
                //
                // With a trailing foreground `sleep 30` instead, the shell was
                // still forking when the kill arrived, and a child created
                // between `kill(-pgid, SIGTERM)` and the shell's death inherits
                // the group *without* having been signalled: `kill_with_grace`
                // returns as soon as the direct child is gone and never sends
                // the SIGKILL. That is faithful to the TS (`killWithGrace` has
                // the same early return), so the implementation is right and
                // the test's premise was wrong — it asserted a stronger
                // property than a group kill actually provides.
                //
                // Measured, from a preserved gate log: `pid … state S ppid 1
                // pgid <target>` — a live, reparented, never-signalled
                // grandchild. Load-dependent, which is why it only ever
                // appeared in a cold gate.
                &sh("sleep 30 & sleep 30 & sleep 30 & printf 'up\\n'; wait"),
                SpawnOptions::default(),
            )
            .expect("spawn");

        // Wait for the grandchildren to exist.
        let mut out = [0u8; 3];
        let mut stdout = child.stdout.expect("stdout");
        stdout.read_exact(&mut out).await.expect("read");

        pm.kill_with_grace(child.pid, Duration::from_secs(2)).await;
        assert!(wait_until_dead(&pm, child.pid).await);

        // Nothing *live* may be left in the group.
        //
        // This used to poll `kill(-pgid, 0)` until it failed, which meant
        // waiting for the group to be completely empty — and that call
        // succeeds for a **zombie** as well as a running process, so the test
        // was really waiting on init to reap the orphaned grandchildren.
        // Nothing here controls that, and it flaked at roughly 1 in 10
        // full-workspace runs while saying only "the group outlived the kill",
        // which does not distinguish a broken kill from slow reaping.
        //
        // A zombie means the kill worked. A live member means it did not, and
        // that is a product bug the assertion must still catch — so the check
        // is on live members, and the budget stays well under the 30 s the
        // children would live for on their own. Shortening *their* lifetime
        // instead would let a missed signal expire by itself and quietly turn
        // this assertion into a no-op.
        if let Err(survivors) = wait_for_no_live_members(child.pid, Duration::from_secs(10)).await {
            panic!(
                "the group kill left {} live process(es) behind, so it did not reach the \
                 whole group: {}",
                survivors.len(),
                describe(&survivors)
            );
        }
    }
}
