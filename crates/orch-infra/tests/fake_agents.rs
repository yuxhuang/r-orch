//! The fake agent CLIs in `fixtures/fake-agents/`, driven through the real
//! adapters.
//!
//! The differential harness puts that directory first on `PATH` and runs both
//! binaries against it, so these scripts are as load-bearing as a fixture:
//! if one emits a shape an adapter mis-parses, every scenario built on it is
//! quietly wrong. This suite is what keeps them honest, and it exercises the
//! adapters against a genuine CLI-shaped child rather than a scripted one.
//!
//! The command is resolved here rather than through `PATH`, because the
//! resolution a `PATH` override gets is platform-dependent and the crate
//! forbids the `unsafe` needed to mutate the process environment.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use orch_core::adapter::{Adapter, AgentEventKind, ExecuteParams};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_domain::{AdapterErrorKind, AgentConfig};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, CodexAdapter, CursorAdapter, GrokAdapter, OpenCodeAdapter,
    PiAdapter, ShellAdapter,
};
use orch_infra::process::SystemProcessManager;
use support::{collect, fixed_clock, params};

fn fake_agents_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents")
}

/// Spawns the real thing, except that a command with a fake in
/// `fixtures/fake-agents/` resolves to that script.
struct FakeAgentProcessManager {
    inner: SystemProcessManager,
}

impl FakeAgentProcessManager {
    fn new() -> Arc<Self> {
        Arc::new(FakeAgentProcessManager {
            inner: SystemProcessManager::new(),
        })
    }
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
        let fake = fake_agents_dir().join(command);
        // Default deny: no silent fallback to `PATH`. A renamed or deleted
        // fixture would otherwise send these tests at the developer's *real*
        // agent CLI — network, credentials, and a suite that passes while
        // testing the wrong binary.
        //
        // `bash` is the one real command allowed through, because the shell
        // adapter's whole purpose is to run one (`bash -lc`), so there is no
        // fixture to rename and nothing to mistake it for.
        let command = if fake.is_file() {
            fake.to_string_lossy().into_owned()
        } else if command == "bash" {
            command.to_owned()
        } else {
            return Err(orch_core::error::Error::other(format!(
                "no fake agent fixture for {command:?}: expected {}",
                fake.display()
            )));
        };
        // The scripts run anywhere; the adapters' workspaces do not exist here.
        let opts = SpawnOptions {
            cwd: std::env::temp_dir(),
            ..opts
        };
        self.inner.spawn(&command, args, opts)
    }
}

fn scenario_env(marker: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("ORCH_FAKE_REPLY".to_owned(), "ORCH_FAKE_OK".to_owned());
    if let Some(marker) = marker {
        env.insert("ORCH_FAKE_SCENARIO".to_owned(), marker.to_owned());
    }
    env
}

fn execute_params(kind: &str, scenario: Option<&str>) -> ExecuteParams {
    let mut execute_params = params("do the work", "/ws");
    execute_params.env = scenario_env(scenario);
    if kind == "shell" {
        execute_params.config = AgentConfig {
            // The shell adapter has no CLI of its own; it runs a command.
            command: Some("printf '%s\\n' \"$ORCHESTRY_TASK_PROMPT\"".to_owned()),
            ..Default::default()
        };
    }
    execute_params
}

fn adapter_for(kind: &str, pm: Arc<dyn ProcessManager>) -> Box<dyn Adapter> {
    match kind {
        "claude" => Box::new(ClaudeAdapter::new(pm).with_clock(fixed_clock())),
        "codex" => Box::new(CodexAdapter::new(pm).with_clock(fixed_clock())),
        "cursor" => Box::new(CursorAdapter::new(pm).with_clock(fixed_clock())),
        "opencode" => Box::new(OpenCodeAdapter::new(pm).with_clock(fixed_clock())),
        "pi" => Box::new(PiAdapter::new(pm).with_clock(fixed_clock())),
        "grok" => Box::new(GrokAdapter::new(pm).with_clock(fixed_clock())),
        "antigravity" => Box::new(AntigravityAdapter::new(pm).with_clock(fixed_clock())),
        "shell" => Box::new(ShellAdapter::new(pm).with_clock(fixed_clock())),
        other => panic!("unknown adapter {other}"),
    }
}

async fn run(kind: &str, scenario: Option<&str>) -> support::Collected {
    let pm = FakeAgentProcessManager::new();
    let adapter = adapter_for(kind, pm as Arc<dyn ProcessManager>);
    let handle = adapter
        .execute(execute_params(kind, scenario))
        .expect("execute");
    collect(handle).await
}

const JSONL_ADAPTERS: [&str; 6] = ["claude", "codex", "cursor", "opencode", "pi", "grok"];

#[tokio::test]
async fn every_fake_cli_drives_its_adapter_to_a_done_event() {
    for kind in JSONL_ADAPTERS.into_iter().chain(["antigravity"]) {
        let collected = run(kind, None).await;
        collected.expect_ok();
        assert_eq!(
            collected.last().kind,
            AgentEventKind::Done,
            "{kind} should end with done, got {:?}",
            collected.kinds()
        );
        assert!(
            collected.has_kind(AgentEventKind::Output),
            "{kind} should report the agent's reply, got {:?}",
            collected.kinds()
        );
        assert!(
            collected
                .of_kind(AgentEventKind::Output)
                .iter()
                .any(|event| serde_json::to_string(&event.data)
                    .expect("json")
                    .contains("ORCH_FAKE_OK")),
            "{kind} lost the reply text: {:?}",
            collected.data()
        );
    }
}

#[tokio::test]
async fn the_shell_fake_round_trips_the_prompt_through_the_environment() {
    let collected = run("shell", None).await;
    collected.expect_ok();
    assert_eq!(
        collected.first().data,
        serde_json::json!("do the work"),
        "the prompt reaches the command through ORCHESTRY_TASK_PROMPT"
    );
}

#[tokio::test]
async fn every_fake_cli_reports_a_version_so_doctor_can_probe_it() {
    for kind in JSONL_ADAPTERS.into_iter().chain(["antigravity", "shell"]) {
        let pm = FakeAgentProcessManager::new();
        let result = adapter_for(kind, pm as Arc<dyn ProcessManager>)
            .test()
            .await;
        assert!(result.ok, "{kind} probe failed: {result:?}");
        assert!(
            result.version.is_some_and(|version| !version.is_empty()),
            "{kind} reported no version"
        );
    }
}

#[tokio::test]
async fn the_fail_scenario_fails_every_adapter_that_can_fail() {
    // grok and antigravity report a non-zero exit; the JSONL adapters report
    // their own terminal error. Either way the run must not look successful.
    for kind in ["claude", "codex", "opencode", "pi", "grok", "antigravity"] {
        let collected = run(kind, Some("fail")).await;
        assert!(
            collected.error.is_some() || collected.has_kind(AgentEventKind::Error),
            "{kind} treated the fail scenario as success: {:?}",
            collected.kinds()
        );
        assert!(
            !collected.has_kind(AgentEventKind::Done),
            "{kind} emitted done for a failed run"
        );
    }
}

#[tokio::test]
async fn the_rate_limit_scenario_is_classified_as_one_where_the_adapter_can_see_it() {
    // pi surfaces the provider message, so its classification is exact.
    let collected = run("pi", Some("rate_limit")).await;
    assert_eq!(
        collected.error.as_ref().expect("error").kind,
        AdapterErrorKind::RateLimit
    );
}

#[tokio::test]
async fn the_stderr_flood_scenario_does_not_deadlock_any_adapter() {
    for kind in JSONL_ADAPTERS.into_iter().chain(["antigravity"]) {
        let collected = run(kind, Some("stderr_flood")).await;
        collected.expect_ok();
        assert_eq!(
            collected.last().kind,
            AgentEventKind::Done,
            "{kind} did not survive 256 KiB of stderr"
        );
    }
}

#[tokio::test]
async fn the_prompt_can_select_a_scenario_on_its_own() {
    // This is how one differential scenario makes a single task fail.
    let pm = FakeAgentProcessManager::new();
    let adapter = adapter_for("claude", pm as Arc<dyn ProcessManager>);
    let mut execute_params = params("please ORCH_FAKE_FAIL now", "/ws");
    execute_params.env = scenario_env(None);

    let handle = adapter.execute(execute_params).expect("execute");
    let collected = collect(handle).await;
    assert!(collected.error.is_some(), "{:?}", collected.kinds());
}

#[test]
fn a_command_with_no_fixture_is_refused_rather_than_resolved_on_path() {
    let pm = FakeAgentProcessManager::new();
    // `cat` exists on every PATH this suite could run on, has no fixture and
    // is not the shell adapter's `bash`, so it distinguishes the two
    // behaviours: refused here, spawned for real if the fallback came back.
    let err = pm
        .spawn("cat", &[], SpawnOptions::default())
        .expect_err("a command with no fixture must not spawn");
    let message = err.to_string();
    assert!(
        message.contains("no fake agent fixture for \"cat\""),
        "{message}"
    );

    // The fixtures these tests rely on really are present, so the guard
    // cannot be masking a broken checkout. These are the file names, which is
    // what the adapters spawn — `cursor-agent`, not `cursor`.
    for adapter in [
        "claude",
        "codex",
        "cursor-agent",
        "opencode",
        "pi",
        "grok",
        "agy",
    ] {
        assert!(
            fake_agents_dir().join(adapter).is_file(),
            "missing fixture for {adapter}"
        );
    }
}
