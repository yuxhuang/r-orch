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

    pub(super) fn open(
        &mut self,
        project: &str,
        facts: &AttentionFacts,
        at: i64,
    ) -> Result<Attention> {
        facts.validate(project, at)?;
        for row in self.rows.values() {
            if row.resolution.is_none()
                && row.facts.kind() == facts.kind()
                && row.facts.subject_id() == facts.subject_id()
            {
                return Ok(row.clone());
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
        Ok(created)
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
    ) -> Result<Attention> {
        check_id(identity)?;
        let mut changed = self.current(id, expected, at)?;
        if changed.acknowledgement.is_some() {
            return Ok(changed);
        }
        changed.revision = changed.revision.next()?;
        changed.acknowledgement = Some(Acknowledgement {
            identity: identity.to_owned(),
            at,
        });
        self.rows.insert(id, changed.clone());
        Ok(changed)
    }

    pub(super) fn resolve(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<Attention> {
        let mut changed = self.current(id, expected, at)?;
        reason.validate(changed.facts.kind())?;
        if changed.resolution.is_none() {
            changed.revision = changed.revision.next()?;
            changed.resolution = Some(Resolution { reason, at });
            self.rows.insert(id, changed.clone());
        }
        Ok(changed)
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
        let row = table.open("p1", &facts(), 20).unwrap();
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
        let row = table.open("p1", &facts(), 20).unwrap();
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
        let row = table.open("p1", &facts(), 20).unwrap();
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
