//! Eco prompt mode and effort preparation in dispatch (design D1, D3, D5.3,
//! D8.1).
//!
//! - one prompt mode is captured per dispatch, re-read before prompt
//!   assembly, so a project toggle applies to the **next** dispatch;
//! - a config read failure or an eco/custom-template conflict fails before
//!   any run exists, through the existing claim rollback;
//! - the effort preparation hook is awaited before execute, its failure goes
//!   through the run-failure path, and effort diagnostics reach exactly one
//!   surface (non-fatal `orchestrator:error`), never a run event (ruling 8);
//! - the retry context names the exact failed run only when its event window
//!   is stable;
//! - with equal adapter outputs, the mechanical trace is identical in both
//!   modes (D8.1): only prompt strings and the captured mode differ.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use orch_core::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, ExecuteHandle, ExecuteParams,
};
use orch_core::test_support::{
    FakeAdapter, FakeConfigStore, FakeExecuteCall, done_event, make_agent, make_run, make_task,
    output_event,
};
use orch_core::{EffortPreparation, storage::ConfigStore};
use orch_domain::{
    Agent, AgentStatus, OrchestratorConfig, PromptConfig, PromptMode, ReasoningEffort, Run,
    RunEvent, RunEventType, RunStatus, Task, TaskStatus,
};
use serde_json::json;

use super::harness::{Harness, NOW_ISO, config_with_concurrency, test_config};

fn project_store(mode: PromptMode) -> Arc<FakeConfigStore> {
    let mut config = test_config();
    config.defaults.agent.prompt_mode = mode;
    Arc::new(FakeConfigStore::with_config(config))
}

async fn set_project_mode(store: &FakeConfigStore, mode: PromptMode) {
    let mut config = store.peek();
    config.defaults.agent.prompt_mode = mode;
    store.write(&config).await.expect("write");
}

async fn dispatch_once(h: &Harness) {
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    h.settle().await;
}

fn todo(id: &str) -> Task {
    Task {
        id: id.into(),
        ..make_task()
    }
}

// ── Mode capture and next-dispatch toggle (D1-3) ─────────────────────────

#[tokio::test]
async fn a_project_toggle_applies_to_the_next_dispatch_and_overrides_are_retained() {
    let inherits = make_agent();
    let pinned = Agent {
        id: "agt_pinned".into(),
        name: "Pinned".into(),
        config: orch_domain::AgentConfig {
            prompt_mode: Some(PromptMode::Normal),
            ..make_agent().config
        },
        ..make_agent()
    };
    let h = Harness::builder()
        .tasks([Task {
            assignee: Some(inherits.id.clone()),
            ..todo("tsk_1")
        }])
        .agents([inherits.clone(), pinned.clone()])
        .config(config_with_concurrency(1))
        .build();
    let store = project_store(PromptMode::Normal);
    h.orchestrator
        .set_config_store(Arc::clone(&store) as Arc<dyn ConfigStore>);

    dispatch_once(&h).await;
    let first = h.adapter.execute_calls.last().expect("first run");
    assert_eq!(first.config.prompt_mode, Some(PromptMode::Normal));
    assert!(h.templates.contexts().iter().all(|c| c.eco.is_none()));

    // Toggle the project default; the next dispatch reads it.
    set_project_mode(&store, PromptMode::Eco).await;
    h.templates.calls.clear();
    h.tasks.seed(Task {
        assignee: Some(inherits.id.clone()),
        ..todo("tsk_2")
    });
    h.tasks.seed(Task {
        assignee: Some(pinned.id.clone()),
        ..todo("tsk_3")
    });
    dispatch_once(&h).await;
    dispatch_once(&h).await;

    let calls = h.adapter.execute_calls.calls();
    let by_task = |task: &str| -> FakeExecuteCall {
        calls
            .iter()
            .find(|call| call.env.get("ORCH_TASK_ID").map(String::as_str) == Some(task))
            .cloned()
            .unwrap_or_else(|| panic!("no execute for {task}"))
    };
    assert_eq!(by_task("tsk_2").config.prompt_mode, Some(PromptMode::Eco));
    // The agent override wins over the new project default.
    assert_eq!(
        by_task("tsk_3").config.prompt_mode,
        Some(PromptMode::Normal)
    );
    // The stored definitions were not rewritten.
    assert_eq!(h.agent(&inherits.id).config.prompt_mode, None);
    assert_eq!(
        h.agent(&pinned.id).config.prompt_mode,
        Some(PromptMode::Normal)
    );
    let contexts = h.templates.contexts();
    assert!(contexts.iter().any(|c| c.eco.is_some()));
    assert!(contexts.iter().any(|c| c.eco.is_none()));
}

#[tokio::test]
async fn without_a_config_store_the_startup_mode_is_used() {
    let mut config = test_config();
    config.defaults.agent.prompt_mode = PromptMode::Eco;
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .config(config)
        .build();
    dispatch_once(&h).await;
    let call = h.adapter.execute_calls.last().expect("run");
    assert_eq!(call.config.prompt_mode, Some(PromptMode::Eco));
    let context = h.templates.contexts().pop().expect("rendered");
    let eco = context.eco.expect("eco projection");
    assert_eq!(eco.roster.len(), 1);
}

// ── Configuration preflight failures (D1, D8 failure oracles) ────────────

#[tokio::test]
async fn a_config_read_failure_stops_before_any_run_and_releases_the_claim() {
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .build();
    let store = project_store(PromptMode::Eco);
    store.faults.fail_always("config.yml unreadable");
    h.orchestrator
        .set_config_store(Arc::clone(&store) as Arc<dyn ConfigStore>);

    dispatch_once(&h).await;

    assert!(h.adapter.execute_calls.is_empty());
    assert!(h.runs.snapshot().is_empty(), "no run before the preflight");
    assert!(h.persisted_state().claimed.is_empty());
    assert!(h.persisted_state().running.is_empty());
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Todo);
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    let errors: Vec<String> = h
        .events()
        .into_iter()
        .filter_map(|event| match event {
            orch_domain::OrchestratorEvent::OrchestratorError { error, .. } => Some(error),
            _ => None,
        })
        .collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("configuration preflight") && e.contains("unreadable")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_hand_edited_eco_and_custom_template_conflict_fails_before_spawn() {
    let config = OrchestratorConfig {
        prompt: Some(PromptConfig {
            system_template: Some("custom {{ task.title }}".into()),
            ..PromptConfig::default()
        }),
        ..test_config()
    };
    let mut agent = make_agent();
    agent.config.prompt_mode = Some(PromptMode::Eco);
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent])
        .config(config)
        .build();

    dispatch_once(&h).await;

    assert!(h.adapter.execute_calls.is_empty());
    assert!(h.runs.snapshot().is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    let errors: Vec<String> = h
        .events()
        .into_iter()
        .filter_map(|event| match event {
            orch_domain::OrchestratorEvent::OrchestratorError { error, .. } => Some(error),
            _ => None,
        })
        .collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("prompt.system_template") && e.contains("agt_test1")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn a_custom_template_still_renders_normal_agents_byte_for_byte() {
    let config = OrchestratorConfig {
        prompt: Some(PromptConfig {
            system_template: Some("custom".into()),
            ..PromptConfig::default()
        }),
        ..test_config()
    };
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .config(config)
        .build();
    dispatch_once(&h).await;
    let templates: Vec<String> = h
        .templates
        .calls
        .calls()
        .into_iter()
        .map(|call| call.template)
        .collect();
    assert_eq!(templates[0], "custom");
    assert_eq!(templates[1], orch_infra::template::DEFAULT_USER_TEMPLATE);
}

// ── Effort preparation (ruling 7, D5.3) ──────────────────────────────────

/// A fake adapter whose preparation outcome is scripted; execute delegates.
struct PreparingAdapter {
    kind: &'static str,
    inner: Arc<FakeAdapter>,
    outcome: Mutex<Option<std::result::Result<EffortPreparation, AdapterError>>>,
    prepared: Mutex<Vec<orch_domain::AgentConfig>>,
}

impl PreparingAdapter {
    fn new(outcome: std::result::Result<EffortPreparation, AdapterError>) -> Self {
        Self::of_kind("shell", Some(outcome))
    }

    /// The default hook: identity, no diagnostics.
    fn identity() -> Self {
        Self::of_kind("shell", None)
    }

    fn of_kind(
        kind: &'static str,
        outcome: Option<std::result::Result<EffortPreparation, AdapterError>>,
    ) -> Self {
        PreparingAdapter {
            kind,
            inner: Arc::new(FakeAdapter::with_events([
                output_event(NOW_ISO, "hi"),
                done_event(NOW_ISO),
            ])),
            outcome: Mutex::new(outcome),
            prepared: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Adapter for PreparingAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn test(&self) -> AdapterTestResult {
        AdapterTestResult::ok("1.0.0")
    }

    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> std::result::Result<EffortPreparation, AdapterError> {
        self.prepared.lock().unwrap().push(params.config.clone());
        self.outcome.lock().unwrap().take().unwrap_or_else(|| {
            Ok(EffortPreparation {
                config: params.config.clone(),
                diagnostics: Vec::new(),
            })
        })
    }

    fn execute(&self, params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError> {
        self.inner.execute(params)
    }

    async fn stop(&self, pid: u32) -> orch_core::error::Result<()> {
        self.inner.stop(pid).await
    }
}

fn agent_with_effort() -> Agent {
    let mut agent = make_agent();
    agent.config.effort = Some(ReasoningEffort::High);
    agent.config.model = Some("opaque-model#keep".into());
    agent
}

#[tokio::test]
async fn a_preparation_failure_cleans_up_through_the_run_failure_path() {
    let adapter = Arc::new(PreparingAdapter::new(Err(AdapterError::new(
        "r-orch does not accept effort `xhigh` (wire check failed)",
        None,
    ))));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_effort()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    // No model child.
    assert!(adapter.inner.execute_calls.is_empty());
    assert_eq!(adapter.prepared.lock().unwrap().len(), 1);
    // The run is recorded failed with the reason.
    let runs = h.runs.snapshot();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert!(
        runs[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("effort preparation failed") && e.contains("xhigh")),
        "{:?}",
        runs[0].error
    );
    // The agent is not stuck running and shows the failure.
    let agent = h.agent("agt_test1");
    assert_eq!(agent.status, AgentStatus::Idle);
    assert_eq!(agent.current_task, None);
    assert!(
        agent
            .last_error
            .as_ref()
            .is_some_and(|e| e.message.contains("effort preparation failed"))
    );
    // Slot and claim released; the task follows the normal retry policy.
    let state = h.persisted_state();
    assert!(state.running.is_empty());
    assert!(state.claimed.is_empty());
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);
    // The stored definition is unchanged.
    assert_eq!(agent.config, agent_with_effort().config);
}

#[tokio::test]
async fn an_execute_failure_after_preparation_also_releases_the_agent() {
    // A valid identity preparation returns the invocation config, which
    // carries the mode captured for this dispatch.
    let mut invocation = agent_with_effort().config;
    invocation.prompt_mode = Some(PromptMode::Normal);
    let adapter = Arc::new(PreparingAdapter::new(Ok(EffortPreparation {
        config: invocation,
        diagnostics: Vec::new(),
    })));
    adapter
        .inner
        .set_execute_error(Some(AdapterError::new("spawn ENOENT", None)));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_effort()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert!(h.persisted_state().running.is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    let runs = h.runs.snapshot();
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert_eq!(runs[0].error.as_deref(), Some("spawn ENOENT"));
}

#[tokio::test]
async fn preparation_runs_before_execute_and_its_config_and_diagnostics_are_used() {
    let mut cleared = agent_with_effort().config;
    cleared.effort = None;
    cleared.prompt_mode = Some(PromptMode::Normal);
    let adapter = Arc::new(PreparingAdapter::new(Ok(EffortPreparation {
        config: cleared.clone(),
        diagnostics: vec!["legacy-ignored: wire check failed".into()],
    })));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_effort()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    // The hook saw the invocation config, with the captured mode.
    let prepared = adapter.prepared.lock().unwrap().clone();
    assert_eq!(prepared[0].effort, Some(ReasoningEffort::High));
    assert_eq!(prepared[0].prompt_mode, Some(PromptMode::Normal));
    // Execute received the prepared copy: effort cleared, model byte-for-byte.
    let call = adapter.inner.execute_calls.last().expect("executed");
    assert_eq!(call.config, cleared);
    assert_eq!(call.config.model.as_deref(), Some("opaque-model#keep"));
    // The authored definition is untouched.
    assert_eq!(
        h.agent("agt_test1").config.effort,
        Some(ReasoningEffort::High)
    );
    // The preparation's diagnostic is reported once, as a diagnostic.
    assert_eq!(
        effort_diagnostics(&h),
        vec!["legacy-ignored: wire check failed".to_owned()]
    );
    assert_no_diagnostic_run_events(&h);
}

/// `(context, error)` of every effort diagnostic on the bus, in order.
fn effort_diagnostic_events(h: &Harness) -> Vec<(String, String)> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            orch_domain::OrchestratorEvent::OrchestratorError {
                error,
                context,
                fatal,
            } if context.starts_with(super::super::dispatch::EFFORT_DIAGNOSTIC_CONTEXT) => {
                assert!(!fatal, "an effort diagnostic is never fatal");
                Some((context, error))
            }
            _ => None,
        })
        .collect()
}

fn effort_diagnostics(h: &Harness) -> Vec<String> {
    effort_diagnostic_events(h)
        .into_iter()
        .map(|(_, error)| error)
        .collect()
}

/// The run's event log holds only what the adapter streamed: no diagnostic
/// can reach `previous_output` or `final_result`, and the bus carries no
/// diagnostic as `agent:output`.
fn assert_no_diagnostic_run_events(h: &Harness) {
    let run = h.runs.snapshot().pop().expect("run");
    for event in h.runs.peek_events(&run.id) {
        let text = event.data.to_string();
        assert!(
            !["effort preparation", "legacy-ignored", "request-only"]
                .iter()
                .any(|marker| text.contains(marker)),
            "diagnostic in run events: {event:?}"
        );
    }
    for event in h.events() {
        if let orch_domain::OrchestratorEvent::AgentOutput { data, .. } = event {
            assert!(
                !data.contains("legacy-ignored") && !data.contains("request-only"),
                "diagnostic on the bus as agent output: {data}"
            );
        }
    }
}

#[tokio::test]
async fn a_non_opencode_plan_diagnostic_is_reported_once_before_spawn_and_never_as_output() {
    // Identity preparation: the shell agent's legacy `high` is ignored by the
    // pure plan, which is the diagnostic's only source.
    let adapter = Arc::new(PreparingAdapter::identity());
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_effort()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    let call = adapter.inner.execute_calls.last().expect("executed");
    assert_eq!(call.config.effort, Some(ReasoningEffort::High));
    let reported = effort_diagnostic_events(&h);
    assert_eq!(reported.len(), 1, "{reported:?}");
    let run = h.runs.snapshot().pop().expect("run");
    assert_eq!(
        reported[0].0,
        format!("effort preparation for run {} (agent agt_test1)", run.id)
    );
    assert!(
        reported[0].1.starts_with("legacy-ignored: "),
        "{:?}",
        reported[0].1
    );
    // Reported before the model child exists, i.e. before agent:started.
    let events = h.events();
    let diagnostic_at = events
        .iter()
        .position(|e| {
            matches!(e, orch_domain::OrchestratorEvent::OrchestratorError { context, .. }
                if context.starts_with("effort preparation"))
        })
        .expect("diagnostic");
    if let Some(started_at) = events
        .iter()
        .position(|e| matches!(e, orch_domain::OrchestratorEvent::AgentStarted { .. }))
    {
        assert!(diagnostic_at < started_at);
    }
    assert_no_diagnostic_run_events(&h);
}

#[tokio::test]
async fn opencode_diagnostics_come_only_from_its_preparation() {
    // The preparation keeps the legacy effort and returns one note. The pure
    // plan for this unknown model would add a `legacy-ignored` note; it must
    // not be consulted for OpenCode.
    let mut invocation = agent_with_effort().config;
    invocation.prompt_mode = Some(PromptMode::Normal);
    let adapter = Arc::new(PreparingAdapter::of_kind(
        "opencode",
        Some(Ok(EffortPreparation {
            config: invocation,
            diagnostics: vec!["request-only: from the wire-checked preparation".into()],
        })),
    ));
    let mut agent = agent_with_effort();
    agent.adapter = "opencode".into();
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    assert!(adapter.inner.execute_calls.last().is_some(), "executed");
    assert_eq!(
        effort_diagnostics(&h),
        vec!["request-only: from the wire-checked preparation".to_owned()]
    );
    assert_no_diagnostic_run_events(&h);
}

#[tokio::test]
async fn a_non_opencode_plan_rejection_fails_before_spawn_with_no_diagnostic() {
    let adapter = Arc::new(PreparingAdapter::identity());
    let mut agent = agent_with_effort();
    agent.config.effort = Some(ReasoningEffort::Xhigh);
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    assert!(adapter.inner.execute_calls.is_empty(), "no model child");
    assert!(effort_diagnostics(&h).is_empty());
    let runs = h.runs.snapshot();
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert!(
        runs[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("xhigh")),
        "{:?}",
        runs[0].error
    );
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert!(h.persisted_state().running.is_empty());
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Retrying);
}

#[tokio::test]
async fn a_preparation_that_changes_more_than_effort_is_refused() {
    let mut changed = agent_with_effort().config;
    changed.model = Some("another-model".into());
    let adapter = Arc::new(PreparingAdapter::new(Ok(EffortPreparation {
        config: changed,
        diagnostics: Vec::new(),
    })));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([agent_with_effort()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    dispatch_once(&h).await;

    assert!(adapter.inner.execute_calls.is_empty());
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert_eq!(h.runs.snapshot()[0].status, RunStatus::Failed);
}

// ── Retry source (D3-10) ─────────────────────────────────────────────────

fn failed_run(id: &str, finished_at: &str) -> Run {
    Run {
        id: id.into(),
        status: RunStatus::Failed,
        error: Some(format!("{id} failed")),
        finished_at: Some(finished_at.into()),
        ..make_run()
    }
}

fn retry_harness() -> Harness {
    let h = Harness::builder()
        .tasks([Task {
            attempts: 1,
            ..make_task()
        }])
        .agents([make_agent()])
        .build();
    h.runs
        .seed(failed_run("run_old", "2026-01-01T00:00:00.000Z"));
    h.runs
        .seed(failed_run("run_new", "2026-01-02T00:00:00.000Z"));
    h.runs.seed_events(
        "run_new",
        [RunEvent {
            timestamp: NOW_ISO.into(),
            event_type: RunEventType::AgentOutput,
            data: json!("evidence"),
        }],
    );
    h
}

#[tokio::test]
async fn the_retry_context_names_the_same_failed_run_its_evidence_came_from() {
    let h = retry_harness();
    dispatch_once(&h).await;
    let retry = h
        .templates
        .contexts()
        .pop()
        .and_then(|c| c.retry)
        .expect("retry context");
    assert_eq!(retry.previous_error, "run_new failed");
    assert_eq!(retry.previous_output, "evidence");
    assert_eq!(retry.source_run_id.as_deref(), Some("run_new"));
}

#[tokio::test]
async fn a_still_draining_collector_defers_the_retry_until_its_window_is_final() {
    // The source window is only vouched for once nothing can append to it.
    // A retry does not start while the failed run's collector still drains,
    // so when it does start, the window is final and eco names its source.
    let h = retry_harness();
    h.orchestrator
        .inner
        .active_collectors
        .lock()
        .await
        .insert("tsk_test1".to_owned());
    h.load_state().await.unwrap();
    h.tick().await.unwrap();
    assert!(h.templates.contexts().is_empty());
    assert!(!h.was_dispatched("tsk_test1"));

    h.orchestrator
        .inner
        .active_collectors
        .lock()
        .await
        .remove("tsk_test1");
    h.tick().await.unwrap();
    let retry = h
        .templates
        .contexts()
        .pop()
        .and_then(|c| c.retry)
        .expect("retry context");
    assert_eq!(retry.previous_output, "evidence");
    assert_eq!(retry.source_run_id.as_deref(), Some("run_new"));
}

// ── Engine invariance under equal outputs (D8.1) ─────────────────────────

/// Fails each task's first attempt, then succeeds: identical outcomes in
/// both modes, decided by task id and attempt alone.
struct ScriptedByTask {
    attempts: Mutex<BTreeMap<String, u32>>,
    calls: Mutex<Vec<FakeExecuteCall>>,
}

#[async_trait]
impl Adapter for ScriptedByTask {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> AdapterTestResult {
        AdapterTestResult::ok("1.0.0")
    }

    fn execute(&self, params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError> {
        let task = params.env.get("ORCH_TASK_ID").cloned().unwrap_or_default();
        let attempt = {
            let mut attempts = self.attempts.lock().unwrap();
            let entry = attempts.entry(task.clone()).or_default();
            *entry += 1;
            *entry
        };
        self.calls.lock().unwrap().push(FakeExecuteCall {
            prompt: params.prompt,
            system_prompt: params.system_prompt,
            workspace: params.workspace,
            env: params.env,
            config: params.config,
        });
        let events: Vec<std::result::Result<AgentEvent, AdapterError>> =
            if task == "tsk_flaky" && attempt == 1 {
                vec![
                    Ok(output_event(NOW_ISO, "working")),
                    Err(AdapterError::new("flaky failure", None)),
                ]
            } else {
                vec![Ok(output_event(NOW_ISO, "done")), Ok(done_event(NOW_ISO))]
            };
        Ok(ExecuteHandle {
            pid: 4242,
            events: Box::pin(futures::stream::iter(events)),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct Trace {
    events: Vec<serde_json::Value>,
    tasks: Vec<Task>,
    runs: Vec<Run>,
    agents: Vec<Agent>,
    state: orch_domain::OrchestratorState,
    calls: Vec<FakeExecuteCall>,
}

/// Run one scripted workload: a dependency chain, an independent task that
/// fails once and is retried, at concurrency 1 so the order is
/// deterministic. `modes` gives the project mode before each tick, so a
/// toggle mid-workload is one more parameter.
async fn trace(modes: &[PromptMode]) -> Trace {
    let adapter = Arc::new(ScriptedByTask {
        attempts: Mutex::new(BTreeMap::new()),
        calls: Mutex::new(Vec::new()),
    });
    let mut config = config_with_concurrency(1);
    config.scheduling.retry_base_delay_ms = 1_000;
    let h = Harness::builder()
        .tasks([
            Task {
                priority: 1,
                ..todo("tsk_a")
            },
            Task {
                priority: 1,
                depends_on: vec!["tsk_a".into()],
                ..todo("tsk_b")
            },
            Task {
                priority: 2,
                ..todo("tsk_flaky")
            },
        ])
        .agents([make_agent()])
        .config(config)
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();
    let store = project_store(modes[0]);
    h.orchestrator
        .set_config_store(Arc::clone(&store) as Arc<dyn ConfigStore>);
    h.load_state().await.unwrap();
    for mode in modes {
        set_project_mode(&store, *mode).await;
        h.tick().await.unwrap();
        h.settle().await;
        h.clock.advance_ms(5_000);
    }

    let mut tasks = h.tasks.snapshot();
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    let mut runs = h.runs.snapshot();
    runs.sort_by(|a, b| a.id.cmp(&b.id));
    // Prompt strings are the one permitted difference.
    for run in &mut runs {
        run.prompt.clear();
    }
    let mut agents = h.agents.snapshot();
    agents.sort_by(|a, b| a.id.cmp(&b.id));
    let calls = adapter
        .calls
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(|mut call| {
            call.prompt.clear();
            call.system_prompt = None;
            call.config.prompt_mode = None;
            call
        })
        .collect();
    Trace {
        events: h
            .events()
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect(),
        tasks,
        runs,
        agents,
        state: h.persisted_state(),
        calls,
    }
}

#[tokio::test]
async fn equal_outputs_give_an_identical_engine_trace_in_both_modes() {
    use PromptMode::{Eco, Normal};
    let ticks = 6;
    let normal = trace(&vec![Normal; ticks]).await;
    let eco = trace(&vec![Eco; ticks]).await;
    let toggled = trace(&[Normal, Eco, Normal, Eco, Eco, Normal]).await;

    // The workload actually exercised dependencies, a failure and a retry.
    assert_eq!(normal.calls.len(), 4, "{:?}", normal.calls);
    assert!(
        normal
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Done || task.status == TaskStatus::Review),
        "{:?}",
        normal.tasks
    );
    assert!(
        normal
            .runs
            .iter()
            .any(|run| run.status == RunStatus::Failed)
    );

    assert_eq!(normal, eco, "eco changed the mechanical trace");
    assert_eq!(normal, toggled, "a mid-workload toggle changed the trace");
}

// ── Remote bypass (D1) ───────────────────────────────────────────────────

#[tokio::test]
async fn a_mirror_dispatch_reads_no_prompt_mode_and_keeps_its_own_prompt() {
    // A store that cannot be read: a mirror must not even try.
    let store = project_store(PromptMode::Eco);
    store.faults.fail_always("must not be read");
    let mirror = Task {
        labels: vec![
            "remote:ctl".into(),
            "remote-ctl:ctl-1".into(),
            "remote-from:alice".into(),
        ],
        description: "remote body".into(),
        ..make_task()
    };
    let h = Harness::builder()
        .tasks([mirror])
        .agents([make_agent()])
        .build();
    h.orchestrator.set_inbound_controller(Some("ctl-1".into()));
    h.orchestrator
        .set_config_store(Arc::clone(&store) as Arc<dyn ConfigStore>);

    dispatch_once(&h).await;

    let call = h.adapter.execute_calls.last().expect("executed");
    assert_eq!(call.prompt, "remote body");
    assert!(
        call.system_prompt
            .as_deref()
            .is_some_and(|s| s.contains("## Remote execution"))
    );
    assert_eq!(call.config.prompt_mode, None);
}

// ── Standalone interrupt (gate-resilience F1, lead ruling 19) ────────────

/// Preparation that, when `hang` is set, waits for its cancellation (a hung
/// wire probe) before returning the identity; a model stream that ends only
/// when its run is cancelled.
struct InterruptibleAdapter {
    hang: bool,
    entered: tokio::sync::Notify,
    executed: Mutex<u32>,
}

impl InterruptibleAdapter {
    fn new(hang: bool) -> Self {
        InterruptibleAdapter {
            hang,
            entered: tokio::sync::Notify::new(),
            executed: Mutex::new(0),
        }
    }
}

const INTERRUPTIBLE_PID: u32 = 4321;

#[async_trait]
impl Adapter for InterruptibleAdapter {
    fn kind(&self) -> &'static str {
        "shell"
    }

    async fn test(&self) -> AdapterTestResult {
        AdapterTestResult::ok("1.0.0")
    }

    async fn prepare_effort(
        &self,
        params: &ExecuteParams,
    ) -> std::result::Result<EffortPreparation, AdapterError> {
        if self.hang {
            self.entered.notify_one();
            params.cancel.cancelled().await;
        }
        Ok(EffortPreparation {
            config: params.config.clone(),
            diagnostics: Vec::new(),
        })
    }

    fn execute(&self, params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError> {
        *self.executed.lock().unwrap() += 1;
        let events = futures::stream::unfold(params.cancel, |cancel| async move {
            cancel.cancelled().await;
            None::<(std::result::Result<AgentEvent, AdapterError>, _)>
        });
        Ok(ExecuteHandle {
            pid: INTERRUPTIBLE_PID,
            events: Box::pin(events),
        })
    }

    async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn after_a_standalone_interrupt_run_task_claims_and_starts_nothing() {
    let adapter = Arc::new(InterruptibleAdapter::new(false));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    h.orchestrator.interrupt();
    h.orchestrator.run_task("tsk_test1").await.unwrap();

    assert_eq!(*adapter.executed.lock().unwrap(), 0);
    assert!(h.runs.snapshot().is_empty(), "no run record");
    assert_eq!(h.task("tsk_test1").status, TaskStatus::Todo);
    assert!(h.persisted_state().claimed.is_empty());
}

#[tokio::test]
async fn an_interrupt_during_preparation_cancels_it_and_fails_before_any_model_child() {
    let adapter = Arc::new(InterruptibleAdapter::new(true));
    let h = Harness::builder()
        .tasks([make_task()])
        .agents([make_agent()])
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    let dispatch = {
        let orchestrator = h.orchestrator.clone();
        tokio::spawn(async move { orchestrator.run_task("tsk_test1").await })
    };
    adapter.entered.notified().await;
    h.orchestrator.interrupt();
    // The preparation only returns once cancelled: a dispatch that ignored
    // the interrupt would hang here.
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), dispatch)
        .await
        .expect("the interrupt must cancel the in-flight preparation")
        .unwrap();

    assert!(result.is_err(), "the dispatch reports the interrupt");
    assert_eq!(*adapter.executed.lock().unwrap(), 0, "no model child");
    let runs = h.runs.snapshot();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert!(
        runs[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("interrupted")),
        "{:?}",
        runs[0].error
    );
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert!(h.persisted_state().running.is_empty());
    assert!(h.persisted_state().claimed.is_empty());
    assert_ne!(h.task("tsk_test1").status, TaskStatus::InProgress);
}

#[tokio::test]
async fn stopping_interrupted_runs_winds_down_only_this_process_runs() {
    // Another process's run (say a second `orch run` in another terminal) is
    // in the shared state; this process starts its own and is interrupted.
    let foreign = Task {
        id: "tsk_foreign".into(),
        status: TaskStatus::InProgress,
        assignee: Some("agt_other".into()),
        ..make_task()
    };
    let mine = Task {
        assignee: Some("agt_test1".into()),
        ..make_task()
    };
    let other = Agent {
        id: "agt_other".into(),
        name: "Other".into(),
        status: AgentStatus::Running,
        ..make_agent()
    };
    let adapter = Arc::new(InterruptibleAdapter::new(false));
    let h = Harness::builder()
        .tasks([mine, foreign])
        .agents([make_agent(), other])
        .state(super::harness::state_running(
            "tsk_foreign",
            "agt_other",
            "run_foreign",
            777,
            NOW_ISO,
        ))
        .adapter_impl(Arc::clone(&adapter) as Arc<dyn Adapter>)
        .build();

    h.orchestrator.run_task("tsk_test1").await.unwrap();
    assert_eq!(*adapter.executed.lock().unwrap(), 1);
    assert!(h.persisted_state().running.contains_key("tsk_test1"));

    h.orchestrator
        .stop_interrupted_runs("SIGINT")
        .await
        .unwrap();

    let state = h.persisted_state();
    assert!(!state.running.contains_key("tsk_test1"), "own run stopped");
    assert!(
        state.running.contains_key("tsk_foreign"),
        "another process's run is not touched"
    );
    let killed: Vec<u32> = h
        .processes
        .killed_with_grace()
        .into_iter()
        .map(|(pid, _)| pid)
        .collect();
    assert_eq!(killed, [INTERRUPTIBLE_PID]);
    let mine = h
        .runs
        .snapshot()
        .into_iter()
        .find(|run| run.task_id == "tsk_test1")
        .expect("own run");
    assert_eq!(mine.status, RunStatus::Cancelled);
    assert_eq!(h.agent("agt_test1").status, AgentStatus::Idle);
    assert_eq!(h.agent("agt_other").status, AgentStatus::Running);
    assert_ne!(h.task("tsk_test1").status, TaskStatus::InProgress);
    assert_eq!(h.task("tsk_foreign").status, TaskStatus::InProgress);
    assert!(
        h.orchestrator
            .inner
            .active_collectors
            .lock()
            .await
            .is_empty(),
        "the collector finished"
    );
}
