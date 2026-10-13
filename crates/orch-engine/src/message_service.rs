//! Message service — ported from `src/application/message-service.ts`.
//!
//! Inter-agent messaging: creation, routing (direct/broadcast/lead), draining
//! an agent's mailbox into its prompt, and purging expired messages. The
//! normative rules are PARITY.md §7.16 and the `message:sent` /
//! `message:delivered` payloads are contract (§7.19).

use std::collections::HashSet;
use std::sync::Arc;

use orch_core::clock::{Clock, iso_from_epoch_ms};
use orch_core::error::{Error, Result};
use orch_core::ids::IdGenerator;
use orch_core::storage::{AgentStore, MessageStore, TeamStore};
use orch_domain::{
    AgentStatus, CreateMessageInput, DEFAULT_MESSAGE_TTL_MS, MAX_MESSAGE_TTL_MS, Message,
    MessageChannel, MessageStatus, OrchestratorEvent,
};

use crate::events::EventBus;

/// The one sender that need not be a registered agent: a message sent by the
/// human through the CLI (`message-service.ts:37`, PARITY.md §7.16).
const CLI_SENDER_ID: &str = "cli";

/// `message-service.ts:45` — the subject default and its cap.
const NO_SUBJECT: &str = "(no subject)";
const SUBJECT_MAX_UNITS: usize = 200;
const BODY_MAX_UNITS: usize = 4000;

pub struct MessageService {
    messages: Arc<dyn MessageStore>,
    agents: Arc<dyn AgentStore>,
    teams: Arc<dyn TeamStore>,
    events: Arc<EventBus>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl MessageService {
    pub fn new(
        messages: Arc<dyn MessageStore>,
        agents: Arc<dyn AgentStore>,
        teams: Arc<dyn TeamStore>,
        events: Arc<EventBus>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        MessageService {
            messages,
            agents,
            teams,
            events,
            clock,
            ids,
        }
    }

    /// Send a message, returning every record created.
    ///
    /// The vector is there for `broadcast`, which fans out to one message per
    /// recipient — each with its own id, each saved, each emitting its own
    /// `message:sent`. `direct` and `lead` always return exactly one.
    ///
    /// Validation order is contract, because the errors differ: empty body,
    /// then TTL range, then sender existence, and only then anything
    /// channel-specific (`message-service.ts:28-39`).
    pub async fn send(&self, input: CreateMessageInput) -> Result<Vec<Message>> {
        if input.body.trim().is_empty() {
            return Err(Error::invalid_arguments("Message body is required"));
        }

        let ttl_ms = input.ttl_ms.unwrap_or(DEFAULT_MESSAGE_TTL_MS);
        if ttl_ms == 0 || ttl_ms > MAX_MESSAGE_TTL_MS {
            return Err(Error::invalid_arguments(format!(
                "TTL must be between 1ms and {MAX_MESSAGE_TTL_MS}ms"
            )));
        }

        // The lookup happens even for `cli`, so a store failure surfaces for
        // every sender rather than only for registered ones.
        if self.agents.get(&input.from_agent_id).await?.is_none()
            && input.from_agent_id != CLI_SENDER_ID
        {
            return Err(Error::invalid_arguments(format!(
                "Sender agent not found: {}",
                input.from_agent_id
            )));
        }

        // One clock read, like the single `new Date()` the TS derives both
        // timestamps from: `created_at` and `expires_at` must not straddle a
        // millisecond boundary (PARITY.md §7.16, `expires_at = created_at + ttl`).
        let now_ms = self.clock.now_ms();
        let subject = if input.subject.is_empty() {
            NO_SUBJECT
        } else {
            &input.subject
        };

        // The TS `baseMessage` spread: every field the channels share, with
        // `id` and `to_agent_id` filled in per recipient by `addressed`.
        let template = Message {
            id: String::new(),
            channel: input.channel,
            from_agent_id: input.from_agent_id.clone(),
            to_agent_id: None,
            subject: truncate_utf16(subject, SUBJECT_MAX_UNITS),
            // Deliberately the raw body, not the trimmed one the check above
            // used: the TS stores `input.body.slice(0, 4000)` verbatim.
            body: truncate_utf16(&input.body, BODY_MAX_UNITS),
            created_at: iso_from_epoch_ms(now_ms),
            expires_at: Some(iso_from_epoch_ms(now_ms + ttl_ms as i64)),
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: input.team_id.clone(),
            reply_to: input.reply_to.clone(),
        };

        match input.channel {
            MessageChannel::Broadcast => self.broadcast(&input, &template).await,
            MessageChannel::Lead => self.to_lead(&input, &template).await,
            MessageChannel::Direct => self.direct(&input, &template).await,
        }
    }

    /// Fan out to every agent except the sender and every `disabled` agent.
    ///
    /// A `team_id` narrows the pool to that team's members first, and a
    /// `team_id` naming no team **silently degrades to all agents** rather than
    /// erroring (`message-service.ts:62`) — unlike the `lead` channel, which
    /// rejects it. Replicated: a CLI `message send --broadcast --team` typo
    /// therefore messages the whole fleet, and the differential harness pins
    /// that behaviour.
    async fn broadcast(
        &self,
        input: &CreateMessageInput,
        template: &Message,
    ) -> Result<Vec<Message>> {
        let mut agents = self.agents.list().await?;

        if let Some(team_id) = non_empty(input.team_id.as_deref())
            && let Some(team) = self.teams.get(team_id).await?
        {
            let member_ids: HashSet<&str> = team
                .members
                .iter()
                .map(|member| member.agent_id.as_str())
                .collect();
            agents.retain(|agent| member_ids.contains(agent.id.as_str()));
        }

        // Ids are handed out in recipient order, then every message is saved,
        // then every message emits — the three phases the TS runs in sequence
        // (`message-service.ts:70-79`). Store order is observable (§6.10), so
        // the recipient order from `AgentStore::list` is preserved throughout.
        let messages: Vec<Message> = agents
            .iter()
            .filter(|agent| {
                agent.id != input.from_agent_id && agent.status != AgentStatus::Disabled
            })
            .map(|agent| self.addressed(template, &agent.id))
            .collect();

        for message in &messages {
            self.messages.save(message).await?;
        }
        for message in &messages {
            self.emit_sent(message);
        }

        Ok(messages)
    }

    /// Resolve the team's lead and send it one direct-style message. The
    /// channel stays `lead` on the record.
    async fn to_lead(
        &self,
        input: &CreateMessageInput,
        template: &Message,
    ) -> Result<Vec<Message>> {
        let Some(team_id) = non_empty(input.team_id.as_deref()) else {
            return Err(Error::invalid_arguments(
                "team_id is required for lead channel",
            ));
        };
        let Some(team) = self.teams.get(team_id).await? else {
            return Err(Error::invalid_arguments(format!(
                "Team not found: {team_id}"
            )));
        };

        // No check that the lead still exists as an agent — the TS does not
        // make one either, so a team whose lead was deleted yields a message
        // nobody can drain rather than an error.
        Ok(vec![self.save_one(template, &team.lead_agent_id).await?])
    }

    async fn direct(&self, input: &CreateMessageInput, template: &Message) -> Result<Vec<Message>> {
        let Some(to_agent_id) = non_empty(input.to_agent_id.as_deref()) else {
            return Err(Error::invalid_arguments(
                "to_agent_id is required for direct messages",
            ));
        };
        if self.agents.get(to_agent_id).await?.is_none() {
            return Err(Error::invalid_arguments(format!(
                "Recipient agent not found: {to_agent_id}"
            )));
        }

        Ok(vec![self.save_one(template, to_agent_id).await?])
    }

    /// Fetch an agent's pending messages and mark them all delivered.
    ///
    /// Called by the orchestrator during `dispatchTask`, which injects the
    /// returned messages into the prompt — so draining is destructive and a
    /// failure between `mark_delivered` and the prompt loses the message. The
    /// TS has the same window.
    pub async fn drain_mailbox(&self, agent_id: &str, task_id: &str) -> Result<Vec<Message>> {
        let pending = self.messages.list_pending(agent_id).await?;

        for message in &pending {
            self.messages.mark_delivered(&message.id).await?;
        }
        for message in &pending {
            self.events.emit(OrchestratorEvent::MessageDelivered {
                message_id: message.id.clone(),
                // The *parameter*, not `message.to_agent_id`
                // (`message-service.ts:124`). They agree for anything
                // `list_pending` returns, since it filters on this agent.
                to_agent_id: agent_id.to_owned(),
                task_id: task_id.to_owned(),
            });
        }

        Ok(pending)
    }

    pub async fn list_all(&self) -> Result<Vec<Message>> {
        self.messages.list().await
    }

    pub async fn list_pending_for_agent(&self, agent_id: &str) -> Result<Vec<Message>> {
        self.messages.list_pending(agent_id).await
    }

    /// Everything addressed to *or* sent by an agent, in `list()` order
    /// (`created_at` ascending).
    pub async fn list_for_agent(&self, agent_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .messages
            .list()
            .await?
            .into_iter()
            .filter(|message| {
                message.to_agent_id.as_deref() == Some(agent_id)
                    || message.from_agent_id == agent_id
            })
            .collect())
    }

    /// Number of messages purged. The expiry rules live in the store
    /// (PARITY.md §7.16), not here.
    pub async fn purge_expired(&self) -> Result<usize> {
        self.messages.purge_expired().await
    }

    async fn save_one(&self, template: &Message, to_agent_id: &str) -> Result<Message> {
        let message = self.addressed(template, to_agent_id);
        self.messages.save(&message).await?;
        self.emit_sent(&message);
        Ok(message)
    }

    fn addressed(&self, template: &Message, to_agent_id: &str) -> Message {
        Message {
            id: self.ids.generate("msg"),
            to_agent_id: Some(to_agent_id.to_owned()),
            ..template.clone()
        }
    }

    fn emit_sent(&self, message: &Message) {
        self.events.emit(OrchestratorEvent::MessageSent {
            message_id: message.id.clone(),
            from_agent_id: message.from_agent_id.clone(),
            to_agent_id: message.to_agent_id.clone(),
            channel: message.channel,
        });
    }
}

/// JS truthiness for an optional string field: `''` is as absent as `undefined`
/// to `if (!input.team_id)`.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// `String.prototype.slice(0, max)` counts **UTF-16 code units**, so the caps
/// are applied in those units rather than bytes or `char`s
/// (`message-service.ts:45-46`).
///
/// One divergence, in the only case Rust cannot express: where a cut would
/// land between the halves of a surrogate pair, JS keeps the lone high
/// surrogate and this drops the whole character. Every other input — ASCII
/// included, which is all the CLI and the adapters produce — matches exactly.
fn truncate_utf16(text: &str, max_units: usize) -> String {
    // UTF-16 units never exceed UTF-8 bytes, so this is a sound fast path.
    if text.len() <= max_units {
        return text.to_owned();
    }

    let mut units = 0;
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if units + ch.len_utf16() > max_units {
            break;
        }
        units += ch.len_utf16();
        end = index + ch.len_utf8();
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{
        FIXED_NOW_ISO, FakeAgentStore, FakeMessageStore, FakeTeamStore, FixedClock,
        MessageStoreCall, SeqIdGenerator, make_agent,
    };
    use orch_domain::{
        Agent, OrchestratorEventType, Team, TeamConfig, TeamMember, TeamMemberRole, TeamStatus,
    };

    #[test]
    fn truncation_stops_at_the_last_whole_character() {
        // PARITY.md §14 / TRAITS-FROZEN §11, ruled L2 above the limit. JS
        // slices by UTF-16 code unit and will cut *between* the surrogates of
        // a non-BMP character, leaving a lone surrogate that a Rust `String`
        // cannot even hold. We stop at the last whole character instead.
        let emoji = "\u{1F600}"; // two UTF-16 units, four UTF-8 bytes

        // 3999 units of ASCII plus a 2-unit character is 4001: the emoji does
        // not fit, so it is dropped whole rather than halved.
        let body = "a".repeat(3999) + emoji;
        let truncated = truncate_utf16(&body, BODY_MAX_UNITS);
        assert_eq!(truncated, "a".repeat(3999));
        assert!(!truncated.contains(emoji));

        // One character earlier it fits exactly, and must be kept.
        let fits = "a".repeat(3998) + emoji;
        assert_eq!(truncate_utf16(&fits, BODY_MAX_UNITS), fits);
        assert_eq!(utf16_len(&truncate_utf16(&fits, BODY_MAX_UNITS)), 4000);

        // Same arithmetic at the subject's 200-unit cap.
        assert_eq!(
            truncate_utf16(&("b".repeat(199) + emoji), SUBJECT_MAX_UNITS),
            "b".repeat(199)
        );
        assert_eq!(
            truncate_utf16(&("b".repeat(198) + emoji), SUBJECT_MAX_UNITS),
            "b".repeat(198) + emoji
        );
    }

    #[test]
    fn truncation_never_exceeds_the_cap_for_any_split_point() {
        // The property, over every alignment of a 2-unit character against
        // the boundary: the result is always whole characters and always
        // within the cap. A single hand-picked case would miss an off-by-one.
        for ascii in 0..12 {
            for emoji_count in 0..6 {
                let text = "a".repeat(ascii) + &"\u{1F600}".repeat(emoji_count);
                for cap in 0..16 {
                    let truncated = truncate_utf16(&text, cap);
                    assert!(utf16_len(&truncated) <= cap, "{text:?} cap={cap}");
                    assert!(text.starts_with(&truncated), "{text:?} cap={cap}");
                    // Whole characters only — guaranteed by `String`, but the
                    // point is that nothing was dropped mid-character.
                    assert!(
                        truncated.chars().count()
                            == text.chars().take(truncated.chars().count()).count(),
                        "{text:?} cap={cap}"
                    );
                }
            }
        }
    }

    fn utf16_len(text: &str) -> usize {
        text.chars().map(char::len_utf16).sum()
    }

    const TIMESTAMP: &str = "2025-01-01T00:00:00Z";
    /// [`FIXED_NOW_ISO`] plus the default 24-hour TTL.
    const FIXED_EXPIRY_ISO: &str = "2025-10-14T16:37:33.426Z";

    struct Harness {
        service: MessageService,
        messages: Arc<FakeMessageStore>,
        teams: Arc<FakeTeamStore>,
        bus: Arc<EventBus>,
    }

    fn agent(id: &str, name: &str, status: AgentStatus) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            status,
            ..make_agent()
        }
    }

    /// The five agents the TS `beforeEach` seeds, in its order — `list()`
    /// returns seed order and broadcast fan-out follows it.
    fn harness() -> Harness {
        let messages = Arc::new(FakeMessageStore::new());
        let agents = Arc::new(FakeAgentStore::with_agents([
            agent("agt_sender", "Sender", AgentStatus::Idle),
            agent("agt_receiver", "Receiver", AgentStatus::Idle),
            agent("agt_lead", "Lead", AgentStatus::Idle),
            agent("agt_m1", "Member1", AgentStatus::Idle),
            agent("agt_disabled", "Disabled", AgentStatus::Disabled),
        ]));
        let teams = Arc::new(FakeTeamStore::new());
        let bus = Arc::new(EventBus::new());
        let service = MessageService::new(
            Arc::clone(&messages) as Arc<dyn MessageStore>,
            agents as Arc<dyn AgentStore>,
            Arc::clone(&teams) as Arc<dyn TeamStore>,
            Arc::clone(&bus),
            Arc::new(FixedClock::new()),
            Arc::new(SeqIdGenerator::new()),
        );
        Harness {
            service,
            messages,
            teams,
            bus,
        }
    }

    fn make_team() -> Team {
        Team {
            id: "team_t1".into(),
            name: "Test Team".into(),
            description: None,
            status: TeamStatus::Active,
            members: vec![
                TeamMember {
                    agent_id: "agt_lead".into(),
                    role: TeamMemberRole::Lead,
                    joined_at: TIMESTAMP.into(),
                },
                TeamMember {
                    agent_id: "agt_m1".into(),
                    role: TeamMemberRole::Member,
                    joined_at: TIMESTAMP.into(),
                },
            ],
            task_pool: vec![],
            lead_agent_id: "agt_lead".into(),
            created_at: TIMESTAMP.into(),
            updated_at: TIMESTAMP.into(),
            config: TeamConfig {
                max_concurrent_tasks: None,
                auto_claim: true,
                message_ttl_ms: None,
            },
        }
    }

    fn input(channel: MessageChannel, from: &str, body: &str) -> CreateMessageInput {
        CreateMessageInput {
            channel,
            from_agent_id: from.into(),
            subject: "Test".into(),
            body: body.into(),
            ..Default::default()
        }
    }

    fn pending(id: &str, from: &str, to: &str, created_at: &str) -> Message {
        Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: from.into(),
            to_agent_id: Some(to.into()),
            subject: "Hi".into(),
            body: "Hello".into(),
            created_at: created_at.into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }
    }

    fn saves(messages: &FakeMessageStore) -> usize {
        messages
            .calls
            .count_matching(|call| matches!(call, MessageStoreCall::Save(_)))
    }

    // ── send direct message ────────────────────────────────────────────────

    #[tokio::test]
    async fn direct_send_creates_a_message_with_the_right_fields() {
        let h = harness();
        let messages = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                subject: "Test Subject".into(),
                ..input(MessageChannel::Direct, "agt_sender", "Hello from sender")
            })
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.channel, MessageChannel::Direct);
        assert_eq!(message.from_agent_id, "agt_sender");
        assert_eq!(message.to_agent_id.as_deref(), Some("agt_receiver"));
        assert_eq!(message.subject, "Test Subject");
        assert_eq!(message.body, "Hello from sender");
        assert_eq!(message.status, MessageStatus::Pending);
        assert_eq!(message.id, "msg_0000001");
        assert_eq!(message.created_at, FIXED_NOW_ISO);
        assert_eq!(message.expires_at.as_deref(), Some(FIXED_EXPIRY_ISO));
        assert_eq!(saves(&h.messages), 1);
        assert_eq!(h.messages.peek("msg_0000001").as_ref(), Some(message));
    }

    #[tokio::test]
    async fn direct_send_rejects_an_unknown_recipient() {
        let h = harness();
        let err = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_nonexistent".into()),
                ..input(MessageChannel::Direct, "agt_sender", "Hello")
            })
            .await
            .expect_err("rejected");

        assert_eq!(
            err.to_string(),
            "Recipient agent not found: agt_nonexistent"
        );
        assert_eq!(err.exit_code(), 2);
        assert_eq!(saves(&h.messages), 0);
    }

    #[tokio::test]
    async fn a_whitespace_only_body_is_rejected() {
        let h = harness();
        let err = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ..input(MessageChannel::Direct, "agt_sender", "   ")
            })
            .await
            .expect_err("rejected");

        assert_eq!(err.to_string(), "Message body is required");
        assert_eq!(err.exit_code(), 2);
    }

    // ── send broadcast ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn broadcast_creates_one_message_per_non_sender_enabled_agent() {
        let h = harness();
        let messages = h
            .service
            .send(CreateMessageInput {
                subject: "Broadcast".into(),
                ..input(MessageChannel::Broadcast, "agt_sender", "Hello everyone")
            })
            .await
            .unwrap();

        let recipients: Vec<Option<&str>> =
            messages.iter().map(|m| m.to_agent_id.as_deref()).collect();
        assert_eq!(
            recipients,
            vec![Some("agt_receiver"), Some("agt_lead"), Some("agt_m1")]
        );
        assert_eq!(saves(&h.messages), 3);
        // One generated id each, handed out in recipient order.
        let ids: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["msg_0000001", "msg_0000002", "msg_0000003"]);
    }

    #[tokio::test]
    async fn broadcast_with_a_team_id_restricts_to_team_members() {
        let h = harness();
        h.teams.seed(make_team());

        let messages = h
            .service
            .send(CreateMessageInput {
                subject: "Team broadcast".into(),
                team_id: Some("team_t1".into()),
                ..input(MessageChannel::Broadcast, "agt_lead", "Team message")
            })
            .await
            .unwrap();

        // agt_lead is the sender and so excluded; only agt_m1 is left.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].to_agent_id.as_deref(), Some("agt_m1"));
        assert_eq!(messages[0].team_id.as_deref(), Some("team_t1"));
    }

    // ── send lead channel ──────────────────────────────────────────────────

    #[tokio::test]
    async fn lead_channel_resolves_to_the_team_lead() {
        let h = harness();
        h.teams.seed(make_team());

        let messages = h
            .service
            .send(CreateMessageInput {
                subject: "To lead".into(),
                team_id: Some("team_t1".into()),
                ..input(MessageChannel::Lead, "agt_m1", "Need guidance")
            })
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].to_agent_id.as_deref(), Some("agt_lead"));
        assert_eq!(messages[0].channel, MessageChannel::Lead);
    }

    #[tokio::test]
    async fn lead_channel_requires_a_team_id() {
        let h = harness();
        let err = h
            .service
            .send(input(MessageChannel::Lead, "agt_m1", "Need guidance"))
            .await
            .expect_err("rejected");

        assert_eq!(err.to_string(), "team_id is required for lead channel");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn lead_channel_rejects_an_unknown_team() {
        let h = harness();
        let err = h
            .service
            .send(CreateMessageInput {
                team_id: Some("team_nonexistent".into()),
                ..input(MessageChannel::Lead, "agt_m1", "Need guidance")
            })
            .await
            .expect_err("rejected");

        assert_eq!(err.to_string(), "Team not found: team_nonexistent");
        assert_eq!(err.exit_code(), 2);
    }

    // ── drainMailbox ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn drain_mailbox_returns_pending_messages_and_marks_them_delivered() {
        let h = harness();
        h.messages.seed(pending(
            "msg_1",
            "agt_sender",
            "agt_receiver",
            "2025-01-01T00:00:00Z",
        ));
        h.messages.seed(Message {
            channel: MessageChannel::Broadcast,
            subject: "Update".into(),
            body: "News".into(),
            ..pending("msg_2", "agt_lead", "agt_receiver", "2025-01-01T00:01:00Z")
        });

        let drained = h
            .service
            .drain_mailbox("agt_receiver", "tsk_1")
            .await
            .unwrap();

        assert_eq!(drained.len(), 2);
        let marked = h
            .messages
            .calls
            .matching(|call| matches!(call, MessageStoreCall::MarkDelivered(_)));
        assert_eq!(
            marked,
            vec![
                MessageStoreCall::MarkDelivered("msg_1".into()),
                MessageStoreCall::MarkDelivered("msg_2".into()),
            ]
        );
        assert_eq!(
            h.messages.peek("msg_1").unwrap().status,
            MessageStatus::Delivered
        );
        // Draining is destructive: nothing is left pending.
        assert!(
            h.service
                .list_pending_for_agent("agt_receiver")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn drain_mailbox_emits_message_delivered() {
        let h = harness();
        h.messages.seed(pending(
            "msg_ev",
            "agt_sender",
            "agt_receiver",
            "2025-01-01T00:00:00Z",
        ));
        let mut sub = h.bus.subscribe();

        h.service
            .drain_mailbox("agt_receiver", "tsk_1")
            .await
            .unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type(),
            OrchestratorEventType::MessageDelivered
        );
        assert_eq!(
            *events[0],
            OrchestratorEvent::MessageDelivered {
                message_id: "msg_ev".into(),
                to_agent_id: "agt_receiver".into(),
                task_id: "tsk_1".into(),
            }
        );
    }

    // ── listForAgent ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_for_agent_returns_messages_sent_to_and_from_the_agent() {
        let h = harness();
        h.messages.seed(pending(
            "msg_to",
            "agt_sender",
            "agt_receiver",
            "2025-01-01T00:00:00Z",
        ));
        h.messages.seed(pending(
            "msg_from",
            "agt_receiver",
            "agt_sender",
            "2025-01-01T00:01:00Z",
        ));
        h.messages.seed(pending(
            "msg_unrelated",
            "agt_lead",
            "agt_m1",
            "2025-01-01T00:02:00Z",
        ));

        let found = h.service.list_for_agent("agt_receiver").await.unwrap();
        let mut ids: Vec<&str> = found.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["msg_from", "msg_to"]);
    }

    // ── purgeExpired ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn purge_expired_delegates_to_the_store() {
        let h = harness();

        // The TS test stubs the store to return 5; `FakeMessageStore` has no
        // clock and so always purges nothing. The assertion that carries the
        // behaviour is the delegation, not the number.
        assert_eq!(h.service.purge_expired().await.unwrap(), 0);
        assert_eq!(
            h.messages.calls.count_of(&MessageStoreCall::PurgeExpired),
            1
        );
    }

    // ── send emits message:sent ────────────────────────────────────────────

    #[tokio::test]
    async fn direct_send_emits_message_sent() {
        let h = harness();
        let mut sub = h.bus.subscribe();

        h.service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ..input(MessageChannel::Direct, "agt_sender", "Hello")
            })
            .await
            .unwrap();

        let events = sub.try_drain();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type(), OrchestratorEventType::MessageSent);
        assert_eq!(
            *events[0],
            OrchestratorEvent::MessageSent {
                message_id: "msg_0000001".into(),
                from_agent_id: "agt_sender".into(),
                to_agent_id: Some("agt_receiver".into()),
                channel: MessageChannel::Direct,
            }
        );
    }

    // ── TTL validation ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_zero_ttl_is_rejected() {
        let h = harness();
        let err = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ttl_ms: Some(0),
                ..input(MessageChannel::Direct, "agt_sender", "Hello")
            })
            .await
            .expect_err("rejected");

        assert_eq!(err.to_string(), "TTL must be between 1ms and 604800000ms");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn a_ttl_above_the_maximum_is_rejected() {
        let h = harness();
        let err = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ttl_ms: Some(8 * 24 * 60 * 60 * 1000),
                ..input(MessageChannel::Direct, "agt_sender", "Hello")
            })
            .await
            .expect_err("rejected");

        assert_eq!(err.to_string(), "TTL must be between 1ms and 604800000ms");
        assert_eq!(err.exit_code(), 2);
    }

    // ── Beyond the TS suite: rules §7.16 states that it never exercises ────

    #[tokio::test]
    async fn the_cli_may_send_without_being_a_registered_agent() {
        let h = harness();
        let messages = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ..input(MessageChannel::Direct, "cli", "Hello")
            })
            .await
            .unwrap();
        assert_eq!(messages[0].from_agent_id, "cli");

        let err = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                ..input(MessageChannel::Direct, "agt_ghost", "Hello")
            })
            .await
            .expect_err("rejected");
        assert_eq!(err.to_string(), "Sender agent not found: agt_ghost");
        assert_eq!(err.exit_code(), 2);
    }

    #[tokio::test]
    async fn an_empty_subject_falls_back_and_both_caps_apply() {
        let h = harness();
        let messages = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                subject: String::new(),
                ..input(MessageChannel::Direct, "agt_sender", &"b".repeat(5_000))
            })
            .await
            .unwrap();
        assert_eq!(messages[0].subject, "(no subject)");
        assert_eq!(messages[0].body.len(), BODY_MAX_UNITS);

        let messages = h
            .service
            .send(CreateMessageInput {
                to_agent_id: Some("agt_receiver".into()),
                subject: "s".repeat(250),
                ..input(MessageChannel::Direct, "agt_sender", "body")
            })
            .await
            .unwrap();
        assert_eq!(messages[0].subject.len(), SUBJECT_MAX_UNITS);
    }

    #[tokio::test]
    async fn a_broadcast_team_id_naming_no_team_degrades_to_every_agent() {
        let h = harness();
        let messages = h
            .service
            .send(CreateMessageInput {
                team_id: Some("team_nonexistent".into()),
                ..input(MessageChannel::Broadcast, "agt_sender", "Hello everyone")
            })
            .await
            .unwrap();

        // Replicated TS behaviour, not an oversight: unlike `lead`, broadcast
        // does not reject an unknown team (`message-service.ts:62`).
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn truncation_counts_utf16_units_and_never_splits_a_character() {
        assert_eq!(truncate_utf16("abcdef", 3), "abc");
        assert_eq!(truncate_utf16("abc", 10), "abc");
        // 'é' is one UTF-16 unit but two UTF-8 bytes.
        assert_eq!(truncate_utf16("ééé", 2), "éé");
        // An emoji is a surrogate pair: two units. A cap of 3 fits one whole
        // emoji and must not split the second (JS keeps a lone surrogate).
        assert_eq!(truncate_utf16("🙂🙂", 3), "🙂");
        assert_eq!(truncate_utf16("🙂🙂", 4), "🙂🙂");
        assert_eq!(truncate_utf16("🙂", 1), "");
    }
}
