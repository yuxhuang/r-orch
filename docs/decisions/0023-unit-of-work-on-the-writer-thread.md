# 0023. The unit of work stays on the writer's thread: Amendment 3 of the frozen port

- Status: accepted. The owner approved the change on 2026-11-26 (record M of the storage spec's "Owner's answers");
  it takes effect when the owner merges the PR that carries it.
- Date: 2026-11-26
- Spec: [SQLite storage for release 1.1](../specs/2026-11-20-sqlite-storage.md) ("Amendment 3: the unit of work
  stays on the writer's thread" under "Storage port v2"); plan:
  [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md) ("Amendment 3: the unit of work on the
  writer's thread")
- Amends: the signatures that [0017](0017-sqlite-storage.md) decision 3 and [0020](0020-storage-consumer-contracts.md)
  record in [`docs/TRAITS-FROZEN.md`](../TRAITS-FROZEN.md), by removing seven `Send` bounds. It adds to the port;
  it changes no accepted decision's wording and supersedes no ADR.
- Scope of the decision: the contract of the port, and the seven-line source change in `orch-core` that carries it.
  No SQLite `Storage` implementation exists yet and nothing here is wired into the running daemon.

## Context

Facts read from `main` at `075f2b124`:

- `UnitOfWork` has `Send` as a supertrait, and `AttentionWrite`, `EventWrite`, `IntentWrite`, `LeaseWrite`,
  `RequestKeyWrite` and `RetryWrite` are each declared `: Send`. Only two types implement `UnitOfWork`: `Session`
  (the file reference, `crates/orch-infra/src/storage/v2.rs`) and `View` (the memory fake,
  `crates/orch-core/src/store/fake.rs`). Each is built inside the write call, on the thread that then runs the
  callback, and neither is moved to another thread.
- The callback is the owned part of a write: `WriteWork` is `Box<dyn FnOnce(&mut dyn UnitOfWork) -> Result<()> + Send +
  'static>`, and `StorageExt::transaction` requires `F: Send + 'static` and `T: Send + 'static`. The unit of work is
  only ever lent to the callback as `&mut dyn UnitOfWork`.
- The SQLite adapter's unit of work is a view over the writer's open transaction. `rusqlite` 0.41.1 (the pinned
  version, ADR 0017 decision 19) implements `Send` for `Connection` and does not implement `Sync`, so a borrow of its
  transaction is not `Send`, and no type that holds one can meet `UnitOfWork: Send`. One writer performs every
  write (ADR 0017 decision 6), and the landed event adapter already runs a `Send` closure against a borrowed,
  locally built view (`event_job` in `crates/orch-infra/src/sqlite/events/adapter.rs`).
- The landed writer documents that a panic of its callback rolls the transaction back and drops the worker, closing
  every queued receipt (`Writer::run`). The file reference resumes a panic in the work on the awaiting caller, and
  the memory fake runs the work on the caller's own task, so on both a panic unwinds the caller and storage goes on
  serving. The spec's record L records that the owner declined to map "a panic of the writer's thread" to
  `storage_unavailable`.
- The owner's ruling of 2026-11-26 (record M) approved removing the bounds from these seven traits only, on four
  conditions: the change is checked as a seven-bound diff, no planned consumer moves a borrowed view across threads,
  a panic in a callback is contained, and the restriction on Tokio runtime context in callback bodies is enforced.

## Decision

1. **Seven bounds are removed, and no other.** `UnitOfWork` no longer has `Send` as a supertrait, and `AttentionWrite`,
   `EventWrite`, `IntentWrite`, `LeaseWrite`, `RequestKeyWrite` and `RetryWrite` no longer have `: Send`. Nothing else
   in `orch_core::store` changes: `ReadSnapshot`, `AttentionRead`, `EventRead`, `IntentRead`, `LeaseRead` and
   `RetryRead` keep `Send + Sync`; `Storage` keeps `Send + Sync`; `StorageExt` and `WriteWork` keep their bounds and
   signatures.
2. **The callback is owned and runs once, on the writer.** `WriteWork`, and the `F` of `StorageExt::transaction`, stay
   `Send + 'static`, because the caller hands them to the writer. A backend invokes an admitted callback exactly once,
   on the thread that performs its writes, and never replays it. A backend that refuses admission (a request answered
   `overloaded` at its deadline, a failed writer) never invokes it.
3. **A borrowed view stays on the thread that lent it.** The `&mut dyn UnitOfWork` of a callback, and anything
   borrowed from it, is used only on the thread running the callback, until the callback returns. It is not sent to
   another thread, a spawned task or a scoped thread. The compiler enforces one case of this: `dyn UnitOfWork` is no
   longer `Send`, so the lent `&mut dyn UnitOfWork` cannot be moved into another thread (vector A3-W3 shows it for a
   scoped `move` closure). It enforces nothing about other data: a `&Caller`, or a value that the callback captures, is
   not thread-confined by this decision, and the rest of the rule is kept by review.
4. **A callback depends on no Tokio runtime.** The SQLite adapter runs the writer (`Writer::run`, a blocking function) on
   a plain thread, so a callback there has no runtime context. A callback calls neither `Handle::current()` nor `block_on` nor anything else that needs a runtime. Sending a result
   on a `tokio::sync::oneshot` channel, as `StorageExt::transaction` does, needs none and stays allowed.
5. **A panic in a callback is contained.** The SQLite `Storage::write` catches it before it leaves the job, rolls the
   transaction back so that nothing the callback did is committed, and lets the awaiting caller observe the panic by
   resuming it there (as the file reference does). The writer thread survives, no cause is latched in the failure
   state, and the next write on the same storage commits. This maps no panic to `storage_unavailable` and leaves
   record L unchanged. The catch surrounds the invocation of the caller's callback and nothing else (record N,
   2026-11-27): building the unit of work, its one read of the `Clock` and the check after a callback that returned
   success run outside it, and a panic there is a panic of adapter code that is not contained. A leaf of the adapter
   whose savepoint cleanup fails raises a typed unwind (`CleanupUnwind`) that is not contained either: it is observed
   under record L, so that a fatal cause latches even when the outer rollback would succeed, and raised again, and
   `Writer::run` ends the worker. Classification is by cause (record O, 2026-11-27): the operation's cause and the
   cleanup's are observed separately. When both are ordinary the running and the queued requests are answered
   `WriterClosed` and nothing is latched; when either is fatal it latches and they are answered `storage_unavailable`. If a callback panics and the outer rollback then fails, the payload
   stays in the error and reaches the caller. The spec's "Amendment 3" states the consequences.
6. **Proof is an obligation of the SQLite adapter's PR.** It must show decisions 2 to 5 with named tests: the vectors
   A3-W2, A3-W3 and A3-W6 of the spec, and A3-W7 to A3-W10 for the scope of the catch and the typed unwind in
   decision 5 (the plan's steps 2 and 2a say which PR shows which). The workspace compile check of the seven-bound
   diff (A3-W1) is a condition of the PR that carries this record.

## Consequences

- The SQLite unit of work can be the borrowed view over the writer's transaction. The spec's sentence "runs `work` on a
  `UnitOfWork` in one transaction on the writer" is then literally true.
- Engine code that runs inside a callback is written for a plain thread. A callback that happened to work on the
  references because it ran on a Tokio thread (`Handle::current()`) now fails on SQLite. The shared suites that run
  on SQLite are what catch it.
- A consumer that wants to hand a unit of work to another thread cannot, and the reference types being `Send` does
  not help, because a callback sees `dyn UnitOfWork`. Such a need requires a new design review. It is not met by
  restoring the bound silently.
- The change is reversible toward a request channel. A channel handle is `Send`, so a unit of work built on one still
  satisfies the relaxed trait; restoring the bound would instead force that channel to be built first.
- A source-level change only: no stored data, no protocol and no file format changes, and it needs no changelog
  entry.
- Reads are untouched. How a SQLite snapshot is served is not decided here.

## Alternatives considered

| Alternative | Why it lost |
|---|---|
| Keep the bounds; run the callback in the caller's task against a request channel to the writer | A request protocol, an admission handshake and an extra hop inside the write lock, to keep a bound for which no consumer has been found (condition A3-W1 of decision 6). Its cost could only be settled by a lock-hold measurement that the owner deferred |
| The writer lends `&mut Connection` to the callback | Reopens the qualified writer executor and every SQLite leaf that takes a transaction; does nothing for reads |
| A thread-checked wrapper that is `Send` | The same as this decision, with a runtime panic in place of a compile error, and an `unsafe impl Send` that a dependency of the wrapper would carry |
