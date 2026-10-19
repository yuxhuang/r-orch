//! The interaction between `review_criteria` and auto-approve: criteria that
//! pass promote the task, criteria that fail are *overridden* by auto-approve
//! but leave a record, and no criteria at all means auto-approve promotes the
//! task directly.

use orch_core::test_support::{make_agent, make_run, make_task};
use orch_domain::{
    Agent, AgentStatus, ApprovalPolicy, OrchestratorEvent, OrchestratorEventType, ReviewCriterion,
    Run, RunStatus, Task, TaskStatus,
};

use super::harness::{Harness, NOW_ISO, review_fail, review_pass, state_running};

struct Setup {
    harness: Harness,
}

fn setup(auto_approve: bool, criteria: Vec<ReviewCriterion>, criteria_pass: bool) -> Setup {
    let policy = if auto_approve {
        ApprovalPolicy::Auto
    } else {
        ApprovalPolicy::Suggest
    };
    let mut agent = Agent {
        id: "agt_ar1".into(),
        status: AgentStatus::Running,
        current_task: Some("tsk_ar1".into()),
        ..make_agent()
    };
    agent.config.approval_policy = Some(policy);

    let task = Task {
        id: "tsk_ar1".into(),
        status: TaskStatus::InProgress,
        attempts: 1,
        review_criteria: Some(criteria),
        ..make_task()
    };

    let harness = Harness::builder()
        .tasks([task])
        .agents([agent])
        .state(state_running(
            "tsk_ar1", "agt_ar1", "run_ar1", 12345, NOW_ISO,
        ))
        .review_outcome(if criteria_pass {
            review_pass()
        } else {
            review_fail()
        })
        .build();
    harness.runs.seed(Run {
        id: "run_ar1".into(),
        task_id: "tsk_ar1".into(),
        agent_id: "agt_ar1".into(),
        status: RunStatus::Running,
        ..make_run()
    });
    Setup { harness }
}

async fn complete(h: &Harness) {
    h.load_state().await.unwrap();
    h.orchestrator
        .handle_run_success(
            "tsk_ar1",
            "run_ar1",
            "agt_ar1",
            None,
            Some("result text".into()),
            vec![],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn passing_criteria_plus_auto_approve_reaches_done() {
    let s = setup(true, vec![ReviewCriterion::TestPass], true);
    complete(&s.harness).await;
    assert_eq!(s.harness.task("tsk_ar1").status, TaskStatus::Done);
}

#[tokio::test]
async fn failing_criteria_are_overridden_by_auto_approve() {
    let s = setup(true, vec![ReviewCriterion::TestPass], false);
    complete(&s.harness).await;
    assert_eq!(s.harness.task("tsk_ar1").status, TaskStatus::Done);
}

#[tokio::test]
async fn an_overridden_failure_is_reported_as_an_orchestrator_error() {
    let s = setup(true, vec![ReviewCriterion::TestPass], false);
    complete(&s.harness).await;

    let warning = s
        .harness
        .events_of(OrchestratorEventType::OrchestratorError)
        .into_iter()
        .find(|event| match event {
            OrchestratorEvent::OrchestratorError { context, .. } => {
                context == "auto-review-with-auto-approve"
            }
            _ => false,
        })
        .expect("a force-approve warning");
    match warning {
        OrchestratorEvent::OrchestratorError { error, fatal, .. } => {
            assert!(error.contains("force-approving"), "error was {error:?}");
            assert!(!fatal);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn failing_criteria_without_auto_approve_stay_in_review() {
    let s = setup(false, vec![ReviewCriterion::TestPass], false);
    complete(&s.harness).await;
    assert_eq!(s.harness.task("tsk_ar1").status, TaskStatus::Review);
}

#[tokio::test]
async fn passing_criteria_without_auto_approve_still_reach_done() {
    // All-passed is sufficient on its own; auto-approve is the *override*.
    let s = setup(false, vec![ReviewCriterion::TestPass], true);
    complete(&s.harness).await;
    assert_eq!(s.harness.task("tsk_ar1").status, TaskStatus::Done);
}

#[tokio::test]
async fn the_review_results_are_recorded_even_when_auto_approve_overrides_them() {
    let s = setup(true, vec![ReviewCriterion::TestPass], false);
    complete(&s.harness).await;

    let task = s.harness.task("tsk_ar1");
    let results = task.review_results.expect("review results");
    assert_eq!(results.len(), 1);
    assert!(!results[0].passed);
    assert_eq!(results[0].criterion, ReviewCriterion::TestPass);
    // And the formatted report landed on the proof.
    assert!(
        task.proof
            .unwrap()
            .test_results
            .unwrap()
            .contains("✗ test_pass: FAILED")
    );

    let reviewed = s.harness.events_of(OrchestratorEventType::TaskAutoReviewed);
    assert_eq!(reviewed.len(), 1);
    match &reviewed[0] {
        OrchestratorEvent::TaskAutoReviewed {
            task_id, passed, ..
        } => {
            assert_eq!(task_id, "tsk_ar1");
            assert!(!passed);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn auto_approve_with_no_criteria_promotes_directly_and_runs_nothing() {
    let s = setup(true, vec![], true);
    complete(&s.harness).await;

    let task = s.harness.task("tsk_ar1");
    assert_eq!(task.status, TaskStatus::Done);
    assert_eq!(task.review_results, None);
    assert!(s.harness.commands.calls().is_empty());
}

#[tokio::test]
async fn criteria_run_in_the_tasks_own_workspace() {
    let s = setup(true, vec![ReviewCriterion::Typecheck], true);
    {
        let mut task = s.harness.task("tsk_ar1");
        task.workspace = Some("/tmp/worktrees/tsk_ar1".into());
        s.harness.tasks.seed(task);
    }
    complete(&s.harness).await;

    let calls = s.harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "npx");
    assert_eq!(calls[0].1, vec!["tsc".to_owned(), "--noEmit".to_owned()]);
    assert_eq!(
        calls[0].2,
        std::path::PathBuf::from("/tmp/worktrees/tsk_ar1")
    );
}

#[tokio::test]
async fn criteria_fall_back_to_the_project_root_when_the_task_has_no_workspace() {
    let s = setup(true, vec![ReviewCriterion::Lint], true);
    complete(&s.harness).await;

    let calls = s.harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].2, std::path::PathBuf::from("/tmp/project"));
}
