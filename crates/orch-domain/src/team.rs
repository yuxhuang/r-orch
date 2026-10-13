//! Team domain model — a group of agents with a lead (`src/domain/team.ts`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Active,
    Paused,
    Disbanded,
}

impl TeamStatus {
    pub const ALL: [TeamStatus; 3] = [
        TeamStatus::Active,
        TeamStatus::Paused,
        TeamStatus::Disbanded,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            TeamStatus::Active => "active",
            TeamStatus::Paused => "paused",
            TeamStatus::Disbanded => "disbanded",
        }
    }
}

impl std::fmt::Display for TeamStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TeamStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        TeamStatus::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMemberRole {
    Lead,
    Member,
}

impl TeamMemberRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            TeamMemberRole::Lead => "lead",
            TeamMemberRole::Member => "member",
        }
    }
}

impl std::fmt::Display for TeamMemberRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMember {
    pub agent_id: String,
    pub role: TeamMemberRole,
    pub joined_at: String,
}

/// Field order mirrors the TS `Team` interface (PARITY.md §6.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: TeamStatus,
    pub members: Vec<TeamMember>,
    pub task_pool: Vec<String>,
    pub lead_agent_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub config: TeamConfig,
}

/// `auto_claim` and `max_concurrent_tasks` are stored and surfaced by the CLI
/// but never read by the orchestrator (PARITY.md §7.18). The data model is
/// preserved; the absent behaviour is not invented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<u32>,
    pub auto_claim: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ttl_ms: Option<u64>,
}

/// `{ auto_claim: true, message_ttl_ms: 86400000 }` — 24 hours.
pub const DEFAULT_TEAM_CONFIG: TeamConfig = TeamConfig {
    max_concurrent_tasks: None,
    auto_claim: true,
    message_ttl_ms: Some(24 * 60 * 60 * 1000),
};

impl Default for TeamConfig {
    fn default() -> Self {
        DEFAULT_TEAM_CONFIG
    }
}

/// `config` here is TS `Partial<TeamConfig>`: every field independently
/// optional, overlaid on [`DEFAULT_TEAM_CONFIG`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialTeamConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_claim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_ttl_ms: Option<u64>,
}

impl PartialTeamConfig {
    /// Overlay these fields on [`DEFAULT_TEAM_CONFIG`], mirroring the TS
    /// `{ ...DEFAULT_TEAM_CONFIG, ...input.config }` spread.
    pub fn resolve(&self) -> TeamConfig {
        TeamConfig {
            max_concurrent_tasks: self
                .max_concurrent_tasks
                .or(DEFAULT_TEAM_CONFIG.max_concurrent_tasks),
            auto_claim: self.auto_claim.unwrap_or(DEFAULT_TEAM_CONFIG.auto_claim),
            message_ttl_ms: self.message_ttl_ms.or(DEFAULT_TEAM_CONFIG.message_ttl_ms),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTeamInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub lead_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_agent_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PartialTeamConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_the_contract() {
        const { assert!(DEFAULT_TEAM_CONFIG.auto_claim) };
        assert_eq!(DEFAULT_TEAM_CONFIG.message_ttl_ms, Some(86_400_000));
        assert_eq!(
            serde_json::to_string(&DEFAULT_TEAM_CONFIG).unwrap(),
            r#"{"auto_claim":true,"message_ttl_ms":86400000}"#
        );
    }

    #[test]
    fn partial_config_overlays_defaults() {
        let partial = PartialTeamConfig {
            auto_claim: Some(false),
            ..Default::default()
        };
        let resolved = partial.resolve();
        assert!(!resolved.auto_claim);
        assert_eq!(resolved.message_ttl_ms, Some(86_400_000));
    }

    #[test]
    fn reference_shape() {
        // PARITY.md §4.2.
        let team = Team {
            id: "team_tmmAHja".into(),
            name: "squad".into(),
            description: None,
            status: TeamStatus::Active,
            members: vec![TeamMember {
                agent_id: "agt_creator".into(),
                role: TeamMemberRole::Lead,
                joined_at: "2026-10-13T16:42:11.108Z".into(),
            }],
            task_pool: vec![],
            lead_agent_id: "agt_creator".into(),
            created_at: "2026-10-13T16:42:11.108Z".into(),
            updated_at: "2026-10-13T16:42:11.108Z".into(),
            config: DEFAULT_TEAM_CONFIG,
        };
        let value = serde_json::to_value(&team).unwrap();
        assert!(value.get("description").is_none());
        assert_eq!(value["members"][0]["role"], "lead");
        assert_eq!(value["config"]["message_ttl_ms"], 86_400_000);
    }
}
