//! Internal task-ID paging leaf. `task_adapter` hydrates its IDs and owns the
//! public cursor token; this leaf only selects.
//!
//! Times are schema UTC milliseconds. Numeric ordering matches the file
//! reference's bytewise ordering for canonical `YYYY-MM-DDTHH:MM:SS.sssZ`
//! strings (four-digit years, UTC, exactly millisecond precision). It also
//! agrees within a uniformly formatted UTC second-precision corpus. It cannot
//! reproduce arbitrary mixed offsets/precisions, sub-millisecond differences,
//! invalid strings, or extended years after those strings have been discarded.
//! Import/hydration must resolve that compatibility boundary, not silently
//! claim equivalence for every RFC3339 representation.

use orch_core::store::{Filter, PageRequest, Sort, StoreError};
use rusqlite::{Transaction, params_from_iter, types::Value};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Page(#[from] StoreError),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

/// Trusted internal continuation, never a public token. Contains only the
/// seek tuple and its query binding, no snapshot or row offset.
#[derive(Debug, Clone)]
pub(crate) struct Continuation {
    project: String,
    filter: Filter,
    sort: Sort,
    values: Vec<Value>,
}

impl Continuation {
    /// Validate decoded fields before accepting a continuation. The integrator
    /// must bound and authenticate the public token and derive `project` from
    /// the authorized caller; this checks its decoded binding and seek shape.
    pub(crate) fn from_decoded(
        decoded_project: String,
        decoded_filter: Filter,
        decoded_sort: Sort,
        values: Vec<Value>,
        project: &str,
        filter: &Filter,
        sort: Sort,
    ) -> Result<Self, StoreError> {
        validate_continuation(
            &decoded_project,
            &decoded_filter,
            decoded_sort,
            &values,
            project,
            filter,
            sort,
        )?;
        Ok(Self {
            project: decoded_project,
            filter: decoded_filter,
            sort: decoded_sort,
            values,
        })
    }
}

fn validate_continuation(
    decoded_project: &str,
    decoded_filter: &Filter,
    decoded_sort: Sort,
    values: &[Value],
    project: &str,
    filter: &Filter,
    sort: Sort,
) -> Result<(), StoreError> {
    if decoded_project != project
        || decoded_filter != filter
        || decoded_sort != sort
        || !matches!(filter, Filter::Tasks { .. })
    {
        return Err(StoreError::InvalidCursor);
    }
    let keys = sort_keys(sort).map_err(|_| StoreError::InvalidCursor)?;
    // Every supported sort ends in a text ID; preceding keys are integer
    // columns or the computed integer status rank. Check arity before
    // inspecting or indexing values.
    if values.len() != keys.len()
        || !matches!(values.last(), Some(Value::Text(_)))
        || !values[..values.len() - 1]
            .iter()
            .all(|value| matches!(value, Value::Integer(_)))
    {
        return Err(StoreError::InvalidCursor);
    }
    Ok(())
}

impl Continuation {
    /// The seek tuple: integer sort keys, then the last row's text ID.
    pub(crate) fn seek(&self) -> &[Value] {
        &self.values
    }
}

/// Number of components in a sort's seek tuple, the ID included. Refuses
/// the sorts that Task does not offer.
pub(crate) fn seek_width(sort: Sort) -> Result<usize, StoreError> {
    sort_keys(sort).map(<[_]>::len)
}

/// Request-side checks that precede any cursor decoding, so that a bad
/// request keeps its own error whatever token it carries.
pub(crate) fn validate_request(filter: &Filter, request: &PageRequest) -> Result<(), StoreError> {
    request.validate()?;
    if !matches!(filter, Filter::Tasks { .. }) {
        return Err(StoreError::InvalidPage("task filter required".into()));
    }
    sort_keys(request.sort).map(drop)
}

#[derive(Debug)]
pub(crate) struct TaskIdPage {
    pub(crate) ids: Vec<String>,
    pub(crate) next: Option<Continuation>,
    pub(crate) total: Option<u64>,
}

const RANK: &str = "CASE status WHEN 'in_progress' THEN 0 WHEN 'retrying' THEN 1 WHEN 'review' THEN 2 WHEN 'todo' THEN 3 WHEN 'done' THEN 4 WHEN 'failed' THEN 5 WHEN 'cancelled' THEN 6 ELSE 7 END";

fn sort_keys(sort: Sort) -> Result<&'static [(&'static str, bool)], StoreError> {
    Ok(match sort {
        Sort::Default => &[
            (RANK, false),
            ("updated_at", true),
            ("position", false),
            ("id", false),
        ],
        Sort::Id => &[("id", false)],
        Sort::Position => &[("position", false), ("id", false)],
        Sort::Priority => &[("priority", false), ("position", false), ("id", false)],
        Sort::CreatedAt => &[("created_at", false), ("position", false), ("id", false)],
        Sort::UpdatedAt => &[("updated_at", true), ("position", false), ("id", false)],
        Sort::Attempt => {
            return Err(StoreError::InvalidPage(
                "Attempt is not offered for Task".into(),
            ));
        }
        Sort::Name => {
            return Err(StoreError::InvalidPage(
                "Name is not offered for Task".into(),
            ));
        }
    })
}

/// The first ID of `project`'s tasks, in ID order, that begins with `prefix`,
/// is exactly `byte_len` bytes long and that `accept` takes, from the
/// transaction's snapshot. The page cursor names an ID too long to carry by its
/// length, a digest and a prefix; `accept` checks the digest. Candidates are
/// read one at a time and none is kept, so memory is one ID however many rows
/// share the prefix. The primary key bounds the scan to the rows that do.
pub(crate) fn find_id_with_prefix(
    tx: &Transaction<'_>,
    project: &str,
    prefix: &str,
    byte_len: usize,
    mut accept: impl FnMut(&str) -> bool,
) -> rusqlite::Result<Option<String>> {
    let mut sql = "SELECT id FROM tasks WHERE project_id = ?1 AND id >= ?2".to_owned();
    let mut args = vec![
        Value::Text(project.to_owned()),
        Value::Text(prefix.to_owned()),
    ];
    if let Some(end) = prefix_end(prefix) {
        args.push(Value::Text(end));
        sql.push_str(&format!(" AND id < ?{}", args.len()));
    }
    args.push(Value::Integer(byte_len as i64));
    sql.push_str(&format!(" AND length(CAST(id AS BLOB)) = ?{}", args.len()));
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(&args))?;
    while let Some(row) = rows.next()? {
        let id = row.get_ref(0)?.as_str()?;
        if accept(id) {
            return Ok(Some(id.to_owned()));
        }
    }
    Ok(None)
}

/// The least string that sorts after every string beginning with `prefix`
/// (byte order is code point order in UTF-8), or `None` when none is bounded.
fn prefix_end(prefix: &str) -> Option<String> {
    let chars = prefix.char_indices().collect::<Vec<_>>();
    for (at, c) in chars.into_iter().rev() {
        let next = match c {
            char::MAX => continue,
            '\u{D7FF}' => '\u{E000}',
            c => char::from_u32(u32::from(c) + 1)?,
        };
        let mut end = prefix[..at].to_owned();
        end.push(next);
        return Some(end);
    }
    None
}

/// Borrow an already-open transaction: count and IDs share its snapshot.
/// Call on the read executor thread, or inside a writer callback. A supplied
/// public cursor must first be resolved by the integrator to `after`; this
/// leaf refuses unresolved tokens rather than ignoring them.
pub(crate) fn query(
    tx: &Transaction<'_>,
    project: &str,
    filter: &Filter,
    request: &PageRequest,
    after: Option<&Continuation>,
) -> Result<TaskIdPage, Error> {
    request.validate()?;
    if request.cursor.is_some() {
        return Err(StoreError::InvalidCursor.into());
    }
    let Filter::Tasks { status, goal } = filter else {
        return Err(StoreError::InvalidPage("task filter required".into()).into());
    };
    let keys = sort_keys(request.sort)?;
    if let Some(after) = after {
        validate_continuation(
            &after.project,
            &after.filter,
            after.sort,
            &after.values,
            project,
            filter,
            request.sort,
        )?;
    }
    let mut predicate = "project_id = ?".to_owned();
    let mut args = vec![Value::Text(project.to_owned())];
    if let Some(status) = status {
        predicate.push_str(" AND status = ?");
        args.push(Value::Text(status.to_string()));
    }
    if let Some(goal) = goal {
        predicate.push_str(" AND goal_id = ?");
        args.push(Value::Text(goal.clone()));
    }
    let total = if request.with_count {
        Some(tx.query_row(
            &format!("SELECT COUNT(*) FROM tasks WHERE {predicate}"),
            params_from_iter(&args),
            |r| r.get::<_, i64>(0).map(|count| count as u64),
        )?)
    } else {
        None
    };
    if let Some(after) = after {
        // Lexicographic seek with per-component direction. Repeating bounded
        // parameters avoids numeric negation (which would overflow i64::MIN).
        let mut terms = Vec::new();
        for (i, (column, descending)) in keys.iter().enumerate() {
            let mut parts = Vec::new();
            for (j, (equal_column, _)) in keys.iter().enumerate().take(i) {
                parts.push(format!("{equal_column} = ?"));
                args.push(after.values[j].clone());
            }
            parts.push(format!(
                "{column} {} ?",
                if *descending { "<" } else { ">" }
            ));
            args.push(after.values[i].clone());
            terms.push(format!("({})", parts.join(" AND ")));
        }
        predicate.push_str(&format!(" AND ({})", terms.join(" OR ")));
    }
    let columns = keys.iter().map(|(c, _)| *c).collect::<Vec<_>>().join(", ");
    let order = keys
        .iter()
        .map(|(c, d)| format!("{c} {}", if *d { "DESC" } else { "ASC" }))
        .collect::<Vec<_>>()
        .join(", ");
    args.push(Value::Integer((request.limit + 1) as i64));
    let sql = format!("SELECT id, {columns} FROM tasks WHERE {predicate} ORDER BY {order} LIMIT ?");
    let mut statement = tx.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(&args), |r| {
        let id: String = r.get(0)?;
        let values = (1..=keys.len())
            .map(|i| r.get(i))
            .collect::<rusqlite::Result<Vec<Value>>>()?;
        Ok((id, values))
    })?;
    let mut rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > request.limit;
    rows.truncate(request.limit);
    let next = if more {
        rows.last().map(|(_, values)| Continuation {
            project: project.to_owned(),
            filter: filter.clone(),
            sort: request.sort,
            values: values.clone(),
        })
    } else {
        None
    };
    Ok(TaskIdPage {
        ids: rows.into_iter().map(|(id, _)| id).collect(),
        next,
        total,
    })
}

#[cfg(test)]
mod tests;
