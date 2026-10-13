//! File-based message store — ported from
//! `src/infrastructure/storage/message-store.ts`.
//!
//! One JSON file per message plus an `_index.json`. `list` sorts by
//! `created_at` ascending, which is the one place the YAML timestamp hazard
//! would have bitten: the TS implementation calls `.localeCompare()` on that
//! field, and an unquoted timestamp read back as a `Date` makes it **throw**
//! (PARITY.md §6.4). Messages are JSON, so the hazard reaches them only
//! through a cross-binary write of a *task* or *agent* — the guard lives in
//! `write_yaml` either way.

use std::sync::Arc;

use async_trait::async_trait;
use orch_core::clock::Clock;
use orch_core::error::{Error, Result};
use orch_core::storage::MessageStore;
use orch_domain::{Message, MessageStatus};

use super::fs_utils::{ensure_dir, read_json, write_json};
use super::index::{IndexManager, ItemFormat, replace_at_end};
use super::paths::Paths;

/// A delivered message is purged once it is older than this.
const DELIVERED_RETENTION_MS: i64 = 3_600_000;

pub struct FileMessageStore {
    paths: Paths,
    index: IndexManager<Message>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FileMessageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileMessageStore")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl FileMessageStore {
    pub fn new(paths: Paths, clock: Arc<dyn Clock>) -> Self {
        let index = IndexManager::new(paths.messages_dir(), ItemFormat::Json);
        FileMessageStore {
            paths,
            index,
            clock,
        }
    }

    fn is_expired(&self, message: &Message) -> bool {
        let Some(expires_at) = &message.expires_at else {
            return false;
        };
        expires_at.as_str() < self.clock.now_iso().as_str()
    }

    async fn remove_file(&self, id: &str) -> Result<()> {
        let path = self.paths.message_path(id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::io(format!("delete {}", path.display()), &path, err)),
        }
    }
}

#[async_trait]
impl MessageStore for FileMessageStore {
    async fn save(&self, message: &Message) -> Result<()> {
        ensure_dir(&self.paths.messages_dir()).await?;
        write_json(&self.paths.message_path(&message.id)?, message).await?;
        let message = message.clone();
        self.index
            .update_index(move |items| replace_at_end(items, message, |m| &m.id))
            .await
    }

    async fn get(&self, id: &str) -> Result<Option<Message>> {
        read_json(&self.paths.message_path(id)?).await
    }

    /// Sorted by `created_at` ascending.
    async fn list(&self) -> Result<Vec<Message>> {
        let mut messages = self.index.read_index().await?;
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(messages)
    }

    /// Pending, unexpired, addressed to exactly this agent — so a broadcast
    /// with a `null` recipient is never "pending" for anyone.
    async fn list_pending(&self, agent_id: &str) -> Result<Vec<Message>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|m| {
                m.status == MessageStatus::Pending
                    && !self.is_expired(m)
                    && m.to_agent_id.as_deref() == Some(agent_id)
            })
            .collect())
    }

    async fn mark_delivered(&self, id: &str) -> Result<()> {
        // A missing id is a no-op, not an error.
        let Some(mut message) = self.get(id).await? else {
            return Ok(());
        };
        message.status = MessageStatus::Delivered;
        message.delivered_at = Some(self.clock.now_iso());

        write_json(&self.paths.message_path(id)?, &message).await?;
        self.index
            .update_index(move |items| replace_at_end(items, message, |m| &m.id))
            .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.remove_file(id).await?;
        let id = id.to_owned();
        self.index
            .update_index(move |items| items.into_iter().filter(|m| m.id != id).collect())
            .await
    }

    /// Delete every message that has expired **or** was delivered more than an
    /// hour ago, in one index rewrite. Returns how many went.
    async fn purge_expired(&self) -> Result<usize> {
        let now = self.clock.now_ms();
        let doomed: Vec<Message> = self
            .list()
            .await?
            .into_iter()
            .filter(|m| {
                let expired = self.is_expired(m);
                let stale_delivery = m
                    .delivered_at
                    .as_deref()
                    .and_then(parse_epoch_ms)
                    .is_some_and(|at| now - at > DELIVERED_RETENTION_MS);
                expired || stale_delivery
            })
            .collect();

        if doomed.is_empty() {
            return Ok(0);
        }

        for message in &doomed {
            self.remove_file(&message.id).await?;
        }
        let gone: std::collections::HashSet<String> = doomed.iter().map(|m| m.id.clone()).collect();
        self.index
            .update_index(move |items| {
                items
                    .into_iter()
                    .filter(|m| !gone.contains(&m.id))
                    .collect()
            })
            .await?;

        Ok(doomed.len())
    }
}

/// Epoch milliseconds from an ISO-8601 instant of the shape the clock writes
/// (`YYYY-MM-DDTHH:MM:SS.mmmZ`). `None` for anything else, which makes a
/// malformed `delivered_at` simply not purgeable rather than fatal.
fn parse_epoch_ms(iso: &str) -> Option<i64> {
    let bytes = iso.as_bytes();
    if bytes.len() < 20 || bytes[10] != b'T' {
        return None;
    }
    let num = |range: std::ops::Range<usize>| iso.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let millis = if bytes.get(19) == Some(&b'.') {
        num(20..23).unwrap_or(0)
    } else {
        0
    };

    let days = days_from_civil(year, month, day);
    Some(((days * 86_400) + hour * 3_600 + minute * 60 + second) * 1_000 + millis)
}

/// Howard Hinnant's `days_from_civil`: (y, m, d) → days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fs_utils::{clear_ensured_dirs, path_exists};
    use orch_core::test_support::FixedClock;
    use orch_domain::MessageChannel;
    use std::path::Path;

    fn store(root: &Path) -> (FileMessageStore, Arc<FixedClock>) {
        clear_ensured_dirs();
        let clock = Arc::new(FixedClock::at(1_760_000_000_000));
        let store = FileMessageStore::new(Paths::new(root), Arc::clone(&clock) as Arc<dyn Clock>);
        (store, clock)
    }

    fn message(id: &str, to: Option<&str>, created_at: &str) -> Message {
        Message {
            id: id.into(),
            channel: MessageChannel::Direct,
            from_agent_id: "cli".into(),
            to_agent_id: to.map(str::to_owned),
            subject: "Hi".into(),
            body: "hello there".into(),
            created_at: created_at.into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        }
    }

    fn ids(messages: &[Message]) -> Vec<&str> {
        messages.iter().map(|m| m.id.as_str()).collect()
    }

    #[tokio::test]
    async fn saves_reads_and_deletes_a_message() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        let msg = message("msg_1", Some("agt_1"), "2026-10-13T16:42:10.904Z");

        assert!(store.get("msg_1").await.unwrap().is_none());
        store.save(&msg).await.unwrap();
        assert_eq!(store.get("msg_1").await.unwrap().unwrap(), msg);

        store.delete("msg_1").await.unwrap();
        assert!(store.get("msg_1").await.unwrap().is_none());
        assert!(store.list().await.unwrap().is_empty());
        store.delete("msg_1").await.unwrap();
    }

    #[tokio::test]
    async fn a_broadcast_recipient_is_written_as_null() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store.save(&message("msg_1", None, "t")).await.unwrap();

        let raw = tokio::fs::read_to_string(store.paths.message_path("msg_1").unwrap())
            .await
            .unwrap();
        // The one field in the domain that emits null rather than being
        // omitted (PARITY.md §6.4).
        assert!(raw.contains("\"to_agent_id\": null"), "{raw}");
        assert_eq!(store.get("msg_1").await.unwrap().unwrap().to_agent_id, None);
    }

    #[tokio::test]
    async fn list_sorts_by_created_at_ascending() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        for (id, created) in [
            ("msg_c", "2026-10-13T16:42:12.000Z"),
            ("msg_a", "2026-10-13T16:42:10.000Z"),
            ("msg_b", "2026-10-13T16:42:11.000Z"),
        ] {
            store
                .save(&message(id, Some("agt_1"), created))
                .await
                .unwrap();
        }
        assert_eq!(
            ids(&store.list().await.unwrap()),
            vec!["msg_a", "msg_b", "msg_c"]
        );
    }

    #[tokio::test]
    async fn list_pending_filters_on_status_recipient_and_expiry() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());

        store
            .save(&message("msg_mine", Some("agt_1"), "t1"))
            .await
            .unwrap();
        store
            .save(&message("msg_theirs", Some("agt_2"), "t2"))
            .await
            .unwrap();
        store
            .save(&message("msg_broadcast", None, "t3"))
            .await
            .unwrap();
        store
            .save(&Message {
                status: MessageStatus::Delivered,
                ..message("msg_done", Some("agt_1"), "t4")
            })
            .await
            .unwrap();
        store
            .save(&Message {
                expires_at: Some(orch_core::clock::iso_from_epoch_ms(clock.now_ms() - 1)),
                ..message("msg_stale", Some("agt_1"), "t5")
            })
            .await
            .unwrap();

        // A broadcast has a null recipient, so it is pending for nobody.
        assert_eq!(
            ids(&store.list_pending("agt_1").await.unwrap()),
            vec!["msg_mine"]
        );
        assert_eq!(
            ids(&store.list_pending("agt_2").await.unwrap()),
            vec!["msg_theirs"]
        );
        assert!(store.list_pending("agt_nobody").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn mark_delivered_sets_the_status_and_timestamp_in_both_file_and_index() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        store
            .save(&message("msg_1", Some("agt_1"), "t"))
            .await
            .unwrap();

        store.mark_delivered("msg_1").await.unwrap();
        let expected_at = clock.now_iso();

        let from_file = store.get("msg_1").await.unwrap().unwrap();
        assert_eq!(from_file.status, MessageStatus::Delivered);
        assert_eq!(
            from_file.delivered_at.as_deref(),
            Some(expected_at.as_str())
        );

        // The index entry moved too, or `list` would still report it pending.
        let from_index = &store.list().await.unwrap()[0];
        assert_eq!(from_index.status, MessageStatus::Delivered);
        assert!(store.list_pending("agt_1").await.unwrap().is_empty());

        // A missing id is a no-op.
        store.mark_delivered("msg_nope").await.unwrap();
    }

    #[tokio::test]
    async fn purge_expired_removes_expired_and_long_delivered_messages() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());

        store
            .save(&message("msg_keep", Some("agt_1"), "t1"))
            .await
            .unwrap();
        store
            .save(&Message {
                expires_at: Some(orch_core::clock::iso_from_epoch_ms(clock.now_ms() + 60_000)),
                ..message("msg_future", Some("agt_1"), "t2")
            })
            .await
            .unwrap();
        store
            .save(&Message {
                expires_at: Some(orch_core::clock::iso_from_epoch_ms(clock.now_ms() - 1)),
                ..message("msg_expired", Some("agt_1"), "t3")
            })
            .await
            .unwrap();
        // Delivered an hour and a millisecond ago.
        store
            .save(&Message {
                status: MessageStatus::Delivered,
                delivered_at: Some(orch_core::clock::iso_from_epoch_ms(
                    clock.now_ms() - DELIVERED_RETENTION_MS - 1,
                )),
                ..message("msg_old_delivery", Some("agt_1"), "t4")
            })
            .await
            .unwrap();
        // Delivered just now — kept.
        store
            .save(&Message {
                status: MessageStatus::Delivered,
                delivered_at: Some(clock.now_iso()),
                ..message("msg_fresh_delivery", Some("agt_1"), "t5")
            })
            .await
            .unwrap();

        assert_eq!(store.purge_expired().await.unwrap(), 2);
        assert_eq!(
            ids(&store.list().await.unwrap()),
            vec!["msg_keep", "msg_future", "msg_fresh_delivery"]
        );
        for id in ["msg_expired", "msg_old_delivery"] {
            assert!(
                !path_exists(&store.paths.message_path(id).unwrap()).await,
                "{id}"
            );
        }
        // Idempotent.
        assert_eq!(store.purge_expired().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_deleted_index_is_rebuilt_and_excludes_itself() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store
            .save(&message("msg_1", Some("agt_1"), "t1"))
            .await
            .unwrap();
        store
            .save(&message("msg_2", Some("agt_1"), "t2"))
            .await
            .unwrap();
        tokio::fs::remove_file(store.index.index_path())
            .await
            .unwrap();

        assert_eq!(store.list().await.unwrap().len(), 2);
    }

    #[test]
    fn epoch_parsing_matches_the_clock_formatter() {
        for ms in [
            0i64,
            1_000,
            1_760_000_000_000,
            1_760_373_453_426,
            4_102_444_800_000,
        ] {
            let iso = orch_core::clock::iso_from_epoch_ms(ms);
            assert_eq!(parse_epoch_ms(&iso), Some(ms), "{iso}");
        }
        // A malformed value is simply not purgeable.
        assert_eq!(parse_epoch_ms("not a date"), None);
        assert_eq!(parse_epoch_ms(""), None);
        assert_eq!(parse_epoch_ms("2026-10-13"), None);
        // The no-fraction form the TS can also produce. `node -e
        // "Date.parse('2026-10-13T16:37:33Z')"` is 1791909453000.
        assert_eq!(
            parse_epoch_ms("2026-10-13T16:37:33Z"),
            Some(1_791_909_453_000)
        );
        assert_eq!(
            parse_epoch_ms("2025-10-13T16:37:33Z"),
            Some(1_760_373_453_000)
        );
    }

    #[tokio::test]
    async fn a_malformed_delivered_at_is_not_purged_rather_than_fatal() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        store
            .save(&Message {
                status: MessageStatus::Delivered,
                delivered_at: Some("whenever".into()),
                ..message("msg_1", Some("agt_1"), "t")
            })
            .await
            .unwrap();
        assert_eq!(store.purge_expired().await.unwrap(), 0);
        assert!(store.get("msg_1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn an_invalid_id_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _clock) = store(temp.path());
        assert!(store.get("a/b").await.is_err());
        assert!(store.delete("../escape").await.is_err());
    }
}
