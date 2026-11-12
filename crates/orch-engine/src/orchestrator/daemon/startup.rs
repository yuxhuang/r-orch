use orch_core::error::{Error, Result};
use orch_domain::RunStatus;

use super::super::Orchestrator;

impl Orchestrator {
    /// The process port can return immediately after SIGKILL. Confirm only
    /// liveness of the already-selected PID; this grants no signal authority.
    pub(super) async fn daemon_confirm_child_stopped(&self, pid: u32) -> bool {
        if !self.deps().processes.is_alive(pid) {
            return true;
        }
        let deadline = tokio::time::Instant::now() + super::super::DAEMON_KILL_CONFIRMATION;
        loop {
            let next = tokio::time::Instant::now() + std::time::Duration::from_millis(10);
            tokio::time::sleep_until(next.min(deadline)).await;
            // Also probe after the final sleep, at the deadline.
            if !self.deps().processes.is_alive(pid) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
        }
    }

    pub(super) fn daemon_survivor_identity(run_id: &str, pid: u32) -> String {
        // Saved metadata can be hand-edited: bound each identifier and keep
        // control characters from adding diagnostic lines.
        let run_id: String = run_id
            .chars()
            .take(64)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        format!("run {run_id}, PID {pid}")
    }

    /// Recover under lifecycle ownership acquired by orch-daemon. No PID lock
    /// is acquired or released here, and orphan children are reaped externally.
    pub async fn daemon_initialize(&self) -> Result<()> {
        self.enable_daemon_mode();
        let boundary = self.deps().events.operation_boundary();
        let orphaned = boundary
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                self.invalidate_caches().await;
                let state = slot.as_mut().expect("state loaded");
                self.cleanup_stale_running_entries(state).await?;
                let runs = self.deps().run_store.list_all().await?;
                let orphaned = runs
                    .into_iter()
                    .filter(|run| {
                        run.status == RunStatus::Running
                            && !state.running.values().any(|entry| entry.run_id == run.id)
                    })
                    .collect::<Vec<_>>();
                self.save_state(state).await?;
                Ok::<_, Error>(orphaned)
            })
            .await
            .value?;
        let mut failures = Vec::new();
        for run in &orphaned {
            if !run.agent_id.contains('@')
                && let Some(pid) = run.pid
                && self.deps().processes.is_alive(pid)
            {
                self.deps()
                    .processes
                    .kill_with_grace(pid, orch_core::process::DEFAULT_KILL_GRACE)
                    .await;
                if !self.daemon_confirm_child_stopped(pid).await {
                    failures.push(Self::daemon_survivor_identity(&run.id, pid));
                }
            }
        }
        if !failures.is_empty() {
            return Err(Error::other(format!(
                "Daemon recovery could not stop an orphaned local child: {}",
                failures.join("; ")
            )));
        }
        boundary
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                for run in &orphaned {
                    if state.running.values().any(|entry| entry.run_id == run.id) {
                        continue;
                    }
                    if run.agent_id.contains('@') {
                        self.force_task_cancelled(&run.task_id).await;
                    }
                    self.deps()
                        .run_service
                        .finish(
                            &run.id,
                            RunStatus::Cancelled,
                            None,
                            Some("Orchestrator restarted".to_owned()),
                        )
                        .await?;
                }
                state.pid = Some(std::process::id());
                state.started_at = Some(self.deps().clock.now_iso());
                self.save_state(state).await?;
                if let Err(error) = self.expire_context().await {
                    self.emit_error(error, "context expiry");
                }
                Ok::<_, Error>(())
            })
            .await
            .value
    }
}
