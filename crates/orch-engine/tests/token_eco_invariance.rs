//! T1 / F4 — D8-1: prompt mode must not change orchestration mechanics.
//!
//! The same workload (identical tasks, agents, clock, ids and scripted adapter
//! outcomes) runs through the real dispatch path in normal, eco and a
//! toggled-mid-workload configuration. Everything the engine persists or
//! emits — task, run, agent and scheduler-state records, bus events and every
//! `ExecuteParams` field — must be identical. The only permitted differences
//! are the prompt strings and the captured `prompt_mode`.
//!
//! This proves engine invariance under equal model output. It does not prove a
//! model produces equivalent follow-on work (that is the held-out eval's job).

mod token_eco_support;

use orch_core::adapter::AgentEvent;
use orch_core::test_support::{done_event, error_event, output_event};
use orch_domain::{
    AdapterErrorKind, AgentStatus, ApprovalPolicy, PromptMode, RunStatus, Task, TaskStatus,
};
use serde_json::{Value, json};
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, task};

const T: &str = "2025-10-13T16:37:33.426Z";

type Workload = for<'a> fn(
    &'a Rig,
    &'a [PromptMode],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>;

/// Everything mechanical, with the permitted differences removed.
async fn mechanical_trace(rig: &Rig) -> Value {
    let mut tasks = Vec::new();
    for id in rig_task_ids(rig).await {
        tasks.push(serde_json::to_value(rig.task(&id).await).unwrap());
    }
    let mut runs = rig.engine.runs.list_all().await.unwrap();
    runs.sort_by(|a, b| a.id.cmp(&b.id));
    let mut run_values = Vec::new();
    for mut run in runs {
        run.prompt.clear(); // permitted difference
        let events = rig.engine.runs.read_events(&run.id).await.unwrap();
        run_values.push(json!({"run": run, "events": events}));
    }
    let mut agents = Vec::new();
    let mut listed = rig.engine.agents.list().await.unwrap();
    // The file store lists in last-write order; identity order is canonical.
    listed.sort_by(|a, b| a.id.cmp(&b.id));
    for a in listed {
        agents.push(serde_json::to_value(a).unwrap());
    }
    let state = rig.engine.state.read().await.unwrap();
    let calls: Vec<Value> = rig
        .adapter
        .calls()
        .into_iter()
        .map(|mut c| {
            c.config.prompt_mode = None; // permitted difference
            json!({
                "workspace": c.workspace,
                "env": c.env,
                "inherit_env": format!("{:?}", c.inherit_env),
                "config": c.config,
            })
        })
        .collect();
    json!({
        "tasks": tasks,
        "runs": run_values,
        "agents": agents,
        "state": state,
        "events": rig.events().iter().map(|e| serde_json::to_value(e).unwrap()).collect::<Vec<_>>(),
        "calls": calls,
        "order": rig.adapter.order(),
    })
}

/// The first JSON path at which two traces differ (keeps failures readable).
fn first_difference(path: &str, a: &Value, b: &Value) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys().chain(y.keys()) {
                let (l, r) = (
                    x.get(key).unwrap_or(&Value::Null),
                    y.get(key).unwrap_or(&Value::Null),
                );
                if let Some(found) = first_difference(&format!("{path}/{key}"), l, r) {
                    return Some(found);
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!("{path}: length {} vs {}", x.len(), y.len()));
            }
            x.iter()
                .zip(y)
                .enumerate()
                .find_map(|(i, (l, r))| first_difference(&format!("{path}/{i}"), l, r))
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

async fn rig_task_ids(rig: &Rig) -> Vec<String> {
    let mut ids: Vec<String> = rig
        .engine
        .tasks
        .list(&Default::default())
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    ids.sort();
    ids
}

async fn run_workload(
    workload: Workload,
    modes: &[PromptMode],
    agents: Vec<orch_domain::Agent>,
    tasks: Vec<Task>,
) -> (Value, Rig) {
    let rig = Rig::new(RigSpec {
        raw_config: config_with(Some(modes[0].as_str()), json!({})),
        agents,
        tasks,
        skills: vec![],
        config_store: true,
    })
    .await;
    workload(&rig, modes).await;
    (mechanical_trace(&rig).await, rig)
}

async fn set_mode(rig: &Rig, mode: PromptMode) {
    rig.set_project_mode_raw(mode).await;
}

fn scripted_failure() -> Vec<Result<AgentEvent, orch_core::adapter::AdapterError>> {
    vec![
        Ok(output_event(T, "working")),
        Ok(error_event(T, "flaky failure", AdapterErrorKind::Unknown)),
        Err(orch_core::adapter::AdapterError::new("flaky failure", None)),
    ]
}

fn ok_script() -> Vec<Result<AgentEvent, orch_core::adapter::AdapterError>> {
    vec![Ok(output_event(T, "done")), Ok(done_event(T))]
}

async fn assert_all_equal(
    label: &str,
    workload: Workload,
    agents: impl Fn() -> Vec<orch_domain::Agent>,
    tasks: impl Fn() -> Vec<Task>,
    sanity: impl Fn(&Value),
) {
    let (normal, _r1) = run_workload(workload, &[PromptMode::Normal; 6], agents(), tasks()).await;
    let (eco, r2) = run_workload(workload, &[PromptMode::Eco; 6], agents(), tasks()).await;
    let (toggled, r3) = run_workload(
        workload,
        &[
            PromptMode::Normal,
            PromptMode::Eco,
            PromptMode::Normal,
            PromptMode::Eco,
            PromptMode::Eco,
            PromptMode::Normal,
        ],
        agents(),
        tasks(),
    )
    .await;
    sanity(&normal);
    assert!(
        r2.adapter.calls().iter().all(|c| c.is_eco()
            || c.system_prompt
                .as_deref()
                .is_some_and(|s| s.contains("Remote execution"))),
        "{label}: eco workload really rendered eco (or a bypassed mirror)"
    );
    assert!(
        r3.adapter.calls().len() == r2.adapter.calls().len(),
        "{label}"
    );
    assert!(
        normal == eco,
        "{label}: eco changed the mechanical trace at: {:?}",
        first_difference("", &normal, &eco)
    );
    assert!(
        normal == toggled,
        "{label}: a mid-workload toggle changed the mechanical trace at: {:?}",
        first_difference("", &normal, &toggled)
    );
}

// ── S1: dependencies, one failure, retry ────────────────────────────────

fn s1<'a>(
    rig: &'a Rig,
    modes: &'a [PromptMode],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        // tsk_b has a single attempt: its first run fails terminally, so no
        // engine-initiated backoff retry (whose timing races with the
        // collector) is involved. The operator then retries it explicitly.
        // Engine backoff arithmetic is covered by the in-crate D8.1 trace test
        // with its deterministic harness.
        for (i, mode) in modes.iter().enumerate() {
            set_mode(rig, *mode).await;
            match i {
                0 => rig.adapter.push_script(ok_script()),        // tsk_a
                1 => rig.adapter.push_script(scripted_failure()), // tsk_b attempt 1
                2 => {
                    rig.engine.task_service.retry("tsk_b").await.unwrap();
                    rig.adapter.push_script(ok_script()); // tsk_b again
                }
                3 => {
                    // tsk_c was failed by the dependency cascade; retry it too.
                    rig.engine.task_service.retry("tsk_c").await.unwrap();
                }
                _ => {}
            }
            rig.dispatch().await;
            rig.clock.advance_ms(5_000);
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn s1_dependency_chain_failure_and_retry_have_identical_mechanics_in_every_mode() {
    assert_all_equal(
        "s1",
        s1,
        // A strict chain a -> b -> c: exactly one task is dispatchable per pass,
        // so the schedule cannot depend on how quickly concurrent collectors
        // finish (their bus events interleave nondeterministically).
        || vec![claude_agent("agt_a", "Alpha")],
        || {
            vec![
                Task {
                    priority: 1,
                    ..task("tsk_a", None)
                },
                Task {
                    priority: 1,
                    max_attempts: 1,
                    depends_on: vec!["tsk_a".into()],
                    ..task("tsk_b", None)
                },
                Task {
                    priority: 1,
                    depends_on: vec!["tsk_b".into()],
                    ..task("tsk_c", None)
                },
            ]
        },
        |trace| {
            let runs = trace["runs"].as_array().unwrap();
            assert!(
                runs.len() >= 4,
                "the workload dispatched several runs: {}",
                runs.len()
            );
            assert!(
                runs.iter()
                    .any(|r| r["run"]["status"] == json!(RunStatus::Failed)),
                "the workload really contained a failure"
            );
            let tb = trace["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == "tsk_c")
                .unwrap();
            assert_ne!(
                tb["status"],
                json!(TaskStatus::Todo),
                "the last task of the chain ran after its prerequisites"
            );
        },
    )
    .await;
}

// ── S2: manual review, rejection with feedback, re-dispatch ─────────────

fn s2<'a>(
    rig: &'a Rig,
    modes: &'a [PromptMode],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        set_mode(rig, modes[0]).await;
        rig.dispatch().await;
        set_mode(rig, modes[1]).await;
        rig.engine
            .task_service
            .reject("tsk_rev", Some("FEEDBACK: add the missing test".into()))
            .await
            .unwrap();
        rig.dispatch().await;
        set_mode(rig, modes[2]).await;
        rig.engine.task_service.approve("tsk_rev").await.unwrap();
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn s2_review_rejection_feedback_and_redispatch_have_identical_mechanics() {
    fn reviewer() -> orch_domain::Agent {
        let mut a = claude_agent("agt_a", "Alpha");
        a.config.approval_policy = Some(ApprovalPolicy::Manual);
        a
    }
    assert_all_equal(
        "s2",
        s2,
        || vec![reviewer()],
        || vec![task("tsk_rev", Some("agt_a"))],
        |trace| {
            let t = &trace["tasks"][0];
            assert_eq!(t["status"], json!(TaskStatus::Done));
            assert_eq!(
                trace["runs"].as_array().unwrap().len(),
                2,
                "dispatched again after rejection"
            );
        },
    )
    .await;

    // The feedback itself reaches the child in both modes (E6-style obligation).
    for mode in [PromptMode::Normal, PromptMode::Eco] {
        let (_t, rig) = run_workload(
            s2,
            &[mode; 3],
            vec![reviewer()],
            vec![task("tsk_rev", Some("agt_a"))],
        )
        .await;
        let second = rig.adapter.calls().remove(1);
        assert!(
            second.all_text().contains("FEEDBACK: add the missing test"),
            "{mode:?}: reviewer correction must not be omitted"
        );
    }
}

// ── S3: cancel a long-running task ──────────────────────────────────────

fn s3<'a>(
    rig: &'a Rig,
    modes: &'a [PromptMode],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        set_mode(rig, modes[0]).await;
        rig.adapter.hang_next(1);
        let _ = rig.orchestrator.run_all().await;
        // The task is running; flip the project default under it.
        set_mode(rig, modes[1]).await;
        rig.orchestrator.cancel_task("tsk_long").await.unwrap();
        rig.orchestrator.wait_for_collectors().await;
        // A second task dispatches under whatever the project mode now is.
        rig.reseed_task(task("tsk_after", Some("agt_a"))).await;
        set_mode(rig, modes[2]).await;
        rig.dispatch().await;
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn s3_cancel_and_a_toggle_during_an_active_run_release_the_same_resources() {
    assert_all_equal(
        "s3",
        s3,
        || vec![claude_agent("agt_a", "Alpha")],
        || vec![task("tsk_long", Some("agt_a"))],
        |trace| {
            let tasks = trace["tasks"].as_array().unwrap();
            let long = tasks.iter().find(|t| t["id"] == "tsk_long").unwrap();
            assert_eq!(long["status"], json!(TaskStatus::Cancelled));
            assert_eq!(trace["state"]["running"], json!({}), "nothing left running");
            assert_eq!(
                trace["state"]["claimed"],
                json!([]).clone(),
                "claims released"
            );
            let agent = &trace["agents"][0];
            assert_eq!(agent["status"], json!(AgentStatus::Idle));
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_toggle_during_an_active_run_leaves_that_runs_captured_mode_unchanged() {
    let rig = Rig::new(RigSpec {
        raw_config: config_with(Some("eco"), json!({})),
        agents: vec![claude_agent("agt_a", "Alpha")],
        tasks: vec![task("tsk_long", Some("agt_a"))],
        skills: vec![],
        config_store: true,
    })
    .await;
    rig.adapter.hang_next(1);
    let _ = rig.orchestrator.run_all().await;
    let captured = rig.adapter.for_task("tsk_long");
    assert!(captured.is_eco());
    assert_eq!(captured.config.prompt_mode, Some(PromptMode::Eco));
    let recorded = rig.runs().await[0].prompt.clone();

    rig.set_project_mode_raw(PromptMode::Normal).await;

    assert_eq!(
        rig.runs().await[0].prompt,
        recorded,
        "the active run keeps the prompt it started with"
    );
    assert_eq!(
        rig.agent("agt_a").await.status,
        AgentStatus::Running,
        "toggling stops no agent"
    );
    assert_eq!(
        rig.task("tsk_long").await.status,
        TaskStatus::InProgress,
        "and resets no task"
    );
    rig.orchestrator.cancel_task("tsk_long").await.unwrap();
    rig.orchestrator.wait_for_collectors().await;
}

// ── S4: remote mirror bypass ────────────────────────────────────────────

fn s4<'a>(
    rig: &'a Rig,
    modes: &'a [PromptMode],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
    Box::pin(async move {
        rig.orchestrator
            .set_inbound_controller(Some("ctl-1".into()));
        for mode in modes.iter().take(2) {
            set_mode(rig, *mode).await;
            rig.dispatch().await;
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn s4_a_mirror_dispatch_is_untouched_by_the_project_mode_including_its_prompt() {
    let mirror = || Task {
        labels: vec![
            "remote:ctl".into(),
            "remote-ctl:ctl-1".into(),
            "remote-from:alice".into(),
        ],
        description: "remote body".into(),
        ..task("tsk_mirror", Some("agt_a"))
    };
    let mut prompts = Vec::new();
    for mode in [PromptMode::Normal, PromptMode::Eco] {
        let (_t, rig) = run_workload(
            s4,
            &[mode; 6],
            vec![claude_agent("agt_a", "Alpha")],
            vec![mirror()],
        )
        .await;
        let call = rig.adapter.calls().remove(0);
        assert_eq!(call.prompt, "remote body");
        assert!(
            call.system_prompt
                .as_deref()
                .unwrap()
                .contains("## Remote execution")
        );
        assert_eq!(call.config.prompt_mode, None, "a mirror captures no mode");
        prompts.push((call.prompt, call.system_prompt));
    }
    assert_eq!(
        prompts[0], prompts[1],
        "a mirror's own prompt is byte-identical in both modes"
    );

    assert_all_equal(
        "s4",
        s4,
        || vec![claude_agent("agt_a", "Alpha")],
        || vec![mirror()],
        |trace| assert_eq!(trace["calls"].as_array().unwrap().len(), 1),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mirror_dispatch_survives_an_unreadable_config_because_it_never_reads_the_mode() {
    let rig = Rig::new(RigSpec {
        raw_config: config_with(Some("eco"), json!({})),
        agents: vec![claude_agent("agt_a", "Alpha")],
        tasks: vec![Task {
            labels: vec![
                "remote:ctl".into(),
                "remote-ctl:ctl-1".into(),
                "remote-from:alice".into(),
            ],
            description: "remote body".into(),
            ..task("tsk_mirror", Some("agt_a"))
        }],
        skills: vec![],
        config_store: true,
    })
    .await;
    rig.orchestrator
        .set_inbound_controller(Some("ctl-1".into()));
    std::fs::write(rig.config_path(), "defaults: [this is not a mapping").unwrap();

    rig.dispatch().await;

    assert_eq!(
        rig.adapter.calls().len(),
        1,
        "the mirror ran without reading config.yml's mode"
    );
}
