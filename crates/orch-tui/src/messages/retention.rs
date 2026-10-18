//! Whole-record message retention under explicit limits (design D4).
//!
//! Three helpers own every admission into the pending and live feeds:
//! [`stage_message`] (a live event arriving), [`flush_messages`] (the batch
//! timer moving pending records into the feed) and [`prepend_messages`]
//! (run history arriving underneath). They enforce, together and at all
//! times:
//!
//! * the existing [`MAX_MESSAGES`] entry limit on **each** collection;
//! * [`MAX_BODY_BYTES`] per record — a larger text or detail is replaced
//!   whole by an explicit "Body unavailable" record naming its size, never
//!   cut to a prefix that would masquerade as the full text;
//! * [`RETENTION_BUDGET_BYTES`] over every owned string of both collections
//!   combined (text, detail, agent and task IDs), counted once per record.
//!
//! Limits evict whole, oldest records: newer live data displaces older live
//! data, and history — which is older than anything live — is evicted before
//! any live record. Budgets are enforced record by record while consuming
//! input, not after collecting a batch, and records are moved, never cloned.
//! The byte totals are recomputed by summing capacities over at most
//! `2 × MAX_MESSAGES` records per call; no string is copied to do it.
//!
//! Deduplication compares the **complete** body (text and detail): two events
//! that share a 200-character preview but differ later both survive.

use std::collections::VecDeque;

use super::{
    MAX_BODY_BYTES, MAX_MESSAGES, Message, MsgType, PendingMessage, RETENTION_BUDGET_BYTES,
    unavailable_bytes, unavailable_notice,
};

/// What one helper call did. Counts are per call; the caller keeps any
/// cumulative notice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// Previously admitted records removed (oldest first) to respect the
    /// entry or byte limits, plus history records dropped for the same
    /// reason before they became visible.
    pub evicted: usize,
    /// Incoming records not retained as received: bodies over
    /// [`MAX_BODY_BYTES`] (kept as an explicit unavailable notice), and
    /// records that could not fit at all.
    pub rejected: usize,
    /// Incoming records dropped by the existing repeat policy: identical to
    /// the latest record from the same source, compared on the complete body.
    pub deduplicated: usize,
    /// Owned text allocation capacity of pending + live after the call.
    pub owned_bytes: usize,
}

fn bytes_of_pending(pending: &[PendingMessage]) -> usize {
    pending
        .iter()
        .map(PendingMessage::owned_bytes)
        .fold(0, usize::saturating_add)
}

fn bytes_of_live(live: &[Message]) -> usize {
    live.iter()
        .map(Message::owned_bytes)
        .fold(0, usize::saturating_add)
}

/// Replace a body over [`MAX_BODY_BYTES`] by the explicit unavailable pair
/// (notice text, empty detail). Returns whether the record was rejected.
///
/// The oversized strings are dropped, not copied: formatting the notice needs
/// only their length.
fn admit_body(message: &mut PendingMessage) -> bool {
    let detail_len = message.detail.as_ref().map_or(0, String::len);
    let text_len = message.text.len();
    if detail_len <= MAX_BODY_BYTES && text_len <= MAX_BODY_BYTES {
        return false;
    }
    let original = if detail_len > MAX_BODY_BYTES {
        detail_len
    } else {
        text_len
    };
    message.text = unavailable_notice(original);
    message.detail = Some(String::new());
    true
}

/// Initial admission also counts bodies already rejected by the formatter.
/// Flush uses only `admit_body`: a staged notice has already been counted.
fn admit_incoming_body(message: &mut PendingMessage) -> bool {
    admit_body(message) || unavailable_bytes(&message.text, message.detail.as_deref()).is_some()
}

/// Evict the oldest live records, then the oldest pending ones, until the
/// combined total plus `incoming` fits the budget. Returns how many went.
fn make_room(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    total: &mut usize,
    incoming: usize,
) -> usize {
    let mut live_drop = 0;
    while total.saturating_add(incoming) > RETENTION_BUDGET_BYTES && live_drop < live.len() {
        *total = total.saturating_sub(live[live_drop].owned_bytes());
        live_drop += 1;
    }
    live.drain(..live_drop);
    let mut pending_drop = 0;
    while total.saturating_add(incoming) > RETENTION_BUDGET_BYTES && pending_drop < pending.len() {
        *total = total.saturating_sub(pending[pending_drop].owned_bytes());
        pending_drop += 1;
    }
    pending.drain(..pending_drop);
    live_drop + pending_drop
}

/// A live event arrives: admit it to `pending`, evicting whole older records
/// as needed. One bounded incoming document is inspected before admission.
pub fn stage_message(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    mut incoming: PendingMessage,
) -> RetentionReport {
    let rejected = usize::from(admit_incoming_body(&mut incoming));
    let mut total = bytes_of_pending(pending).saturating_add(bytes_of_live(live));
    let size = incoming.owned_bytes();
    if size > RETENTION_BUDGET_BYTES {
        // Spare capacity or IDs can make even a bounded body unretainable.
        // Refuse it whole without evicting records that still fit.
        return RetentionReport {
            rejected: 1,
            owned_bytes: total,
            ..RetentionReport::default()
        };
    }
    let mut evicted = make_room(pending, live, &mut total, size);
    pending.push(incoming);
    total = total.saturating_add(size);
    if pending.len() > MAX_MESSAGES {
        let overflow = pending.len() - MAX_MESSAGES;
        total = total.saturating_sub(bytes_of_pending(&pending[..overflow]));
        pending.drain(..overflow);
        evicted += overflow;
    }
    RetentionReport {
        evicted,
        rejected,
        deduplicated: 0,
        owned_bytes: total,
    }
}

/// The batch timer fires: move every pending record into the feed, assigning
/// IDs and timestamps, deduplicating repeats and keeping the feed within its
/// limits. Pending and live bytes are one budget, so the transfer itself
/// never needs room; only the entry limit can evict here.
pub fn flush_messages(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    next_id: &mut u64,
    now_ms: i64,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    if pending.is_empty() {
        report.owned_bytes = bytes_of_live(live);
        return report;
    }
    for mut p in std::mem::take(pending) {
        // A line identical to the latest one from the same source adds
        // nothing: keep the first, whose timestamp then reads as how long
        // the state has lasted (PARITY.md §12.8 (k)).
        if repeats_latest(live, &p) {
            report.deduplicated += 1;
            continue;
        }
        report.rejected += usize::from(admit_body(&mut p));
        let id = *next_id;
        *next_id = next_id.saturating_add(1);
        // A message with its own instant (a history entry) keeps it;
        // everything else is stamped with the flush time.
        live.push(into_message(id, p, now_ms));
    }
    if live.len() > MAX_MESSAGES {
        let overflow = live.len() - MAX_MESSAGES;
        live.drain(..overflow);
        report.evicted += overflow;
    }
    let mut total = bytes_of_live(live);
    report.evicted += make_room(pending, live, &mut total, 0);
    report.owned_bytes = total;
    report
}

/// A history batch (oldest first) arrives underneath the feed. History is
/// older than every live and pending record, so it only ever takes what is
/// left: its own oldest records are evicted first, and no live or pending
/// record is displaced. Input is consumed one record at a time.
// `&mut Vec` is the I0-seeded contract shared with B0's call site; history
// never grows or shrinks the pending queue, so clippy sees only a slice use.
#[allow(
    clippy::ptr_arg,
    reason = "`&mut Vec` is the contract shared with the call site"
)]
pub fn prepend_messages(
    pending: &mut Vec<PendingMessage>,
    live: &mut Vec<Message>,
    incoming: impl IntoIterator<Item = PendingMessage>,
    next_id: &mut u64,
    now_ms: i64,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    let newer = bytes_of_pending(pending).saturating_add(bytes_of_live(live));
    let room_bytes = RETENTION_BUDGET_BYTES.saturating_sub(newer);
    let room_entries = MAX_MESSAGES.saturating_sub(live.len());
    let mut prefix: VecDeque<Message> = VecDeque::new();
    let mut prefix_bytes = 0usize;
    for mut p in incoming {
        let body_rejected = admit_incoming_body(&mut p);
        report.rejected += usize::from(body_rejected);
        let size = p.owned_bytes();
        if size > room_bytes || room_entries == 0 {
            // Cannot fit even alone: newer records always win.
            report.rejected += usize::from(!body_rejected);
            continue;
        }
        while prefix_bytes.saturating_add(size) > room_bytes || prefix.len() >= room_entries {
            let Some(oldest) = prefix.pop_front() else {
                break;
            };
            prefix_bytes = prefix_bytes.saturating_sub(oldest.owned_bytes());
            report.evicted += 1;
        }
        let id = *next_id;
        *next_id = next_id.saturating_add(1);
        prefix_bytes = prefix_bytes.saturating_add(size);
        prefix.push_back(into_message(id, p, now_ms));
    }
    if !prefix.is_empty() {
        let mut merged: Vec<Message> = Vec::with_capacity(prefix.len() + live.len());
        merged.extend(prefix);
        merged.append(live);
        *live = merged;
    }
    report.owned_bytes = newer.saturating_add(prefix_bytes);
    report
}

fn into_message(id: u64, p: PendingMessage, now_ms: i64) -> Message {
    let mut message = Message::new(id, p.ts_ms.unwrap_or(now_ms), "", p.color, p.msg_type)
        .with_agent(p.agent_id)
        .with_task(p.task_id)
        .with_detail(p.detail);
    // Move, not copy: the text was already admitted.
    message.text = p.text;
    message
}

/// Whether `p` repeats the latest record from the same source, compared on
/// the complete body. Looks back at most 64 records, as before.
fn repeats_latest(feed: &[Message], p: &PendingMessage) -> bool {
    if p.msg_type == MsgType::System && p.agent_id.is_none() {
        return false;
    }
    feed.iter()
        .rev()
        .take(64)
        .find(|m| m.agent_id == p.agent_id && m.task_id == p.task_id)
        .is_some_and(|m| {
            m.msg_type == p.msg_type
                && m.text == p.text
                && m.detail.as_deref() == p.detail.as_deref()
        })
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
    fn helpers_keep_history_order_timestamps_and_live_cap() {
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

    fn from_agent(agent: &str, text: &str, detail: Option<String>) -> PendingMessage {
        PendingMessage {
            agent_id: Some(agent.into()),
            msg_type: MsgType::Output,
            detail,
            ..pending(text)
        }
    }

    fn owned(pending: &[PendingMessage], live: &[Message]) -> usize {
        bytes_of_pending(pending) + bytes_of_live(live)
    }

    fn spare_capacity(value: &str, capacity: usize) -> String {
        let mut text = String::with_capacity(capacity);
        text.push_str(value);
        text
    }

    #[test]
    fn capacity_accounting_charges_pending_capacity_and_shared_live_payload() {
        let incoming = PendingMessage {
            text: spare_capacity("preview", 128),
            detail: Some(spare_capacity("body", 256)),
            agent_id: Some(spare_capacity("agent", 64)),
            task_id: Some(spare_capacity("", 32)),
            ..pending("")
        };
        let capacity = incoming.text.capacity()
            + incoming.detail.as_ref().unwrap().capacity()
            + incoming.agent_id.as_ref().unwrap().capacity()
            + incoming.task_id.as_ref().unwrap().capacity();
        let pointers = (
            incoming.text.as_ptr(),
            incoming.agent_id.as_ref().unwrap().as_ptr(),
            incoming.task_id.as_ref().unwrap().as_ptr(),
        );
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        let report = stage_message(&mut staged, &mut live, incoming);
        assert_eq!(report.owned_bytes, capacity);
        let report = flush_messages(&mut staged, &mut live, &mut next_id, 1);
        // Conversion drops the String's spare capacity. The exact-size Arc
        // payload is charged once and shared with the pinned Raw document.
        assert_eq!(report.owned_bytes, capacity - 256 + 4);
        let message = &live[0];
        let document = super::super::message_document(message, 0, 0);
        assert!(std::sync::Arc::ptr_eq(
            message.detail.as_ref().unwrap(),
            &document.body,
        ));
        assert_eq!(
            (
                message.text.as_ptr(),
                message.agent_id.as_ref().unwrap().as_ptr(),
                message.task_id.as_ref().unwrap().as_ptr(),
            ),
            pointers,
        );
    }

    #[test]
    fn capacity_pressure_evicts_oldest_live_before_pending_and_refuses_history() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        // Four tiny bodies each own a quarter of the budget in spare space.
        for i in 0..4 {
            let mut incoming = pending("");
            incoming.text = spare_capacity(&i.to_string(), RETENTION_BUDGET_BYTES / 4);
            stage_message(&mut staged, &mut live, incoming);
        }
        flush_messages(&mut staged, &mut live, &mut next_id, 1);
        let mut newest = pending("");
        newest.text = spare_capacity("newest", RETENTION_BUDGET_BYTES / 4);
        let report = stage_message(&mut staged, &mut live, newest);
        assert_eq!(report.evicted, 1);
        assert_eq!(live[0].text, "1");
        assert_eq!(staged[0].text, "newest");
        assert_eq!(report.owned_bytes, RETENTION_BUDGET_BYTES);
        let report = prepend_messages(
            &mut staged,
            &mut live,
            [pending("history")],
            &mut next_id,
            2,
        );
        assert_eq!(report.rejected, 1);
        assert_eq!(report.evicted, 0);
        assert_eq!(live.len(), 3);
        assert_eq!(staged.len(), 1);
    }

    #[test]
    fn capacity_larger_than_the_budget_is_refused_without_displacing_live() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        stage_message(&mut staged, &mut live, pending("keep"));
        flush_messages(&mut staged, &mut live, &mut next_id, 1);
        let mut incoming = pending("tiny body");
        incoming.agent_id = Some(spare_capacity("agent", RETENTION_BUDGET_BYTES + 1));
        let report = stage_message(&mut staged, &mut live, incoming);
        assert_eq!(report.rejected, 1);
        assert_eq!(report.evicted, 0);
        assert_eq!(report.owned_bytes, 4);
        assert!(staged.is_empty());
        assert_eq!(live[0].text, "keep");
    }

    /// R1: live records approaching and past 32 MiB. The budget holds after
    /// every call, the report's total is exact, and the oldest go first.
    #[test]
    fn r1_live_staging_and_flushing_never_exceed_the_budget_and_evict_oldest_first() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        let body = "x".repeat(MAX_BODY_BYTES);
        for i in 0..40 {
            let report = stage_message(
                &mut staged,
                &mut live,
                from_agent(&format!("a{i:02}"), "t", Some(body.clone())),
            );
            assert!(report.owned_bytes <= RETENTION_BUDGET_BYTES, "stage {i}");
            assert_eq!(report.owned_bytes, owned(&staged, &live), "stage {i}");
            if i % 7 == 6 {
                let report = flush_messages(&mut staged, &mut live, &mut next_id, i);
                assert!(report.owned_bytes <= RETENTION_BUDGET_BYTES, "flush {i}");
                assert_eq!(report.owned_bytes, owned(&staged, &live), "flush {i}");
            }
        }
        flush_messages(&mut staged, &mut live, &mut next_id, 99);
        let agents: Vec<&str> = live.iter().filter_map(|m| m.agent_id.as_deref()).collect();
        assert_eq!(agents.last(), Some(&"a39"), "the newest record survives");
        assert!(!agents.contains(&"a00"), "the oldest record went first");
        assert!(
            agents.windows(2).all(|w| w[0] < w[1]),
            "order kept: {agents:?}"
        );
        assert!(
            live.iter()
                .all(|m| m.detail.as_deref() == Some(body.as_str()))
        );
        let ids: Vec<u64> = live.iter().map(|m| m.id).collect();
        assert!(
            ids.windows(2).all(|w| w[0] < w[1]),
            "displayed IDs reconcile"
        );
    }

    /// R1: history larger than the remaining room loses its own oldest
    /// records and never displaces a live one.
    #[test]
    fn r1_history_larger_than_the_room_drops_its_oldest_and_never_displaces_live() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        let body = "y".repeat(MAX_BODY_BYTES);
        for i in 0..20 {
            stage_message(
                &mut staged,
                &mut live,
                from_agent(&format!("live{i:02}"), "t", Some(body.clone())),
            );
        }
        flush_messages(&mut staged, &mut live, &mut next_id, 1_000);
        let live_before: Vec<u64> = live.iter().map(|m| m.id).collect();
        let history = (0..20).map(|i| {
            let mut p = from_agent(&format!("hist{i:02}"), "t", Some(body.clone()));
            p.ts_ms = Some(i);
            p
        });
        let report = prepend_messages(&mut staged, &mut live, history, &mut next_id, 2_000);
        assert!(report.owned_bytes <= RETENTION_BUDGET_BYTES);
        assert_eq!(report.owned_bytes, owned(&staged, &live));
        let kept: Vec<&str> = live
            .iter()
            .filter_map(|m| m.agent_id.as_deref())
            .filter(|a| a.starts_with("hist"))
            .collect();
        assert!(!kept.is_empty() && kept.len() < 20, "{kept:?}");
        assert_eq!(kept.last(), Some(&"hist19"), "newest history kept");
        assert!(!kept.contains(&"hist00"), "oldest history evicted first");
        assert_eq!(report.evicted, 20 - kept.len());
        let live_after: Vec<u64> = live.iter().skip(kept.len()).map(|m| m.id).collect();
        assert_eq!(live_after, live_before, "no live record displaced");
    }

    /// R3: deduplication compares the whole body, not a preview.
    #[test]
    fn r3_bodies_sharing_a_preview_both_survive_and_an_exact_repeat_dedups() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        let prefix = "p".repeat(250);
        let first = format!("{prefix}A");
        let second = format!("{prefix}B");
        for body in [&first, &second, &second] {
            stage_message(
                &mut staged,
                &mut live,
                from_agent("a", body, Some(body.clone())),
            );
        }
        let report = flush_messages(&mut staged, &mut live, &mut next_id, 1);
        assert_eq!(report.deduplicated, 1);
        let bodies: Vec<&str> = live.iter().filter_map(|m| m.detail.as_deref()).collect();
        assert_eq!(bodies, [first.as_str(), second.as_str()]);
    }

    /// V1 (retention half): a 50 KiB multiline UTF-8 body with a suffix
    /// marker past byte 49,000 survives the live path and the history path
    /// whole, and the viewer document carries every byte.
    #[test]
    fn v1_a_50_kib_body_survives_live_and_history_whole() {
        let mut body = String::new();
        while body.len() < 49_100 {
            body.push_str("line \u{4e2d}\u{e9}\u{1f469}\u{200d}\u{1f4bb} \u{2014} ok\n");
        }
        body.push_str("SUFFIX-MARKER\n");
        while body.len() < 50 * 1024 {
            body.push('z');
        }
        assert!(body.find("SUFFIX-MARKER").unwrap() > 49_000);
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let mut next_id = 1;
        stage_message(
            &mut staged,
            &mut live,
            from_agent("a", "line", Some(body.clone())),
        );
        flush_messages(&mut staged, &mut live, &mut next_id, 5);
        let mut old = from_agent("b", "line", Some(body.clone()));
        old.ts_ms = Some(1);
        prepend_messages(&mut staged, &mut live, [old], &mut next_id, 6);
        assert_eq!(live.len(), 2);
        for message in &live {
            assert_eq!(message.detail.as_deref(), Some(body.as_str()));
            let document = super::super::message_document(message, 0, 0);
            assert_eq!(&*document.body, body.as_str());
        }
    }

    /// R2 at admission: the exact limit is kept; one byte more — reached by
    /// a multibyte character straddling the limit — becomes the explicit
    /// unavailable pair naming its size, never a prefix.
    #[test]
    fn r2_admission_keeps_the_exact_limit_and_rejects_a_multibyte_overflow_whole() {
        let mut staged = Vec::new();
        let mut live = Vec::new();
        let exact = format!("{}\u{e9}", "a".repeat(MAX_BODY_BYTES - 2));
        assert_eq!(exact.len(), MAX_BODY_BYTES);
        let over = format!("{}\u{e9}", "a".repeat(MAX_BODY_BYTES - 1));
        assert_eq!(over.len(), MAX_BODY_BYTES + 1);
        let kept = stage_message(
            &mut staged,
            &mut live,
            from_agent("a", "t", Some(exact.clone())),
        );
        assert_eq!(kept.rejected, 0);
        let refused = stage_message(&mut staged, &mut live, from_agent("b", "t", Some(over)));
        assert_eq!(refused.rejected, 1);
        assert_eq!(staged[0].detail.as_deref(), Some(exact.as_str()));
        assert_eq!(staged[1].detail.as_deref(), Some(""));
        assert_eq!(staged[1].text, unavailable_notice(MAX_BODY_BYTES + 1));
        let flushed = flush_messages(&mut staged, &mut live, &mut 1, 0);
        assert_eq!(flushed.rejected, 0, "stage already counted the rejection");
    }

    #[test]
    fn oversized_history_that_cannot_fit_counts_the_record_once() {
        for preformatted in [false, true] {
            let mut staged = Vec::new();
            let mut live = (0..MAX_MESSAGES)
                .map(|id| Message::new(id as u64, 0, "live", Token::Dim, MsgType::Output))
                .collect();
            let incoming = if preformatted {
                from_agent(
                    "a",
                    &unavailable_notice(MAX_BODY_BYTES + 1),
                    Some(String::new()),
                )
            } else {
                from_agent("a", "body", Some("x".repeat(MAX_BODY_BYTES + 1)))
            };
            let report = prepend_messages(&mut staged, &mut live, [incoming], &mut 1, 0);
            assert_eq!(report.rejected, 1);
            assert_eq!(live.len(), MAX_MESSAGES);
        }
    }
}
