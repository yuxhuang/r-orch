//! File-based orchestrator state store — ported from
//! `src/infrastructure/storage/state-store.ts`.
//!
//! Reads are **tolerant**: a missing, malformed or wrong-typed field falls
//! back to its default rather than failing the command
//! ([`OrchestratorState::from_raw`], PARITY.md §6.6). Writes emit `claimed` as
//! a JSON array; the `{}` form belongs to `status --json` alone (§14.1).

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::StateStore;
use orch_domain::{OrchestratorState, default_state};

use super::fs_utils::{read_json, write_json};
use super::paths::Paths;

#[derive(Debug)]
pub struct FileStateStore {
    paths: Paths,
}

impl FileStateStore {
    pub fn new(paths: Paths) -> Self {
        FileStateStore { paths }
    }
}

#[async_trait]
impl StateStore for FileStateStore {
    async fn read(&self) -> Result<OrchestratorState> {
        // Read as an untyped value: a strict deserialize would reject exactly
        // the malformed files the TS reader tolerates.
        match read_json::<serde_json::Value>(&self.paths.state_path()).await {
            Ok(Some(raw)) => Ok(OrchestratorState::from_raw(Some(&raw))),
            Ok(None) => Ok(default_state()),
            // Unparseable JSON is not recoverable field-by-field; the TS
            // `readJson` throws here too.
            Err(err) => Err(err),
        }
    }

    async fn write(&self, state: &OrchestratorState) -> Result<()> {
        write_json(&self.paths.state_path(), state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{atomic_write, clear_ensured_dirs, ensure_dir};
    use orch_domain::{RetryEntry, RunningEntry};

    async fn store(root: &std::path::Path) -> FileStateStore {
        clear_ensured_dirs();
        let paths = Paths::new(root);
        ensure_dir(&paths.root()).await.unwrap();
        FileStateStore::new(paths)
    }

    #[tokio::test]
    async fn a_missing_file_reads_as_the_default_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        assert_eq!(store.read().await.unwrap(), default_state());
    }

    #[tokio::test]
    async fn written_state_matches_the_reference_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        store.write(&default_state()).await.unwrap();

        // PARITY.md §6.6 — note `claimed` is an array on disk.
        assert_eq!(
            tokio::fs::read_to_string(store.paths.state_path())
                .await
                .unwrap(),
            r#"{
  "version": 1,
  "onboardingCompleted": false,
  "running": {},
  "claimed": [],
  "retry_queue": [],
  "stats": {
    "total_runs": 0,
    "total_tasks_completed": 0,
    "total_tasks_failed": 0,
    "total_tokens": {
      "input": 0,
      "output": 0,
      "reasoning": 0,
      "total": 0,
      "cache_read": 0,
      "cache_write": 0
    },
    "total_runtime_ms": 0
  }
}
"#
        );
    }

    #[tokio::test]
    async fn a_full_state_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        let mut state = default_state();
        state.pid = Some(4242);
        state.started_at = Some("2026-10-13T16:37:33.426Z".into());
        state.claimed.insert("tsk_b".into());
        state.claimed.insert("tsk_a".into());
        state.running.insert(
            "tsk_1".into(),
            RunningEntry {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                pid: 99,
                started_at: "t0".into(),
                last_event_at: "t1".into(),
            },
        );
        state.retry_queue.push(RetryEntry {
            task_id: "tsk_2".into(),
            attempt: 1,
            due_at: "t2".into(),
            error: "boom".into(),
        });
        state.stats.total_runs = 3;

        store.write(&state).await.unwrap();
        let read_back = store.read().await.unwrap();
        assert_eq!(read_back, state);
        // Set insertion order survives, like a JS Set's.
        assert_eq!(
            read_back.claimed.iter().collect::<Vec<_>>(),
            vec!["tsk_b", "tsk_a"]
        );
    }

    #[tokio::test]
    async fn malformed_fields_fall_back_individually() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(
            &store.paths.state_path(),
            r#"{"onboardingCompleted":"yes","running":[1],"claimed":{"a":1},
                "retry_queue":"nope","stats":{"total_runs":7}}"#,
        )
        .await
        .unwrap();

        let state = store.read().await.unwrap();
        assert_eq!(state.onboarding_completed, Some(false));
        assert!(state.running.is_empty());
        assert!(state.claimed.is_empty());
        assert!(state.retry_queue.is_empty());
        // The one good field is kept.
        assert_eq!(state.stats.total_runs, 7);
        assert_eq!(state.version, 1);
    }

    #[tokio::test]
    async fn unparseable_json_is_an_error_not_a_silent_reset() {
        let temp = tempfile::tempdir().unwrap();
        let store = store(temp.path()).await;
        atomic_write(&store.paths.state_path(), "{not json")
            .await
            .unwrap();
        assert!(store.read().await.is_err());
    }
}
