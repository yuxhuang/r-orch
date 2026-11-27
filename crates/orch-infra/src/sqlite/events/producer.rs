//! The first production submission of run events (T16 first slice; spec R26,
//! R27, R63, R128, record P; plan "Writer retry policy", WP5, WP9 and WP10).
//!
//! A collector hands over numbered batches of one run each. This producer owns
//! every batch until the writer has answered for it, and it is the only thing
//! that submits events, so the event lane never holds more than the one
//! transaction that R26 allows:
//!
//! * **Coalescing.** Everything that waits, of any number of runs, goes into
//!   the next transaction together, in arrival order. A run's events keep their
//!   order, and a batch of another caller or project never shares a transaction
//!   with this one (a transaction has one caller). A refused batch (`StaleEpoch`,
//!   `InvalidEvents`) is answered alone: the refusal wrote nothing and the
//!   other runs commit.
//! * **Cut.** A transaction holds at most 1,000 rows or 20 ms of work (R27).
//!   The rows are planned from the stored form of each event (one row, or two
//!   when its payload goes behind a blob reference); the time is checked
//!   between statements, never inside one. At least one statement always runs.
//!   What is not committed is kept, in order, for the next transaction.
//! * **Actual work.** The writer counts every physical transaction once in
//!   `writer_txn_over_bound_total` when its actual rows pass 1,000 or its
//!   measured work, the COMMIT included, passes 20 ms. This module never counts.
//!   A transaction that it cuts by time has already spent 20 ms of statements
//!   before its COMMIT, so the writer counts it too: the counter does not
//!   separate divisible bulk work that was cut from an indivisible event (one
//!   statement that cannot be cut). That is the decided meaning (option 1 of the
//!   lead's metric decision), not a defect. A retry never counts a transaction
//!   twice.
//! * **Admission.** What the producer keeps for batches that wait is bounded in
//!   three independent ways, so that a collector cannot make it hold more than a
//!   known amount: batches (an empty batch is a lease check and counts as one),
//!   events, and accounted bytes, metadata included (see `QUEUE_LIMIT_BYTES`).
//!   The cheap checks run first and a batch that is already excluded is never
//!   encoded. A batch that is refused is returned to its caller whole.
//! * **Retained input.** A batch is dropped from memory only on a known commit
//!   or a definitive refusal. After a confirmed noncommit with a retryable cause
//!   the same transaction is rebuilt from the retained input as a new request of
//!   the same retry episode (`RetainedWrite`). After an unknown commit it is
//!   first reconciled: the producer reads back the exact rows it expected and
//!   commits to nothing until the database has answered.
//! * **Failure of the task.** If the producer's task ends with batches still
//!   held (a panic in the clock or the observer, or a runtime that shuts down),
//!   every held batch is answered at once and intake closes. A batch that was in
//!   a transaction at that moment says its outcome is unknown; the others were
//!   not started.
//!
//! Not here (T16, later): numbering at handover, the collector's buffer and its
//! gaps, the counters at start and the stop record (R61, R63, R139), and pruning.
//! Daemon wiring and activation are not part of this slice.

use super::{MAX_ROWS, encoded::EncodedEvent};
#[cfg(test)]
use crate::blob::INLINE_TEXT_MAX_BYTES;
use crate::blob::MAX_BLOB_BYTES;
use crate::sqlite::{RetainedWrite, SqliteStorage, WriteAccounting, WriteAttempt, WriteOutcome};
#[cfg(test)]
use orch_core::store::ReadSnapshot;
use orch_core::store::events::{AppendResult, SequencedEvent};
use orch_core::store::{Caller, Lane, Principal, StoreError};
#[cfg(test)]
use orch_domain::RunEvent;
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, oneshot};

/// R27: the work of one transaction, checked between statements.
pub(crate) const MAX_WORK: Duration = Duration::from_millis(20);
/// Events per statement. The clock is read between statements, so this is also
/// the grain of the 20 ms cut; an event with a blob is a statement of its own.
const STEP_EVENTS: usize = 32;
/// The most events that may wait in the producer, and also the most one batch
/// may hold: a collector that is refused keeps its buffer (R63) and tries again,
/// or splits a batch that can never fit.
const QUEUE_LIMIT_EVENTS: usize = 16_384;
/// The most batches that may wait. It is independent of the events: an empty
/// batch (a lease check) holds a caller, a run name and a reply channel, and a
/// producer that counted only events would keep any number of them.
const QUEUE_LIMIT_BATCHES: usize = 4_096;
/// The most accounted bytes that may wait: four times the largest blob. A single
/// event may be as large as the blob store takes (64 MiB, R97) and is one
/// statement that cannot be cut, so the envelope has to hold one of them with
/// room beside it; an event past what a blob holds is still admitted below this
/// and fails alone at its statement. The figure is the encoded size of the
/// events plus retained string/array capacities and a conservative reserve for
/// compact object entries (see `Measure`). Accepted objects are rebuilt from
/// live entries because serde_json hides their spare capacity. Allocator headers
/// and transient SQL/serialization work are not measured by this counter.
const QUEUE_LIMIT_BYTES: usize = 4 * MAX_BLOB_BYTES as usize;
/// What a waiting batch holds besides its strings and events: its entry in the
/// queue, the slice of the plan built from it, and a reply channel.
const BATCH_FIXED_BYTES: usize =
    4 * size_of::<Pending>() + 3 * size_of::<Slice>() + size_of::<Charge>() + 256;
/// What a waiting event holds besides its encoded form: the event, and its cost.
const EVENT_FIXED_BYTES: usize =
    size_of::<EncodedEvent>() + 1 + size_of::<Step>() + size_of::<EncodedExpected>() + 64;
/// Reserve for the original caller/run allocations and their simultaneous
/// plan, attempt, reconciliation and report copies. Clones have no more live
/// bytes than the original capacities counted here.
const STRING_COPIES: usize = 8;
/// Waits of a producer whose submission the writer's queue did not admit.
const ADMISSION_WAIT: Duration = Duration::from_millis(25);
const ADMISSION_WAIT_MAX: Duration = Duration::from_millis(250);
/// Reads that reconcile an unknown commit are tried this many times.
const RECONCILE_TRIES: u32 = 5;

/// One run's numbered events, handed over by its collector.
#[derive(Debug, Clone)]
pub struct EventBatch {
    pub caller: Caller,
    pub run: String,
    pub lineage: String,
    pub epoch: u64,
    pub events: Vec<SequencedEvent>,
}

/// Why a batch has no complete receipt. `committed` is what was durably stored
/// before the failure (a batch can span transactions); `not_stored` is the
/// number of its events that were not committed. A batch whose commit is unknown
/// is reconciled and never answered with a guess; only when reconciliation
/// itself cannot finish is `outcome_unknown` set, and then the last transaction
/// that carried the batch may have committed some or all of the `not_stored`
/// events, so the collector must not turn them into a gap row on this answer.
#[derive(Debug)]
pub struct SubmitFailure {
    pub error: StoreError,
    pub committed: AppendResult,
    pub not_stored: usize,
    pub outcome_unknown: bool,
}

/// Why `submit` returned the batch to its caller.
#[derive(Debug)]
pub enum Rejected {
    /// Too many batches, events or bytes wait. Keep the batch and submit it
    /// again later.
    Full(Box<EventBatch>),
    /// The batch could never be admitted, however little waits: it holds more
    /// events than the limit, or more accounted bytes than the envelope. Split
    /// it, or turn it into a gap; submitting it again is refused again.
    TooLarge(Box<EventBatch>),
    /// The producer has failed or was closed; the text is the first cause.
    Closed(Box<EventBatch>, String),
}

/// What one physical transaction did, for metrics and tests.
#[derive(Debug, Clone)]
pub struct TransactionReport {
    pub id: u64,
    /// `(project, run)` of every batch in the transaction, in order.
    pub runs: Vec<(String, String)>,
    /// Events whose statement ran (stored or skipped as duplicates).
    pub events: usize,
    pub cut_by_time: bool,
    /// Submissions of this logical transaction, the first included.
    pub attempts: u32,
    /// The writer's actual rows and time for the committed attempt.
    pub accounting: WriteAccounting,
}

type Observer = Arc<dyn Fn(&TransactionReport) + Send + Sync>;
type Now = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Clone)]
pub struct EventProducerOptions {
    /// The clock of the 20 ms cut. The writer measures its own actual time. It
    /// runs inside the transaction, on the writer's thread; a panic in it ends
    /// the producer (see the module's "Failure of the task").
    pub now: Now,
    /// Called once for every committed transaction, after the table has been
    /// updated and before the batches it completed are answered. A panic in it
    /// ends the producer after those batches have been answered.
    pub observer: Option<Observer>,
    /// The most events that may wait, and the most one batch may hold.
    pub queue_limit: usize,
    /// The most batches that may wait, empty ones included.
    pub batch_limit: usize,
    /// The most accounted bytes that may wait, metadata included.
    pub byte_limit: usize,
}

impl Default for EventProducerOptions {
    fn default() -> Self {
        Self {
            now: Arc::new(Instant::now),
            observer: None,
            queue_limit: QUEUE_LIMIT_EVENTS,
            batch_limit: QUEUE_LIMIT_BATCHES,
            byte_limit: QUEUE_LIMIT_BYTES,
        }
    }
}

type Answer = Result<AppendResult, SubmitFailure>;

/// Resolves when the whole batch is committed or definitively refused.
/// Dropping it does not cancel the batch.
#[derive(Debug)]
pub struct Submission(oneshot::Receiver<Answer>);

impl Future for Submission {
    type Output = Answer;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Answer> {
        Pin::new(&mut self.0).poll(cx).map(|answer| {
            answer.unwrap_or_else(|_| {
                Err(SubmitFailure {
                    error: backend("event producer ended before it answered the batch"),
                    committed: AppendResult {
                        stored: 0,
                        skipped: 0,
                    },
                    not_stored: 0,
                    outcome_unknown: true,
                })
            })
        })
    }
}

struct Pending {
    id: u64,
    caller: Caller,
    run: String,
    lineage: String,
    epoch: u64,
    events: Arc<[EncodedEvent]>,
    costs: Arc<[u8]>,
    /// What the batch holds, as `Measure` counted it; given back when it leaves.
    consumed: usize,
    /// A statement of this batch has committed. An empty batch is answered
    /// only after its lease check has committed.
    touched: bool,
    acc: AppendResult,
    reply: Option<oneshot::Sender<Answer>>,
    charge: Option<Arc<Charge>>,
}

impl Pending {
    fn finish(mut self, answer: Answer) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(answer);
        }
    }
    fn failure(&self, error: StoreError, outcome_unknown: bool) -> SubmitFailure {
        SubmitFailure {
            error,
            committed: self.acc,
            not_stored: self.events.len() - self.consumed,
            outcome_unknown,
        }
    }
    fn key(&self) -> (String, String) {
        (self.caller.project.clone(), self.run.clone())
    }
}

struct Charge {
    shared: Weak<Shared>,
    bytes: usize,
}
impl Drop for Charge {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.queue().bytes -= self.bytes;
            shared.idle.notify_waiters();
        }
    }
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Pending>,
    /// Events not yet committed or answered, over every waiting batch.
    events: usize,
    /// Accounted bytes of every waiting batch, whole until it is answered: the
    /// events of a committed prefix stay in memory with their batch.
    bytes: usize,
    busy: bool,
    /// The batches of the transaction being driven. If the task ends while they
    /// are set, their outcome is unknown.
    in_flight: Vec<u64>,
    closed: Option<String>,
}

/// Why a batch was not admitted.
enum Refusal {
    Closed(String),
    Full,
    TooLarge,
}

impl Queue {
    /// Whether a batch of `events`, holding `bytes` accounted bytes, may wait.
    /// `TooLarge` does not depend on what waits; `Full` does.
    fn admit(
        &self,
        limits: &EventProducerOptions,
        events: usize,
        bytes: usize,
    ) -> Result<(), Refusal> {
        if let Some(cause) = &self.closed {
            return Err(Refusal::Closed(cause.clone()));
        }
        if events > limits.queue_limit || bytes > limits.byte_limit {
            return Err(Refusal::TooLarge);
        }
        if self.pending.len() >= limits.batch_limit
            || events > limits.queue_limit.saturating_sub(self.events)
            || bytes > limits.byte_limit.saturating_sub(self.bytes)
        {
            return Err(Refusal::Full);
        }
        Ok(())
    }
}

fn rejected(batch: EventBatch, refusal: Refusal) -> Rejected {
    let batch = Box::new(batch);
    match refusal {
        Refusal::Closed(cause) => Rejected::Closed(batch, cause),
        Refusal::Full => Rejected::Full(batch),
        Refusal::TooLarge => Rejected::TooLarge(batch),
    }
}

struct Shared {
    queue: Mutex<Queue>,
    work: Notify,
    idle: Notify,
    next: AtomicU64,
    transactions: AtomicU64,
    options: EventProducerOptions,
}

impl Shared {
    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The owner of the event lane's one transaction. Must be created inside a
/// Tokio runtime. Dropping it stops intake and lets the task answer what waits.
pub struct EventProducer {
    shared: Arc<Shared>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl EventProducer {
    pub fn new(storage: SqliteStorage, options: EventProducerOptions) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            work: Notify::new(),
            idle: Notify::new(),
            next: AtomicU64::new(1),
            transactions: AtomicU64::new(1),
            options,
        });
        let drain = Drain {
            shared: Arc::clone(&shared),
            storage,
        };
        let task = tokio::spawn(drain.run());
        Self {
            shared,
            task: Mutex::new(Some(task)),
        }
    }

    /// Takes ownership of the batch. It is returned when it cannot be kept.
    ///
    /// Admission goes from the cheap check to the dear one, and a batch that an
    /// earlier check excludes is returned before anything is encoded: the strings
    /// and the event count against the limits first, then the events' encoded
    /// sizes against what is still free (the walk stops at the first event that
    /// does not fit), and last the same limits again, because other submissions
    /// may have arrived while the events were measured.
    pub fn submit(&self, batch: EventBatch) -> Result<Submission, Rejected> {
        let options = &self.shared.options;
        let fixed = batch.fixed_bytes();
        let free = {
            let queue = self.shared.queue();
            match queue.admit(options, batch.events.len(), fixed) {
                Ok(()) => options.byte_limit - queue.bytes - fixed,
                Err(refusal) => return Err(rejected(batch, refusal)),
            }
        };
        let Some(measured) = Measure::of(&batch.events, free) else {
            // Past what was free. It could never fit if nothing else was waiting.
            let refusal = match &self.shared.queue().closed {
                Some(cause) => Refusal::Closed(cause.clone()),
                None if free == options.byte_limit - fixed => Refusal::TooLarge,
                None => Refusal::Full,
            };
            return Err(rejected(batch, refusal));
        };
        let accounted = fixed + measured.bytes;
        let costs = measured.costs;
        let mut queue = self.shared.queue();
        if let Err(refusal) = queue.admit(options, batch.events.len(), accounted) {
            return Err(rejected(batch, refusal));
        }
        let (reply, receiver) = oneshot::channel();
        queue.events += batch.events.len();
        queue.bytes += accounted;
        queue.pending.push_back(Pending {
            id: self.shared.next.fetch_add(1, Ordering::Relaxed),
            caller: batch.caller,
            run: batch.run,
            lineage: batch.lineage,
            epoch: batch.epoch,
            events: measured.events,
            costs,
            consumed: 0,
            touched: false,
            acc: AppendResult {
                stored: 0,
                skipped: 0,
            },
            reply: Some(reply),
            charge: Some(Arc::new(Charge {
                shared: Arc::downgrade(&self.shared),
                bytes: accounted,
            })),
        });
        drop(batch.events);
        drop(queue);
        self.shared.work.notify_one();
        Ok(Submission(receiver))
    }

    /// Returns once every batch submitted before the call has been answered.
    pub async fn flush(&self) {
        loop {
            let idle = self.shared.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            {
                let queue = self.shared.queue();
                if queue.pending.is_empty() && !queue.busy {
                    return;
                }
            }
            idle.await;
        }
    }

    /// Stops intake, answers what waits, and ends the task.
    pub async fn close(self) {
        self.stop();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    /// Waiting batches and whether a transaction is in flight, for tests that
    /// need the producer to have taken its first transaction before more arrive.
    #[cfg(test)]
    pub(super) fn waiting(&self) -> (usize, bool) {
        let queue = self.shared.queue();
        (queue.pending.len(), queue.busy)
    }

    /// Batches, events and accounted bytes held; all zero once everything is
    /// answered, so that the capacity a batch took is shown to come back.
    #[cfg(test)]
    pub(super) fn retained(&self) -> (usize, usize, usize) {
        let queue = self.shared.queue();
        (queue.pending.len(), queue.events, queue.bytes)
    }

    fn stop(&self) {
        let mut queue = self.shared.queue();
        queue
            .closed
            .get_or_insert_with(|| "event producer closed".into());
        drop(queue);
        self.shared.work.notify_one();
    }
}

impl Drop for EventProducer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn backend(message: &str) -> StoreError {
    StoreError::Backend(orch_core::Error::other(message.to_owned()))
}

/// A refusal of one batch's contract: nothing was written, and the rest of the
/// transaction is unaffected. Anything else on a statement dooms the transaction.
fn refusal(error: &StoreError) -> bool {
    matches!(error, StoreError::StaleEpoch | StoreError::InvalidEvents(_))
}

fn copy_error(error: &StoreError) -> StoreError {
    match error {
        StoreError::StaleEpoch => StoreError::StaleEpoch,
        StoreError::InvalidEvents(reason) => StoreError::InvalidEvents(reason.clone()),
        other => backend(&other.to_string()),
    }
}

impl EventBatch {
    /// What the batch holds whatever its events encode to: the producer's
    /// structures, the caller and run strings (each kept twice), and the fixed
    /// part of every event. Every string is counted at its capacity, so a batch
    /// with no events still has a size.
    pub(super) fn fixed_bytes(&self) -> usize {
        let principal = match &self.caller.principal {
            Principal::Owner { identity } | Principal::Worker { identity } => identity.capacity(),
            Principal::Task {
                identity,
                task,
                run,
                lineage,
                ..
            } => identity.capacity() + task.capacity() + run.capacity() + lineage.capacity(),
        };
        let strings = self
            .caller
            .project
            .capacity()
            .saturating_add(principal)
            .saturating_add(self.run.capacity())
            .saturating_add(self.lineage.capacity());
        BATCH_FIXED_BYTES
            .saturating_add(strings.saturating_mul(STRING_COPIES))
            .saturating_add(self.events.len().saturating_mul(EVENT_FIXED_BYTES))
            // Reconciliation can keep one Expected run name per statement,
            // at most one per event within the 1,000-row transaction.
            .saturating_add(
                self.run
                    .capacity()
                    .saturating_mul(self.events.len().min(1000)),
            )
    }
}

/// What the events of a batch cost: the rows each changes, and their encoded bytes.
struct Measure {
    costs: Arc<[u8]>,
    events: Arc<[EncodedEvent]>,
    bytes: usize,
}

impl Measure {
    fn of(events: &[SequencedEvent], free: usize) -> Option<Self> {
        let mut costs = Vec::with_capacity(events.len());
        let mut encoded = Vec::with_capacity(events.len());
        let mut bytes = 0usize;
        for event in events {
            #[cfg(test)]
            ENCODED.with(|n| n.set(n.get() + 1));
            let row = EncodedEvent::encode(event, free.checked_sub(bytes)?)?;
            bytes = bytes.checked_add(row.payload.len())?;
            costs.push(row.rows());
            encoded.push(row);
        }
        Some(Self {
            costs: costs.into(),
            events: encoded.into(),
            bytes,
        })
    }
}

/// What `submit` counts for a batch, as it would hold it.
#[cfg(test)]
pub(super) fn accounted_bytes(batch: &EventBatch) -> usize {
    batch.fixed_bytes() + Measure::of(&batch.events, usize::MAX).unwrap().bytes
}

/// Events encoded on this thread since it started.
#[cfg(test)]
pub(super) fn encoded_events() -> usize {
    ENCODED.with(std::cell::Cell::get)
}

#[cfg(test)]
thread_local! {
    /// Events encoded on this thread, so that a test can show that a refusal
    /// came before any encoding.
    static ENCODED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Rows a stored event changes (the actual `total_changes` of the writer): its
/// own, plus the blob's when the stored payload does not fit in the row; and its
/// encoded size. This mirrors `stored_payload`; an event that cannot be encoded
/// counts one row, and storage refuses it at its statement. `None` as soon as
/// the encoded size passes `limit`.
#[cfg(test)]
fn row_cost(event: &RunEvent, limit: usize) -> Option<(u8, usize)> {
    struct Count {
        size: usize,
        limit: usize,
    }
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.size = self.size.saturating_add(buf.len());
            if self.size > self.limit {
                return Err(std::io::Error::other("over the limit"));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[cfg(test)]
    ENCODED.with(|n| n.set(n.get() + 1));
    let mut count = Count { size: 0, limit };
    match serde_json::to_writer(&mut count, event) {
        Ok(()) if count.size > INLINE_TEXT_MAX_BYTES => Some((2, count.size)),
        Ok(()) => Some((1, count.size)),
        Err(_) if count.size > count.limit => None,
        Err(_) => Some((1, count.size)),
    }
}

/// One batch's share of a transaction.
#[derive(Clone)]
struct Slice {
    pending: u64,
    run: String,
    lineage: String,
    epoch: u64,
    events: Arc<[EncodedEvent]>,
    costs: Arc<[u8]>,
    from: usize,
    to: usize,
    charge: Option<Arc<Charge>>,
}

/// Immutable input of one logical transaction. Every attempt is built from it.
struct Plan {
    id: u64,
    caller: Caller,
    slices: Vec<Slice>,
}

enum Step {
    Appended {
        slice: usize,
        from: usize,
        to: usize,
        result: AppendResult,
    },
    Refused {
        slice: usize,
        error: StoreError,
    },
}

#[derive(Default)]
struct Progress {
    steps: Vec<Step>,
    cut_by_time: bool,
}

/// Plans the next transaction: the waiting batches of the first batch's caller,
/// in order, up to 1,000 rows. `alone` takes the first batch only. Nothing is
/// removed from the queue; the answer decides that.
fn next_plan(queue: &Queue, id: u64, alone: bool) -> Option<Plan> {
    let first = queue.pending.front()?;
    let caller = first.caller.clone();
    let mut skipped: HashSet<(String, String)> = HashSet::new();
    let mut slices = Vec::new();
    let mut rows = 0usize;
    for pending in &queue.pending {
        if skipped.contains(&pending.key()) {
            continue;
        }
        if pending.caller != caller {
            // Later batches of its run wait for it, so a run never overtakes itself.
            skipped.insert(pending.key());
            continue;
        }
        if alone && !slices.is_empty() {
            break;
        }
        let from = pending.consumed;
        let mut to = from;
        while to < pending.events.len() && rows + usize::from(pending.costs[to]) <= MAX_ROWS {
            rows += usize::from(pending.costs[to]);
            to += 1;
        }
        let empty_turn = pending.events.is_empty() && !pending.touched;
        if to > from || empty_turn {
            slices.push(Slice {
                pending: pending.id,
                run: pending.run.clone(),
                lineage: pending.lineage.clone(),
                epoch: pending.epoch,
                events: Arc::clone(&pending.events),
                costs: Arc::clone(&pending.costs),
                from,
                to,
                charge: pending.charge.clone(),
            });
        }
        if to < pending.events.len() {
            break;
        }
    }
    (!slices.is_empty()).then_some(Plan { id, caller, slices })
}

/// The callback of one attempt. Runs once, on the writer's thread, inside the
/// transaction. Statements are cut between each other at the 20 ms of R27.
fn build_work(
    plan: &Plan,
    now: Now,
    progress: Arc<Mutex<Progress>>,
) -> crate::sqlite::storage::UnitWork {
    let slices = plan.slices.clone();
    Box::new(
        move |unit: &mut crate::sqlite::storage::view::Unit<'_, '_>| {
            let started = now();
            let mut done = Progress::default();
            let mut first = true;
            'slices: for (index, slice) in slices.iter().enumerate() {
                let (events, costs) = (&slice.events, &slice.costs);
                let mut at = slice.from;
                // An empty batch still checks its lease: one statement over nothing.
                let mut once = events.is_empty();
                while at < slice.to || once {
                    if !first && now().saturating_duration_since(started) >= MAX_WORK {
                        done.cut_by_time = true;
                        break 'slices;
                    }
                    first = false;
                    once = false;
                    // An event with a blob is a statement of its own.
                    let mut end = at;
                    while end < slice.to && end - at < STEP_EVENTS {
                        let heavy = costs[end] > 1;
                        if heavy && end > at {
                            break;
                        }
                        end += 1;
                        if heavy {
                            break;
                        }
                    }
                    match unit.append_encoded_events(
                        &slice.run,
                        &slice.lineage,
                        slice.epoch,
                        &events[at..end],
                    ) {
                        Ok(result) => done.steps.push(Step::Appended {
                            slice: index,
                            from: at,
                            to: end,
                            result,
                        }),
                        Err(error) if refusal(&error) => {
                            done.steps.push(Step::Refused {
                                slice: index,
                                error,
                            });
                            continue 'slices;
                        }
                        Err(error) => return Err(error),
                    }
                    at = end;
                }
            }
            *progress.lock().unwrap_or_else(PoisonError::into_inner) = done;
            Ok(())
        },
    )
}

enum Flow {
    Continue,
    Stop,
}

struct Drain {
    shared: Arc<Shared>,
    storage: SqliteStorage,
}

impl Drop for Drain {
    /// The task is over. Normally nothing is left to answer; if it ended
    /// otherwise (a panic in the clock or the observer, which the retained write
    /// resumes on this task, or a runtime that is shutting down), every batch
    /// still held is answered now and intake closes, so that no `Submission`
    /// waits for a task that no longer exists.
    fn drop(&mut self) {
        let doubtful = {
            let mut queue = self.shared.queue();
            if queue.closed.is_some() && queue.pending.is_empty() {
                return;
            }
            std::mem::take(&mut queue.in_flight)
        };
        self.shared.fail_all(
            &backend("event producer task ended before it answered every batch"),
            &doubtful,
        );
    }
}

impl Drain {
    async fn run(self) {
        let mut alone = false;
        loop {
            let plan = loop {
                let work = self.shared.work.notified();
                tokio::pin!(work);
                work.as_mut().enable();
                {
                    let mut queue = self.shared.queue();
                    let id = self.shared.transactions.fetch_add(1, Ordering::Relaxed);
                    if let Some(plan) = next_plan(&queue, id, alone) {
                        queue.busy = true;
                        queue.in_flight = plan.slices.iter().map(|s| s.pending).collect();
                        break plan;
                    }
                    queue.busy = false;
                    self.shared.idle.notify_waiters();
                    if queue.closed.is_some() && queue.pending.is_empty() {
                        return;
                    }
                }
                work.await;
            };
            let flow = self.drive(plan, &mut alone).await;
            {
                let mut queue = self.shared.queue();
                queue.busy = false;
                queue.in_flight.clear();
            }
            self.shared.idle.notify_waiters();
            if matches!(flow, Flow::Stop) {
                return;
            }
        }
    }

    /// One logical transaction until the database has answered for it.
    async fn drive(&self, plan: Plan, alone: &mut bool) -> Flow {
        let mut write = self
            .storage
            .retained_write(plan.caller.clone(), Lane::Events, None);
        let mut attempts = 0u32;
        let mut wait = ADMISSION_WAIT;
        loop {
            let progress = Arc::new(Mutex::new(Progress::default()));
            let work = build_work(
                &plan,
                Arc::clone(&self.shared.options.now),
                Arc::clone(&progress),
            );
            let attempt = match write.attempt_unit(work).await {
                Ok(attempt) => attempt,
                Err(error) if self.admission_pressure(&error) => {
                    // Not admitted, so nothing ran; daemon-owned input is kept.
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(ADMISSION_WAIT_MAX);
                    continue;
                }
                Err(error) => return self.fail_all(&error, None),
            };
            attempts += 1;
            if attempt.result.is_ok() && attempt.outcome == WriteOutcome::Committed {
                self.committed(&plan, &progress, attempts, attempt.accounting);
                *alone = false;
                return Flow::Continue;
            }
            if attempt.outcome == WriteOutcome::Unknown {
                match self.reconcile(&mut write, &plan, &progress, &attempt).await {
                    Reconciled::Committed => {
                        self.committed(&plan, &progress, attempts, attempt.accounting);
                        *alone = false;
                        return Flow::Continue;
                    }
                    Reconciled::Rebuild => continue,
                    Reconciled::NotStored(error) => return self.fail_plan(&plan, &error, alone),
                    Reconciled::Stuck(error) => return self.fail_all(&error, Some(&plan)),
                }
            }
            if attempt.retryable() {
                continue;
            }
            let Err(error) = attempt.result else {
                // A commit that was not reported as one: nothing here may guess.
                return self.fail_all(&backend("commit outcome is inconsistent"), Some(&plan));
            };
            if self.storage.failure_state().failure().is_some() {
                return self.fail_all(&error, None);
            }
            return self.fail_plan(&plan, &error, alone);
        }
    }

    /// The writer's queue refused this submission before it ran anything.
    /// `port` gives this one answer no type, so its text is the discriminator;
    /// a test provokes the real refusal to pin it.
    fn admission_pressure(&self, error: &StoreError) -> bool {
        self.storage.failure_state().failure().is_none() && error.to_string().contains("overloaded")
    }

    async fn reconcile(
        &self,
        write: &mut RetainedWrite,
        plan: &Plan,
        progress: &Arc<Mutex<Progress>>,
        attempt: &WriteAttempt,
    ) -> Reconciled {
        let expected = Arc::new(expected_rows(plan, progress));
        let mut pause = Duration::from_millis(20);
        for tries in 1..=RECONCILE_TRIES {
            let check = Arc::clone(&expected);
            match write.reconcile_encoded(check).await {
                Ok(true) => return Reconciled::Committed,
                Ok(false) if attempt.retry_code.is_some() => return Reconciled::Rebuild,
                Ok(false) => {
                    return Reconciled::NotStored(backend(
                        "commit outcome was unknown, reconciliation found none of its rows, \
                         and the cause allows no retry",
                    ));
                }
                Err(error) => {
                    if tries == RECONCILE_TRIES || self.storage.failure_state().failure().is_some()
                    {
                        return Reconciled::Stuck(error);
                    }
                    tokio::time::sleep(pause).await;
                    pause *= 2;
                }
            }
        }
        Reconciled::Stuck(backend("reconciliation did not finish"))
    }

    /// A known commit: what the callback appended is durable, in this order.
    fn committed(
        &self,
        plan: &Plan,
        progress: &Arc<Mutex<Progress>>,
        attempts: u32,
        accounting: WriteAccounting,
    ) {
        let done = std::mem::take(&mut *progress.lock().unwrap_or_else(PoisonError::into_inner));
        let mut stepped = 0;
        let mut answers: Vec<(Pending, Answer)> = Vec::new();
        {
            let mut queue = self.shared.queue();
            for step in done.steps {
                match step {
                    Step::Appended {
                        slice,
                        from,
                        to,
                        result,
                    } => {
                        stepped += to - from;
                        if let Some(p) = find(&mut queue, plan.slices[slice].pending) {
                            p.consumed = to;
                            p.touched = true;
                            p.acc.stored += result.stored;
                            p.acc.skipped += result.skipped;
                        }
                    }
                    Step::Refused { slice, error } => {
                        let id = plan.slices[slice].pending;
                        if let Some(p) = take(&mut queue, id) {
                            let failure = p.failure(error, false);
                            answers.push((p, Err(failure)));
                        }
                    }
                }
            }
            for slice in &plan.slices {
                let complete = find(&mut queue, slice.pending)
                    .is_some_and(|p| p.touched && p.consumed == p.events.len());
                if complete && let Some(p) = take(&mut queue, slice.pending) {
                    let acc = p.acc;
                    answers.push((p, Ok(acc)));
                }
            }
            queue.events -= stepped.min(queue.events);
            // Known: what is not answered below is a committed prefix and a tail.
            queue.in_flight.clear();
        }
        // The observer is the caller's. If it panics, the batches this commit
        // completed are still answered, and then the task ends the way any panic
        // ends it: what is left is answered by `Drain::drop`.
        let observed = self.shared.options.observer.as_ref().map(|observer| {
            let report = TransactionReport {
                id: plan.id,
                runs: plan
                    .slices
                    .iter()
                    .map(|s| (plan.caller.project.clone(), s.run.clone()))
                    .collect(),
                events: stepped,
                cut_by_time: done.cut_by_time,
                attempts,
                accounting,
            };
            catch_unwind(AssertUnwindSafe(|| observer(&report)))
        });
        for (pending, answer) in answers {
            pending.finish(answer);
        }
        if let Some(Err(panic)) = observed {
            resume_unwind(panic);
        }
    }

    /// The transaction rolled back for a cause that is not retried. A coalesced
    /// transaction is replanned with the first batch alone, so that one batch's
    /// failure does not answer for the others; a lone batch is answered.
    fn fail_plan(&self, plan: &Plan, error: &StoreError, alone: &mut bool) -> Flow {
        if plan.slices.len() > 1 {
            *alone = true;
            return Flow::Continue;
        }
        *alone = false;
        let taken = take(&mut self.shared.queue(), plan.slices[0].pending);
        if let Some(p) = taken {
            let failure = p.failure(copy_error(error), false);
            p.finish(Err(failure));
        }
        Flow::Continue
    }

    /// The writer failed or closed, or an outcome cannot be established. Every
    /// waiting batch is answered with its own count and the producer stops.
    /// `in_doubt` is the transaction whose commit could not be established: its
    /// batches say so.
    fn fail_all(&self, error: &StoreError, in_doubt: Option<&Plan>) -> Flow {
        let doubtful: Vec<u64> = in_doubt
            .map(|plan| plan.slices.iter().map(|s| s.pending).collect())
            .unwrap_or_default();
        self.shared.fail_all(error, &doubtful);
        Flow::Stop
    }
}

impl Shared {
    /// Closes intake and answers every held batch; those of `doubtful` say that
    /// their outcome is unknown. Nothing is kept: capacity is all returned.
    fn fail_all(&self, error: &StoreError, doubtful: &[u64]) {
        let drained: Vec<Pending> = {
            let mut queue = self.queue();
            queue.closed = Some(error.to_string());
            queue.events = 0;
            queue.busy = false;
            queue.in_flight.clear();
            let drained = queue.pending.drain(..).collect();
            queue.pending.shrink_to_fit();
            drained
        };
        for pending in drained {
            let failure = pending.failure(copy_error(error), doubtful.contains(&pending.id));
            pending.finish(Err(failure));
        }
        self.idle.notify_waiters();
    }
}

enum Reconciled {
    Committed,
    Rebuild,
    NotStored(StoreError),
    Stuck(StoreError),
}

fn find(queue: &mut Queue, id: u64) -> Option<&mut Pending> {
    queue.pending.iter_mut().find(|p| p.id == id)
}

/// Removes a batch that is answered, and the events it still held from the count.
fn take(queue: &mut Queue, id: u64) -> Option<Pending> {
    let position = queue.pending.iter().position(|p| p.id == id)?;
    let pending = queue.pending.remove(position)?;
    queue.events -= pending.events.len() - pending.consumed;
    queue.pending.shrink_to_fit();
    Some(pending)
}

/// What the callback appended in an unknown attempt, as the rows to look for.
#[cfg(test)]
pub(super) struct Expected {
    pub(super) run: String,
    pub(super) events: Arc<[SequencedEvent]>,
    pub(super) from: usize,
    pub(super) to: usize,
}

pub(in crate::sqlite) struct EncodedExpected {
    pub run: String,
    pub events: Arc<[EncodedEvent]>,
    pub from: usize,
    pub to: usize,
    _charge: Option<Arc<Charge>>,
}

fn expected_rows(plan: &Plan, progress: &Arc<Mutex<Progress>>) -> Vec<EncodedExpected> {
    let done = progress.lock().unwrap_or_else(PoisonError::into_inner);
    done.steps
        .iter()
        .filter_map(|step| match step {
            // A step that stored nothing (all duplicates, or empty) has no row to find.
            Step::Appended {
                slice,
                from,
                to,
                result,
            } if result.stored > 0 => Some(EncodedExpected {
                run: plan.slices[*slice].run.clone(),
                events: Arc::clone(&plan.slices[*slice].events),
                from: *from,
                to: *to,
                _charge: plan.slices[*slice].charge.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// `true` when every expected row is stored with the expected bytes, `false`
/// when none is. A transaction is atomic, so anything between is a divergence
/// that the producer reports and does not resolve.
#[cfg(test)]
pub(super) async fn rows_are_durable(
    snapshot: Box<dyn ReadSnapshot>,
    expected: &[Expected],
) -> orch_core::store::Result<bool> {
    let (mut found, mut wanted) = (0usize, 0usize);
    for step in expected {
        let rows: BTreeMap<u64, &RunEvent> = step.events[step.from..step.to]
            .iter()
            .rev()
            .map(|e| (e.seq, &e.event))
            .collect();
        wanted += rows.len();
        let (Some(&low), Some(&high)) = (rows.keys().next(), rows.keys().next_back()) else {
            continue;
        };
        let mut after = low - 1;
        'pages: loop {
            let page = snapshot.read_events(&step.run, after, MAX_ROWS).await?;
            let Some(last) = page.last().map(|e| e.seq) else {
                break;
            };
            for stored in page {
                if stored.seq > high {
                    break 'pages;
                }
                match rows.get(&stored.seq) {
                    Some(event) if **event == stored.event => found += 1,
                    Some(_) => {
                        return Err(backend(
                            "reconciliation found a stored event that differs from the expected one",
                        ));
                    }
                    None => {}
                }
            }
            after = last;
        }
    }
    match found {
        // Nothing was stored, so there is nothing a lost commit could have lost.
        _ if wanted == 0 => Ok(true),
        0 => Ok(false),
        n if n == wanted => Ok(true),
        _ => Err(backend(
            "reconciliation found only some rows of an atomic transaction",
        )),
    }
}

#[cfg(test)]
mod tests {
    //! The planner on its own: no database, so every boundary is exact.
    use super::*;
    use orch_core::store::Principal;
    use orch_domain::RunEventType;

    fn caller(project: &str, who: &str) -> Caller {
        Caller {
            project: project.into(),
            principal: Principal::Owner {
                identity: who.into(),
            },
        }
    }

    fn event(seq: u64, bytes: usize) -> SequencedEvent {
        SequencedEvent {
            seq,
            event: RunEvent {
                timestamp: "2026-01-01T00:00:00Z".into(),
                event_type: RunEventType::AgentOutput,
                data: serde_json::json!({ "pad": "x".repeat(bytes) }),
            },
        }
    }

    fn pending(id: u64, who: Caller, run: &str, events: Vec<SequencedEvent>) -> Pending {
        let costs: Arc<[u8]> = events
            .iter()
            .map(|e| row_cost(&e.event, usize::MAX).unwrap().0)
            .collect();
        Pending {
            id,
            caller: who,
            run: run.into(),
            lineage: "l".into(),
            epoch: 1,
            events: Measure::of(&events, usize::MAX).unwrap().events,
            costs,
            consumed: 0,
            touched: false,
            acc: AppendResult {
                stored: 0,
                skipped: 0,
            },
            reply: None,
            charge: None,
        }
    }

    fn queue(items: Vec<Pending>) -> Queue {
        Queue {
            events: items.iter().map(|p| p.events.len()).sum(),
            pending: items.into(),
            ..Queue::default()
        }
    }

    fn shape(plan: &Plan) -> Vec<(u64, usize, usize)> {
        plan.slices
            .iter()
            .map(|s| (s.pending, s.from, s.to))
            .collect()
    }

    fn small(n: u64) -> Vec<SequencedEvent> {
        (1..=n).map(|seq| event(seq, 8)).collect()
    }

    #[test]
    fn metadata_reserves_eight_copies_and_only_1000_statement_names() {
        let mut batch = batch_of(caller("project", "owner"), "run", "lineage", 0);
        let empty = batch.fixed_bytes();
        let capacity = batch.run.capacity();
        batch.run.reserve_exact(4096);
        assert_eq!(
            batch.fixed_bytes() - empty,
            8 * (batch.run.capacity() - capacity)
        );
        let empty = batch.fixed_bytes();
        batch.events = small(1001);
        assert_eq!(
            batch.fixed_bytes() - empty,
            1001 * EVENT_FIXED_BYTES + 1000 * batch.run.capacity()
        );
    }

    #[test]
    fn every_shape_retains_exactly_its_serialized_payload_without_value_allocations() {
        let mut nested =
            serde_json::json!({"z": "escape\n\"é", "a": [1, 18446744073709551615u64, -1.25]});
        for _ in 0..160 {
            nested = serde_json::json!([nested]);
        }
        let mut batch = batch_of(caller("p", "o"), "r", "l", 1);
        batch.events[0].event.data = nested;
        let expected = serde_json::to_string(&batch.events[0].event).unwrap();
        let measured = Measure::of(&batch.events, expected.len()).unwrap();
        assert_eq!(measured.bytes, expected.len());
        assert_eq!(measured.events[0].payload.as_ref(), expected);
        assert!(Measure::of(&batch.events, expected.len() - 1).is_none());
    }

    #[test]
    fn the_cut_falls_at_exactly_1000_rows_and_the_tail_is_kept() {
        let p = caller("p", "a");
        let q = queue(vec![pending(1, p, "r", small(1001))]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 0, 1000)]);
        let p = caller("p", "a");
        let q = queue(vec![pending(1, p, "r", small(1000))]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 0, 1000)]);
    }

    #[test]
    fn the_waiting_batches_of_several_runs_share_one_plan_up_to_the_bound() {
        let p = caller("p", "a");
        let q = queue(vec![
            pending(1, p.clone(), "a", small(400)),
            pending(2, p.clone(), "b", small(400)),
            pending(3, p.clone(), "c", small(400)),
            pending(4, p, "a", small(5)),
        ]);
        // The third batch is cut and nothing after it is taken: batch 4 of run "a"
        // would otherwise run ahead of the rest of run "c" it was handed over after.
        assert_eq!(
            shape(&next_plan(&q, 1, false).unwrap()),
            [(1, 0, 400), (2, 0, 400), (3, 0, 200)]
        );
    }

    #[test]
    fn a_consumed_prefix_is_not_planned_again() {
        let p = caller("p", "a");
        let mut first = pending(1, p, "r", small(1001));
        first.consumed = 1000;
        first.touched = true;
        let q = queue(vec![first]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 1000, 1001)]);
    }

    #[test]
    fn an_event_with_a_blob_costs_two_rows_and_the_cut_counts_them() {
        let big = INLINE_TEXT_MAX_BYTES + 1;
        let cost = |bytes| row_cost(&event(1, bytes).event, usize::MAX).unwrap().0;
        assert_eq!(cost(8), 1);
        assert_eq!(cost(big), 2);
        // The stored payload is the whole event JSON, so a payload that only
        // fits without its envelope is already a blob.
        assert_eq!(cost(INLINE_TEXT_MAX_BYTES - 8), 2);
        let p = caller("p", "a");
        let heavy: Vec<_> = (1..=501).map(|seq| event(seq, big)).collect();
        let q = queue(vec![pending(1, p, "r", heavy)]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 0, 500)]);
    }

    #[test]
    fn another_caller_waits_and_its_run_is_not_overtaken() {
        let (a, b) = (caller("p", "a"), caller("p", "b"));
        let q = queue(vec![
            pending(1, a.clone(), "r", small(1)),
            pending(2, b, "r", small(1)),
            pending(3, a.clone(), "r", small(1)),
            pending(4, a.clone(), "other", small(1)),
            pending(5, caller("q", "a"), "r", small(1)),
        ]);
        // Batch 3 follows batch 2 of its run and waits for it; "other" is not held.
        assert_eq!(
            shape(&next_plan(&q, 1, false).unwrap()),
            [(1, 0, 1), (4, 0, 1)]
        );
        let q = queue(vec![
            pending(1, a, "r", small(1)),
            pending(2, caller("q", "a"), "r", small(1)),
        ]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 0, 1)]);
    }

    #[test]
    fn a_lone_replan_takes_the_first_batch_only() {
        let p = caller("p", "a");
        let q = queue(vec![
            pending(1, p.clone(), "a", small(3)),
            pending(2, p, "b", small(3)),
        ]);
        assert_eq!(shape(&next_plan(&q, 1, true).unwrap()), [(1, 0, 3)]);
    }

    #[test]
    fn a_cut_tail_is_not_overtaken_by_a_later_batch_of_its_run() {
        // 999 light events and one that needs a blob stop at 999 rows, because the
        // blob (two rows) does not fit. A later one-row batch of the same run must
        // not be planned ahead of that tail: seq 1001 would commit before 1000.
        let p = caller("p", "a");
        let mut first = small(999);
        first.push(event(1000, INLINE_TEXT_MAX_BYTES + 1));
        let q = queue(vec![
            pending(1, p.clone(), "r", first),
            pending(2, p, "r", vec![event(1001, 8)]),
        ]);
        assert_eq!(shape(&next_plan(&q, 1, false).unwrap()), [(1, 0, 999)]);
    }

    /// An event whose JSON is exactly `total` bytes.
    fn encoded_to(total: usize) -> SequencedEvent {
        let base = serde_json::to_vec(&event(1, 0).event).unwrap().len();
        let at = event(1, total - base);
        assert_eq!(serde_json::to_vec(&at.event).unwrap().len(), total);
        at
    }

    #[test]
    fn the_blob_threshold_is_pinned_at_equality_and_the_size_is_the_encoded_one() {
        // `placement` keeps 65,536 bytes inline and sends one more behind a blob.
        let at = |total| row_cost(&encoded_to(total).event, usize::MAX).unwrap();
        assert_eq!(
            at(INLINE_TEXT_MAX_BYTES - 1),
            (1, INLINE_TEXT_MAX_BYTES - 1)
        );
        assert_eq!(at(INLINE_TEXT_MAX_BYTES), (1, INLINE_TEXT_MAX_BYTES));
        assert_eq!(
            at(INLINE_TEXT_MAX_BYTES + 1),
            (2, INLINE_TEXT_MAX_BYTES + 1)
        );
    }

    #[test]
    fn an_event_is_encoded_only_as_far_as_the_limit_and_the_limit_is_exact() {
        let e = encoded_to(1000).event;
        assert_eq!(row_cost(&e, 1000), Some((1, 1000)));
        assert_eq!(row_cost(&e, 999), None);
        assert_eq!(row_cost(&e, 0), None);
    }

    #[test]
    fn a_batch_is_measured_until_the_first_event_that_does_not_fit() {
        let events: Vec<_> = (1..=5).map(|_| encoded_to(1000)).collect();
        let one = Measure::of(&events[..1], usize::MAX).unwrap().bytes;
        let before = encoded_events();
        assert!(
            one == 1000,
            "frozen payloads retain exactly their encoded bytes"
        );
        assert!(Measure::of(&events, 5 * one).is_some_and(|m| m.bytes == 5 * one));
        assert_eq!(encoded_events() - before, 5);
        let before = encoded_events();
        assert!(Measure::of(&events, 2 * one - 1).is_none());
        assert_eq!(
            encoded_events() - before,
            2,
            "the second event does not fit, so the other three are not looked at"
        );
    }

    fn batch_of(caller: Caller, run: &str, lineage: &str, events: usize) -> EventBatch {
        EventBatch {
            caller,
            run: run.into(),
            lineage: lineage.into(),
            epoch: 1,
            events: small(events as u64),
        }
    }

    #[test]
    fn metadata_is_counted_at_its_capacity_even_with_no_events() {
        let p = caller("p", "a");
        let bare = batch_of(p.clone(), "r", "l", 0).fixed_bytes();
        assert_eq!(
            bare,
            BATCH_FIXED_BYTES + STRING_COPIES * ("p".len() + "a".len() + 2)
        );
        let long = batch_of(p.clone(), &"r".repeat(4096), "l", 0).fixed_bytes();
        assert_eq!(long - bare, STRING_COPIES * 4095, "a run name is held");
        let more = batch_of(p.clone(), "r", "l", 3).fixed_bytes();
        assert_eq!(
            more - bare,
            3 * (EVENT_FIXED_BYTES + 1),
            "and so is each event's fixed part and possible reconciliation run name"
        );
        let task = Caller {
            project: "p".into(),
            principal: Principal::Task {
                identity: "a".into(),
                task: "t".repeat(10),
                run: "u".repeat(20),
                lineage: "v".repeat(30),
                epoch: 1,
            },
        };
        let held = batch_of(task, "r", "l", 0).fixed_bytes();
        assert_eq!(
            held - bare,
            STRING_COPIES * 60,
            "every string of a task principal"
        );
        assert_eq!(
            batch_of(p, "r", "l", 0).fixed_bytes(),
            bare,
            "the count is a function of the batch"
        );
    }

    #[test]
    fn the_default_envelope_holds_a_whole_blob_with_room_beside_it() {
        // One event may be as large as a blob (64 MiB) and is one statement.
        const BLOB: usize = MAX_BLOB_BYTES as usize;
        const { assert!(QUEUE_LIMIT_BYTES >= 3 * BLOB) };
        const { assert!(QUEUE_LIMIT_BATCHES * BATCH_FIXED_BYTES < QUEUE_LIMIT_BYTES) };
    }

    #[test]
    fn an_empty_batch_takes_one_turn_for_its_lease_check() {
        let p = caller("p", "a");
        let q = queue(vec![
            pending(1, p.clone(), "a", Vec::new()),
            pending(2, p, "b", small(2)),
        ]);
        assert_eq!(
            shape(&next_plan(&q, 1, false).unwrap()),
            [(1, 0, 0), (2, 0, 2)]
        );
    }
}
