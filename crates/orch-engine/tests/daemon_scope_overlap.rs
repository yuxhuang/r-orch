//! Scope observations through daemon selection, with an explicitly driven clock.
#[allow(
    dead_code,
    reason = "shared support also serves other integration targets"
)]
mod common;

use async_trait::async_trait;
use common::{PassingCommandRunner, ScriptedProcessManager};
use futures::FutureExt;
use orch_core::adapter::AdapterRegistry;
use orch_core::error::{Error, Result};
use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::*;
use orch_domain::*;
use orch_engine::container::commit::Publication;
use orch_engine::events::{EventBus, Unsubscribe};
use orch_engine::lock::InMemoryLock;
use orch_engine::orchestrator::{Orchestrator, OrchestratorDeps};
use orch_engine::{AgentService, RunService, TaskService};
use orch_infra::adapters::ShellAdapter;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const R: i64 = 300_000;
const T0: i64 = 1_760_373_453_426;

#[derive(Default)]
struct Tasks {
    inner: FakeTaskStore,
    fail_list: AtomicBool,
}
#[async_trait]
impl TaskStore for Tasks {
    async fn list(&self, f: &TaskFilter) -> Result<Vec<Task>> {
        if self.fail_list.load(Ordering::SeqCst) {
            return Err(Error::other("scope snapshot failed"));
        }
        self.inner.list(f).await
    }
    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }
    async fn save(&self, task: &Task) -> Result<()> {
        self.inner.save(task).await
    }
    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}
fn task(id: &str, scope: &[&str]) -> Task {
    Task {
        id: id.into(),
        scope: Some(scope.iter().map(|s| (*s).into()).collect()),
        ..make_task()
    }
}
struct Rig {
    orch: Orchestrator,
    tasks: Arc<Tasks>,
    state: Arc<FakeStateStore>,
    agents: Arc<FakeAgentStore>,
    runs: Arc<FakeRunStore>,
    goals: Arc<FakeGoalStore>,
    clock: Arc<FixedClock>,
    bus: Arc<EventBus>,
    seen: Arc<Mutex<Vec<OrchestratorEvent>>>,
    _sub: Unsubscribe,
}
impl Rig {
    fn new() -> Self {
        let tasks = Arc::new(Tasks::default());
        tasks.inner.seed(task("A", &["src/**"]));
        tasks.inner.seed(task("B", &["src/auth/**"]));
        let mut state = default_state();
        state.claimed.insert("A".into());
        let state = Arc::new(FakeStateStore::with_state(state));
        let agents = Arc::new(FakeAgentStore::new());
        let runs = Arc::new(FakeRunStore::new());
        let goals = Arc::new(FakeGoalStore::new());
        let bus = Arc::new(EventBus::new());
        let clock = Arc::new(FixedClock::at(T0));
        let ids = Arc::new(SeqIdGenerator::new());
        let config = default_config();
        let processes = ScriptedProcessManager::new(4242);
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(ShellAdapter::new(processes.clone())));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let sub = bus.on_any(move |e| sink.lock().unwrap().push(e.clone()));
        let deps = OrchestratorDeps {
            task_store: tasks.clone(),
            agent_store: agents.clone(),
            run_store: runs.clone(),
            state_store: state.clone(),
            adapters: Arc::new(registry),
            workspace: Arc::new(FakeWorkspaceManager::new()),
            templates: Arc::new(FakeTemplateEngine::new()),
            processes,
            events: bus.clone(),
            task_service: Arc::new(TaskService::new(
                tasks.clone(),
                agents.clone(),
                bus.clone(),
                config.clone(),
                clock.clone(),
                ids.clone(),
                None,
            )),
            agent_service: Arc::new(AgentService::new(
                agents.clone(),
                state.clone(),
                bus.clone(),
                config.clone(),
                ids.clone(),
            )),
            run_service: Arc::new(RunService::new(
                runs.clone(),
                bus.clone(),
                clock.clone(),
                ids,
            )),
            clock: clock.clone(),
            lock: Arc::new(InMemoryLock::new()),
            command_runner: Arc::new(PassingCommandRunner),
            config,
            project_root: std::env::temp_dir(),
            context_store: None,
            message_service: None,
            goal_store: Some(goals.clone()),
            skill_loader: None,
        };
        let orch = Orchestrator::new(deps);
        orch.enable_daemon_mode();
        Self {
            orch,
            tasks,
            state,
            agents,
            runs,
            goals,
            clock,
            bus,
            seen,
            _sub: sub,
        }
    }
    async fn eval(&self) {
        self.orch
            .daemon_launch_task_with_guard("B", Arc::new(AtomicBool::new(false)), false)
            .await
            .unwrap();
    }
    async fn tick(&self) {
        self.orch.daemon_tick(true).await.unwrap();
    }
    async fn repeats(&self) {
        for _ in 0..100 {
            self.eval().await;
            self.tick().await;
        }
    }
    fn overlaps(&self) -> Vec<OrchestratorEvent> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }))
            .cloned()
            .collect()
    }
    fn count(&self, n: usize) {
        assert_eq!(self.overlaps().len(), n, "overlap count");
    }
    fn pair(&self, blocker: &str) {
        assert_eq!(
            self.overlaps().last(),
            Some(&OrchestratorEvent::TaskScopeOverlap {
                task_id: "B".into(),
                overlapping_task_id: blocker.into(),
                patterns: self.tasks.inner.peek("B").unwrap().scope.unwrap()
            })
        );
    }
    fn reserve(&self, ids: &[&str]) {
        let mut s = self.state.peek();
        s.claimed = ids.iter().map(|s| (*s).to_owned()).collect();
        self.state.seed(s);
    }
}

#[tokio::test]
async fn t1_first_hold_and_completed_repeats_do_not_write() {
    let r = Rig::new();
    let before = r.tasks.inner.peek("B").unwrap();
    let seq = r.bus.operation_boundary().current_commit_seq();
    r.eval().await;
    r.count(1);
    r.pair("A");
    r.repeats().await;
    r.count(1);
    assert_eq!(r.tasks.inner.peek("B").unwrap(), before);
    assert!(r.tasks.inner.saved().is_empty());
    assert!(r.runs.snapshot().is_empty());
    assert!(r.state.written().is_empty());
    assert_eq!(r.bus.operation_boundary().current_commit_seq(), seq);
}
#[tokio::test]
async fn t2_free_later_candidate_runs_after_overlap_publication() {
    let r = Rig::new();
    r.reserve(&[]);
    for (id, priority, scope) in [
        ("A", 1, "src/**"),
        ("B", 2, "src/auth/**"),
        ("C", 3, "docs/**"),
    ] {
        let mut t = task(id, &[scope]);
        t.priority = priority;
        r.tasks.inner.seed(t);
    }
    // Real adapter, synthetic child pipes stay open until this runtime ends.
    let agent_store = r.agents.clone();
    for id in ["worker1", "worker2"] {
        agent_store.seed(Agent {
            id: id.into(),
            name: id.into(),
            ..make_agent()
        });
    }
    r.tick().await;
    r.count(1);
    let seen = r.seen.lock().unwrap();
    let pos = |id: &str| {
        seen.iter()
            .position(
                |e| matches!(e, OrchestratorEvent::AgentStarted {task_id, ..} if task_id == id),
            )
            .unwrap()
    };
    let overlap = seen
        .iter()
        .position(|e| matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }))
        .unwrap();
    assert!(pos("A") < overlap && overlap < pos("C"));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, OrchestratorEvent::AgentStarted {task_id, ..} if task_id == "B"))
    );
}
#[tokio::test]
async fn t3_earlier_exclusions_and_unscoped_candidates_emit_nothing() {
    for kind in 0..8 {
        let r = Rig::new();
        let mut b = r.tasks.inner.peek("B").unwrap();
        match kind {
            0 => b.depends_on = vec!["A".into()],
            1 => {
                b.goal_id = Some("goal".into());
                r.goals.seed(Goal {
                    id: "goal".into(),
                    title: "paused".into(),
                    description: String::new(),
                    status: GoalStatus::Paused,
                    assignee: None,
                    created_at: String::new(),
                    updated_at: None,
                });
            }
            2 => {
                b.labels.push(AWAITING_APPROVAL_LABEL.into());
            }
            3 => {
                b.status = TaskStatus::Retrying;
                let mut s = r.state.peek();
                s.retry_queue.push(RetryEntry {
                    task_id: "B".into(),
                    attempt: 1,
                    due_at: "2099-01-01T00:00:00Z".into(),
                    error: String::new(),
                });
                r.state.seed(s);
            }
            4 => r.reserve(&["A", "B"]),
            5 => {
                r.orch
                    .daemon_launch_task_with_guard("B", Arc::new(AtomicBool::new(true)), false)
                    .await
                    .unwrap();
                r.count(0);
                continue;
            }
            6 => b.scope = None,
            _ => b.scope = Some(vec![]),
        }
        r.tasks.inner.seed(b);
        r.eval().await;
        r.count(0);
        assert!(r.runs.snapshot().is_empty());
    }
    let r = Rig::new();
    r.orch.daemon_stop().await.unwrap();
    r.eval().await;
    r.count(0);
}
#[tokio::test]
async fn t4_stable_blocker_survives_reorder_and_claim_to_running() {
    let r = Rig::new();
    r.tasks.inner.seed(task("D", &["src/**"]));
    r.reserve(&["D", "A"]);
    r.eval().await;
    r.count(1);
    r.pair("A");
    let a = r.tasks.inner.peek("A").unwrap();
    r.tasks.save(&a).await.unwrap();
    r.tasks.inner.seed(task("0", &["src/**"]));
    r.reserve(&["D", "0", "A"]);
    r.eval().await;
    r.count(1);
    let mut s = r.state.peek();
    s.claimed.shift_remove("A");
    s.running.insert(
        "A".into(),
        RunningEntry {
            task_id: "A".into(),
            run_id: "run-a".into(),
            agent_id: "worker".into(),
            pid: 4242,
            started_at: "2025-10-13T16:37:33.426Z".into(),
            last_event_at: "2025-10-13T16:37:33.426Z".into(),
            remote: None,
        },
    );
    r.state.seed(s);
    r.eval().await;
    r.count(1);
}
#[tokio::test]
async fn t5_replacement_names_the_remaining_real_blocker() {
    let r = Rig::new();
    r.tasks.inner.seed(task("D", &["src/**"]));
    r.reserve(&["A", "D"]);
    r.eval().await;
    r.count(1);
    r.reserve(&["D"]);
    let mut a = r.tasks.inner.peek("A").unwrap();
    a.status = TaskStatus::Done;
    r.tasks.inner.seed(a);
    r.tick().await;
    r.eval().await;
    r.count(2);
    r.pair("D");
}
#[tokio::test]
async fn t6_observed_end_rearms_the_same_pair() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.reserve(&[]);
    r.eval().await;
    r.reserve(&["A"]);
    r.eval().await;
    r.count(2);
    let mut a = r.tasks.inner.peek("A").unwrap();
    a.scope = Some(vec!["docs/**".into()]);
    r.tasks.inner.seed(a.clone());
    r.eval().await;
    a.scope = Some(vec!["src/**".into()]);
    r.tasks.inner.seed(a);
    r.eval().await;
    r.count(3);
}
#[tokio::test]
async fn t8_fresh_instance_emits_its_first_observation() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.eval().await;
    r.count(1);
    let restarted = Rig::new();
    restarted.eval().await;
    restarted.count(1);
    restarted.pair("A");
}
#[tokio::test]
async fn t9_publication_is_identical_after_permit_release_and_future_only() {
    let r = Rig::new();
    let published = Arc::new(Mutex::new(Vec::new()));
    let sink = published.clone();
    r.bus
        .operation_boundary()
        .set_publisher(Some(Arc::new(move |p| {
            if let Publication::Bus(e) = p
                && matches!(e.as_ref(), OrchestratorEvent::TaskScopeOverlap { .. })
            {
                sink.lock().unwrap().push(e.as_ref().clone());
            }
        })));
    let acquired = Arc::new(Mutex::new(Vec::new()));
    let sink = acquired.clone();
    let boundary = r.bus.operation_boundary();
    let _probe = r.bus.on_any(move |e| {
        if matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }) {
            sink.lock().unwrap().push(
                std::thread::spawn({
                    let boundary = boundary.clone();
                    move || boundary.run(false, async {}).now_or_never().is_some()
                })
                .join()
                .unwrap(),
            );
        }
    });
    r.eval().await;
    r.count(1);
    assert_eq!(*acquired.lock().unwrap(), vec![true]);
    assert_eq!(*published.lock().unwrap(), r.overlaps());
    let late = Arc::new(Mutex::new(Vec::new()));
    let sink = late.clone();
    let _late = r.bus.on_any(move |e| {
        if matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }) {
            sink.lock().unwrap().push(e.clone());
        }
    });
    r.eval().await;
    assert!(late.lock().unwrap().is_empty());
    r.tasks.inner.seed(task("D", &["src/**"]));
    r.reserve(&["D"]);
    r.eval().await;
    r.count(2);
    assert_eq!(*late.lock().unwrap(), r.overlaps()[1..]);
    assert_eq!(*published.lock().unwrap(), r.overlaps());
    assert!(acquired.lock().unwrap().iter().all(|v| *v));
}
#[tokio::test]
async fn t10_failed_snapshot_preserves_the_previous_observation() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.tasks.fail_list.store(true, Ordering::SeqCst);
    let error = r.orch.daemon_launch_task("B").await.unwrap_err();
    assert!(error.to_string().contains("scope snapshot failed"));
    r.count(1);
    assert!(r.orch.daemon_tick(true).await.is_err());
    r.count(1);
    r.tasks.fail_list.store(false, Ordering::SeqCst);
    r.eval().await;
    r.count(1);
}
#[tokio::test]
async fn t11_payload_preserves_unicode_order_duplicates_and_keys() {
    let r = Rig::new();
    r.tasks
        .inner
        .seed(task("B", &["src/日本/**", "src/日本/**", "src/z"]));
    r.eval().await;
    r.count(1);
    assert_eq!(
        serde_json::to_value(&r.overlaps()[0]).unwrap(),
        serde_json::json!({"type":"task:scope_overlap","taskId":"B","overlappingTaskId":"A","patterns":["src/日本/**","src/日本/**","src/z"]})
    );
}
#[tokio::test]
async fn t13_reminders_use_emission_time_and_inclusive_boundary() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.repeats().await;
    r.count(1);
    r.clock.set_ms(T0 + R - 1);
    r.repeats().await;
    r.count(1);
    let mut b = r.tasks.inner.peek("B").unwrap();
    b.scope = Some(vec!["src/current/**".into(), "src/current/**".into()]);
    r.tasks.inner.seed(b);
    r.clock.set_ms(T0 + R);
    r.eval().await;
    r.count(2);
    r.pair("A");
    r.repeats().await;
    r.count(2);
    r.clock.set_ms(T0 + 2 * R);
    r.eval().await;
    r.count(3);
    r.pair("A");
    assert!(r.tasks.inner.saved().is_empty());
    assert!(r.runs.snapshot().is_empty());
    assert!(r.state.written().is_empty());
    assert_eq!(r.bus.operation_boundary().current_commit_seq(), 0);
}
#[tokio::test]
async fn t14_dispatch_gaps_retain_time_and_manual_evaluation_consumes_reminder() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.clock.set_ms(T0 + R - 1);
    r.eval().await;
    r.eval().await;
    r.count(1);
    r.clock.set_ms(T0 + R);
    r.orch.daemon_launch_task("B").await.unwrap();
    r.count(2);
    r.tick().await;
    r.count(2);
    let r = Rig::new();
    r.eval().await;
    r.clock.set_ms(T0 + 3 * R);
    r.tick().await;
    r.count(2);
    r.tick().await;
    r.count(2);
}
#[tokio::test]
async fn t15_late_subscriber_receives_the_due_reminder_without_replay() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    r.clock.set_ms(T0 + 120_000);
    let late = Arc::new(Mutex::new(Vec::new()));
    let sink = late.clone();
    let _sub = r.bus.on_any(move |e| {
        if matches!(e, OrchestratorEvent::TaskScopeOverlap { .. }) {
            sink.lock().unwrap().push(e.clone());
        }
    });
    assert!(late.lock().unwrap().is_empty());
    r.clock.set_ms(T0 + R - 1);
    r.eval().await;
    r.count(1);
    assert!(late.lock().unwrap().is_empty());
    r.clock.set_ms(T0 + R);
    r.eval().await;
    assert_eq!(late.lock().unwrap().len(), 1, "late listener due reminder");
    r.count(2);
    assert_eq!(*late.lock().unwrap(), r.overlaps()[1..]);
    r.eval().await;
    r.count(2);
    assert_eq!(late.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn t16_overdue_intervals_coalesce_from_the_actual_emission() {
    let r = Rig::new();
    r.eval().await;
    r.clock.set_ms(T0 + 3 * R + 1);
    r.repeats().await;
    r.count(2);
    r.clock.set_ms(T0 + 4 * R);
    r.eval().await;
    r.count(2);
    r.clock.set_ms(T0 + 4 * R + 1);
    r.eval().await;
    r.count(3);
}
#[tokio::test]
async fn t17_backward_and_far_forward_clock_steps_do_not_burst() {
    let r = Rig::new();
    r.eval().await;
    r.clock.set_ms(T0 - 1);
    r.repeats().await;
    r.count(1);
    r.clock.set_ms(T0 + R - 1);
    r.eval().await;
    r.count(1);
    r.clock.set_ms(T0 + R);
    r.eval().await;
    r.count(2);
    r.clock.set_ms(T0 + 1000 * R);
    r.repeats().await;
    r.count(3);
}
#[tokio::test]
async fn t18_changed_blocker_resets_deadline_and_coalesces_due_change() {
    for change_at in [R - 1, R] {
        let r = Rig::new();
        r.eval().await;
        r.count(1);
        r.tasks.inner.seed(task("D", &["src/**"]));
        r.reserve(&["D"]);
        r.clock.set_ms(T0 + change_at);
        r.eval().await;
        r.count(2);
        r.pair("D");
        r.clock.set_ms(T0 + R);
        r.eval().await;
        r.count(2);
        r.clock.set_ms(T0 + change_at + R - 1);
        r.eval().await;
        r.count(2);
        r.clock.set_ms(T0 + change_at + R);
        r.eval().await;
        r.count(3);
        assert!(r.overlaps().len() <= 1 + 2 + 1);
    }
}

#[tokio::test]
async fn t3_earlier_waits_retain_an_existing_episode() {
    let r = Rig::new();
    r.eval().await;
    r.count(1);
    let original = r.tasks.inner.peek("B").unwrap();
    let mut waiting = original.clone();
    waiting.depends_on = vec!["A".into()];
    r.tasks.inner.seed(waiting);
    r.tick().await;
    r.count(1);
    waiting = original.clone();
    waiting.goal_id = Some("paused".into());
    r.goals.seed(Goal {
        id: "paused".into(),
        title: "paused".into(),
        description: String::new(),
        status: GoalStatus::Paused,
        assignee: None,
        created_at: String::new(),
        updated_at: None,
    });
    r.tasks.inner.seed(waiting);
    r.tick().await;
    r.count(1);
    waiting = original.clone();
    waiting.labels.push(AWAITING_APPROVAL_LABEL.into());
    r.tasks.inner.seed(waiting);
    r.tick().await;
    r.count(1);
    waiting = original.clone();
    waiting.status = TaskStatus::Retrying;
    let mut state = r.state.peek();
    state.retry_queue.push(RetryEntry {
        task_id: "B".into(),
        attempt: 1,
        due_at: "2099-01-01T00:00:00Z".into(),
        error: String::new(),
    });
    r.state.seed(state);
    r.tasks.inner.seed(waiting);
    r.eval().await;
    r.count(1);
    r.tasks.inner.seed(original);
    r.eval().await;
    r.count(1);
}
