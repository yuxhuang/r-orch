//! A failed merge-back must still release the agent. The bug was that the
//! `force_task_to_review` early return skipped the `current_task` clear, so the
//! TUI showed an idle agent as busy forever.

use orch_core::test_support::{make_agent, make_run, make_task};
use orch_core::workspace::MergeResult;
use orch_domain::{
    Agent, AgentStatus, OrchestratorEventType, Run, RunStatus, Task, TaskProof, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, state_running};

/// A task mid-run on a worktree branch, with the agent holding it.
fn setup() -> Harness {
    let task = Task {
        id: "tsk_1".into(),
        status: TaskStatus::InProgress,
        attempts: 1,
        workspace: Some("/tmp/worktree".into()),
        proof: Some(TaskProof {
            branch: Some("orch/tsk_1".into()),
            files_changed: vec!["a.ts".into()],
            ..TaskProof::default()
        }),
        ..make_task()
    };
    let agent = Agent {
        id: "agt_1".into(),
        status: AgentStatus::Running,
        current_task: Some("tsk_1".into()),
        ..make_agent()
    };

    let h = Harness::builder()
        .tasks([task])
        .agents([agent])
        .state(state_running("tsk_1", "agt_1", "run_1", 12345, NOW_ISO))
        .build();
    h.runs.seed(Run {
        id: "run_1".into(),
        task_id: "tsk_1".into(),
        agent_id: "agt_1".into(),
        status: RunStatus::Running,
        ..make_run()
    });
    h
}

async fn complete(h: &Harness, files: &[&str]) {
    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success(
            "tsk_1",
            "run_1",
            "agt_1",
            None,
            Some("done".into()),
            files.iter().map(|f| (*f).to_owned()).collect(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_merge_conflict_clears_current_task() {
    let h = setup();
    h.workspace.set_merge_result(MergeResult::Conflict {
        conflict_info: "conflict in a.ts".into(),
    });

    complete(&h, &["a.ts"]).await;

    let agent = h.agent("agt_1");
    assert_eq!(agent.current_task, None);
    assert_eq!(agent.status, AgentStatus::Idle);
}

#[tokio::test]
async fn a_merge_that_errors_outright_clears_current_task() {
    let h = setup();
    h.workspace.faults.fail_always("git merge failed");

    complete(&h, &["b.ts"]).await;

    let agent = h.agent("agt_1");
    assert_eq!(agent.current_task, None);
    assert_eq!(agent.status, AgentStatus::Idle);
}

#[tokio::test]
async fn the_task_lands_in_review_with_the_conflict_named_in_the_summary() {
    let h = setup();
    h.workspace.set_merge_result(MergeResult::Conflict {
        conflict_info: "conflict".into(),
    });

    complete(&h, &["a.ts"]).await;

    let task = h.task("tsk_1");
    assert_eq!(task.status, TaskStatus::Review);
    let summary = task.proof.unwrap().agent_summary.unwrap();
    assert!(
        summary.starts_with("MERGE CONFLICT: conflict"),
        "summary was {summary:?}"
    );

    let conflicts = h.events_of(OrchestratorEventType::WorkspaceMergeConflict);
    assert_eq!(conflicts.len(), 1);
}

#[tokio::test]
async fn a_merge_error_prefixes_the_summary_and_skips_auto_approve() {
    // `make_agent` has `approval_policy: auto`, so a clean merge would have
    // taken this task straight to `done`. A failed merge must not.
    let h = setup();
    h.workspace.faults.fail_always("git merge failed");

    complete(&h, &["b.ts"]).await;

    let task = h.task("tsk_1");
    assert_eq!(task.status, TaskStatus::Review);
    assert!(
        task.proof
            .unwrap()
            .agent_summary
            .unwrap()
            .starts_with("MERGE ERROR: git merge failed")
    );
}

#[tokio::test]
async fn a_clean_merge_reports_success_and_cleans_the_worktree_up() {
    let h = setup();

    complete(&h, &["a.ts"]).await;

    assert_eq!(h.workspace.merged(), vec!["orch/tsk_1".to_owned()]);
    assert_eq!(
        h.workspace.cleaned(),
        vec![("tsk_1".to_owned(), Some("orch/tsk_1".to_owned()))]
    );
    assert_eq!(
        h.events_of(OrchestratorEventType::WorkspaceMergeSucceeded)
            .len(),
        1
    );
    // Auto-approve applies on the happy path.
    assert_eq!(h.task("tsk_1").status, TaskStatus::Done);
}
