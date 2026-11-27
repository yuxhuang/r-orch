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
//! * **Actual work.** The writer counts the actual rows and time of every
//!   physical transaction, once, in `writer_txn_over_bound_total`. This module
//!   never counts, so an indivisible event over the bound (one statement that
//!   cannot be cut) is counted by the writer alone and a retry never counts it
//!   twice.
//! * **Retained input.** A batch is dropped from memory only on a known commit
//!   or a definitive refusal. After a confirmed noncommit with a retryable cause
//!   the same transaction is rebuilt from the retained input as a new request of
//!   the same retry episode (`RetainedWrite`). After an unknown commit it is
//!   first reconciled: the producer reads back the exact rows it expected and
//!   commits to nothing until the database has answered.
//!
//! Not here (T16, later): numbering at handover, the collector's buffer and its
//! gaps, the counters at start and the stop record (R61, R63, R139), and pruning.
//! Daemon wiring and activation are not part of this slice.

use super::MAX_ROWS;
use crate::blob::INLINE_TEXT_MAX_BYTES;
use crate::sqlite::{RetainedWrite, SqliteStorage, WriteAccounting, WriteAttempt, WriteOutcome};
use orch_core::store::events::{AppendResult, SequencedEvent};
use orch_core::store::{Caller, Lane, ReadSnapshot, StoreError, UnitOfWork, WriteWork};
use orch_domain::RunEvent;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, oneshot};

/// R27: the work of one transaction, checked between statements.
pub(crate) const MAX_WORK: Duration = Duration::from_millis(20);
/// Events per statement. The clock is read between statements, so this is also
/// the grain of the 20 ms cut; an event with a blob is a statement of its own.
const STEP_EVENTS: usize = 32;
/// The most events that may wait in the producer; a collector that is refused
/// keeps its buffer (R63) and tries again.
const QUEUE_LIMIT_EVENTS: usize = 16_384;
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
    /// Too many events wait. Keep the batch and submit it again later.
    Full(Box<EventBatch>),
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
    /// The clock of the 20 ms cut. The writer measures its own actual time.
    pub now: Now,
    pub observer: Option<Observer>,
    pub queue_limit: usize,
}

impl Default for EventProducerOptions {
    fn default() -> Self {
        Self {
            now: Arc::new(Instant::now),
            observer: None,
            queue_limit: QUEUE_LIMIT_EVENTS,
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
    events: Arc<[SequencedEvent]>,
    costs: Arc<[u8]>,
    consumed: usize,
    /// A statement of this batch has committed. An empty batch is answered
    /// only after its lease check has committed.
    touched: bool,
    acc: AppendResult,
    reply: Option<oneshot::Sender<Answer>>,
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

#[derive(Default)]
struct Queue {
    pending: VecDeque<Pending>,
    events: usize,
    busy: bool,
    closed: Option<String>,
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
    pub fn submit(&self, batch: EventBatch) -> Result<Submission, Rejected> {
        let costs: Arc<[u8]> = batch.events.iter().map(|e| row_cost(&e.event)).collect();
        let mut queue = self.shared.queue();
        if let Some(cause) = &queue.closed {
            let cause = cause.clone();
            return Err(Rejected::Closed(Box::new(batch), cause));
        }
        if !queue.pending.is_empty()
            && queue.events + batch.events.len() > self.shared.options.queue_limit
        {
            return Err(Rejected::Full(Box::new(batch)));
        }
        let (reply, receiver) = oneshot::channel();
        queue.events += batch.events.len();
        queue.pending.push_back(Pending {
            id: self.shared.next.fetch_add(1, Ordering::Relaxed),
            caller: batch.caller,
            run: batch.run,
            lineage: batch.lineage,
            epoch: batch.epoch,
            events: Arc::from(batch.events),
            costs,
            consumed: 0,
            touched: false,
            acc: AppendResult {
                stored: 0,
                skipped: 0,
            },
            reply: Some(reply),
        });
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

/// Rows a stored event changes (the actual `total_changes` of the writer): its
/// own, plus the blob's when the stored payload does not fit in the row. This
/// mirrors `stored_payload`; an event that cannot be encoded counts one row, and
/// storage refuses it at its statement.
fn row_cost(event: &RunEvent) -> u8 {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut size = Count(0);
    match serde_json::to_writer(&mut size, event) {
        Ok(()) if size.0 > INLINE_TEXT_MAX_BYTES => 2,
        _ => 1,
    }
}

/// One batch's share of a transaction.
#[derive(Clone)]
struct Slice {
    pending: u64,
    run: String,
    lineage: String,
    epoch: u64,
    events: Arc<[SequencedEvent]>,
    costs: Arc<[u8]>,
    from: usize,
    to: usize,
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
fn build_work(plan: &Plan, now: Now, progress: Arc<Mutex<Progress>>) -> WriteWork {
    let slices = plan.slices.clone();
    Box::new(move |unit: &mut dyn UnitOfWork| {
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
                match unit.append_events(&slice.run, &slice.lineage, slice.epoch, &events[at..end])
                {
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
    })
}

enum Flow {
    Continue,
    Stop,
}

struct Drain {
    shared: Arc<Shared>,
    storage: SqliteStorage,
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
            self.shared.queue().busy = false;
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
            let attempt = match write.attempt(work).await {
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
            match write
                .reconcile(move |snapshot| async move { rows_are_durable(snapshot, &check).await })
                .await
            {
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
        }
        if let Some(observer) = &self.shared.options.observer {
            observer(&TransactionReport {
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
            });
        }
        for (pending, answer) in answers {
            pending.finish(answer);
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
        let drained: Vec<Pending> = {
            let mut queue = self.shared.queue();
            queue.closed = Some(error.to_string());
            queue.events = 0;
            queue.pending.drain(..).collect()
        };
        for pending in drained {
            let doubtful =
                in_doubt.is_some_and(|plan| plan.slices.iter().any(|s| s.pending == pending.id));
            let failure = pending.failure(copy_error(error), doubtful);
            pending.finish(Err(failure));
        }
        Flow::Stop
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
    Some(pending)
}

/// What the callback appended in an unknown attempt, as the rows to look for.
pub(super) struct Expected {
    pub(super) run: String,
    pub(super) events: Arc<[SequencedEvent]>,
    pub(super) from: usize,
    pub(super) to: usize,
}

fn expected_rows(plan: &Plan, progress: &Arc<Mutex<Progress>>) -> Vec<Expected> {
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
            } if result.stored > 0 => Some(Expected {
                run: plan.slices[*slice].run.clone(),
                events: Arc::clone(&plan.slices[*slice].events),
                from: *from,
                to: *to,
            }),
            _ => None,
        })
        .collect()
}

/// `true` when every expected row is stored with the expected bytes, `false`
/// when none is. A transaction is atomic, so anything between is a divergence
/// that the producer reports and does not resolve.
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
        let costs: Arc<[u8]> = events.iter().map(|e| row_cost(&e.event)).collect();
        Pending {
            id,
            caller: who,
            run: run.into(),
            lineage: "l".into(),
            epoch: 1,
            events: Arc::from(events),
            costs,
            consumed: 0,
            touched: false,
            acc: AppendResult {
                stored: 0,
                skipped: 0,
            },
            reply: None,
        }
    }

    fn queue(items: Vec<Pending>) -> Queue {
        Queue {
            events: items.iter().map(|p| p.events.len()).sum(),
            pending: items.into(),
            busy: false,
            closed: None,
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
        assert_eq!(row_cost(&event(1, 8).event), 1);
        assert_eq!(row_cost(&event(1, big).event), 2);
        // The stored payload is the whole event JSON, so a payload that only
        // fits without its envelope is already a blob.
        assert_eq!(row_cost(&event(1, INLINE_TEXT_MAX_BYTES - 8).event), 2);
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
