use orch_core::error::{Error, Result};
use orch_domain::RunStatus;

use super::super::Orchestrator;

impl Orchestrator {
    /// Recover under lifecycle ownership acquired by orch-daemon. No PID lock
    /// is acquired or released here, and orphan children are reaped externally.
    pub async fn daemon_initialize(&self) -> Result<()> {
        self.enable_daemon_mode();
        let boundary = self.deps().events.operation_boundary();
        let orphaned = boundary
            .run(true, async {
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
        for run in &orphaned {
            if !run.agent_id.contains('@')
                && let Some(pid) = run.pid
                && self.deps().processes.is_alive(pid)
            {
                self.deps()
                    .processes
                    .kill_with_grace(pid, orch_core::process::DEFAULT_KILL_GRACE)
                    .await;
                if self.deps().processes.is_alive(pid) {
                    return Err(Error::other(
                        "Daemon recovery could not stop an orphaned local child",
                    ));
                }
            }
        }
        boundary
            .run(true, async {
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
