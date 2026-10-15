//! Peer registry and runtime directory records. Remote agents never enter AgentStore.

use crate::Agent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub alias: String,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRunRef {
    pub alias: String,
    pub exec_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub alias: String,
    pub addr: String,
    pub added_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteStatus {
    pub alias: String,
    pub addr: String,
    pub online: bool,
    pub last_seen: Option<String>,
    pub last_error: Option<String>,
    pub peer_name: Option<String>,
    pub instance_id: Option<String>,
    pub os: Option<String>,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundStatus {
    pub controller_name: String,
    pub instance_id: String,
    pub connected_since: String,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunningEntry, TaskProof};

    #[test]
    fn legacy_runs_and_proofs_default_to_local_and_remote_metadata_round_trips() {
        let mut entry: RunningEntry = serde_json::from_value(serde_json::json!({
            "run_id": "run1", "agent_id": "agent1", "task_id": "task1",
            "pid": 12, "started_at": "now", "last_event_at": "now"
        }))
        .unwrap();
        assert!(entry.remote.is_none());
        assert!(
            serde_json::to_value(&entry)
                .unwrap()
                .get("remote")
                .is_none()
        );
        entry.remote = Some(RemoteRunRef {
            alias: "w1".into(),
            exec_key: "run1".into(),
        });
        let read: RunningEntry =
            serde_json::from_value(serde_json::to_value(&entry).unwrap()).unwrap();
        assert_eq!(read, entry);
        let mut proof: TaskProof =
            serde_json::from_value(serde_json::json!({"files_changed": []})).unwrap();
        assert!(proof.remote.is_none());
        assert!(
            serde_json::to_value(&proof)
                .unwrap()
                .get("remote")
                .is_none()
        );
        proof.remote = Some("w1".into());
        let read: TaskProof =
            serde_json::from_value(serde_json::to_value(&proof).unwrap()).unwrap();
        assert_eq!(read, proof);
    }
}
