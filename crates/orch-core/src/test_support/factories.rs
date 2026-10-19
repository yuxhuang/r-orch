//! Entity factories with fixed defaults.
//!
//! Overrides are applied with struct-update syntax, which reads the same way and is checked:
//!
//! ```
//! # use orch_core::test_support::make_task;
//! # use orch_domain::{Task, TaskStatus};
//! let task = Task { status: TaskStatus::Review, attempts: 2, ..make_task() };
//! assert_eq!(task.id, "tsk_test1"); // everything else is the default
//! ```

use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, ApprovalPolicy, Run, RunStatus, Task, TaskStatus,
};

/// The timestamp the task and run factories hard-code.
pub const FACTORY_TIMESTAMP: &str = "2025-01-01T00:00:00Z";

pub fn make_task() -> Task {
    Task {
        id: "tsk_test1".into(),
        title: "Test task".into(),
        description: "desc".into(),
        status: TaskStatus::Todo,
        priority: 3,
        assignee: None,
        labels: Vec::new(),
        depends_on: Vec::new(),
        created_at: FACTORY_TIMESTAMP.into(),
        updated_at: FACTORY_TIMESTAMP.into(),
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

/// Note `approval_policy: auto` and `stall_timeout_ms: 300_000` — both are
/// load-bearing for the orchestrator tests, and neither matches the
/// project config default.
pub fn make_agent() -> Agent {
    Agent {
        id: "agt_test1".into(),
        name: "TestAgent".into(),
        adapter: "shell".into(),
        role: None,
        config: AgentConfig {
            command: None,
            model: None,
            effort: None,
            approval_policy: Some(ApprovalPolicy::Auto),
            max_turns: Some(50),
            timeout_ms: Some(3_600_000),
            stall_timeout_ms: Some(300_000),
            env: None,
            system_prompt: None,
            workspace_mode: None,
            skills: None,
        },
        status: AgentStatus::Idle,
        current_task: None,
        autonomous: None,
        stats: AgentStats {
            tasks_completed: 0,
            tasks_failed: 0,
            total_runs: 0,
            total_runtime_ms: 0,
            tokens_used: None,
        },
        last_error: None,
        capabilities: Vec::new(),
        remote: None,
    }
}

pub fn make_run() -> Run {
    Run {
        id: "run_test1".into(),
        task_id: "tsk_test1".into(),
        agent_id: "agt_test1".into(),
        attempt: 1,
        status: RunStatus::Preparing,
        started_at: FACTORY_TIMESTAMP.into(),
        finished_at: None,
        workspace_path: "/tmp/ws".into(),
        prompt: "do stuff".into(),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_factory_uses_its_fixed_defaults() {
        let task = make_task();
        assert_eq!(task.id, "tsk_test1");
        assert_eq!(task.title, "Test task");
        assert_eq!(task.description, "desc");
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.priority, 3);
        assert!(task.labels.is_empty());
        assert!(task.depends_on.is_empty());
        assert_eq!(task.created_at, "2025-01-01T00:00:00Z");
        assert_eq!(task.updated_at, "2025-01-01T00:00:00Z");
        assert_eq!(task.attempts, 0);
        assert_eq!(task.max_attempts, 3);
        // Everything optional is unset, so the serialized shape is minimal.
        let value = serde_json::to_value(&task).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 11);
    }

    #[test]
    fn the_agent_factory_uses_its_fixed_defaults() {
        let agent = make_agent();
        assert_eq!(agent.id, "agt_test1");
        assert_eq!(agent.name, "TestAgent");
        assert_eq!(agent.adapter, "shell");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.config.approval_policy, Some(ApprovalPolicy::Auto));
        assert_eq!(agent.config.max_turns, Some(50));
        assert_eq!(agent.config.timeout_ms, Some(3_600_000));
        assert_eq!(agent.config.stall_timeout_ms, Some(300_000));
        assert_eq!(agent.stats, AgentStats::default());
        assert_eq!(agent.role, None);
    }

    #[test]
    fn the_run_factory_uses_its_fixed_defaults() {
        let run = make_run();
        assert_eq!(run.id, "run_test1");
        assert_eq!(run.task_id, "tsk_test1");
        assert_eq!(run.agent_id, "agt_test1");
        assert_eq!(run.attempt, 1);
        assert_eq!(run.status, RunStatus::Preparing);
        assert_eq!(run.started_at, "2025-01-01T00:00:00Z");
        assert_eq!(run.workspace_path, "/tmp/ws");
        assert_eq!(run.prompt, "do stuff");
        assert_eq!(run.pid, None);
    }

    #[test]
    fn struct_update_syntax_overrides_one_field() {
        let task = Task {
            status: TaskStatus::Done,
            ..make_task()
        };
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(task.id, "tsk_test1");
    }

    #[test]
    fn the_default_run_and_task_ids_line_up() {
        // The factories are wired this way on purpose: a default run points
        // at the default task and the default agent.
        assert_eq!(make_run().task_id, make_task().id);
        assert_eq!(make_run().agent_id, make_agent().id);
    }
}
