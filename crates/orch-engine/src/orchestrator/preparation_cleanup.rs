//! Retry failed preparation releases without admitting a second launch.

use orch_domain::OrchestratorState;

use super::Orchestrator;

impl Orchestrator {
    /// One attempt per owned obligation per reconciliation. A broken store
    /// keeps the reservation and reports the failure; it does not prevent
    /// reconciliation of other tasks. The caller holds the state lock and,
    /// in daemon mode, the project operation boundary.
    pub(super) async fn reconcile_preparation_cleanup(&self, state: &mut OrchestratorState) {
        let pending = self.inner.pending_preparation_cleanup.lock().await.clone();
        for (task_id, generation) in pending {
            if !self.owns_preparation(&task_id, generation).await {
                // A stale obligation cannot finish runs, clear a claim or
                // cancel the token belonging to a superseding preparation.
                self.inner
                    .pending_preparation_cleanup
                    .lock()
                    .await
                    .remove(&task_id);
                continue;
            }
            if let Err(error) = self
                .release_preparation_locked(state, &task_id, generation)
                .await
            {
                self.emit_error(error, format!("pending preparation cleanup {task_id}"));
            }
        }
    }
}
