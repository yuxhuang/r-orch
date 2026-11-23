//! In-memory reference of one attention scope, cloned with its transaction.

use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct AttentionState {
    issued: u64,
    rows: BTreeMap<u64, Attention>,
}

impl AttentionState {
    pub fn get(&self, id: u64) -> Option<Attention> {
        self.rows.get(&id).cloned()
    }

    pub fn page(
        &self,
        project: &str,
        filter: AttentionFilter,
        cursor: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        check_limit(limit)?;
        let after = cursor
            .map(|c| c.check(project, filter))
            .transpose()?
            .unwrap_or(0);
        let mut items: Vec<_> = self
            .rows
            .values()
            .filter(|r| r.id > after && filter.matches(r))
            .take(limit + 1)
            .cloned()
            .collect();
        let more = items.len() > limit;
        items.truncate(limit);
        let next = if more {
            items
                .last()
                .map(|r| AttentionCursor::after(project, filter, r.id))
        } else {
            None
        };
        Ok(AttentionPage { items, next })
    }

    pub fn open(&mut self, project: &str, facts: &AttentionFacts, at: i64) -> Result<Attention> {
        facts.validate(project, at)?;
        if let Some(row) = self.rows.values().find(|r| {
            r.resolution.is_none()
                && r.facts.kind() == facts.kind()
                && r.facts.subject_id() == facts.subject_id()
        }) {
            return Ok(row.clone());
        }
        let id = self
            .issued
            .checked_add(1)
            .filter(|id| *id <= i64::MAX as u64)
            .ok_or(StoreError::CounterExhausted)?;
        let row = Attention {
            id,
            facts: facts.clone(),
            opened_at: at,
            acknowledgement: None,
            resolution: None,
            revision: Revision::ONE,
        };
        self.rows.insert(id, row.clone());
        self.issued = id;
        Ok(row)
    }

    fn checked(&self, id: u64, expected: Revision, at: i64) -> Result<Attention> {
        let row = self.get(id);
        if row.as_ref().map(|r| r.revision) != Some(expected) {
            return Err(StoreError::StaleAttentionRevision {
                id,
                expected,
                actual: row.map(|r| r.revision),
            });
        }
        let row = row.ok_or(StoreError::InvalidAttention("missing item".into()))?;
        row.check_transition_time(at)?;
        Ok(row)
    }

    pub fn ack(
        &mut self,
        id: u64,
        expected: Revision,
        identity: &str,
        at: i64,
    ) -> Result<Attention> {
        check_id(identity)?;
        let mut row = self.checked(id, expected, at)?;
        if row.acknowledgement.is_none() {
            row.revision = row.revision.next()?;
            row.acknowledgement = Some(Acknowledgement {
                identity: identity.into(),
                at,
            });
            self.rows.insert(id, row.clone());
        }
        Ok(row)
    }

    pub fn resolve(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<Attention> {
        let mut row = self.checked(id, expected, at)?;
        reason.validate(row.facts.kind())?;
        if row.resolution.is_none() {
            row.revision = row.revision.next()?;
            row.resolution = Some(Resolution { reason, at });
            self.rows.insert(id, row.clone());
        }
        Ok(row)
    }

    pub fn expire(&mut self, cutoff: i64, limit: usize) -> Result<usize> {
        check_time(cutoff)?;
        check_limit(limit)?;
        let ids: Vec<_> = self
            .rows
            .values()
            .filter(|r| r.resolution.as_ref().is_some_and(|v| v.at < cutoff))
            .take(limit)
            .map(|r| r.id)
            .collect();
        for id in &ids {
            self.rows.remove(id);
        }
        Ok(ids.len())
    }
}
