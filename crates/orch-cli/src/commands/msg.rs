//! `orch msg` — ported from `src/cli/commands/msg.ts`
//! (`docs/parity/cli.md` §12).
//!
//! `send` and `broadcast` share the service's fan-out signature, so **both**
//! print a JSON *array* even though a direct send is always one message. A
//! broadcast excludes the sender and every disabled agent, so broadcasting
//! `--from` the only agent in the tree prints `[]` and exits 0 rather than
//! failing.

use orch_core::error::Result;
use orch_domain::{CreateMessageInput, Message, MessageChannel};
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::{Styles, format_duration_since, print_json, print_success, print_table};

pub struct MsgSendArgs<'a> {
    pub to_agent_id: &'a str,
    pub body: &'a str,
    pub subject: Option<&'a str>,
    pub from: Option<&'a str>,
    pub ttl_ms: Option<u64>,
    pub reply_to: Option<&'a str>,
}

pub struct MsgBroadcastArgs<'a> {
    pub body: &'a str,
    pub subject: Option<&'a str>,
    pub from: Option<&'a str>,
    pub team: Option<&'a str>,
    pub ttl_ms: Option<u64>,
}

pub async fn send(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: MsgSendArgs<'_>,
) -> Result<()> {
    let messages = engine
        .message_service
        .send(CreateMessageInput {
            channel: MessageChannel::Direct,
            from_agent_id: args.from.unwrap_or("cli").to_owned(),
            to_agent_id: Some(args.to_agent_id.to_owned()),
            subject: args.subject.unwrap_or_default().to_owned(),
            body: args.body.to_owned(),
            ttl_ms: args.ttl_ms,
            team_id: None,
            reply_to: args.reply_to.map(str::to_owned),
        })
        .await?;

    if ctx.json {
        return print_json(&messages);
    }
    // `messages[0]?.id` — an empty fan-out prints the JS `undefined`.
    let id = first_id(&messages);
    if ctx.quiet {
        println!("{id}");
        return Ok(());
    }
    print_success(
        styles,
        &format!("Message sent: {id} → {}", args.to_agent_id),
    );
    Ok(())
}

pub async fn broadcast(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: MsgBroadcastArgs<'_>,
) -> Result<()> {
    let messages = engine
        .message_service
        .send(CreateMessageInput {
            channel: MessageChannel::Broadcast,
            from_agent_id: args.from.unwrap_or("cli").to_owned(),
            to_agent_id: None,
            subject: args.subject.unwrap_or_default().to_owned(),
            body: args.body.to_owned(),
            ttl_ms: args.ttl_ms,
            team_id: args.team.map(str::to_owned),
            reply_to: None,
        })
        .await?;

    if ctx.json {
        return print_json(&messages);
    }
    if ctx.quiet {
        // `join('\n')` over the ids — nothing at all for an empty fan-out.
        println!(
            "{}",
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        return Ok(());
    }
    print_success(
        styles,
        &format!("Broadcast sent to {} agent(s)", messages.len()),
    );
    Ok(())
}

pub async fn inbox(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    agent_id: &str,
) -> Result<()> {
    let pending = engine
        .message_service
        .list_pending_for_agent(agent_id)
        .await?;

    if ctx.json {
        return print_json(&pending);
    }
    if pending.is_empty() {
        println!("{}", styles.dim("\n  No pending messages.\n"));
        return Ok(());
    }

    println!();
    for message in &pending {
        let subject = if message.subject.is_empty() {
            String::new()
        } else {
            format!(" — {}", message.subject)
        };
        println!(
            "  {} from {}{subject}",
            styles.dim(&message.id),
            message.from_agent_id
        );
        println!("  {}", message.body);
        println!();
    }
    Ok(())
}

pub async fn list(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    agent: Option<&str>,
) -> Result<()> {
    let messages = match agent {
        Some(agent_id) => engine.message_service.list_for_agent(agent_id).await?,
        None => engine.message_service.list_all().await?,
    };

    if ctx.json {
        return print_json(&messages);
    }
    if messages.is_empty() {
        println!("{}", styles.dim("\n  No messages.\n"));
        return Ok(());
    }

    let rows: Vec<Vec<String>> = messages.iter().map(row).collect();
    println!();
    print_table(
        styles,
        &["ID", "FROM", "TO", "CHANNEL", "STATUS", "SENT"],
        &rows,
        2,
    );
    println!("\n  {} message(s)\n", messages.len());
    Ok(())
}

/// `messages[0]?.id` — `undefined` when the fan-out produced nothing, which
/// is what the original interpolates into its success line.
fn first_id(messages: &[Message]) -> &str {
    messages
        .first()
        .map(|message| message.id.as_str())
        .unwrap_or("undefined")
}

fn row(message: &Message) -> Vec<String> {
    vec![
        message.id.clone(),
        message.from_agent_id.clone(),
        message
            .to_agent_id
            .clone()
            .unwrap_or_else(|| "*".to_owned()),
        message.channel.as_str().to_owned(),
        message.status.as_str().to_owned(),
        format_duration_since(&message.created_at),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_domain::MessageStatus;

    fn message(to: Option<&str>) -> Message {
        Message {
            id: "msg_1".to_owned(),
            channel: if to.is_some() {
                MessageChannel::Direct
            } else {
                MessageChannel::Broadcast
            },
            from_agent_id: "cli".to_owned(),
            to_agent_id: to.map(str::to_owned),
            subject: "Hi".to_owned(),
            body: "hello".to_owned(),
            created_at: jiff::Timestamp::now().to_string(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }
    }

    #[test]
    fn a_broadcast_row_shows_a_star_for_the_recipient() {
        assert_eq!(row(&message(None))[2], "*");
        assert_eq!(row(&message(Some("agt_1")))[2], "agt_1");
    }

    #[test]
    fn an_empty_fan_out_interpolates_the_js_undefined() {
        assert_eq!(first_id(&[]), "undefined");
        assert_eq!(first_id(&[message(Some("agt_1"))]), "msg_1");
    }
}
