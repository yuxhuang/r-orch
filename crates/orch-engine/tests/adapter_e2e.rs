//! Real adapter, real `AdapterRegistry`, real `Orchestrator`, real state
//! machine and real run-event path; only the child process is scripted. Each
//! case drives one task the whole way — `todo → in_progress → review → done`
//! (auto-approve, because the shared agent factory sets
//! `approval_policy: auto`) — and asserts on the argv, the parsed event
//! sequence, the run record and the agent's stats.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Rig, ScriptedProcessManager, adapter_fixture};
use orch_core::test_support::{make_agent, make_task};
use orch_domain::{
    AdapterErrorKind, Agent, AgentStatus, RunEventType, RunStatus, Task, TaskStatus, TokenUsage,
};
use orch_infra::adapters::{AntigravityAdapter, GrokAdapter, PiAdapter};

fn agent(id: &str, name: &str, adapter: &str) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        adapter: adapter.into(),
        status: AgentStatus::Idle,
        ..make_agent()
    }
}

fn task(id: &str, title: &str) -> Task {
    Task {
        id: id.into(),
        title: title.into(),
        status: TaskStatus::Todo,
        ..make_task()
    }
}

fn run_event_types(rig: &Rig, run_id: &str) -> Vec<RunEventType> {
    rig.runs
        .peek_events(run_id)
        .into_iter()
        .map(|event| event.event_type)
        .collect()
}

fn only_run(rig: &Rig) -> orch_domain::Run {
    let runs = rig.runs.snapshot();
    assert_eq!(runs.len(), 1, "expected exactly one run");
    runs.into_iter().next().expect("a run")
}

// ── Pi ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pi_drives_a_task_from_todo_to_done() {
    let processes = ScriptedProcessManager::new(24601);
    let rig = Rig::new(
        vec![task("tsk_pi", "Implement feature X")],
        vec![agent("agt_pi", "pi-engineer", "pi")],
        Arc::new(PiAdapter::new(
            Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>
        )),
        Arc::clone(&processes),
    );

    rig.orchestrator.run_all().await.unwrap();

    // Dispatch: the adapter spawned `pi --mode rpc`.
    let spawns = rig.processes.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].command, "pi");
    assert!(spawns[0].has_flag("--mode"));
    assert_eq!(spawns[0].value_after("--mode"), Some("rpc"));

    // The prompt went out as one JSONL frame and stdin stayed open: closing it
    // stalls `--mode rpc` right after the user `message_end`.
    let written = rig.processes.read_stdin().await;
    assert!(written.contains("rendered prompt"), "stdin was {written:?}");
    assert!(written.ends_with('\n'));

    assert_eq!(rig.task("tsk_pi").status, TaskStatus::InProgress);
    let busy = rig.agent("agt_pi");
    assert_eq!(busy.status, AgentStatus::Running);
    assert_eq!(busy.current_task.as_deref(), Some("tsk_pi"));

    // Feed the recorded RPC session. `text_delta` frames aggregate and
    // `text_end` flushes one canonical `output`; the `extension_ui_request`
    // and `tool_execution_update` frames in the fixture are passive and
    // produce nothing.
    rig.processes
        .write_stdout(adapter_fixture("pi", "session.jsonl"))
        .await;

    rig.wait_for_status("tsk_pi", TaskStatus::Done).await;

    assert_eq!(
        rig.transitions("tsk_pi"),
        vec!["todo→in_progress", "in_progress→review", "review→done"]
    );

    let freed = rig.agent("agt_pi");
    assert_eq!(freed.status, AgentStatus::Idle);
    assert_eq!(freed.current_task, None);
    assert_eq!(freed.stats.tasks_completed, 1);
    assert_eq!(freed.stats.total_runs, 1);

    // The adapter reaps the long-lived Pi process itself, with its own grace.
    assert!(
        rig.processes
            .killed_with_grace()
            .contains(&(24601, Duration::from_millis(1_000))),
        "grace kills were {:?}",
        rig.processes.killed_with_grace()
    );

    let run = only_run(&rig);
    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(
        run_event_types(&rig, &run.id),
        vec![
            RunEventType::AgentOutput, // text_end
            RunEventType::ToolCall,    // tool_execution_start (read)
            RunEventType::CommandRun,  // tool_execution_end (bash)
            RunEventType::FileChanged, // tool_execution_end (write)
            RunEventType::Done,        // agent_end
        ]
    );
    // Cache tokens are informational and deliberately not in `total`.
    assert_eq!(
        run.tokens,
        Some(TokenUsage {
            input: 42,
            output: 17,
            reasoning: 3,
            total: 62,
            cache_read: 5,
            cache_write: 0,
        })
    );
    // The `write` tool's path reached the proof.
    assert!(
        rig.task("tsk_pi")
            .proof
            .expect("proof")
            .files_changed
            .contains(&"src/foo.ts".to_owned())
    );
}

#[tokio::test]
async fn a_rate_limited_pi_run_fails_and_never_reaches_done() {
    let processes = ScriptedProcessManager::new(24601);
    let rig = Rig::new(
        vec![Task {
            max_attempts: 1,
            ..task("tsk_pi_rate_limit", "Task that hits the provider limit")
        }],
        vec![agent("agt_pi_rate_limit", "pi-rate-limited", "pi")],
        Arc::new(PiAdapter::new(
            Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>
        )),
        Arc::clone(&processes),
    );

    rig.orchestrator.run_all().await.unwrap();

    let message = "Permission denied: Reached overall message rate limit. Please try again later.";
    rig.processes
        .write_line(&format!(
            r#"{{"type":"agent_end","willRetry":false,"messages":[{{"role":"assistant","content":[],"stopReason":"error","errorMessage":{}}}]}}"#,
            serde_json::to_string(message).unwrap()
        ))
        .await;
    rig.processes
        .write_line(r#"{"type":"agent_settled"}"#)
        .await;

    rig.wait_for_status("tsk_pi_rate_limit", TaskStatus::Failed)
        .await;

    // `max_attempts: 1` with one attempt spent means no retry: straight to
    // `failed`, and never through `done`.
    assert_eq!(
        rig.transitions("tsk_pi_rate_limit"),
        vec!["todo→in_progress", "in_progress→failed"]
    );

    let final_agent = rig.agent("agt_pi_rate_limit");
    assert_eq!(final_agent.status, AgentStatus::Idle);
    assert_eq!(final_agent.current_task, None);
    assert_eq!(final_agent.stats.tasks_completed, 0);
    assert_eq!(final_agent.stats.tasks_failed, 1);
    assert_eq!(final_agent.stats.total_runs, 1);
    let last_error = final_agent.last_error.expect("last_error");
    assert_eq!(last_error.message, message);
    assert_eq!(last_error.kind, AdapterErrorKind::RateLimit.as_str());

    let run = only_run(&rig);
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.error.as_deref(), Some(message));
    assert_eq!(run_event_types(&rig, &run.id), vec![RunEventType::Error]);
    assert!(
        rig.processes
            .killed_with_grace()
            .contains(&(24601, Duration::from_millis(1_000)))
    );
}

// ── Grok ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn grok_drives_a_task_from_todo_to_done() {
    let processes = ScriptedProcessManager::new(31337);
    let rig = Rig::new(
        vec![task("tsk_grok", "Grok work")],
        vec![agent("agt_grok", "grok-engineer", "grok")],
        Arc::new(GrokAdapter::new(
            Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>
        )),
        Arc::clone(&processes),
    );

    rig.orchestrator.run_all().await.unwrap();

    let spawns = rig.processes.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].command, "grok");
    assert_eq!(spawns[0].value_after("-p"), Some("rendered prompt"));
    assert_eq!(
        spawns[0].value_after("--output-format"),
        Some("streaming-json")
    );
    assert_eq!(
        spawns[0].value_after("--permission-mode"),
        Some("bypassPermissions")
    );

    rig.processes
        .write_stdout(adapter_fixture("grok", "session.jsonl"))
        .await;
    rig.processes.close_stdout();
    rig.processes.exit(0);

    rig.wait_for_status("tsk_grok", TaskStatus::Done).await;

    assert_eq!(
        rig.transitions("tsk_grok"),
        vec!["todo→in_progress", "in_progress→review", "review→done"]
    );
    let freed = rig.agent("agt_grok");
    assert_eq!(freed.status, AgentStatus::Idle);
    assert_eq!(freed.stats.tasks_completed, 1);

    let run = only_run(&rig);
    // The order in `fixtures/adapters/grok/session.expected.json`: the tool
    // call passes straight through, while the two `text` frames aggregate into
    // one `output` that only flushes at `end`.
    assert_eq!(
        run_event_types(&rig, &run.id),
        vec![
            RunEventType::ToolCall,
            RunEventType::AgentOutput,
            RunEventType::Done
        ]
    );
    // The `thought` frame is dropped, not forwarded as output.
    let output = rig
        .runs
        .peek_events(&run.id)
        .into_iter()
        .find(|event| event.event_type == RunEventType::AgentOutput)
        .expect("an output event");
    assert_eq!(
        output.data,
        serde_json::Value::String(r#"{"text":"ORCH_OK"}"#.into())
    );
}

// ── Antigravity ──────────────────────────────────────────────────────────

#[tokio::test]
async fn antigravity_drives_a_task_from_todo_to_done() {
    let processes = ScriptedProcessManager::new(4242);
    let rig = Rig::new(
        vec![task("tsk_antigravity", "Antigravity work")],
        vec![agent("agt_antigravity", "agy-engineer", "antigravity")],
        Arc::new(AntigravityAdapter::new(
            Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>
        )),
        Arc::clone(&processes),
    );

    rig.orchestrator.run_all().await.unwrap();

    let spawns = rig.processes.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].command, "agy");
    assert!(spawns[0].has_flag("--dangerously-skip-permissions"));
    assert!(
        spawns[0]
            .value_after("-p")
            .is_some_and(|prompt| prompt.contains("rendered prompt"))
    );

    rig.processes
        .write_stdout(adapter_fixture("antigravity", "session.txt"))
        .await;
    rig.processes.close_stdout();
    rig.processes.exit(0);

    rig.wait_for_status("tsk_antigravity", TaskStatus::Done)
        .await;

    assert_eq!(
        rig.transitions("tsk_antigravity"),
        vec!["todo→in_progress", "in_progress→review", "review→done"]
    );
    assert_eq!(rig.agent("agt_antigravity").stats.tasks_completed, 1);

    let run = only_run(&rig);
    assert_eq!(
        run_event_types(&rig, &run.id),
        vec![
            RunEventType::AgentOutput,
            RunEventType::AgentOutput,
            RunEventType::Done,
        ]
    );
    let events = rig.runs.peek_events(&run.id);
    assert_eq!(
        events[0].data,
        serde_json::Value::String(r#"{"text":"Antigravity line one"}"#.into())
    );
    // A plain-text adapter's `done` result is the whole transcript.
    assert_eq!(
        events[2].data,
        serde_json::Value::String(
            r#"{"result":"Antigravity line one\nAntigravity line two"}"#.into()
        )
    );
}

#[tokio::test]
async fn a_grok_child_that_floods_stderr_still_completes_and_leaks_nothing_into_the_error() {
    // grok and antigravity spawn with `stderr: 'pipe'` and never attach a
    // consumer, so a child writing past one pipe buffer would block forever
    // and only stall detection would clear it. The fix is to **drain and
    // discard** — attaching a tail capture instead would change the error text, the
    // classification and the persisted `agent.last_error`.
    //
    // So this asserts both halves: the flood does not deadlock, and the
    // stderr does not reach the error message.
    let processes = ScriptedProcessManager::new(31338);
    let rig = Rig::new(
        vec![Task {
            max_attempts: 1,
            ..task("tsk_grok_err", "Grok work that fails")
        }],
        vec![agent("agt_grok_err", "grok-broken", "grok")],
        Arc::new(GrokAdapter::new(
            Arc::clone(&processes) as Arc<dyn orch_core::process::ProcessManager>
        )),
        Arc::clone(&processes),
    );

    rig.orchestrator.run_all().await.unwrap();

    // Well past a Linux pipe buffer, and a recognizable substring that would
    // flip `classify_adapter_error` if it ever reached the message.
    let flood = "error: rate limit reached\n".repeat(4_000);
    rig.processes.write_stderr(flood.as_bytes()).await;
    rig.processes.close_stdout();
    rig.processes.exit(1);

    rig.wait_for_status("tsk_grok_err", TaskStatus::Failed)
        .await;

    assert_eq!(
        rig.transitions("tsk_grok_err"),
        vec!["todo→in_progress", "in_progress→failed"]
    );
    let run = only_run(&rig);
    assert_eq!(run.status, RunStatus::Failed);
    let error = run.error.expect("an error on the run");
    assert_eq!(error, "Grok process exited with code 1");

    let last_error = rig.agent("agt_grok_err").last_error.expect("last_error");
    assert_eq!(last_error.message, "Grok process exited with code 1");
    // The classification came from the exit code, not from the stderr text.
    assert_eq!(last_error.kind, AdapterErrorKind::ProcessCrash.as_str());
}
