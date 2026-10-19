//! Task state machine — pure functions, no side effects.
//!
//! ```text
//!   todo → in_progress → review → done
//!                      ↘ retrying → in_progress
//!                      ↘ failed (max attempts)
//!   review → todo (rejected)
//!   * → cancelled
//!   failed → todo | retrying   (manual reactivation)
//!   cancelled → todo           (manual reactivation)
//! ```
//!
//! Terminal statuses (`done`, `failed`, `cancelled`) are not auto-dispatched
//! by the orchestrator but may still have manual outgoing transitions.

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::task::{Task, TaskStatus};

/// The transition table. This is contract.
fn valid_transitions(from: TaskStatus) -> &'static [TaskStatus] {
    use TaskStatus::*;
    match from {
        Todo => &[InProgress, Cancelled],
        InProgress => &[Review, Retrying, Failed, Cancelled],
        Retrying => &[InProgress, Failed, Cancelled],
        Review => &[Done, Todo, Cancelled],
        Done => &[],
        Failed => &[Todo, Retrying],
        Cancelled => &[Todo],
    }
}

/// Whether a status transition is valid.
pub fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    valid_transitions(from).contains(&to)
}

/// Whether a status is terminal — the orchestrator will not auto-dispatch or
/// retry it. Terminal tasks may still have valid *manual* transitions
/// (`cancelled → todo`, `failed → todo`).
pub fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Done | TaskStatus::Failed | TaskStatus::Cancelled
    )
}

/// Whether a task is ready for execution.
pub fn is_dispatchable(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Todo | TaskStatus::Retrying)
}

/// A collection a dependency id can be resolved against.
///
/// `is_blocked` accepts either a list or a pre-built lookup, so callers keep
/// one function name.
pub trait TaskSource {
    fn find_task(&self, id: &str) -> Option<&Task>;
}

impl TaskSource for [Task] {
    fn find_task(&self, id: &str) -> Option<&Task> {
        self.iter().find(|t| t.id == id)
    }
}

impl TaskSource for Vec<Task> {
    fn find_task(&self, id: &str) -> Option<&Task> {
        self.as_slice().find_task(id)
    }
}

impl TaskSource for HashMap<String, Task> {
    fn find_task(&self, id: &str) -> Option<&Task> {
        self.get(id)
    }
}

impl TaskSource for IndexMap<String, Task> {
    fn find_task(&self, id: &str) -> Option<&Task> {
        self.get(id)
    }
}

impl<T: TaskSource + ?Sized> TaskSource for &T {
    fn find_task(&self, id: &str) -> Option<&Task> {
        (**self).find_task(id)
    }
}

/// Whether a task is blocked by unfinished dependencies.
///
/// A dependency id missing from the source is treated as **resolved**
/// (deleted = done), never as blocking: dependencies are validated at creation
/// time, so a missing dep at runtime means it was deleted afterwards and must
/// not block its dependents forever.
pub fn is_blocked<S: TaskSource + ?Sized>(task: &Task, all_tasks: &S) -> bool {
    if task.depends_on.is_empty() {
        return false;
    }
    task.depends_on.iter().any(|dep_id| {
        all_tasks
            .find_task(dep_id)
            .is_some_and(|dep| dep.status != TaskStatus::Done)
    })
}

/// The next status after a task failure (run error or shutdown): `retrying`
/// if attempts remain, `failed` otherwise.
pub fn resolve_failure_status(task: &Task) -> TaskStatus {
    if task.attempts < task.max_attempts {
        TaskStatus::Retrying
    } else {
        TaskStatus::Failed
    }
}

/// The next status after an agent completes or fails.
///
/// Success **always** goes through `review`; auto-approve is applied
/// afterwards by the orchestrator as a separate `review → done` transition,
/// which is why `_auto_approve` is ignored here.
pub fn resolve_completion_status(task: &Task, success: bool, _auto_approve: bool) -> TaskStatus {
    if success {
        TaskStatus::Review
    } else {
        resolve_failure_status(task)
    }
}

/// Retry delay with exponential backoff and a cap: `min(base * 2^attempt, max)`.
///
/// The exponent is `attempt`, **not** `attempt - 1`; callers pass
/// `task.attempts - 1`, so the first retry waits exactly `base`.
pub fn calculate_retry_delay(attempt: u32, base_delay_ms: u64, max_delay_ms: u64) -> u64 {
    // saturating_pow/mul keeps a pathological attempt count from overflowing;
    // the result is capped immediately afterwards either way.
    let delay = base_delay_ms.saturating_mul(2u64.saturating_pow(attempt));
    delay.min(max_delay_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal task for the tests.
    fn make_task(id: &str, status: TaskStatus, depends_on: &[&str]) -> Task {
        Task {
            id: id.into(),
            title: "Test task".into(),
            description: String::new(),
            status,
            priority: 3,
            assignee: None,
            labels: vec![],
            depends_on: depends_on.iter().map(|s| (*s).to_owned()).collect(),
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
            attempts: 0,
            max_attempts: 3,
            workspace_mode: None,
            workspace: None,
            proof: None,
            review_criteria: None,
            review_results: None,
            scope: None,
            feedback: None,
            goal_id: None,
            attachments: None,
        }
    }

    fn with_attempts(attempts: u32, max_attempts: u32) -> Task {
        let mut task = make_task("tsk_test", TaskStatus::Todo, &[]);
        task.attempts = attempts;
        task.max_attempts = max_attempts;
        task
    }

    // ── canTransition ──────────────────────────────────────────────────────

    const VALID: &[(TaskStatus, TaskStatus)] = {
        use TaskStatus::*;
        &[
            (Todo, InProgress),
            (Todo, Cancelled),
            (InProgress, Review),
            (InProgress, Retrying),
            (InProgress, Failed),
            (InProgress, Cancelled),
            (Retrying, InProgress),
            (Retrying, Failed),
            (Retrying, Cancelled),
            (Review, Done),
            (Review, Todo),
            (Review, Cancelled),
            (Failed, Todo),
            (Failed, Retrying),
            (Cancelled, Todo),
        ]
    };

    #[test]
    fn allows_every_valid_transition() {
        for (from, to) in VALID {
            assert!(can_transition(*from, *to), "{from} → {to}");
        }
    }

    #[test]
    fn rejects_the_listed_invalid_transitions() {
        use TaskStatus::*;
        let invalid: &[(TaskStatus, TaskStatus)] = &[
            (Todo, Done),
            (Todo, Review),
            (Todo, Failed),
            (Todo, Retrying),
            (InProgress, Done),
            (InProgress, Todo),
            (InProgress, InProgress),
            (Retrying, Todo),
            (Retrying, Review),
            (Retrying, Done),
            (Review, InProgress),
            (Review, Failed),
            (Done, Todo),
            (Done, InProgress),
            (Done, Cancelled),
            (Cancelled, InProgress),
            (Cancelled, Done),
        ];
        for (from, to) in invalid {
            assert!(!can_transition(*from, *to), "{from} → {to}");
        }
    }

    /// V9 — the full 7×7 matrix, so nothing outside `VALID` is reachable.
    #[test]
    fn exhaustive_transition_table() {
        for from in TaskStatus::ALL {
            for to in TaskStatus::ALL {
                let expected = VALID.contains(&(from, to));
                assert_eq!(can_transition(from, to), expected, "{from} → {to}");
            }
        }
    }

    // ── isTerminal / isDispatchable ────────────────────────────────────────

    #[test]
    fn returns_true_for_terminal_statuses() {
        assert!(is_terminal(TaskStatus::Done));
        assert!(is_terminal(TaskStatus::Failed));
        assert!(is_terminal(TaskStatus::Cancelled));
    }

    #[test]
    fn returns_false_for_non_terminal_statuses() {
        assert!(!is_terminal(TaskStatus::Todo));
        assert!(!is_terminal(TaskStatus::InProgress));
        assert!(!is_terminal(TaskStatus::Retrying));
        assert!(!is_terminal(TaskStatus::Review));
    }

    #[test]
    fn dispatchable_is_todo_and_retrying_only() {
        assert!(is_dispatchable(TaskStatus::Todo));
        assert!(is_dispatchable(TaskStatus::Retrying));
        assert!(!is_dispatchable(TaskStatus::InProgress));
        assert!(!is_dispatchable(TaskStatus::Review));
        assert!(!is_dispatchable(TaskStatus::Done));
        assert!(!is_dispatchable(TaskStatus::Failed));
        assert!(!is_dispatchable(TaskStatus::Cancelled));
    }

    // ── isBlocked (slice) ──────────────────────────────────────────────────

    #[test]
    fn not_blocked_without_dependencies() {
        let task = make_task("tsk_test", TaskStatus::Todo, &[]);
        assert!(!is_blocked(&task, &Vec::<Task>::new()));
    }

    #[test]
    fn not_blocked_when_all_dependencies_are_done() {
        let dep = make_task("tsk_dep", TaskStatus::Done, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_dep"]);
        assert!(!is_blocked(&task, &vec![dep, task.clone()]));
    }

    #[test]
    fn blocked_when_a_dependency_is_not_done() {
        let dep = make_task("tsk_dep", TaskStatus::InProgress, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_dep"]);
        assert!(is_blocked(&task, &vec![dep, task.clone()]));
    }

    #[test]
    fn a_missing_dependency_counts_as_resolved() {
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_missing"]);
        assert!(!is_blocked(&task, &vec![task.clone()]));
    }

    #[test]
    fn all_of_several_dependencies_must_be_done() {
        let dep1 = make_task("tsk_d1", TaskStatus::Done, &[]);
        let dep2 = make_task("tsk_d2", TaskStatus::Todo, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_d1", "tsk_d2"]);
        assert!(is_blocked(&task, &vec![dep1, dep2, task.clone()]));
    }

    #[test]
    fn blocked_when_one_dep_is_missing_and_another_is_unfinished() {
        let dep = make_task("tsk_d1", TaskStatus::InProgress, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_missing", "tsk_d1"]);
        assert!(is_blocked(&task, &vec![dep, task.clone()]));
    }

    #[test]
    fn not_blocked_when_one_dep_is_missing_and_another_is_done() {
        let dep = make_task("tsk_d1", TaskStatus::Done, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_missing", "tsk_d1"]);
        assert!(!is_blocked(&task, &vec![dep, task.clone()]));
    }

    // ── isBlocked (map) ────────────────────────────────────────────────────

    fn task_map(tasks: Vec<Task>) -> HashMap<String, Task> {
        tasks.into_iter().map(|t| (t.id.clone(), t)).collect()
    }

    #[test]
    fn map_path_not_blocked_without_dependencies() {
        let task = make_task("tsk_test", TaskStatus::Todo, &[]);
        assert!(!is_blocked(&task, &HashMap::new()));
    }

    #[test]
    fn map_path_not_blocked_when_all_dependencies_are_done() {
        let dep = make_task("tsk_dep", TaskStatus::Done, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_dep"]);
        assert!(!is_blocked(&task, &task_map(vec![dep, task.clone()])));
    }

    #[test]
    fn map_path_blocked_when_a_dependency_is_not_done() {
        let dep = make_task("tsk_dep", TaskStatus::InProgress, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_dep"]);
        assert!(is_blocked(&task, &task_map(vec![dep, task.clone()])));
    }

    #[test]
    fn map_path_missing_dependency_counts_as_resolved() {
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_missing"]);
        assert!(!is_blocked(&task, &task_map(vec![task.clone()])));
    }

    #[test]
    fn map_path_all_of_several_dependencies_must_be_done() {
        let dep1 = make_task("tsk_d1", TaskStatus::Done, &[]);
        let dep2 = make_task("tsk_d2", TaskStatus::Todo, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_d1", "tsk_d2"]);
        assert!(is_blocked(&task, &task_map(vec![dep1, dep2, task.clone()])));
    }

    #[test]
    fn index_map_is_also_a_task_source() {
        let dep = make_task("tsk_dep", TaskStatus::InProgress, &[]);
        let task = make_task("tsk_test", TaskStatus::Todo, &["tsk_dep"]);
        let map: IndexMap<String, Task> = [(dep.id.clone(), dep)].into_iter().collect();
        assert!(is_blocked(&task, &map));
    }

    // ── resolveFailureStatus ───────────────────────────────────────────────

    #[test]
    fn retrying_while_attempts_remain() {
        assert_eq!(
            resolve_failure_status(&with_attempts(1, 3)),
            TaskStatus::Retrying
        );
        assert_eq!(
            resolve_failure_status(&with_attempts(0, 3)),
            TaskStatus::Retrying
        );
    }

    #[test]
    fn failed_once_attempts_are_exhausted() {
        assert_eq!(
            resolve_failure_status(&with_attempts(3, 3)),
            TaskStatus::Failed
        );
        assert_eq!(
            resolve_failure_status(&with_attempts(5, 3)),
            TaskStatus::Failed
        );
        assert_eq!(
            resolve_failure_status(&with_attempts(1, 1)),
            TaskStatus::Failed
        );
    }

    // ── resolveCompletionStatus ────────────────────────────────────────────

    #[test]
    fn success_always_goes_to_review_regardless_of_auto_approve() {
        let task = with_attempts(1, 3);
        assert_eq!(
            resolve_completion_status(&task, true, true),
            TaskStatus::Review
        );
        assert_eq!(
            resolve_completion_status(&task, true, false),
            TaskStatus::Review
        );
    }

    #[test]
    fn failure_delegates_to_resolve_failure_status() {
        assert_eq!(
            resolve_completion_status(&with_attempts(1, 3), false, false),
            TaskStatus::Retrying
        );
        assert_eq!(
            resolve_completion_status(&with_attempts(3, 3), false, false),
            TaskStatus::Failed
        );
        assert_eq!(
            resolve_completion_status(&with_attempts(5, 3), false, false),
            TaskStatus::Failed
        );
        let task = with_attempts(2, 3);
        assert_eq!(
            resolve_completion_status(&task, false, false),
            resolve_failure_status(&task)
        );
    }

    // ── calculateRetryDelay ────────────────────────────────────────────────

    #[test]
    fn calculates_exponential_backoff() {
        assert_eq!(calculate_retry_delay(0, 1000, 60000), 1000);
        assert_eq!(calculate_retry_delay(1, 1000, 60000), 2000);
        assert_eq!(calculate_retry_delay(2, 1000, 60000), 4000);
        assert_eq!(calculate_retry_delay(3, 1000, 60000), 8000);
    }

    #[test]
    fn caps_at_max_delay() {
        assert_eq!(calculate_retry_delay(10, 1000, 60000), 60000);
        assert_eq!(calculate_retry_delay(20, 1000, 60000), 60000);
        // V10: a pathological attempt count must cap, not overflow.
        assert_eq!(calculate_retry_delay(u32::MAX, 1000, 60000), 60000);
    }

    #[test]
    fn works_with_zero_attempt() {
        assert_eq!(calculate_retry_delay(0, 500, 10000), 500);
    }

    #[test]
    fn the_documented_default_sequence() {
        // base 10 s, cap 300 s → 10, 20, 40, 80, 160, 300, 300 …
        let seq: Vec<u64> = (0..7)
            .map(|a| calculate_retry_delay(a, 10_000, 300_000))
            .collect();
        assert_eq!(
            seq,
            vec![10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000]
        );
    }
}
