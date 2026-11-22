//! Event rows of the file reference, written from the port's rules
//! (`orch_core::store::events`) and not from the test double: first bytes win
//! per sequence, reads are in sequence order, and a gap is keyed by its start.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ops::Bound::{Excluded, Unbounded};

use orch_core::store::StoreError;
use orch_core::store::events::{AppendResult, EventGap, PruneResult, SequencedEvent};
use orch_domain::RunEvent;
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

/// Every event and gap of one project, by run id.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Events {
    rows: BTreeMap<String, BTreeMap<u64, RunEvent>>,
    gaps: BTreeMap<String, BTreeMap<u64, u64>>,
}

fn bound(value: usize) -> Result<()> {
    if (1..=1_000).contains(&value) {
        Ok(())
    } else {
        Err(StoreError::InvalidEvents(
            "row bound must be in 1..=1000".into(),
        ))
    }
}

fn row(seq: &u64, event: &RunEvent) -> SequencedEvent {
    SequencedEvent {
        seq: *seq,
        event: event.clone(),
    }
}

impl Events {
    /// The whole batch is refused for a zero sequence, before any row is stored.
    pub(super) fn append(&mut self, run: &str, batch: &[SequencedEvent]) -> Result<AppendResult> {
        if batch.iter().any(|e| e.seq == 0) {
            return Err(StoreError::InvalidEvents(
                "sequence numbers start at 1".into(),
            ));
        }
        let mut result = AppendResult {
            stored: 0,
            skipped: 0,
        };
        if batch.is_empty() {
            return Ok(result);
        }
        let rows = self.rows.entry(run.to_owned()).or_default();
        for e in batch {
            match rows.entry(e.seq) {
                Entry::Vacant(slot) => {
                    slot.insert(e.event.clone());
                    result.stored += 1;
                }
                Entry::Occupied(_) => result.skipped += 1,
            }
        }
        Ok(result)
    }

    pub(super) fn read(&self, run: &str, after: u64, limit: usize) -> Result<Vec<SequencedEvent>> {
        bound(limit)?;
        Ok(self
            .rows
            .get(run)
            .into_iter()
            .flat_map(|rows| rows.range((Excluded(after), Unbounded)))
            .take(limit)
            .map(|(seq, event)| row(seq, event))
            .collect())
    }

    pub(super) fn tail(&self, run: &str, count: usize) -> Result<Vec<SequencedEvent>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        bound(count)?;
        let mut tail: Vec<_> = self
            .rows
            .get(run)
            .into_iter()
            .flat_map(|rows| rows.iter().rev())
            .take(count)
            .map(|(seq, event)| row(seq, event))
            .collect();
        tail.reverse();
        Ok(tail)
    }

    pub(super) fn gaps(&self, run: &str) -> Vec<EventGap> {
        self.gaps
            .get(run)
            .into_iter()
            .flatten()
            .map(|(after_seq, dropped)| EventGap {
                after_seq: *after_seq,
                dropped: *dropped,
            })
            .collect()
    }

    /// Whether a row was added: a repeat of a stored gap changes nothing.
    pub(super) fn gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> Result<bool> {
        if dropped == 0 || after_seq.checked_add(dropped).is_none() {
            return Err(StoreError::InvalidEvents(
                "gap must name a positive, non-overflowing interval".into(),
            ));
        }
        match self
            .gaps
            .entry(run.to_owned())
            .or_default()
            .entry(after_seq)
        {
            Entry::Vacant(slot) => {
                slot.insert(dropped);
                Ok(true)
            }
            Entry::Occupied(slot) if *slot.get() == dropped => Ok(false),
            Entry::Occupied(_) => Err(StoreError::InvalidEvents("conflicting gap count".into())),
        }
    }

    pub(super) fn prune(&mut self, run: &str, max_rows: usize) -> Result<PruneResult> {
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
