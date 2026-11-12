//! A failed early release must retain an obligation before reading state.
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use orch_core::error::{Error, Result};
use orch_core::storage::RunStore;
use orch_core::test_support::{FakeRunStore, make_agent, make_task};
use orch_domain::{Run, RunEvent, RunStatus, Task};
use tokio_util::sync::CancellationToken;

use super::harness::Harness;

const TASK: &str = "tsk_test1";
const AGENT: &str = "agt_test1";
type Arm = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

struct FailCreate {
    inner: Arc<FakeRunStore>,
    arm: Arm,
}

#[async_trait]
impl RunStore for FailCreate {
    async fn save(&self, run: &Run) -> Result<()> {
        if run.status == RunStatus::Preparing {
            if let Some(arm) = self.arm.lock().unwrap().take() {
                arm();
                return Err(Error::other("run create failed"));
            }
        }
        self.inner.save(run).await
    }
    async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.inner.get(id).await
    }
    async fn list_all(&self) -> Result<Vec<Run>> {
        self.inner.list_all().await
    }
    async fn list_for_task(&self, t: &str) -> Result<Vec<Run>> {
        self.inner.list_for_task(t).await
    }
    async fn list_for_agent(&self, a: &str) -> Result<Vec<Run>> {
        self.inner.list_for_agent(a).await
    }
    async fn append_event(&self, r: &str, e: &RunEvent) -> Result<()> {
        self.inner.append_event(r, e).await
    }
    async fn read_events(&self, r: &str) -> Result<Vec<RunEvent>> {
        self.inner.read_events(r).await
    }
    async fn read_events_tail(&self, r: &str, c: usize) -> Result<Vec<RunEvent>> {
        self.inner.read_events_tail(r, c).await
    }
    fn stream_events(&self, r: &str, t: CancellationToken) -> BoxStream<'static, Result<RunEvent>> {
        self.inner.stream_events(r, t)
    }
    fn close_run_events(&self, r: &str) {
        self.inner.close_run_events(r);
    }
}

#[tokio::test]
async fn a_failed_state_read_in_the_error_path_release_is_owned_and_reconciled() {
    let arm: Arm = Arc::new(Mutex::new(None));
    let h = Harness::builder()
        .tasks([Task {
            assignee: Some(AGENT.into()),
            ..make_task()
        }])
        .agents([make_agent()])
        .wrap_run_store({
            let arm = Arc::clone(&arm);
            move |inner| Arc::new(FailCreate { inner, arm }) as Arc<dyn RunStore>
        })
        .build();
    h.orchestrator.enable_daemon_mode();
    let state = Arc::clone(&h.state);
    *arm.lock().unwrap() = Some(Box::new(move || state.faults.fail_once("state unreadable")));

    assert!(h.orchestrator.daemon_launch_task(TASK).await.is_err());
    assert!(
        !h.state.faults.is_armed(),
        "the error-path release never read state"
    );
    assert!(h.persisted_state().claimed.contains(TASK));

    h.orchestrator.daemon_tick(true).await.unwrap();
    assert!(
        h.persisted_state().claimed.is_empty(),
        "stranded claim: no cleanup owner"
    );
    assert!(
        h.orchestrator
            .inner
            .preparing_agents
            .lock()
            .await
            .is_empty(),
        "agent still reserved"
    );
}
