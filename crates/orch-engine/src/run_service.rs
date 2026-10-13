//! Run service — ported from `src/application/run-service.ts`.
//!
//! Almost every method is a pass-through to [`RunStore`]; the service exists
//! for the three that are not — `start` and `finish`, which mutate the record
//! *and* emit, and [`RunService::get_last_failed_run_context`], which builds
//! the retry context an agent is re-prompted with.

use std::sync::Arc;

use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::RunStore;
use orch_domain::{OrchestratorEvent, Run, RunEvent, RunEventType, RunStatus, TokenUsage};

use crate::events::EventBus;

/// How many trailing events the retry context reads. The TS comment calls this
/// out as OOM protection on a long run (`run-service.ts:126`), and the ported
/// test asserts the cap, so it is observable rather than a tuning knob.
const RETRY_CONTEXT_EVENTS: usize = 50;

/// `create`'s parameters.
///
/// The TS signature takes an inline object literal (`run-service.ts:16`) and
/// `orch-domain` has no `CreateRunInput`, so it is declared next to its only
/// caller. A struct rather than five positional arguments because three of
/// them are strings and swapping two would type-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRunInput {
    pub task_id: String,
    pub agent_id: String,
    pub attempt: u32,
    pub prompt: String,
    pub workspace_path: String,
}

/// What a retried agent is told about its predecessor's failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryContext {
    pub error: String,
    pub output: String,
}

pub struct RunService {
    runs: Arc<dyn RunStore>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl RunService {
    pub fn new(
        runs: Arc<dyn RunStore>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        RunService {
            runs,
            events,
            clock,
            ids,
        }
    }

    /// Record a `preparing` run. **No event** — the TS version emits nothing
    /// here either, and `agent:started` is `start`'s job, so a run that never
    /// launches leaves no trace on the bus.
    pub async fn create(&self, input: CreateRunInput) -> Result<Run> {
        let run = Run {
            id: self.ids.generate("run"),
            task_id: input.task_id,
            agent_id: input.agent_id,
            attempt: input.attempt,
            status: RunStatus::Preparing,
            started_at: self.clock.now_iso(),
            finished_at: None,
            workspace_path: input.workspace_path,
            prompt: input.prompt,
            pid: None,
            error: None,
            tokens: None,
        };

        self.runs.save(&run).await?;
        Ok(run)
    }

    /// `None` for a missing run, matching the TS `Run | null`. The callers that
    /// need a hard failure go through `start`/`finish`.
    pub async fn get(&self, id: &str) -> Result<Option<Run>> {
        self.runs.get(id).await
    }

    pub async fn start(&self, id: &str, pid: u32) -> Result<Run> {
        let mut run = self.require(id).await?;

        run.status = RunStatus::Running;
        run.pid = Some(pid);
        self.runs.save(&run).await?;

        self.events.emit(OrchestratorEvent::AgentStarted {
            agent_id: run.agent_id.clone(),
            task_id: run.task_id.clone(),
            run_id: id.to_owned(),
        });

        Ok(run)
    }

    /// `tokens` and `error` are **overwritten, not merged**: the TS assigns the
    /// parameters straight onto the record (`run-service.ts:71-72`), so passing
    /// `None` clears a value an earlier call set. Replicated — the orchestrator
    /// calls `finish` exactly once per run, so the clearing is unreachable, and
    /// a merge here would be a silent behaviour change if that ever stopped
    /// being true.
    pub async fn finish(
        &self,
        id: &str,
        status: RunStatus,
        tokens: Option<TokenUsage>,
        error: Option<String>,
    ) -> Result<Run> {
        let mut run = self.require(id).await?;

        run.status = status;
        run.finished_at = Some(self.clock.now_iso());
        run.tokens = tokens;
        run.error = error;
        self.runs.save(&run).await?;

        self.events.emit(OrchestratorEvent::AgentCompleted {
            run_id: id.to_owned(),
            agent_id: run.agent_id.clone(),
            success: status == RunStatus::Succeeded,
        });

        Ok(run)
    }

    pub async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
        self.runs.append_event(run_id, event).await
    }

    pub async fn list_all(&self) -> Result<Vec<Run>> {
        self.runs.list_all().await
    }

    pub async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
        self.runs.list_for_task(task_id).await
    }

    pub async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
        self.runs.list_for_agent(agent_id).await
    }

    pub async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
        self.runs.read_events(run_id).await
    }

    pub async fn read_events_tail(&self, run_id: &str, count: usize) -> Result<Vec<RunEvent>> {
        self.runs.read_events_tail(run_id, count).await
    }

    /// The error and tail output of the most recent **failed** run of a task,
    /// so a retried agent can learn from the previous attempt.
    ///
    /// Two details are contract rather than taste. The ordering key is
    /// `finished_at` compared as a *string*, descending, with an absent value
    /// sorting as `""` (`run-service.ts:119`) — so a run that failed before it
    /// recorded a finish time always loses to one that recorded one. The TS
    /// uses `localeCompare`, which agrees with code-unit order on the
    /// fixed-width ISO-8601 timestamps the run store writes (PARITY.md §6.3).
    /// And a failing event read degrades to empty output instead of failing
    /// the retry: on disk the events file may simply not exist yet
    /// (`run-service.ts:134`).
    pub async fn get_last_failed_run_context(&self, task_id: &str) -> Result<Option<RetryContext>> {
        let mut failed: Vec<Run> = self
            .runs
            .list_for_task(task_id)
            .await?
            .into_iter()
            .filter(|run| run.status == RunStatus::Failed)
            .collect();
        // Stable, so equal `finished_at` keeps store order (clippy.toml).
        failed.sort_by(|a, b| finished_at(b).cmp(finished_at(a)));

        let Some(run) = failed.first() else {
            return Ok(None);
        };

        let error = run
            .error
            .clone()
            .unwrap_or_else(|| "Unknown error".to_owned());

        let output = match self
            .runs
            .read_events_tail(&run.id, RETRY_CONTEXT_EVENTS)
            .await
        {
            Ok(events) => events
                .iter()
                .filter(|event| {
                    matches!(
                        event.event_type,
                        RunEventType::AgentOutput | RunEventType::Error
                    )
                })
                .map(event_data)
                .collect::<Vec<String>>()
                .join("\n"),
            Err(err) => {
                tracing::debug!(
                    run_id = %run.id,
                    error = %err,
                    "no retry output: reading the run events failed"
                );
                String::new()
            }
        };

        Ok(Some(RetryContext { error, output }))
    }

    /// The TS throws a bare `Error`, not an `OrchestratorError`
    /// (`run-service.ts:44`), so this is exit 1 and carries no hint.
    async fn require(&self, id: &str) -> Result<Run> {
        self.runs
            .get(id)
            .await?
            .ok_or_else(|| Error::other(format!("Run not found: {id}")))
    }
}

/// `run.finished_at ?? ''` as a comparable key.
fn finished_at(run: &Run) -> &str {
    run.finished_at.as_deref().unwrap_or("")
}

/// `typeof e.data === 'string' ? e.data : JSON.stringify(e.data)`
/// (`run-service.ts:132`). A structured payload is therefore re-serialized,
/// not pretty-printed, and `Value::to_string` is the same rendering.
fn event_data(event: &RunEvent) -> String {
    match &event.data {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use orch_core::test_support::{FIXED_NOW_ISO, FakeRunStore, FixedClock, SeqIdGenerator};
    use orch_domain::OrchestratorEventType;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct Harness {
        service: RunService,
        runs: Arc<FakeRunStore>,
        bus: Arc<EventBus>,
    }

    fn harness() -> Harness {
        let runs = Arc::new(FakeRunStore::new());
        harness_over(Arc::clone(&runs) as Arc<dyn RunStore>, runs)
    }

    fn harness_over(store: Arc<dyn RunStore>, runs: Arc<FakeRunStore>) -> Harness {
        let bus = Arc::new(EventBus::new());
        let service = RunService::new(
            store,
            Arc::clone(&bus),
            Arc::new(FixedClock::new()),
            Arc::new(SeqIdGenerator::new()),
        );
        Harness { service, runs, bus }
    }

    /// `mockRejectedValueOnce` for a single method. [`orch_core::test_support`]
    /// arms faults per store, not per method, and the path under test calls
    /// `list_for_task` first — so a store-wide fault would fail the wrong call.
    struct FailingTail(Arc<FakeRunStore>);

    #[async_trait]
    impl RunStore for FailingTail {
        async fn save(&self, run: &Run) -> Result<()> {
            self.0.save(run).await
        }
        async fn get(&self, id: &str) -> Result<Option<Run>> {
            self.0.get(id).await
        }
        async fn list_all(&self) -> Result<Vec<Run>> {
            self.0.list_all().await
        }
        async fn list_for_task(&self, task_id: &str) -> Result<Vec<Run>> {
            self.0.list_for_task(task_id).await
        }
        async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Run>> {
            self.0.list_for_agent(agent_id).await
        }
        async fn append_event(&self, run_id: &str, event: &RunEvent) -> Result<()> {
            self.0.append_event(run_id, event).await
        }
        async fn read_events(&self, run_id: &str) -> Result<Vec<RunEvent>> {
            self.0.read_events(run_id).await
        }
        async fn read_events_tail(&self, _run_id: &str, _count: usize) -> Result<Vec<RunEvent>> {
            Err(Error::other("ENOENT"))
        }
        fn stream_events(
            &self,
            run_id: &str,
            token: CancellationToken,
        ) -> BoxStream<'static, Result<RunEvent>> {
            self.0.stream_events(run_id, token)
        }
        fn close_run_events(&self, run_id: &str) {
            self.0.close_run_events(run_id);
        }
    }

    fn failed(id: &str, error: Option<&str>, finished_at: &str) -> Run {
        Run {
            id: id.into(),
            task_id: "tsk_1".into(),
            status: RunStatus::Failed,
            error: error.map(str::to_owned),
            finished_at: Some(finished_at.into()),
            ..orch_core::test_support::make_run()
        }
    }

    fn event(event_type: RunEventType, data: serde_json::Value) -> RunEvent {
        RunEvent {
            timestamp: "2025-01-01T00:00:00Z".into(),
            event_type,
            data,
        }
    }

    fn output(text: &str) -> RunEvent {
        event(RunEventType::AgentOutput, json!(text))
    }

    #[tokio::test]
    async fn returns_none_when_no_failed_runs_exist() {
        let h = harness();
        h.runs.seed(Run {
            id: "run_1".into(),
            task_id: "tsk_1".into(),
            status: RunStatus::Succeeded,
            ..orch_core::test_support::make_run()
        });

        assert_eq!(
            h.service
                .get_last_failed_run_context("tsk_1")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn returns_none_when_no_runs_exist_for_the_task() {
        let h = harness();
        assert_eq!(
            h.service
                .get_last_failed_run_context("tsk_1")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn returns_the_error_and_output_of_the_most_recent_failed_run() {
        let h = harness();
        h.runs
            .seed(failed("run_1", Some("Old error"), "2025-01-01T00:00:00Z"));
        h.runs.seed(failed(
            "run_2",
            Some("Process crashed"),
            "2025-01-01T01:00:00Z",
        ));
        h.runs.seed_events(
            "run_2",
            [
                output("line 1"),
                output("line 2"),
                event(RunEventType::Error, json!("fatal error occurred")),
            ],
        );

        let context = h
            .service
            .get_last_failed_run_context("tsk_1")
            .await
            .unwrap()
            .expect("a failed run");
        assert_eq!(context.error, "Process crashed");
        assert_eq!(context.output, "line 1\nline 2\nfatal error occurred");
    }

    #[tokio::test]
    async fn reports_unknown_error_when_the_run_has_no_error_field() {
        let h = harness();
        h.runs.seed(failed("run_1", None, "2025-01-01T00:00:00Z"));

        let context = h
            .service
            .get_last_failed_run_context("tsk_1")
            .await
            .unwrap()
            .expect("a failed run");
        assert_eq!(context.error, "Unknown error");
    }

    #[tokio::test]
    async fn the_output_is_capped_at_the_last_fifty_events() {
        let h = harness();
        h.runs.seed(failed(
            "run_1",
            Some("Too much output"),
            "2025-01-01T00:00:00Z",
        ));
        h.runs
            .seed_events("run_1", (0..100).map(|n| output(&format!("line {n}"))));

        let context = h
            .service
            .get_last_failed_run_context("tsk_1")
            .await
            .unwrap()
            .expect("a failed run");
        assert_eq!(context.output.split('\n').count(), 50);
        assert!(!context.output.contains("line 0"));
        assert!(context.output.starts_with("line 50\n"));
        assert!(context.output.ends_with("\nline 99"));
    }

    #[tokio::test]
    async fn a_failing_events_read_degrades_to_empty_output() {
        let runs = Arc::new(FakeRunStore::new());
        let h = harness_over(
            Arc::new(FailingTail(Arc::clone(&runs))) as Arc<dyn RunStore>,
            runs,
        );
        h.runs
            .seed(failed("run_1", Some("Crashed"), "2025-01-01T00:00:00Z"));

        let context = h
            .service
            .get_last_failed_run_context("tsk_1")
            .await
            .unwrap()
            .expect("a failed run");
        assert_eq!(context.error, "Crashed");
        assert_eq!(context.output, "");
    }

    #[tokio::test]
    async fn only_agent_output_and_error_events_reach_the_output() {
        let h = harness();
        h.runs
            .seed(failed("run_1", Some("Failed"), "2025-01-01T00:00:00Z"));
        h.runs.seed_events(
            "run_1",
            [
                output("visible output"),
                event(RunEventType::FileChanged, json!("src/foo.ts")),
                event(RunEventType::ToolCall, json!({ "tool": "read" })),
                event(RunEventType::Error, json!("visible error")),
                event(RunEventType::CommandRun, json!("npm test")),
            ],
        );

        let context = h
            .service
            .get_last_failed_run_context("tsk_1")
            .await
            .unwrap()
            .expect("a failed run");
        assert_eq!(context.output, "visible output\nvisible error");
    }

    // ── Beyond the TS suite: the lifecycle methods it never exercises ──────

    #[tokio::test]
    async fn create_records_a_preparing_run_and_emits_nothing() {
        let h = harness();
        let mut sub = h.bus.subscribe();

        let run = h
            .service
            .create(CreateRunInput {
                task_id: "tsk_1".into(),
                agent_id: "agt_1".into(),
                attempt: 2,
                prompt: "do something".into(),
                workspace_path: "/workspace".into(),
            })
            .await
            .unwrap();

        assert_eq!(run.id, "run_0000001");
        assert_eq!(run.status, RunStatus::Preparing);
        assert_eq!(run.started_at, FIXED_NOW_ISO);
        assert_eq!(run.attempt, 2);
        assert_eq!(run.pid, None);
        assert_eq!(run.finished_at, None);
        assert_eq!(h.runs.peek("run_0000001"), Some(run));
        assert!(sub.try_drain().is_empty());
    }

    #[tokio::test]
    async fn start_sets_the_pid_and_emits_agent_started() {
        let h = harness();
        h.runs.seed(orch_core::test_support::make_run());
        let mut sub = h.bus.subscribe();

        let run = h.service.start("run_test1", 4242).await.unwrap();
        assert_eq!(run.status, RunStatus::Running);
        assert_eq!(run.pid, Some(4242));
        assert_eq!(h.runs.peek("run_test1").unwrap().pid, Some(4242));

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), OrchestratorEventType::AgentStarted);
        assert_eq!(
            *events[0],
            OrchestratorEvent::AgentStarted {
                agent_id: "agt_test1".into(),
                task_id: "tsk_test1".into(),
                run_id: "run_test1".into(),
            }
        );
    }

    #[tokio::test]
    async fn finish_stamps_the_finish_time_and_reports_success_per_status() {
        for (status, success) in [
            (RunStatus::Succeeded, true),
            (RunStatus::Failed, false),
            (RunStatus::TimedOut, false),
            (RunStatus::Cancelled, false),
        ] {
            let h = harness();
            h.runs.seed(orch_core::test_support::make_run());
            let mut sub = h.bus.subscribe();

            let run = h
                .service
                .finish("run_test1", status, None, Some("boom".into()))
                .await
                .unwrap();
            assert_eq!(run.status, status);
            assert_eq!(run.finished_at.as_deref(), Some(FIXED_NOW_ISO));
            assert_eq!(run.error.as_deref(), Some("boom"));

            let events = sub.try_drain();
            assert_eq!(
                *events[0],
                OrchestratorEvent::AgentCompleted {
                    run_id: "run_test1".into(),
                    agent_id: "agt_test1".into(),
                    success,
                }
            );
        }
    }

    #[tokio::test]
    async fn start_and_finish_report_run_not_found_with_exit_1() {
        let h = harness();
        for err in [
            h.service.start("run_nope", 1).await.expect_err("missing"),
            h.service
                .finish("run_nope", RunStatus::Succeeded, None, None)
                .await
                .expect_err("missing"),
        ] {
            assert_eq!(err.to_string(), "Run not found: run_nope");
            assert_eq!(err.exit_code(), 1);
        }
        assert!(h.service.get("run_nope").await.unwrap().is_none());
    }
}
