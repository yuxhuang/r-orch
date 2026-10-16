//! Peer registry and runtime directory records. Remote agents never enter AgentStore.

use crate::Agent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub alias: String,
    pub online: bool,
}

/// The prefix a worker puts on the `reason` of a mirror that its own owner
/// has to approve. Part of the wire contract: the controller recognises it to
/// name the command, and a peer that omits it only loses the hint.
pub const AWAITING_APPROVAL_REASON: &str = "awaiting approval:";

/// How far a remote execution has actually got, as its worker last said.
///
/// `TaskStatus` cannot carry this: a dispatched task is `in_progress` from the
/// moment the controller reserves it, which for a peer means the request has
/// not even been written to the socket yet. Views overlay the phase so they
/// never claim a peer is running work it has merely been sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemotePhase {
    /// Enqueued here; the worker has not acknowledged the submit.
    #[default]
    Dispatched,
    /// The worker holds a mirror that has not started — possibly waiting for
    /// its own owner's `orch task approve`.
    Queued,
    /// The worker reports its agent is executing.
    Running,
    /// The link dropped after the submit was written, or a restart rehydrated
    /// the entry. Only a sync can say what the worker still has.
    Unknown,
    /// An outcome arrived; the entry is on its way out of `state.running`.
    Terminal,
}

impl RemotePhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            RemotePhase::Dispatched => "dispatched",
            RemotePhase::Queued => "queued",
            RemotePhase::Running => "running",
            RemotePhase::Unknown => "unknown",
            RemotePhase::Terminal => "terminal",
        }
    }

    fn rank(self) -> u8 {
        match self {
            RemotePhase::Dispatched => 0,
            RemotePhase::Queued => 1,
            RemotePhase::Running => 2,
            RemotePhase::Unknown => 2,
            RemotePhase::Terminal => 3,
        }
    }

    /// Whether `next` may replace `self`.
    ///
    /// Progress is one-way, so a late frame from a dead session cannot pull a
    /// running execution back to `queued`. `Unknown` is the exception in both
    /// directions: a lost link may interrupt any phase, and the sync that
    /// follows is the only thing that can resolve it.
    pub fn advances_to(self, next: RemotePhase) -> bool {
        if self == next {
            return false;
        }
        match (self, next) {
            (RemotePhase::Terminal, _) => false,
            (RemotePhase::Unknown, _) | (_, RemotePhase::Unknown) => true,
            (from, to) => to.rank() > from.rank(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRunRef {
    pub alias: String,
    pub exec_key: String,
    /// Defaulted so a `state.json` written before the phase model still
    /// loads; startup recovery re-derives it from the worker.
    #[serde(default)]
    pub phase: RemotePhase,
    /// The mirror's task id on the worker, once it has reported one. The only
    /// id that can name a command for the worker's owner to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_task_id: Option<String>,
    /// The worker's own explanation for the phase, verbatim. Display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RemoteRunRef {
    pub fn new(alias: impl Into<String>, exec_key: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            exec_key: exec_key.into(),
            phase: RemotePhase::Dispatched,
            worker_task_id: None,
            reason: None,
        }
    }

    /// What a task view shows instead of the local status. `None` once the
    /// outcome has arrived, when the task's own status is the truth again.
    pub fn status_label(&self) -> Option<String> {
        let alias = &self.alias;
        Some(match self.phase {
            RemotePhase::Dispatched => format!("dispatched → {alias}"),
            RemotePhase::Queued => format!("queued @{alias}"),
            RemotePhase::Running => format!("RUN @{alias}"),
            RemotePhase::Unknown => format!("RUN? ({alias} offline)"),
            RemotePhase::Terminal => return None,
        })
    }

    /// The one line that tells the operator what to do about this phase: the
    /// worker's reason, with an approval turned into the command to run.
    pub fn hint(&self) -> Option<String> {
        let reason = self.reason.as_deref()?;
        match self.worker_task_id.as_deref() {
            Some(task_id) if reason.starts_with(AWAITING_APPROVAL_REASON) => Some(format!(
                "awaiting approval: orch task approve {task_id} on {}",
                self.alias
            )),
            _ => Some(reason.to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub alias: String,
    pub addr: String,
    pub added_at: String,
}

/// What an outbound link is doing, for `orch remote status`. A snapshot alone
/// cannot say this: `offline` covers a worker that is down, one that is
/// serving another controller, and one this instance is about to redial.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// Dialling, or handshaking. Also what a record written before this field
    /// reads as.
    #[default]
    Connecting,
    /// Handshaken, and inside its heartbeat.
    Online,
    /// Waiting out the redial delay in `next_retry_at`.
    Backoff,
    /// The worker is serving a different controller.
    Busy,
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
    /// The peer's release, as its `Welcome` reported it. `None` from a peer
    /// older than the field, or before the first handshake.
    #[serde(default)]
    pub peer_version: Option<String>,
    #[serde(default)]
    pub link_state: LinkState,
    /// When the next dial is due, while in `backoff` or `busy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<String>,
    /// Consecutive dials since the last session that proved the peer healthy.
    /// A peer that keeps closing sessions is only visible from this.
    #[serde(default)]
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundStatus {
    pub controller_name: String,
    pub instance_id: String,
    pub connected_since: String,
    #[serde(default)]
    pub agents: Vec<Agent>,
    /// The controller's release, as its `Hello` reported it.
    #[serde(default)]
    pub controller_version: Option<String>,
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
        entry.remote = Some(RemoteRunRef::new("w1", "run1"));
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

    #[test]
    fn a_state_file_without_a_phase_loads_and_keeps_its_remote_reference() {
        let entry: RunningEntry = serde_json::from_value(serde_json::json!({
            "run_id": "run1", "agent_id": "agt@w1", "task_id": "task1",
            "pid": 0, "started_at": "now", "last_event_at": "now",
            "remote": {"alias": "w1", "exec_key": "run1"}
        }))
        .unwrap();
        let remote = entry.remote.unwrap();
        assert_eq!(remote.phase, RemotePhase::Dispatched);
        assert_eq!(remote, RemoteRunRef::new("w1", "run1"));
        // The two optional fields stay off disk until a worker reports them.
        let written = serde_json::to_value(&remote).unwrap();
        assert_eq!(written["phase"], "dispatched");
        assert!(written.get("worker_task_id").is_none() && written.get("reason").is_none());
    }

    #[test]
    fn phase_labels_never_claim_a_run_the_worker_has_not_started() {
        let mut remote = RemoteRunRef::new("2com", "run1");
        assert_eq!(remote.status_label().unwrap(), "dispatched → 2com");
        remote.phase = RemotePhase::Queued;
        assert_eq!(remote.status_label().unwrap(), "queued @2com");
        remote.phase = RemotePhase::Running;
        assert_eq!(remote.status_label().unwrap(), "RUN @2com");
        remote.phase = RemotePhase::Unknown;
        assert_eq!(remote.status_label().unwrap(), "RUN? (2com offline)");
        remote.phase = RemotePhase::Terminal;
        assert!(remote.status_label().is_none());
    }

    #[test]
    fn an_approval_reason_becomes_the_command_to_run_on_the_worker() {
        let mut remote = RemoteRunRef::new("2com", "run1");
        assert!(remote.hint().is_none());
        remote.reason = Some(format!(
            "{AWAITING_APPROVAL_REASON} run `orch task approve tsk_x`"
        ));
        // Without the worker's task id there is no command to name.
        assert_eq!(remote.hint().unwrap(), remote.reason.clone().unwrap());
        remote.worker_task_id = Some("tsk_mirror".into());
        assert_eq!(
            remote.hint().unwrap(),
            "awaiting approval: orch task approve tsk_mirror on 2com"
        );
        remote.reason = Some("worker storage error: corrupt record".into());
        assert_eq!(
            remote.hint().unwrap(),
            "worker storage error: corrupt record"
        );
    }

    #[test]
    fn progress_is_one_way_except_through_unknown() {
        use RemotePhase::*;
        assert!(Dispatched.advances_to(Queued));
        assert!(Queued.advances_to(Running));
        assert!(Running.advances_to(Terminal));
        assert!(
            !Running.advances_to(Queued),
            "a late frame must not undo a run"
        );
        assert!(!Queued.advances_to(Dispatched));
        assert!(!Running.advances_to(Running));
        assert!(!Terminal.advances_to(Unknown), "an outcome is final");
        // A lost link interrupts any phase, and the sync after it resolves.
        assert!(Dispatched.advances_to(Unknown) && Running.advances_to(Unknown));
        assert!(Unknown.advances_to(Queued) && Unknown.advances_to(Running));
    }
}
