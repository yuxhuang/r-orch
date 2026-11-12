//! Shared cascade callers must save and announce each dependent once, in
//! first-discovery order, even when several edges lead to the same task.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orch_core::adapter::AdapterError;
use orch_core::error::{Error, Result};
use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::{FakeTaskStore, make_agent, make_run, make_task};
use orch_domain::{
    Agent, AgentStatus, DomainError, OrchestratorEvent, RunStatus, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, state_running};

const ROOT: &str = "tsk_test1";
const AGENT: &str = "agt_test1";
const RUN: &str = "run_cascade";

#[derive(Clone, Copy)]
enum Caller {
    Workspace,
    Collector,
}

#[derive(Clone, Copy)]
enum Shape {
    Diamond,
    ThreeParents,
    RepeatedEdge,
    Cycle,
    Terminal,
}

fn dependent(id: &str, parents: &[&str]) -> Task {
    Task {
        id: id.into(),
        title: format!("current title of {id}"),
        depends_on: parents.iter().map(|id| (*id).to_owned()).collect(),
        assignee: None,
        ..make_task()
    }
}

fn graph(shape: Shape) -> (Vec<Task>, Vec<&'static str>) {
    let mut tasks = vec![dependent("a", &[ROOT]), dependent("b", &[ROOT])];
    let mut order = vec!["a", "b", "c"];
    match shape {
        Shape::ThreeParents => {
            tasks.push(dependent("p", &[ROOT]));
            tasks.push(dependent("c", &["a", "b", "p"]));
            order = vec!["a", "b", "p", "c"];
        }
        _ => tasks.push(dependent("c", &["a", "b"])),
    }
    match shape {
        Shape::RepeatedEdge => tasks[0].depends_on.push(ROOT.into()),
        Shape::Cycle => tasks[0].depends_on.push("c".into()),
        Shape::Terminal => {
            for (id, status) in [
                ("done", TaskStatus::Done),
                ("failed", TaskStatus::Failed),
                ("cancelled", TaskStatus::Cancelled),
            ] {
                tasks.push(Task {
                    status,
                    ..dependent(id, &[ROOT, "a", "b"])
                });
                tasks.push(dependent(&format!("after_{id}"), &[id]));
            }
        }
        _ => {}
    }
    // A non-todo dependent also keeps its own original status in the event.
    tasks[1].status = TaskStatus::Retrying;
    (tasks, order)
}

/// Counts every attempted save, including one that fails before or after
/// persisting its record. The latter models a partial entity/index write.
struct SaveProbe {
    inner: Arc<FakeTaskStore>,
    attempts: Arc<Mutex<Vec<Task>>>,
    fault: Option<bool>,
}

#[async_trait]
impl TaskStore for SaveProbe {
    async fn list(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.inner.list(filter).await
    }

    async fn get(&self, id: &str) -> Result<Option<Task>> {
        self.inner.get(id).await
    }

    async fn save(&self, task: &Task) -> Result<()> {
        self.attempts.lock().unwrap().push(task.clone());
        if task.id == "c"
            && let Some(partial) = self.fault
        {
            if partial {
                self.inner.save(task).await?;
            }
            return Err(Error::other("dependent save failed"));
        }
        self.inner.save(task).await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id).await
    }
}

fn assert_cascade(h: &Harness, originals: &[Task], order: &[&str], reason: &str) {
    let actual: Vec<_> = h
        .events()
        .into_iter()
        .filter(|event| match event {
            OrchestratorEvent::TaskCascadeFailed { .. } => true,
            OrchestratorEvent::TaskStatusChanged { task_id, .. } => task_id != ROOT,
            _ => false,
        })
        .collect();
    let expected: Vec<_> = order
        .iter()
        .flat_map(|id| {
            let original = originals.iter().find(|task| task.id == *id).unwrap();
            [
                OrchestratorEvent::TaskStatusChanged {
                    task_id: (*id).into(),
                    from: original.status,
                    to: TaskStatus::Failed,
                },
                OrchestratorEvent::TaskCascadeFailed {
                    task_id: (*id).into(),
                    failed_dependency_id: ROOT.into(),
                    reason: reason.into(),
                },
            ]
        })
        .collect();
    assert_eq!(actual, expected, "cascade event amplification or order");
}

async fn exercise(caller: Caller, shape: Shape) {
    let dir = tempfile::tempdir().unwrap();
    let (dependents, order) = graph(shape);
    let root = Task {
        assignee: Some(AGENT.into()),
        status: match caller {
            Caller::Workspace => TaskStatus::Retrying,
            Caller::Collector => TaskStatus::InProgress,
        },
        attempts: 2,
        max_attempts: 3,
        ..make_task()
    };
    let state = match caller {
        Caller::Workspace => Default::default(),
        Caller::Collector => state_running(ROOT, AGENT, RUN, 11111, NOW_ISO),
    };
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let sink = attempts.clone();
    let h = Harness::builder()
        .project_root(dir.path().to_owned())
        .tasks(std::iter::once(root.clone()).chain(dependents.clone()))
        .agents([Agent {
            status: match caller {
                Caller::Workspace => AgentStatus::Idle,
                Caller::Collector => AgentStatus::Running,
            },
            ..make_agent()
        }])
        .state(state)
        .wrap_task_store(move |inner| {
            Arc::new(SaveProbe {
                inner,
                attempts: sink,
                fault: None,
            })
        })
        .build();
    h.orchestrator.enable_daemon_mode();
    let reason = match caller {
        Caller::Workspace => {
            let error = DomainError::Workspace {
                message: "workspace unavailable".into(),
                hint: None,
            };
            h.workspace.faults.fail_always_with(error.clone());
            let result = h.orchestrator.daemon_launch_task(ROOT).await;
            assert_eq!(
                result.unwrap_err().to_string(),
                Error::Domain(error.clone()).to_string()
            );
            format!("dependency {ROOT} failed: {}", Error::Domain(error))
        }
        Caller::Collector => {
            // At the final attempt, a live collector calls the shared cascade.
            h.tasks.seed(Task {
                attempts: 3,
                ..root
            });
            h.runs.seed(orch_domain::Run {
                id: RUN.into(),
                task_id: ROOT.into(),
                agent_id: AGENT.into(),
                status: RunStatus::Running,
                ..make_run()
            });
            h.load_state().await.unwrap();
            h.orchestrator
                .collect_events(
                    Box::pin(futures::stream::iter([Err(AdapterError::new(
                        "boom", None,
                    ))])),
                    RUN,
                    ROOT,
                    AGENT,
                )
                .await
                .unwrap();
            format!("dependency {ROOT} failed: boom")
        }
    };
    assert_eq!(h.task(ROOT).status, TaskStatus::Failed);
    let saves: Vec<_> = attempts
        .lock()
        .unwrap()
        .iter()
        .map(|t| t.id.clone())
        .collect();
    let expected: Vec<_> = std::iter::once(ROOT).chain(order.iter().copied()).collect();
    assert_eq!(saves, expected, "cascade save amplification or order");
    assert_cascade(&h, &dependents, &order, &reason);
    for original in &dependents {
        let current = h.task(&original.id);
        let eligible = order.contains(&original.id.as_str());
        assert_eq!(
            current.status,
            if eligible {
                TaskStatus::Failed
            } else {
                original.status
            }
        );
        assert_eq!(current.title, original.title);
    }
}

#[tokio::test]
async fn workspace_diamond() {
    exercise(Caller::Workspace, Shape::Diamond).await;
}
#[tokio::test]
async fn collector_diamond() {
    exercise(Caller::Collector, Shape::Diamond).await;
}
#[tokio::test]
async fn workspace_three_parents() {
    exercise(Caller::Workspace, Shape::ThreeParents).await;
}
#[tokio::test]
async fn collector_three_parents() {
    exercise(Caller::Collector, Shape::ThreeParents).await;
}
#[tokio::test]
async fn workspace_repeated_edge() {
    exercise(Caller::Workspace, Shape::RepeatedEdge).await;
}
#[tokio::test]
async fn collector_repeated_edge() {
    exercise(Caller::Collector, Shape::RepeatedEdge).await;
}
#[tokio::test]
async fn workspace_cycle() {
    exercise(Caller::Workspace, Shape::Cycle).await;
}
#[tokio::test]
async fn collector_cycle() {
    exercise(Caller::Collector, Shape::Cycle).await;
}
#[tokio::test]
async fn workspace_terminal() {
    exercise(Caller::Workspace, Shape::Terminal).await;
}
#[tokio::test]
async fn collector_terminal() {
    exercise(Caller::Collector, Shape::Terminal).await;
}

async fn failed_save_invalidates(partial: bool) {
    let dir = tempfile::tempdir().unwrap();
    let (tasks, order) = graph(Shape::Diamond);
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let sink = attempts.clone();
    let h = Harness::builder()
        .project_root(dir.path().to_owned())
        .tasks(tasks.clone())
        .wrap_task_store(move |inner| {
            Arc::new(SaveProbe {
                inner,
                attempts: sink,
                fault: Some(partial),
            })
        })
        .build();
    let cached = &h.orchestrator.inner.cached_tasks;
    assert_eq!(cached.list(&Default::default()).await.unwrap(), tasks);
    h.orchestrator
        .cascade_fail_dependents(ROOT, &tasks, "failed dependency")
        .await;
    let saves: Vec<_> = attempts
        .lock()
        .unwrap()
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(saves, order, "a failed save is still attempted only once");
    // F emits the failure-intent pair even on a save error. It does not
    // report a successful save or suppress cache invalidation on that error.
    assert_cascade(&h, &tasks, &order, "failed dependency");
    let refreshed = cached.list(&Default::default()).await.unwrap();
    for task in refreshed {
        let expected = if task.id == "c" && !partial {
            TaskStatus::Todo
        } else {
            TaskStatus::Failed
        };
        assert_eq!(task.status, expected, "failed save left a stale cache");
        assert_eq!(
            task,
            h.task(&task.id),
            "cache must reflect the actual stored record"
        );
    }
}

#[tokio::test]
async fn failed_save_keeps_cache_invalidation() {
    failed_save_invalidates(false).await;
}
#[tokio::test]
async fn partial_save_keeps_cache_invalidation() {
    failed_save_invalidates(true).await;
}
