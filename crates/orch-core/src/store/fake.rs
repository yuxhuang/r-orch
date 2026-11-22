//! In-memory v2 test double, enabled only by `test-support`.
//!
//! Cursor tokens retain only query bindings and sort keys, never snapshots.
//! They deliberately have no persistent or cross-backend representation.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use orch_domain::{GoalStatus, TaskStatus};
use tokio::sync::Mutex as AsyncMutex;

use super::*;

type Rows = BTreeMap<(EntityKind, String), Record>;

#[derive(Clone, Default)]
struct Project {
    rows: Rows,
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
#[derive(Default)]
pub struct FakeStorage {
    projects: AsyncMutex<BTreeMap<String, Project>>,
    bookmarks: Arc<Mutex<Bookmarks>>,
}

struct View {
    caller: Caller,
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
            .unwrap_or_default();
        Ok(Box::new(View {
            caller,
            project,
            bookmarks: self.bookmarks.clone(),
        }))
    }

    async fn write(&self, caller: Caller, _lane: Lane, work: WriteWork) -> Result<()> {
        let mut projects = self.projects.lock().await;
        let project = projects.get(&caller.project).cloned().unwrap_or_default();
        let mut view = View {
            caller,
            project,
            bookmarks: self.bookmarks.clone(),
        };
        work(&mut view)?;
        projects.insert(view.caller.project, view.project);
        Ok(())
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
        validate_sort(filter.kind(), request.sort)?;
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
        _ => false,
    }
}

fn validate_sort(kind: EntityKind, sort: Sort) -> Result<()> {
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
        (_, Entity::Run(v)) => vec![Newest(Reverse(Some(v.started_at.clone())))],
        (_, Entity::Message(v)) => vec![Text(v.created_at.clone()), Position(row.position)],
        (_, Entity::Context(_) | Entity::Config(_) | Entity::Template(_)) => vec![],
    };
    key.push(Text(row.entity.id().into()));
    key
}
