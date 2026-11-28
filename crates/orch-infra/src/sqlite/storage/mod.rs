//! Owned SQLite storage composition. The daemon continues using files until T24.
#[cfg(test)]
mod containment_tests;
mod page;
#[cfg(test)]
mod page_tests;
mod ports;
#[cfg(test)]
mod prior_latch_tests;
mod retained;
#[cfg(test)]
mod tests;
pub(in crate::sqlite) mod view;
pub(in crate::sqlite) use retained::UnitWork;
pub use retained::{RetainedWrite, WriteAttempt};

use super::{
    ConnectionRole, FailureSource, FailureState, ReadPool, Result, SqliteError, Synchronous,
    Writer, WriterHandle, migrations, open_connection,
};
use super::{non_task_cursor::NonTaskCursors, task_adapter::TaskCursors};
use crate::{blob::DiskBlobStore, data_dir::DataDir};
use async_trait::async_trait;
use orch_core::{
    clock::Clock,
    store::{
        self, Caller, Entity, EntityKind, Filter, Page, PageRequest, ReadSnapshot, Record,
        Revision, Storage, StoreError, WriteWork,
    },
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};
use view::{ReadView, Unit};

/// Construction inputs, supplied by the daemon after acquiring its data lock.
/// Generation must change after restore and on each daemon start.
pub struct StorageOptions {
    pub generation: String,
    pub synchronous: Synchronous,
    pub read_connections: usize,
    pub writer_capacity: usize,
    pub snapshot_idle: Duration,
    pub clock: Arc<dyn Clock>,
    #[cfg(test)]
    pub(crate) reader_setup: Option<ReaderSetup>,
    #[cfg(test)]
    pub(crate) reader_progress: Option<Box<dyn FnMut() -> bool + Send>>,
    #[cfg(test)]
    pub(crate) writer_setup: Option<WriterSetup>,
    #[cfg(test)]
    pub(crate) writer_commit_probe: Option<super::writer::CommitProbe>,
    #[cfg(test)]
    pub(crate) completion_probe: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(crate) pin_now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
    // Fixtures testing corruption/cleanup opt out of incidental admission expiry.
    #[cfg(test)]
    pub(crate) unbounded_read_admission: bool,
    #[cfg(test)]
    pub(crate) write_admission: Option<WriteAdmission>,
    #[cfg(test)]
    pub(crate) write_observation_probe: Option<Arc<dyn Fn() -> Option<SqliteError> + Send + Sync>>,
}
#[cfg(test)]
type WriteAdmission = Arc<dyn Fn(Instant, Option<Instant>) -> Option<Instant> + Send + Sync>;
#[cfg(test)]
type ReaderSetup = Box<dyn Fn(&mut rusqlite::Connection) + Send>;
#[cfg(test)]
type WriterSetup = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;
impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            generation: nanoid::nanoid!(),
            synchronous: Synchronous::Normal,
            read_connections: 4,
            writer_capacity: 64,
            snapshot_idle: Duration::from_secs(5),
            clock: Arc::new(crate::clock::SystemClock),
            #[cfg(test)]
            reader_setup: None,
            #[cfg(test)]
            reader_progress: None,
            #[cfg(test)]
            writer_setup: None,
            #[cfg(test)]
            writer_commit_probe: None,
            #[cfg(test)]
            completion_probe: None,
            #[cfg(test)]
            pin_now: None,
            #[cfg(test)]
            unbounded_read_admission: false,
            #[cfg(test)]
            write_admission: None,
            #[cfg(test)]
            write_observation_probe: None,
        }
    }
}
struct Shared {
    state: FailureState,
    disk: DiskBlobStore,
    retry_admission: orch_core::store::retries::stream::RetryStreamAdmission,
    task_cursors: TaskCursors,
    cursors: NonTaskCursors,
    clock: Arc<dyn Clock>,
    stopping: AtomicBool,
    interrupts: Mutex<Vec<Option<rusqlite::InterruptHandle>>>,
    idle: Duration,
    active_reads: Arc<super::read_pool::ActiveReads>,
    #[cfg(test)]
    completion_probe: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pin_now: Option<Arc<dyn Fn() -> Instant + Send + Sync>>,
    #[cfg(test)]
    unbounded_read_admission: bool,
    #[cfg(test)]
    write_admission: Option<WriteAdmission>,
    #[cfg(test)]
    write_observation_probe: Option<Arc<dyn Fn() -> Option<SqliteError> + Send + Sync>>,
}
impl Shared {
    fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        for handle in self
            .interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flatten()
        {
            handle.interrupt();
        }
    }
    /// Admission of a write: refused once the state is latched (R128) or closing.
    fn available(&self) -> Result<()> {
        self.state.ensure_available()?;
        self.readable()
    }
    /// Admission of a snapshot or of a call on one. A latch set before it does not
    /// refuse a read (R128 lists mutations, claims and checkpoints; issue 389).
    fn readable(&self) -> Result<()> {
        if self.stopping.load(Ordering::SeqCst) {
            Err(SqliteError::WriterClosed)
        } else {
            Ok(())
        }
    }
    /// A fatal observation since `since`, on any connection, may mean that what the
    /// snapshot reads is damaged, as for `ReadPool::read`: refuse its value.
    fn refuse_fresh_fatal(&self, since: u64) -> Result<()> {
        if self.state.fatal_observations() == since {
            Ok(())
        } else {
            self.state.ensure_available()
        }
    }
}
struct Owner {
    shared: Arc<Shared>,
    data: Mutex<Option<Arc<DataDir>>>,
    sync: Synchronous,
    writer: WriterHandle,
    reads: mpsc::SyncSender<OpenRead>,
    stop: mpsc::SyncSender<()>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Owner {
    fn stop(&self) {
        self.shared.stop();
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.writer.close();
        let _ = self.stop.try_send(());
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Owned entry point. Neither connection nor transaction crosses its worker
/// boundary. The last storage client stops the workers even if snapshots remain.
/// External `Arc<DataDir>` clones retain their own directory lock ownership.
#[derive(Clone)]
pub struct SqliteStorage {
    owner: Arc<Owner>,
    _life: Arc<ClientLife>,
}
// Snapshot handles retain the reply state, not write admission. The last
// Storage handle stops workers even when a client leaks a snapshot handle.
struct ClientLife(Arc<Owner>);
impl Drop for ClientLife {
    fn drop(&mut self) {
        self.0.stop();
    }
}
impl SqliteStorage {
    pub fn open(data: Arc<DataDir>, options: StorageOptions) -> Result<Self> {
        if options.read_connections == 0
            || options.writer_capacity == 0
            || options.snapshot_idle.is_zero()
        {
            return Err(SqliteError::InvalidCapacity);
        }
        let owner_data = data.clone();
        let owner_sync = options.synchronous;
        let (started, ready) = mpsc::sync_channel(1);
        let (stop, stopped) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("sqlite-owner".into())
            .spawn(move || {
                let setup = (|| {
                    let mut conn =
                        open_connection(&data, ConnectionRole::Writer, options.synchronous)?;
                    migrations::migrate(&data, &mut conn)?;
                    drop(conn);
                    let state = FailureState::new();
                    let shared = Arc::new(Shared {
                        disk: DiskBlobStore::open(data.root())
                            .map_err(SqliteError::EntityPublication)?,
                        retry_admission: Default::default(),
                        state: state.clone(),
                        task_cursors: TaskCursors::new(&options.generation),
                        cursors: NonTaskCursors::new(options.generation),
                        clock: options.clock,
                        stopping: AtomicBool::new(false),
                        interrupts: Mutex::new(
                            (0..options.read_connections).map(|_| None).collect(),
                        ),
                        idle: options.snapshot_idle,
                        active_reads: Arc::new(super::read_pool::ActiveReads::default()),
                        #[cfg(test)]
                        completion_probe: options.completion_probe,
                        #[cfg(test)]
                        pin_now: options.pin_now,
                        #[cfg(test)]
                        unbounded_read_admission: options.unbounded_read_admission,
                        #[cfg(test)]
                        write_admission: options.write_admission,
                        #[cfg(test)]
                        write_observation_probe: options.write_observation_probe,
                    });
                    let mut pool = ReadPool::open_with_state(
                        &data,
                        options.synchronous,
                        options.read_connections,
                        state.clone(),
                    )?;
                    pool.active = shared.active_reads.clone();
                    #[cfg(test)]
                    if let Some(setup) = options.reader_setup {
                        pool.configure_test_readers(setup);
                    }
                    #[cfg(test)]
                    if let Some(probe) = options.reader_progress {
                        pool.configure_progress_for_test(probe);
                    }
                    let (writer, handle) = Writer::open_with_state(
                        &data,
                        options.synchronous,
                        options.writer_capacity,
                        state,
                    )?;
                    #[cfg(test)]
                    let writer = {
                        let mut writer = writer;
                        if let Some(probe) = options.writer_commit_probe {
                            writer.set_commit_probe(probe);
                        }
                        if let Some(setup) = options.writer_setup {
                            setup(writer.connection_for_test());
                        }
                        writer
                    };
                    let (reads, requests) = mpsc::sync_channel(options.read_connections);
                    let requests = Mutex::new(requests);
                    std::thread::scope(|scope| {
                        // A construction unwind must stop already spawned workers
                        // before the scope waits to join them.
                        struct StopWorkers<'a>(&'a Shared, &'a WriterHandle);
                        impl Drop for StopWorkers<'_> {
                            fn drop(&mut self) {
                                self.0.stop();
                                self.1.close();
                            }
                        }
                        let _stop = StopWorkers(&shared, &handle);
                        scope.spawn(move || writer.run());
                        for index in 0..options.read_connections {
                            let shared = shared.clone();
                            let requests = &requests;
                            let pool = &pool;
                            scope.spawn(move || read_worker(pool, requests, &shared, index));
                        }
                        // A cancelled constructor must close admission so the scoped
                        // workers can exit; no thread or directory lock is leaked.
                        if started
                            .send(Ok((shared.clone(), handle.clone(), reads)))
                            .is_ok()
                        {
                            let _ = stopped.recv();
                        }
                        shared.stop();
                        handle.close();
                    });
                    Ok::<_, SqliteError>(())
                })();
                if let Err(error) = setup {
                    let _ = started.send(Err(error));
                }
            })?;
        match ready.recv().map_err(|_| SqliteError::WriterClosed)? {
            Ok((shared, writer, reads)) => {
                let owner = Arc::new(Owner {
                    data: Mutex::new(Some(owner_data)),
                    sync: owner_sync,
                    shared,
                    writer,
                    reads,
                    stop,
                    thread: Mutex::new(Some(thread)),
                });
                Ok(Self {
                    owner: owner.clone(),
                    _life: Arc::new(ClientLife(owner)),
                })
            }
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    /// One passive checkpoint on its own connection, followed by R127 reader
    /// interruption from the actual backlog. T15 owns when this is scheduled.
    pub async fn checkpoint(&self) -> Result<super::CheckpointObservation> {
        self.owner.shared.available()?;
        let owner = self.owner.clone();
        tokio::task::spawn_blocking(move || {
            owner.shared.available()?;
            let data = owner
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .ok_or(SqliteError::WriterClosed)?;
            owner
                .shared
                .active_reads
                .checkpoint(&data, owner.sync, &owner.shared.state)
        })
        .await
        .map_err(|_| SqliteError::WriterClosed)?
    }
    pub fn failure_state(&self) -> FailureState {
        self.owner.shared.state.clone()
    }
    /// Stop admission, interrupt readers, drain admitted writes and join every
    /// worker off the runtime. Safe even when other handles still exist. External
    /// `Arc<DataDir>` clones must also be dropped to release the directory lock.
    pub async fn close(&self) -> store::Result<()> {
        self.owner.stop();
        let owner = self.owner.clone();
        tokio::task::spawn_blocking(move || {
            // Serialize concurrent close callers until the join completes.
            let mut slot = owner
                .thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(thread) = slot.take() {
                thread
                    .join()
                    .map_err(|_| backend("SQLite worker panicked"))?;
            }
            Ok::<(), StoreError>(())
        })
        .await
        .map_err(|_| backend("SQLite close worker failed"))??;
        Ok(())
    }
}
fn backend(message: &str) -> StoreError {
    StoreError::Backend(orch_core::Error::other(message))
}
fn port<T>(result: Result<T>) -> store::Result<T> {
    result.map_err(|e| match e {
        SqliteError::Store(e) => e,
        SqliteError::Unrepresentable(_) => StoreError::Unrepresentable,
        SqliteError::StorageUnavailable(_) => backend("storage_unavailable"),
        SqliteError::Overloaded => backend("overloaded"),
        SqliteError::SnapshotExpired => backend("snapshot_expired"),
        SqliteError::WriterClosed => backend("SQLite writer is closed"),
        _ => backend("SQLite storage operation failed"),
    })
}
// A fatal observation must be visible in the first public reply too. Keep the
// original SQLite error on the worker/pool path for classification and retirement.
fn public_result<T>(state: &FailureState, result: store::Result<T>) -> store::Result<T> {
    port(state.ensure_available())?;
    result
}
const SNAPSHOT_CALL_CAPACITY: usize = 16;
type RetiredReply = Box<dyn FnOnce() + Send>;
type ReadJob = Box<dyn FnOnce(&ReadView<'_, '_>, &mut Option<RetiredReply>) -> Result<()> + Send>;
struct OpenRead {
    expired: Arc<AtomicBool>,
    caller: Caller,
    deadline: Instant,
    commands: mpsc::Receiver<ReadJob>,
    ready: oneshot::Sender<Result<()>>,
}
struct Snapshot {
    expired: Arc<AtomicBool>,
    owner: Arc<Owner>,
    caller: Caller,
    commands: mpsc::SyncSender<ReadJob>,
    capacity: Arc<Semaphore>,
    /// `FailureState::fatal_observations` before the snapshot was requested.
    observed: u64,
}
impl Snapshot {
    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ReadView<'_, '_>) -> Result<T> + Send + 'static,
    ) -> store::Result<T> {
        port(self.owner.shared.readable())?;
        if self.expired.load(Ordering::SeqCst) {
            return port(Err(SqliteError::SnapshotExpired));
        }
        // Bound queued/running jobs without blocking an async caller. Recheck
        // stop/fatal state after waiting: admission may have closed meanwhile.
        let capacity = self
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| backend("snapshot_expired"))?;
        port(self.owner.shared.readable())?;
        let (reply, answer) = oneshot::channel();
        let observed = self.observed;
        let expired = self.expired.clone();
        let job: ReadJob = Box::new(move |view, retired_reply| {
            let _capacity = capacity;
            // Only the reader operation is contained. An adapter panic retires
            // this view, but is not evidence of database corruption (R140).
            let (result, panicked) =
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(view))) {
                    Ok(result) => (view.observed(result), false),
                    Err(_) => (
                        Err(SqliteError::Store(backend(
                            "SQLite reader operation panicked",
                        ))),
                        true,
                    ),
                };
            let result = if expired.load(Ordering::SeqCst) {
                Err(SqliteError::SnapshotExpired)
            } else {
                result
            };
            let result = result.and_then(|value| {
                view.shared.refuse_fresh_fatal(observed)?;
                Ok(value)
            });
            match result {
                Ok(value) => {
                    let _ = reply.send(Ok(value));
                    Ok(())
                }
                Err(SqliteError::Store(error))
                    if !panicked && view.shared.refuse_fresh_fatal(observed).is_ok() =>
                {
                    let _ = reply.send(Err(error));
                    Ok(())
                }
                Err(error) => {
                    let snapshot_expired = matches!(error, SqliteError::SnapshotExpired);
                    let state = view.shared.state.clone();
                    // Answer only after ReadPool has classified the original
                    // error, retired the connection and updated its one streak.
                    *retired_reply = Some(Box::new(move || {
                        let message = if state.fatal_observations() != observed {
                            "storage_unavailable"
                        } else if snapshot_expired {
                            "snapshot_expired"
                        } else if panicked {
                            "SQLite reader operation panicked"
                        } else {
                            "SQLite storage operation failed"
                        };
                        let _ = reply.send(Err(backend(message)));
                    }));
                    Err(error)
                }
            }
        });
        self.commands.try_send(job).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => backend("overloaded"),
            mpsc::TrySendError::Disconnected(_) => backend("snapshot_expired"),
        })?;
        match answer.await {
            Ok(result) => result,
            Err(_) => Err(self.dropped()),
        }
    }
    /// The answer of an admitted call whose job was dropped unanswered because
    /// the worker left the snapshot with it still queued. A fatal result is
    /// classified and its connection retired before the worker drops the
    /// channel, so a fatal observation since the snapshot was requested is
    /// visible here and every sibling answers as the failing call did (spec,
    /// "A pooled read after a writer latch"). Otherwise the view only ended (idle
    /// or checkpoint expiry, or close). A call made after the view ended is
    /// refused at `try_send` and always answers `snapshot_expired`.
    fn dropped(&self) -> StoreError {
        port(self.owner.shared.refuse_fresh_fatal(self.observed))
            .err()
            .unwrap_or_else(|| backend("snapshot_expired"))
    }
}
fn read_worker(
    pool: &ReadPool<'_>,
    requests: &Mutex<mpsc::Receiver<OpenRead>>,
    shared: &Arc<Shared>,
    index: usize,
) {
    while !shared.stopping.load(Ordering::SeqCst) {
        let request = requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv_timeout(Duration::from_millis(20));
        let request = match request {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let OpenRead {
            expired: snapshot_expired,
            caller,
            deadline,
            commands,
            ready,
        } = request;
        // Cancellation and stale admission must not enter the pool, pin a
        // snapshot, or retire a healthy connection at an overloaded boundary.
        if ready.is_closed() {
            continue;
        }
        let expired = Instant::now() >= deadline;
        #[cfg(test)]
        let expired = expired && !shared.unbounded_read_admission;
        if expired {
            let _ = ready.send(Err(SqliteError::Overloaded));
            continue;
        }
        let mut ready = Some(ready);
        let mut retired_reply: Option<RetiredReply> = None;
        let result = pool.read_tracked(deadline, snapshot_expired.clone(), |tx| {
            shared.readable()?;
            let now = Instant::now();
            #[cfg(test)]
            let now = shared.pin_now.as_ref().map_or(now, |clock| clock());
            let expired = now >= deadline;
            #[cfg(test)]
            let expired = expired && !shared.unbounded_read_admission;
            if expired {
                return Err(SqliteError::Overloaded);
            }
            shared
                .interrupts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[index] =
                Some(tx.get_interrupt_handle());
            // BEGIN is deferred: this real read pins the committed view before
            // acknowledging open, even when no entity is ever requested.
            tx.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
                r.get::<_, i64>(0)
            })?;
            if ready
                .take()
                .expect("open acknowledgement once")
                .send(Ok(()))
                .is_err()
            {
                return Ok(());
            }
            let view = ReadView::new(tx, &caller, shared);
            let mut idle = Instant::now();
            loop {
                if snapshot_expired.load(Ordering::SeqCst) {
                    return Err(SqliteError::SnapshotExpired);
                }
                if shared.stopping.load(Ordering::SeqCst) {
                    break;
                }
                let remaining = shared.idle.saturating_sub(idle.elapsed());
                if remaining.is_zero() {
                    break;
                }
                match commands.recv_timeout(remaining.min(Duration::from_millis(50))) {
                    Ok(job) => {
                        job(&view, &mut retired_reply)?;
                        idle = Instant::now();
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            }
            Ok(())
        });
        shared
            .interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[index] = None;
        if let Some(reply) = retired_reply {
            reply();
        }
        if let Some(ready) = ready {
            let _ = ready.send(result);
        }
    }
}
impl SqliteStorage {
    async fn snapshot(&self, caller: Caller) -> store::Result<Snapshot> {
        port(self.owner.shared.readable())?;
        let observed = self.owner.shared.state.fatal_observations();
        let (commands, receiver) = mpsc::sync_channel(SNAPSHOT_CALL_CAPACITY);
        let (ready, answer) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(200);
        let expired = Arc::new(AtomicBool::new(false));
        let request = OpenRead {
            expired: expired.clone(),
            caller: caller.clone(),
            deadline,
            commands: receiver,
            ready,
        };
        self.owner
            .reads
            .try_send(request)
            .map_err(|_| backend("overloaded"))?;
        #[cfg(not(test))]
        let answer =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), answer).await;
        #[cfg(test)]
        let answer = if self.owner.shared.unbounded_read_admission {
            Ok(answer.await)
        } else {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), answer).await
        };
        // Include queued admission and pinning in the request's original
        // baseline. ReadPool has classified any pin failure before this reply.
        port(self.owner.shared.refuse_fresh_fatal(observed))?;
        let answer = answer.map_err(|_| backend("overloaded"))?;
        port(answer.map_err(|_| backend("SQLite reader closed"))?)?;
        Ok(Snapshot {
            expired,
            owner: self.owner.clone(),
            caller,
            commands,
            capacity: Arc::new(Semaphore::new(SNAPSHOT_CALL_CAPACITY)),
            observed,
        })
    }
}
#[async_trait]
impl Storage for SqliteStorage {
    async fn read(&self, caller: Caller) -> store::Result<Box<dyn ReadSnapshot>> {
        Ok(Box::new(self.snapshot(caller).await?))
    }
    async fn write(&self, caller: Caller, lane: store::Lane, work: WriteWork) -> store::Result<()> {
        let now = Instant::now();
        let deadline = Some(
            now + match lane {
                store::Lane::Claims => Duration::from_millis(50),
                store::Lane::Entities => Duration::from_millis(200),
                store::Lane::Events => Duration::from_secs(1),
                store::Lane::Maintenance => Duration::from_secs(5),
            },
        );
        #[cfg(test)]
        let deadline = self
            .owner
            .shared
            .write_admission
            .as_ref()
            .map_or(deadline, |admit| admit(now, deadline));
        self.retained_write(caller, lane, deadline)
            .attempt(work)
            .await?
            .result
    }
}
