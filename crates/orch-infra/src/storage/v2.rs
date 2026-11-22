//! File reference for the revisioned storage port ([`orch_core::store`]).
//!
//! It exists so the conformance suite can run on a backend with real files and
//! no SQLite (spec R17). It is not a product backend: nothing opens it outside
//! tests, and the 1.0 stores in this module's parent are not changed by it.
//!
//! One document per project holds every row, event, gap, lease and counter, and
//! is replaced by a rename, so a commit is one atomic step. Writers take one gate,
//! as the port requires. A read loads its own copy of the document, so it keeps
//! one committed state whatever commits next. Surviving a process stop mid-way
//! is required of SQLite only (R17); the rename is only there so a reader never
//! sees half a commit. A document that does not parse is an error, never an
//! empty project: a later write would turn corruption into loss.
//!
//! The daemon's lineage is daemon-wide, so it is one small file at the root, and
//! [`FileStorage::restore_to_lineage`] is the fixture's stand-in for a restore.
//!
//! Cursors are tokens into a table held by the instance: they name the sort key
//! of the last row returned and the project, filter and sort they were issued
//! for, and no snapshot. They do not outlive the instance, and the table is
//! never trimmed.
//!
//! What it does not guarantee, because it is a test reference and not a
//! product backend: it trusts the root it is given (a symlinked document or
//! project directory is followed, and the document is created 0600 in a
//! directory with the process umask); a load reads the whole document with no
//! size cap, and a write rewrites all of it; a document that was removed reads
//! as a project that was never written, so positions and lease epochs start
//! again at 1; the rename is not preceded by a sync; and a hand-edited document
//! is checked only for a row listed twice, a position past its counter, and a
//! lease past its epoch counter or sharing an agent or a run, not for two task
//! ids with one fold key, two rows at one position, or events of no known run.

mod events;
mod leases;
mod order;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use orch_core::Error;
use orch_core::store::events::{
    AppendResult, EventGap, EventRead, EventWrite, PruneResult, SequencedEvent,
};
use orch_core::store::leases::{ClaimOutcome, Lease, LeaseRead, LeaseWrite, NewLease};
use orch_core::store::{
    Caller, Cursor, Entity, EntityKind, Filter, Lane, Page, PageRequest, ReadSnapshot, Record,
    Result, Revision, Sort, Storage, StoreError, UnitOfWork, WriteWork,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use events::Events;
use leases::{By, Leases};
use order::{Key, check_sort, key, matches};

const DOCUMENT: &str = "state.json";
const LINEAGE: &str = "lineage";
/// The lineage of a root that no restore has touched.
const FIRST_LINEAGE: &str = "file-lineage-1";

/// Rows of one project, in memory.
#[derive(Clone, Default)]
struct Project {
    rows: BTreeMap<(EntityKind, String), Record>,
    /// The last position handed out per family. Never lowered, so a deletion
    /// does not free a position.
    counters: BTreeMap<EntityKind, u64>,
    events: Events,
    leases: Leases,
}

#[derive(Serialize, Deserialize)]
struct Document {
    counters: BTreeMap<EntityKind, u64>,
    records: Vec<Record>,
    #[serde(default)]
    events: Events,
    #[serde(default)]
    leases: Leases,
}

struct Bookmark {
    project: String,
    filter: Filter,
    sort: Sort,
    key: Key,
}

#[derive(Default)]
struct Cursors {
    issued: u64,
    entries: HashMap<String, Bookmark>,
}

/// The file reference: one directory per project below `root`.
pub struct FileStorage {
    root: PathBuf,
    writer: Arc<AsyncMutex<()>>,
    cursors: Arc<Mutex<Cursors>>,
}

impl FileStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            writer: Arc::default(),
            cursors: Arc::default(),
        }
    }

    /// The project id is data, never a path: its bytes are written in hex, so
    /// no id can name a parent, a separator or another project's directory.
    fn project_dir(&self, project: &str) -> PathBuf {
        let hex: String = project.bytes().map(|b| format!("{b:02x}")).collect();
        self.root.join(format!("p-{hex}"))
    }

    /// Fixture authority only: stands for a restore, which starts a new lineage,
    /// ends every project's leases and keeps every epoch counter (R77, R138).
    /// It takes the writer gate, so it never interleaves with a commit.
    pub async fn restore_to_lineage(&self, lineage: &str) -> Result<()> {
        let root = self.root.clone();
        let lineage = lineage.to_owned();
        let gate = self.writer.clone().lock_owned().await;
        blocking(move || {
            let _gate = gate;
            let entries = match std::fs::read_dir(&root) {
                Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(e),
            }
            .map_err(|e| io_error("cannot list", &root, e))?;
            for entry in entries {
                let dir = entry.path();
                if !entry.file_name().to_string_lossy().starts_with("p-") || !dir.is_dir() {
                    continue;
                }
                let mut project = load(&dir)?;
                if !project.leases.is_empty() {
                    project.leases.end_all();
                    persist(&dir, &project)?;
                }
            }
            write_lineage(&root, &lineage)
        })
        .await
    }
}

fn read_lineage(root: &Path) -> Result<String> {
    let path = root.join(LINEAGE);
    match std::fs::read_to_string(&path) {
        Ok(lineage) => Ok(lineage),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FIRST_LINEAGE.to_owned()),
        Err(e) => Err(io_error("cannot read", &path, e)),
    }
}

fn write_lineage(root: &Path, lineage: &str) -> Result<()> {
    std::fs::create_dir_all(root).map_err(|e| io_error("cannot create", root, e))?;
    let mut temp =
        tempfile::NamedTempFile::new_in(root).map_err(|e| io_error("cannot write in", root, e))?;
    std::io::Write::write_all(&mut temp, lineage.as_bytes())
        .map_err(|e| io_error("cannot write", temp.path(), e))?;
    let path = root.join(LINEAGE);
    temp.persist(&path)
        .map_err(|e| io_error("cannot replace", &path, e.error))?;
    Ok(())
}

fn io_error(context: &str, path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Backend(Error::Io {
        context: format!("{context} {}", path.display()),
        path: Some(path.to_path_buf()),
        source,
    })
}

fn unparsable(path: &Path, source: serde_json::Error) -> StoreError {
    StoreError::Backend(Error::serde(
        "cannot parse the storage document",
        path,
        source,
    ))
}

fn corrupt(path: &Path, detail: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(Error::other(format!(
        "{} is not a valid storage document: {detail}",
        path.display()
    )))
}

fn load(dir: &Path) -> Result<Project> {
    let path = dir.join(DOCUMENT);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Project::default()),
        Err(e) => return Err(io_error("cannot read", &path, e)),
    };
    let document: Document = serde_json::from_slice(&bytes).map_err(|e| unparsable(&path, e))?;
    let mut project = Project {
        rows: BTreeMap::new(),
        counters: document.counters,
        events: document.events,
        leases: document.leases,
    };
    project
        .leases
        .validate()
        .map_err(|detail| corrupt(&path, detail))?;
    for record in document.records {
        let kind = record.entity.kind();
        let id = record.entity.id().to_owned();
        if record.position > project.counters.get(&kind).copied().unwrap_or(0) {
            return Err(corrupt(&path, format!("{kind:?} {id} is past its counter")));
        }
        if project.rows.insert((kind, id.clone()), record).is_some() {
            return Err(corrupt(&path, format!("{kind:?} {id} appears twice")));
        }
    }
    Ok(project)
}

/// Replaces the document by a rename of a sibling file, which only this process
/// writes and which a failed run leaves behind at most as a stray temp file.
fn persist(dir: &Path, project: &Project) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io_error("cannot create", dir, e))?;
    let document = Document {
        counters: project.counters.clone(),
        records: project.rows.values().cloned().collect(),
        events: project.events.clone(),
        leases: project.leases.clone(),
    };
    let temp =
        tempfile::NamedTempFile::new_in(dir).map_err(|e| io_error("cannot write in", dir, e))?;
    // `Document` always serializes, so a failure here is the temp file's I/O.
    serde_json::to_writer_pretty(&temp, &document)
        .map_err(|e| io_error("cannot write", temp.path(), e.into()))?;
    let path = dir.join(DOCUMENT);
    temp.persist(&path)
        .map_err(|e| io_error("cannot replace", &path, e.error))?;
    Ok(())
}

/// Runs blocking file work off the async threads. A panic in the work (a test
/// assertion in a callback) reaches the caller as that panic.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(StoreError::Backend(Error::other(
            "storage work was cancelled",
        ))),
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn read(&self, caller: Caller) -> Result<Box<dyn ReadSnapshot>> {
        let dir = self.project_dir(&caller.project);
        let root = self.root.clone();
        let cursors = self.cursors.clone();
        let session = blocking(move || {
            Ok(Session {
                lineage: read_lineage(&root)?,
                project: load(&dir)?,
                caller,
                dirty: false,
                cursors,
            })
        })
        .await?;
        Ok(Box::new(session))
    }

    async fn write(&self, caller: Caller, _lane: Lane, work: WriteWork) -> Result<()> {
        let dir = self.project_dir(&caller.project);
        let root = self.root.clone();
        let cursors = self.cursors.clone();
        // The guard moves into the blocking work: a caller that stops waiting
        // does not release the gate while the work still runs.
        let gate = self.writer.clone().lock_owned().await;
        blocking(move || {
            let _gate = gate;
            let mut session = Session {
                lineage: read_lineage(&root)?,
                project: load(&dir)?,
                caller,
                dirty: false,
                cursors,
            };
            work(&mut session)?;
            if session.dirty {
                persist(&dir, &session.project)?;
            }
            Ok(())
        })
        .await
    }
}

/// One project as of one moment: a read's private snapshot, or a write's working
/// copy, which is dropped unless the work returned `Ok`.
struct Session {
    caller: Caller,
    /// The daemon's lineage as of this session's start.
    lineage: String,
    project: Project,
    dirty: bool,
    cursors: Arc<Mutex<Cursors>>,
}

impl Session {
    fn row(&self, kind: EntityKind, id: &str) -> Option<Record> {
        self.project.rows.get(&(kind, id.to_owned())).cloned()
    }

    /// The stored row, when `expected` is its revision.
    fn current(&self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record> {
        match self.row(kind, id) {
            Some(row) if row.revision == expected => Ok(row),
            other => Err(StoreError::StaleRevision {
                kind,
                id: id.to_owned(),
                expected,
                actual: other.map(|row| row.revision),
            }),
        }
    }

    fn next_position(&self, kind: EntityKind) -> Result<u64> {
        let last = self.project.counters.get(&kind).copied().unwrap_or(0);
        last.checked_add(1).ok_or(StoreError::CounterExhausted)
    }

    fn put(&mut self, record: Record) -> Record {
        let kind = record.entity.kind();
        let counter = self.project.counters.entry(kind).or_default();
        *counter = (*counter).max(record.position);
        self.project
            .rows
            .insert((kind, record.entity.id().to_owned()), record.clone());
        self.dirty = true;
        record
    }

    fn page(&self, filter: &Filter, request: &PageRequest) -> Result<Page<Record>> {
        request.validate()?;
        check_sort(filter.kind(), request.sort)?;
        let after = match &request.cursor {
            Some(cursor) => Some(self.resume(cursor, filter, request.sort)?),
            None => None,
        };
        let mut rows: Vec<(Key, &Record)> = self
            .project
            .rows
            .values()
            .filter(|row| matches(&row.entity, filter))
            .map(|row| (key(row, request.sort), row))
            .collect();
        let total = request.with_count.then_some(rows.len() as u64);
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let start = after.map_or(0, |after| rows.partition_point(|(k, _)| *k <= after));
        let rest = &rows[start..];
        let shown = &rest[..rest.len().min(request.limit)];
        let next_cursor = match shown.last() {
            Some((last, _)) if rest.len() > shown.len() => {
                Some(self.bookmark(filter, request.sort, last.clone())?)
            }
            _ => None,
        };
        Ok(Page {
            items: shown.iter().map(|(_, row)| (*row).clone()).collect(),
            next_cursor,
            total,
        })
    }

    fn resume(&self, cursor: &Cursor, filter: &Filter, sort: Sort) -> Result<Key> {
        let cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        match cursors.entries.get(cursor.as_str()) {
            Some(b)
                if b.project == self.caller.project && b.filter == *filter && b.sort == sort =>
            {
                Ok(b.key.clone())
            }
            _ => Err(StoreError::InvalidCursor),
        }
    }

    fn bookmark(&self, filter: &Filter, sort: Sort, key: Key) -> Result<Cursor> {
        let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        cursors.issued = cursors
            .issued
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)?;
        let token = format!("file-{}", cursors.issued);
        cursors.entries.insert(
            token.clone(),
            Bookmark {
                project: self.caller.project.clone(),
                filter: filter.clone(),
                sort,
                key,
            },
        );
        Cursor::new(token)
    }
}

#[async_trait]
impl ReadSnapshot for Session {
    fn caller(&self) -> &Caller {
        &self.caller
    }
    async fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        Ok(self.row(kind, id))
    }
    async fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>> {
        self.page(filter, page)
    }
}

impl UnitOfWork for Session {
    fn caller(&self) -> &Caller {
        &self.caller
    }
    fn get(&self, kind: EntityKind, id: &str) -> Result<Option<Record>> {
        Ok(self.row(kind, id))
    }
    fn list(&self, filter: &Filter, page: &PageRequest) -> Result<Page<Record>> {
        self.page(filter, page)
    }

    fn create(&mut self, entity: Entity) -> Result<Record> {
        let (kind, id) = (entity.kind(), entity.id());
        if self.row(kind, id).is_some() {
            return Err(StoreError::AlreadyExists {
                kind,
                id: id.to_owned(),
            });
        }
        // R117: task ids that differ only in ASCII case are one id. The ids are
        // compared and never rewritten.
        if kind == EntityKind::Task
            && let Some(existing) =
                self.project.rows.values().find(|row| {
                    row.entity.kind() == kind && row.entity.id().eq_ignore_ascii_case(id)
                })
        {
            return Err(StoreError::IdAlias {
                existing: existing.entity.id().to_owned(),
                requested: id.to_owned(),
            });
        }
        let position = self.next_position(kind)?;
        Ok(self.put(Record {
            entity,
            revision: Revision::ONE,
            position,
        }))
    }

    fn update(&mut self, entity: Entity, expected: Revision) -> Result<Record> {
        let old = self.current(entity.kind(), entity.id(), expected)?;
        let revision = old.revision.next()?;
        Ok(self.put(Record {
            entity,
            revision,
            position: old.position,
        }))
    }

    fn delete(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<()> {
        self.current(kind, id, expected)?;
        self.project.rows.remove(&(kind, id.to_owned()));
        self.dirty = true;
        Ok(())
    }

    fn move_to_end(&mut self, kind: EntityKind, id: &str, expected: Revision) -> Result<Record> {
        let old = self.current(kind, id, expected)?;
        let revision = old.revision.next()?;
        let position = self.next_position(kind)?;
        Ok(self.put(Record {
            revision,
            position,
            ..old
        }))
    }
}

#[async_trait]
impl LeaseRead for Session {
    async fn read_lease(&self, task: &str) -> Result<Option<Lease>> {
        Ok(self.project.leases.by_task(task))
    }
    async fn read_agent_lease(&self, agent: &str) -> Result<Option<Lease>> {
        Ok(self.project.leases.by_agent(agent))
    }
    async fn read_leases(&self) -> Result<Vec<Lease>> {
        Ok(self.project.leases.all())
    }
}

impl LeaseWrite for Session {
    fn claim_lease(&mut self, new: &NewLease, now_ms: i64) -> Result<ClaimOutcome> {
        let outcome = self.project.leases.claim(&self.lineage, new, now_ms)?;
        self.dirty |= matches!(outcome, ClaimOutcome::Granted(_));
        Ok(outcome)
    }

    fn renew_lease(&mut self, task: &str, lineage: &str, epoch: u64, now_ms: i64) -> Result<Lease> {
        let lease = self.project.leases.renew(
            &self.caller,
            &self.lineage,
            task,
            (lineage, epoch),
            now_ms,
        )?;
        self.dirty = true;
        Ok(lease)
    }

    fn release_lease(&mut self, task: &str, lineage: &str, epoch: u64) -> Result<Lease> {
        let lease =
            self.project
                .leases
                .release(&self.caller, &self.lineage, task, (lineage, epoch))?;
        self.dirty = true;
        Ok(lease)
    }

    fn get_lease(&self, task: &str) -> Result<Option<Lease>> {
        Ok(self.project.leases.by_task(task))
    }
    fn get_agent_lease(&self, agent: &str) -> Result<Option<Lease>> {
        Ok(self.project.leases.by_agent(agent))
    }
    fn list_leases(&self) -> Result<Vec<Lease>> {
        Ok(self.project.leases.all())
    }
}

#[async_trait]
impl EventRead for Session {
    async fn read_events(
        &self,
        run: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<SequencedEvent>> {
        self.project.events.read(run, after_seq, limit)
    }
    async fn read_events_tail(&self, run: &str, count: usize) -> Result<Vec<SequencedEvent>> {
        self.project.events.tail(run, count)
    }
    async fn read_event_gaps(&self, run: &str) -> Result<Vec<EventGap>> {
        Ok(self.project.events.gaps(run))
    }
}

impl EventWrite for Session {
    fn append_events(
        &mut self,
        run: &str,
        lineage: &str,
        epoch: u64,
        batch: &[SequencedEvent],
    ) -> Result<AppendResult> {
        // The fence comes first, so an empty or duplicate batch is refused too.
        self.project
            .leases
            .check(&self.caller, &self.lineage, By::Run(run), lineage, epoch)?;
        let result = self.project.events.append(run, batch)?;
        self.dirty |= result.stored > 0;
        Ok(result)
    }

    fn record_event_gap(&mut self, run: &str, after_seq: u64, dropped: u64) -> Result<()> {
        self.dirty |= self.project.events.gap(run, after_seq, dropped)?;
        Ok(())
    }

    fn prune_events(&mut self, run: &str, max_rows: usize) -> Result<PruneResult> {
        let result = self.project.events.prune(run, max_rows)?;
        self.dirty |= result.deleted > 0;
        Ok(result)
    }
}
