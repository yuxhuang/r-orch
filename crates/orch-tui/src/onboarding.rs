//! Onboarding state machine and per-tab empty states.

use orch_domain::{OrchestratorEvent, OrchestratorState, TaskStatus};

use crate::state::ViewId;

/// `completed → dismissed` delay.
pub const COMPLETED_DISMISS_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    Welcome,
    TaskCreated,
    RunStarted,
    Completed,
    Dismissed,
}

impl OnboardingStep {
    /// The five values, in order.
    pub const ALL: [OnboardingStep; 5] = [
        OnboardingStep::Welcome,
        OnboardingStep::TaskCreated,
        OnboardingStep::RunStarted,
        OnboardingStep::Completed,
        OnboardingStep::Dismissed,
    ];

    /// Initial step derivation from the launch props.
    pub fn initial(state: &OrchestratorState, task_count: usize) -> OnboardingStep {
        if state.onboarding_completed == Some(true) {
            return OnboardingStep::Dismissed;
        }
        if state.stats.total_tasks_completed > 0 {
            return OnboardingStep::Dismissed;
        }
        if !state.running.is_empty() {
            return OnboardingStep::RunStarted;
        }
        if task_count > 0 {
            return OnboardingStep::TaskCreated;
        }
        OnboardingStep::Welcome
    }

    /// Event-driven, one-way transitions; `dismissed` is never re-entered.
    pub fn next_for(self, event: &OrchestratorEvent) -> Option<OnboardingStep> {
        match (self, event) {
            (OnboardingStep::Welcome, OrchestratorEvent::TaskCreated { .. }) => {
                Some(OnboardingStep::TaskCreated)
            }
            (OnboardingStep::TaskCreated, OrchestratorEvent::AgentStarted { .. }) => {
                Some(OnboardingStep::RunStarted)
            }
            (
                OnboardingStep::RunStarted,
                OrchestratorEvent::TaskStatusChanged {
                    to: TaskStatus::Done,
                    ..
                },
            ) => Some(OnboardingStep::Completed),
            _ => None,
        }
    }
}

/// A `{key} {label}` hotkey hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
}

/// Per-tab empty-state copy (`OnboardingConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingConfig {
    pub title: &'static str,
    pub description: [&'static str; 3],
    pub hints: &'static [Hint],
    pub nudge: &'static str,
}

const GOALS: OnboardingConfig = OnboardingConfig {
    title: "Goals",
    description: [
        "Define what your team should achieve.",
        "The orchestrator breaks goals into tasks",
        "and assigns them to agents automatically.",
    ],
    hints: &[
        Hint {
            key: "N",
            label: "new goal",
        },
        Hint {
            key: "/",
            label: "commands",
        },
    ],
    nudge: "Add more goals to keep your team focused.",
};

// There is no `W start orchestrator` hint: no handler ever existed and watch
// mode is reachable through `/watch`.
const TASKS: OnboardingConfig = OnboardingConfig {
    title: "Tasks",
    description: [
        "Units of work dispatched to agents.",
        "Create them manually or let goals",
        "generate them automatically.",
    ],
    hints: &[Hint {
        key: "N",
        label: "new task",
    }],
    nudge: "Add more tasks to keep agents busy.",
};

const AGENTS: OnboardingConfig = OnboardingConfig {
    title: "Agents",
    description: [
        "AI workers that execute your tasks.",
        "Adapters: claude, opencode, codex, cursor,",
        "pi, grok, antigravity, shell.",
    ],
    hints: &[Hint {
        key: "N",
        label: "new agent",
    }],
    nudge: "Add more agents to increase parallelism.",
};

/// The config for a view, or `None` for the logs view (which has its own empty
/// state).
pub fn config_for(view: ViewId) -> Option<&'static OnboardingConfig> {
    match view {
        ViewId::Goals => Some(&GOALS),
        ViewId::Tasks => Some(&TASKS),
        ViewId::Agents => Some(&AGENTS),
        ViewId::Logs => None,
    }
}

/// What `OnboardingBox` renders for a given item count: nothing at 3+, a
/// one-line nudge at 1–2, the full box at 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingBoxMode {
    Hidden,
    Nudge,
    Full,
}

pub fn box_mode(count: usize) -> OnboardingBoxMode {
    match count {
        0 => OnboardingBoxMode::Full,
        1..=2 => OnboardingBoxMode::Nudge,
        _ => OnboardingBoxMode::Hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::{RunningEntry, SUPPORTED_ADAPTERS, default_state};

    fn state() -> OrchestratorState {
        default_state()
    }

    #[test]
    fn the_step_enum_is_five_values_in_order() {
        assert_eq!(OnboardingStep::ALL.len(), 5);
        assert_eq!(OnboardingStep::ALL[0], OnboardingStep::Welcome);
        assert_eq!(OnboardingStep::ALL[4], OnboardingStep::Dismissed);
    }

    #[test]
    fn initial_welcome_with_no_tasks() {
        assert_eq!(
            OnboardingStep::initial(&state(), 0),
            OnboardingStep::Welcome
        );
    }

    #[test]
    fn initial_task_created_with_tasks() {
        assert_eq!(
            OnboardingStep::initial(&state(), 3),
            OnboardingStep::TaskCreated
        );
    }

    #[test]
    fn initial_run_started_with_a_running_map() {
        let mut s = state();
        s.running.insert(
            "run_1".into(),
            RunningEntry {
                remote: None,
                run_id: "run_1".into(),
                agent_id: "agt_1".into(),
                task_id: "tsk_1".into(),
                pid: 1,
                started_at: "x".into(),
                last_event_at: "x".into(),
            },
        );
        assert_eq!(OnboardingStep::initial(&s, 1), OnboardingStep::RunStarted);
    }

    #[test]
    fn initial_dismissed_when_onboarding_completed() {
        let mut s = state();
        s.onboarding_completed = Some(true);
        assert_eq!(OnboardingStep::initial(&s, 0), OnboardingStep::Dismissed);
    }

    #[test]
    fn initial_dismissed_when_a_task_has_already_completed() {
        let mut s = state();
        s.stats.total_tasks_completed = 1;
        assert_eq!(OnboardingStep::initial(&s, 0), OnboardingStep::Dismissed);
    }

    #[test]
    fn the_three_event_transitions_fire_in_order_only() {
        let created = OrchestratorEvent::TaskCreated {
            task: Box::new(crate::backend::make_task(
                "tsk_1",
                "t",
                orch_domain::TaskStatus::Todo,
            )),
        };
        let started = OrchestratorEvent::AgentStarted {
            agent_id: "a".into(),
            task_id: "t".into(),
            run_id: "r".into(),
        };
        let done = OrchestratorEvent::TaskStatusChanged {
            task_id: "t".into(),
            from: orch_domain::TaskStatus::InProgress,
            to: orch_domain::TaskStatus::Done,
        };

        assert_eq!(
            OnboardingStep::Welcome.next_for(&created),
            Some(OnboardingStep::TaskCreated)
        );
        assert_eq!(OnboardingStep::Welcome.next_for(&started), None);
        assert_eq!(
            OnboardingStep::TaskCreated.next_for(&started),
            Some(OnboardingStep::RunStarted)
        );
        assert_eq!(
            OnboardingStep::RunStarted.next_for(&done),
            Some(OnboardingStep::Completed)
        );
        // A non-terminal transition does not advance.
        let review = OrchestratorEvent::TaskStatusChanged {
            task_id: "t".into(),
            from: orch_domain::TaskStatus::InProgress,
            to: orch_domain::TaskStatus::Review,
        };
        assert_eq!(OnboardingStep::RunStarted.next_for(&review), None);
    }

    #[test]
    fn dismissed_is_never_re_entered() {
        let created = OrchestratorEvent::TaskCreated {
            task: Box::new(crate::backend::make_task(
                "tsk_1",
                "t",
                orch_domain::TaskStatus::Todo,
            )),
        };
        assert_eq!(OnboardingStep::Dismissed.next_for(&created), None);
    }

    #[test]
    fn the_agents_description_names_every_supported_adapter() {
        let text = AGENTS.description.join(" ");
        for adapter in SUPPORTED_ADAPTERS {
            assert!(text.contains(adapter.as_str()), "{}", adapter.as_str());
        }
        assert_eq!(AGENTS.description.len(), 3);
    }

    #[test]
    fn the_dropped_w_hint_is_not_advertised() {
        for config in [&GOALS, &TASKS, &AGENTS] {
            assert!(
                !config.hints.iter().any(|h| h.key == "W"),
                "{} still advertises W",
                config.title
            );
        }
    }

    #[test]
    fn box_mode_thresholds() {
        assert_eq!(box_mode(0), OnboardingBoxMode::Full);
        assert_eq!(box_mode(1), OnboardingBoxMode::Nudge);
        assert_eq!(box_mode(2), OnboardingBoxMode::Nudge);
        assert_eq!(box_mode(3), OnboardingBoxMode::Hidden);
    }

    #[test]
    fn logs_has_no_onboarding_config() {
        assert!(config_for(ViewId::Logs).is_none());
        assert_eq!(config_for(ViewId::Tasks).unwrap().title, "Tasks");
    }
}
