//! Orchestrator event system (`src/domain/events.ts`).
//!
//! All communication between layers goes through these typed events. The event
//! names and payload field names are contract (PARITY.md §7.19) — serve mode's
//! structured logger flattens them straight into its log records, so the
//! camelCase field names (and the one snake_case outlier, `delay_ms`) are
//! reproduced exactly.

use serde::{Deserialize, Serialize};

use crate::error::AdapterErrorKind;
use crate::goal::GoalStatus;
use crate::message::MessageChannel;
use crate::task::{ReviewResult, Task, TaskStatus};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OrchestratorEvent {
    /// The `Task` is boxed to keep the enum small: every other variant is a
    /// handful of strings, and the bus clones events per subscriber.
    #[serde(rename = "task:created")]
    TaskCreated { task: Box<Task> },

    #[serde(rename = "task:assigned")]
    TaskAssigned {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
    },

    #[serde(rename = "task:status_changed")]
    TaskStatusChanged {
        #[serde(rename = "taskId")]
        task_id: String,
        from: TaskStatus,
        to: TaskStatus,
    },

    #[serde(rename = "task:auto_reviewed")]
    TaskAutoReviewed {
        #[serde(rename = "taskId")]
        task_id: String,
        passed: bool,
        results: Vec<ReviewResult>,
    },

    #[serde(rename = "agent:started")]
    AgentStarted {
        #[serde(rename = "agentId")]
        agent_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "runId")]
        run_id: String,
    },

    #[serde(rename = "agent:output")]
    AgentOutput {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        data: String,
    },

    #[serde(rename = "agent:file_changed")]
    AgentFileChanged {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        path: String,
    },

    #[serde(rename = "agent:completed")]
    AgentCompleted {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        success: bool,
    },

    #[serde(rename = "agent:error")]
    AgentError {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
        error: String,
        #[serde(rename = "errorKind", default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<AdapterErrorKind>,
    },

    #[serde(rename = "run:retry")]
    RunRetry {
        #[serde(rename = "runId")]
        run_id: String,
        attempt: u32,
        /// snake_case, unlike its siblings. On the wire by name, so preserved.
        delay_ms: u64,
    },

    #[serde(rename = "orchestrator:tick")]
    OrchestratorTick { running: usize, queued: usize },

    #[serde(rename = "orchestrator:stall_detected")]
    OrchestratorStallDetected {
        #[serde(rename = "runId")]
        run_id: String,
    },

    #[serde(rename = "task:scope_overlap")]
    TaskScopeOverlap {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "overlappingTaskId")]
        overlapping_task_id: String,
        patterns: Vec<String>,
    },

    #[serde(rename = "task:cascade_failed")]
    TaskCascadeFailed {
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "failedDependencyId")]
        failed_dependency_id: String,
        reason: String,
    },

    #[serde(rename = "workspace:merge_succeeded")]
    WorkspaceMergeSucceeded {
        #[serde(rename = "taskId")]
        task_id: String,
        branch: String,
    },

    #[serde(rename = "workspace:merge_conflict")]
    WorkspaceMergeConflict {
        #[serde(rename = "taskId")]
        task_id: String,
        branch: String,
        #[serde(rename = "conflictInfo")]
        conflict_info: String,
    },

    #[serde(rename = "task:orphaned")]
    TaskOrphaned {
        #[serde(rename = "taskId")]
        task_id: String,
    },

    #[serde(rename = "orchestrator:error")]
    OrchestratorError {
        error: String,
        context: String,
        fatal: bool,
    },

    #[serde(rename = "orchestrator:shutdown")]
    OrchestratorShutdown { reason: String },

    #[serde(rename = "message:sent")]
    MessageSent {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "fromAgentId")]
        from_agent_id: String,
        /// `string | null`, so it emits `null` rather than being omitted.
        #[serde(rename = "toAgentId")]
        to_agent_id: Option<String>,
        channel: MessageChannel,
    },

    #[serde(rename = "message:delivered")]
    MessageDelivered {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "toAgentId")]
        to_agent_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },

    #[serde(rename = "team:created")]
    TeamCreated {
        #[serde(rename = "teamId")]
        team_id: String,
        name: String,
        #[serde(rename = "leadAgentId")]
        lead_agent_id: String,
    },

    #[serde(rename = "team:member_joined")]
    TeamMemberJoined {
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
    },

    #[serde(rename = "team:member_left")]
    TeamMemberLeft {
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
    },

    /// Declared in the TS union but **never emitted** by any code path. Kept
    /// for forward compatibility; nothing emits it (PARITY.md §7.18, §13).
    #[serde(rename = "team:task_claimed")]
    TeamTaskClaimed {
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
    },

    #[serde(rename = "team:disbanded")]
    TeamDisbanded {
        #[serde(rename = "teamId")]
        team_id: String,
    },

    #[serde(rename = "team:task_added")]
    TeamTaskAdded {
        #[serde(rename = "teamId")]
        team_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },

    #[serde(rename = "agent:autonomous_toggled")]
    AgentAutonomousToggled {
        #[serde(rename = "agentId")]
        agent_id: String,
        autonomous: bool,
    },

    #[serde(rename = "goal:created")]
    GoalCreated {
        #[serde(rename = "goalId")]
        goal_id: String,
        title: String,
    },

    #[serde(rename = "goal:status_changed")]
    GoalStatusChanged {
        #[serde(rename = "goalId")]
        goal_id: String,
        from: GoalStatus,
        to: GoalStatus,
    },

    #[serde(rename = "goal:updated")]
    GoalUpdated {
        #[serde(rename = "goalId")]
        goal_id: String,
    },

    #[serde(rename = "goal:deleted")]
    GoalDeleted {
        #[serde(rename = "goalId")]
        goal_id: String,
    },
}

/// The `type` discriminator on its own — the key subscribers register against
/// (`on`, `once`, `off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OrchestratorEventType {
    #[serde(rename = "task:created")]
    TaskCreated,
    #[serde(rename = "task:assigned")]
    TaskAssigned,
    #[serde(rename = "task:status_changed")]
    TaskStatusChanged,
    #[serde(rename = "task:auto_reviewed")]
    TaskAutoReviewed,
    #[serde(rename = "agent:started")]
    AgentStarted,
    #[serde(rename = "agent:output")]
    AgentOutput,
    #[serde(rename = "agent:file_changed")]
    AgentFileChanged,
    #[serde(rename = "agent:completed")]
    AgentCompleted,
    #[serde(rename = "agent:error")]
    AgentError,
    #[serde(rename = "run:retry")]
    RunRetry,
    #[serde(rename = "orchestrator:tick")]
    OrchestratorTick,
    #[serde(rename = "orchestrator:stall_detected")]
    OrchestratorStallDetected,
    #[serde(rename = "task:scope_overlap")]
    TaskScopeOverlap,
    #[serde(rename = "task:cascade_failed")]
    TaskCascadeFailed,
    #[serde(rename = "workspace:merge_succeeded")]
    WorkspaceMergeSucceeded,
    #[serde(rename = "workspace:merge_conflict")]
    WorkspaceMergeConflict,
    #[serde(rename = "task:orphaned")]
    TaskOrphaned,
    #[serde(rename = "orchestrator:error")]
    OrchestratorError,
    #[serde(rename = "orchestrator:shutdown")]
    OrchestratorShutdown,
    #[serde(rename = "message:sent")]
    MessageSent,
    #[serde(rename = "message:delivered")]
    MessageDelivered,
    #[serde(rename = "team:created")]
    TeamCreated,
    #[serde(rename = "team:member_joined")]
    TeamMemberJoined,
    #[serde(rename = "team:member_left")]
    TeamMemberLeft,
    #[serde(rename = "team:task_claimed")]
    TeamTaskClaimed,
    #[serde(rename = "team:disbanded")]
    TeamDisbanded,
    #[serde(rename = "team:task_added")]
    TeamTaskAdded,
    #[serde(rename = "agent:autonomous_toggled")]
    AgentAutonomousToggled,
    #[serde(rename = "goal:created")]
    GoalCreated,
    #[serde(rename = "goal:status_changed")]
    GoalStatusChanged,
    #[serde(rename = "goal:updated")]
    GoalUpdated,
    #[serde(rename = "goal:deleted")]
    GoalDeleted,
}

impl OrchestratorEventType {
    /// All 32 event types, in TS union declaration order.
    pub const ALL: [OrchestratorEventType; 32] = {
        use OrchestratorEventType::*;
        [
            TaskCreated,
            TaskAssigned,
            TaskStatusChanged,
            TaskAutoReviewed,
            AgentStarted,
            AgentOutput,
            AgentFileChanged,
            AgentCompleted,
            AgentError,
            RunRetry,
            OrchestratorTick,
            OrchestratorStallDetected,
            TaskScopeOverlap,
            TaskCascadeFailed,
            WorkspaceMergeSucceeded,
            WorkspaceMergeConflict,
            TaskOrphaned,
            OrchestratorError,
            OrchestratorShutdown,
            MessageSent,
            MessageDelivered,
            TeamCreated,
            TeamMemberJoined,
            TeamMemberLeft,
            TeamTaskClaimed,
            TeamDisbanded,
            TeamTaskAdded,
            AgentAutonomousToggled,
            GoalCreated,
            GoalStatusChanged,
            GoalUpdated,
            GoalDeleted,
        ]
    };

    pub fn as_str(&self) -> &'static str {
        use OrchestratorEventType::*;
        match self {
            TaskCreated => "task:created",
            TaskAssigned => "task:assigned",
            TaskStatusChanged => "task:status_changed",
            TaskAutoReviewed => "task:auto_reviewed",
            AgentStarted => "agent:started",
            AgentOutput => "agent:output",
            AgentFileChanged => "agent:file_changed",
            AgentCompleted => "agent:completed",
            AgentError => "agent:error",
            RunRetry => "run:retry",
            OrchestratorTick => "orchestrator:tick",
            OrchestratorStallDetected => "orchestrator:stall_detected",
            TaskScopeOverlap => "task:scope_overlap",
            TaskCascadeFailed => "task:cascade_failed",
            WorkspaceMergeSucceeded => "workspace:merge_succeeded",
            WorkspaceMergeConflict => "workspace:merge_conflict",
            TaskOrphaned => "task:orphaned",
            OrchestratorError => "orchestrator:error",
            OrchestratorShutdown => "orchestrator:shutdown",
            MessageSent => "message:sent",
            MessageDelivered => "message:delivered",
            TeamCreated => "team:created",
            TeamMemberJoined => "team:member_joined",
            TeamMemberLeft => "team:member_left",
            TeamTaskClaimed => "team:task_claimed",
            TeamDisbanded => "team:disbanded",
            TeamTaskAdded => "team:task_added",
            AgentAutonomousToggled => "agent:autonomous_toggled",
            GoalCreated => "goal:created",
            GoalStatusChanged => "goal:status_changed",
            GoalUpdated => "goal:updated",
            GoalDeleted => "goal:deleted",
        }
    }
}

impl std::fmt::Display for OrchestratorEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OrchestratorEventType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OrchestratorEventType::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

impl OrchestratorEvent {
    /// This event's `type` discriminator.
    pub fn event_type(&self) -> OrchestratorEventType {
        use OrchestratorEventType as T;
        match self {
            OrchestratorEvent::TaskCreated { .. } => T::TaskCreated,
            OrchestratorEvent::TaskAssigned { .. } => T::TaskAssigned,
            OrchestratorEvent::TaskStatusChanged { .. } => T::TaskStatusChanged,
            OrchestratorEvent::TaskAutoReviewed { .. } => T::TaskAutoReviewed,
            OrchestratorEvent::AgentStarted { .. } => T::AgentStarted,
            OrchestratorEvent::AgentOutput { .. } => T::AgentOutput,
            OrchestratorEvent::AgentFileChanged { .. } => T::AgentFileChanged,
            OrchestratorEvent::AgentCompleted { .. } => T::AgentCompleted,
            OrchestratorEvent::AgentError { .. } => T::AgentError,
            OrchestratorEvent::RunRetry { .. } => T::RunRetry,
            OrchestratorEvent::OrchestratorTick { .. } => T::OrchestratorTick,
            OrchestratorEvent::OrchestratorStallDetected { .. } => T::OrchestratorStallDetected,
            OrchestratorEvent::TaskScopeOverlap { .. } => T::TaskScopeOverlap,
            OrchestratorEvent::TaskCascadeFailed { .. } => T::TaskCascadeFailed,
            OrchestratorEvent::WorkspaceMergeSucceeded { .. } => T::WorkspaceMergeSucceeded,
            OrchestratorEvent::WorkspaceMergeConflict { .. } => T::WorkspaceMergeConflict,
            OrchestratorEvent::TaskOrphaned { .. } => T::TaskOrphaned,
            OrchestratorEvent::OrchestratorError { .. } => T::OrchestratorError,
            OrchestratorEvent::OrchestratorShutdown { .. } => T::OrchestratorShutdown,
            OrchestratorEvent::MessageSent { .. } => T::MessageSent,
            OrchestratorEvent::MessageDelivered { .. } => T::MessageDelivered,
            OrchestratorEvent::TeamCreated { .. } => T::TeamCreated,
            OrchestratorEvent::TeamMemberJoined { .. } => T::TeamMemberJoined,
            OrchestratorEvent::TeamMemberLeft { .. } => T::TeamMemberLeft,
            OrchestratorEvent::TeamTaskClaimed { .. } => T::TeamTaskClaimed,
            OrchestratorEvent::TeamDisbanded { .. } => T::TeamDisbanded,
            OrchestratorEvent::TeamTaskAdded { .. } => T::TeamTaskAdded,
            OrchestratorEvent::AgentAutonomousToggled { .. } => T::AgentAutonomousToggled,
            OrchestratorEvent::GoalCreated { .. } => T::GoalCreated,
            OrchestratorEvent::GoalStatusChanged { .. } => T::GoalStatusChanged,
            OrchestratorEvent::GoalUpdated { .. } => T::GoalUpdated,
            OrchestratorEvent::GoalDeleted { .. } => T::GoalDeleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn there_are_32_event_types() {
        assert_eq!(OrchestratorEventType::ALL.len(), 32);
    }

    #[test]
    fn event_type_names_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for t in OrchestratorEventType::ALL {
            assert!(seen.insert(t.as_str()), "duplicate {t}");
            assert_eq!(
                serde_json::to_string(&t).unwrap(),
                format!("\"{}\"", t.as_str())
            );
            assert_eq!(t.as_str().parse::<OrchestratorEventType>(), Ok(t));
        }
    }

    #[test]
    fn event_names_are_namespace_colon_name() {
        for t in OrchestratorEventType::ALL {
            let name = t.as_str();
            let (namespace, rest) = name.split_once(':').expect(name);
            assert!(
                [
                    "task",
                    "agent",
                    "run",
                    "orchestrator",
                    "workspace",
                    "message",
                    "team",
                    "goal"
                ]
                .contains(&namespace),
                "{name}"
            );
            assert!(!rest.is_empty(), "{name}");
        }
    }

    #[test]
    fn payload_field_names_are_camel_case_on_the_wire() {
        let event = OrchestratorEvent::TaskAssigned {
            task_id: "tsk_1".into(),
            agent_id: "agt_1".into(),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "type": "task:assigned", "taskId": "tsk_1", "agentId": "agt_1" })
        );
    }

    #[test]
    fn run_retry_keeps_its_snake_case_delay_field() {
        let event = OrchestratorEvent::RunRetry {
            run_id: "run_1".into(),
            attempt: 2,
            delay_ms: 20_000,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "type": "run:retry", "runId": "run_1", "attempt": 2, "delay_ms": 20_000 })
        );
    }

    #[test]
    fn agent_error_omits_an_absent_error_kind() {
        let event = OrchestratorEvent::AgentError {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            error: "boom".into(),
            error_kind: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert!(value.get("errorKind").is_none());

        let event = OrchestratorEvent::AgentError {
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            error: "boom".into(),
            error_kind: Some(AdapterErrorKind::RateLimit),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap()["errorKind"],
            "rate_limit"
        );
    }

    #[test]
    fn message_sent_emits_a_null_recipient_for_a_broadcast() {
        let event = OrchestratorEvent::MessageSent {
            message_id: "msg_1".into(),
            from_agent_id: "cli".into(),
            to_agent_id: None,
            channel: MessageChannel::Broadcast,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "message:sent",
                "messageId": "msg_1",
                "fromAgentId": "cli",
                "toAgentId": null,
                "channel": "broadcast",
            })
        );
    }

    #[test]
    fn status_changed_carries_the_status_strings() {
        let event = OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "type": "task:status_changed",
                "taskId": "tsk_1",
                "from": "todo",
                "to": "in_progress",
            })
        );
    }

    #[test]
    fn every_event_type_has_a_constructible_variant() {
        // Builds one of each so that `event_type()` is exercised exhaustively
        // and the two enums cannot drift apart.
        let events = sample_events();
        let covered: std::collections::BTreeSet<_> =
            events.iter().map(|e| e.event_type()).collect();
        let missing: Vec<_> = OrchestratorEventType::ALL
            .into_iter()
            .filter(|t| !covered.contains(t))
            .collect();
        assert!(missing.is_empty(), "missing: {missing:?}");
        assert_eq!(covered.len(), 32);
    }

    #[test]
    fn every_event_round_trips_through_json() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).unwrap();
            let back: OrchestratorEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, event, "{json}");
            // The tag matches `event_type()`.
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["type"], event.event_type().as_str());
        }
    }

    fn sample_events() -> Vec<OrchestratorEvent> {
        use OrchestratorEvent::*;
        let task = Task {
            id: "tsk_1".into(),
            title: "t".into(),
            description: String::new(),
            status: TaskStatus::Todo,
            priority: 3,
            assignee: None,
            labels: vec![],
            depends_on: vec![],
            created_at: "t0".into(),
            updated_at: "t0".into(),
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
        };
        vec![
            TaskCreated {
                task: Box::new(task),
            },
            TaskAssigned {
                task_id: "tsk_1".into(),
                agent_id: "agt_1".into(),
            },
            TaskStatusChanged {
                task_id: "tsk_1".into(),
                from: TaskStatus::Todo,
                to: TaskStatus::InProgress,
            },
            TaskAutoReviewed {
                task_id: "tsk_1".into(),
                passed: true,
                results: vec![],
            },
            AgentStarted {
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                run_id: "run_1".into(),
            },
            AgentOutput {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                data: "hello".into(),
            },
            AgentFileChanged {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                path: "src/x.rs".into(),
            },
            AgentCompleted {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                success: true,
            },
            AgentError {
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                error: "boom".into(),
                error_kind: Some(AdapterErrorKind::Timeout),
            },
            RunRetry {
                run_id: "run_1".into(),
                attempt: 1,
                delay_ms: 10_000,
            },
            OrchestratorTick {
                running: 1,
                queued: 2,
            },
            OrchestratorStallDetected {
                run_id: "run_1".into(),
            },
            TaskScopeOverlap {
                task_id: "tsk_1".into(),
                overlapping_task_id: "tsk_2".into(),
                patterns: vec!["src/**".into()],
            },
            TaskCascadeFailed {
                task_id: "tsk_1".into(),
                failed_dependency_id: "tsk_0".into(),
                reason: "dep failed".into(),
            },
            WorkspaceMergeSucceeded {
                task_id: "tsk_1".into(),
                branch: "orch/tsk_1".into(),
            },
            WorkspaceMergeConflict {
                task_id: "tsk_1".into(),
                branch: "orch/tsk_1".into(),
                conflict_info: "CONFLICT".into(),
            },
            TaskOrphaned {
                task_id: "tsk_1".into(),
            },
            OrchestratorError {
                error: "boom".into(),
                context: "tick".into(),
                fatal: false,
            },
            OrchestratorShutdown {
                reason: "SIGINT".into(),
            },
            MessageSent {
                message_id: "msg_1".into(),
                from_agent_id: "cli".into(),
                to_agent_id: None,
                channel: MessageChannel::Broadcast,
            },
            MessageDelivered {
                message_id: "msg_1".into(),
                to_agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
            },
            TeamCreated {
                team_id: "team_1".into(),
                name: "squad".into(),
                lead_agent_id: "agt_1".into(),
            },
            TeamMemberJoined {
                team_id: "team_1".into(),
                agent_id: "agt_2".into(),
            },
            TeamMemberLeft {
                team_id: "team_1".into(),
                agent_id: "agt_2".into(),
            },
            TeamTaskClaimed {
                team_id: "team_1".into(),
                task_id: "tsk_1".into(),
                agent_id: "agt_1".into(),
            },
            TeamDisbanded {
                team_id: "team_1".into(),
            },
            TeamTaskAdded {
                team_id: "team_1".into(),
                task_id: "tsk_1".into(),
            },
            AgentAutonomousToggled {
                agent_id: "agt_1".into(),
                autonomous: true,
            },
            GoalCreated {
                goal_id: "goal_1".into(),
                title: "Ship".into(),
            },
            GoalStatusChanged {
                goal_id: "goal_1".into(),
                from: GoalStatus::Active,
                to: GoalStatus::Achieved,
            },
            GoalUpdated {
                goal_id: "goal_1".into(),
            },
            GoalDeleted {
                goal_id: "goal_1".into(),
            },
        ]
    }
}
