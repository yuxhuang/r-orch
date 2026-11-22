//! Test-only event rows. The enclosing unit of work checks lease authority.

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

use super::*;
use crate::store::StoreError;

#[derive(Clone, Default)]
pub struct EventState {
    rows: BTreeMap<String, BTreeMap<u64, RunEvent>>,
    gaps: BTreeMap<String, Vec<EventGap>>,
}

impl EventState {
    pub fn append(&mut self, run: &str, batch: &[SequencedEvent]) -> Result<AppendResult> {
        if batch.iter().any(|row| row.seq == 0) {
            return Err(StoreError::InvalidEvents(
                "sequence numbers start at 1".into(),
            ));
        }
        let rows = self.rows.entry(run.into()).or_default();
        let mut result = AppendResult {
            stored: 0,
            skipped: 0,
        };
        for row in batch {
            if let std::collections::btree_map::Entry::Vacant(entry) = rows.entry(row.seq) {
                entry.insert(row.event.clone());
                result.stored += 1;
            } else {
                result.skipped += 1;
            }
        }
        Ok(result)
    }

    pub fn read(&self, run: &str, after: u64, limit: usize) -> Result<Vec<SequencedEvent>> {
        bound(limit)?;
        Ok(self
            .rows
            .get(run)
            .into_iter()
            .flat_map(|rows| rows.range((Excluded(after), Unbounded)))
            .take(limit)
            .map(|(seq, event)| SequencedEvent {
                seq: *seq,
                event: event.clone(),
            })
            .collect())
    }

    pub fn tail(&self, run: &str, count: usize) -> Result<Vec<SequencedEvent>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        bound(count)?;
        let mut rows: Vec<_> = self
            .rows
            .get(run)
            .into_iter()
            .flat_map(|rows| rows.iter().rev())
            .take(count)
            .map(|(seq, event)| SequencedEvent {
                seq: *seq,
                event: event.clone(),
            })
            .collect();
        rows.reverse();
        Ok(rows)
    }

    pub fn gaps(&self, run: &str) -> Vec<EventGap> {
        self.gaps.get(run).cloned().unwrap_or_default()
    }

    pub fn gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> Result<()> {
        if dropped == 0 || after_seq.checked_add(dropped).is_none() {
            return Err(StoreError::InvalidEvents(
                "gap must name a positive, non-overflowing interval".into(),
            ));
        }
        let gaps = self.gaps.entry(run.into()).or_default();
        gaps.push(EventGap { after_seq, dropped });
        gaps.sort_by_key(|gap| (gap.after_seq, gap.dropped));
        Ok(())
    }

    pub fn prune(&mut self, run: &str, max_rows: usize) -> Result<PruneResult> {
        bound(max_rows)?;
        let Some(rows) = self.rows.get_mut(run) else {
            return Ok(PruneResult {
                deleted: 0,
                complete: true,
            });
        };
        let deleted = rows.len().min(max_rows);
        for _ in 0..deleted {
            rows.pop_first();
        }
        Ok(PruneResult {
            deleted,
            complete: rows.is_empty(),
        })
    }
}

fn bound(value: usize) -> Result<()> {
    if !(1..=1_000).contains(&value) {
        return Err(StoreError::InvalidEvents(
            "row bound must be in 1..=1000".into(),
        ));
    }
    Ok(())
}
