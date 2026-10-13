//! A process manager that spawns nothing.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::error::Result;
use crate::process::{ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild};
use crate::test_support::recorder::Recorder;

/// The pid the TS `createMockProcessManager` hands back.
pub const FAKE_PID: u32 = 12345;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeProcessManagerCall {
    IsAlive(u32),
    Kill(u32, Signal),
    KillWithGrace(u32, Duration),
    Spawn {
        command: String,
        args: Vec<String>,
        cwd: String,
    },
}

/// `is_alive` answers `true` for everything by default, like the TS mock.
/// Call [`FakeProcessManager::set_dead`] to make specific pids look exited, or
/// [`FakeProcessManager::set_all_dead`] to flip the default.
#[derive(Debug, Clone)]
pub struct FakeProcessManager {
    pid: u32,
    alive_by_default: Arc<AtomicBool>,
    dead_pids: Arc<Mutex<HashSet<u32>>>,
    /// The outcome every spawned child's [`ExitSignal`] resolves to.
    exit: ExitOutcome,
    pub calls: Recorder<FakeProcessManagerCall>,
}

impl Default for FakeProcessManager {
    fn default() -> Self {
        FakeProcessManager::new()
    }
}

impl FakeProcessManager {
    pub fn new() -> Self {
        FakeProcessManager {
            pid: FAKE_PID,
            alive_by_default: Arc::new(AtomicBool::new(true)),
            dead_pids: Arc::new(Mutex::new(HashSet::new())),
            exit: ExitOutcome {
                code: Some(0),
                signal: None,
            },
            calls: Recorder::new(),
        }
    }

    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = pid;
        self
    }

    pub fn with_exit(mut self, exit: ExitOutcome) -> Self {
        self.exit = exit;
        self
    }

    /// The pid `spawn` reports.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Make one pid look exited — the crash-recovery and zombie-reconcile
    /// tests turn on this (PARITY.md §7.4, §7.6).
    pub fn set_dead(&self, pid: u32) {
        self.dead_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid);
    }

    pub fn set_alive(&self, pid: u32) {
        self.dead_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
    }

    /// Flip the default so unknown pids look exited.
    pub fn set_all_dead(&self, all_dead: bool) {
        self.alive_by_default.store(!all_dead, Ordering::SeqCst);
    }

    /// Pids passed to `kill`, in order.
    pub fn killed(&self) -> Vec<(u32, Signal)> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeProcessManagerCall::Kill(pid, signal) => Some((pid, signal)),
                _ => None,
            })
            .collect()
    }

    /// Pids passed to `kill_with_grace`, in order.
    pub fn killed_with_grace(&self) -> Vec<(u32, Duration)> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeProcessManagerCall::KillWithGrace(pid, grace) => Some((pid, grace)),
                _ => None,
            })
            .collect()
    }

    /// The argv of every `spawn`, in order.
    pub fn spawned(&self) -> Vec<(String, Vec<String>)> {
        self.calls
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                FakeProcessManagerCall::Spawn { command, args, .. } => Some((command, args)),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl ProcessManager for FakeProcessManager {
    fn is_alive(&self, pid: u32) -> bool {
        self.calls.record(FakeProcessManagerCall::IsAlive(pid));
        if self
            .dead_pids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&pid)
        {
            return false;
        }
        self.alive_by_default.load(Ordering::SeqCst)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        self.calls.record(FakeProcessManagerCall::Kill(pid, signal));
        self.set_dead(pid);
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        self.calls
            .record(FakeProcessManagerCall::KillWithGrace(pid, grace));
        self.set_dead(pid);
    }

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        self.calls.record(FakeProcessManagerCall::Spawn {
            command: command.to_owned(),
            args: args.to_vec(),
            cwd: opts.cwd.to_string_lossy().into_owned(),
        });
        Ok(SpawnedChild {
            pid: self.pid,
            // Real `ChildStdout`/`ChildStderr`/`ChildStdin` cannot be
            // fabricated without a process; the TS mock passes `{} as any` for
            // the same reason. Drive adapter streams with `FakeAdapter`.
            stdout: None,
            stderr: None,
            stdin: None,
            exit: ExitSignal::resolved(self.exit),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn opts() -> SpawnOptions {
        SpawnOptions {
            cwd: PathBuf::from("/tmp/ws"),
            ..Default::default()
        }
    }

    #[test]
    fn spawn_reports_the_fixed_pid_and_records_the_argv() {
        let pm = FakeProcessManager::new();
        let child = pm.spawn("git", &["status".to_owned()], opts()).unwrap();
        assert_eq!(child.pid, 12345);
        assert!(child.stdout.is_none());
        assert_eq!(
            pm.spawned(),
            vec![("git".to_owned(), vec!["status".to_owned()])]
        );
    }

    #[tokio::test]
    async fn a_spawned_child_exits_successfully_by_default() {
        let pm = FakeProcessManager::new();
        let child = pm.spawn("true", &[], opts()).unwrap();
        assert!(child.exit.wait().await.success());

        let pm = FakeProcessManager::new().with_exit(ExitOutcome {
            code: Some(1),
            signal: None,
        });
        let child = pm.spawn("false", &[], opts()).unwrap();
        assert_eq!(child.exit.wait().await.code, Some(1));
    }

    #[test]
    fn is_alive_is_true_by_default() {
        let pm = FakeProcessManager::new();
        assert!(pm.is_alive(1));
        assert!(pm.is_alive(99999));
    }

    #[test]
    fn specific_pids_can_be_marked_dead() {
        let pm = FakeProcessManager::new();
        pm.set_dead(4242);
        assert!(!pm.is_alive(4242));
        assert!(pm.is_alive(1));
        pm.set_alive(4242);
        assert!(pm.is_alive(4242));
    }

    #[test]
    fn the_default_can_be_flipped_to_dead() {
        let pm = FakeProcessManager::new();
        pm.set_all_dead(true);
        assert!(!pm.is_alive(1));
        pm.set_all_dead(false);
        assert!(pm.is_alive(1));
    }

    #[tokio::test]
    async fn killing_records_the_signal_and_makes_the_pid_look_dead() {
        let pm = FakeProcessManager::new();
        pm.kill(12345, Signal::Term);
        assert_eq!(pm.killed(), vec![(12345, Signal::Term)]);
        assert!(!pm.is_alive(12345));

        pm.kill_with_grace(777, Duration::from_secs(3)).await;
        assert_eq!(pm.killed_with_grace(), vec![(777, Duration::from_secs(3))]);
        assert!(!pm.is_alive(777));
    }

    #[test]
    fn the_pid_is_overridable() {
        let pm = FakeProcessManager::new().with_pid(7);
        assert_eq!(pm.pid(), 7);
        assert_eq!(pm.spawn("x", &[], opts()).unwrap().pid, 7);
    }

    #[tokio::test]
    async fn works_as_a_trait_object() {
        let pm: Arc<dyn ProcessManager> = Arc::new(FakeProcessManager::new());
        assert!(pm.is_alive(1));
        pm.kill_with_grace(1, Duration::from_secs(1)).await;
        assert!(!pm.is_alive(1));
    }
}
