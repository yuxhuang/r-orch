//! A scripted adapter: hand it a list of events and it replays them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use orch_domain::{AdapterErrorKind, AgentConfig};

use crate::adapter::{
    Adapter, AdapterError, AdapterTestResult, AgentEvent, ExecuteHandle, ExecuteParams,
};
use crate::error::Result;
use crate::test_support::recorder::Recorder;

/// The parts of an [`ExecuteParams`] worth asserting on.
///
/// `ExecuteParams` itself carries a `CancellationToken` and so is neither
/// `PartialEq` nor usefully comparable; this is the recorded projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeExecuteCall {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub workspace: PathBuf,
    pub env: BTreeMap<String, String>,
    pub config: AgentConfig,
}

/// The pid the TS `createMockAdapter` hands back.
const FAKE_PID: u32 = 99999;

/// Replays a scripted event sequence.
///
/// An `Err` item ends the stream after it is yielded, which is how the TS
/// generator's "threw" case appears to the orchestrator.
#[derive(Clone)]
pub struct FakeAdapter {
    kind: &'static str,
    pid: u32,
    script: Arc<Mutex<Vec<std::result::Result<AgentEvent, AdapterError>>>>,
    test_result: Arc<Mutex<AdapterTestResult>>,
    execute_error: Arc<Mutex<Option<AdapterError>>>,
    pub execute_calls: Recorder<FakeExecuteCall>,
    pub stop_calls: Recorder<u32>,
}

impl Default for FakeAdapter {
    fn default() -> Self {
        FakeAdapter::new()
    }
}

impl FakeAdapter {
    /// `kind: "shell"`, `pid: 99999`, `test() → ok`, an empty event stream —
    /// the TS `createMockAdapter()` defaults.
    pub fn new() -> Self {
        FakeAdapter {
            kind: "shell",
            pid: FAKE_PID,
            script: Arc::new(Mutex::new(Vec::new())),
            test_result: Arc::new(Mutex::new(AdapterTestResult {
                ok: true,
                ..Default::default()
            })),
            execute_error: Arc::new(Mutex::new(None)),
            execute_calls: Recorder::new(),
            stop_calls: Recorder::new(),
        }
    }

    /// Replay these events, in order, then end.
    pub fn with_events(events: impl IntoIterator<Item = AgentEvent>) -> Self {
        let adapter = FakeAdapter::new();
        *adapter.lock_script() = events.into_iter().map(Ok).collect();
        adapter
    }

    pub fn with_kind(mut self, kind: &'static str) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = pid;
        self
    }

    /// Replay these events, then yield `Err` and end — the TS "generator
    /// threw" case, which fails the run.
    pub fn failing_after(
        events: impl IntoIterator<Item = AgentEvent>,
        error: AdapterError,
    ) -> Self {
        let adapter = FakeAdapter::with_events(events);
        adapter.lock_script().push(Err(error));
        adapter
    }

    /// Make `test()` report a failure.
    pub fn set_test_result(&self, result: AdapterTestResult) {
        *self.test_result.lock().unwrap_or_else(|e| e.into_inner()) = result;
    }

    /// Make `execute()` fail outright, before any stream exists.
    pub fn set_execute_error(&self, error: Option<AdapterError>) {
        *self.execute_error.lock().unwrap_or_else(|e| e.into_inner()) = error;
    }

    /// Replace the script between runs.
    pub fn set_events(&self, events: impl IntoIterator<Item = AgentEvent>) {
        *self.lock_script() = events.into_iter().map(Ok).collect();
    }

    fn lock_script(
        &self,
    ) -> std::sync::MutexGuard<'_, Vec<std::result::Result<AgentEvent, AdapterError>>> {
        self.script.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl Adapter for FakeAdapter {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn test(&self) -> AdapterTestResult {
        self.test_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn execute(&self, params: ExecuteParams) -> std::result::Result<ExecuteHandle, AdapterError> {
        self.execute_calls.record(FakeExecuteCall {
            prompt: params.prompt,
            system_prompt: params.system_prompt,
            workspace: params.workspace,
            env: params.env,
            config: params.config,
        });

        if let Some(error) = self
            .execute_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(error);
        }

        let script = self.lock_script().clone();
        Ok(ExecuteHandle {
            pid: self.pid,
            events: Box::pin(stream::iter(script)),
        })
    }

    async fn stop(&self, pid: u32) -> Result<()> {
        self.stop_calls.record(pid);
        Ok(())
    }
}

impl std::fmt::Debug for FakeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeAdapter")
            .field("kind", &self.kind)
            .field("pid", &self.pid)
            .field("scripted_events", &self.lock_script().len())
            .finish()
    }
}

/// An `output` event carrying `{ "text": … }` — the canonical shape.
pub fn output_event(timestamp: &str, text: &str) -> AgentEvent {
    AgentEvent::new(
        crate::adapter::AgentEventKind::Output,
        timestamp,
        serde_json::json!({ "text": text }),
    )
}

/// A `done` event carrying `{ "result": … }`.
pub fn done_event(timestamp: &str) -> AgentEvent {
    AgentEvent::new(
        crate::adapter::AgentEventKind::Done,
        timestamp,
        serde_json::json!({ "result": "ok" }),
    )
}

/// An `error` event carrying `{ "message": … }` and a classification.
pub fn error_event(timestamp: &str, message: &str, kind: AdapterErrorKind) -> AgentEvent {
    AgentEvent::new(
        crate::adapter::AgentEventKind::Error,
        timestamp,
        serde_json::json!({ "message": message }),
    )
    .with_error_kind(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AgentEventKind;
    use futures::StreamExt;
    use tokio_util::sync::CancellationToken;

    fn params() -> ExecuteParams {
        ExecuteParams {
            prompt: "do stuff".into(),
            system_prompt: Some("you are a shell".into()),
            workspace: PathBuf::from("/tmp/ws"),
            env: BTreeMap::from([("ORCH_TASK_ID".to_owned(), "tsk_test1".to_owned())]),
            config: AgentConfig::default(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn defaults_match_the_ts_mock() {
        let adapter = FakeAdapter::new();
        assert_eq!(adapter.kind(), "shell");
        assert!(adapter.test().await.ok);

        let handle = adapter.execute(params()).expect("executes");
        assert_eq!(handle.pid, 99999);
        assert_eq!(handle.events.count().await, 0);
    }

    #[tokio::test]
    async fn replays_the_scripted_events_in_order() {
        let adapter = FakeAdapter::with_events([
            output_event("t0", "hello"),
            output_event("t1", "world"),
            done_event("t2"),
        ]);
        let handle = adapter.execute(params()).expect("executes");
        let kinds: Vec<AgentEventKind> = handle.events.map(|e| e.unwrap().kind).collect().await;
        assert_eq!(
            kinds,
            vec![
                AgentEventKind::Output,
                AgentEventKind::Output,
                AgentEventKind::Done
            ]
        );
    }

    #[tokio::test]
    async fn the_script_can_be_replayed_for_a_second_run() {
        let adapter = FakeAdapter::with_events([done_event("t0")]);
        for _ in 0..2 {
            let handle = adapter.execute(params()).expect("executes");
            assert_eq!(handle.events.count().await, 1);
        }
        assert_eq!(adapter.execute_calls.len(), 2);
    }

    #[tokio::test]
    async fn failing_after_yields_the_error_last_and_then_ends() {
        let adapter = FakeAdapter::failing_after(
            [output_event("t0", "partial")],
            AdapterError::new("Shell command exited with code 1", Some(1)),
        );
        let handle = adapter.execute(params()).expect("executes");
        let items: Vec<_> = handle.events.collect().await;
        assert_eq!(items.len(), 2);
        assert!(items[0].is_ok());
        let err = items[1].as_ref().expect_err("last item is the failure");
        assert_eq!(err.kind, AdapterErrorKind::ProcessCrash);
    }

    #[tokio::test]
    async fn records_the_execute_parameters() {
        let adapter = FakeAdapter::new();
        adapter.execute(params()).expect("executes");

        let call = adapter.execute_calls.first().expect("one call");
        assert_eq!(call.prompt, "do stuff");
        assert_eq!(call.system_prompt.as_deref(), Some("you are a shell"));
        assert_eq!(call.workspace, PathBuf::from("/tmp/ws"));
        assert_eq!(call.env["ORCH_TASK_ID"], "tsk_test1");
    }

    #[tokio::test]
    async fn execute_can_be_made_to_fail_outright() {
        let adapter = FakeAdapter::new();
        adapter.set_execute_error(Some(AdapterError::new("spawn ENOENT", None)));
        let err = adapter.execute(params()).expect_err("fails");
        assert_eq!(err.kind, AdapterErrorKind::SpawnFailed);
        // The call is still recorded.
        assert_eq!(adapter.execute_calls.len(), 1);
    }

    #[tokio::test]
    async fn test_result_is_configurable() {
        let adapter = FakeAdapter::new();
        adapter.set_test_result(AdapterTestResult::failed(
            "claude: command not found",
            AdapterErrorKind::AdapterNotFound,
        ));
        let result = adapter.test().await;
        assert!(!result.ok);
        assert_eq!(result.error_kind, Some(AdapterErrorKind::AdapterNotFound));
    }

    #[tokio::test]
    async fn records_stop_calls() {
        let adapter = FakeAdapter::new();
        adapter.stop(4242).await.unwrap();
        assert_eq!(adapter.stop_calls.calls(), vec![4242]);
    }

    #[tokio::test]
    async fn kind_and_pid_are_overridable_and_it_works_as_a_trait_object() {
        let adapter: Arc<dyn Adapter> =
            Arc::new(FakeAdapter::new().with_kind("claude").with_pid(7));
        assert_eq!(adapter.kind(), "claude");
        assert_eq!(adapter.execute(params()).expect("executes").pid, 7);
    }

    #[test]
    fn the_event_helpers_use_the_canonical_data_shapes() {
        assert_eq!(
            output_event("t0", "hi").data,
            serde_json::json!({ "text": "hi" })
        );
        assert_eq!(done_event("t0").data, serde_json::json!({ "result": "ok" }));
        let error = error_event("t0", "boom", AdapterErrorKind::Timeout);
        assert_eq!(error.data, serde_json::json!({ "message": "boom" }));
        assert_eq!(error.error_kind, Some(AdapterErrorKind::Timeout));
    }
}
