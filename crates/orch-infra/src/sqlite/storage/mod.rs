//! Owned SQLite storage composition. The daemon continues using files until T24.
mod page;
#[cfg(test)]
mod page_tests;
mod ports;
#[cfg(test)]
mod prior_latch_tests;
#[cfg(test)]
mod tests;
mod view;

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
}
#[cfg(test)]
type ReaderSetup = Box<dyn Fn(&mut rusqlite::Connection) + Send>;
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
        }
    }
}
struct Shared {
    state: FailureState,
    disk: DiskBlobStore,
    task_cursors: TaskCursors,
    cursors: NonTaskCursors,
    clock: Arc<dyn Clock>,
    stopping: AtomicBool,
    interrupts: Mutex<Vec<Option<rusqlite::InterruptHandle>>>,
    idle: Duration,
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
    writer: WriterHandle,
    reads: mpsc::SyncSender<OpenRead>,
    stop: mpsc::SyncSender<()>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Owner {
    fn stop(&self) {
        self.shared.stop();
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
                        state: state.clone(),
                        task_cursors: TaskCursors::new(&options.generation),
                        cursors: NonTaskCursors::new(options.generation),
                        clock: options.clock,
                        stopping: AtomicBool::new(false),
                        interrupts: Mutex::new(
                            (0..options.read_connections).map(|_| None).collect(),
                        ),
                        idle: options.snapshot_idle,
                    });
                    let pool = ReadPool::open_with_state(
                        &data,
                        options.synchronous,
                        options.read_connections,
                        state.clone(),
                    )?;
                    #[cfg(test)]
                    if let Some(setup) = options.reader_setup {
                        pool.configure_test_readers(setup);
                    }
                    let (writer, handle) = Writer::open_with_state(
                        &data,
                        options.synchronous,
                        options.writer_capacity,
                        state,
                    )?;
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
        SqliteError::StorageUnavailable(_) => backend("storage_unavailable"),
        SqliteError::Overloaded => backend("overloaded"),
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
type RetiredReply = Box<dyn FnOnce() + Send>;
type ReadJob = Box<dyn FnOnce(&ReadView<'_, '_>, &mut Option<RetiredReply>) -> Result<()> + Send>;
struct OpenRead {
    caller: Caller,
    deadline: Instant,
    commands: mpsc::Receiver<ReadJob>,
    ready: oneshot::Sender<Result<()>>,
}
struct Snapshot {
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
                    let state = view.shared.state.clone();
                    // Answer only after ReadPool has classified the original
                    // error, retired the connection and updated its one streak.
                    *retired_reply = Some(Box::new(move || {
                        let message = if state.fatal_observations() != observed {
                            "storage_unavailable"
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
        answer.await.map_err(|_| backend("snapshot_expired"))?
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
        if Instant::now() >= deadline {
            let _ = ready.send(Err(SqliteError::Overloaded));
            continue;
        }
        let mut ready = Some(ready);
        let mut retired_reply: Option<RetiredReply> = None;
        let result = pool.read(deadline, |tx| {
            shared.readable()?;
            if Instant::now() >= deadline {
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
#[async_trait]
impl Storage for SqliteStorage {
    async fn read(&self, caller: Caller) -> store::Result<Box<dyn ReadSnapshot>> {
        port(self.owner.shared.readable())?;
        let observed = self.owner.shared.state.fatal_observations();
        let (commands, receiver) = mpsc::sync_channel(16);
        let (ready, answer) = oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(200);
        let request = OpenRead {
            caller: caller.clone(),
            deadline,
            commands: receiver,
            ready,
        };
        self.owner
            .reads
            .try_send(request)
            .map_err(|_| backend("overloaded"))?;
        let answer = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), answer)
            .await
            .map_err(|_| backend("overloaded"))?;
        port(answer.map_err(|_| backend("SQLite reader closed"))?)?;
        Ok(Box::new(Snapshot {
            owner: self.owner.clone(),
            caller,
            commands,
            capacity: Arc::new(Semaphore::new(16)),
            observed,
        }))
    }
    async fn write(&self, caller: Caller, lane: store::Lane, work: WriteWork) -> store::Result<()> {
        port(self.owner.shared.available())?;
        let shared = self.owner.shared.clone();
        let panic = Arc::new(Mutex::new(None));
        let callback_panic = panic.clone();
        let lane = match lane {
            store::Lane::Claims => super::Lane::Claims,
            store::Lane::Entities => super::Lane::Entities,
            store::Lane::Events => super::Lane::Events,
            store::Lane::Maintenance => super::Lane::Maintenance,
        };
        let budget = match lane {
            super::Lane::Claims => Duration::from_millis(50),
            super::Lane::Entities => Duration::from_millis(200),
            super::Lane::Events => Duration::from_secs(1),
            super::Lane::Maintenance => Duration::from_secs(5),
        };
        let receipt = port(self.owner.writer.submit(
            lane,
            Some(Instant::now() + budget),
            move |tx| {
                let mut unit = Unit::new(tx, &caller, &shared);
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&mut unit))) {
                    Ok(Ok(())) => unit.ensure_committable(),
                    Ok(Err(error)) => Err(SqliteError::Store(error)),
                    Err(payload) => {
                        *callback_panic
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(payload);
                        Err(SqliteError::TransactionAborted)
                    }
                }
            },
        ))?;
        let result = tokio::task::spawn_blocking(move || receipt.wait())
            .await
            .map_err(|_| backend("SQLite receipt worker failed"))?;
        if let Some(payload) = panic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            std::panic::resume_unwind(payload);
        }
        public_result(&self.owner.shared.state, port(result))
    }
}
