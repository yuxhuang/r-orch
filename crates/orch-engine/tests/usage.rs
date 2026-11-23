//! What a run's usage looks like at the collector, before anything stores it
//! (spec R86 and R87, observation only: the durable finish is a later slice).
//!
//! Real adapter, real `Orchestrator`, real run-event path. Two kinds of child:
//! the executable stand-ins in `fixtures/fake-agents/` (a real process each, so
//! the adapter's own stream and exit handling run), and scripted pipes where a
//! case needs a stream no stand-in emits. Nothing here calls a real agent CLI.

#[allow(
    dead_code,
    reason = "the shared scripted rig also serves other integration suites"
)]
mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Rig, ScriptedProcessManager, adapter_fixture};
use indexmap::IndexMap;
use orch_core::adapter::{Adapter, AdapterTestResult};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_core::test_support::{make_agent, make_task};
use orch_core::usage::{RunUsage, RunUsageObserver};
use orch_domain::{Agent, AgentConfig, AgentStatus, RunStatus, Task, TaskStatus, TokenUsage};
use orch_infra::adapters::{
    ClaudeAdapter, CodexAdapter, CursorAdapter, OpenCodeAdapter, PiAdapter,
};
use orch_infra::process::SystemProcessManager;

const MODEL: &str = "synthetic-requested-model";

/// Resolves a command to its stand-in in `fixtures/fake-agents/` and spawns it
/// for real. A command without a stand-in is refused, never looked up on
/// `PATH`, where it would find the real agent CLI.
struct FakeAgentProcessManager {
    inner: SystemProcessManager,
}

#[async_trait]
impl ProcessManager for FakeAgentProcessManager {
    fn is_alive(&self, pid: u32) -> bool {
        self.inner.is_alive(pid)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        self.inner.kill(pid, signal);
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        self.inner.kill_with_grace(pid, grace).await;
    }

    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/fake-agents")
            .join(command);
        assert!(fake.is_file(), "no stand-in for `{command}`");
        let opts = SpawnOptions {
            cwd: std::env::temp_dir(),
            ..opts
        };
        self.inner.spawn(&fake.to_string_lossy(), args, opts)
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(String, RunUsage)>>);

impl RunUsageObserver for Recorder {
    fn run_usage_observed(&self, run_id: &str, usage: &RunUsage) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((run_id.to_owned(), usage.clone()));
    }
}

impl Recorder {
    /// The single observation of the single run, which proves one report per run.
    fn only(&self) -> (String, RunUsage) {
        let seen = self.0.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(seen.len(), 1, "one observation per run: {seen:?}");
        seen[0].clone()
    }
}

fn counters(usage: &RunUsage) -> [Option<i64>; 5] {
    [
        usage.tokens_input,
        usage.tokens_output,
        usage.tokens_reasoning,
        usage.tokens_cache_read,
        usage.tokens_cache_write,
    ]
}

fn agent(adapter: &str, scenario: Option<&str>) -> Agent {
    let mut env = IndexMap::new();
    if let Some(scenario) = scenario {
        env.insert("ORCH_FAKE_SCENARIO".to_owned(), scenario.to_owned());
    }
    Agent {
        id: "agt_usage".into(),
        name: format!("{adapter}-usage"),
        adapter: adapter.into(),
        status: AgentStatus::Idle,
        config: AgentConfig {
            model: Some(MODEL.into()),
            env: Some(env),
            ..make_agent().config
        },
        ..make_agent()
    }
}

fn task() -> Task {
    Task {
        id: "tsk_usage".into(),
        title: "synthetic usage task".into(),
        status: TaskStatus::Todo,
        // One attempt, so a failed run ends the task instead of retrying.
        max_attempts: 1,
        ..make_task()
    }
}

fn adapter_for(kind: &str, pm: Arc<dyn ProcessManager>) -> Arc<dyn Adapter> {
    match kind {
        "claude" => Arc::new(ClaudeAdapter::new(pm)),
        "codex" => Arc::new(CodexAdapter::new(pm)),
        "cursor" => Arc::new(CursorAdapter::new(pm)),
        "opencode" => Arc::new(OpenCodeAdapter::new(pm)),
        "pi" => Arc::new(PiAdapter::new(pm)),
        other => panic!("no adapter {other}"),
    }
}

/// One run through a stand-in CLI to its final status. The rig's own scripted
/// process manager is never used: the adapter owns the real one.
async fn run_fake(
    kind: &str,
    scenario: Option<&str>,
    probe: Option<AdapterTestResult>,
    status: TaskStatus,
) -> (Rig, Arc<Recorder>) {
    let pm = Arc::new(FakeAgentProcessManager {
        inner: SystemProcessManager::new(),
    });
    let rig = Rig::new(
        vec![task()],
        vec![agent(kind, scenario)],
        adapter_for(kind, pm),
        ScriptedProcessManager::new(1),
    );
    let recorder = Arc::new(Recorder::default());
    rig.orchestrator.set_run_usage_observer(recorder.clone());
    if let Some(probe) = probe {
        rig.orchestrator.record_adapter_probe(kind, probe);
    }
    rig.orchestrator.daemon_tick(true).await.unwrap();
    rig.wait_for_status("tsk_usage", status).await;
    (rig, recorder)
}

#[tokio::test]
async fn a_normal_run_reports_exactly_the_counters_its_cli_supplied() {
    // Input and output only; the stand-ins say nothing else, so everything
    // else is unknown. OpenCode also reports an explicit reasoning zero.
    let expected: [(&str, [Option<i64>; 5]); 5] = [
        ("claude", [Some(11), Some(7), None, None, None]),
        ("cursor", [Some(11), Some(7), None, None, None]),
        ("codex", [Some(11), Some(7), None, None, None]),
        ("opencode", [Some(11), Some(7), Some(0), None, None]),
        ("pi", [Some(11), Some(7), None, None, None]),
    ];
    for (kind, counts) in expected {
        let (rig, recorder) = run_fake(kind, None, None, TaskStatus::Done).await;
        let (run_id, usage) = recorder.only();
        assert_eq!(counters(&usage), counts, "{kind}");
        assert_eq!(
            usage.model.as_deref(),
            Some(MODEL),
            "{kind}: requested model"
        );
        assert_eq!(usage.adapter_version, None, "{kind}: nothing was probed");
        assert_eq!(
            rig.runs.peek(&run_id).expect("run").status,
            RunStatus::Succeeded
        );
    }
}

#[tokio::test]
async fn a_failed_run_reports_unknown_counters_not_zeros() {
    // The `fail` scenario exits 1 with an error and no usage at all. The old
    // path would have recorded zeros; observation records nothing.
    for kind in ["claude", "cursor", "codex", "opencode", "pi"] {
        let (rig, recorder) = run_fake(kind, Some("fail"), None, TaskStatus::Failed).await;
        let (run_id, usage) = recorder.only();
        assert_eq!(counters(&usage), [None; 5], "{kind}");
        assert_eq!(
            usage.model.as_deref(),
            Some(MODEL),
            "{kind}: requested model"
        );
        assert_eq!(
            rig.runs.peek(&run_id).expect("run").status,
            RunStatus::Failed
        );
    }
}

#[tokio::test]
async fn the_cached_probe_version_reaches_normal_and_failed_runs() {
    for (scenario, status) in [(None, TaskStatus::Done), (Some("fail"), TaskStatus::Failed)] {
        let (_rig, recorder) = run_fake(
            "claude",
            scenario,
            Some(AdapterTestResult::ok("fake-cli 9.9.9")),
            status,
        )
        .await;
        let (_, usage) = recorder.only();
        assert_eq!(usage.adapter_version.as_deref(), Some("fake-cli 9.9.9"));
    }
}

#[tokio::test]
async fn a_failed_version_probe_neither_fails_the_run_nor_invents_a_version() {
    let unreachable = AdapterTestResult {
        ok: false,
        error: Some("cli unavailable".into()),
        ..Default::default()
    };
    let (_rig, recorder) = run_fake("claude", None, Some(unreachable), TaskStatus::Done).await;
    assert_eq!(recorder.only().1.adapter_version, None);

    // A probe that failed but still printed a version keeps it.
    let partial = AdapterTestResult {
        ok: false,
        version: Some("fake-cli 1.0.0".into()),
        error: Some("exit 2".into()),
        ..Default::default()
    };
    let (_rig, recorder) = run_fake("claude", None, Some(partial), TaskStatus::Done).await;
    assert_eq!(
        recorder.only().1.adapter_version.as_deref(),
        Some("fake-cli 1.0.0")
    );
}

/// A scripted run (no real child) of `kind` that writes `lines`, ends stdout
/// and exits with `code`.
async fn run_scripted(
    kind: &str,
    lines: &[&str],
    code: i32,
    status: TaskStatus,
) -> (Rig, Arc<Recorder>) {
    let processes = ScriptedProcessManager::new(4711);
    let rig = Rig::new(
        vec![task()],
        vec![agent(kind, None)],
        adapter_for(kind, Arc::clone(&processes) as Arc<dyn ProcessManager>),
        Arc::clone(&processes),
    );
    let recorder = Arc::new(Recorder::default());
    rig.orchestrator.set_run_usage_observer(recorder.clone());
    rig.orchestrator.daemon_tick(true).await.unwrap();
    for line in lines {
        rig.processes.write_line(line).await;
    }
    rig.processes.close_stdout();
    rig.processes.close_stderr();
    rig.processes.exit(code);
    rig.wait_for_status("tsk_usage", status).await;
    (rig, recorder)
}

#[tokio::test]
async fn a_run_that_fails_after_reporting_usage_keeps_it_including_zero() {
    let (rig, recorder) = run_scripted(
        "codex",
        &[
            r#"{"type":"turn.started"}"#,
            r#"{"type":"turn.failed","model":"synthetic-reported-model","usage":{"input_tokens":5,"output_tokens":0},"error":{"message":"synthetic provider failure"}}"#,
        ],
        1,
        TaskStatus::Failed,
    )
    .await;
    let (run_id, usage) = recorder.only();
    assert_eq!(
        rig.runs.peek(&run_id).expect("run").status,
        RunStatus::Failed
    );
    // Input known, output an explicit zero, the rest never reported.
    assert_eq!(counters(&usage), [Some(5), Some(0), None, None, None]);
    // The CLI's own model replaces the requested one.
    assert_eq!(usage.model.as_deref(), Some("synthetic-reported-model"));
}

#[tokio::test]
async fn codex_native_counters_reach_the_observer_without_double_counting() {
    let report = r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":20,"reasoning_output_tokens":7,"total_tokens":120}}"#;
    let (rig, recorder) = run_scripted("codex", &[report, report], 0, TaskStatus::Done).await;
    let (run_id, usage) = recorder.only();
    assert_eq!(
        counters(&usage),
        [Some(100), Some(20), Some(7), Some(60), None]
    );
    // The legacy record still uses its existing extractor: the native detail
    // fields do not add a second copy of tokens already in input and output.
    assert_eq!(
        rig.runs.peek(&run_id).expect("run").tokens,
        Some(TokenUsage {
            input: 100,
            output: 20,
            reasoning: 0,
            cache_read: 0,
            cache_write: 0,
            total: 120
        })
    );
}

#[tokio::test]
async fn codex_native_zero_and_absence_reach_the_observer_on_success_and_failure() {
    for (kind, code, status) in [
        ("turn.completed", 0, TaskStatus::Done),
        ("turn.failed", 1, TaskStatus::Failed),
    ] {
        for (fields, expected) in [
            (
                r#""cached_input_tokens":0,"reasoning_output_tokens":0"#,
                [None, None, Some(0), Some(0), None],
            ),
            ("", [None; 5]),
        ] {
            let report = format!(
                r#"{{"type":"{kind}","usage":{{{fields}}},"error":{{"message":"synthetic failure"}}}}"#
            );
            let (rig, recorder) = run_scripted("codex", &[&report], code, status).await;
            let (run_id, usage) = recorder.only();
            assert_eq!(counters(&usage), expected, "{report}");
            assert_eq!(rig.runs.peek(&run_id).expect("run").tokens, None);
        }
    }
}

#[tokio::test]
async fn usage_survives_a_nonzero_exit_that_the_done_event_forgives() {
    // The adapter treats a `result` as completion whatever the exit code says,
    // so this run succeeds; the counts must be the same either way.
    let (rig, recorder) = run_scripted(
        "claude",
        &[
            r#"{"type":"assistant","message":{"role":"assistant","model":"synthetic-reported-model","content":[{"type":"text","text":"x"}]}}"#,
            r#"{"type":"result","usage":{"input_tokens":3}}"#,
        ],
        1,
        TaskStatus::Done,
    )
    .await;
    let (run_id, usage) = recorder.only();
    assert_eq!(
        rig.runs.peek(&run_id).expect("run").status,
        RunStatus::Succeeded
    );
    assert_eq!(counters(&usage), [Some(3), None, None, None, None]);
    assert_eq!(usage.model.as_deref(), Some("synthetic-reported-model"));
}

#[tokio::test]
async fn observation_leaves_the_legacy_token_record_unchanged() {
    // The recorded OpenCode session reports no cache counters. The run keeps
    // the 1.0 record, which fills them with zero; observation does not.
    let processes = ScriptedProcessManager::new(4712);
    let rig = Rig::new(
        vec![task()],
        vec![agent("opencode", None)],
        adapter_for(
            "opencode",
            Arc::clone(&processes) as Arc<dyn ProcessManager>,
        ),
        Arc::clone(&processes),
    );
    let recorder = Arc::new(Recorder::default());
    rig.orchestrator.set_run_usage_observer(recorder.clone());
    rig.orchestrator.daemon_tick(true).await.unwrap();
    rig.processes
        .write_stdout(adapter_fixture("opencode", "session.jsonl"))
        .await;
    rig.processes.close_stdout();
    rig.processes.exit(0);
    rig.wait_for_status("tsk_usage", TaskStatus::Done).await;

    let (run_id, usage) = recorder.only();
    let legacy: Option<TokenUsage> = rig.runs.peek(&run_id).expect("run").tokens;
    let legacy = legacy.expect("the 1.0 record");
    assert_eq!(legacy.cache_read, 0);
    assert_eq!(legacy.cache_write, 0);
    assert_eq!(usage.tokens_cache_read, None);
    assert_eq!(usage.tokens_cache_write, None);
    assert_eq!(usage.tokens_input, i64::try_from(legacy.input).ok());
    assert_eq!(usage.tokens_output, i64::try_from(legacy.output).ok());
}

#[tokio::test]
async fn without_an_observer_a_run_is_recorded_exactly_as_before() {
    let processes = ScriptedProcessManager::new(4713);
    let rig = Rig::new(
        vec![task()],
        vec![agent("codex", None)],
        adapter_for("codex", Arc::clone(&processes) as Arc<dyn ProcessManager>),
        Arc::clone(&processes),
    );
    rig.orchestrator.daemon_tick(true).await.unwrap();
    rig.processes
        .write_stdout(adapter_fixture("codex", "session.jsonl"))
        .await;
    rig.processes.close_stdout();
    rig.processes.exit(0);
    rig.wait_for_status("tsk_usage", TaskStatus::Done).await;
    let run = rig.runs.snapshot().pop().expect("a run");
    assert_eq!(run.status, RunStatus::Succeeded);
    assert_eq!(run.tokens.expect("tokens").input, 200);
}
