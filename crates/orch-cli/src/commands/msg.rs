//! `orch msg`.
//!
//! `send` and `broadcast` share the service's fan-out signature, so **both**
//! print a JSON *array* even though a direct send is always one message. A
//! broadcast excludes the sender and every disabled agent, so broadcasting
//! `--from` the only agent in the tree prints `[]` and exits 0 rather than
//! failing.

use orch_core::error::Result;
use orch_domain::Message;
use orch_proto::ops::args;
use serde_json::json;

use crate::commands::rpc::Rpc;

use crate::context::CliContext;
use crate::output::{Styles, format_duration_since, print_success, print_table};

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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: MsgSendArgs<'_>,
) -> Result<()> {
    let messages = engine
        .call::<args::MessageSendArgs>(json!({
            "from_agent_id": sender(args.from, engine.agent_id.as_deref()),
            "subject": args.subject.unwrap_or_default(),
            "body": args.body,
            "ttl_ms": args.ttl_ms,
            "to_agent_id": args.to_agent_id,
            "reply_to": args.reply_to,
        }))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&messages);
    }
    // An empty fan-out prints `undefined` as the id.
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: MsgBroadcastArgs<'_>,
) -> Result<()> {
    let messages = engine
        .call::<args::MessageBroadcastArgs>(json!({
            "from_agent_id": sender(args.from, engine.agent_id.as_deref()),
            "subject": args.subject.unwrap_or_default(),
            "body": args.body,
            "ttl_ms": args.ttl_ms,
            "team_id": args.team,
        }))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&messages);
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

pub async fn inbox(engine: &Rpc, ctx: &CliContext, styles: &Styles, agent_id: &str) -> Result<()> {
    let pending = engine
        .call::<args::MessageInboxArgs>(json!({"agent_id": agent_id}))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&pending);
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    agent: Option<&str>,
) -> Result<()> {
    let messages = engine
        .call::<args::MessageListArgs>(json!({"agent_id": agent}))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&messages);
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

/// Explicit `--from` wins; the session caller is the default sender.
fn sender<'a>(explicit: Option<&'a str>, caller: Option<&'a str>) -> &'a str {
    explicit.or(caller).unwrap_or("cli")
}

/// `messages[0]?.id` — `undefined` when the fan-out produced nothing, which
/// is what the success line interpolates.
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
    use orch_domain::{MessageChannel, MessageStatus};

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
    fn explicit_sender_overrides_caller_and_cli_is_the_last_default() {
        assert_eq!(sender(Some("explicit"), Some("caller")), "explicit");
        assert_eq!(sender(None, Some("caller")), "caller");
        assert_eq!(sender(None, None), "cli");
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
