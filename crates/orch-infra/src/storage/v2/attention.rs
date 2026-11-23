//! Persisted attention table of one explicit scope. Independent file reference;
//! the surrounding project document supplies commit, rollback and snapshots.

use orch_core::store::attention::*;
use orch_core::store::{Result, Revision, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttentionRows {
    issued: u64,
    rows: BTreeMap<u64, Attention>,
}

impl AttentionRows {
    pub(super) fn validate(&self, project: &str) -> Result<()> {
        if self.issued > i64::MAX as u64 {
            return Err(StoreError::InvalidAttention(
                "counter past SQLite integer range".into(),
            ));
        }
        let mut open = std::collections::BTreeSet::new();
        for (id, row) in &self.rows {
            if *id == 0 || *id != row.id || *id > self.issued {
                return Err(StoreError::InvalidAttention(
                    "invalid persisted id or counter".into(),
                ));
            }
            row.facts.validate(project, row.opened_at)?;
            if let Some(ack) = &row.acknowledgement {
                check_id(&ack.identity)?;
                if ack.at < row.opened_at {
                    return Err(StoreError::InvalidAttention("ack before open".into()));
                }
            }
            if let Some(resolved) = &row.resolution {
                resolved.reason.validate(row.facts.kind())?;
                if resolved.at < row.opened_at {
                    return Err(StoreError::InvalidAttention(
                        "resolution before open".into(),
                    ));
                }
            } else if !open.insert((format!("{:?}", row.facts.kind()), row.facts.subject_id())) {
                return Err(StoreError::InvalidAttention(
                    "duplicate open subject".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn get(&self, id: u64) -> Option<Attention> {
        self.rows.get(&id).cloned()
    }

    pub(super) fn page(
        &self,
        project: &str,
        filter: AttentionFilter,
        cursor: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        check_limit(limit)?;
        let start = match cursor {
            Some(c) => c.check(project, filter)?,
            None => 0,
        };
        let mut items = Vec::new();
        let mut more = false;
        for (id, row) in self
            .rows
            .range((std::ops::Bound::Excluded(start), std::ops::Bound::Unbounded))
        {
            if !filter.matches(row) {
                continue;
            }
            if items.len() == limit {
                more = true;
                break;
            }
            debug_assert_eq!(*id, row.id);
            items.push(row.clone());
        }
        let next = if more {
            items
                .last()
                .map(|r| AttentionCursor::after(project, filter, r.id))
        } else {
            None
        };
        Ok(AttentionPage { items, next })
    }

    /// The row now open for these facts and whether this call wrote anything: a
    /// duplicate open returns the existing row and leaves the table as it was.
    pub(super) fn open(
        &mut self,
        project: &str,
        facts: &AttentionFacts,
        at: i64,
    ) -> Result<(Attention, bool)> {
        facts.validate(project, at)?;
        for row in self.rows.values() {
            if row.resolution.is_none()
                && row.facts.kind() == facts.kind()
                && row.facts.subject_id() == facts.subject_id()
            {
                return Ok((row.clone(), false));
            }
        }
        let next = self
            .issued
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(StoreError::CounterExhausted)?;
        let created = Attention {
            id: next,
            facts: facts.clone(),
            opened_at: at,
            acknowledgement: None,
            resolution: None,
            revision: Revision::ONE,
        };
        self.rows.insert(next, created.clone());
        self.issued = next;
        Ok((created, true))
    }

    fn current(&self, id: u64, expected: Revision, at: i64) -> Result<Attention> {
        match self.rows.get(&id) {
            Some(row) if row.revision == expected => {
                row.check_transition_time(at)?;
                Ok(row.clone())
            }
            other => Err(StoreError::StaleAttentionRevision {
                id,
                expected,
                actual: other.map(|r| r.revision),
            }),
        }
    }

    pub(super) fn ack(
        &mut self,
        id: u64,
        expected: Revision,
        identity: &str,
        at: i64,
    ) -> Result<(Attention, bool)> {
        check_id(identity)?;
        let mut changed = self.current(id, expected, at)?;
        if changed.acknowledgement.is_some() {
            return Ok((changed, false));
        }
        changed.revision = changed.revision.next()?;
        changed.acknowledgement = Some(Acknowledgement {
            identity: identity.to_owned(),
            at,
        });
        self.rows.insert(id, changed.clone());
        Ok((changed, true))
    }

    pub(super) fn resolve(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<(Attention, bool)> {
        let mut changed = self.current(id, expected, at)?;
        reason.validate(changed.facts.kind())?;
        if changed.resolution.is_some() {
            return Ok((changed, false));
        }
        changed.revision = changed.revision.next()?;
        changed.resolution = Some(Resolution { reason, at });
        self.rows.insert(id, changed.clone());
        Ok((changed, true))
    }

    pub(super) fn expire(&mut self, cutoff: i64, limit: usize) -> Result<usize> {
        check_limit(limit)?;
        check_time(cutoff)?;
        let mut removed = 0;
        self.rows.retain(|_, row| {
            if removed < limit && row.resolution.as_ref().is_some_and(|r| r.at < cutoff) {
                removed += 1;
                false
            } else {
                true
            }
        });
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::v2::FileStorage;
    use orch_core::store::{Caller, Lane, Principal, Storage, StorageExt};

    fn caller() -> Caller {
        Caller {
            project: "p1".into(),
            principal: Principal::Owner {
                identity: "owner_1".into(),
            },
        }
    }
    fn facts() -> AttentionFacts {
        AttentionFacts::DispatchOffWithWork {
            waiting_tasks: 1,
            oldest_waiting_at: 10,
        }
    }

    #[tokio::test]
    async fn reopen_preserves_history_and_expiry_counter() {
        let root = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(root.path());
        let storage = FileStorage::new(root.path());
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                let r = u.open_attention(&facts(), 20)?;
                let r = u.ack_attention(r.id, r.revision, 30)?;
                let r =
                    u.resolve_attention(r.id, r.revision, ResolutionReason::DispatchEnabled, 40)?;
                assert_eq!(r.revision.get(), 3);
                Ok(())
            })
            .await
            .unwrap();
        drop(storage);
        let storage = FileStorage::new(root.path());
        let snap = storage.read(caller()).await.unwrap();
        let row = snap.read_attention(1).await.unwrap().unwrap();
        assert_eq!(row.acknowledgement.unwrap().identity, "owner_1");
        assert_eq!(
            row.resolution.unwrap().reason,
            ResolutionReason::DispatchEnabled
        );
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                assert_eq!(u.expire_attention(41, 1000)?, 1);
                Ok(())
            })
            .await
            .unwrap();
        drop(storage);
        let storage = FileStorage::new(root.path());
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                assert_eq!(u.open_attention(&facts(), 50)?.id, 2);
                Ok(())
            })
            .await
            .unwrap();
        assert!(snap.read_attention(1).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn legacy_document_defaults_to_empty_attention() {
        let root = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(root.path());
        let dir = root.path().join("p-7031");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("state.json"), r#"{"counters":{},"records":[]}"#).unwrap();
        let storage = FileStorage::new(root.path());
        assert!(
            storage
                .read(caller())
                .await
                .unwrap()
                .read_attention(1)
                .await
                .unwrap()
                .is_none()
        );
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                assert_eq!(u.open_attention(&facts(), 20)?.id, 1);
                Ok(())
            })
            .await
            .unwrap();
        drop(storage);
        assert!(
            FileStorage::new(root.path())
                .read(caller())
                .await
                .unwrap()
                .read_attention(1)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn outside_text_and_invalid_persisted_facts_are_refused() {
        let mut table = AttentionRows::default();
        let (row, _) = table.open("p1", &facts(), 20).unwrap();
        let mut json = serde_json::to_value(&table).unwrap();
        json["rows"]["1"]["title"] = serde_json::json!("$(touch canary); --yes");
        assert!(serde_json::from_value::<AttentionRows>(json).is_err());
        table.rows.get_mut(&row.id).unwrap().facts = AttentionFacts::DispatchOffWithWork {
            waiting_tasks: 0,
            oldest_waiting_at: 10,
        };
        assert!(table.validate("p1").is_err());
        let mut table = AttentionRows::default();
        table.open("p1", &facts(), 20).unwrap();
        table.issued = 0;
        assert!(table.validate("p1").is_err());
    }

    #[tokio::test]
    async fn reopen_preserves_typed_scopes_and_numeric_history() {
        let root = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(root.path());
        let storage = FileStorage::new(root.path());
        let history = storage
            .transaction(caller(), Lane::Maintenance, |u| {
                let mut rows = Vec::new();
                for id in 1..=11 {
                    let row = u.open_attention(&facts(), 20)?;
                    assert_eq!(row.id, id);
                    let row = u.ack_attention(id, row.revision, 30)?;
                    rows.push(u.resolve_attention(
                        id,
                        row.revision,
                        ResolutionReason::DispatchEnabled,
                        40,
                    )?);
                }
                Ok(rows)
            })
            .await
            .unwrap();
        let daemon = Caller {
            project: String::new(),
            ..caller()
        };
        let disks = storage
            .transaction(daemon.clone(), Lane::Maintenance, |u| {
                let mut rows = Vec::new();
                for subject in [
                    DiskSubject::DataDirectory,
                    DiskSubject::ProjectWorkspaces {
                        project: "p1".into(),
                    },
                ] {
                    rows.push(u.open_attention(
                        &AttentionFacts::DiskFloor {
                            subject,
                            free_bytes: 1,
                            floor_bytes: i64::MAX as u64,
                        },
                        20,
                    )?);
                }
                Ok(rows)
            })
            .await
            .unwrap();
        drop(storage);
        let storage = FileStorage::new(root.path());
        let snap = storage.read(caller()).await.unwrap();
        let mut cursor = None;
        let mut loaded = Vec::new();
        loop {
            let p = snap
                .read_attention_page(AttentionFilter::All, cursor.as_ref(), 3)
                .await
                .unwrap();
            loaded.extend(p.items);
            cursor = p.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(loaded, history);
        let global = storage.read(daemon).await.unwrap();
        assert_eq!(
            global
                .read_attention_page(AttentionFilter::All, None, 3)
                .await
                .unwrap()
                .items,
            disks
        );
        assert!(
            storage
                .read(Caller {
                    project: "p2".into(),
                    ..caller()
                })
                .await
                .unwrap()
                .read_attention(1)
                .await
                .unwrap()
                .is_none()
        );
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                assert_eq!(u.expire_attention(40, 3)?, 0);
                for first in [1, 4, 7, 10] {
                    assert_eq!(u.expire_attention(41, 3)?, if first == 10 { 2 } else { 3 });
                    for id in first..=(first + 2).min(11) {
                        assert!(u.get_attention(id)?.is_none());
                    }
                }
                Ok(())
            })
            .await
            .unwrap();
        drop(storage);
        let storage = FileStorage::new(root.path());
        storage
            .transaction(caller(), Lane::Maintenance, |u| {
                assert_eq!(u.open_attention(&facts(), 50)?.id, 12);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            snap.read_attention(11).await.unwrap(),
            Some(history[10].clone())
        );
    }

    #[test]
    fn duplicate_persisted_open_subjects_are_refused() {
        let mut table = AttentionRows::default();
        let (row, _) = table.open("p1", &facts(), 20).unwrap();
        let mut second = row;
        second.id = 2;
        table.rows.insert(2, second);
        table.issued = 2;
        let bytes = serde_json::to_vec(&table).unwrap();
        let loaded: AttentionRows = serde_json::from_slice(&bytes).unwrap();
        assert!(
            matches!(loaded.validate("p1"), Err(StoreError::InvalidAttention(message)) if message == "duplicate open subject")
        );
    }

    #[test]
    fn allocation_and_revision_overflow_leave_the_table_unchanged() {
        let mut table = AttentionRows {
            issued: i64::MAX as u64,
            ..Default::default()
        };
        assert!(matches!(
            table.open("p1", &facts(), 20),
            Err(StoreError::CounterExhausted)
        ));
        assert!(table.rows.is_empty());
        let mut table = AttentionRows::default();
        let (row, _) = table.open("p1", &facts(), 20).unwrap();
        let max: Revision = serde_json::from_value(serde_json::json!(u64::MAX)).unwrap();
        table.rows.get_mut(&row.id).unwrap().revision = max;
        assert!(matches!(
            table.ack(row.id, max, "owner_1", 30),
            Err(StoreError::CounterExhausted)
        ));
        let unchanged = table.get(row.id).unwrap();
        assert_eq!(unchanged.revision, max);
        assert!(unchanged.acknowledgement.is_none());
    }
}

/// The project document is replaced by a rename whenever a transaction wrote
/// something, so a new inode or new bytes is a write. A no-op must leave both.
#[cfg(test)]
mod unchanged_writes {
    use super::*;
    use crate::storage::v2::FileStorage;
    use orch_core::store::{Caller, Lane, Principal, Storage, StorageExt};
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    struct Fixture {
        root: tempfile::TempDir,
        storage: FileStorage,
    }

    fn caller() -> Caller {
        Caller {
            project: "p1".into(),
            principal: Principal::Owner {
                identity: "owner_1".into(),
            },
        }
    }

    fn facts() -> AttentionFacts {
        AttentionFacts::DispatchOffWithWork {
            waiting_tasks: 1,
            oldest_waiting_at: 10,
        }
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        orch_core::test_support::assert_hermetic(root.path());
        let storage = FileStorage::new(root.path());
        Fixture { root, storage }
    }

    impl Fixture {
        fn document(&self) -> PathBuf {
            // `p1` in hex: a project id is never a path.
            self.root.path().join("p-7031").join("state.json")
        }

        /// The document's inode and bytes.
        fn identity(&self) -> (u64, Vec<u8>) {
            let path = self.document();
            (
                std::fs::metadata(&path).unwrap().ino(),
                std::fs::read(&path).unwrap(),
            )
        }

        async fn run<T: Send + 'static>(
            &self,
            work: impl FnOnce(&mut dyn orch_core::store::UnitOfWork) -> Result<T> + Send + 'static,
        ) -> T {
            self.storage
                .transaction(caller(), Lane::Maintenance, work)
                .await
                .unwrap()
        }

        /// One open row at revision 3 (acknowledged, then resolved) and one open
        /// row at revision 1, committed.
        async fn seeded(&self) -> (Attention, Attention) {
            self.run(|u| {
                let first = u.open_attention(&facts(), 20)?;
                let first = u.ack_attention(first.id, first.revision, 30)?;
                let first = u.resolve_attention(
                    first.id,
                    first.revision,
                    ResolutionReason::DispatchEnabled,
                    40,
                )?;
                let second = u.open_attention(&facts(), 50)?;
                Ok((first, second))
            })
            .await
        }
    }

    #[tokio::test]
    async fn a_detector_that_sees_a_real_write_is_the_baseline() {
        let fx = fixture();
        let (_, open) = fx.seeded().await;
        let before = fx.identity();
        fx.run(move |u| u.ack_attention(open.id, open.revision, 60))
            .await;
        let after = fx.identity();
        assert_ne!(after.0, before.0, "a write replaces the document");
        assert_ne!(after.1, before.1);
    }

    #[tokio::test]
    async fn duplicate_open_leaves_the_document_untouched() {
        let fx = fixture();
        let (_, open) = fx.seeded().await;
        let before = fx.identity();
        let again = fx.run(|u| u.open_attention(&facts(), 70)).await;
        assert_eq!(again, open, "the existing open row, not a new one");
        assert_eq!(fx.identity(), before);
    }

    #[tokio::test]
    async fn repeated_ack_and_resolve_at_the_current_revision_leave_the_document_untouched() {
        let fx = fixture();
        let (resolved, _) = fx.seeded().await;
        assert_eq!(resolved.revision.get(), 3);
        let before = fx.identity();
        let (acked, resolved_again) = fx
            .run(move |u| {
                let acked = u.ack_attention(resolved.id, resolved.revision, 80)?;
                let again = u.resolve_attention(
                    resolved.id,
                    resolved.revision,
                    ResolutionReason::DispatchEnabled,
                    80,
                )?;
                Ok((acked, again))
            })
            .await;
        assert_eq!(acked, resolved);
        assert_eq!(resolved_again, resolved);
        assert_eq!(fx.identity(), before);
    }

    #[tokio::test]
    async fn expiry_that_removes_nothing_leaves_the_document_untouched() {
        let fx = fixture();
        fx.seeded().await;
        let before = fx.identity();
        // The one resolution is at 40 and is not before the cutoff 40; the other
        // row is still open.
        let removed = fx.run(|u| u.expire_attention(40, 1000)).await;
        assert_eq!(removed, 0);
        assert_eq!(fx.identity(), before);
    }

    #[tokio::test]
    async fn earlier_real_writes_in_one_transaction_still_persist() {
        let fx = fixture();
        let (resolved, open) = fx.seeded().await;
        let before = fx.identity();
        // Each no-op after a real write must not clear the pending write.
        let kept = fx
            .run(move |u| {
                let kept = u.resolve_attention(
                    open.id,
                    open.revision,
                    ResolutionReason::DispatchEnabled,
                    60,
                )?;
                assert_eq!(u.open_attention(&facts(), 70)?.id, 3);
                u.ack_attention(resolved.id, resolved.revision, 80)?;
                u.resolve_attention(
                    resolved.id,
                    resolved.revision,
                    ResolutionReason::DispatchEnabled,
                    80,
                )?;
                assert_eq!(u.expire_attention(0, 1000)?, 0);
                Ok(kept)
            })
            .await;
        let after = fx.identity();
        assert_ne!(after.0, before.0);
        assert_ne!(after.1, before.1);
        let loaded = FileStorage::new(fx.root.path());
        let snap = loaded.read(caller()).await.unwrap();
        assert_eq!(snap.read_attention(kept.id).await.unwrap(), Some(kept));
        assert!(snap.read_attention(3).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn no_ops_before_a_real_write_in_one_transaction_still_persist() {
        let fx = fixture();
        let (resolved, _) = fx.seeded().await;
        let before = fx.identity();
        fx.run(move |u| {
            u.ack_attention(resolved.id, resolved.revision, 80)?;
            assert_eq!(u.expire_attention(0, 1000)?, 0);
            assert_eq!(u.expire_attention(41, 1000)?, 1);
            Ok(())
        })
        .await;
        let after = fx.identity();
        assert_ne!(after.0, before.0);
        assert_ne!(after.1, before.1);
        let snap = FileStorage::new(fx.root.path())
            .read(caller())
            .await
            .unwrap();
        assert!(snap.read_attention(resolved.id).await.unwrap().is_none());
    }
}
