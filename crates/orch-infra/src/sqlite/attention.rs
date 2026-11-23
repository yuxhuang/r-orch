//! Scope-bound attention over the caller's connection/snapshot or transaction.
//! No authorization, clocks, evaluator or public Storage registration lives here.
use super::{Result, SqliteError};
use orch_core::store::{Caller, Principal, Revision, StoreError, attention::*};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

const COLUMNS: &str = "id,facts_json,opened_at,acked_at,acked_by,resolved_at,resolved_reason,revision,kind,subject_kind,subject_id,severity,title,author_kind,author_run_id,resolved_by";

fn damaged(message: &str) -> SqliteError {
    StoreError::InvalidAttention(format!("damaged stored attention: {message}")).into()
}
fn decode_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| damaged("invalid typed JSON"))
}
fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|_| damaged("cannot encode typed value"))
}
fn label<T: serde::Serialize>(value: &T) -> Result<String> {
    decode_json(&json(value)?)
}
fn subject_kind(facts: &AttentionFacts) -> &'static str {
    match facts {
        AttentionFacts::DiskFloor {
            subject: DiskSubject::DataDirectory,
            ..
        } => "data_directory",
        AttentionFacts::DiskFloor {
            subject: DiskSubject::ProjectWorkspaces { .. },
            ..
        } => "project_workspaces",
        AttentionFacts::DispatchOffWithWork { .. } => "dispatch",
    }
}
fn severity(facts: &AttentionFacts) -> &'static str {
    match facts.severity() {
        Severity::Blocking => "blocking",
        Severity::Warning => "warning",
    }
}
fn decode(r: &Row<'_>, project: &str) -> Result<Attention> {
    let text: String = r.get(0)?;
    let id: u64 = text.parse().map_err(|_| damaged("invalid id"))?;
    if id == 0 || id > i64::MAX as u64 || text != id.to_string() {
        return Err(damaged("noncanonical id"));
    }
    let facts_text: Option<String> = r.get(1)?;
    let facts_text = facts_text.ok_or_else(|| damaged("typed facts missing"))?;
    if facts_text.len() > 512 {
        return Err(damaged("facts exceed bound"));
    }
    let facts: AttentionFacts = decode_json(&facts_text)?;
    let opened_at = r.get(2)?;
    facts.validate(project, opened_at)?;
    if r.get::<_, String>(8)? != label(&facts.kind())?
        || r.get::<_, String>(9)? != subject_kind(&facts)
        || r.get::<_, String>(10)? != facts.subject_id()
        || r.get::<_, String>(11)? != severity(&facts)
        || r.get::<_, String>(12)? != facts.title()
        || r.get::<_, String>(13)? != "daemon"
        || r.get::<_, Option<String>>(14)?.is_some()
    {
        return Err(damaged("facts disagree with row metadata"));
    }
    let acknowledgement = match (r.get::<_, Option<i64>>(3)?, r.get::<_, Option<String>>(4)?) {
        (None, None) => None,
        (Some(at), Some(identity)) if at >= opened_at => {
            check_id(&identity)?;
            Some(Acknowledgement { identity, at })
        }
        _ => return Err(damaged("invalid acknowledgement")),
    };
    let resolution = match (
        r.get::<_, Option<i64>>(5)?,
        r.get::<_, Option<String>>(6)?,
        r.get::<_, Option<String>>(15)?,
    ) {
        (None, None, None) => None,
        (Some(at), Some(reason), Some(by)) if at >= opened_at && by == "daemon" => {
            let reason: ResolutionReason = decode_json(&json(&reason)?)?;
            reason.validate(facts.kind())?;
            Some(Resolution { reason, at })
        }
        _ => return Err(damaged("invalid resolution")),
    };
    let revision: i64 = r.get(7)?;
    let revision: Revision =
        serde_json::from_value(revision.into()).map_err(|_| damaged("invalid revision"))?;
    Ok(Attention {
        id,
        facts,
        opened_at,
        acknowledgement,
        resolution,
        revision,
    })
}

pub(super) fn get(conn: &Connection, project: &str, id: u64) -> Result<Option<Attention>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM attention WHERE project_id=?1 AND id=?2"
    ))?;
    let mut rows = stmt.query(params![project, id.to_string()])?;
    rows.next()?.map(|r| decode(r, project)).transpose()
}

pub(super) fn page(
    conn: &Connection,
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
    let predicate = match filter {
        AttentionFilter::All => "1",
        AttentionFilter::Open => "resolved_at IS NULL",
        AttentionFilter::Resolved => "resolved_at IS NOT NULL",
    };
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM attention WHERE project_id=?1 AND CAST(id AS INTEGER)>?2 AND {predicate} ORDER BY CAST(id AS INTEGER) LIMIT ?3"))?;
    let mut rows = stmt.query(params![
        project,
        after.min(i64::MAX as u64) as i64,
        (limit + 1) as i64
    ])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(decode(row, project)?);
    }
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

/// A single INSERT and its counter trigger are statement-atomic, even when the
/// caller catches a constraint error and continues its outer transaction.
pub(super) fn open(
    tx: &Transaction<'_>,
    project: &str,
    facts: &AttentionFacts,
    at: i64,
) -> Result<Attention> {
    facts.validate(project, at)?;
    let kind = label(&facts.kind())?;
    let subject = facts.subject_id();
    let existing: Option<String> = tx.query_row("SELECT id FROM attention WHERE project_id=?1 AND kind=?2 AND subject_id=?3 AND resolved_at IS NULL", params![project, kind, subject], |r| r.get(0)).optional()?;
    if let Some(id) = existing {
        return get(tx, project, id.parse().map_err(|_| damaged("invalid id"))?)?
            .ok_or_else(|| damaged("missing duplicate"));
    }
    let last: i64 = tx
        .query_row(
            "SELECT value FROM counters WHERE project_id=?1 AND name='attention:id'",
            [project],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let id = last
        .checked_add(1)
        .filter(|_| last >= 0)
        .ok_or(StoreError::CounterExhausted)?;
    let encoded = json(facts)?;
    if encoded.len() > 512 {
        return Err(damaged("facts exceed bound"));
    }
    tx.execute("INSERT INTO attention(id,project_id,kind,subject_kind,subject_id,severity,title,opened_at,revision,facts_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9)", params![id.to_string(),project,kind,subject_kind(facts),subject,severity(facts),facts.title(),at,encoded])?;
    Ok(Attention {
        id: id as u64,
        facts: facts.clone(),
        opened_at: at,
        acknowledgement: None,
        resolution: None,
        revision: Revision::ONE,
    })
}
fn current(
    tx: &Transaction<'_>,
    project: &str,
    id: u64,
    expected: Revision,
    at: i64,
) -> Result<Attention> {
    match get(tx, project, id)? {
        Some(row) if row.revision == expected => {
            row.check_transition_time(at)?;
            Ok(row)
        }
        other => Err(StoreError::StaleAttentionRevision {
            id,
            expected,
            actual: other.map(|r| r.revision),
        }
        .into()),
    }
}
fn next_revision(row: &Attention) -> Result<Revision> {
    let revision = row.revision.next()?;
    i64::try_from(revision.get()).map_err(|_| StoreError::CounterExhausted)?;
    Ok(revision)
}
pub(super) fn ack(
    tx: &Transaction<'_>,
    caller: &Caller,
    id: u64,
    expected: Revision,
    at: i64,
) -> Result<Attention> {
    let identity = match &caller.principal {
        Principal::Owner { identity }
        | Principal::Worker { identity }
        | Principal::Task { identity, .. } => identity,
    };
    check_id(identity)?;
    let mut row = current(tx, &caller.project, id, expected, at)?;
    if row.acknowledgement.is_none() {
        row.revision = next_revision(&row)?;
        tx.execute("UPDATE attention SET acked_at=?3,acked_by=?4,revision=?5 WHERE project_id=?1 AND id=?2", params![caller.project,id.to_string(),at,identity,row.revision.get() as i64])?;
        row.acknowledgement = Some(Acknowledgement {
            identity: identity.clone(),
            at,
        });
    }
    Ok(row)
}
pub(super) fn resolve(
    tx: &Transaction<'_>,
    project: &str,
    id: u64,
    expected: Revision,
    reason: ResolutionReason,
    at: i64,
) -> Result<Attention> {
    let mut row = current(tx, project, id, expected, at)?;
    reason.validate(row.facts.kind())?;
    if row.resolution.is_none() {
        row.revision = next_revision(&row)?;
        tx.execute("UPDATE attention SET resolved_at=?3,resolved_by='daemon',resolved_reason=?4,revision=?5 WHERE project_id=?1 AND id=?2", params![project,id.to_string(),at,label(&reason)?,row.revision.get() as i64])?;
        row.resolution = Some(Resolution { reason, at });
    }
    Ok(row)
}
pub(super) fn expire(
    tx: &Transaction<'_>,
    project: &str,
    cutoff: i64,
    limit: usize,
) -> Result<usize> {
    check_limit(limit)?;
    check_time(cutoff)?;
    Ok(tx.execute("DELETE FROM attention WHERE project_id=?1 AND id IN (SELECT id FROM attention WHERE project_id=?1 AND resolved_at<?2 ORDER BY CAST(id AS INTEGER) LIMIT ?3)", params![project,cutoff,limit as i64])?)
}

#[cfg(test)]
mod tests;
