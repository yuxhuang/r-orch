//! Explicit port forwarding into the original transaction and caller.
use super::*;
use orch_core::store::attention::*;
#[async_trait]
impl AttentionRead for Snapshot {
    async fn read_attention(&self, id: u64) -> store::Result<Option<Attention>> {
        self.call(move |view| view.router.get_attention(id)).await
    }
    async fn read_attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> store::Result<AttentionPage> {
        let after = after.cloned();
        self.call(move |view| view.router.attention_page(filter, after.as_ref(), limit))
            .await
    }
}
impl AttentionWrite for Unit<'_, '_> {
    fn get_attention(&self, id: u64) -> store::Result<Option<Attention>> {
        port(self.ensure_committable())?;
        let result = self.router.get_attention(id);
        self.observed(result)
    }
    fn attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> store::Result<AttentionPage> {
        port(self.ensure_committable())?;
        let result = self.router.attention_page(filter, after, limit);
        self.observed(result)
    }
    fn open_attention(&mut self, facts: &AttentionFacts, at: i64) -> store::Result<Attention> {
        port(self.ensure_committable())?;
        let result = self.router.open_attention(facts, at);
        self.observed(result)
    }
    fn ack_attention(&mut self, id: u64, expected: Revision, at: i64) -> store::Result<Attention> {
        port(self.ensure_committable())?;
        let result = self.router.ack_attention(id, expected, at);
        self.observed(result)
    }
    fn resolve_attention(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> store::Result<Attention> {
        port(self.ensure_committable())?;
        let result = self.router.resolve_attention(id, expected, reason, at);
        self.observed(result)
    }
    fn expire_attention(&mut self, cutoff: i64, limit: usize) -> store::Result<usize> {
        port(self.ensure_committable())?;
        let result = self.router.expire_attention(cutoff, limit);
        self.observed(result)
    }
}
use orch_core::store::events::*;
#[async_trait]
impl EventRead for Snapshot {
    async fn read_events(
        &self,
        run: &str,
        after_seq: u64,
        limit: usize,
    ) -> store::Result<Vec<SequencedEvent>> {
        let run = run.to_owned();
        self.call(move |view| view.router.read_events(&run, after_seq, limit))
            .await
    }
    async fn read_events_tail(
        &self,
        run: &str,
        count: usize,
    ) -> store::Result<Vec<SequencedEvent>> {
        let run = run.to_owned();
        self.call(move |view| view.router.read_events_tail(&run, count))
            .await
    }
    async fn read_event_gaps(&self, run: &str) -> store::Result<Vec<EventGap>> {
        let run = run.to_owned();
        self.call(move |view| view.router.read_event_gaps(&run))
            .await
    }
}
impl EventWrite for Unit<'_, '_> {
    fn append_events(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        batch: &[SequencedEvent],
    ) -> store::Result<AppendResult> {
        port(self.ensure_committable())?;
        let result = self.router.append_events(run, lineage, epoch, batch);
        self.observed(result)
    }
    fn record_event_gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> store::Result<()> {
        port(self.ensure_committable())?;
        let result = self.router.record_event_gap(run, after_seq, dropped);
        self.observed(result)
    }
    fn prune_events(&mut self, run: &str, max_rows: usize) -> store::Result<PruneResult> {
        port(self.ensure_committable())?;
        let result = self.router.prune_events(run, max_rows);
        self.observed(result)
    }
}
use orch_core::store::intents::*;
#[async_trait]
impl IntentRead for Snapshot {
    async fn read_intent(&self, id: u64) -> store::Result<Option<Intent>> {
        self.call(move |view| view.router.get_intent(id)).await
    }
    async fn read_pending_intents(&self, after: u64, limit: usize) -> store::Result<Vec<Intent>> {
        self.call(move |view| view.router.list_pending_intents(after, limit))
            .await
    }
}
impl IntentWrite for Unit<'_, '_> {
    fn add_intent(&mut self, payload: &IntentPayload, now_ms: i64) -> store::Result<Intent> {
        port(self.ensure_committable())?;
        let result = self.router.add_intent(payload, now_ms);
        self.observed(result)
    }
    fn finish_intent(&mut self, id: u64, now_ms: i64) -> store::Result<FinishOutcome> {
        port(self.ensure_committable())?;
        let result = self.router.finish_intent(id, now_ms);
        self.observed(result)
    }
    fn delete_finished_intents(&mut self, now_ms: i64, max_rows: usize) -> store::Result<usize> {
        port(self.ensure_committable())?;
        let result = self.router.delete_finished_intents(now_ms, max_rows);
        self.observed(result)
    }
    fn get_intent(&self, id: u64) -> store::Result<Option<Intent>> {
        port(self.ensure_committable())?;
        let result = self.router.get_intent(id);
        self.observed(result)
    }
    fn list_pending_intents(&self, after: u64, limit: usize) -> store::Result<Vec<Intent>> {
        port(self.ensure_committable())?;
        let result = self.router.list_pending_intents(after, limit);
        self.observed(result)
    }
}
use orch_core::store::leases::*;
#[async_trait]
impl LeaseRead for Snapshot {
    async fn read_lease(&self, task: &str) -> store::Result<Option<Lease>> {
        let task = task.to_owned();
        self.call(move |view| view.router.get_lease(&task)).await
    }
    async fn read_agent_lease(&self, agent: &str) -> store::Result<Option<Lease>> {
        let agent = agent.to_owned();
        self.call(move |view| view.router.get_agent_lease(&agent))
            .await
    }
    async fn read_leases(&self) -> store::Result<Vec<Lease>> {
        self.call(move |view| view.router.list_leases()).await
    }
}
impl LeaseWrite for Unit<'_, '_> {
    fn claim_lease(&mut self, new: &NewLease, now_ms: i64) -> store::Result<ClaimOutcome> {
        port(self.ensure_committable())?;
        let result = self.router.claim_lease(new, now_ms);
        self.observed(result)
    }
    fn renew_lease(
        &mut self,
        task: &str,
        lineage: &str,
        epoch: u64,
        now_ms: i64,
    ) -> store::Result<Lease> {
        port(self.ensure_committable())?;
        let result = self.router.renew_lease(task, lineage, epoch, now_ms);
        self.observed(result)
    }
    fn release_lease(&mut self, task: &str, lineage: &str, epoch: u64) -> store::Result<Lease> {
        port(self.ensure_committable())?;
        let result = self.router.release_lease(task, lineage, epoch);
        self.observed(result)
    }
    fn get_lease(&self, task: &str) -> store::Result<Option<Lease>> {
        port(self.ensure_committable())?;
        let result = self.router.get_lease(task);
        self.observed(result)
    }
    fn get_agent_lease(&self, agent: &str) -> store::Result<Option<Lease>> {
        port(self.ensure_committable())?;
        let result = self.router.get_agent_lease(agent);
        self.observed(result)
    }
    fn list_leases(&self) -> store::Result<Vec<Lease>> {
        port(self.ensure_committable())?;
        let result = self.router.list_leases();
        self.observed(result)
    }
}
use orch_core::store::request_keys::*;
impl RequestKeyWrite for Unit<'_, '_> {
    fn check_request_key(
        &self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> store::Result<KeyCheck> {
        port(self.ensure_committable())?;
        let result = self.router.check_request_key(scope, op, req_sha256, now_ms);
        self.observed(result)
    }
    fn store_request_key(
        &mut self,
        scope: &RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> store::Result<()> {
        port(self.ensure_committable())?;
        let result = self
            .router
            .store_request_key(scope, op, req_sha256, result, now_ms);
        self.observed(result)
    }
    fn delete_expired_request_keys(
        &mut self,
        now_ms: i64,
        max_rows: usize,
    ) -> store::Result<usize> {
        port(self.ensure_committable())?;
        let result = self.router.delete_expired_request_keys(now_ms, max_rows);
        self.observed(result)
    }
}
use orch_core::store::retries::*;
#[async_trait]
impl RetryRead for Snapshot {
    async fn read_retry(&self, task: &str) -> store::Result<Option<Retry>> {
        let task = task.to_owned();
        self.call(move |view| view.router.get_retry(&task)).await
    }
    async fn read_due_retries(&self, now_ms: i64, limit: usize) -> store::Result<Vec<Retry>> {
        self.call(move |view| view.router.due_retries(now_ms, limit))
            .await
    }
    async fn read_retries(
        &self,
        after: Option<&RetryCursor>,
        limit: usize,
    ) -> store::Result<RetryPage> {
        let after = after.cloned();
        self.call(move |view| view.router.list_retries(after.as_ref(), limit))
            .await
    }
}
impl RetryWrite for Unit<'_, '_> {
    fn put_retry(&mut self, retry: &Retry) -> store::Result<()> {
        port(self.ensure_committable())?;
        let result = self.router.put_retry(retry, self.now);
        self.observed(result)
    }
    fn delete_retry(&mut self, task: &str) -> store::Result<bool> {
        port(self.ensure_committable())?;
        let result = self.router.delete_retry(task);
        self.observed(result)
    }
    fn get_retry(&self, task: &str) -> store::Result<Option<Retry>> {
        port(self.ensure_committable())?;
        let result = self.router.get_retry(task);
        self.observed(result)
    }
    fn due_retries(&self, now_ms: i64, limit: usize) -> store::Result<Vec<Retry>> {
        port(self.ensure_committable())?;
        let result = self.router.due_retries(now_ms, limit);
        self.observed(result)
    }
    fn list_retries(&self, after: Option<&RetryCursor>, limit: usize) -> store::Result<RetryPage> {
        port(self.ensure_committable())?;
        let result = self.router.list_retries(after, limit);
        self.observed(result)
    }
}
