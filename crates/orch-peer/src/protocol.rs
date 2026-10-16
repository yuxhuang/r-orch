//! Version-one wire contract. Snapshots carry only worker-local agent IDs.
//!
//! # Compatibility rule
//!
//! Within one `PROTOCOL` every peer must tolerate **every** frame variant and
//! **every** omitted field: an old peer meets frames it has no use for, and a
//! new peer meets frames that predate half its fields. So a change that stays
//! inside this `PROTOCOL` adds *fields only* — `#[serde(default)]` to read an
//! old peer's frame, `skip_serializing_if` so an old peer reads ours — and a
//! new frame variant, a removed field or a changed meaning requires a bump.
//! Both directions are covered by the compatibility tests below.
use orch_core::{adapter::AgentEvent, remote::RemoteOutcome};
use orch_domain::{Agent, ReviewCriterion};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 1;

/// This peer's release, for `Hello`/`Welcome`. `None` from a peer older than
/// the field.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncItem {
    pub exec_key: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RemoteOutcome>,
    /// Why the key is in this state, when the state alone is not actionable:
    /// a mirror the worker cannot read, or one waiting for its owner's
    /// `orch task approve`. Operator-facing text, never parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The mirror's task id on the worker, so the controller can name the
    /// command its owner has to run there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_task_id: Option<String>,
}

impl SyncItem {
    pub fn new(exec_key: &str, state: &str) -> Self {
        Self {
            exec_key: exec_key.into(),
            state: state.into(),
            outcome: None,
            reason: None,
            worker_task_id: None,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
    Welcome {
        proto: u32,
        instance_id: String,
        name: String,
        os: String,
        #[serde(with = "orch_infra::storage::remote::public_snapshot")]
        agents: Vec<Agent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_task_id: Option<String>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::make_agent;
    use orch_domain::{AgentConfig, AgentLastError, AgentStatus, ApprovalPolicy};
    use serde_json::json;

    #[test]
    fn message_ack_is_compatible_with_legacy_peers() {
        let wire = r#"{"t":"msg_ack","id":"msg_one"}"#;
        let frame: Frame = serde_json::from_str(wire).unwrap();
        assert!(matches!(&frame, Frame::MsgAck { reason: None, .. }));
        assert_eq!(serde_json::to_string(&frame).unwrap(), wire);
        // The old shape's real serde policy ignores additive fields.
        let LegacyFrame::MsgAck { id } = legacy(&Frame::MsgAck {
            id: "msg_one".into(),
            reason: Some("unknown recipient".into()),
        }) else {
            panic!("message ack expected")
        };
        assert_eq!(id, "msg_one");
    }

    /// The 0.5.0 shapes of every frame this release added a field to, with
    /// that release's serde policy (additive fields ignored, nothing else
    /// optional). Reading a current frame with it is the "old peer sees a new
    /// peer" half of the compatibility rule.
    #[derive(Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    enum LegacyFrame {
        Hello {
            proto: u32,
            instance_id: String,
            name: String,
            os: String,
        },
        Welcome {
            proto: u32,
            instance_id: String,
            name: String,
            os: String,
        },
        SyncState {
            items: Vec<LegacySyncItem>,
        },
        State {
            exec_key: String,
            state: String,
        },
        MsgAck {
            id: String,
        },
    }

    #[derive(Deserialize)]
    struct LegacySyncItem {
        exec_key: String,
        state: String,
        #[serde(default)]
        outcome: Option<RemoteOutcome>,
    }

    fn legacy(frame: &Frame) -> LegacyFrame {
        serde_json::from_str(&serde_json::to_string(frame).unwrap()).unwrap()
    }

    #[test]
    fn additive_fields_are_readable_by_a_legacy_peer() {
        let LegacyFrame::Hello {
            proto,
            instance_id,
            name,
            os,
        } = legacy(&Frame::Hello {
            proto: PROTOCOL,
            instance_id: "controller-id".into(),
            name: "controller".into(),
            os: "macos".into(),
            agents: vec![],
            version: Some(version()),
        })
        else {
            panic!("hello expected")
        };
        assert_eq!(
            (proto, instance_id.as_str(), name.as_str(), os.as_str()),
            (PROTOCOL, "controller-id", "controller", "macos")
        );
        let LegacyFrame::Welcome {
            proto,
            instance_id,
            name,
            os,
        } = legacy(&Frame::Welcome {
            proto: PROTOCOL,
            instance_id: "worker-id".into(),
            name: "worker".into(),
            os: "linux".into(),
            agents: vec![],
            version: Some(version()),
        })
        else {
            panic!("welcome expected")
        };
        assert_eq!(
            (proto, instance_id.as_str(), name.as_str(), os.as_str()),
            (PROTOCOL, "worker-id", "worker", "linux")
        );
        let LegacyFrame::SyncState { items } = legacy(&Frame::SyncState {
            items: vec![
                SyncItem::new("key", "unknown").with_reason("worker storage error: data"),
                SyncItem {
                    worker_task_id: Some("tsk_mirror".into()),
                    ..SyncItem::new("other", "queued")
                },
            ],
        }) else {
            panic!("sync state expected")
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            (items[0].exec_key.as_str(), items[0].state.as_str()),
            ("key", "unknown")
        );
        assert!(items[0].outcome.is_none() && items[1].outcome.is_none());
        let LegacyFrame::State { exec_key, state } = legacy(&Frame::State {
            exec_key: "key".into(),
            state: "queued".into(),
            reason: Some("awaiting approval".into()),
            worker_task_id: Some("tsk_mirror".into()),
        }) else {
            panic!("state expected")
        };
        assert_eq!((exec_key.as_str(), state.as_str()), ("key", "queued"));
    }

    #[test]
    fn legacy_frames_without_the_additive_fields_are_readable_here() {
        for json in [
            r#"{"t":"hello","proto":1,"instance_id":"id","name":"controller","os":"macos","agents":[]}"#,
            r#"{"t":"welcome","proto":1,"instance_id":"worker","name":"worker","os":"linux","agents":[]}"#,
        ] {
            match serde_json::from_str::<Frame>(json).unwrap() {
                Frame::Hello { version, .. } | Frame::Welcome { version, .. } => {
                    assert!(version.is_none())
                }
                other => panic!("handshake expected, got {other:?}"),
            }
        }
        let Frame::SyncState { items } = serde_json::from_str::<Frame>(
            r#"{"t":"sync_state","items":[{"exec_key":"key","state":"queued"}]}"#,
        )
        .unwrap() else {
            panic!("sync state expected")
        };
        assert_eq!(items[0].state, "queued");
        assert!(items[0].reason.is_none() && items[0].worker_task_id.is_none());
        let Frame::State {
            state,
            reason,
            worker_task_id,
            ..
        } = serde_json::from_str::<Frame>(r#"{"t":"state","exec_key":"key","state":"running"}"#)
            .unwrap()
        else {
            panic!("state expected")
        };
        assert_eq!(state, "running");
        assert!(reason.is_none() && worker_task_id.is_none());
        // An omitted additive field is also omitted on the way back out, so a
        // relayed frame stays byte-identical for the older peer.
        assert_eq!(
            serde_json::to_string(&Frame::State {
                exec_key: "key".into(),
                state: "running".into(),
                reason: None,
                worker_task_id: None,
            })
            .unwrap(),
            r#"{"t":"state","exec_key":"key","state":"running"}"#
        );
    }

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
                version: Some(version()),
            },
            Frame::Welcome {
                proto: PROTOCOL,
                instance_id: "id".into(),
                name: "worker".into(),
                os: "macos".into(),
                agents: vec![agent.clone()],
                version: Some(version()),
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
