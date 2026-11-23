# 0020. Storage port v2 consumer contracts: pending messages, per-task run order and whole retry text

- Status: proposed
- Date: 2026-11-23
- Issue: [#109](https://github.com/yuxhuang/r-orch/issues/109) (the T8 consumer follow-ups);
  spec: [SQLite storage for release 1.1](../specs/2026-11-20-sqlite-storage.md) ("Storage port v2", the
  "T8 consumer-contract amendment (issue 109)");
  plan: [SQLite storage for release 1.1](../plans/2026-11-20-sqlite-storage.md) ("T8 consumer contracts (issue 109)")
- Amends: nothing in [0017](0017-sqlite-storage.md). It adds to the port that decision 3 defines, applies decisions 7
  and 11 to the retry row, and is the ADR that `docs/TRAITS-FROZEN.md` requires for its record of the landed v2
  port. It supersedes no accepted ADR.
- Scope of the decision: contracts. The code that meets them, and the tests that prove them, are in the PR that
  closes #109. Nothing here is wired into the running daemon, and no SQLite adapter exists for it yet.

## Context

Facts read from `main` at `07b5f967`, after the retry enumeration (#191) and the reference conformance
reconciliation (#196) landed and #130 closed:

- **Pending messages.** `Filter::Messages { recipient }` selects by recipient only. 1.0's
  `MessageStore::list_pending` (`crates/orch-infra/src/storage/message.rs`) returns a message only when its status
  is `pending`, its `to_agent_id` is exactly the agent (a broadcast with no recipient is never pending for anyone)
  and it is not expired. `is_expired` is `expires_at < clock.now_iso()` on the two strings: strictly less, so a
  message whose expiry equals the clock is still pending, and an expiry that is not a date is compared as text,
  never parsed. A consumer that filtered a page client-side could miss every match on a page of rejected rows.
- **Per-task run order.** The port's `Sort` has no run order. The runs list defaults to `started_at` descending then
  id. The spec's table promised "per task: `run_no`", but `Run` has no `run_no`: it carries `attempt: u32`, and the
  schema's `run_no` and `attempt_no` columns are T10's.
- **Retry text.** `Retry::validate` refuses an `error` over 65,536 bytes
  (`crates/orch-core/src/store/retries.rs`). The spec's R97 requires text over 64 KiB, written through the API or
  imported, to be stored in the blob store and returned whole, goal decision 7 sets the 64 KB line, and ADR 0017
  decision 11 requires bytes to be published before the row that names them. 1.0's `RetryEntry.error` is unbounded.
  Issue 109's comment of 2026-11-22 suggested a diagnostic cap with the full text on the run row instead, and no
  ruling waived R97. `DiskBlobStore` (`crates/orch-infra/src/blob.rs`, `MAX_BLOB_BYTES`) accepts and returns at most
  67,108,864 bytes (64 MiB) per object, so a single object cannot hold the largest 1.0 errors.
- **Frozen record.** `docs/TRAITS-FROZEN.md` records only the additive event and lease signatures. Entities,
  paging, retries, intents, request keys, attention and the blob store landed without a record, and its sentence
  that `ReadSnapshot` extends `EventRead + LeaseRead` is out of date.
- **Verification names.** Some filters in the spec's rows for R7, R117 and R132 name tests that do not exist or
  cannot select one (`storage_conformance::...` is not a prefix of any test name, and a bare filter that matches
  nothing exits 0).

## Decision

1. **Pending messages are a port filter, applied before the page is cut.** Add the variant
   `Filter::PendingMessages { recipient: String, now: String }`. A message matches when its status is `pending`,
   `to_agent_id` is `Some(recipient)` byte for byte, and its `expires_at` is absent or `expires_at >= now` as a
   string comparison. Equality is therefore pending, which is 1.0's rule (`expires_at < now` is expired). `now` is
   the caller's captured `Clock::now_iso()`: the port reads no clock, parses no date and adds no validation, so an
   expiry that is not a date is compared as text exactly as 1.0 compared it. The filter runs before the count, the
   cursor and the bound, so a page fills with matches however many rejected rows precede them, and `total` counts
   matches. A cursor binds the recipient, the captured `now`, the sort and the project, so a cursor used with
   another `now` or recipient is refused. Order is the messages' default (`created_at`, `position`, id), and the
   family's other sorts are as for `Filter::Messages`.
2. **Per-task run order is `Sort::Attempt`, and only there.** Add `Sort::Attempt`. It is offered only with
   `Filter::Runs { task: Some(_), .. }` (an `agent` filter may accompany it) and orders by `Run.attempt`
   numerically, descending, then the run id's bytes ascending. It is refused as `InvalidPage` before any paging
   work for `Runs` with no task, for every other family and for every other filter. `Sort::Default` for runs is
   unchanged (`started_at` descending, then id), so no existing caller changes. The spec's `run_no` in the sorts
   table is corrected to this: the port's number is `Run.attempt`, not a schema column.
3. **A retry owns its whole `error` text.** This is the lead's decision of 2026-11-23, option A of the two put to
   it, taken under goal decision 7, R97 and ADR 0017 decision 11. `Retry` keeps its shape, and
   `Retry.error` is the complete logical text, including for a retry that has no surviving run. The text is
   measured in UTF-8 bytes. Up to and including 65,536 bytes it may be stored inline. Above that, the writer
   publishes the complete bytes to the blob store first and commits the hash in the retry row, and a read resolves
   the hash and returns the original text byte for byte. A failed publication, or a missing or corrupt blob at
   write time, fails the write and leaves the previous row exactly as it was. A reference that cannot be resolved
   at read time is an error on `read_retry`, `read_due_retries`, `read_retries`, `get_retry`, `due_retries` and
   `list_retries`, never an empty, shortened or partial row. Nothing truncates, and `Retry::validate` stops
   refusing long text (it keeps refusing an empty task, attempt 0 and a deadline before the epoch). The
   transaction-local first-wins rule, the due and complete read orders, the 1..=1,000 bounds, the millisecond unit
   and the absence of any cap on rows do not change.
4. **Text above one blob object is split across ordered chunks.** This is the lead's decision of 2026-11-23, option A
   of the follow-up question about `DiskBlobStore`, which accepts and returns at most 67,108,864 bytes (64 MiB,
   `MAX_BLOB_BYTES`) per object. Size alone never refuses a valid diagnostic, and no object limit is raised.
   Three representations exist, chosen by the UTF-8 length of `Retry.error`:
   - up to and including 65,536 bytes: inline, as above;
   - 65,537 to 67,108,864 bytes inclusive: one blob object whose hash the row holds, unchanged by this decision, and
     every such row that exists stays readable;
   - more than 67,108,864 bytes: the row keeps the **whole-value hash** (the SHA-256 of the complete bytes, which
     no single object holds) and an **ordered mapping to chunks**, each chunk an existing blob object of at most
     67,108,864 bytes. The mapping records position, so a chunk that repeats, such as two identical chunks of
     one text, appears at each position it occupies. The split is by bytes: a UTF-8 character may straddle a
     boundary, and the value is decoded only after the chunks are joined.

   A read fetches and verifies every referenced object, joins them in order, verifies the whole-value hash and
   only then decodes. A missing, corrupt or reordered chunk, or a joined value whose hash is not the whole-value hash, is a read
   error. A write publishes every object it needs before the row and its mapping are
   committed together, atomically. If a later chunk fails after an earlier one was published, the previous row
   stays readable, including when the caller catches that error and commits other work in the same unit of work. An
   object left unreferenced by an interrupted or rolled-back write follows the blob store's existing lifecycle (a
   sweep may remove it). The physical encoding of the mapping is the reference adapters' and T10's to choose; this
   decision fixes what it must hold and when it is checked, not its bytes.
5. **The frozen record follows the landed port.** `docs/TRAITS-FROZEN.md` records every v2 signature that is on
   `main`, including the `ReadSnapshot` and `UnitOfWork` supertraits and the expected revision that `update`,
   `delete` and `move_to_end` take, and the three additions above once they land. The 1.0 traits stay active and
   unchanged until T25 removes the product file adapter.
6. **Verification names follow the tests.** The spec's rows for R7, R117 and the caller part of R132 name the
   bare filters the tests have, with the registrations each must select. A representative selection of them is
   pending until the PR that closes #109 runs it. R8 to R15 and R97 keep the inventory that the #130 crosswalk
   reconciled.
7. **Who builds what.** The PR that closes #109 implements decisions 1 to 4 in `orch-core`, the memory fake and the
   file reference, with the shared tests registered on both. T10 owns the SQLite representation of all three
   (the retry blob and chunk mapping, the `PendingMessages` statement, the attempt index) and the adapter registration of
   the shared suites. T12 owns the finish and enqueue caller and lease-bound finish, T14 the dispatch order, T21
   the legacy import (a whole 1.0 error, rebuilt into chunks when it needs them), and T23 the backup, restore and
   export round trip of every reachable chunk. Each consumer keeps the obligations listed in the plan.

## Alternatives considered

- **Select pending messages by recipient and filter each page in the client.** A page of non-matching rows hides
  later matches and breaks the count. Rejected for decision 1's filter before the cut.
- **Let the backend read the clock.** The port reads none (every other time is passed in), a cursor would then
  have to survive a changing "now", and a test could not fix the time. Rejected for a caller-captured `now` that
  the cursor binds.
- **Parse `expires_at` and compare instants, or use a strict `>`.** The first direction proposed in the decision
  input. It changes 1.0's rule twice: equality was pending, and an expiry that does not parse was compared as
  text. Superseded by the paging design that read `message.rs`.
- **Order runs by the run id.** Ids are text, so `run_10` sorts before `run_2`. Rejected: no lexical stand-in for a
  number.
- **Reuse `started_at` for a per-task order, or add a `run_no` to `Run`.** The first is the default and answers a
  different question (newest first, equal times by id). The second changes the domain type and the wire for a
  number the schema may derive. Neither is needed.
- **Option B for retry text: a capped diagnostic with the whole text kept elsewhere.** It would waive R97 for one
  field, needs an express owner amendment, and must keep the full text for a retry whose run row is gone, with its
  own read, import and export rules. Not adopted. Adopting it needs the owner's amendment of R97, which this ADR
  does not make.
- **Refuse retry text above one blob object (64 MiB).** It would make a valid diagnostic fail by size alone, and a
  1.0 queue entry larger than 64 MiB could not be imported whole. Rejected for decision 4.
- **Raise the blob object limit for retries.** `DiskBlobStore` applies the one 64 MiB bound to every object on put and
  get, and its callers rely on that bound for bounded reads. A separate limit would split that contract. Rejected for
  chunks of at most the existing size.
- **Keep refusing text over 64 KiB.** That fails a finish whose agent printed a long error, and fails the import of
  a 1.0 queue entry that 1.0 accepted. It leaves R97 unmet.
- **Truncate.** Forbidden by R97 and by the decision of 2026-11-23.

## Consequences

- A consumer asks for an agent's pending messages in one bounded call, with a stable cursor and an exact count, and
  a test can fix `now`. A message whose expiry equals `now` is delivered, and one with an unparsable expiry behaves
  as 1.0 behaved. A caller that needs a different notion of time passes a different string.
- `Filter` and `Sort` each gain one serde variant (`pending_messages`, `attempt`). A backend and a wire consumer
  that match them exhaustively must handle them. The 1.1 protocol is not published yet, so no frozen wire changes.
- A SQLite adapter must keep lexical semantics for both the expiry comparison and the attempt tie-break: the
  statement compares text under the binary collation, and an index that orders `attempt` descending then id
  ascending serves `Sort::Attempt`. For imported runs whose 1.0 attempt repeats within a task (the spec's
  `attempt_no` uncounted case), T10 must serve the 1.0 number that the entity returns as `Run.attempt`, with the id
  breaking ties. Which column holds it is T10's.
- Retry rows may now reference blobs, and above 64 MiB many of them. A backup, a restore and an export must carry
  every reachable chunk, in order, with the whole-value hash (T23). The blob sweep must not remove an object that a
  retry names directly or as a chunk, and the check of blob references must count a missing one as damage. A row
  count no longer bounds a page's bytes: a page of 1,000 rows can hold 1,000 large texts, and its memory cost is
  the callers' to watch, since no cap is added.
- Because a failed publication or an unreadable blob is an error, a consumer that reads every retry (recovery,
  `state.get`, export) can now fail on one damaged row. Whether recovery then skips,
  repairs or stops is T12's and T13's decision, and it is never silent.
- T21 can import any 1.0 retry error whole. T12 must call `get_retry` before `put_retry` to keep 1.0's first-wins
  enqueue, and T14 must keep dispatching in candidate order and not due-row order.
- The chunk path is reference-port behaviour. This decision supplies no measurement above 64 MiB and no runtime
  evidence for any consumer, and it grants no publication or merge authority. The proof it requires is in the
  plan's "T8 consumer contracts (issue 109)".
- Until the closing PR merges, `main` still refuses a long retry error and has neither variant. The spec and the
  plan say so where they state the contract.
