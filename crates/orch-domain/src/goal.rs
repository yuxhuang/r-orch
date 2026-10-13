//! Goal domain model — a persistent objective that drives autonomous agent
//! work (`src/domain/goal.ts`).
//!
//! State machine: `active → achieved | abandoned | paused`,
//! `paused → active | achieved | abandoned` (PARITY.md §7.2).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Achieved,
    Abandoned,
}

/// Declaration order, which is also the canonical sort order.
pub const GOAL_STATUSES: [GoalStatus; 4] = [
    GoalStatus::Active,
    GoalStatus::Paused,
    GoalStatus::Achieved,
    GoalStatus::Abandoned,
];

impl GoalStatus {
    pub const ALL: [GoalStatus; 4] = GOAL_STATUSES;

    pub fn as_str(&self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Achieved => "achieved",
            GoalStatus::Abandoned => "abandoned",
        }
    }

    /// Allowed outgoing transitions (PARITY.md §7.2).
    pub fn allowed_transitions(&self) -> &'static [GoalStatus] {
        match self {
            GoalStatus::Active => &[
                GoalStatus::Paused,
                GoalStatus::Achieved,
                GoalStatus::Abandoned,
            ],
            GoalStatus::Paused => &[
                GoalStatus::Active,
                GoalStatus::Achieved,
                GoalStatus::Abandoned,
            ],
            GoalStatus::Achieved | GoalStatus::Abandoned => &[],
        }
    }

    pub fn can_transition_to(&self, to: GoalStatus) -> bool {
        self.allowed_transitions().contains(&to)
    }
}

impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for GoalStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GOAL_STATUSES
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// Terminal goal statuses — no further transitions possible.
pub fn is_goal_terminal(status: GoalStatus) -> bool {
    matches!(status, GoalStatus::Achieved | GoalStatus::Abandoned)
}

/// Canonical sort order: `active(0) < paused(1) < achieved(2) < abandoned(3)`.
pub fn goal_status_order(status: GoalStatus) -> u8 {
    match status {
        GoalStatus::Active => 0,
        GoalStatus::Paused => 1,
        GoalStatus::Achieved => 2,
        GoalStatus::Abandoned => 3,
    }
}

/// Field order mirrors the TS `Goal` interface (PARITY.md §6.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateGoalInput {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_statuses() {
        assert!(is_goal_terminal(GoalStatus::Achieved));
        assert!(is_goal_terminal(GoalStatus::Abandoned));
        assert!(!is_goal_terminal(GoalStatus::Active));
        assert!(!is_goal_terminal(GoalStatus::Paused));
    }

    #[test]
    fn sort_order_is_canonical() {
        let mut order: Vec<_> = GOAL_STATUSES.into_iter().collect();
        order.sort_by_key(|&s| goal_status_order(s));
        assert_eq!(order, GOAL_STATUSES.to_vec());
    }

    #[test]
    fn goal_transition_table_matches_the_contract() {
        use GoalStatus::*;
        let table: &[(GoalStatus, &[GoalStatus])] = &[
            (Active, &[Paused, Achieved, Abandoned]),
            (Paused, &[Active, Achieved, Abandoned]),
            (Achieved, &[]),
            (Abandoned, &[]),
        ];
        for (from, allowed) in table {
            for to in GOAL_STATUSES {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&to),
                    "{from} → {to}"
                );
            }
        }
    }

    #[test]
    fn reference_shape() {
        // PARITY.md §4.2.
        let goal = Goal {
            id: "goal_717sF1B".into(),
            title: "Ship v2".into(),
            description: "Get it out".into(),
            status: GoalStatus::Active,
            assignee: None,
            created_at: "2026-10-13T16:42:02.793Z".into(),
            updated_at: Some("2026-10-13T16:42:02.793Z".into()),
        };
        assert_eq!(
            serde_json::to_string_pretty(&goal).unwrap(),
            r#"{
  "id": "goal_717sF1B",
  "title": "Ship v2",
  "description": "Get it out",
  "status": "active",
  "created_at": "2026-10-13T16:42:02.793Z",
  "updated_at": "2026-10-13T16:42:02.793Z"
}"#
        );
    }

    #[test]
    fn status_strings_round_trip() {
        for s in GOAL_STATUSES {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
            assert_eq!(s.as_str().parse::<GoalStatus>(), Ok(s));
        }
    }
}
