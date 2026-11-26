//! Operational methods on the original borrowed views. All SQL and format
//! policy belongs to the accepted leaves; these methods bind scope and observe
//! structured results before a callback can catch or publicly convert them.
use super::*;
use crate::sqlite::retry_message::{Retries, RetryReader};
use crate::sqlite::{attention, events, intents::Intents, leases, request_keys::RequestKeys};
use orch_core::store::attention::{
    Attention, AttentionCursor, AttentionFacts, AttentionFilter, AttentionPage, ResolutionReason,
};
use orch_core::store::events::{AppendResult, EventGap, PruneResult, SequencedEvent};
use orch_core::store::intents::{FinishOutcome, Intent, IntentPayload};
use orch_core::store::leases::{ClaimOutcome, Lease, NewLease};
use orch_core::store::request_keys::{KeyCheck, RequestScope};
use orch_core::store::retries::{Retry, RetryCursor, RetryPage};

impl EntityRead<'_, '_> {
    fn operation<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        self.active()?;
        self.observed(work())
    }

    pub(in crate::sqlite) fn get_attention(&self, id: u64) -> Result<Option<Attention>> {
        self.operation(|| attention::get(self.tx, &self.caller.project, id))
    }
    pub(in crate::sqlite) fn attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        self.operation(|| attention::page(self.tx, &self.caller.project, filter, after, limit))
    }
    pub(in crate::sqlite) fn get_intent(&self, id: u64) -> Result<Option<Intent>> {
        self.operation(|| Intents::new(self.tx, self.caller).get_intent(id))
    }
    pub(in crate::sqlite) fn list_pending_intents(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Intent>> {
        self.operation(|| Intents::new(self.tx, self.caller).list_pending_intents(after, limit))
    }
    pub(in crate::sqlite) fn get_lease(&self, task: &str) -> Result<Option<Lease>> {
        self.operation(|| leases::get_lease(self.tx, self.caller, task).map_err(Into::into))
    }
    pub(in crate::sqlite) fn get_agent_lease(&self, agent: &str) -> Result<Option<Lease>> {
        self.operation(|| leases::get_agent_lease(self.tx, self.caller, agent).map_err(Into::into))
    }
    pub(in crate::sqlite) fn list_leases(&self) -> Result<Vec<Lease>> {
        self.operation(|| leases::list_leases(self.tx, self.caller).map_err(Into::into))
    }
    pub(in crate::sqlite) fn read_events(
        &self,
        run: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<SequencedEvent>> {
        self.operation(|| {
            events::read_with_blobs(
                self.tx,
                &self.caller.project,
                run,
                after,
                limit,
                Some(self.disk),
            )
            .map_err(events::execution)
        })
    }
    pub(in crate::sqlite) fn read_events_tail(
        &self,
        run: &str,
        count: usize,
    ) -> Result<Vec<SequencedEvent>> {
        self.operation(|| {
            events::tail_with_blobs(self.tx, &self.caller.project, run, count, Some(self.disk))
                .map_err(events::execution)
        })
    }
    pub(in crate::sqlite) fn read_event_gaps(&self, run: &str) -> Result<Vec<EventGap>> {
        self.operation(|| {
            events::read_event_gaps(self.tx, &self.caller.project, run).map_err(events::execution)
        })
    }
    pub(in crate::sqlite) fn get_retry(&self, task: &str) -> Result<Option<Retry>> {
        self.operation(|| {
            RetryReader::new(self.tx, &self.caller.project, self.disk).read_retry(task)
        })
    }
    pub(in crate::sqlite) fn due_retries(&self, now: i64, limit: usize) -> Result<Vec<Retry>> {
        self.operation(|| {
            RetryReader::new(self.tx, &self.caller.project, self.disk).read_due_retries(now, limit)
        })
    }
    pub(in crate::sqlite) fn list_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryPage> {
        self.operation(|| {
            RetryReader::new(self.tx, &self.caller.project, self.disk).read_retries(after, limit)
        })
    }
}

impl EntityWrite<'_, '_> {
    fn operation<T>(&self, work: impl FnOnce(&EntityRead<'_, '_>) -> Result<T>) -> Result<T> {
        self.ensure_committable()?;
        self.read.observed(work(&self.read))
    }

    pub(in crate::sqlite) fn get_attention(&self, id: u64) -> Result<Option<Attention>> {
        self.ensure_committable()?;
        self.read.get_attention(id)
    }
    pub(in crate::sqlite) fn attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        self.ensure_committable()?;
        self.read.attention_page(filter, after, limit)
    }
    pub(in crate::sqlite) fn get_intent(&self, id: u64) -> Result<Option<Intent>> {
        self.ensure_committable()?;
        self.read.get_intent(id)
    }
    pub(in crate::sqlite) fn list_pending_intents(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<Vec<Intent>> {
        self.ensure_committable()?;
        self.read.list_pending_intents(after, limit)
    }
    pub(in crate::sqlite) fn get_lease(&self, task: &str) -> Result<Option<Lease>> {
        self.ensure_committable()?;
        self.read.get_lease(task)
    }
    pub(in crate::sqlite) fn get_agent_lease(&self, agent: &str) -> Result<Option<Lease>> {
        self.ensure_committable()?;
        self.read.get_agent_lease(agent)
    }
    pub(in crate::sqlite) fn list_leases(&self) -> Result<Vec<Lease>> {
        self.ensure_committable()?;
        self.read.list_leases()
    }
    pub(in crate::sqlite) fn read_events(
        &self,
        run: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<SequencedEvent>> {
        self.ensure_committable()?;
        self.read.read_events(run, after, limit)
    }
    pub(in crate::sqlite) fn read_events_tail(
        &self,
        run: &str,
        count: usize,
    ) -> Result<Vec<SequencedEvent>> {
        self.ensure_committable()?;
        self.read.read_events_tail(run, count)
    }
    pub(in crate::sqlite) fn read_event_gaps(&self, run: &str) -> Result<Vec<EventGap>> {
        self.ensure_committable()?;
        self.read.read_event_gaps(run)
    }
    pub(in crate::sqlite) fn get_retry(&self, task: &str) -> Result<Option<Retry>> {
        self.ensure_committable()?;
        self.read.get_retry(task)
    }
    pub(in crate::sqlite) fn due_retries(&self, now: i64, limit: usize) -> Result<Vec<Retry>> {
        self.ensure_committable()?;
        self.read.due_retries(now, limit)
    }
    pub(in crate::sqlite) fn list_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> Result<RetryPage> {
        self.ensure_committable()?;
        self.read.list_retries(after, limit)
    }
    pub(in crate::sqlite) fn open_attention(
        &mut self,
        facts: &AttentionFacts,
        at: i64,
    ) -> Result<Attention> {
        self.operation(|r| attention::open(r.tx, &r.caller.project, facts, at))
    }
    pub(in crate::sqlite) fn ack_attention(
        &mut self,
        id: u64,
        expected: Revision,
        at: i64,
    ) -> Result<Attention> {
        self.operation(|r| attention::ack(r.tx, r.caller, id, expected, at))
    }
    pub(in crate::sqlite) fn resolve_attention(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<Attention> {
        self.operation(|r| attention::resolve(r.tx, &r.caller.project, id, expected, reason, at))
    }
    pub(in crate::sqlite) fn expire_attention(
        &mut self,
        cutoff: i64,
        limit: usize,
    ) -> Result<usize> {
        self.operation(|r| attention::expire(r.tx, &r.caller.project, cutoff, limit))
    }
    pub(in crate::sqlite) fn add_intent(
        &mut self,
        payload: &IntentPayload,
        now: i64,
    ) -> Result<Intent> {
        self.operation(|r| Intents::new(r.tx, r.caller).add_intent(payload, now))
    }
    pub(in crate::sqlite) fn finish_intent(&mut self, id: u64, now: i64) -> Result<FinishOutcome> {
        self.operation(|r| Intents::new(r.tx, r.caller).finish_intent(id, now))
    }
    pub(in crate::sqlite) fn delete_finished_intents(
        &mut self,
        now: i64,
        limit: usize,
    ) -> Result<usize> {
        self.operation(|r| Intents::new(r.tx, r.caller).delete_finished_intents(now, limit))
    }
    pub(in crate::sqlite) fn claim_lease(
        &mut self,
        new: &NewLease,
        now: i64,
    ) -> Result<ClaimOutcome> {
        self.operation(|r| leases::claim_lease(r.tx, r.caller, new, now).map_err(Into::into))
    }
    pub(in crate::sqlite) fn renew_lease(
        &mut self,
        task: &str,
        lineage: &str,
        epoch: u64,
        now: i64,
    ) -> Result<Lease> {
        self.operation(|r| {
            leases::renew_lease(r.tx, r.caller, task, lineage, epoch, now).map_err(Into::into)
        })
    }
    pub(in crate::sqlite) fn release_lease(
        &mut self,
        task: &str,
        lineage: &str,
        epoch: u64,
    ) -> Result<Lease> {
        self.operation(|r| {
            leases::release_lease(r.tx, r.caller, task, lineage, epoch).map_err(Into::into)
        })
    }
    pub(in crate::sqlite) fn put_retry(&mut self, retry: &Retry, now: i64) -> Result<()> {
        self.operation(|r| Retries::new(r.tx, r.caller, r.disk).put_retry(retry, now))
    }
    pub(in crate::sqlite) fn delete_retry(&mut self, task: &str) -> Result<bool> {
        self.operation(|r| Retries::new(r.tx, r.caller, r.disk).delete_retry(task))
    }
    pub(in crate::sqlite) fn check_request_key(
        &self,
        scope: &RequestScope,
        op: &str,
        hash: &str,
        now: i64,
    ) -> Result<KeyCheck> {
        self.operation(|r| RequestKeys::new(r.tx, r.caller).check_request_key(scope, op, hash, now))
    }
    pub(in crate::sqlite) fn store_request_key(
        &mut self,
        scope: &RequestScope,
        op: &str,
        hash: &str,
        result: &str,
        now: i64,
    ) -> Result<()> {
        self.operation(|r| {
            RequestKeys::new(r.tx, r.caller).store_request_key(scope, op, hash, result, now)
        })
    }
    pub(in crate::sqlite) fn delete_expired_request_keys(
        &mut self,
        now: i64,
        limit: usize,
    ) -> Result<usize> {
        self.operation(|r| RequestKeys::new(r.tx, r.caller).delete_expired_request_keys(now, limit))
    }
}

impl EntityWrite<'_, '_> {
    // EventBatch's accepted backend-error rule also applies when the caller
    // catches the result or reconstructs a router in this same callback.
    fn event_operation<T>(
        &self,
        work: impl FnOnce(&EntityRead<'_, '_>) -> events::Result<T>,
    ) -> Result<T> {
        self.ensure_committable()?;
        let result = work(&self.read);
        if matches!(&result, Err(error) if !matches!(error, events::EventsError::Store(_))) {
            self.read.state.abort_transaction();
        }
        self.read.observed(result.map_err(events::execution))
    }

    pub(in crate::sqlite) fn append_events(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        batch: &[SequencedEvent],
    ) -> Result<AppendResult> {
        self.event_operation(|r| {
            events::append_with_blobs(r.tx, r.caller, run, lineage, epoch, batch, Some(r.disk))
        })
    }
    pub(in crate::sqlite) fn record_event_gap(
        &mut self,
        run: &str,
        after: u64,
        dropped: u64,
    ) -> Result<()> {
        self.event_operation(|r| {
            events::record_event_gap(r.tx, &r.caller.project, run, after, dropped)
        })
    }
    pub(in crate::sqlite) fn prune_events(
        &mut self,
        run: &str,
        limit: usize,
    ) -> Result<PruneResult> {
        self.event_operation(|r| events::prune_events(r.tx, &r.caller.project, run, limit))
    }
}
