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
        agents: Vec<Agent>,
    },
    Welcome {
        proto: u32,
        instance_id: String,
        name: String,
        os: String,
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
