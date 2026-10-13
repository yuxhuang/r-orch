//! Message domain model — inter-agent communication (`src/domain/message.ts`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageChannel {
    #[default]
    Direct,
    Broadcast,
    Lead,
}

impl MessageChannel {
    pub const ALL: [MessageChannel; 3] = [
        MessageChannel::Direct,
        MessageChannel::Broadcast,
        MessageChannel::Lead,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageChannel::Direct => "direct",
            MessageChannel::Broadcast => "broadcast",
            MessageChannel::Lead => "lead",
        }
    }
}

impl std::fmt::Display for MessageChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MessageChannel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MessageChannel::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Delivered,
    Expired,
}

impl MessageStatus {
    pub const ALL: [MessageStatus; 3] = [
        MessageStatus::Pending,
        MessageStatus::Delivered,
        MessageStatus::Expired,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            MessageStatus::Pending => "pending",
            MessageStatus::Delivered => "delivered",
            MessageStatus::Expired => "expired",
        }
    }
}

impl std::fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MessageStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        MessageStatus::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or(())
    }
}

/// Field order mirrors the TS `Message` interface.
///
/// `to_agent_id` is the **one** field in the whole domain that must emit
/// `null` rather than being omitted: its TS type is `string | null`, not
/// `string?` (PARITY.md §6.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub channel: MessageChannel,
    pub from_agent_id: String,
    pub to_agent_id: Option<String>,
    pub subject: String,
    pub body: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub status: MessageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMessageInput {
    pub channel: MessageChannel,
    pub from_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_agent_id: Option<String>,
    pub subject: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

/// Maximum TTL: 7 days.
pub const MAX_MESSAGE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Default TTL: 24 hours.
pub const DEFAULT_MESSAGE_TTL_MS: u64 = 24 * 60 * 60 * 1000;

#[cfg(test)]
mod tests {
    use super::*;

    fn broadcast() -> Message {
        Message {
            id: "msg_zMOeUPn".into(),
            channel: MessageChannel::Broadcast,
            from_agent_id: "cli".into(),
            to_agent_id: None,
            subject: "Hi".into(),
            body: "hello there".into(),
            created_at: "2026-10-13T16:42:10.904Z".into(),
            expires_at: Some("2026-10-14T16:42:10.904Z".into()),
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }
    }

    #[test]
    fn to_agent_id_emits_null_when_absent() {
        let value = serde_json::to_value(broadcast()).unwrap();
        assert_eq!(value["to_agent_id"], serde_json::Value::Null);
        // Every other optional is omitted.
        assert!(value.get("delivered_at").is_none());
        assert!(value.get("team_id").is_none());
        assert!(value.get("reply_to").is_none());
    }

    #[test]
    fn explicit_null_to_agent_id_round_trips() {
        let json = serde_json::to_string(&broadcast()).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, broadcast());
    }

    #[test]
    fn ttl_constants() {
        assert_eq!(MAX_MESSAGE_TTL_MS, 604_800_000);
        assert_eq!(DEFAULT_MESSAGE_TTL_MS, 86_400_000);
    }

    #[test]
    fn channel_and_status_strings_round_trip() {
        for c in MessageChannel::ALL {
            assert_eq!(
                serde_json::to_string(&c).unwrap(),
                format!("\"{}\"", c.as_str())
            );
            assert_eq!(c.as_str().parse::<MessageChannel>(), Ok(c));
        }
        for s in MessageStatus::ALL {
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
            assert_eq!(s.as_str().parse::<MessageStatus>(), Ok(s));
        }
    }
}
