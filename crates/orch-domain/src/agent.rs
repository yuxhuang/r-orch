//! Agent domain model — a configured AI tool (`src/domain/agent.ts`).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::task::WorkspaceMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Error,
    Disabled,
    Offline,
}

impl AgentStatus {
    pub const ALL: [AgentStatus; 5] = [
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Error,
        AgentStatus::Disabled,
        AgentStatus::Offline,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
            AgentStatus::Error => "error",
            AgentStatus::Disabled => "disabled",
            AgentStatus::Offline => "offline",
        }
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentStatus::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Suggest,
    Auto,
    Manual,
}

impl ApprovalPolicy {
    pub const ALL: [ApprovalPolicy; 3] = [
        ApprovalPolicy::Suggest,
        ApprovalPolicy::Auto,
        ApprovalPolicy::Manual,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalPolicy::Suggest => "suggest",
            ApprovalPolicy::Auto => "auto",
            ApprovalPolicy::Manual => "manual",
        }
    }
}

impl std::fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ApprovalPolicy {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ApprovalPolicy::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub const ALL: [ReasoningEffort; 3] = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ReasoningEffort::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
    /// Insertion-ordered to mirror the JS object this came from; the spawn
    /// environment is built from it in order (PARITY.md §8.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<WorkspaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStats {
    pub tasks_completed: u64,
    pub tasks_failed: u64,
    pub total_runs: u64,
    pub total_runtime_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLastError {
    /// Truncated to 500 characters by the writer (PARITY.md §6.8).
    pub message: String,
    /// An [`crate::error::AdapterErrorKind`] string value.
    pub kind: String,
    pub timestamp: String,
}

/// Field order mirrors the TS `Agent` interface (PARITY.md §6.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub config: AgentConfig,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<bool>,
    pub stats: AgentStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<AgentLastError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<crate::remote::RemoteRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgentInput {
    pub name: String,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<WorkspaceMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_agent_matches_the_reference_shape() {
        // PARITY.md §4.2 `Agent` (shell).
        let agent = Agent {
            id: "agt_veWG_kK".into(),
            name: "shelly".into(),
            adapter: "shell".into(),
            role: None,
            config: AgentConfig {
                command: Some("echo hello".into()),
                approval_policy: Some(ApprovalPolicy::Auto),
                max_turns: Some(50),
                timeout_ms: Some(3_600_000),
                stall_timeout_ms: Some(600_000),
                ..AgentConfig::default()
            },
            status: AgentStatus::Idle,
            current_task: None,
            autonomous: None,
            stats: AgentStats::default(),
            last_error: None,
            capabilities: Vec::new(),
            remote: None,
        };

        let json = serde_json::to_string_pretty(&agent).unwrap();
        assert_eq!(
            json,
            r#"{
  "id": "agt_veWG_kK",
  "name": "shelly",
  "adapter": "shell",
  "config": {
    "command": "echo hello",
    "approval_policy": "auto",
    "max_turns": 50,
    "timeout_ms": 3600000,
    "stall_timeout_ms": 600000
  },
  "status": "idle",
  "stats": {
    "tasks_completed": 0,
    "tasks_failed": 0,
    "total_runs": 0,
    "total_runtime_ms": 0
  }
}"#
        );
    }

    #[test]
    fn enum_strings_round_trip() {
        for p in ApprovalPolicy::ALL {
            assert_eq!(
                serde_json::to_string(&p).unwrap(),
                format!("\"{}\"", p.as_str())
            );
            assert_eq!(p.as_str().parse::<ApprovalPolicy>(), Ok(p));
        }
        for s in AgentStatus::ALL {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
            assert_eq!(s.as_str().parse::<AgentStatus>(), Ok(s));
        }
        for e in ReasoningEffort::ALL {
            assert_eq!(
                serde_json::to_string(&e).unwrap(),
                format!("\"{}\"", e.as_str())
            );
            assert_eq!(e.as_str().parse::<ReasoningEffort>(), Ok(e));
        }
    }

    #[test]
    fn empty_agent_config_serializes_as_an_empty_map() {
        assert_eq!(
            serde_json::to_string(&AgentConfig::default()).unwrap(),
            "{}"
        );
    }
}
