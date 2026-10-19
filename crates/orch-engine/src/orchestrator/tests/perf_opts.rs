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

// ── cascade writes are ordered, not concurrent ───────────────────────────

/// Records save order, holding up one task's save.
///
/// `_index.json` keeps entries in the order their saves complete,
/// so the delay is what turns "are these writes
/// concurrent?" into a deterministic assertion: awaited in order, the slow one
/// still lands first; issued together, it lands last.
struct OrderedSaveProbe {
    inner: Arc<FakeTaskStore>,
    saved: std::sync::Mutex<Vec<String>>,
    slow: &'static str,
}

#[async_trait]
impl TaskStore for OrderedSaveProbe {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        if task.id == self.slow {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        self.inner.save(task).await?;
        self.saved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(task.id.clone());
        Ok(())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

/// A cascade must write its dependents in order, because the order they land
/// in `_index.json` is observable, and `join_all` on a multi-threaded runtime
/// does not preserve it.
#[tokio::test(flavor = "multi_thread")]
async fn a_cascade_saves_its_dependents_in_order() {
    let probe: Arc<std::sync::Mutex<Option<Arc<OrderedSaveProbe>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured = Arc::clone(&probe);

    let mut config = super::harness::test_config();
    // One attempt, so the root fails outright instead of being retried.
    config.defaults.task.max_attempts = 1;

    let h = Harness::builder()
        .tasks([
            Task {
                id: "tsk_root".into(),
                status: TaskStatus::InProgress,
                assignee: Some("agt_test1".into()),
                // Out of attempts, so the failure is final and cascades
                // instead of queueing a retry.
                attempts: 1,
                max_attempts: 1,
                ..make_task()
            },
            Task {
                id: "tsk_left".into(),
                title: "left".into(),
                depends_on: vec!["tsk_root".into()],
                ..make_task()
            },
            Task {
                id: "tsk_right".into(),
                title: "right".into(),
                depends_on: vec!["tsk_root".into()],
                ..make_task()
            },
        ])
        .agents([Agent {
            id: "agt_test1".into(),
            status: AgentStatus::Running,
            current_task: Some("tsk_root".into()),
            ..make_agent()
        }])
        .config(config)
        .state(super::harness::state_running(
            "tsk_root",
            "agt_test1",
            "run_1",
            11111,
            NOW_ISO,
        ))
        .wrap_task_store(move |inner| {
            let wrapped = Arc::new(OrderedSaveProbe {
                inner,
                saved: std::sync::Mutex::new(Vec::new()),
                // The first dependent the cascade visits.
                slow: "tsk_left",
            });
            *captured.lock().unwrap() = Some(Arc::clone(&wrapped));
            wrapped
        })
        .build();
    h.runs.seed(orch_domain::Run {
        id: "run_1".into(),
        task_id: "tsk_root".into(),
        agent_id: "agt_test1".into(),
        status: orch_domain::RunStatus::Running,
        ..orch_core::test_support::make_run()
    });

    h.load_state().await.unwrap();

    // The failure path directly: the root's run errors, which fails the task
    // and cascades to both dependents.
    h.orchestrator
        .collect_events(
            Box::pin(futures::stream::iter(vec![Err(
                orch_core::adapter::AdapterError::new("boom", None),
            )])),
            "run_1",
            "tsk_root",
            "agt_test1",
        )
        .await
        .unwrap();

    let probe = probe.lock().unwrap().clone().expect("probe installed");
    let saved = probe.saved.lock().unwrap().clone();
    let cascaded: Vec<&str> = saved
        .iter()
        .map(String::as_str)
        .filter(|id| *id == "tsk_left" || *id == "tsk_right")
        .collect();
    assert_eq!(
        cascaded,
        vec!["tsk_left", "tsk_right"],
        "the cascade transposed its dependents: {saved:?}"
    );
}
