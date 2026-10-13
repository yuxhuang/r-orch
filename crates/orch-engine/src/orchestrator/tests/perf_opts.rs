//! Ported from `test/unit/application/orchestrator-perf-opts.test.ts`.
//!
//! Two optimizations with observable contracts: `state.claimed` is a **set**
//! that round-trips through a JSON array (and recovers from a corrupt one),
//! and reconcile pre-fetches its running tasks and agents **concurrently**.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use orch_core::error::Result;
use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::{FakeTaskStore, make_agent, make_task};
use orch_domain::{Agent, AgentStatus, OrchestratorState, Task, TaskStatus, default_state};

use super::harness::{Harness, NOW_ISO, running_entry};

// ── `claimed` is a set ───────────────────────────────────────────────────

#[test]
fn the_default_states_claimed_is_an_empty_set() {
    let state = default_state();
    assert!(state.claimed.is_empty());
}

#[test]
fn claimed_supports_insert_remove_and_membership() {
    let mut state = default_state();
    assert!(state.claimed.insert("tsk_1".to_owned()));
    assert!(state.claimed.contains("tsk_1"));
    // Inserting twice does not duplicate.
    assert!(!state.claimed.insert("tsk_1".to_owned()));
    assert_eq!(state.claimed.len(), 1);

    assert!(state.claimed.shift_remove("tsk_1"));
    assert!(!state.claimed.contains("tsk_1"));
    // Removing something absent is a no-op, not an error.
    assert!(!state.claimed.shift_remove("tsk_absent"));
}

#[test]
fn claimed_serializes_as_an_array_and_round_trips() {
    let mut state = default_state();
    for id in ["tsk_a", "tsk_b", "tsk_c"] {
        state.claimed.insert(id.to_owned());
    }

    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(
        json["claimed"],
        serde_json::json!(["tsk_a", "tsk_b", "tsk_c"])
    );

    let restored: OrchestratorState = serde_json::from_value(json).unwrap();
    assert_eq!(restored.claimed.len(), 3);
    for id in ["tsk_a", "tsk_b", "tsk_c"] {
        assert!(restored.claimed.contains(id));
    }
}

#[test]
fn a_corrupt_claimed_recovers_as_an_empty_set() {
    // `from_raw` is the lenient reader the state store uses; a `null` or a
    // non-array must not take the whole file down.
    for corrupt in [
        serde_json::json!(null),
        serde_json::json!("not-an-array"),
        serde_json::json!({ "tsk_1": true }),
        serde_json::json!(42),
    ] {
        let raw = serde_json::json!({ "version": 1, "claimed": corrupt });
        let state = OrchestratorState::from_raw(Some(&raw));
        assert!(state.claimed.is_empty(), "corrupt={corrupt}");
    }
}

// ── reconcile pre-fetches concurrently ───────────────────────────────────

/// Records how many `get` calls were in flight at once.
struct ConcurrencyProbe {
    inner: Arc<FakeTaskStore>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    gets: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl TaskStore for ConcurrencyProbe {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.gets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id.to_owned());
        let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(current, Ordering::SeqCst);
        // Long enough that a sequential loop could not overlap.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

fn in_progress(id: &str) -> Task {
    Task {
        id: id.into(),
        status: TaskStatus::InProgress,
        ..make_task()
    }
}

#[tokio::test]
async fn reconcile_reads_every_running_task() {
    let mut state = OrchestratorState::default();
    state.running.insert(
        "tsk_r1".to_owned(),
        running_entry("tsk_r1", "agt_1", "run_1", 1111, NOW_ISO),
    );
    state.running.insert(
        "tsk_r2".to_owned(),
        running_entry("tsk_r2", "agt_1", "run_2", 2222, NOW_ISO),
    );

    let probe: Arc<std::sync::Mutex<Option<Arc<ConcurrencyProbe>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured = Arc::clone(&probe);

    let h = Harness::builder()
        .tasks([in_progress("tsk_r1"), in_progress("tsk_r2")])
        .agents([Agent {
            id: "agt_1".into(),
            status: AgentStatus::Running,
            ..make_agent()
        }])
        .state(state)
        .wrap_task_store(move |inner| {
            let probe = Arc::new(ConcurrencyProbe {
                inner,
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                gets: std::sync::Mutex::new(Vec::new()),
            });
            *captured.lock().unwrap() = Some(Arc::clone(&probe));
            probe as Arc<dyn TaskStore>
        })
        .build();

    h.reconcile().await.unwrap();

    let probe = probe.lock().unwrap().clone().expect("probe installed");
    let gets = probe.gets.lock().unwrap().clone();
    assert!(gets.contains(&"tsk_r1".to_owned()), "{gets:?}");
    assert!(gets.contains(&"tsk_r2".to_owned()), "{gets:?}");
}

#[tokio::test]
async fn reconcile_issues_all_of_its_pre_fetches_before_processing_any_of_them() {
    let mut state = OrchestratorState::default();
    for id in ["tsk_a", "tsk_b", "tsk_c"] {
        state.running.insert(
            id.to_owned(),
            running_entry(id, "agt_1", &format!("run_{id}"), 9999, NOW_ISO),
        );
    }

    let probe: Arc<std::sync::Mutex<Option<Arc<ConcurrencyProbe>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured = Arc::clone(&probe);

    let h = Harness::builder()
        .tasks([
            in_progress("tsk_a"),
            in_progress("tsk_b"),
            in_progress("tsk_c"),
        ])
        .agents([Agent {
            id: "agt_1".into(),
            status: AgentStatus::Running,
            ..make_agent()
        }])
        .state(state)
        .wrap_task_store(move |inner| {
            let probe = Arc::new(ConcurrencyProbe {
                inner,
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                gets: std::sync::Mutex::new(Vec::new()),
            });
            *captured.lock().unwrap() = Some(Arc::clone(&probe));
            probe as Arc<dyn TaskStore>
        })
        .build();

    h.reconcile().await.unwrap();

    let probe = probe.lock().unwrap().clone().expect("probe installed");
    // All three pre-fetches overlapped — one round trip, not three.
    assert_eq!(probe.max_in_flight.load(Ordering::SeqCst), 3);
}
