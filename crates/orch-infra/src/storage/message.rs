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
use std::time::Duration;

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

const MUTATION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

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

    /// Serialize the complete file/index operation across CLI processes and
    /// store instances. The stable lock inode is never removed. A detached
    /// transaction finishes even if its caller (e.g. the expiry task) is aborted
    /// between writing an entity and updating its index.
    async fn transaction<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T>> + Send,
    {
        let store = Self::new(self.paths.clone(), self.clock.clone());
        // Waiting is cancellable; only an acquired transaction is detached.
        // Never put a blocking lock acquisition in Tokio's blocking pool: it
        // cannot be aborted and runtime shutdown would wait for it forever.
        ensure_dir(&store.paths.messages_dir()).await?;
        let path = store.paths.messages_dir().join(".mutation.lock");
        let guard = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .await
            .map_err(|e| Error::io("Open message mutation lock", &path, e))?
            .into_std()
            .await;
        let deadline = tokio::time::Instant::now() + MUTATION_LOCK_TIMEOUT;
        let mut backoff = Duration::from_millis(1);
        loop {
            match guard.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(Error::io("Lock message mutations", &path, err));
                }
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(Error::io(
                    format!(
                        "Timed out after {} ms waiting for message mutation lock {}; retry the operation",
                        MUTATION_LOCK_TIMEOUT.as_millis(),
                        path.display()
                    ),
                    &path,
                    std::io::Error::from(std::io::ErrorKind::TimedOut),
                ));
            }
            tokio::time::sleep_until(deadline.min(now + backoff)).await;
            backoff = (backoff * 2).min(Duration::from_millis(5));
        }
        // No await between acquiring the lock and transferring it to the
        // owned task: caller cancellation cannot tear a file/index update.
        tokio::spawn(async move {
            let result = operation(store).await;
            drop(guard);
            result
        })
        .await
        .map_err(|e| Error::other(format!("Message transaction: {e}")))?
    }

    async fn list_unlocked(&self) -> Result<Vec<Message>> {
        let mut messages = self.index.read_index().await?;
        messages.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(messages)
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
        let message = message.clone();
        self.transaction(move |store| async move {
            write_json(&store.paths.message_path(&message.id)?, &message).await?;
            store
                .index
                .update_index(move |items| replace_at_end(items, message, |m| &m.id))
                .await
        })
        .await
    }

    async fn get(&self, id: &str) -> Result<Option<Message>> {
        read_json(&self.paths.message_path(id)?).await
    }

    /// Sorted by `created_at` ascending.
    async fn list(&self) -> Result<Vec<Message>> {
        // Even a read can rebuild the index, so it takes the writer lock too.
        self.transaction(|store| async move { store.list_unlocked().await })
            .await
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
        let id = id.to_owned();
        self.transaction(move |store| async move {
            // A missing id is a no-op, not an error.
            let Some(mut message) = store.get(&id).await? else {
                return Ok(());
            };
            message.status = MessageStatus::Delivered;
            message.delivered_at = Some(store.clock.now_iso());

            write_json(&store.paths.message_path(&id)?, &message).await?;
            store
                .index
                .update_index(move |items| replace_at_end(items, message, |m| &m.id))
                .await
        })
        .await
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.transaction(move |store| async move {
            store.remove_file(&id).await?;
            store
                .index
                .update_index(move |items| items.into_iter().filter(|m| m.id != id).collect())
                .await
        })
        .await
    }

    /// Delete every message that has expired **or** was delivered more than an
    /// hour ago, in one index rewrite. Returns how many went.
    async fn purge_expired(&self) -> Result<usize> {
        self.transaction(|store| async move {
            let now = store.clock.now_ms();
            let doomed: Vec<Message> = store
                .list_unlocked()
                .await?
                .into_iter()
                .filter(|m| {
                    let expired = store.is_expired(m);
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
                store.remove_file(&message.id).await?;
            }
            let gone: std::collections::HashSet<String> =
                doomed.iter().map(|m| m.id.clone()).collect();
            store
                .index
                .update_index(move |items| {
                    items
                        .into_iter()
                        .filter(|m| !gone.contains(&m.id))
                        .collect()
                })
                .await?;

            Ok(doomed.len())
        })
        .await
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
    async fn concurrent_stores_preserve_valid_messages_during_expiry_and_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let (store, clock) = store(temp.path());
        for n in 0..16 {
            store
                .save(&message(&format!("msg_ack{n}"), Some("cli@offline"), "t"))
                .await
                .unwrap();
            store
                .save(&Message {
                    expires_at: Some(orch_core::clock::iso_from_epoch_ms(clock.now_ms() - 1)),
                    ..message(&format!("msg_expired{n}"), Some("cli@removed"), "t")
                })
                .await
                .unwrap();
        }
        let mut tasks = Vec::new();
        for n in 0..32 {
            let other = FileMessageStore::new(store.paths.clone(), clock.clone());
            tasks.push(tokio::spawn(async move {
                other
                    .save(&message(&format!("msg_new{n}"), Some("cli@offline"), "t"))
                    .await
                    .unwrap();
            }));
        }
        for n in 0..16 {
            let other = FileMessageStore::new(store.paths.clone(), clock.clone());
            tasks.push(tokio::spawn(async move {
                other.mark_delivered(&format!("msg_ack{n}")).await.unwrap();
                other.purge_expired().await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let messages = store.list().await.unwrap();
        assert_eq!(messages.len(), 48);
        for m in messages {
            assert_eq!(store.get(&m.id).await.unwrap().as_ref(), Some(&m));
            if m.id.starts_with("msg_ack") {
                assert_eq!(m.status, MessageStatus::Delivered);
            }
        }
        for n in 0..16 {
            assert!(
                store
                    .get(&format!("msg_expired{n}"))
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn cancelling_a_caller_does_not_interrupt_its_file_index_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = store(temp.path());
        let other = FileMessageStore::new(store.paths.clone(), store.clock.clone());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            other
                .transaction(move |store| async move {
                    let msg = message("msg_inflight", Some("cli@offline"), "t");
                    write_json(&store.paths.message_path(&msg.id)?, &msg).await?;
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    store
                        .index
                        .update_index(move |items| replace_at_end(items, msg, |m| &m.id))
                        .await?;
                    done_tx.send(()).unwrap();
                    Ok(())
                })
                .await
        });
        started_rx.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release_tx.send(()).unwrap();
        done_rx.await.unwrap();
        assert_eq!(ids(&store.list().await.unwrap()), vec!["msg_inflight"]);
    }

    fn hold_mutation_lock(store: &FileMessageStore) -> std::fs::File {
        std::fs::create_dir_all(store.paths.messages_dir()).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.paths.messages_dir().join(".mutation.lock"))
            .unwrap();
        file.try_lock().unwrap();
        file
    }

    #[tokio::test]
    async fn contended_mutation_times_out_without_writing_and_can_be_retried() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = store(temp.path());
        let lock = hold_mutation_lock(&store);
        let msg = message("msg_retry", Some("cli@offline"), "t");
        let err = tokio::time::timeout(Duration::from_secs(7), store.save(&msg))
            .await
            .expect("mutation lock wait must be bounded")
            .unwrap_err();
        assert!(err.to_string().contains("Timed out after 5000 ms"));
        assert!(err.to_string().contains("retry the operation"));
        assert_eq!(
            err.path(),
            Some(store.paths.messages_dir().join(".mutation.lock").as_path())
        );
        assert!(
            matches!(err, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(store.get(&msg.id).await.unwrap().is_none());
        drop(lock);
        store.save(&msg).await.unwrap();
        assert_eq!(store.list().await.unwrap(), vec![msg]);
    }

    #[tokio::test]
    async fn cancelling_a_lock_waiter_discards_its_unstarted_operation() {
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = store(temp.path());
        let lock = hold_mutation_lock(&store);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut waiting = Box::pin(store.transaction(move |_| async move {
            let _ = started_tx.send(());
            Ok(())
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiting)
                .await
                .is_err()
        );
        drop(waiting);
        // The closure must be dropped with the waiter, before unlocking.
        // Otherwise an aborted request could mutate storage much later.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), started_rx)
                .await
                .expect("cancelled waiter retained its operation")
                .is_err()
        );
        drop(lock);
        assert!(store.list().await.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn contended_shutdown_child() {
        let Some(root) = std::env::var_os("ORCH_MESSAGE_LOCK_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            let (store, _) = store(&root);
            let mut caller = tokio::spawn(async move { store.purge_expired().await });
            tokio::select! {
                result = &mut caller => panic!("contended mutation finished before shutdown: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            std::fs::write(root.join("ready"), "waiting").unwrap();
            terminate.recv().await;
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        });
        // Exercise real runtime drop, which waits for non-abortable blocking
        // tasks. The parent keeps the external lock until after child exit.
        drop(runtime);
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_shutdown_exits_while_another_process_keeps_the_lock() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let (store, _) = store(temp.path());
        let lock = hold_mutation_lock(&store);
        let mut child = Child(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::message::tests::contended_shutdown_child",
                    "--nocapture",
                ])
                .env("ORCH_MESSAGE_LOCK_TEST_ROOT", temp.path())
                .env("HOME", temp.path())
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !temp.path().join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before becoming ready"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "child readiness deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.0.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "child shutdown failed: {status}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shutdown waited for external mutation lock"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lock);
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
