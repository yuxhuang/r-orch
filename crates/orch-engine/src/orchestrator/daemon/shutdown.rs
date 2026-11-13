use std::sync::atomic::Ordering;

use orch_core::error::{Error, Result};
use orch_domain::{AgentStatus, RemotePhase, RunStatus};

use super::super::Orchestrator;

impl Orchestrator {
    /// Stop execution while leaving PID/guard ownership to the daemon lifecycle.
    pub async fn daemon_stop(&self) -> Result<()> {
        self.daemon_stop_with_spawn_seal(|| {}).await
    }

    /// Close new admissions before any wait. An admitted launch that passed
    /// its final checks owns its spawn through the running-task registration;
    /// the stop snapshots and reconciles it in the next project operation.
    pub async fn daemon_stop_with_spawn_seal(&self, seal: impl FnOnce() + Send) -> Result<()> {
        seal();
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        let _lifecycle = self.inner.lifecycle.lock().await;
        let boundary = self.deps().events.operation_boundary();
        let entries = boundary
            .run(false, async {
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                let entries = state.running.values().cloned().collect::<Vec<_>>();
                for entry in &entries {
                    if entry.remote.is_none() && self.worker_signal_unverified(entry).await {
                        return Err(Error::other(
                            "Worker execution ownership cannot verify; reservation retained",
                        ));
                    }
                }
                let mut cancels = self.inner.cancels.lock().await;
                for (_, token) in cancels.drain() {
                    token.cancel();
                }
                Ok::<_, Error>(entries)
            })
            .await
            .value?;
        let mut failures = Vec::new();
        let mut survivors = Vec::new();
        for entry in &entries {
            if entry.remote.is_none() {
                self.deps()
                    .processes
                    .kill_with_grace(entry.pid, orch_core::process::DEFAULT_KILL_GRACE)
                    .await;
                if !self.daemon_confirm_child_stopped(entry.pid).await {
                    failures.push(entry.task_id.clone());
                    survivors.push(Self::daemon_survivor_identity(&entry.run_id, entry.pid));
                }
            }
        }
        boundary
            .run(false, async {
                self.cancel_lazy_save().await;
                let mut slot = self.inner.state.lock().await;
                self.load_state(&mut slot).await?;
                let state = slot.as_mut().expect("state loaded");
                for entry in &entries {
                    if failures.contains(&entry.task_id) {
                        continue;
                    }
                    if entry.remote.is_some() {
                        if let Some(remote) = state
                            .running
                            .get_mut(&entry.task_id)
                            .and_then(|entry| entry.remote.as_mut())
                        {
                            remote.phase = RemotePhase::Unknown;
                        }
                        continue;
                    }
                    self.deps()
                        .run_service
                        .finish(&entry.run_id, RunStatus::Cancelled, None, None)
                        .await?;
                    if let Some(task) = self.deps().task_store.get(&entry.task_id).await?
                        && !orch_domain::is_terminal(task.status)
                    {
                        self.deps()
                            .task_service
                            .update_status(
                                &entry.task_id,
                                orch_domain::resolve_failure_status(&task),
                            )
                            .await?;
                    }
                    self.deps()
                        .agent_service
                        .set_status(&entry.agent_id, AgentStatus::Idle)
                        .await?;
                    if let Some(mut agent) = self.deps().agent_store.get(&entry.agent_id).await? {
                        agent.current_task = None;
                        self.deps().agent_store.save(&agent).await?;
                    }
                    if state
                        .running
                        .get(&entry.task_id)
                        .is_some_and(|current| current.run_id == entry.run_id)
                    {
                        state.running.shift_remove(&entry.task_id);
                    }
                }
                for run in self.deps().run_store.list_all().await? {
                    if run.status == RunStatus::Preparing
                        && !state.running.values().any(|entry| entry.run_id == run.id)
                    {
                        self.deps()
                            .run_service
                            .finish(&run.id, RunStatus::Cancelled, None, None)
                            .await?;
                    }
                }
                if failures.is_empty() {
                    state.claimed.clear();
                    state.pid = None;
                    state.started_at = None;
                    self.inner.preparing_agents.lock().await.clear();
                }
                self.save_state(state).await
            })
            .await
            .value?;
        if !failures.is_empty() {
            return Err(Error::other(format!(
                "Daemon shutdown incomplete; local children remain alive: {}",
                survivors.join("; ")
            )));
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.wait_for_collectors(),
        )
        .await
        .map_err(|_| Error::other("Daemon shutdown incomplete; collectors did not finish"))?;
        Ok(())
    }
}
