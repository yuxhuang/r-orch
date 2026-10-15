//! Version-one wire contract. Snapshots carry only worker-local agent IDs.
use orch_core::{adapter::AgentEvent, remote::RemoteOutcome};
use orch_domain::{Agent, ReviewCriterion};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncItem {
    pub exec_key: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RemoteOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    Hello {
        proto: u32,
        instance_id: String,
        name: String,
        os: String,
        #[serde(with = "orch_infra::storage::remote::public_snapshot")]
        agents: Vec<Agent>,
    },
    Welcome {
        proto: u32,
        instance_id: String,
        name: String,
        os: String,
        #[serde(with = "orch_infra::storage::remote::public_snapshot")]
        agents: Vec<Agent>,
    },
    Busy {
        controller: String,
    },
    Error {
        msg: String,
    },
    Ping,
    Pong,
    Agents {
        #[serde(with = "orch_infra::storage::remote::public_snapshot")]
        agents: Vec<Agent>,
    },
    Sync {
        active: Vec<String>,
    },
    SyncState {
        items: Vec<SyncItem>,
    },
    Submit {
        exec_key: String,
        agent_id: String,
        from: String,
        prompt: String,
        title: String,
        priority: i64,
        review_criteria: Vec<ReviewCriterion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
    },
    Submitted {
        exec_key: String,
        worker_task_id: String,
    },
    Rejected {
        exec_key: String,
        reason: String,
    },
    Event {
        exec_key: String,
        seq: u64,
        event: AgentEvent,
    },
    Attach {
        exec_key: String,
        after_seq: u64,
    },
    Gap {
        exec_key: String,
    },
    State {
        exec_key: String,
        state: String,
    },
    Finished {
        exec_key: String,
        outcome: RemoteOutcome,
    },
    FinishedAck {
        exec_key: String,
    },
    Cancel {
        exec_key: String,
    },
    CancelAck {
        exec_key: String,
    },
    Msg {
        id: String,
        from: String,
        to: String,
        subject: String,
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    MsgAck {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::make_agent;
    use orch_domain::{AgentConfig, AgentLastError, AgentStatus, ApprovalPolicy};
    use serde_json::json;

    #[test]
    fn all_snapshot_frames_allowlist_disabled_agents_and_strip_legacy_private_fields() {
        let mut agent = make_agent();
        agent.status = AgentStatus::Disabled;
        agent.config.approval_policy = Some(ApprovalPolicy::Auto);
        agent.role = Some("public role".into());
        agent.current_task = Some("tsk_public".into());
        agent.capabilities = vec!["macos".into()];
        agent.config.command = Some("PRIVATE_COMMAND_CANARY".into());
        agent.config.system_prompt = Some("PRIVATE_PROMPT_CANARY".into());
        agent.config.env = Some([("TOKEN".into(), "PRIVATE_ENV_CANARY".into())].into());
        agent.last_error = Some(AgentLastError {
            message: "PRIVATE_ERROR_CANARY".into(),
            kind: "unknown".into(),
            timestamp: "now".into(),
        });
        for frame in [
            Frame::Hello {
                proto: PROTOCOL,
                instance_id: "id".into(),
                name: "controller".into(),
                os: "macos".into(),
                agents: vec![agent.clone()],
            },
            Frame::Welcome {
                proto: PROTOCOL,
                instance_id: "id".into(),
                name: "worker".into(),
                os: "macos".into(),
                agents: vec![agent.clone()],
            },
            Frame::Agents {
                agents: vec![agent.clone()],
            },
        ] {
            let public = serde_json::to_value(&frame).unwrap();
            assert_eq!(
                public["agents"],
                json!([{
                    "id": agent.id, "name": agent.name, "adapter": agent.adapter,
                    "role": "public role", "status": "disabled", "approval_policy": "auto", "current_task": "tsk_public",
                    "capabilities": ["macos"]
                }])
            );
            // Older/malicious peers may still send execution config; never
            // retain it in memory or forward it to another boundary.
            let mut legacy = public.clone();
            legacy["agents"] = json!([agent]);
            let received: Frame = serde_json::from_value(legacy).unwrap();
            assert_eq!(serde_json::to_value(&received).unwrap(), public);
            let agents = match received {
                Frame::Hello { agents, .. }
                | Frame::Welcome { agents, .. }
                | Frame::Agents { agents } => agents,
                _ => unreachable!(),
            };
            assert_eq!(
                agents[0].config,
                AgentConfig {
                    approval_policy: Some(ApprovalPolicy::Auto),
                    ..Default::default()
                }
            );
            assert!(agents[0].last_error.is_none());
            assert_eq!(agents[0].autonomous, Some(false));
        }
    }
}
