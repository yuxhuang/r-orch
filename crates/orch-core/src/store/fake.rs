//! In-memory v2 test double, enabled only by `test-support`.
//!
//! Cursor tokens retain only query bindings and sort keys, never snapshots.
//! They deliberately have no persistent or cross-backend representation.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use orch_domain::{GoalStatus, MessageStatus, TaskStatus};
use tokio::sync::Mutex as AsyncMutex;

use super::attention::*;
use super::*;

type Rows = BTreeMap<(EntityKind, String), Record>;

#[derive(Clone, Default)]
struct Project {
    rows: Rows,
    attention: attention::fake::AttentionState,
    events: events::fake::EventState,
    intents: intents::fake::IntentState,
    leases: leases::fake::LeaseState,
    request_keys: request_keys::fake::KeyState,
    retries: retries::fake::RetryState,
    positions: BTreeMap<EntityKind, u64>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Number(i64),
    Position(u64),
    Text(String),
    Newest(Reverse<Option<String>>),
}

type Key = Vec<Part>;

struct Bookmark {
    project: String,
    filter: Filter,
    sort: Sort,
    key: Key,
}

#[derive(Default)]
struct Bookmarks {
    next: u64,
    entries: BTreeMap<String, Bookmark>,
}

/// Serializable writers and immutable snapshots, with no filesystem or clock.
pub struct FakeStorage {
    projects: AsyncMutex<BTreeMap<String, Project>>,
    bookmarks: Arc<Mutex<Bookmarks>>,
    /// Daemon-wide, so it lives beside the projects. Lock `projects` first.
    lineage: Mutex<String>,
    retry_state: retries::fake::RetryState,
}

impl Default for FakeStorage {
    fn default() -> Self {
        Self {
            projects: AsyncMutex::default(),
            bookmarks: Arc::default(),
            lineage: Mutex::new("fake-lineage-1".into()),
            retry_state: retries::fake::RetryState::default(),
        }
    }
}

struct View {
    caller: Caller,
    lineage: String,
    project: Project,
    bookmarks: Arc<Mutex<Bookmarks>>,
}

#[async_trait]
impl Storage for FakeStorage {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>> {
        let project = self
            .projects
            .lock()
            .await
            .get(&caller.project)
            .cloned()
            .unwrap_or_else(|| Project {
                retries: self.retry_state.clone(),
                ..Project::default()
            });
        let lineage = self.lineage();
        Ok(Box::new(View {
            caller,
            lineage,
            project,
            bookmarks: self.bookmarks.clone(),
        }))
    }

    async fn write(&self, caller: Caller, _lane: Lane, work: WriteWork) -> Result<()> {
        let mut projects = self.projects.lock().await;
        let project = projects
            .get(&caller.project)
            .cloned()
            .unwrap_or_else(|| Project {
                retries: self.retry_state.clone(),
                ..Project::default()
            });
        let mut view = View {
            caller,
            lineage: self.lineage(),
            project,
            bookmarks: self.bookmarks.clone(),
        };
        work(&mut view)?;
        projects.insert(view.caller.project, view.project);
        Ok(())
    }
}

impl FakeStorage {
    /// Use a blob port for retry backing and publication/damage fault proofs.
    pub fn with_retry_blob_store<B: blob::BlobStore + 'static>(
        blobs: B,
        hash: fn(&[u8]) -> B::Hash,
        chunk_bytes: usize,
    ) -> Self {
        Self {
            retry_state: retries::fake::RetryState::with_blob_store(blobs, hash, chunk_bytes),
            ..Self::default()
        }
    }

    fn lineage(&self) -> String {
        self.lineage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Fixture authority only: stands for a restore, which starts a new lineage,
    /// ends every lease and keeps every epoch counter (R77, R138).
    pub async fn restore_to_lineage(&self, lineage: &str) {
        let mut projects = self.projects.lock().await;
        for project in projects.values_mut() {
            project.leases.end_all();
        }
        *self.lineage.lock().unwrap_or_else(PoisonError::into_inner) = lineage.into();
    }
}

impl View {
    fn get_row(&self, kind: EntityKind, id: &str) -> Option<Record> {
        self.project.rows.get(&(kind, id.into())).cloned()
    }

    fn checked(&self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record> {
        let row = self.get_row(kind, id);
        if row.as_ref().map(|r| r.revision) != Some(expected) {
            return Err(StoreError::StaleRevision {
                kind,
                id: id.into(),
                expected,
                actual: row.map(|r| r.revision),
            });
        }
        row.ok_or(StoreError::StaleRevision {
            kind,
            id: id.into(),
            expected,
            actual: None,
        })
    }

    fn next_position(&self, kind: EntityKind) -> Result<u64> {
        self.project
            .positions
            .get(&kind)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)
    }

    fn store_row(&mut self, row: Record) -> Record {
        self.project
            .rows
            .insert((row.entity.kind(), row.entity.id().into()), row.clone());
        row
    }

    fn page(&self, filter: &Filter, request: &PageRequest) -> Result<Page<Record>> {
        request.validate()?;
        validate_sort(filter, request.sort)?;
        let after = if let Some(cursor) = &request.cursor {
            let bookmarks = self
                .bookmarks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let bookmark = bookmarks
                .entries
                .get(cursor.as_str())
                .ok_or(StoreError::InvalidCursor)?;
            if bookmark.project != self.caller.project
                || &bookmark.filter != filter
                || bookmark.sort != request.sort
            {
                return Err(StoreError::InvalidCursor);
            }
            Some(bookmark.key.clone())
        } else {
            None
        };
        let mut rows: Vec<_> = self
            .project
            .rows
            .values()
            .filter(|row| matches_filter(&row.entity, filter))
            .map(|row| (sort_key(row, request.sort), row.clone()))
            .collect();
        let total = request.with_count.then_some(rows.len() as u64);
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(key) = after {
            rows.retain(|(k, _)| k > &key);
        }
        let more = rows.len() > request.limit;
        rows.truncate(request.limit);
        let next_cursor = if more {
            let mut bookmarks = self
                .bookmarks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            bookmarks.next = bookmarks
                .next
                .checked_add(1)
                .ok_or(StoreError::CounterExhausted)?;
            let token = format!("fake:{}", bookmarks.next);
            let key = rows.last().ok_or(StoreError::InvalidCursor)?.0.clone();
            bookmarks.entries.insert(
                token.clone(),
                Bookmark {
                    project: self.caller.project.clone(),
                    filter: filter.clone(),
                    sort: request.sort,
                    key,
                },
            );
            Some(Cursor::new(token)?)
        } else {
            None
        };
        Ok(Page {
            items: rows.into_iter().map(|(_, row)| row).collect(),
            next_cursor,
            total,
        })
    }
}

#[async_trait]
impl ReadSnapshot for View {
    fn caller(&self) -> &Caller {
        &self.caller
    }
    async fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        Ok(self.get_row(kind, id))
    }
    async fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>> {
        self.page(filter, page)
    }
}

impl UnitOfWork for View {
    fn caller(&self) -> &Caller {
        &self.caller
    }
    fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        Ok(self.get_row(kind, id))
    }
    fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>> {
        self.page(filter, page)
    }

    fn create(&mut self, entity: Entity) -> Result<Record> {
        let (kind, id) = (entity.kind(), entity.id());
        if self.get_row(kind, id).is_some() {
            return Err(StoreError::AlreadyExists {
                kind,
                id: id.into(),
            });
        }
        if kind == EntityKind::Task
            && let Some(row) = self
                .project
                .rows
                .values()
                .find(|r| r.entity.kind() == kind && r.entity.id().eq_ignore_ascii_case(id))
        {
            return Err(StoreError::IdAlias {
                existing: row.entity.id().into(),
                requested: id.into(),
            });
        }
        let position = self.next_position(kind)?;
        self.project.positions.insert(kind, position);
        Ok(self.store_row(Record {
            entity,
            revision: Revision::ONE,
            position,
        }))
    }

    fn update(&mut self, entity: Entity, expected: Revision) -> Result<Record> {
        let old = self.checked(entity.kind(), entity.id(), expected)?;
        let revision = old.revision.next()?;
        Ok(self.store_row(Record {
            entity,
            revision,
            position: old.position,
        }))
    }

    fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<()> {
        self.checked(kind, id, expected)?;
        self.project.rows.remove(&(kind, id.into()));
        Ok(())
    }

    fn move_to_end(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record> {
        let old = self.checked(kind, id, expected)?;
        let revision = old.revision.next()?;
        let position = self.next_position(kind)?;
        self.project.positions.insert(kind, position);
        Ok(self.store_row(Record {
            revision,
            position,
            ..old
        }))
    }
}

fn matches_filter(entity: &Entity, filter: &Filter) -> bool {
    match (entity, filter) {
        (Entity::Task(v), Filter::Tasks { status, goal }) => {
            status.is_none_or(|s| s == v.status)
                && goal.as_ref().is_none_or(|g| v.goal_id.as_ref() == Some(g))
        }
        (Entity::Goal(v), Filter::Goals { status }) => status.is_none_or(|s| s == v.status),
        (Entity::Agent(_), Filter::Agents)
        | (Entity::Team(_), Filter::Teams)
        | (Entity::Context(_), Filter::Context)
        | (Entity::Config(_), Filter::Config)
        | (Entity::Template(_), Filter::Templates) => true,
        (Entity::Run(v), Filter::Runs { task, agent }) => {
            task.as_ref().is_none_or(|t| t == &v.task_id)
                && agent.as_ref().is_none_or(|a| a == &v.agent_id)
        }
        (Entity::Message(v), Filter::Messages { recipient }) => recipient
            .as_ref()
            .is_none_or(|r| v.to_agent_id.as_ref() == Some(r)),
        (Entity::Message(v), Filter::PendingMessages { recipient, now }) => {
            v.status == MessageStatus::Pending
                && v.to_agent_id.as_ref() == Some(recipient)
                && v.expires_at.as_ref().is_none_or(|expiry| expiry >= now)
        }
        _ => false,
    }
}

fn validate_sort(filter: &Filter, sort: Sort) -> Result<()> {
    let kind = filter.kind();
    if sort == Sort::Attempt {
        return if matches!(filter, Filter::Runs { task: Some(_), .. }) {
            Ok(())
        } else {
            Err(StoreError::InvalidPage(
                "Attempt requires a task-scoped Runs filter".into(),
            ))
        };
    }
    let valid = matches!(sort, Sort::Default | Sort::Id)
        || match kind {
            EntityKind::Task => matches!(
                sort,
                Sort::Position | Sort::Priority | Sort::CreatedAt | Sort::UpdatedAt
            ),
            EntityKind::Goal => sort == Sort::CreatedAt,
            EntityKind::Agent | EntityKind::Team => sort == Sort::Name,
            EntityKind::Run
            | EntityKind::Message
            | EntityKind::Context
            | EntityKind::Config
            | EntityKind::Template => false,
        };
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidPage(format!(
            "{sort:?} is not supported for {kind:?}"
        )))
    }
}

fn sort_key(row: &Record, sort: Sort) -> Key {
    use Part::*;
    let mut key = match (sort, &row.entity) {
        (Sort::Id, _) => vec![],
        (Sort::Position, _) => vec![Position(row.position)],
        (Sort::Priority, Entity::Task(v)) => vec![Number(v.priority), Position(row.position)],
        (Sort::CreatedAt, Entity::Task(v)) => {
            vec![Text(v.created_at.clone()), Position(row.position)]
        }
        (Sort::UpdatedAt, Entity::Task(v)) => vec![
            Newest(Reverse(Some(v.updated_at.clone()))),
            Position(row.position),
        ],
        (Sort::CreatedAt, Entity::Goal(v)) => {
            vec![Text(v.created_at.clone()), Position(row.position)]
        }
        (Sort::Name, Entity::Agent(v)) => vec![Text(v.name.clone()), Position(row.position)],
        (Sort::Name, Entity::Team(v)) => vec![Text(v.name.clone())],
        (_, Entity::Task(v)) => vec![
            Number(match v.status {
                TaskStatus::InProgress => 0,
                TaskStatus::Retrying => 1,
                TaskStatus::Review => 2,
                TaskStatus::Todo => 3,
                TaskStatus::Done => 4,
                TaskStatus::Failed => 5,
                TaskStatus::Cancelled => 6,
            }),
            Newest(Reverse(Some(v.updated_at.clone()))),
            Position(row.position),
        ],
        (_, Entity::Goal(v)) => vec![
            Number(match v.status {
                GoalStatus::Active => 0,
                GoalStatus::Paused => 1,
                GoalStatus::Achieved => 2,
                GoalStatus::Abandoned => 3,
            }),
            Newest(Reverse(v.updated_at.clone())),
            Position(row.position),
        ],
        (_, Entity::Agent(_)) => vec![Position(row.position)],
        (_, Entity::Team(v)) => vec![Text(v.created_at.clone())],
        (Sort::Attempt, Entity::Run(v)) => vec![Number(-i64::from(v.attempt))],
        (_, Entity::Run(v)) => vec![Newest(Reverse(Some(v.started_at.clone())))],
        (_, Entity::Message(v)) => vec![Text(v.created_at.clone()), Position(row.position)],
        (_, Entity::Context(_) | Entity::Config(_) | Entity::Template(_)) => vec![],
    };
    key.push(Text(row.entity.id().into()));
    key
}

#[async_trait]
impl leases::LeaseRead for View {
    async fn read_lease(&self, task: &str) -> Result<Option<leases::Lease>> {
        Ok(self.project.leases.by_task(task))
    }
    async fn read_agent_lease(&self, agent: &str) -> Result<Option<leases::Lease>> {
        Ok(self.project.leases.by_agent(agent))
    }
    async fn read_leases(&self) -> Result<Vec<leases::Lease>> {
        Ok(self.project.leases.all())
    }
}

impl leases::LeaseWrite for View {
    fn claim_lease(&mut self, new: &leases::NewLease, now_ms: i64) -> Result<leases::ClaimOutcome> {
        self.project.leases.claim(&self.lineage, new, now_ms)
    }
    fn renew_lease(
        &mut self,
        task: &str,
        lineage: &str,
        epoch: u64,
        now_ms: i64,
    ) -> Result<leases::Lease> {
        self.project
            .leases
            .renew(&self.caller, &self.lineage, task, lineage, epoch, now_ms)
    }
    fn release_lease(&mut self, task: &str, lineage: &str, epoch: u64) -> Result<leases::Lease> {
        self.project
            .leases
            .release(&self.caller, &self.lineage, task, lineage, epoch)
    }
    fn get_lease(&self, task: &str) -> Result<Option<leases::Lease>> {
        Ok(self.project.leases.by_task(task))
    }
    fn get_agent_lease(&self, agent: &str) -> Result<Option<leases::Lease>> {
        Ok(self.project.leases.by_agent(agent))
    }
    fn list_leases(&self) -> Result<Vec<leases::Lease>> {
        Ok(self.project.leases.all())
    }
}

#[async_trait]
impl events::EventRead for View {
    async fn read_events(
        &self,
        run: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<events::SequencedEvent>> {
        self.project.events.read(run, after_seq, limit)
    }
    async fn read_events_tail(
        &self,
        run: &str,
        count: usize,
    ) -> Result<Vec<events::SequencedEvent>> {
        self.project.events.tail(run, count)
    }
    async fn read_event_gaps(&self, run: &str) -> Result<Vec<events::EventGap>> {
        Ok(self.project.events.gaps(run))
    }
}

impl events::EventWrite for View {
    fn append_events(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        batch: &[events::SequencedEvent],
    ) -> Result<events::AppendResult> {
        self.project.leases.check(
            &self.caller,
            &self.lineage,
            leases::fake::By::Run(run),
            lineage,
            epoch,
        )?;
        self.project.events.append(run, batch)
    }
    fn record_event_gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> Result<()> {
        self.project.events.gap(run, after_seq, dropped)
    }
    fn prune_events(&mut self, run: &str, max_rows: usize) -> Result<events::PruneResult> {
        self.project.events.prune(run, max_rows)
    }
}

#[async_trait]
impl intents::IntentRead for View {
    async fn read_intent(&self, id: u64) -> Result<Option<intents::Intent>> {
        Ok(self.project.intents.get(id))
    }
    async fn read_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<intents::Intent>> {
        self.project.intents.pending(after, limit)
    }
}

impl intents::IntentWrite for View {
    fn add_intent(
        &mut self,
        payload: &intents::IntentPayload,
        now_ms: i64,
    ) -> Result<intents::Intent> {
        payload.validate()?;
        if let Some(run) = payload.run() {
            // The lease of that run, as this unit of work sees it, under the
            // current lineage; a task principal must be its holder.
            let lease = self
                .project
                .leases
                .all()
                .into_iter()
                .find(|lease| lease.run == run)
                .ok_or(StoreError::StaleEpoch)?;
            self.project.leases.check(
                &self.caller,
                &self.lineage,
                leases::fake::By::Task(&lease.task),
                &lease.lineage,
                lease.epoch,
            )?;
        }
        self.project.intents.add(payload, now_ms)
    }
    fn finish_intent(&mut self, id: u64, now_ms: i64) -> Result<intents::FinishOutcome> {
        self.project.intents.finish(id, now_ms)
    }
    fn delete_finished_intents(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        self.project.intents.delete_finished(now_ms, max_rows)
    }
    fn get_intent(&self, id: u64) -> Result<Option<intents::Intent>> {
        Ok(self.project.intents.get(id))
    }
    fn list_pending_intents(&self, after: u64, limit: usize) -> Result<Vec<intents::Intent>> {
        self.project.intents.pending(after, limit)
    }
}

impl request_keys::RequestKeyWrite for View {
    fn check_request_key(
        &self,
        scope: &request_keys::RequestScope,
        op: &str,
        req_sha256: &str,
        now_ms: i64,
    ) -> Result<request_keys::KeyCheck> {
        scope.check_caller(&self.caller)?;
        self.project
            .request_keys
            .check(scope, op, req_sha256, now_ms)
    }
    fn store_request_key(
        &mut self,
        scope: &request_keys::RequestScope,
        op: &str,
        req_sha256: &str,
        result: &str,
        now_ms: i64,
    ) -> Result<()> {
        scope.check_caller(&self.caller)?;
        self.project
            .request_keys
            .store(scope, op, req_sha256, result, now_ms)
    }
    fn delete_expired_request_keys(&mut self, now_ms: i64, max_rows: usize) -> Result<usize> {
        self.project.request_keys.delete_expired(now_ms, max_rows)
    }
}

#[async_trait]
impl retries::RetryRead for View {
    async fn read_retry(&self, task: &str) -> Result<Option<retries::Retry>> {
        self.project.retries.get(task)
    }
    async fn read_due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<retries::Retry>> {
        self.project.retries.due(now_ms, limit)
    }
    async fn read_retries(
        &self,
        after: Option<&retries::RetryCursor>,
        limit: usize,
    ) -> Result<retries::RetryPage> {
        self.project.retries.page(after, limit)
    }
}

impl retries::RetryWrite for View {
    fn put_retry(&mut self, retry: &retries::Retry) -> Result<()> {
        self.project.retries.put(retry)
    }
    fn delete_retry(&mut self, task: &str) -> Result<bool> {
        Ok(self.project.retries.delete(task))
    }
    fn get_retry(&self, task: &str) -> Result<Option<retries::Retry>> {
        self.project.retries.get(task)
    }
    fn due_retries(&self, now_ms: i64, limit: usize) -> Result<Vec<retries::Retry>> {
        self.project.retries.due(now_ms, limit)
    }
    fn list_retries(
        &self,
        after: Option<&retries::RetryCursor>,
        limit: usize,
    ) -> Result<retries::RetryPage> {
        self.project.retries.page(after, limit)
    }
}

#[async_trait]
impl AttentionRead for View {
    async fn read_attention(&self, id: u64) -> Result<Option<Attention>> {
        Ok(self.project.attention.get(id))
    }
    async fn read_attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        self.project
            .attention
            .page(&self.caller.project, filter, after, limit)
    }
}
impl AttentionWrite for View {
    fn get_attention(&self, id: u64) -> Result<Option<Attention>> {
        Ok(self.project.attention.get(id))
    }
    fn attention_page(
        &self,
        filter: AttentionFilter,
        after: Option<&AttentionCursor>,
        limit: usize,
    ) -> Result<AttentionPage> {
        self.project
            .attention
            .page(&self.caller.project, filter, after, limit)
    }
    fn open_attention(&mut self, facts: &AttentionFacts, at: i64) -> Result<Attention> {
        let row = self
            .project
            .attention
            .open(&self.caller.project, facts, at)?;
        Ok(row)
    }
    fn ack_attention(&mut self, id: u64, expected: Revision, at: i64) -> Result<Attention> {
        let identity = match &self.caller.principal {
            Principal::Owner { identity }
            | Principal::Worker { identity }
            | Principal::Task { identity, .. } => identity,
        };
        let row = self.project.attention.ack(id, expected, identity, at)?;
        Ok(row)
    }
    fn resolve_attention(
        &mut self,
        id: u64,
        expected: Revision,
        reason: ResolutionReason,
        at: i64,
    ) -> Result<Attention> {
        let row = self.project.attention.resolve(id, expected, reason, at)?;
        Ok(row)
    }
    fn expire_attention(&mut self, cutoff: i64, limit: usize) -> Result<usize> {
        let count = self.project.attention.expire(cutoff, limit)?;
        Ok(count)
    }
}
