//! Internal task-ID paging leaf; hydration and public cursor issuance belong to T10.
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

#[derive(Debug)]
pub(crate) struct TaskIdPage {
    pub(crate) ids: Vec<String>,
    pub(crate) next: Option<Continuation>,
    pub(crate) total: Option<u64>,
}

const RANK: &str = "CASE status WHEN 'in_progress' THEN 0 WHEN 'retrying' THEN 1 WHEN 'review' THEN 2 WHEN 'todo' THEN 3 WHEN 'done' THEN 4 WHEN 'failed' THEN 5 WHEN 'cancelled' THEN 6 ELSE 7 END";

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
    let keys: Vec<(&str, bool)> = match request.sort {
        Sort::Default => vec![
            (RANK, false),
            ("updated_at", true),
            ("position", false),
            ("id", false),
        ],
        Sort::Id => vec![("id", false)],
        Sort::Position => vec![("position", false), ("id", false)],
        Sort::Priority => vec![("priority", false), ("position", false), ("id", false)],
        Sort::CreatedAt => vec![("created_at", false), ("position", false), ("id", false)],
        Sort::UpdatedAt => vec![("updated_at", true), ("position", false), ("id", false)],
        Sort::Name => {
            return Err(StoreError::InvalidPage("Name is not offered for Task".into()).into());
        }
    };
    if let Some(after) = after
        && (after.project != project || after.filter != *filter || after.sort != request.sort)
    {
        return Err(StoreError::InvalidCursor.into());
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
