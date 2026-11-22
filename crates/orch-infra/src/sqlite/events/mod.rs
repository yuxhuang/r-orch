//! Run-event operations on the SQLite schema (T16; spec R14, R61 to R63, R132).
//!
//! Plain functions over a borrowed transaction or connection, so the adapter
//! decides the unit of work and the lane: the write helpers take the `&Transaction`
//! of one writer job, and the read helpers take the connection (or the read-pool
//! transaction) of one snapshot. Nothing here opens a connection, queues work or
//! reads a clock. The rules are the port's (`orch_core::store::events`), proved
//! against the same shared cases as the memory and file references.
//!
//! * Sequence numbers come from the collector. Storage never allocates or
//!   renumbers; the first stored bytes win for a `(project, run, seq)`.
//! * An append checks its lease first, even for an empty or wholly duplicate
//!   batch: the active lease of the run, in the current lineage (`daemon_config`),
//!   with the caller's lineage and epoch, and, for a task principal, that task,
//!   run, lineage and epoch. Every mismatch is the one refusal, `StaleEpoch`.
//! * Nothing here authorizes a gap or a prune; the dispatcher decides who may
//!   call them (the port's rule).
//! * A refused operation writes nothing. After a returned `Sql` error the
//!   enclosing transaction must be rolled back, as the writer does for any
//!   error its job returns.
//! * Event batches over 1,000 rows are not refused here: the producer cuts them
//!   (R27), as it does for the other bulk work.
//!
//! Stored form: `payload` is the whole `RunEvent` as JSON, so that a read
//! returns the exact timestamp string and data that were appended. `type` repeats
//! the event type for queries and is checked against the payload on every read.
//! `at` is the timestamp in UTC milliseconds, or 0 when it is not an RFC 3339
//! instant (an imported 1.0 line can carry any text); it is for queries only.
//! Sequence numbers and gap ranges must fit a SQLite integer, so one above
//! `i64::MAX` is refused as invalid, and a read after such a number is empty.
//!
//! Not wired into `Storage` or the daemon: the parent registers this module.

use orch_core::store::events::{AppendResult, EventGap, PruneResult, SequencedEvent};
use orch_core::store::{Caller, Principal, StoreError};
use orch_domain::{RunEvent, RunEventType};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

/// The most rows a read, a tail or a prune handles in one call (the port's bound).
pub const MAX_ROWS: usize = 1_000;

#[derive(Debug, thiserror::Error)]
pub enum EventsError {
    /// A refusal of the port's contract: `StaleEpoch` or `InvalidEvents`.
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    /// A stored row that no operation of this module wrote. It is reported and
    /// never skipped, so that corruption does not become silent loss.
    #[error("stored run event is damaged: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, EventsError>;

fn invalid(reason: &str) -> EventsError {
    StoreError::InvalidEvents(reason.into()).into()
}

fn bound(value: usize) -> Result<i64> {
    if (1..=MAX_ROWS).contains(&value) {
        Ok(value as i64)
    } else {
        Err(invalid("row bound must be in 1..=1000"))
    }
}

/// The largest number SQLite can store, as a number of ours.
const MAX_STORED: u64 = i64::MAX as u64;

/// Appends one run's batch under the run's lease and reports how many rows were
/// stored and how many were skipped as already present (a duplicate inside the
/// batch counts as skipped too). A zero or too-large sequence refuses the whole
/// batch before anything is stored.
pub fn append_events(
    tx: &Transaction<'_>,
    caller: &Caller,
    run: &str,
    lineage: &str,
    epoch: u64,
    batch: &[SequencedEvent],
) -> Result<AppendResult> {
    // The fence comes first, so an empty or duplicate batch is refused as well.
    check_lease(tx, caller, run, lineage, epoch)?;
    let mut rows = Vec::with_capacity(batch.len());
    for row in batch {
        if row.seq == 0 {
            return Err(invalid("sequence numbers start at 1"));
        }
        if row.seq > MAX_STORED {
            return Err(invalid("sequence number is too large to store"));
        }
        rows.push((row.seq as i64, encode(&row.event)?));
    }
    // A row comes back only for an insert that happened, which is how a stored
    // row is told from a skipped one. (It also keeps this production code clear of
    // the `.execute(` scan of `orch-engine`'s `token_eco_preparation`, which
    // reads every crate and lets only a SQL literal argument pass.)
    let mut insert = tx.prepare_cached(
        "INSERT INTO run_events (project_id, run_id, seq, at, type, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (project_id, run_id, seq) DO NOTHING RETURNING seq",
    )?;
    let mut result = AppendResult {
        stored: 0,
        skipped: 0,
    };
    for (seq, (at, kind, payload)) in rows {
        if insert.exists(params![caller.project, run, seq, at, kind, payload])? {
            result.stored += 1;
        } else {
            result.skipped += 1;
        }
    }
    Ok(result)
}

/// Records the missing interval `(after_seq, after_seq + dropped]`. A repeat of
/// a stored gap is a no-op and a different count for the same start is refused;
/// neither writes anything.
pub fn record_event_gap(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    after_seq: u64,
    dropped: u64,
) -> Result<()> {
    let end = after_seq.checked_add(dropped);
    if dropped == 0 || end.is_none_or(|end| end > MAX_STORED) {
        return Err(invalid(
            "gap must name a positive, non-overflowing interval",
        ));
    }
    let (after, dropped) = (after_seq as i64, dropped as i64);
    let added = tx.execute(
        "INSERT INTO run_event_gaps (project_id, run_id, after_seq, dropped)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (project_id, run_id, after_seq) DO NOTHING",
        params![project, run, after, dropped],
    )?;
    if added == 1 {
        return Ok(());
    }
    let stored: i64 = tx.query_row(
        "SELECT dropped FROM run_event_gaps
         WHERE project_id = ?1 AND run_id = ?2 AND after_seq = ?3",
        params![project, run, after],
        |r| r.get(0),
    )?;
    if stored == dropped {
        Ok(())
    } else {
        Err(invalid("conflicting gap count"))
    }
}

/// Deletes at most `max_rows` events of one run, lowest sequence first, and
/// reports whether any are left. Gaps stay as loss records; the run, its result
/// and its blobs are never touched. The caller picks the runs to prune.
pub fn prune_events(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    max_rows: usize,
) -> Result<PruneResult> {
    let limit = bound(max_rows)?;
    let deleted = tx.execute(
        "DELETE FROM run_events
         WHERE project_id = ?1 AND run_id = ?2 AND seq IN (
           SELECT seq FROM run_events WHERE project_id = ?1 AND run_id = ?2
           ORDER BY seq LIMIT ?3)",
        params![project, run, limit],
    )?;
    let remains: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM run_events WHERE project_id = ?1 AND run_id = ?2)",
        params![project, run],
        |r| r.get(0),
    )?;
    Ok(PruneResult {
        deleted,
        complete: !remains,
    })
}

/// Events with a sequence above `after_seq`, oldest first; `limit` is 1..=1,000.
/// Call it inside one read transaction for a consistent snapshot.
pub fn read_events(
    conn: &Connection,
    project: &str,
    run: &str,
    after_seq: u64,
    limit: usize,
) -> Result<Vec<SequencedEvent>> {
    let limit = bound(limit)?;
    // Nothing above i64::MAX can be stored, so there is nothing after it.
    let Ok(after) = i64::try_from(after_seq) else {
        return Ok(Vec::new());
    };
    let mut select = conn.prepare_cached(
        "SELECT seq, type, payload FROM run_events
         WHERE project_id = ?1 AND run_id = ?2 AND seq > ?3 ORDER BY seq LIMIT ?4",
    )?;
    decode_all(&mut select, params![project, run, after, limit])
}

/// The last `count` events, still oldest first. Zero is an empty tail.
pub fn read_events_tail(
    conn: &Connection,
    project: &str,
    run: &str,
    count: usize,
) -> Result<Vec<SequencedEvent>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let count = bound(count)?;
    let mut select = conn.prepare_cached(
        "SELECT seq, type, payload FROM run_events
         WHERE project_id = ?1 AND run_id = ?2 ORDER BY seq DESC LIMIT ?3",
    )?;
    let mut tail = decode_all(&mut select, params![project, run, count])?;
    tail.reverse();
    Ok(tail)
}

/// Every recorded loss of the run, by start. Not paged: a run has few gaps.
pub fn read_event_gaps(conn: &Connection, project: &str, run: &str) -> Result<Vec<EventGap>> {
    let mut select = conn.prepare_cached(
        "SELECT after_seq, dropped FROM run_event_gaps
         WHERE project_id = ?1 AND run_id = ?2 ORDER BY after_seq",
    )?;
    let rows = select.query_map(params![project, run], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut gaps = Vec::new();
    for row in rows {
        let (after_seq, dropped) = row?;
        gaps.push(EventGap {
            after_seq: unsigned(after_seq, "gap start")?,
            dropped: unsigned(dropped, "gap count")?,
        });
    }
    Ok(gaps)
}

/// The one fence an event append shares with the other lease-bound calls
/// (`storage::v2::leases::Leases::check`): a missing or inactive lease, another
/// lineage or epoch, or a task principal that is not the holder are one refusal.
fn check_lease(
    tx: &Transaction<'_>,
    caller: &Caller,
    run: &str,
    lineage: &str,
    epoch: u64,
) -> Result<()> {
    let lease = tx
        .query_row(
            "SELECT task_id, lineage, epoch FROM leases
             WHERE project_id = ?1 AND run_id = ?2 AND state = 'active'",
            params![caller.project, run],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((task, held_lineage, held_epoch)) = lease else {
        return Err(StoreError::StaleEpoch.into());
    };
    let held_epoch = unsigned(held_epoch, "lease epoch")?;
    let current: String = tx
        .query_row(
            "SELECT value FROM daemon_config WHERE key = 'lineage'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| EventsError::Corrupt("the database holds no lineage".into()))?;
    if lineage != current || held_lineage != lineage || held_epoch != epoch {
        return Err(StoreError::StaleEpoch.into());
    }
    if let Principal::Task {
        task: named,
        run: named_run,
        lineage: named_lineage,
        epoch: named_epoch,
        ..
    } = &caller.principal
        && (named != &task
            || named_run != run
            || named_lineage != &held_lineage
            || *named_epoch != held_epoch)
    {
        return Err(StoreError::StaleEpoch.into());
    }
    Ok(())
}

fn encode(event: &RunEvent) -> Result<(i64, &'static str, String)> {
    let payload = serde_json::to_string(event)
        .map_err(|e| invalid(&format!("event cannot be stored as JSON: {e}")))?;
    let at = event
        .timestamp
        .parse::<jiff::Timestamp>()
        .map_or(0, |t| t.as_millisecond());
    Ok((at, event.event_type.as_str(), payload))
}

fn decode_all(
    select: &mut rusqlite::CachedStatement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<SequencedEvent>> {
    let rows = select.query_map(params, |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, kind, payload) = row?;
        let seq = unsigned(seq, "event sequence")?;
        let event: RunEvent = serde_json::from_str(&payload).map_err(|e| {
            EventsError::Corrupt(format!("event {seq}: payload is not an event: {e}"))
        })?;
        if kind.parse::<RunEventType>() != Ok(event.event_type) {
            return Err(EventsError::Corrupt(format!(
                "event {seq}: type column {kind:?} disagrees with its payload"
            )));
        }
        events.push(SequencedEvent { seq, event });
    }
    Ok(events)
}

fn unsigned(value: i64, what: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| EventsError::Corrupt(format!("{what} is negative")))
}

#[cfg(test)]
mod tests;
