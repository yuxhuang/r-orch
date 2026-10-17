//! Compatibility retention seed. B1 replaces summary dedup and scanning byte
//! accounting with complete-body, incremental whole-record retention.
use super::{MAX_MESSAGES, Message, MsgType, PendingMessage};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub evicted: usize,
    pub rejected: usize,
    pub owned_bytes: usize,
}
fn report(
    pending: &[PendingMessage],
    live: &[Message],
    before: usize,
    rejected: usize,
) -> RetentionReport {
    let bytes = pending
        .iter()
        .map(|m| record_bytes(&m.text, &m.detail, &m.agent_id, &m.task_id))
        .sum::<usize>()
        + live
            .iter()
            .map(|m| record_bytes(&m.text, &m.detail, &m.agent_id, &m.task_id))
            .sum::<usize>();
    RetentionReport {
        evicted: before.saturating_sub(pending.len() + live.len() + rejected),
        rejected,
        owned_bytes: bytes,
    }
}
fn record_bytes(
    text: &str,
    detail: &Option<String>,
    agent: &Option<String>,
    task: &Option<String>,
) -> usize {
    text.len()
        + detail.as_ref().map_or(0, String::len)
        + agent.as_ref().map_or(0, String::len)
        + task.as_ref().map_or(0, String::len)
}
// The reviewed D9 contract lets B1 evict from either vector; keep its signature.
#[allow(clippy::ptr_arg)]
pub fn stage_message(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    incoming: PendingMessage,
) -> RetentionReport {
    let before = pending.len() + live.len() + 1;
    pending.push(incoming);
    if pending.len() > MAX_MESSAGES {
        pending.drain(..pending.len() - MAX_MESSAGES);
    }
    report(pending, live, before, 0)
}
pub fn flush_messages(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    next_id: &mut u64,
    now_ms: i64,
) -> RetentionReport {
    let before = pending.len() + live.len();
    if pending.is_empty() {
        return report(pending, live, before, 0);
    }
    let staged = std::mem::take(pending);
    let mut rejected = 0;
    let mut batch: Vec<Message> = Vec::with_capacity(staged.len());
    for p in staged {
        // A line identical to the latest one from the same source adds
        // nothing: keep the first, whose timestamp then reads as how long
        // the state has lasted (PARITY.md §12.8 (k)).
        if repeats_latest(live, &batch, &p) {
            rejected += 1;
            continue;
        }
        let id = *next_id;
        *next_id += 1;
        // A message with its own instant (a history entry) keeps it;
        // everything else is stamped with the flush time.
        batch.push(
            Message::new(id, p.ts_ms.unwrap_or(now_ms), &p.text, p.color, p.msg_type)
                .with_agent(p.agent_id)
                .with_task(p.task_id)
                .with_detail(p.detail),
        );
    }
    if batch.is_empty() {
        return report(pending, live, before, rejected);
    }

    if batch.len() >= MAX_MESSAGES {
        let start = batch.len() - MAX_MESSAGES;
        batch.drain(..start);
        *live = batch;
    } else {
        let keep = MAX_MESSAGES - batch.len();
        if live.len() > keep {
            let drop = live.len() - keep;
            live.drain(..drop);
        }
        live.extend(batch);
    }
    report(pending, live, before, rejected)
}
// The reviewed D9 contract lets B1 reconcile pending records while prepending.
#[allow(clippy::ptr_arg)]
pub fn prepend_messages(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    incoming: impl IntoIterator<Item = PendingMessage>,
    next_id: &mut u64,
    now_ms: i64,
) -> RetentionReport {
    let messages: Vec<_> = incoming.into_iter().collect();
    let before = pending.len() + live.len() + messages.len();
    let mut prefix: Vec<Message> = messages
        .into_iter()
        .map(|p| {
            let id = *next_id;
            *next_id += 1;
            // A message with its own instant (a history entry) keeps it;
            // everything else is stamped with the flush time.
            Message::new(id, p.ts_ms.unwrap_or(now_ms), &p.text, p.color, p.msg_type)
                .with_agent(p.agent_id)
                .with_task(p.task_id)
                .with_detail(p.detail)
        })
        .collect();
    prefix.extend(std::mem::take(live));
    if prefix.len() > MAX_MESSAGES {
        let drop = prefix.len() - MAX_MESSAGES;
        prefix.drain(..drop);
    }
    *live = prefix;
    report(pending, live, before, 0)
}
fn repeats_latest(feed: &[Message], batch: &[Message], p: &PendingMessage) -> bool {
    if p.msg_type == MsgType::System && p.agent_id.is_none() {
        return false;
    }
    batch
        .iter()
        .rev()
        .chain(feed.iter().rev())
        .take(64)
        .find(|m| m.agent_id == p.agent_id && m.task_id == p.task_id)
        .is_some_and(|m| m.msg_type == p.msg_type && m.text == p.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Token;
    fn pending(text: &str) -> PendingMessage {
        PendingMessage {
            text: text.into(),
            color: Token::Amber,
            msg_type: MsgType::System,
            agent_id: None,
            task_id: None,
            detail: None,
            ts_ms: None,
        }
    }
    #[test]
    fn compatibility_helpers_keep_history_order_timestamps_and_live_cap() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        let report = stage_message(&mut staged, &mut live, pending("live"));
        assert_eq!(report.owned_bytes, 4);
        flush_messages(&mut staged, &mut live, &mut next_id, 100);
        let mut old = pending("old");
        old.ts_ms = Some(1);
        prepend_messages(&mut staged, &mut live, [old], &mut next_id, 200);
        assert_eq!(
            live.iter()
                .map(|m| (m.text.as_str(), m.ts_ms))
                .collect::<Vec<_>>(),
            [("old", 1), ("live", 100)]
        );
        let report = prepend_messages(
            &mut staged,
            &mut live,
            (0..500).map(|_| pending("history")),
            &mut next_id,
            0,
        );
        assert_eq!(live.len(), 500);
        assert_eq!(report.evicted, 2);
        assert_eq!(live.last().unwrap().text, "live");
    }
}
