//! Event collection and completion handling (PARITY.md §7.8, §7.9, §7.20).
//!
//! `collect_events` runs detached, one task per run. It is the only place that
//! touches state outside a phase, and it does so for one field —
//! `running[task].last_event_at`, the stall clock — through the debounced
//! write.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind};
use orch_core::error::Result;
use orch_core::storage::TaskStore;
use orch_domain::{
    AdapterErrorKind, AgentLastError, AgentStatus, OrchestratorEvent, OrchestratorState,
    ReviewCriterion, RunStatus, RunningEntry, Task, TaskProof, TaskStatus, TokenUsage,
    TokenUsageOpts, classify_adapter_error, create_token_usage, resolve_completion_status,
    resolve_failure_status,
};

use super::{
    MAX_AGENT_SUMMARY_LEN, MAX_BUS_DATA_LEN, MAX_EVENT_DATA_LEN, MAX_LAST_ERROR_LEN, Orchestrator,
};
use crate::agent_service::AgentStatsUpdate;
use crate::orchestrator::tick::epoch_ms_from_iso;
use crate::review_runner::{ReviewRunner, ReviewRunnerOptions, all_passed, format_report};

/// Whether a string is a timestamp `Date.prototype.toISOString` would produce.
///
/// The TS check is `new Date(v).toISOString() === v`, which accepts **only**
/// the canonical form — so `2025-01-01T00:00:00Z` (no milliseconds) is
/// *rejected* even though it parses. Replicated, because the fallback it
/// triggers (mint a fresh timestamp) is what the event stream records.
pub(super) fn is_valid_iso_timestamp(value: &str) -> bool {
    epoch_ms_from_iso(value)
        .map(orch_core::clock::iso_from_epoch_ms)
        .as_deref()
        == Some(value)
}

/// Stringify if not already a string, then truncate with a trailing `…`.
/// Always returns a string, so no caller double-stringifies.
pub(super) fn serialize_event_data(data: &serde_json::Value, max_len: usize) -> String {
    let text = match data {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".to_owned()),
    };
    truncate_with_ellipsis(&text, max_len)
}

fn truncate_with_ellipsis(value: &str, max_len: usize) -> String {
    match value.char_indices().nth(max_len) {
        Some((byte_index, _)) => format!("{}…", &value[..byte_index]),
        None => value.to_owned(),
    }
}

/// `String.prototype.slice(0, n)` over `char`s.
fn slice_chars(value: &str, max: usize) -> &str {
    match value.char_indices().nth(max) {
        Some((byte_index, _)) => &value[..byte_index],
        None => value,
    }
}

/// What one run's event stream produced, accumulated as it is consumed.
#[derive(Default)]
pub(super) struct Collected {
    tokens: Option<TokenUsage>,
    result_text: Option<String>,
    last_agent_message: Option<String>,
    last_error_kind: Option<AdapterErrorKind>,
    /// A set, because an agent commonly edits the same file repeatedly.
    files_changed: BTreeSet<String>,
}

impl Orchestrator {
    /// Drain an adapter's event stream into the run log and the event bus,
    /// then hand off to the success or failure handler.
    pub(super) async fn collect_events(
        &self,
        mut events: BoxStream<'static, std::result::Result<AgentEvent, AdapterError>>,
        run_id: &str,
        task_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        let mut collected = Collected::default();
        let mut stream_error: Option<AdapterError> = None;

        while let Some(item) = events.next().await {
            if self.is_shutting_down() {
                break;
            }
            let event = match item {
                Ok(event) => event,
                // The adapter's generator threw: the run failed. The stream
                // ends here, as the TS generator does.
                Err(err) => {
                    stream_error = Some(err);
                    break;
                }
            };
            self.record_event(&mut collected, event, run_id, task_id, agent_id)
                .await?;
        }

        // Release the cached JSONL append handle for this run either way.
        self.deps().run_store.close_run_events(run_id);

        if let Some(err) = stream_error {
            let error = err.to_string();
            let error_kind = collected.last_error_kind.or(Some(err.kind));
            let entry = {
                let state = self.inner.state.lock().await;
                state
                    .as_ref()
                    .and_then(|state| state.running.get(task_id).cloned())
            };
            match entry {
                Some(entry) => {
                    self.handle_run_failure(task_id, &entry, &error, error_kind)
                        .await?;
                }
                // Reconcile already cleaned the entry up. Finalize the run
                // directly, or it stays `running` forever.
                None => {
                    if let Err(err) = self
                        .deps()
                        .run_service
                        .finish(run_id, RunStatus::Failed, None, Some(error.clone()))
                        .await
                    {
                        self.emit_error(err, format!("finalize orphaned run {run_id}"));
                    }
                }
            }
            return Ok(());
        }

        // The `done` event's result wins; the last assistant message is the
        // fallback for adapters that do not send one.
        let final_result = collected
            .result_text
            .clone()
            .or_else(|| collected.last_agent_message.clone());
        let files: Vec<String> = collected.files_changed.iter().cloned().collect();
        self.handle_run_success(
            task_id,
            run_id,
            agent_id,
            collected.tokens,
            final_result,
            files,
        )
        .await
    }

    pub(super) async fn record_event(
        &self,
        collected: &mut Collected,
        event: AgentEvent,
        run_id: &str,
        task_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        let tap = self
            .inner
            .event_tap
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(tap) = tap {
            tap.record(task_id, run_id, &event);
        }
        match event.kind {
            AgentEventKind::Done => {
                if let Some(tokens) = event.tokens {
                    collected.tokens = Some(create_token_usage(
                        tokens.input,
                        tokens.output,
                        Some(TokenUsageOpts {
                            reasoning: Some(tokens.reasoning),
                            cache_read: Some(tokens.cache_read),
                            cache_write: Some(tokens.cache_write),
                        }),
                    ));
                }
                if let Some(result) = event.data.get("result").and_then(|v| v.as_str()) {
                    collected.result_text = Some(result.to_owned());
                }
            }
            AgentEventKind::Output => {
                // `text` is the canonical field; `message` is what a couple of
                // adapters emit instead.
                let text = event
                    .data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.data.get("message").and_then(|v| v.as_str()));
                if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
                    collected.last_agent_message = Some(text.to_owned());
                }
            }
            AgentEventKind::FileChange => {
                collected
                    .files_changed
                    .extend(file_change_paths(&event.data));
            }
            _ => {}
        }

        // Claude emits `tool_use` with the file path but no separate
        // `file_change` event, so writes have to be recovered from the tool
        // call — and only from the tools that actually write.
        let tool_call_file_path = match event.kind {
            AgentEventKind::ToolCall => write_tool_file_path(&event.data),
            _ => None,
        };
        if let Some(path) = tool_call_file_path.as_deref() {
            collected.files_changed.insert(path.to_owned());
        }

        let event_timestamp = if is_valid_iso_timestamp(&event.timestamp) {
            event.timestamp.clone()
        } else {
            self.deps().clock.now_iso()
        };

        let file_change_path = match event.kind {
            AgentEventKind::FileChange => Some(primary_file_change_path(&event.data)),
            _ => None,
        };

        // Serialized once and reused: the JSONL write and the bus emit differ
        // only in their cap.
        let serialized = serialize_event_data(&event.data, MAX_EVENT_DATA_LEN);
        self.deps()
            .run_service
            .append_event(
                run_id,
                &orch_domain::RunEvent {
                    timestamp: event_timestamp.clone(),
                    event_type: event.kind.to_run_event_type(),
                    data: serde_json::Value::String(serialized.clone()),
                },
            )
            .await?;

        // The stall clock advances per *emitted event*, not per stdout line.
        {
            let mut slot = self.inner.state.lock().await;
            if let Some(entry) = slot
                .as_mut()
                .and_then(|state| state.running.get_mut(task_id))
            {
                entry.last_event_at = event_timestamp;
            }
        }
        self.save_state_lazy().await;

        let bus_data =
            serialize_event_data(&serde_json::Value::String(serialized), MAX_BUS_DATA_LEN);
        match event.kind {
            AgentEventKind::Output | AgentEventKind::ToolCall => {
                self.emit(OrchestratorEvent::AgentOutput {
                    run_id: run_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    data: bus_data,
                });
                if let Some(path) = tool_call_file_path {
                    self.emit(OrchestratorEvent::AgentFileChanged {
                        run_id: run_id.to_owned(),
                        agent_id: agent_id.to_owned(),
                        path,
                    });
                }
            }
            AgentEventKind::FileChange => {
                self.emit(OrchestratorEvent::AgentFileChanged {
                    run_id: run_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    path: file_change_path.unwrap_or_default(),
                });
            }
            AgentEventKind::Error => {
                if let Some(kind) = event.error_kind {
                    collected.last_error_kind = Some(kind);
                }
                self.emit(OrchestratorEvent::AgentError {
                    run_id: run_id.to_owned(),
                    agent_id: agent_id.to_owned(),
                    error: bus_data,
                    error_kind: event.error_kind,
                });
            }
            AgentEventKind::Command | AgentEventKind::Done => {}
        }
        Ok(())
    }

    // ── completion ───────────────────────────────────────────────────────

    pub(super) async fn handle_run_success(
        &self,
        task_id: &str,
        run_id: &str,
        agent_id: &str,
        tokens: Option<TokenUsage>,
        result_text: Option<String>,
        files_changed: Vec<String>,
    ) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        let Some(state) = slot.as_mut() else {
            return Ok(());
        };
        self.handle_run_success_locked(
            state,
            task_id,
            run_id,
            agent_id,
            tokens,
            result_text,
            files_changed,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_run_success_locked(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        run_id: &str,
        agent_id: &str,
        tokens: Option<TokenUsage>,
        result_text: Option<String>,
        files_changed: Vec<String>,
    ) -> Result<()> {
        self.flush_state_lazy(state).await;
        self.drop_cancel(task_id).await;

        // Already cancelled, or the record is gone: nothing to complete.
        if !state.running.contains_key(task_id) {
            return Ok(());
        }
        let Some(mut task) = self.deps().task_store.get(task_id).await? else {
            return Ok(());
        };

        // An adapter that reports no files does not mean none changed — the
        // worktree branch knows better.
        let mut effective_files = files_changed;
        if effective_files.is_empty()
            && let Some(branch) = task.proof.as_ref().and_then(|p| p.branch.as_deref())
        {
            effective_files = self.deps().workspace.get_changed_files(branch).await?;
        }

        let existing_proof = task.proof.clone().unwrap_or_default();
        task.proof = Some(TaskProof {
            agent_summary: result_text
                .as_deref()
                .map(|text| slice_chars(text, MAX_AGENT_SUMMARY_LEN).to_owned())
                .or(existing_proof.agent_summary.clone()),
            files_changed: if effective_files.is_empty() {
                existing_proof.files_changed.clone()
            } else {
                effective_files
            },
            ..existing_proof
        });
        // Stale reviewer feedback must not survive into the next prompt.
        task.feedback = None;
        self.deps().task_store.save(&task).await?;

        let agent = self.deps().agent_store.get(agent_id).await?;
        let auto_approve = task.is_autonomous()
            || agent
                .as_ref()
                .and_then(|agent| agent.config.approval_policy)
                == Some(orch_domain::ApprovalPolicy::Auto);
        // Always `review`; auto-approve is applied afterwards as a separate
        // `review → done` transition.
        let new_status = resolve_completion_status(&task, true, auto_approve);

        self.deps()
            .run_service
            .finish(run_id, RunStatus::Succeeded, tokens, None)
            .await?;

        let runtime_ms = state
            .running
            .get(task_id)
            .and_then(|entry| self.runtime_ms(entry))
            .unwrap_or(0);
        if state.running.contains_key(task_id) {
            state.stats.total_runtime_ms += runtime_ms;
        }
        // Dropped **before** any fallible work below, so a later throw cannot
        // re-enter the failure path for a run that already succeeded.
        state.running.shift_remove(task_id);

        let previous = agent.as_ref().map(|agent| agent.stats.clone());
        let stats_update = AgentStatsUpdate {
            tasks_completed: Some(previous.as_ref().map_or(0, |s| s.tasks_completed) + 1),
            total_runs: Some(previous.as_ref().map_or(0, |s| s.total_runs) + 1),
            total_runtime_ms: Some(
                previous.as_ref().map_or(0, |s| s.total_runtime_ms) + runtime_ms,
            ),
            tokens_used: tokens.map(|tokens| {
                previous.as_ref().and_then(|s| s.tokens_used).unwrap_or(0) + tokens.total
            }),
            ..Default::default()
        };
        if let Err(err) = self
            .deps()
            .agent_service
            .update_stats(agent_id, stats_update)
            .await
        {
            self.emit_error(err, format!("agent stats update for {agent_id}"));
        }

        state.stats.total_tasks_completed += 1;
        state.stats.total_runs += 1;
        if let Some(tokens) = tokens {
            let total = &mut state.stats.total_tokens;
            total.input += tokens.input;
            total.output += tokens.output;
            total.reasoning += tokens.reasoning;
            total.cache_read += tokens.cache_read;
            total.cache_write += tokens.cache_write;
            // Recomputed rather than accumulated, so it cannot drift.
            total.total = total.input + total.output + total.reasoning;
        }

        // Merge-back. A conflict or an infrastructure error forces the task to
        // `review` and **returns** — auto-approve is skipped, because nobody
        // should approve work that did not land.
        if let Some(branch) = task.proof.as_ref().and_then(|p| p.branch.clone()) {
            match self.deps().workspace.merge_back(&branch).await {
                Ok(result) if result.success() => {
                    self.emit(OrchestratorEvent::WorkspaceMergeSucceeded {
                        task_id: task_id.to_owned(),
                        branch: branch.clone(),
                    });
                    if let Err(err) = self.deps().workspace.cleanup(task_id, Some(&branch)).await {
                        self.emit_error(err, format!("workspace cleanup for {task_id}"));
                    }
                }
                Ok(result) => {
                    let conflict_info = result.conflict_info().unwrap_or_default().to_owned();
                    self.emit(OrchestratorEvent::WorkspaceMergeConflict {
                        task_id: task_id.to_owned(),
                        branch,
                        conflict_info: conflict_info.clone(),
                    });
                    self.force_task_to_review(
                        state,
                        task,
                        agent_id,
                        &format!("MERGE CONFLICT: {conflict_info}"),
                    )
                    .await?;
                    return Ok(());
                }
                Err(err) => {
                    self.force_task_to_review(
                        state,
                        task,
                        agent_id,
                        &format!("MERGE ERROR: {err}"),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        if let Err(err) = self
            .deps()
            .task_service
            .update_status(task_id, new_status)
            .await
        {
            // The state machine refused, but the work is done: force-write,
            // and say so loudly enough to be noticed in a log.
            self.emit_error(
                &err,
                format!(
                    "state machine validation failed for task {task_id} -> {new_status}, force-writing"
                ),
            );
            task.status = new_status;
            task.updated_at = self.deps().clock.now_iso();
            if let Err(err) = self.deps().task_store.save(&task).await {
                self.emit_error(err, format!("force-write task {task_id} to store failed"));
            }
        }
        if let Err(err) = self
            .deps()
            .agent_service
            .set_status(agent_id, AgentStatus::Idle)
            .await
        {
            self.emit_error(
                err,
                format!("_handleRunSuccess setStatus idle for agent {agent_id}"),
            );
        }
        if let Some(mut agent_after) = self.deps().agent_store.get(agent_id).await? {
            agent_after.current_task = None;
            self.deps().agent_store.save(&agent_after).await?;
        }

        let criteria = task.review_criteria.clone().unwrap_or_default();
        if new_status == TaskStatus::Review && !criteria.is_empty() {
            let cwd = task
                .workspace
                .clone()
                .unwrap_or_else(|| self.deps().project_root.display().to_string());
            self.run_auto_review(task_id, &criteria, &cwd, auto_approve)
                .await?;
        } else if new_status == TaskStatus::Review && auto_approve {
            let _ = self
                .deps()
                .task_service
                .update_status(task_id, TaskStatus::Done)
                .await;
        }

        self.save_state(state).await?;
        self.schedule_dispatch_after_run(task_id).await;
        Ok(())
    }

    pub(super) async fn handle_run_failure(
        &self,
        task_id: &str,
        entry: &RunningEntry,
        error: &str,
        error_kind: Option<AdapterErrorKind>,
    ) -> Result<()> {
        let mut slot = self.inner.state.lock().await;
        let Some(state) = slot.as_mut() else {
            return Ok(());
        };
        self.handle_run_failure_locked(state, task_id, entry, error, error_kind)
            .await
    }

    pub(super) async fn handle_run_failure_locked(
        &self,
        state: &mut OrchestratorState,
        task_id: &str,
        entry: &RunningEntry,
        error: &str,
        error_kind: Option<AdapterErrorKind>,
    ) -> Result<()> {
        self.flush_state_lazy(state).await;
        self.drop_cancel(task_id).await;

        // The success handler may have got there first.
        if !state.running.contains_key(task_id) {
            return Ok(());
        }
        let Some(task) = self.deps().task_store.get(task_id).await? else {
            return Ok(());
        };

        self.deps()
            .run_service
            .finish_with_error_kind(
                &entry.run_id,
                RunStatus::Failed,
                None,
                Some(error.to_owned()),
                error_kind,
            )
            .await?;
        if entry.remote.is_none() {
            self.deps()
                .agent_service
                .set_status(&entry.agent_id, AgentStatus::Idle)
                .await?;
        } else {
            let event = AgentEvent::new(
                AgentEventKind::Error,
                self.deps().clock.now_iso(),
                serde_json::json!({"message":error,"errorKind":error_kind}),
            );
            self.emit(OrchestratorEvent::AgentError {
                run_id: entry.run_id.clone(),
                agent_id: entry.agent_id.clone(),
                error: error.to_owned(),
                error_kind,
            });
            self.deps()
                .run_store
                .append_event(
                    &entry.run_id,
                    &orch_domain::RunEvent {
                        timestamp: event.timestamp,
                        event_type: orch_domain::RunEventType::Error,
                        data: event.data,
                    },
                )
                .await?;
        }
        let agent_after = if entry.remote.is_none() {
            self.deps().agent_store.get(&entry.agent_id).await?
        } else {
            None
        };
        if let Some(mut agent) = agent_after.clone() {
            agent.current_task = None;
            agent.last_error = Some(AgentLastError {
                message: slice_chars(error, MAX_LAST_ERROR_LEN).to_owned(),
                kind: error_kind
                    .unwrap_or_else(|| classify_adapter_error(error, None))
                    .as_str()
                    .to_owned(),
                timestamp: self.deps().clock.now_iso(),
            });
            self.deps().agent_store.save(&agent).await?;
        }

        let runtime_ms = self.runtime_ms(entry).unwrap_or(0);
        let previous = agent_after.as_ref().map(|agent| agent.stats.clone());
        if entry.remote.is_none() {
            self.deps()
                .agent_service
                .update_stats(
                    &entry.agent_id,
                    AgentStatsUpdate {
                        tasks_failed: Some(previous.as_ref().map_or(0, |s| s.tasks_failed) + 1),
                        total_runs: Some(previous.as_ref().map_or(0, |s| s.total_runs) + 1),
                        total_runtime_ms: Some(
                            previous.as_ref().map_or(0, |s| s.total_runtime_ms) + runtime_ms,
                        ),
                        ..Default::default()
                    },
                )
                .await?;
        }

        let failure_status = if orch_domain::is_terminal(task.status) {
            task.status
        } else {
            resolve_failure_status(&task)
        };
        if task.status != failure_status {
            self.deps()
                .task_service
                .update_status(task_id, failure_status)
                .await?;
        }

        if failure_status == TaskStatus::Retrying {
            // `attempts - 1` as the exponent, so the first retry waits exactly
            // `retry_base_delay_ms` (PARITY.md §7.1).
            let delay = orch_domain::calculate_retry_delay(
                task.attempts.saturating_sub(1),
                self.config().scheduling.retry_base_delay_ms,
                self.config().scheduling.retry_max_delay_ms,
            );
            self.enqueue_retry(state, task_id, task.attempts + 1, delay, error);
            self.emit(OrchestratorEvent::RunRetry {
                run_id: entry.run_id.clone(),
                attempt: task.attempts + 1,
                delay_ms: delay,
            });
        } else {
            self.inner.cached_tasks.invalidate_async().await;
            let all_tasks = self.inner.cached_tasks.list(&Default::default()).await?;
            self.cascade_fail_dependents(
                task_id,
                &all_tasks,
                &format!("dependency {task_id} failed: {error}"),
            )
            .await;
        }

        if entry.remote.is_none()
            && let Some(branch) = task.proof.as_ref().and_then(|p| p.branch.as_deref())
            && let Err(err) = self.deps().workspace.cleanup(task_id, Some(branch)).await
        {
            self.emit_error(err, format!("workspace cleanup for {task_id}"));
        }

        state.running.shift_remove(task_id);
        state.stats.total_runtime_ms += runtime_ms;
        state.stats.total_runs += 1;
        if failure_status == TaskStatus::Failed {
            state.stats.total_tasks_failed += 1;
        }
        if entry.remote.is_some() {
            self.save_remote_state(state).await?;
        } else {
            self.save_state(state).await?;
        }
        self.schedule_dispatch_after_run(task_id).await;
        Ok(())
    }

    /// Run the review criteria and, if they pass — or if auto-approve is set —
    /// move the task to `done` (PARITY.md §7.9).
    async fn run_auto_review(
        &self,
        task_id: &str,
        criteria: &[ReviewCriterion],
        cwd: &str,
        auto_approve: bool,
    ) -> Result<()> {
        let runner = ReviewRunner::with_runner(
            ReviewRunnerOptions::new(cwd),
            Arc::clone(&self.deps().command_runner),
        );
        let results = runner.run_all(criteria).await;
        let passed = all_passed(&results);

        let Some(mut task) = self.deps().task_store.get(task_id).await? else {
            return Ok(());
        };
        let existing = task.proof.clone().unwrap_or_default();
        task.review_results = Some(results.clone());
        task.proof = Some(TaskProof {
            test_results: Some(format_report(&results)),
            ..existing
        });
        self.deps().task_store.save(&task).await?;

        self.emit(OrchestratorEvent::TaskAutoReviewed {
            task_id: task_id.to_owned(),
            passed,
            results,
        });

        if !passed
            && (!auto_approve
                || task
                    .labels
                    .iter()
                    .any(|label| label.starts_with("remote-ctl:")))
        {
            return Ok(());
        }
        if !passed {
            // Auto-approve overriding a failing review is a decision worth a
            // record: it is how a bad change reaches `done`.
            self.emit_error(
                format!(
                    "Review criteria failed for task {task_id} but autoApprove is set — force-approving"
                ),
                "auto-review-with-auto-approve",
            );
        }
        if let Err(err) = self
            .deps()
            .task_service
            .update_status(task_id, TaskStatus::Done)
            .await
        {
            self.emit_error(
                &err,
                format!("auto-review transition failed for task {task_id} -> done, force-writing"),
            );
            task.status = TaskStatus::Done;
            task.updated_at = self.deps().clock.now_iso();
            if let Err(err) = self.deps().task_store.save(&task).await {
                self.emit_error(
                    err,
                    format!("force-write task {task_id} to store failed (auto-review)"),
                );
            }
        }
        Ok(())
    }

    /// Force a task to `review` with a prefixed summary. Used when merge-back
    /// fails, where the work exists but is not on the target branch, so a
    /// human has to look.
    pub(super) async fn force_task_to_review(
        &self,
        state: &OrchestratorState,
        mut task: Task,
        agent_id: &str,
        summary_prefix: &str,
    ) -> Result<()> {
        let existing = task.proof.clone().unwrap_or_default();
        let summary = format!(
            "{summary_prefix}\n\n{}",
            existing.agent_summary.clone().unwrap_or_default()
        );
        task.proof = Some(TaskProof {
            agent_summary: Some(slice_chars(&summary, MAX_AGENT_SUMMARY_LEN).to_owned()),
            ..existing
        });
        task.status = TaskStatus::Review;
        task.updated_at = self.deps().clock.now_iso();
        self.deps().task_store.save(&task).await?;

        if !agent_id.contains('@')
            && let Err(err) = self
                .deps()
                .agent_service
                .set_status(agent_id, AgentStatus::Idle)
                .await
        {
            self.emit_error(
                err,
                format!("forceTaskToReview setStatus idle for agent {agent_id}"),
            );
        }
        if !agent_id.contains('@')
            && let Some(mut agent_after) = self.deps().agent_store.get(agent_id).await?
        {
            agent_after.current_task = None;
            self.deps().agent_store.save(&agent_after).await?;
        }

        self.save_state(state).await
    }

    /// Schedule a reactive dispatch now that an agent is free — unless this
    /// was a `run <id>` single-task run, which must not start anything else.
    pub(super) async fn schedule_dispatch_after_run(&self, task_id: &str) {
        let was_single = self.inner.single_task_runs.lock().await.remove(task_id);
        if !was_single {
            self.schedule_immediate_dispatch(0);
        }
    }

    /// Wall-clock runtime of a running entry. `None` when `started_at` does
    /// not parse, where JS would produce `NaN` and poison the counter.
    pub(super) fn runtime_ms(&self, entry: &RunningEntry) -> Option<u64> {
        let started_at = epoch_ms_from_iso(&entry.started_at)?;
        Some(self.deps().clock.now_ms().saturating_sub(started_at).max(0) as u64)
    }
}

/// Every path a `file_change` event carries. Codex sends `{ paths: [...] }`;
/// the others send a single `path`, or a bare string.
fn file_change_paths(data: &serde_json::Value) -> Vec<String> {
    if let Some(paths) = data.get("paths").and_then(|v| v.as_array()) {
        return paths
            .iter()
            .filter_map(|p| p.as_str().map(str::to_owned))
            .collect();
    }
    vec![primary_file_change_path(data)]
}

/// The single path a `file_change` event names, for the `agent:file_changed`
/// emit. `String(event.data)` is the last resort, as in TS — the field must
/// never come out as the literal `"undefined"`, which was bug-dom-002.
fn primary_file_change_path(data: &serde_json::Value) -> String {
    if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
        return path.to_owned();
    }
    match data {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The file a write-shaped `tool_call` touched, if any.
fn write_tool_file_path(data: &serde_json::Value) -> Option<String> {
    let file_path = data
        .get("input")
        .and_then(|input| input.get("file_path"))
        .and_then(|v| v.as_str())?;
    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
    const WRITE_TOOLS: [&str; 4] = ["write", "edit", "multiedit", "notebookedit"];
    WRITE_TOOLS
        .contains(&name.to_lowercase().as_str())
        .then(|| file_path.to_owned())
}
