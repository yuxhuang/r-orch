//! In-memory positions for retained and lookup cursors (ADR 0024, step 4).
//! The table never holds a database view or blob reference. Its lock is released
//! before the unfiltered identity scan and the caller's same-view hydration.
use super::layout::Arm;
use super::token::{MAX_TEXT, hex, unhex};
use super::{Component, Layout, NonTaskCursors, Tuple};
use crate::sqlite::{Result, SqliteError};
use orch_core::store::{Cursor, Filter, Sort, StoreError};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const RETAINED: u8 = 2;
const LOOKUP: u8 = 3;
const TOKEN_BYTES: usize = 34;
const MAX_ENTRIES: usize = 1_024;
const MAX_CHARGE: usize = 8_388_608;
const TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// A text is represented by its original UTF-8 byte length and full SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TextDigest {
    bytes: u64,
    sha256: [u8; 32],
}
impl TextDigest {
    fn of(text: &str) -> Self {
        Self {
            bytes: text.len() as u64,
            sha256: Sha256::digest(text.as_bytes()).into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum PartDigest {
    Int(i64),
    Text(TextDigest),
}
fn digests(tuple: &Tuple) -> Vec<PartDigest> {
    tuple
        .components()
        .iter()
        .map(|part| match part {
            Component::Int(v) => PartDigest::Int(*v),
            Component::Text(text) => PartDigest::Text(TextDigest::of(text)),
        })
        .collect()
}
#[derive(Clone)]
enum Position {
    Retained(Tuple),
    Lookup(Vec<PartDigest>),
}
impl Position {
    fn version(&self) -> u8 {
        match self {
            Self::Retained(_) => RETAINED,
            Self::Lookup(_) => LOOKUP,
        }
    }
    fn charge(&self) -> usize {
        match self {
            Self::Retained(tuple) => tuple.size(),
            Self::Lookup(parts) => parts
                .iter()
                .map(|p| match p {
                    PartDigest::Int(_) => 8,
                    PartDigest::Text(_) => 40,
                })
                .sum(),
        }
    }
}
#[derive(Clone)]
struct Entry {
    binding: [u8; 16],
    sort: u8,
    issued: Duration,
    position: Position,
}
#[derive(Default)]
struct State {
    next: u64,
    charge: usize,
    entries: VecDeque<(u64, Entry)>,
}
impl State {
    fn pop(&mut self) {
        if let Some((_, entry)) = self.entries.pop_front() {
            self.charge -= entry.position.charge();
        }
    }
    fn expire(&mut self, now: Duration) {
        while self
            .entries
            .front()
            .is_some_and(|(_, e)| now.saturating_sub(e.issued) > TTL)
        {
            self.pop();
        }
    }
}

pub(super) struct HeldTable {
    nonce: [u8; 8],
    clock: Box<dyn Fn() -> Duration + Send + Sync>,
    state: Mutex<State>,
}
impl HeldTable {
    pub(super) fn new() -> Self {
        let start = Instant::now();
        Self::with_clock(move || start.elapsed())
    }
    fn with_clock(clock: impl Fn() -> Duration + Send + Sync + 'static) -> Self {
        // nanoid uses the existing OS-backed random source. Hashing its random
        // identifier gives eight random bytes without another dependency.
        let random = Sha256::digest(nanoid::nanoid!().as_bytes());
        let mut nonce = [0; 8];
        nonce.copy_from_slice(&random[..8]);
        Self {
            nonce,
            clock: Box::new(clock),
            state: Mutex::new(State::default()),
        }
    }
}
impl NonTaskCursors {
    /// The private monotonic clock is injected once for the whole adapter. The
    /// caller must supply a nondecreasing clock; wall time belongs only to inline.
    pub(in crate::sqlite) fn with_clock(
        generation: impl Into<String>,
        clock: impl Fn() -> Duration + Send + Sync + 'static,
    ) -> Self {
        Self {
            generation: generation.into(),
            held: HeldTable::with_clock(clock),
        }
    }

    /// Issue a position outside the database transaction. Neither a rollback
    /// nor use of an entry refreshes it or removes any other live position.
    pub(in crate::sqlite) fn issue(
        &self,
        project: &str,
        filter: &Filter,
        sort: Sort,
        anchor: &Tuple,
        now_ms: i64,
    ) -> Result<Cursor> {
        let layout = Layout::of(filter, sort)
            .ok_or_else(|| SqliteError::Unrepresentable("non-task cursor family or sort".into()))?;
        // A Tuple is valid by construction, but may have been built for another
        // layout. Check its shape before encoding or storing any position.
        if !layout.admits(anchor.components()) {
            return Err(SqliteError::Unrepresentable(
                "non-task cursor tuple shape".into(),
            ));
        }
        let position = match layout.arm(anchor) {
            Arm::Inline => {
                return self
                    .issue_inline(project, filter, sort, anchor, now_ms)?
                    .ok_or_else(invalid);
            }
            Arm::Retained => Position::Retained(anchor.clone()),
            Arm::Lookup => Position::Lookup(digests(anchor)),
        };
        let binding = self.binding(project, filter)?;
        let sort = Self::sort_tag(sort).ok_or_else(invalid)?;
        let version = position.version();
        let charge = position.charge();
        let mut state = self
            .held
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = (self.held.clock)();
        state.expire(now);
        // All positions fit by construction. Admission always succeeds and
        // evicts only the oldest-issued entries, regardless of their use.
        while state.entries.len() >= MAX_ENTRIES || state.charge + charge > MAX_CHARGE {
            state.pop();
        }
        // Wrapping is harmless: a live handle is never reused, even at u64::MAX.
        let handle = loop {
            state.next = state.next.wrapping_add(1);
            if !state.entries.iter().any(|(h, _)| *h == state.next) {
                break state.next;
            }
        };
        state.entries.push_back((
            handle,
            Entry {
                binding,
                sort,
                issued: now,
                position,
            },
        ));
        state.charge += charge;
        drop(state);
        let mut raw = vec![version, sort];
        raw.extend(binding);
        raw.extend(self.held.nonce);
        raw.extend(handle.to_be_bytes());
        Ok(Cursor::new(hex(&raw))?)
    }

    /// Return the verified original position in the supplied resuming read view.
    /// For lookup, `hydrate` must read the *stored locator* (form and key) in
    /// exactly `view`, validate its native projections/blob data, and produce the
    /// original tuple for this sort. Never feed a form-1 digest to `get(id)`.
    /// Its typed storage errors pass through; they are not cursor refusals.
    #[allow(
        clippy::too_many_arguments,
        reason = "the request binding, wall clock and caller-owned read view are explicit"
    )]
    pub(in crate::sqlite) fn resume(
        &self,
        cursor: &Cursor,
        project: &str,
        filter: &Filter,
        sort: Sort,
        now_ms: i64,
        view: &Connection,
        mut hydrate: impl FnMut(&Connection, i64, &str) -> Result<Tuple>,
    ) -> Result<Tuple> {
        if cursor.as_str().is_empty() || cursor.as_str().len() > MAX_TEXT {
            return Err(invalid());
        }
        let raw = unhex(cursor.as_str()).ok_or_else(invalid)?;
        if raw.first() == Some(&1) {
            return self.resume_inline(cursor, project, filter, sort, now_ms);
        }
        // Token-only checks happen before the table or any row is read.
        if raw.len() != TOKEN_BYTES
            || !matches!(raw[0], RETAINED | LOOKUP)
            || Some(raw[1]) != Self::sort_tag(sort)
            || raw[2..18] != self.binding(project, filter).map_err(|_| invalid())?
            || raw[18..26] != self.held.nonce
        {
            return Err(invalid());
        }
        let layout = Layout::of(filter, sort).ok_or_else(invalid)?;
        let handle = u64::from_be_bytes(raw[26..34].try_into().map_err(|_| invalid())?);
        let now = (self.held.clock)();
        let position = {
            let mut state = self
                .held
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.expire(now);
            let (_, entry) = state
                .entries
                .iter()
                .find(|(h, _)| *h == handle)
                .ok_or_else(invalid)?;
            // Both held versions name the same table. P-T3 permits an edited
            // handle to name any live entry of this binding and sort; the entry
            // itself determines whether its position is retained or by lookup.
            if entry.binding != raw[2..18] || entry.sort != raw[1] {
                return Err(invalid());
            }
            entry.position.clone()
        };
        match position {
            Position::Retained(tuple) => Ok(tuple),
            Position::Lookup(parts) => {
                let Some(PartDigest::Text(identity)) = parts.last() else {
                    return Err(invalid());
                };
                let (form, stored) =
                    find_identity(view, project, filter, identity)?.ok_or_else(invalid)?;
                let tuple = hydrate(view, form, &stored)?;
                // A mismatch with the selected stored identity is data damage,
                // even when the remaining tuple could be an ordinary edit.
                if TextDigest::of(tuple.identity()) != *identity {
                    return Err(corrupt(filter, form));
                }
                if !layout.admits(tuple.components()) || digests(&tuple) != parts {
                    return Err(invalid());
                }
                Ok(tuple)
            }
        }
    }
}
fn invalid() -> SqliteError {
    StoreError::InvalidCursor.into()
}

fn family(filter: &Filter) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match filter {
        Filter::Agents => ("agent", "agents", "id"),
        Filter::Teams => ("team", "teams", "id"),
        Filter::Runs { .. } => ("run", "runs", "id"),
        Filter::Messages { .. } | Filter::PendingMessages { .. } => ("message", "messages", "id"),
        Filter::Context => ("context", "context_entries", "key"),
        Filter::Config => ("config", "project_config", "key"),
        Filter::Templates => ("template", "templates", "name"),
        Filter::Goals { .. } => ("goal", "goals", "id"),
        Filter::Tasks { .. } => return None,
    })
}
fn corrupt(filter: &Filter, form: i64) -> SqliteError {
    if matches!(
        filter,
        Filter::Messages { .. } | Filter::PendingMessages { .. }
    ) {
        SqliteError::MessageCorrupt {
            form,
            check: "identity projection",
            locator: "id",
        }
    } else {
        SqliteError::EntityCorrupt {
            family: family(filter).map_or("unknown", |f| f.0),
            form,
            check: "identity projection",
            locator: "id".into(),
        }
    }
}
/// Unfiltered, project- and family-scoped. Only identities are scanned, not
/// candidate rows. Native TEXT length is measured as BLOB (UTF-8 bytes), while
/// form 1 carries the original byte length and digest in its native quartet.
fn find_identity(
    view: &Connection,
    project: &str,
    filter: &Filter,
    wanted: &TextDigest,
) -> Result<Option<(i64, String)>> {
    let (_, table, id) = family(filter).ok_or_else(invalid)?;
    let mut stmt = view.prepare(&format!(
        "SELECT {id}_form,{id},{id}_sha256,CASE WHEN {id}_form=0 THEN length(CAST({id} AS BLOB)) ELSE {id}_bytes END FROM {table} WHERE project_id=?1"
    ))?;
    let mut rows = stmt.query([project])?;
    while let Some(row) = rows.next()? {
        let form = row.get::<_, i64>(0).map_err(|_| corrupt(filter, -1))?;
        if !matches!(form, 0 | 1) {
            return Err(corrupt(filter, form));
        }
        let bytes = u64::try_from(row.get::<_, i64>(3).map_err(|_| corrupt(filter, form))?)
            .map_err(|_| corrupt(filter, form))?;
        if bytes != wanted.bytes {
            continue;
        }
        let stored = row.get::<_, String>(1).map_err(|_| corrupt(filter, form))?;
        let matches = if form == 0 {
            TextDigest::of(&stored) == *wanted
        } else {
            let digest = row.get::<_, String>(2).map_err(|_| corrupt(filter, form))?;
            if stored != digest {
                return Err(corrupt(filter, form));
            }
            digest == hex(&wanted.sha256)
        };
        if matches {
            return Ok(Some((form, stored)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    //! Step 4 fixture: real SQLite identity quartets, synthetic tuple hydration on
    //! the supplied connection. This tests the scan/table, not native blob decoding
    //! or a public Storage page (step 5).
    use super::super::layout::Kind;
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    fn clock() -> (NonTaskCursors, Arc<AtomicU64>) {
        let time = Arc::new(AtomicU64::new(0));
        let copy = time.clone();
        (
            NonTaskCursors::with_clock("g", move || {
                Duration::from_millis(copy.load(Ordering::SeqCst))
            }),
            time,
        )
    }
    fn make(filter: &Filter, sort: Sort, texts: &[&str]) -> Tuple {
        let layout = Layout::of(filter, sort).unwrap();
        let mut texts = texts.iter();
        Tuple::new(
            &layout,
            layout
                .parts()
                .iter()
                .map(|part| {
                    if part.kind == Kind::Text {
                        Component::Text(texts.next().unwrap().to_string())
                    } else {
                        Component::Int(0)
                    }
                })
                .collect(),
        )
        .unwrap()
    }
    fn id(bytes: usize) -> String {
        format!("{}{}", "é".repeat(bytes / 2), "x".repeat(bytes % 2))
    }
    fn issue(c: &NonTaskCursors, f: &Filter, s: Sort, t: &Tuple) -> Cursor {
        c.issue("p", f, s, t, 0).unwrap()
    }
    fn no_read(
        c: &NonTaskCursors,
        token: &Cursor,
        f: &Filter,
        s: Sort,
        wall: i64,
    ) -> Result<Tuple> {
        c.resume(
            token,
            "p",
            f,
            s,
            wall,
            &Connection::open_in_memory().unwrap(),
            |_, _, _| panic!("must not hydrate"),
        )
    }
    fn refused<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(SqliteError::Store(StoreError::InvalidCursor))),
            "{result:?}"
        );
    }
    fn state(c: &NonTaskCursors) -> (usize, usize) {
        let state = c.held.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.entries.len(), state.charge)
    }
    fn filters() -> Vec<Filter> {
        vec![
            Filter::Agents,
            Filter::Teams,
            Filter::Runs {
                task: None,
                agent: None,
            },
            Filter::Runs {
                task: Some("t".into()),
                agent: Some("a".into()),
            },
            Filter::Messages { recipient: None },
            Filter::PendingMessages {
                recipient: "a".into(),
                now: "n".into(),
            },
            Filter::Context,
            Filter::Config,
            Filter::Templates,
            Filter::Goals { status: None },
        ]
    }

    /// Identity quartets mirror migrations 004–006. The additional `tuple` column
    /// substitutes for validated native/blob hydration, deliberately and only here.
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        for f in filters() {
            let (_, table, key) = family(&f).unwrap();
            db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS {table} (project_id TEXT NOT NULL, {key}_form INTEGER NOT NULL, {key} TEXT NOT NULL, {key}_sha256 TEXT, {key}_bytes INTEGER, tuple TEXT NOT NULL, unrelated TEXT NOT NULL DEFAULT '', PRIMARY KEY(project_id,{key}_form,{key})) STRICT;")).unwrap();
        }
        db
    }
    fn stored(t: &Tuple) -> (i64, String) {
        if t.identity().len() <= 65_536 {
            (0, t.identity().into())
        } else {
            (1, hex(&TextDigest::of(t.identity()).sha256))
        }
    }
    fn put(db: &Connection, project: &str, f: &Filter, t: &Tuple) {
        let (_, table, key) = family(f).unwrap();
        let (form, value) = stored(t);
        let bytes = (form == 1).then_some(t.identity().len() as i64);
        let digest = (form == 1).then_some(value.clone());
        let parts: Vec<serde_json::Value> = t
            .components()
            .iter()
            .map(|part| match part {
                Component::Int(n) => (*n).into(),
                Component::Text(s) => s.clone().into(),
            })
            .collect();
        db.execute(&format!("INSERT OR REPLACE INTO {table}(project_id,{key}_form,{key},{key}_sha256,{key}_bytes,tuple) VALUES (?1,?2,?3,?4,?5,?6)"), rusqlite::params![project,form,value,digest,bytes,serde_json::to_string(&parts).unwrap()]).unwrap();
    }
    fn delete(db: &Connection, f: &Filter, t: &Tuple) {
        let (_, table, key) = family(f).unwrap();
        let (form, value) = stored(t);
        db.execute(
            &format!("DELETE FROM {table} WHERE project_id='p' AND {key}_form=?1 AND {key}=?2"),
            rusqlite::params![form, value],
        )
        .unwrap();
    }
    fn read(
        c: &NonTaskCursors,
        token: &Cursor,
        f: &Filter,
        sort: Sort,
        db: &Connection,
    ) -> Result<Tuple> {
        c.resume(token, "p", f, sort, 0, db, |same, form, value| {
            assert!(std::ptr::eq(same, db));
            assert!(c.held.state.try_lock().is_ok(), "no lock across hydration");
            let (_, table, key) = family(f).unwrap();
            let json: String = same.query_row(
                &format!(
                    "SELECT tuple FROM {table} WHERE project_id='p' AND {key}_form=?1 AND {key}=?2"
                ),
                rusqlite::params![form, value],
                |r| r.get(0),
            )?;
            let parts: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
            Ok(Tuple::new(
                &Layout::of(f, sort).unwrap(),
                parts
                    .into_iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => Component::Text(s),
                        _ => Component::Int(v.as_i64().unwrap()),
                    })
                    .collect(),
            )
            .unwrap())
        })
    }

    #[test]
    fn retained_resumes_original_tuple_after_deletion_without_reading_a_row() {
        let (c, _) = clock();
        let f = Filter::Teams;
        let t = make(&f, Sort::Name, &[&"é".repeat(150), "anchor"]);
        let token = issue(&c, &f, Sort::Name, &t);
        assert_eq!(no_read(&c, &token, &f, Sort::Name, i64::MAX).unwrap(), t);
    }
    #[test]
    fn p_b1_all_capacity_rows_and_multibyte_texts_make_no_entry_then_one_retained_entry() {
        for f in filters() {
            for sort in [
                Sort::Default,
                Sort::Id,
                Sort::Name,
                Sort::CreatedAt,
                Sort::Attempt,
            ] {
                let Some(layout) = Layout::of(&f, sort) else {
                    continue;
                };
                let count = layout
                    .parts()
                    .iter()
                    .filter(|p| p.kind == Kind::Text)
                    .count();
                for long_first in [false, true] {
                    let (c, _) = clock();
                    for extra in [0, 1] {
                        let mut texts = vec!["".to_owned(); count];
                        texts[if long_first { 0 } else { count - 1 }] =
                            id(layout.inline_capacity() + extra);
                        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                        let t = make(&f, sort, &refs);
                        let token = issue(&c, &f, sort, &t);
                        assert_eq!(state(&c).0, extra);
                        assert_eq!(no_read(&c, &token, &f, sort, 0).unwrap(), t);
                    }
                }
            }
        }
    }
    #[test]
    fn p_b2_p_b3_q1_q9_d3_a_d2_t_select_whole_tuple_arm_and_exact_charge() {
        let cases = vec![
            (Filter::Context, Sort::Id, vec![id(65_536)], 2, 65_536),
            (Filter::Context, Sort::Id, vec![id(65_537)], 3, 40),
            (Filter::Agents, Sort::Default, vec![id(65_536)], 3, 48),
            (
                Filter::Teams,
                Sort::Name,
                vec![id(32_768), id(32_768)],
                2,
                65_536,
            ),
            (
                Filter::Teams,
                Sort::Name,
                vec![id(32_769), id(32_768)],
                3,
                80,
            ),
            (
                Filter::Agents,
                Sort::Name,
                vec![id(40_000), id(40_000)],
                3,
                88,
            ),
            (
                Filter::Goals { status: None },
                Sort::Default,
                vec![id(40_000), id(40_000)],
                3,
                104,
            ),
            (
                Filter::Goals { status: None },
                Sort::Default,
                vec![id(65_536), id(65_536)],
                3,
                104,
            ),
        ];
        for (f, s, texts, version, charge) in cases {
            let (c, _) = clock();
            let db = database();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let t = make(&f, s, &refs);
            let token = issue(&c, &f, s, &t);
            assert_eq!(unhex(token.as_str()).unwrap()[0], version);
            assert_eq!(state(&c), (1, charge));
            put(&db, "p", &f, &t);
            assert_eq!(read(&c, &token, &f, s, &db).unwrap(), t);
            delete(&db, &f, &t);
            if version == 2 {
                assert_eq!(read(&c, &token, &f, s, &db).unwrap(), t);
            } else {
                refused(read(&c, &token, &f, s, &db));
            }
        }
        let f = Filter::Goals { status: None };
        for presence in [0, 1] {
            for (bytes, version) in [(65_512, 2), (65_513, 3)] {
                let (c, _) = clock();
                let t = make(&f, Sort::Default, &["", &id(bytes)]);
                let mut parts = t.components().to_vec();
                parts[1] = Component::Int(presence);
                let t = Tuple::new(&Layout::of(&f, Sort::Default).unwrap(), parts).unwrap();
                let token = issue(&c, &f, Sort::Default, &t);
                assert_eq!(unhex(token.as_str()).unwrap()[0], version);
            }
        }
    }
    #[test]
    fn p_l1_count_eviction_does_not_refresh_on_resume() {
        let (c, _) = clock();
        let f = Filter::Context;
        let t = make(&f, Sort::Id, &[&id(300)]);
        let first = issue(&c, &f, Sort::Id, &t);
        let second = issue(&c, &f, Sort::Id, &t);
        for _ in 2..MAX_ENTRIES {
            issue(&c, &f, Sort::Id, &t);
        }
        no_read(&c, &first, &f, Sort::Id, 0).unwrap();
        let last = issue(&c, &f, Sort::Id, &t);
        assert_eq!(state(&c), (1024, 1024 * 300));
        refused(no_read(&c, &first, &f, Sort::Id, 0));
        no_read(&c, &second, &f, Sort::Id, 0).unwrap();
        no_read(&c, &last, &f, Sort::Id, 0).unwrap();
    }
    #[test]
    fn p_l2_both_bounds_apply_to_shared_table_and_lookup_is_charged_by_components() {
        let (c, _) = clock();
        let f = Filter::Context;
        let t = make(&f, Sort::Id, &[&id(65_536)]);
        let tokens: Vec<_> = (0..128).map(|_| issue(&c, &f, Sort::Id, &t)).collect();
        assert_eq!(state(&c), (128, 8_388_608));
        for token in &tokens {
            no_read(&c, token, &f, Sort::Id, 0).unwrap();
        }
        let newest = issue(&c, &f, Sort::Id, &t);
        refused(no_read(&c, &tokens[0], &f, Sort::Id, 0));
        for token in &tokens[1..] {
            no_read(&c, token, &f, Sort::Id, 0).unwrap();
        }
        no_read(&c, &newest, &f, Sort::Id, 0).unwrap();
        assert_eq!(state(&c), (128, 8_388_608));
        // A lookup entry shares the byte budget: admission evicts one full tuple.
        let g = Filter::Goals { status: None };
        let huge = make(&g, Sort::Default, &[&id(65_536), &id(65_536)]);
        issue(&c, &g, Sort::Default, &huge);
        assert_eq!(state(&c), (128, 127 * 65_536 + 104));
        let (c, _) = clock();
        let db = database();
        put(&db, "p", &g, &huge);
        let tokens: Vec<_> = (0..1025)
            .map(|_| issue(&c, &g, Sort::Default, &huge))
            .collect();
        assert_eq!(state(&c), (1024, 1024 * 104));
        refused(read(&c, &tokens[0], &g, Sort::Default, &db));
        for token in &tokens[1..] {
            read(&c, token, &g, Sort::Default, &db).unwrap();
        }
    }
    #[test]
    fn p_t1_p_t2_monotonic_age_equality_and_wall_clock_steps() {
        let (c, time) = clock();
        let f = Filter::Context;
        let t = make(&f, Sort::Id, &[&id(300)]);
        let token = issue(&c, &f, Sort::Id, &t);
        for wall in [-30_000, 30_000, 48 * 60 * 60 * 1000, i64::MIN, i64::MAX] {
            time.store(3_600_000, Ordering::SeqCst);
            assert_eq!(no_read(&c, &token, &f, Sort::Id, wall).unwrap(), t);
        }
        time.store(TTL.as_millis() as u64, Ordering::SeqCst);
        no_read(&c, &token, &f, Sort::Id, 0).unwrap();
        time.fetch_add(1, Ordering::SeqCst);
        refused(no_read(&c, &token, &f, Sort::Id, 0));
        assert_eq!(state(&c), (0, 0));
    }
    #[test]
    fn expired_lookup_entries_are_removed_on_insert_and_on_resume() {
        let (c, time) = clock();
        let f = Filter::Context;
        let db = database();
        let t = make(&f, Sort::Id, &[&id(65_537)]);
        put(&db, "p", &f, &t);
        let token = issue(&c, &f, Sort::Id, &t);
        time.store(TTL.as_millis() as u64, Ordering::SeqCst);
        read(&c, &token, &f, Sort::Id, &db).unwrap();
        time.fetch_add(1, Ordering::SeqCst);
        refused(read(&c, &token, &f, Sort::Id, &db));
        assert_eq!(state(&c), (0, 0));
        issue(&c, &f, Sort::Id, &t);
        time.fetch_add(TTL.as_millis() as u64 + 1, Ordering::SeqCst);
        issue(&c, &f, Sort::Id, &t);
        assert_eq!(state(&c), (1, 40));
    }
    #[test]
    fn c3_c4_p_x1_p_s1_p_t3_bindings_nonce_versions_and_edited_handles() {
        let (c, _) = clock();
        let f = Filter::Context;
        let t = make(&f, Sort::Id, &[&id(300)]);
        let token = issue(&c, &f, Sort::Id, &t);
        for f in [Filter::Config, Filter::Templates] {
            refused(no_read(&c, &token, &f, Sort::Id, 0));
        }
        refused(no_read(&c, &token, &f, Sort::Default, 0));
        let db = Connection::open_in_memory().unwrap();
        refused(c.resume(&token, "another", &f, Sort::Id, 0, &db, |_, _, _| panic!()));
        let new = NonTaskCursors::new("g");
        issue(&new, &f, Sort::Id, &t);
        refused(no_read(&new, &token, &f, Sort::Id, 0));
        let changed = NonTaskCursors::new("new-generation");
        refused(no_read(&changed, &token, &f, Sort::Id, 0));
        let raw = unhex(token.as_str()).unwrap();
        assert_eq!(raw.len(), 34);
        for index in [0, 1, 2, 17, 18, 25, 33] {
            let mut edited = raw.clone();
            edited[index] ^= 0x40;
            refused(no_read(
                &c,
                &Cursor::new(hex(&edited)).unwrap(),
                &f,
                Sort::Id,
                0,
            ));
        }
        for bytes in [raw[..33].to_vec(), [raw.as_slice(), &[0]].concat()] {
            refused(no_read(
                &c,
                &Cursor::new(hex(&bytes)).unwrap(),
                &f,
                Sort::Id,
                0,
            ));
        }
        // A live guessed handle of the same binding/sort is permitted, no MAC.
        let other = make(&f, Sort::Id, &[&id(302)]);
        let other_token = issue(&c, &f, Sort::Id, &other);
        let other_raw = unhex(other_token.as_str()).unwrap();
        let mut edited = raw.clone();
        edited[26..34].copy_from_slice(&other_raw[26..34]);
        assert_eq!(
            no_read(&c, &Cursor::new(hex(&edited)).unwrap(), &f, Sort::Id, 0).unwrap(),
            other
        );
        // A guessed live handle of another binding must still fail.
        let other_token = issue(&c, &Filter::Config, Sort::Id, &other);
        let other_raw = unhex(other_token.as_str()).unwrap();
        edited[26..34].copy_from_slice(&other_raw[26..34]);
        refused(no_read(
            &c,
            &Cursor::new(hex(&edited)).unwrap(),
            &f,
            Sort::Id,
            0,
        ));
    }
    // #384: Team Name and Default share a binding and two-text shape.
    #[test]
    fn team_held_cursor_rejects_another_sort_with_the_same_tuple_shape() {
        let (c, _) = clock();
        let f = Filter::Teams;
        for size in [300, 70_000] {
            let tuple = make(&f, Sort::Name, &[&id(size), "anchor"]);
            let db = database();
            put(&db, "p", &f, &tuple);
            let token = issue(&c, &f, Sort::Name, &tuple);
            assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), tuple);
            // Change only the public header: the stored sort must still bind it.
            let mut raw = unhex(token.as_str()).unwrap();
            raw[1] = NonTaskCursors::sort_tag(Sort::Default).unwrap();
            let edited = Cursor::new(hex(&raw)).unwrap();
            refused(read(&c, &edited, &f, Sort::Default, &db));
        }
    }

    #[test]
    fn invalid_token_checks_precede_clock_table_and_database_access() {
        let c =
            NonTaskCursors::with_clock("g", || panic!("invalid token must not touch clock/table"));
        let f = Filter::Context;
        for token in ["zz", "00", &"00".repeat(34), &"02".repeat(34)] {
            refused(no_read(
                &c,
                &Cursor::new(token.into()).unwrap(),
                &f,
                Sort::Id,
                0,
            ));
        }
    }
    #[test]
    fn p_r1_p_r2_q4_lookup_rejects_rename_same_length_edit_and_recreation() {
        let (c, _) = clock();
        let f = Filter::Teams;
        let db = database();
        let name = format!("{}{}", "p".repeat(60_000), "m".repeat(70_000));
        let t = make(&f, Sort::Name, &[&name, "anchor"]);
        put(&db, "p", &f, &t);
        let token = issue(&c, &f, Sort::Name, &t);
        for name in [format!("{}z", "p".repeat(130_000)), "z".repeat(130_000)] {
            let new = make(&f, Sort::Name, &[&name, "anchor"]);
            put(&db, "p", &f, &new);
            refused(read(&c, &token, &f, Sort::Name, &db));
        }
        delete(&db, &f, &t);
        let recreated = make(&f, Sort::Name, &["another", "anchor"]);
        put(&db, "p", &f, &recreated);
        refused(read(&c, &token, &f, Sort::Name, &db));
    }
    #[test]
    fn lookup_compares_every_integer_and_text_including_presence_and_position() {
        for f in filters() {
            for s in [
                Sort::Default,
                Sort::Name,
                Sort::CreatedAt,
                Sort::Attempt,
                Sort::Id,
            ] {
                let Some(l) = Layout::of(&f, s) else {
                    continue;
                };
                let count = l.parts().iter().filter(|p| p.kind == Kind::Text).count();
                let mut texts = vec!["x".to_owned(); count];
                texts[count - 1] = id(70_000);
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                let original = make(&f, s, &refs);
                let (c, _) = clock();
                let db = database();
                put(&db, "p", &f, &original);
                let token = issue(&c, &f, s, &original);
                assert_eq!(read(&c, &token, &f, s, &db).unwrap(), original);
                for (i, part) in l.parts().iter().enumerate() {
                    let mut changed = original.components().to_vec();
                    match &mut changed[i] {
                        Component::Int(n) => *n += 1,
                        Component::Text(t) => t.push('!'),
                    };
                    if part.kind == Kind::Presence {
                        changed[i + 1] = Component::Text(String::new());
                    }
                    let Some(changed) = Tuple::new(&l, changed) else {
                        continue;
                    };
                    delete(&db, &f, &original);
                    put(&db, "p", &f, &changed);
                    refused(read(&c, &token, &f, s, &db));
                    delete(&db, &f, &changed);
                    put(&db, "p", &f, &original);
                }
            }
        }
    }
    #[test]
    fn p_r3_q5_unrelated_edit_and_identical_recreation_resume() {
        let (c, _) = clock();
        let db = database();
        let f = Filter::Teams;
        let t = make(&f, Sort::Name, &["name", &id(70_000)]);
        put(&db, "p", &f, &t);
        let token = issue(&c, &f, Sort::Name, &t);
        // An out-of-order edit changes neither identity nor any ordering component.
        db.execute(
            "UPDATE teams SET unrelated='changed' WHERE project_id='p'",
            [],
        )
        .unwrap();
        assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), t);
        // Delete/recreate proves identity/order equality alone (no revision test).
        delete(&db, &f, &t);
        put(&db, "p", &f, &t);
        assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), t);
    }
    #[test]
    fn d1_s_d2_l_native_and_indirect_identity_scan_uses_byte_length_and_sha256() {
        for bytes in [60_000, 70_000] {
            let (c, _) = clock();
            let db = database();
            let f = Filter::Agents;
            let a = make(&f, Sort::Name, &[&id(10_000), &id(bytes)]);
            let b = make(&f, Sort::Name, &[&id(10_000), &"z".repeat(bytes)]);
            put(&db, "p", &f, &b);
            put(&db, "other-project", &f, &a);
            let token = issue(&c, &f, Sort::Name, &a);
            refused(read(&c, &token, &f, Sort::Name, &db));
            put(&db, "p", &f, &a);
            db.execute(
                "UPDATE agents SET unrelated='edited' WHERE project_id='p'",
                [],
            )
            .unwrap();
            assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), a);
            delete(&db, &f, &a);
            refused(read(&c, &token, &f, Sort::Name, &db));
        }
    }
    #[test]
    fn lookup_native_identity_byte_length_includes_nul_and_does_not_confuse_digest_text_with_form_one()
     {
        let (c, _) = clock();
        let db = database();
        let f = Filter::Teams;
        let t = make(&f, Sort::Name, &[&id(70_000), "é\0anchor"]);
        put(&db, "p", &f, &t);
        let token = issue(&c, &f, Sort::Name, &t);
        assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), t);
        let long = make(&f, Sort::Name, &["name", &id(70_000)]);
        let (_, digest) = stored(&long);
        let native = make(&f, Sort::Name, &["different", &digest]);
        put(&db, "p", &f, &native);
        put(&db, "p", &f, &long);
        let token = issue(&c, &f, Sort::Name, &long);
        assert_eq!(read(&c, &token, &f, Sort::Name, &db).unwrap(), long);
    }
    #[test]
    fn poison_recovery_keeps_old_positions_and_admits_new_ones() {
        let (c, _) = clock();
        let f = Filter::Context;
        let t = make(&f, Sort::Id, &[&id(300)]);
        let token = issue(&c, &f, Sort::Id, &t);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = c.held.state.lock().unwrap();
            panic!("synthetic poison");
        }));
        assert!(panicked.is_err());
        assert!(c.held.state.is_poisoned());
        assert_eq!(no_read(&c, &token, &f, Sort::Id, 0).unwrap(), t);
        let token = issue(&c, &f, Sort::Id, &t);
        assert_eq!(no_read(&c, &token, &f, Sort::Id, 0).unwrap(), t);
    }
    #[test]
    fn lookup_hydration_errors_and_identity_projection_damage_are_not_invalid_cursor() {
        let (c, _) = clock();
        let db = database();
        let f = Filter::Teams;
        let t = make(&f, Sort::Name, &["name", &id(70_000)]);
        put(&db, "p", &f, &t);
        let token = issue(&c, &f, Sort::Name, &t);
        assert!(matches!(
            c.resume(&token, "p", &f, Sort::Name, 0, &db, |_, _, _| Err(
                SqliteError::EntityAccess(std::io::ErrorKind::NotFound)
            )),
            Err(SqliteError::EntityAccess(std::io::ErrorKind::NotFound))
        ));
        let wrong = make(&f, Sort::Name, &["name", "wrong"]);
        assert!(matches!(
            c.resume(&token, "p", &f, Sort::Name, 0, &db, |_, _, _| Ok(
                wrong.clone()
            )),
            Err(SqliteError::EntityCorrupt {
                check: "identity projection",
                ..
            })
        ));
    }
    #[test]
    fn c1_c2_retained_boundary_survives_deleted_anchor_and_two_deleted_successors() {
        let (c, _) = clock();
        let f = Filter::Teams;
        let layout = Layout::of(&f, Sort::Name).unwrap();
        let prefix = "é".repeat(150);
        let rows: Vec<_> = (0..9)
            .map(|i| {
                make(
                    &f,
                    Sort::Name,
                    &[&format!("{prefix}{i}"), &format!("id-{i}")],
                )
            })
            .collect();
        let token = issue(&c, &f, Sort::Name, &rows[2]);
        let boundary = no_read(&c, &token, &f, Sort::Name, 0).unwrap();
        let survivors: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| ![2, 3, 4].contains(i))
            .map(|(_, t)| t)
            .collect();
        let after: Vec<_> = survivors
            .into_iter()
            .filter(|t| layout.compare(t, &boundary).is_gt())
            .collect();
        assert_eq!(after, rows[5..].iter().collect::<Vec<_>>());
    }

    #[test]
    fn same_view_lookup_uses_the_supplied_snapshot_and_not_a_fresh_connection() {
        let (c, _) = clock();
        let f = Filter::Teams;
        let t = make(&f, Sort::Name, &[&id(70_000), "anchor"]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("synthetic.sqlite");
        let fixture = database();
        put(&fixture, "p", &f, &t);
        fixture
            .backup(
                rusqlite::MAIN_DB,
                &path,
                None::<fn(rusqlite::backup::Progress)>,
            )
            .unwrap();
        let mut reader = Connection::open(&path).unwrap();
        reader.pragma_update(None, "journal_mode", "WAL").unwrap();
        let writer = Connection::open(&path).unwrap();
        let token = issue(&c, &f, Sort::Name, &t);
        let snapshot = reader.transaction().unwrap();
        assert_eq!(read(&c, &token, &f, Sort::Name, &snapshot).unwrap(), t);
        let changed = make(&f, Sort::Name, &[&"z".repeat(70_000), "anchor"]);
        put(&writer, "p", &f, &changed);
        assert_eq!(read(&c, &token, &f, Sort::Name, &snapshot).unwrap(), t);
        snapshot.commit().unwrap();
        refused(read(&c, &token, &f, Sort::Name, &reader));
    }
    #[test]
    fn synthetic_transaction_rollback_keeps_retained_position_and_refuses_missing_lookup_row() {
        let (c, _) = clock();
        let f = Filter::Teams;
        let retained = make(&f, Sort::Name, &[&id(300), "retained"]);
        let lookup = make(&f, Sort::Name, &[&id(70_000), "lookup"]);
        let mut db = database();
        let tx = db.transaction().unwrap();
        put(&tx, "p", &f, &retained);
        put(&tx, "p", &f, &lookup);
        let a = issue(&c, &f, Sort::Name, &retained);
        let b = issue(&c, &f, Sort::Name, &lookup);
        read(&c, &b, &f, Sort::Name, &tx).unwrap();
        tx.rollback().unwrap();
        assert_eq!(read(&c, &a, &f, Sort::Name, &db).unwrap(), retained);
        refused(read(&c, &b, &f, Sort::Name, &db));
    }
    #[test]
    fn lookup_scans_all_projects_family_rows_without_recipient_status_or_other_filter_columns() {
        // The fixture has no filter columns at all. A filtered candidates query
        // cannot serve this scan; only the bound project/family and tuple matter.
        for (f, s, texts) in [
            (
                Filter::PendingMessages {
                    recipient: "a".into(),
                    now: "n".into(),
                },
                Sort::Default,
                vec![id(40_000), id(40_000)],
            ),
            (
                Filter::Goals {
                    status: Some(orch_domain::GoalStatus::Active),
                },
                Sort::CreatedAt,
                vec![id(40_000), id(40_000)],
            ),
            (
                Filter::Runs {
                    task: Some("t".into()),
                    agent: Some("a".into()),
                },
                Sort::Attempt,
                vec![id(70_000)],
            ),
        ] {
            let (c, _) = clock();
            let db = database();
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            let t = make(&f, s, &refs);
            put(&db, "p", &f, &t);
            let token = issue(&c, &f, s, &t);
            assert_eq!(read(&c, &token, &f, s, &db).unwrap(), t);
        }
    }
    #[test]
    fn malformed_tuple_shape_and_serde_cursor_length_are_validated_before_use() {
        let (c, _) = clock();
        let team = make(&Filter::Teams, Sort::Name, &["name", "id"]);
        assert!(matches!(
            c.issue("p", &Filter::Context, Sort::Id, &team, 0),
            Err(SqliteError::Unrepresentable(_))
        ));
        assert_eq!(state(&c), (0, 0));
        let malformed: Cursor =
            serde_json::from_value(serde_json::Value::String("01".repeat(257))).unwrap();
        refused(no_read(&c, &malformed, &Filter::Context, Sort::Id, 0));
        let malformed: Cursor =
            serde_json::from_value(serde_json::Value::String(String::new())).unwrap();
        refused(no_read(&c, &malformed, &Filter::Context, Sort::Id, 0));
    }
    #[test]
    fn handle_wrap_never_reuses_a_live_position() {
        let (c, _) = clock();
        let f = Filter::Context;
        let a = make(&f, Sort::Id, &[&id(300)]);
        let first = issue(&c, &f, Sort::Id, &a);
        c.held.state.lock().unwrap().next = u64::MAX - 1;
        let last = issue(&c, &f, Sort::Id, &a);
        let zero = issue(&c, &f, Sort::Id, &a);
        let two = issue(&c, &f, Sort::Id, &a);
        assert_eq!(unhex(last.as_str()).unwrap()[26..], u64::MAX.to_be_bytes());
        assert_eq!(unhex(zero.as_str()).unwrap()[26..], 0u64.to_be_bytes());
        assert_eq!(unhex(two.as_str()).unwrap()[26..], 2u64.to_be_bytes());
        for token in [first, last, zero, two] {
            assert_eq!(no_read(&c, &token, &f, Sort::Id, 0).unwrap(), a);
        }
    }

    #[test]
    fn p_t3_live_handles_of_the_same_binding_and_sort_resume_across_arms() {
        let (c, _) = clock();
        let f = Filter::Context;
        let db = database();
        let retained = make(&f, Sort::Id, &[&id(300)]);
        let lookup = make(&f, Sort::Id, &[&id(70_000)]);
        put(&db, "p", &f, &lookup);
        let a = issue(&c, &f, Sort::Id, &retained);
        let b = issue(&c, &f, Sort::Id, &lookup);
        let original_a = unhex(a.as_str()).unwrap();
        let original_b = unhex(b.as_str()).unwrap();
        let mut edited_a = original_a.clone();
        edited_a[26..34].copy_from_slice(&original_b[26..34]);
        assert_eq!(
            read(&c, &Cursor::new(hex(&edited_a)).unwrap(), &f, Sort::Id, &db).unwrap(),
            lookup
        );
        let mut edited_b = original_b.clone();
        edited_b[26..34].copy_from_slice(&original_a[26..34]);
        assert_eq!(
            read(&c, &Cursor::new(hex(&edited_b)).unwrap(), &f, Sort::Id, &db).unwrap(),
            retained
        );
    }
}
