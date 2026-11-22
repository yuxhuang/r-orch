//! Batched run events (R61, R63, R132). Sequence numbers are assigned by the
//! collector before this port is called; storage never allocates or renumbers.

use async_trait::async_trait;
use orch_domain::RunEvent;

use super::Result;

#[cfg(feature = "test-support")]
pub mod fake;

#[derive(Debug, Clone, PartialEq)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: RunEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendResult {
    pub stored: usize,
    pub skipped: usize,
}

/// The missing interval is `(after_seq, after_seq + dropped]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventGap {
    pub after_seq: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneResult {
    pub deleted: usize,
    /// No event rows remain for this run; gaps are retained as loss records.
    pub complete: bool,
}

/// Every result is from the enclosing immutable read snapshot, in seq order.
#[async_trait]
pub trait EventRead: Send + Sync {
    /// Exclusive lower bound; limit must be in 1..=1,000.
    async fn read_events(
        &self,
        run: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<SequencedEvent>>;
    /// Last `count` events, still oldest first. Zero returns an empty tail.
    async fn read_events_tail(&self, run: &str, count: usize) -> Result<Vec<SequencedEvent>>;
    async fn read_event_gaps(&self, run: &str) -> Result<Vec<EventGap>>;
}

/// Operations share the enclosing unit of work and its caller/project.
pub trait EventWrite: Send {
    /// Checks the active lease even for an empty or wholly duplicate batch.
    /// First stored bytes win for each sequence; duplicates within a batch also
    /// count as skipped. Invalid sequences refuse the whole operation.
    fn append_events(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        batch: &[SequencedEvent],
    ) -> Result<AppendResult>;
    /// Records a positive missing interval; overflowing intervals are refused.
    fn record_event_gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> Result<()>;
    /// Deletes at most max_rows (1..=1,000), lowest sequence first. The caller
    /// selects runs eligible for retention; this operation never deletes a run.
    fn prune_events(&mut self, run: &str, max_rows: usize) -> Result<PruneResult>;
}
