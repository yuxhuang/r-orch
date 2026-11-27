//! Public-page bridge: validate first, resolve on this view, then select after
//! the original tuple. No optional private name/member selector is supplied.
use super::super::{Result, SqliteError};
use super::super::{
    agent_team_run as atr, cctg,
    non_task_cursor::{Component, Layout, NonTaskCursors, Tuple},
    retry_message::message,
};
use crate::blob::DiskBlobStore;
use orch_core::store::{Entity, Filter, Page, PageRequest, Record, Sort, StoreError};
use rusqlite::Transaction;
use std::borrow::Cow;

fn invalid() -> SqliteError {
    StoreError::InvalidCursor.into()
}
fn text(value: &str) -> Component {
    Component::Text(value.to_owned())
}
fn int(value: u64) -> Result<Component> {
    Ok(Component::Int(i64::try_from(value).map_err(|_| invalid())?))
}

pub(super) fn tuple(filter: &Filter, sort: Sort, row: &Record) -> Result<Tuple> {
    let layout = Layout::of(filter, sort).ok_or_else(invalid)?;
    let id = text(row.entity.id());
    let parts = if sort == Sort::Id {
        vec![id]
    } else {
        match &row.entity {
            Entity::Agent(a) if sort == Sort::Name => vec![text(&a.name), int(row.position)?, id],
            Entity::Agent(_) => vec![int(row.position)?, id],
            Entity::Team(t) if sort == Sort::Name => vec![text(&t.name), id],
            Entity::Team(t) => vec![text(&t.created_at), id],
            Entity::Run(r) if matches!(filter, Filter::Runs { task: Some(_), .. }) => {
                vec![int(r.attempt.into())?, id]
            }
            Entity::Run(r) => vec![text(&r.started_at), id],
            Entity::Message(m) => vec![text(&m.created_at), int(row.position)?, id],
            Entity::Goal(g) if sort == Sort::CreatedAt => {
                vec![text(&g.created_at), int(row.position)?, id]
            }
            Entity::Goal(g) => vec![
                int(orch_domain::GoalStatus::ALL
                    .iter()
                    .position(|s| *s == g.status)
                    .ok_or_else(invalid)? as u64)?,
                Component::Int(i64::from(g.updated_at.is_none())),
                text(g.updated_at.as_deref().unwrap_or("")),
                int(row.position)?,
                id,
            ],
            Entity::Context(_) | Entity::Config(_) | Entity::Template(_) => vec![id],
            _ => return Err(invalid()),
        }
    };
    Tuple::new(&layout, parts).ok_or_else(invalid)
}
fn string(t: &Tuple, index: usize) -> Result<Cow<'_, str>> {
    match t.components().get(index) {
        Some(Component::Text(s)) => Ok(Cow::Borrowed(s)),
        _ => Err(invalid()),
    }
}
fn number(t: &Tuple, index: usize) -> Result<u64> {
    match t.components().get(index) {
        Some(Component::Int(n)) => u64::try_from(*n).map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}
fn atr_key<'a>(f: &Filter, s: Sort, t: &'a Tuple) -> Result<atr::OrderKey<'a>> {
    use atr::OrderKey as K;
    Ok(if s == Sort::Id {
        K::Id(string(t, 0)?)
    } else {
        match f {
            Filter::Agents if s == Sort::Name => K::AgentName {
                name: string(t, 0)?,
                position: number(t, 1)?,
                id: string(t, 2)?,
            },
            Filter::Agents => K::AgentPosition {
                position: number(t, 0)?,
                id: string(t, 1)?,
            },
            Filter::Teams if s == Sort::Name => K::TeamName {
                name: string(t, 0)?,
                id: string(t, 1)?,
            },
            Filter::Teams => K::TeamCreated {
                created_at: string(t, 0)?,
                id: string(t, 1)?,
            },
            Filter::Runs { task: Some(_), .. } => K::RunAttempt {
                attempt: u32::try_from(number(t, 0)?).map_err(|_| invalid())?,
                id: string(t, 1)?,
            },
            Filter::Runs { .. } => K::RunStarted {
                started_at: string(t, 0)?,
                id: string(t, 1)?,
            },
            _ => return Err(invalid()),
        }
    })
}
fn cctg_key<'a>(f: &Filter, s: Sort, t: &'a Tuple) -> Result<cctg::OrderKey<'a>> {
    use cctg::OrderKey as K;
    Ok(match (f, s) {
        (_, Sort::Id) | (Filter::Context | Filter::Config | Filter::Templates, _) => {
            K::Id(string(t, 0)?)
        }
        (_, Sort::CreatedAt) => K::GoalCreated {
            created_at: string(t, 0)?,
            position: number(t, 1)?,
            id: string(t, 2)?,
        },
        _ => K::GoalDefault {
            rank: number(t, 0)?,
            updated_at: if number(t, 1)? == 0 {
                Some(string(t, 2)?)
            } else {
                None
            },
            position: number(t, 3)?,
            id: string(t, 4)?,
        },
    })
}
fn message_key(s: Sort, t: &Tuple) -> Result<message::OrderKey<'_>> {
    Ok(if s == Sort::Id {
        message::OrderKey::Id(string(t, 0)?)
    } else {
        message::OrderKey::Default {
            created_at: string(t, 0)?,
            position: number(t, 1)?,
            id: string(t, 2)?,
        }
    })
}
fn get(
    tx: &Transaction<'_>,
    disk: &DiskBlobStore,
    project: &str,
    f: &Filter,
    form: i64,
    stored: &str,
) -> Result<Option<Record>> {
    match f {
        Filter::Agents | Filter::Teams | Filter::Runs { .. } => {
            let family = match f {
                Filter::Agents => atr::Family::Agent,
                Filter::Teams => atr::Family::Team,
                _ => atr::Family::Run,
            };
            atr::NativeSql::new(tx, disk, project, 0)
                .read_stored(family, form, stored)
                .map(Some)
                .map_err(super::super::entity_router::native)
        }
        Filter::Messages { .. } | Filter::PendingMessages { .. } => {
            message::MessageReader::new(tx, project, disk).read(form, stored)
        }
        _ => {
            let family = match f {
                Filter::Context => cctg::Family::Context,
                Filter::Config => cctg::Family::Config,
                Filter::Templates => cctg::Family::Template,
                _ => cctg::Family::Goal,
            };
            cctg::NativeSql::new(tx, disk, project, None, 0)
                .row(family, form, stored)
                .map_err(Into::into)
        }
    }
}
pub(super) fn page(
    tx: &Transaction<'_>,
    disk: &DiskBlobStore,
    project: &str,
    cursors: &NonTaskCursors,
    now: i64,
    f: &Filter,
    p: &PageRequest,
) -> Result<Page<Record>> {
    if !(1..=1000).contains(&p.limit) {
        return Err(StoreError::InvalidPage("limit must be between 1 and 1000".into()).into());
    }
    if Layout::of(f, p.sort).is_none() {
        return Err(StoreError::InvalidPage("unsupported sort".into()).into());
    }
    let after = p
        .cursor
        .as_ref()
        .map(|token| {
            cursors.resume(token, project, f, p.sort, now, tx, |_, form, stored| {
                let row = get(tx, disk, project, f, form, stored)?.ok_or_else(invalid)?;
                tuple(f, p.sort, &row)
            })
        })
        .transpose()?;
    let (items, total, has_more) = match f {
        Filter::Agents | Filter::Teams | Filter::Runs { .. } => {
            let key = after.as_ref().map(|t| atr_key(f, p.sort, t)).transpose()?;
            let page = atr::NativeSql::new(tx, disk, project, now)
                .select_after(
                    atr::NativeSelection {
                        filter: f,
                        sort: p.sort,
                        name: None,
                        member: None,
                    },
                    key.as_ref(),
                    p.limit,
                )
                .map_err(super::super::entity_router::native)?;
            (page.items, page.total, page.has_more)
        }
        Filter::Messages { .. } | Filter::PendingMessages { .. } => {
            let key = after.as_ref().map(|t| message_key(p.sort, t)).transpose()?;
            let page = message::MessageReader::new(tx, project, disk).select_after(
                message::MessageSelection {
                    filter: f,
                    sort: p.sort,
                },
                key.as_ref(),
                p.limit,
            )?;
            (page.items, page.total, page.has_more)
        }
        _ => {
            let key = after.as_ref().map(|t| cctg_key(f, p.sort, t)).transpose()?;
            let page = cctg::NativeSql::new(tx, disk, project, None, now)
                .select_after(
                    cctg::NativeSelection {
                        filter: f,
                        sort: p.sort,
                    },
                    key.as_ref(),
                    p.limit,
                )
                .map_err(SqliteError::from)?;
            (page.items, page.total, page.has_more)
        }
    };
    let next_cursor = if has_more {
        items
            .last()
            .map(|r| tuple(f, p.sort, r).and_then(|t| cursors.issue(project, f, p.sort, &t, now)))
            .transpose()?
    } else {
        None
    };
    Ok(Page {
        items,
        next_cursor,
        total: if p.with_count { Some(total) } else { None },
    })
}
