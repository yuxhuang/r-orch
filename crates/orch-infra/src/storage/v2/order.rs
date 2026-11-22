//! Filters, sorts and sort keys of the file reference, written from the spec's
//! list table ("Port v2") and the 1.0 stores' orders, not from the test double.

use std::cmp::Reverse;

use orch_core::store::{Entity, EntityKind, Filter, Record, Sort, StoreError};
use orch_domain::{GoalStatus, TaskStatus};

/// One comparable component of a sort key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Part {
    Int(i64),
    Position(u64),
    Text(String),
    /// Larger text first; an absent value is the empty text (1.0 goals).
    Newest(Reverse<String>),
}

pub(super) type Key = Vec<Part>;

pub(super) fn matches(entity: &Entity, filter: &Filter) -> bool {
    match (entity, filter) {
        (Entity::Task(t), Filter::Tasks { status, goal }) => {
            status.is_none_or(|s| s == t.status)
                && goal.as_ref().is_none_or(|g| t.goal_id.as_ref() == Some(g))
        }
        (Entity::Goal(g), Filter::Goals { status }) => status.is_none_or(|s| s == g.status),
        (Entity::Agent(_), Filter::Agents)
        | (Entity::Team(_), Filter::Teams)
        | (Entity::Context(_), Filter::Context) => true,
        (Entity::Run(r), Filter::Runs { task, agent }) => {
            task.as_ref().is_none_or(|t| *t == r.task_id)
                && agent.as_ref().is_none_or(|a| *a == r.agent_id)
        }
        (Entity::Message(m), Filter::Messages { recipient }) => recipient
            .as_ref()
            .is_none_or(|r| m.to_agent_id.as_ref() == Some(r)),
        _ => false,
    }
}

/// Every list offers `Default` and `Id`; the rest is per family.
pub(super) fn check_sort(kind: EntityKind, sort: Sort) -> Result<(), StoreError> {
    let offered: &[Sort] = match kind {
        EntityKind::Task => &[
            Sort::Position,
            Sort::Priority,
            Sort::CreatedAt,
            Sort::UpdatedAt,
        ],
        EntityKind::Goal => &[Sort::CreatedAt],
        EntityKind::Agent | EntityKind::Team => &[Sort::Name],
        EntityKind::Run | EntityKind::Message | EntityKind::Context => &[],
    };
    if matches!(sort, Sort::Default | Sort::Id) || offered.contains(&sort) {
        Ok(())
    } else {
        Err(StoreError::InvalidPage(format!(
            "{sort:?} is not offered for {kind:?}"
        )))
    }
}

fn task_rank(status: TaskStatus) -> i64 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Retrying => 1,
        TaskStatus::Review => 2,
        TaskStatus::Todo => 3,
        TaskStatus::Done => 4,
        TaskStatus::Failed => 5,
        TaskStatus::Cancelled => 6,
    }
}

fn goal_rank(status: GoalStatus) -> i64 {
    match status {
        GoalStatus::Active => 0,
        GoalStatus::Paused => 1,
        GoalStatus::Achieved => 2,
        GoalStatus::Abandoned => 3,
    }
}

fn newest(text: &str) -> Part {
    Part::Newest(Reverse(text.to_owned()))
}

/// The row's place in the sort. The id is always last, so keys are unique and
/// a cursor can name the exact row it stopped after.
pub(super) fn key(row: &Record, sort: Sort) -> Key {
    use Part::{Int, Position, Text};
    let position = Position(row.position);
    let mut key = match (&row.entity, sort) {
        (_, Sort::Id) => vec![],
        (Entity::Task(t), Sort::Default) => {
            vec![Int(task_rank(t.status)), newest(&t.updated_at), position]
        }
        (Entity::Task(_), Sort::Position) => vec![position],
        (Entity::Task(t), Sort::Priority) => vec![Int(t.priority), position],
        (Entity::Task(t), Sort::CreatedAt) => vec![Text(t.created_at.clone()), position],
        (Entity::Task(t), Sort::UpdatedAt) => vec![newest(&t.updated_at), position],
        (Entity::Goal(g), Sort::Default) => vec![
            Int(goal_rank(g.status)),
            newest(g.updated_at.as_deref().unwrap_or("")),
            position,
        ],
        (Entity::Goal(g), Sort::CreatedAt) => vec![Text(g.created_at.clone()), position],
        (Entity::Agent(_), Sort::Default) => vec![position],
        (Entity::Agent(a), Sort::Name) => vec![Text(a.name.clone()), position],
        (Entity::Team(t), Sort::Default) => vec![Text(t.created_at.clone())],
        (Entity::Team(t), Sort::Name) => vec![Text(t.name.clone())],
        (Entity::Run(r), Sort::Default) => vec![newest(&r.started_at)],
        (Entity::Message(m), Sort::Default) => vec![Text(m.created_at.clone()), position],
        (Entity::Context(_), Sort::Default) => vec![],
        // `check_sort` refuses every other pair before a key is asked for.
        _ => vec![],
    };
    key.push(Text(row.entity.id().to_owned()));
    key
}
